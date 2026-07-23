//! RPC 客户端：通过 JSON-RPC over named pipe 与核心服务通信。
//!
//! 复用 wind-ipc 的协议类型，保持单一来源。
//! 采用按调用连接模式（连接→发一帧→读一帧→关闭），适合设置操作的低频场景。

use serde_json::{Value, json};
use std::io::{Read, Write};

use wind_ipc::rpc::{PROTOCOL_VERSION, Request, Response};

const MAX_FRAME: u32 = 16 * 1024 * 1024;

/// 管道/socket 后缀。优先级：WIND_PIPE_SUFFIX 环境变量 > 运行时模式。
fn endpoint_suffix() -> String {
    std::env::var("WIND_PIPE_SUFFIX").unwrap_or_else(|_| crate::mode::pipe_suffix().to_string())
}

/// 控制通道端点路径。与 wind-rpc 的 ctrl_endpoint 一致。
pub fn ctrl_endpoint() -> String {
    let suffix = endpoint_suffix();
    #[cfg(windows)]
    {
        // 末尾化，对齐 core wind-rpc::server::ctrl_endpoint
        // （release: wind_input_ctrl，dev: wind_input_ctrl_dev）。
        format!(r"\\.\pipe\wind_input_ctrl{suffix}")
    }
    #[cfg(unix)]
    {
        // unix 侧 core 用中间化 wind_input{suffix}_ctrl.sock，保持一致。
        let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        format!("{dir}/wind_input{suffix}_ctrl.sock")
    }
}

/// 写一帧（4 字节大端长度 + JSON payload）。
fn write_frame<W: Write>(w: &mut W, payload: &[u8]) -> std::io::Result<()> {
    let len = payload.len() as u32;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

/// 读一帧。
fn read_frame<R: Read>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "帧长超限",
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// 在一条已连接的流上完成一次请求-响应。
fn call_on_stream<S: Read + Write>(
    stream: &mut S,
    method: &str,
    params: &Value,
) -> Result<Value, String> {
    let req = Request {
        version: PROTOCOL_VERSION,
        id: 1,
        method: method.to_string(),
        params: params.clone(),
    };
    let payload = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
    write_frame(stream, &payload).map_err(|e| format!("发送失败: {e}"))?;
    let resp_buf = read_frame(stream).map_err(|e| format!("读取失败: {e}"))?;
    let resp: Response = serde_json::from_slice(&resp_buf).map_err(|e| e.to_string())?;
    if let Some(err) = resp.error {
        return Err(err);
    }
    Ok(resp.result.unwrap_or(Value::Null))
}

/// 单次 RPC 的整体超时上限。命名管道 read_exact 本身无超时,core 无响应时会无限
/// 阻塞;该上限把最坏等待收敛到有限值,避免拖住 UI 线程与进程退出(见下方线程包装)。
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 测试用互斥锁：`WIND_RPC_MOCK` 是**进程全局**环境变量,任何读写它的测试都必须先持有本锁。
///
/// 必须全 crate 共用一把。曾经 `state` 与 `theme_import` 各自定义了一个同名 static,
/// 两把锁互不相识却保护同一个变量 —— 一侧刚 `set_var`,另一侧测试结束时的 `remove_var`
/// 就把它抹掉,导致 `mock_disconnect_does_not_block` 随并行调度顺序间歇失败。
/// 新增涉及该变量的测试时,一律 `let _g = crate::rpc::ENV_LOCK.lock()...`,不要另建锁。
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 连接核心并完成一次 RPC 调用。
pub fn call_core(method: &str, params: &Value) -> Result<Value, String> {
    if std::env::var("WIND_RPC_MOCK").as_deref() == Ok("1") {
        return Ok(mock_reply(method, params));
    }
    let endpoint = ctrl_endpoint();
    let t = std::time::Instant::now();
    // 在工作线程执行阻塞式管道 RPC,主调用侧带超时等待:命名管道读取无超时,core
    // 无响应会无限阻塞。超时后放弃该线程(其阻塞的 read 会随进程退出由 OS 回收),
    // 立即返回错误,从而 UI 不卡、退出不被拖住。设置操作低频,每次 spawn 开销可忽略。
    let (tx, rx) = std::sync::mpsc::channel();
    let method_owned = method.to_string();
    let params_owned = params.clone();
    std::thread::spawn(move || {
        let _ = tx.send(call_core_inner(&endpoint, &method_owned, &params_owned));
    });
    let result = match rx.recv_timeout(CALL_TIMEOUT) {
        Ok(r) => r,
        Err(_) => Err(format!("core 无响应(超过 {}s)", CALL_TIMEOUT.as_secs())),
    };
    match &result {
        Ok(_) => tracing::debug!("rpc {} ok ({:?})", method, t.elapsed()),
        Err(e) => tracing::warn!("rpc {} err ({:?}): {}", method, t.elapsed(), e),
    }
    result
}

fn call_core_inner(endpoint: &str, method: &str, params: &Value) -> Result<Value, String> {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        let mut stream = UnixStream::connect(endpoint)
            .map_err(|e| format!("连接 core 失败 ({endpoint}): {e}"))?;
        call_on_stream(&mut stream, method, params)
    }
    #[cfg(windows)]
    {
        let mut stream =
            connect_pipe(endpoint, 100).map_err(|e| format!("连接 core 失败 ({endpoint}): {e}"))?;
        call_on_stream(&mut stream, method, params)
    }
}

/// Windows 命名管道连接，对瞬态错误短重试。
#[cfg(windows)]
fn connect_pipe(endpoint: &str, max_attempts: u32) -> std::io::Result<std::fs::File> {
    let mut attempt = 0u32;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(endpoint)
        {
            Ok(s) => return Ok(s),
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                // 231 = ERROR_PIPE_BUSY：服务存在但繁忙，重试有意义。
                // 2/3 = 管道/路径不存在：服务未启动，立即失败，勿重试浪费 2s。
                if code == 231 && attempt < max_attempts {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                } else {
                    return Err(e);
                }
            }
        }
    }
}

/// 是否处于离线 mock 模式。
pub fn is_mock() -> bool {
    std::env::var("WIND_RPC_MOCK").as_deref() == Ok("1")
}

// 测试模块后仍有 mock_reply 等离线实现；局部豁免 toolchain 新增的风格 lint。
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;

    // 测试遮蔽：旧用例只传 method；转调真实双参实现。
    fn mock_reply(method: &str) -> Value {
        super::mock_reply(method, &json!({}))
    }

    #[test]
    fn test_ctrl_endpoint_format() {
        let ep = ctrl_endpoint();
        assert!(ep.contains("wind_input"));
        assert!(ep.contains("_ctrl"));
    }

    /// mockdata 中刻意偏离 core 系统预置的值（配套 [`mock_config_matches_core_system_preset`]）。
    ///
    /// `schema.available` 必须与 mock 的 `schema.list` 自洽（那里给的是 wubi86 / wubi98 /
    /// wubi_mixed / pinyin_simp 这套演示方案），不能取真实预置的 wubi86_pinyin——
    /// 否则 mock 模式的方案下拉会列出一个 `schema.list` 里不存在的 id。
    const CURATED_MOCK_VALUES: &[&str] = &["schema.available"];

    /// 把 JSON 摊平成「点分路径 → 叶子值」（规则同 core 的 collect_leaf_keys：
    /// 非空对象下钻，数组与空对象作叶子）。
    fn json_leaves(prefix: &str, v: &Value, out: &mut std::collections::BTreeMap<String, Value>) {
        match v {
            Value::Object(m) if !m.is_empty() => {
                for (k, sub) in m {
                    let child = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    json_leaves(&child, sub, out);
                }
            }
            _ => {
                out.insert(prefix.to_string(), v.clone());
            }
        }
    }

    /// 守护：`mockdata/config.json` 必须等于 core 的系统预置（L1 代码默认 ⊕ data/config.toml），
    /// 即全新安装下 `config.get` 的真实返回值；仅 [`CURATED_MOCK_VALUES`] 中的键可以不同。
    ///
    /// mockdata 是 core `Config` 的**手抄镜像**，此前无任何对账，已烂到：缺 45 键、
    /// 残留 6 个 core 已删除的孤儿键（`input.code_commit.*` 在 6 域重命名时就迁走了、
    /// `schema.pinyin.candidate_order` 已下线），另有 7 处值停留在 L1 而非成品的 L1⊕L2。
    /// 后果是 mock 模式下的设置页与真实运行时不是同一个东西，拿它验 UI 会得出错误结论。
    ///
    /// 失败时请重新生成 mockdata，而不是逐个手改。
    #[test]
    fn mock_config_matches_core_system_preset() {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../WindInput/data");
        let preset =
            wind_config::Config::system_preset_value(Some(&data)).expect("读取 core 系统预置失败");
        let cfg: wind_config::Config = preset.try_into().expect("系统预置反序列化为 Config 失败");
        let expect = serde_json::to_value(&cfg).expect("Config 序列化失败");
        let mock: Value = serde_json::from_str(MOCK_CONFIG).expect("mockdata JSON 解析失败");

        let (mut e, mut m) = (Default::default(), Default::default());
        json_leaves("", &expect, &mut e);
        json_leaves("", &mock, &mut m);
        assert!(!e.is_empty(), "core 预置为空，对账无意义");

        let mut problems = Vec::new();
        for (key, want) in &e {
            match m.get(key) {
                None => problems.push(format!("mockdata 缺键: {key}")),
                Some(got) if got != want && !CURATED_MOCK_VALUES.contains(&key.as_str()) => {
                    problems.push(format!("{key}: mock={got} core={want}"));
                }
                _ => {}
            }
        }
        for key in m.keys().filter(|k| !e.contains_key(*k)) {
            problems.push(format!("mockdata 多余键（core 已无此键）: {key}"));
        }
        assert!(
            problems.is_empty(),
            "mockdata/config.json 与 core 系统预置漂移 {} 处，请重新生成：\n{}",
            problems.len(),
            problems.join("\n")
        );
    }

    /// 守护：策展名单不能腐烂——列进去的键必须真的与预置不同，否则说明它已可回归自动对账。
    #[test]
    fn curated_mock_values_still_differ() {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../WindInput/data");
        let preset = wind_config::Config::system_preset_value(Some(&data)).expect("读取预置失败");
        let cfg: wind_config::Config = preset.try_into().expect("反序列化失败");
        let expect = serde_json::to_value(&cfg).expect("序列化失败");
        let mock: Value = serde_json::from_str(MOCK_CONFIG).expect("解析失败");

        let (mut e, mut m) = (Default::default(), Default::default());
        json_leaves("", &expect, &mut e);
        json_leaves("", &mock, &mut m);
        for key in CURATED_MOCK_VALUES {
            assert!(e.contains_key(*key), "策展键已不在 core 配置中: {key}");
            assert_ne!(
                m.get(*key),
                e.get(*key),
                "{key} 现在与 core 预置相同，请从 CURATED_MOCK_VALUES 移除"
            );
        }
    }

    /// mock 的 `config.getDefaults` 必须给出可用的 Config（此前返回空对象，
    /// 导致 mock 模式下「恢复默认」静默无效）。
    #[test]
    fn mock_get_defaults_returns_config_shape() {
        let d = mock_reply("config.getDefaults");
        assert!(d.is_object(), "getDefaults 应返回对象");
        assert!(
            d.get("schema").and_then(|s| s.get("active")).is_some(),
            "getDefaults 应是 Config 形状（含 schema.active），实际: {d}"
        );
    }

    #[test]
    fn test_mock_reply_shapes() {
        // 确保 mock 返回合理的 JSON 结构
        let status = mock_reply("system.status");
        assert!(status.get("running").is_some());

        let config = mock_reply("config.get");
        assert!(config.is_object());

        let list = mock_reply("schema.list");
        assert!(list.is_array());

        let paged = mock_reply("dict.listPaged");
        assert!(paged.get("items").is_some());
        assert!(paged.get("total").is_some());
    }

    #[test]
    fn mock_phrase_list_has_is_system() {
        let list = mock_reply("phrase.list");
        let arr = list.as_array().unwrap();
        assert!(arr.iter().any(|p| p.get("isSystem") == Some(&json!(true))));
        assert!(arr.iter().any(|p| p.get("isSystem") == Some(&json!(false))));
    }

    #[test]
    fn mock_shadow_add_rule_ok() {
        assert_eq!(mock_reply("shadow.addRule").get("ok"), Some(&json!(true)));
    }

    #[test]
    fn mock_phrase_system_and_user() {
        let sys = mock_reply("phrase.listSystem");
        assert!(
            sys.as_array()
                .unwrap()
                .iter()
                .all(|p| p.get("isSystem") == Some(&json!(true)))
        );
        let usr = mock_reply("phrase.listUser");
        assert!(usr.get("items").is_some() && usr.get("total").is_some());
        assert_eq!(usr.get("total"), Some(&json!(2)));
        assert!(usr.get("items").unwrap().is_array());
    }

    #[test]
    fn mock_phrase_reset_system() {
        let reset = mock_reply("phrase.resetSystem");
        assert_eq!(reset.get("ok"), Some(&json!(true)));
        assert_eq!(reset.get("changed"), Some(&json!(0)));
    }

    #[test]
    fn mock_phrase_export_import() {
        assert!(mock_reply("phrase.export").get("content").is_some());
        assert_eq!(mock_reply("phrase.import").get("imported"), Some(&json!(0)));
    }

    #[test]
    fn mock_theme_preview_import_and_get_text() {
        let preview = mock_reply("theme.preview");
        assert_eq!(preview.get("display_name"), Some(&json!("测试主题")));
        assert_eq!(preview.get("slug"), Some(&json!("test-theme")));

        let imported = mock_reply("theme.importFromText");
        assert_eq!(imported.get("ok"), Some(&json!(true)));
        assert_eq!(imported.get("slug"), Some(&json!("test-theme")));

        let text = mock_reply("theme.getText");
        assert_eq!(text.get("slug"), Some(&json!("default")));
        assert!(
            text.get("toml")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .contains("[meta]")
        );
    }

    #[test]
    fn mock_get_config_codetable_has_dictionaries() {
        // 码表方案返回带 dictionaries 数组，含主库 + 附加库。
        let cfg = super::mock_reply("schema.getConfig", &json!({ "id": "wubi86" }));
        let dicts = cfg
            .get("dictionaries")
            .and_then(|v| v.as_array())
            .expect("dictionaries 数组");
        assert!(
            dicts.iter().any(|d| d.get("default") == Some(&json!(true))),
            "含主库"
        );
        assert!(
            dicts
                .iter()
                .any(|d| d.get("id") == Some(&json!("wubi86_extra"))),
            "含扩展词库附加库"
        );
    }

    #[test]
    fn mock_get_config_shuangpin_unchanged() {
        // 双拼方案仍返回布局配置、无 dictionaries（保持原行为）。
        let cfg = super::mock_reply("schema.getConfig", &json!({ "id": "double_pinyin" }));
        assert_eq!(
            cfg.pointer("/engine/pinyin/shuangpin/layout"),
            Some(&json!("ziranma"))
        );
    }

    #[test]
    fn mock_set_dict_enabled_persists_within_session() {
        // 保存 wubi86_extra=false 后，getConfig 应读回 enabled=false（mock 会话内记忆）。
        super::mock_reply(
            "schema.setDictEnabled",
            &json!({ "id": "wubi86", "dictId": "wubi86_extra", "enabled": false }),
        );
        let cfg = super::mock_reply("schema.getConfig", &json!({ "id": "wubi86" }));
        let dicts = cfg.get("dictionaries").and_then(|v| v.as_array()).unwrap();
        let extra = dicts
            .iter()
            .find(|d| d.get("id") == Some(&json!("wubi86_extra")))
            .unwrap();
        assert_eq!(
            extra.get("enabled"),
            Some(&json!(false)),
            "保存的开关应被 getConfig 读回"
        );
    }

    #[test]
    fn mock_set_dict_enabled_ok() {
        let r = super::mock_reply(
            "schema.setDictEnabled",
            &json!({ "id": "wubi86", "dictId": "wubi86_extra", "enabled": false }),
        );
        assert_eq!(r.get("ok"), Some(&json!(true)));
        assert_eq!(r.get("live"), Some(&json!(true)));
    }
}

/// 内置 capability 快照（与 src/assets 同一文件），mock 模式离线提供能力契约。
const MOCK_CAPABILITIES: &str = include_str!("assets/capabilities.snapshot.json");
const MOCK_CONFIG: &str = include_str!("mockdata/config.json");

thread_local! {
    /// Mock 会话内的扩展词库开关记忆：(方案 id, 词库 id) → enabled。
    /// 让离线 mock 下「setDictEnabled 保存 → getConfig 重开」能保留改动，仅用于 UI 自测。
    static MOCK_DICT_OVERRIDES: std::cell::RefCell<std::collections::HashMap<(String, String), bool>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// 记录一次 mock setDictEnabled（会话内）。
fn record_mock_dict_override(schema_id: String, dict_id: String, enabled: bool) {
    MOCK_DICT_OVERRIDES.with(|m| m.borrow_mut().insert((schema_id, dict_id), enabled));
}

/// 把会话内记忆的开关套用到 getConfig 返回的 dictionaries[*].enabled 上。
fn apply_mock_dict_overrides(schema_id: &str, cfg: &mut Value) {
    let Some(arr) = cfg.get_mut("dictionaries").and_then(|d| d.as_array_mut()) else {
        return;
    };
    MOCK_DICT_OVERRIDES.with(|m| {
        let map = m.borrow();
        for d in arr.iter_mut() {
            if let Some(did) = d.get("id").and_then(|x| x.as_str()).map(str::to_string)
                && let Some(&en) = map.get(&(schema_id.to_string(), did))
            {
                d["enabled"] = json!(en);
            }
        }
    });
}

/// Mock 模式：返回形状合理的默认值；manifest/config 用内置真实数据，
/// 方案/主题/字体/系统信息给代表性样例，便于离线核对 UI 保真度。
fn mock_reply(method: &str, params: &Value) -> Value {
    match method {
        "system.capabilities" => {
            serde_json::from_str(MOCK_CAPABILITIES).unwrap_or_else(|_| json!({}))
        }
        "config.get" => serde_json::from_str(MOCK_CONFIG).unwrap_or_else(|_| json!({})),
        "schema.list" => json!([
            { "id": "wubi86", "name": "五笔 86", "engineType": "codetable", "scheme": "",
              "builtin": true, "version": "2.1.0", "description": "标准五笔 86 版码表方案", "author": "王永民" },
            { "id": "wubi98", "name": "五笔 98", "engineType": "codetable", "scheme": "",
              "builtin": true, "version": "1.0.0", "description": "标准五笔 98 版码表方案" },
            { "id": "wubi_mixed", "name": "五笔拼音混输", "engineType": "mixed", "scheme": "",
              "builtin": true, "version": "1.2.0", "description": "五笔 86 + 拼音混合输入" },
            { "id": "pinyin_simp", "name": "全拼·简", "engineType": "pinyin", "scheme": "full",
              "builtin": true, "description": "简体全拼方案" },
            { "id": "double_pinyin", "name": "双拼", "engineType": "pinyin", "scheme": "shuangpin",
              "builtin": false, "version": "0.5.0", "description": "双拼输入方案（布局可在设置中切换）" }
        ]),
        // 双拼布局清单：core 侧合并扫描 schemas/shuangpin/*.toml（安装+用户目录）后返回。
        "schema.layouts" => json!([
            { "id": "abc", "name": "智能ABC" },
            { "id": "mspy", "name": "微软双拼" },
            { "id": "sogou", "name": "搜狗双拼" },
            { "id": "xiaohe", "name": "小鹤双拼" },
            { "id": "ziguang", "name": "紫光双拼" },
            { "id": "ziranma", "name": "自然码" }
        ]),
        "theme.list" => json!([
            { "name": "default_light", "display_name": "清风·浅", "version": "3.2.1", "builtin": true },
            { "name": "default_dark", "display_name": "清风·深", "version": "3.2.1", "builtin": true },
            { "name": "mono", "display_name": "墨", "version": "1.0.0", "builtin": false }
        ]),
        "theme.delete" => json!({ "ok": true }),
        "system.fonts" => json!([
            { "name": "微软雅黑" }, { "name": "思源黑体" }, { "name": "等线" }
        ]),
        "system.info" => json!({
            "version": "3.2.1", "platform": "windows", "arch": "x86_64",
            "build": "release", "dataDir": "C:/Users/.../WindInput", "running": true
        }),
        "dict.stats" | "stats.daily" => json!([]),
        // 词库管理离线样例：便于截图核对非空界面（形状对齐 core webdata.rs）。
        "phrase.list" => json!([
            { "code": "bj", "text": "北京", "position": 0, "weight": 1000, "enabled": true, "isSystem": true },
            { "code": "yx", "text": "example@example.com", "position": 0, "weight": 1000, "enabled": true, "isSystem": true },
            { "code": "dd", "text": "（＾＿＾）", "position": 0, "weight": 1000, "enabled": false, "isSystem": false }
        ]),
        "phrase.listSystem" => json!([
            { "code": "rq", "text": "$date", "weight": 1000, "position": 0, "enabled": true, "isSystem": true },
            { "code": "dd", "text": "（＾＿＾）", "weight": 1000, "position": 0, "enabled": false, "isSystem": true }
        ]),
        "phrase.listUser" => json!({ "items": [
            { "code": "bj", "text": "北京", "weight": 1000, "position": 0, "enabled": true, "isSystem": false },
            { "code": "yx", "text": "example@example.com", "weight": 1000, "position": 0, "enabled": true, "isSystem": false }
        ], "total": 2 }),
        "phrase.export" => {
            json!({ "content": "# WindInput 用户数据文件\nwind_dict:\n  version: 1\n\n--- !phrases\nbj\t北京\t1000\t0\t1\n" })
        }
        "phrase.import" => json!({ "imported": 0, "skipped": 0 }),
        "phrase.resetSystem" => json!({ "ok": true, "changed": 0 }),
        "temp.list" => json!([
            { "code": "nihao", "text": "你好", "count": 3 },
            { "code": "women", "text": "我们", "count": 7 }
        ]),
        "shadow.list" => json!([
            { "code": "wm", "word": "我们", "candId": "c1", "type": "pin", "position": 0 },
            { "code": "nh", "word": "你好", "candId": null, "type": "delete" }
        ]),
        "theme.preview" => json!({
            "slug": "test-theme",
            "display_name": "测试主题",
            "name": "测试主题",
            "version": "1.0.0",
            "author": "WindInput"
        }),
        "theme.importFromText" => json!({
            "ok": true,
            "slug": "test-theme",
            "display_name": "测试主题"
        }),
        "theme.getText" => json!({
            "slug": "default",
            "toml": "base = \"_qingfeng\"\n\n[meta]\nname = \"清风·蓝\"\nversion = \"1.0\"\nauthor = \"清风输入法\"\n"
        }),
        // mockdata 就是「无用户层的系统预置」，正是 getDefaults 的语义，故直接复用。
        // 此前这里返回空对象，Config::from_value({}) 拿不到任何值——mock 模式下
        // 「恢复默认」是静默失效的，点了没反应也没有报错。
        "config.getDefaults" => serde_json::from_str(MOCK_CONFIG).unwrap_or_else(|_| json!({})),
        "schema.references" => {
            json!({})
        }
        // 按方案 id 分流：码表方案返回 dictionaries（含主库 + 三个附加库，
        // 对齐 data/schemas/wubi86.schema.toml）；其余（双拼等）保持布局样例。
        "schema.getConfig" => {
            let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
            // 码表族方案（五笔各版，排除混输）返回 dictionaries 样例；"codetable" 为
            // 未来通用码表 id 预留分支（当前 schema.list 未含此 id）。
            let is_codetable_family =
                (id.starts_with("wubi") && id != "wubi_mixed") || id == "codetable";
            if is_codetable_family {
                let mut v = json!({
                    "schema": { "id": id, "name": "五笔 86" },
                    "engine": { "type": "codetable" },
                    "dictionaries": [
                        { "id": "wubi86_main",  "label": "极点五笔主词库", "description": "含单字与高频词组", "default": true },
                        { "id": "wubi86_extra", "label": "扩展词库", "description": "新词、网络用语、生僻词等", "default_enabled": true },
                        { "id": "wubi86_emoji", "label": "Emoji 表情", "description": "按五笔编码检索 emoji", "default_enabled": true, "enabled": false },
                        { "id": "wubi86_xzqy",  "label": "行政区域", "description": "省市县区等地名词条", "default_enabled": false }
                    ]
                });
                // 套用本会话经 setDictEnabled 记忆的开关，令离线也能验证「保存后重开保留」。
                apply_mock_dict_overrides(id, &mut v);
                v
            } else {
                json!({
                    "engine": { "type": "pinyin", "pinyin": {
                        "scheme": "shuangpin", "shuangpin": { "layout": "ziranma" }
                    } }
                })
            }
        }
        "schema.setDictEnabled" => {
            let id = params
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let dict_id = params
                .get("dictId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let enabled = params
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            record_mock_dict_override(id, dict_id, enabled);
            json!({ "ok": true, "live": true })
        }
        "schema.saveConfig" => json!({ "ok": true }),
        "system.status" => json!({ "running": true }),
        "stats.summary" => json!({ "today": 0, "week": 0, "month": 0, "total": 0, "streak": 0 }),
        "dict.listPaged" => json!({ "items": [
            { "code": "aaaa", "text": "工", "weight": 0 },
            { "code": "ggkg", "text": "王中", "weight": 5 },
            { "code": "wghg", "text": "程序", "weight": 3 }
        ], "total": 3 }),
        // lastUsed 为 unix 秒（与真实 core 返回同形态，前端格式化为本地时间显示）。
        "freq.listPaged" => json!({ "items": [
            { "code": "nihao", "text": "你好", "count": 12, "lastUsed": 1_782_050_000i64 },
            { "code": "women", "text": "我们", "count": 5, "lastUsed": 1_781_900_000i64 }
        ], "total": 2 }),
        "dict.search" => json!([
            { "code": "aaaa", "text": "工", "weight": 0 }
        ]),
        // 加词自动出码：mock 返回占位码，便于对话框演示。
        "dict.encode" => json!("aaaa"),
        // 词库多段导入导出（sections 契约,形状对齐 core webdata.rs）。
        "dict.export" => {
            json!({ "content": "# WindInput 用户数据文件\nwind_dict:\n  version: 1\n  sections:\n    words:\n      columns: [code, text, weight, count]\n    shadow:\n      columns: [action, code, word, position, cand_id]\n\n--- !words\naaaa\t工\t0\t3\n\n--- !shadow\npin\tggkg\t王中\t0\t\n" })
        }
        "dict.import" => json!({ "sections": [
            { "key": "userWords", "added": 1, "updated": 0, "unchanged": 0, "skipped": 0 },
            { "key": "shadow", "imported": 1, "skipped": 0 }
        ] }),
        "dict.previewImport" => json!({ "format": "winddict", "compatible": true,
            "sourceSchema": "wubi86", "sourceEngine": "codetable", "targetEngine": "codetable",
            "sections": [
                { "key": "userWords", "count": 1, "willAdd": 1, "willUpdate": 0, "unchanged": 0, "skipped": 0, "samples": ["工"] },
                { "key": "shadow", "count": 1, "skipped": 0 }
            ] }),
        // 方案包导入导出（v2 契约：零层级条目名 + 可选 package 元信息）。
        "scheme.exportPackage" => {
            json!({ "path": "C:/tmp/demo.zip", "packed": ["demo.schema.toml"], "systemRefs": [], "missing": [] })
        }
        "scheme.previewImport" => json!({
            "package": {
                "package": { "app_version": "0.0.0", "platform": "windows", "created_at": "2026-07-13T00:00:00+08:00" },
                "schema": { "id": "demo", "version": "1.0" },
                "refs": { "system": [], "missing": [] }
            },
            "willAdd": ["demo.schema.toml"], "conflicts": [], "systemRefs": [], "missing": []
        }),
        "scheme.importPackage" => {
            json!({ "imported": ["demo.schema.toml"], "conflicts": [], "schemaIds": ["demo"] })
        }
        // 删除方案:级联清理结果(deleted/keptShared 供 toast 汇总)。
        "schema.delete" => json!({
            "ok": true,
            "deleted": ["demo.schema.toml", "demo/d.dict.yaml"],
            "keptShared": [],
            "schemaIds": ["demo"]
        }),
        // 整机备份（P4 契约）。
        "backup.create" | "backup.inspect" => json!({
            "path": "C:/tmp/backup.zip",
            "manifest": { "format": "windinput-bundle", "kind": "backup", "spec_version": 1,
                          "app_version": "0.0.0", "platform": "windows", "created_at": "2026-07-13T00:00:00+08:00",
                          "contents": [
                              { "type": "config", "path": "config/config.toml", "meta": null },
                              { "type": "dict", "path": "userdata/user_words/wb.wdict", "meta": { "schema": "wb" } },
                              { "type": "phrase", "path": "userdata/phrases.wdict", "meta": null }
                          ] }
        }),
        "backup.restore" => json!({
            "restored": ["config/config.toml", "userdata/user_words/wb.wdict"],
            "conflicts": [],
            "schemasTouched": ["wb"]
        }),
        "shadow.addRule" | "shadow.removeRule" => json!({ "ok": true }),
        _ => Value::Null,
    }
}
