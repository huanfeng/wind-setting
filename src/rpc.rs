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
        return mock_result(method, params);
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

// ─────────────────── 事件订阅（core → 设置端单向推送） ───────────────────
//
// 控制通道是请求-响应的，只能"问"，问不到 core 自发的状态变更（用户用快捷键/托盘
// 切了方案等）。core 另起了一条单向事件管道，这里订阅它，让界面能跟着变。

/// 断线后的重连间隔。core 未启动时会一直按此节奏重试，单次成本仅一次 open 失败。
const EVENTS_RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_secs(3);

/// 事件通道端点。**命名规则与 [`ctrl_endpoint`] 不一致**：core 侧 ctrl 是末尾化
/// （`wind_input_ctrl{suffix}`），events 是中间化（`wind_input{suffix}_events`）——
/// 见 wind-rpc/src/server.rs 的 `ctrl_endpoint` / `events_endpoint`。
///
/// 这里手抄格式而非直接调 core 的函数：wind-rpc 只是 dev-dependency（运行时依赖
/// 刻意只保留轻量的 wind-ipc，见 Cargo.toml）。抄错的后果是静默连不上——事件永远
/// 收不到，而不是报错，故由 [`tests::events_endpoint_matches_core`] 对账兜住。
pub fn events_endpoint() -> String {
    let suffix = endpoint_suffix();
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\wind_input{suffix}_events")
    }
    #[cfg(unix)]
    {
        let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        format!("{dir}/wind_input{suffix}_events.sock")
    }
}

/// core 广播的一条事件（线路格式 `{event, data}`，帧编码与控制通道同为
/// 4 字节大端长度 + JSON，故可复用 [`read_frame`]）。
#[derive(Debug, Clone)]
pub struct CoreEvent {
    pub event: String,
    pub data: Value,
}

/// 打开事件流。Windows 侧 core 用的是单向管道（`PIPE_ACCESS_OUTBOUND`），
/// **必须只读打开**——带上 write 权限会被拒（不同于双向的 ctrl 通道）。
#[cfg(windows)]
fn open_events_stream(endpoint: &str) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().read(true).open(endpoint)
}

#[cfg(unix)]
fn open_events_stream(endpoint: &str) -> std::io::Result<std::os::unix::net::UnixStream> {
    std::os::unix::net::UnixStream::connect(endpoint)
}

/// 解析一帧事件载荷（已剥去长度前缀的 JSON）。`event` 缺失即视为不可解析。
/// 抽成纯函数供 [`tests::parse_event_frame_matches_core_encoding`] 与 core 的
/// `EventMessage` 序列化对账。
fn parse_event_frame(payload: &[u8]) -> Option<CoreEvent> {
    let v: Value = serde_json::from_slice(payload).ok()?;
    let event = v.get("event")?.as_str()?.to_string();
    Some(CoreEvent {
        event,
        data: v.get("data").cloned().unwrap_or(Value::Null),
    })
}

/// 阻塞订阅 core 事件，每收到一条调一次 `on_event`；断线自动重连，**永不返回**。
/// 调用方应在专用后台线程里跑它。mock 模式无 core 可连，直接返回。
pub fn listen_events(mut on_event: impl FnMut(CoreEvent)) {
    if is_mock() {
        return;
    }
    let endpoint = events_endpoint();
    loop {
        match open_events_stream(&endpoint) {
            Ok(mut stream) => {
                tracing::info!("已订阅 core 事件通道 {}", endpoint);
                // 读出错即 core 退出或管道关闭，跳出由外层重连。
                while let Ok(frame) = read_frame(&mut stream) {
                    match parse_event_frame(&frame) {
                        Some(ev) => on_event(ev),
                        None => tracing::warn!("事件帧无法解析，已跳过"),
                    }
                }
                tracing::info!("core 事件通道已断开，{EVENTS_RECONNECT_DELAY:?} 后重连");
            }
            // core 未启动是常态（设置界面可独立打开），故只记 debug。
            Err(e) => tracing::debug!("事件通道连接失败({e})，稍后重试"),
        }
        std::thread::sleep(EVENTS_RECONNECT_DELAY);
    }
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

    /// 守护：事件帧的解析必须与 core 实际发出的 `EventMessage` 序列化结果对得上。
    /// 用 core 自己的类型构造输入（wind-ipc 是硬依赖），字段改名会在此处失败，
    /// 而不是等到运行时表现为"事件收得到但内容全空"。
    #[test]
    fn parse_event_frame_matches_core_encoding() {
        let msg = wind_ipc::rpc::EventMessage {
            event: "schema.activeChanged".to_string(),
            data: json!({ "id": "wubi86" }),
        };
        let payload = serde_json::to_vec(&msg).expect("EventMessage 可序列化");
        let ev = super::parse_event_frame(&payload).expect("应能解析 core 的事件帧");
        assert_eq!(ev.event, "schema.activeChanged");
        assert_eq!(ev.data.get("id").and_then(|v| v.as_str()), Some("wubi86"));

        // 非 JSON / 缺 event 字段：跳过而不是 panic。
        assert!(super::parse_event_frame(b"not json").is_none());
        assert!(super::parse_event_frame(br#"{"data":{}}"#).is_none());
    }

    /// 守护：事件端点必须与 core 的 `wind_rpc::events_endpoint` 逐字一致。
    ///
    /// 抄错不会报错，只会静默收不到任何事件——「当前方案」之类的显示就悄悄退回
    /// 打开那一刻的快照，且看起来一切正常。这种失败模式必须由测试兜住。
    /// 主代码不能直接调该函数（wind-rpc 是 dev-dependency），故用对账代替复用。
    #[test]
    fn events_endpoint_matches_core() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 覆盖 release（空后缀）与 dev 两种变体。
        for suffix in ["", "_dev"] {
            unsafe { std::env::set_var("WIND_PIPE_SUFFIX", suffix) };
            assert_eq!(
                super::events_endpoint(),
                wind_rpc::events_endpoint(suffix),
                "事件端点与 core 漂移(suffix={suffix:?})"
            );
        }
        unsafe { std::env::remove_var("WIND_PIPE_SUFFIX") };
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
            "mockdata/config.json 与 core 系统预置漂移 {} 处，请重新生成：\n\
             cargo test regenerate_mock_config -- --ignored\n{}",
            problems.len(),
            problems.join("\n")
        );
    }

    /// 重新生成 `mockdata/config.json`（**非自动运行**，需显式 `-- --ignored`）。
    ///
    /// 上面的守护测试要求「重新生成而不是逐个手改」，此前却没有生成入口，只能照着
    /// 报错逐条补——正是它要防的手抄漂移。core 改了配置默认值后跑：
    /// `cargo test regenerate_mock_config -- --ignored`
    ///
    /// [`CURATED_MOCK_VALUES`] 中的键沿用旧 mock 的值：它们**有意**偏离 core 预置，
    /// 直接覆盖会把那份策展改掉（`curated_mock_values_still_differ` 会当场发现）。
    ///
    /// 缩进用 1 空格而非 `to_string_pretty` 的 2 空格，与检入格式一致——否则一次
    /// 再生成会把 300 余行全部重排，真正的改动淹没在噪声里。
    #[test]
    #[ignore = "写文件的维护工具，按需显式运行"]
    fn regenerate_mock_config() {
        use serde::Serialize;

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let preset =
            wind_config::Config::system_preset_value(Some(&root.join("../WindInput/data")))
                .expect("读取 core 系统预置失败");
        let cfg: wind_config::Config = preset.try_into().expect("系统预置反序列化为 Config 失败");
        let mut fresh = serde_json::to_value(&cfg).expect("Config 序列化失败");

        let old: Value = serde_json::from_str(MOCK_CONFIG).expect("旧 mockdata 解析失败");
        for key in CURATED_MOCK_VALUES {
            let ptr = format!("/{}", key.replace('.', "/"));
            let want = old.pointer(&ptr).expect("策展键不在旧 mockdata 中").clone();
            *fresh.pointer_mut(&ptr).expect("策展键不在 core 配置中") = want;
        }

        let mut buf = Vec::new();
        let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
        fresh.serialize(&mut ser).expect("序列化失败");
        buf.push(b'\n'); // 与检入格式一致（EOF 换行）

        let path = root.join("src/mockdata/config.json");
        std::fs::write(&path, buf).expect("写入 mockdata 失败");
        println!("已重新生成 {}", path.display());
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

    /// 守护：统计样例必须能被页面的数据层按 core 契约解析。
    ///
    /// mock 与 `data::DailyItem`/`Summary` 的字段名（core 的压缩缩写）一旦对不上，
    /// 表现是「离线模式统计页全零」——静默且容易误判成"功能没做"，故在此锁死。
    #[test]
    fn mock_stats_parses_as_page_data() {
        use crate::pages::stats::data::{DailyItem, Summary};

        let daily = super::mock_reply(
            "stats.daily",
            &json!({ "from": "2026-01-01", "to": "2026-01-31" }),
        );
        let days: Vec<DailyItem> = serde_json::from_value(daily).expect("daily 应能解析");
        assert_eq!(days.len(), 31, "区间内每天都要有一条（含零值日）");
        assert!(days.iter().any(|d| d.total > 0), "样例不应全是空白日");
        assert!(
            days.iter().any(|d| d.total == 0),
            "样例应含空白日，否则热力图色阶 0 档无从核对"
        );
        // 时段分布必须与当日总量自洽，否则「占今日 x%」会算出 >100%。
        for d in days.iter().filter(|d| d.total > 0) {
            assert_eq!(
                d.hours.iter().sum::<u32>(),
                d.total,
                "{} 的时段分布之和应等于总字数",
                d.date
            );
            assert!(!d.by_schema.is_empty(), "{} 应有方案分项", d.date);
        }

        let summary: Summary =
            serde_json::from_value(super::mock_reply("stats.summary", &json!({})))
                .expect("summary 应能解析");
        assert!(summary.total_chars > 0);
        assert!(summary.active_days > 0);
        assert!(
            summary.avg_code_len > 0.0 && summary.first_select_rate > 0.0,
            "码长/首选率应非零，否则详情区整块隐藏"
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

    /// mock 的配置片段导入形状须与 core 契约一致：previewPatch 每条含 key/next
    /// （current 可缺席=未设置，合法条目无 error）；applyPatch 含 ok/applied/needsRestart。
    #[test]
    fn mock_config_patch_shapes() {
        let p = mock_reply("config.previewPatch");
        assert_eq!(p.get("ok"), Some(&json!(true)));
        let entries = p
            .get("entries")
            .and_then(|v| v.as_array())
            .expect("entries 数组");
        assert!(!entries.is_empty());
        assert!(
            entries
                .iter()
                .all(|e| e.get("key").is_some() && e.get("next").is_some())
        );
        assert!(
            entries.iter().all(|e| e.get("error").is_none()),
            "合法条目不带 error"
        );

        // Map 键的条目行：key = 父键、mapEntry = 条目名（条目名不并进点分键）。
        let map_row = entries
            .iter()
            .find(|e| e.get("key") == Some(&json!("keys.key_actions")))
            .expect("样例应含一条 Map 条目");
        assert_eq!(map_row.get("mapEntry"), Some(&json!("backtick")));

        let a = mock_reply("config.applyPatch");
        assert_eq!(a.get("ok"), Some(&json!(true)));
        assert!(a.get("applied").and_then(|v| v.as_u64()).is_some());
        assert!(a.get("needsRestart").and_then(|v| v.as_bool()).is_some());
        // written = 落盘后的最终键值；Map 父键带合并后的整表（不止片段里那一条）。
        let written = a
            .get("written")
            .and_then(|v| v.as_array())
            .expect("written 数组");
        assert!(
            written
                .iter()
                .all(|w| w.get("key").is_some() && w.get("value").is_some())
        );
        let map_written = written
            .iter()
            .find(|w| w.get("key") == Some(&json!("keys.key_actions")))
            .and_then(|w| w.get("value"))
            .and_then(|v| v.as_object())
            .expect("Map 父键的 written 值应是整表");
        assert!(
            map_written.len() > 1,
            "Map 父键必须带合并后的整表（含片段未提及的既有条目），实际: {map_written:?}"
        );
    }

    /// 文本信封两法：内容是信封才成功，否则回 `not_schema_text:` 前缀的错误——
    /// 统一导入通路靠这个前缀回落片段管线，mock 只 Ok 的话离线走不到片段那一支。
    #[test]
    fn mock_schema_text_envelope_detection() {
        let envelope = "[package]\nformat_version = 2\nkind = \"schema_text\"\n";
        let fragment = "[ui.candidate]\nper_page = 9\n";

        let err = super::mock_result("scheme.previewImportText", &json!({ "text": fragment }))
            .expect_err("片段不是信封");
        assert!(
            err.starts_with(crate::pages::config_import::NOT_SCHEMA_TEXT_PREFIX),
            "回落判据是前缀，实际: {err}"
        );

        let v = super::mock_result("scheme.previewImportText", &json!({ "text": envelope }))
            .expect("信封应预览成功");
        assert!(v.get("willAdd").is_some(), "形状须与 previewImport 一致");
        assert!(
            v.pointer("/configPatch/entries")
                .and_then(|e| e.as_array())
                .is_some_and(|a| !a.is_empty()),
            "样例信封应带 configPatch，否则离线核对不到「将修改的配置」区块"
        );

        let imported = super::mock_result("scheme.importText", &json!({ "text": envelope }))
            .expect("信封应导入成功");
        assert!(imported.get("imported").is_some());
        // 单引号写法同样算信封（生成端可自选 TOML 引法）。
        assert!(super::mock_is_schema_text("kind = 'schema_text'"));
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

/// 统计样例的确定性伪随机源：同一天恒定，故截图可复现（不用 rand，避免引入依赖）。
fn mock_noise(seed: u64, salt: u64) -> u64 {
    let mut x = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(salt.wrapping_mul(1_442_695_040_888_963_407));
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 29;
    x
}

/// 统计页离线样例：区间内逐日生成（含零值日），形状对齐 core `daily_item_json`。
///
/// 刻意做出「工作日多、周末少、偶有整日空档」的分布，否则热力图全是同一档色阶，
/// 截图核对色阶与留白就失去意义。
fn mock_stats_daily(params: &Value) -> Value {
    use chrono::Datelike;
    let get = |k: &str| params.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let parse = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok();
    let (Some(from), Some(to)) = (parse(get("from")), parse(get("to"))) else {
        return json!([]);
    };

    let mut out = Vec::new();
    let mut cur = from;
    while cur <= to {
        let seed = cur.num_days_from_ce() as u64;
        let weekday = cur.weekday().num_days_from_monday();
        // 8% 的日子整日无输入（休假/未开机）；周末产出约为工作日四成。
        let idle = mock_noise(seed, 1) % 100 < 8;
        let scale = if weekday >= 5 { 0.4 } else { 1.0 };
        let base = if idle {
            0
        } else {
            ((800 + mock_noise(seed, 2) % 3_700) as f64 * scale) as u32
        };

        let chinese = base * 70 / 100;
        let english = base * 18 / 100;
        let punct = base * 8 / 100;
        let other = base - chinese - english - punct;
        let total = base;

        // 时段：上午 9-12、下午 14-18、晚间 20-23 三个峰，其余低谷。
        let mut hours = [0u32; 24];
        if total > 0 {
            let weights: [u32; 24] = [
                0, 0, 0, 0, 0, 0, 1, 2, 4, 9, 10, 9, 5, 6, 9, 10, 9, 8, 5, 4, 7, 8, 6, 3,
            ];
            let wsum: u32 = weights.iter().sum();
            let mut acc = 0u32;
            for h in 0..24 {
                // 末位吸收余数，保证 sum(hours) == total（页面按今日总量算占比）。
                hours[h] = if h == 23 {
                    total - acc
                } else {
                    let v = total * weights[h] / wsum;
                    acc += v;
                    v
                };
            }
        }

        let commit_count = total / 3;
        // 码长以 4 码为主，符合码表方案的实际分布。
        let cld_w = [2u32, 8, 22, 52, 12, 4];
        let mut code_len_dist = [0u32; 6];
        let mut cl_sum = 0u32;
        for (i, w) in cld_w.iter().enumerate() {
            code_len_dist[i] = commit_count * w / 100;
            cl_sum += code_len_dist[i] * (i as u32 + 1);
        }
        let code_len_count: u32 = code_len_dist.iter().sum();
        // 候选位置：首选约 78%，越靠后越少。
        let cpd_w = [78u32, 13, 5, 3, 1];
        let mut cand_pos_dist = [0u32; 5];
        for (i, w) in cpd_w.iter().enumerate() {
            cand_pos_dist[i] = commit_count * w / 100;
        }

        // 活跃秒数反推出 55~85 字/分的速度区间。
        let speed = 55 + mock_noise(seed, 3) % 31;
        let active_seconds = if total > 0 {
            (total as u64 * 60 / speed) as u32
        } else {
            0
        };

        // 双方案共存，便于验证「方案占比」条形图（id 取自 mock 的 schema.list）。
        let wubi = total * 62 / 100;
        let pinyin = total - wubi;
        let by_schema = if total > 0 {
            json!({
                "wubi86": { "tc": wubi, "cn": wubi / 3, "cls": cl_sum * 62 / 100,
                            "clc": code_len_count * 62 / 100, "cpd": cand_pos_dist },
                "pinyin_simp": { "tc": pinyin, "cn": pinyin / 3, "cls": cl_sum * 38 / 100,
                                 "clc": code_len_count * 38 / 100, "cpd": cand_pos_dist },
            })
        } else {
            json!({})
        };

        out.push(json!({
            "d": cur.format("%Y-%m-%d").to_string(),
            "tc": total, "cc": chinese, "ec": english, "pc": punct, "oc": other,
            "h": hours, "cn": commit_count,
            "cls": cl_sum, "clc": code_len_count,
            "cld": code_len_dist, "cpd": cand_pos_dist,
            "as": active_seconds, "bs": by_schema,
            "src": [commit_count, 0, 0],
        }));
        cur += chrono::Duration::days(1);
    }
    json!(out)
}

/// 统计概览样例（17 字段，形状对齐 core `web_stats_summary`）。
///
/// 由 `mock_stats_daily` 的同一份样例汇总而来——两个接口若各编各的，
/// 页面上「今日输入」和热力图最后一格会对不上，离线核对反而误导。
fn mock_stats_summary() -> Value {
    let today = chrono::Local::now();
    let from = (today - chrono::Duration::days(180))
        .format("%Y-%m-%d")
        .to_string();
    let to = today.format("%Y-%m-%d").to_string();
    let days = mock_stats_daily(&json!({ "from": from, "to": to }));
    let days = days.as_array().cloned().unwrap_or_default();
    let num = |d: &Value, k: &str| d.get(k).and_then(|v| v.as_u64()).unwrap_or(0);

    let last = days.last().cloned().unwrap_or_else(|| json!({}));
    let today_chars = num(&last, "tc");
    let window_total: u64 = days.iter().map(|d| num(d, "tc")).sum();
    let active_days = days.iter().filter(|d| num(d, "tc") > 0).count() as u64;
    // 窗口外还有更早的历史，累计量取窗口和的 1.6 倍，避免「累计 == 半年」的失真。
    let total_chars = window_total * 8 / 5;

    // 连续天数：从今天往回数到第一个空白日。
    let streak_current = days.iter().rev().take_while(|d| num(d, "tc") > 0).count() as u64;
    let streak_max = streak_current.max(21);

    let (max_day_chars, max_day_date) = days.iter().fold((0u64, String::new()), |acc, d| {
        let t = num(d, "tc");
        if t > acc.0 {
            (
                t,
                d.get("d")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        } else {
            acc
        }
    });

    let week_chars: u64 = days.iter().rev().take(7).map(|d| num(d, "tc")).sum();
    let month_chars: u64 = days.iter().rev().take(30).map(|d| num(d, "tc")).sum();
    let cl_sum: u64 = days.iter().map(|d| num(d, "cls")).sum();
    let cl_cnt: u64 = days.iter().map(|d| num(d, "clc")).sum();
    let first_sel: u64 = days
        .iter()
        .map(|d| {
            d.get("cpd")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        })
        .sum();
    let cand_sel: u64 = days
        .iter()
        .map(|d| {
            d.get("cpd")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_u64()).sum::<u64>())
                .unwrap_or(0)
        })
        .sum();
    let active_sec: u64 = days.iter().map(|d| num(d, "as")).sum();
    let speed = |chars: u64, secs: u64| if secs == 0 { 0 } else { chars * 60 / secs };

    json!({
        "today_chars": today_chars,
        "today_chinese": num(&last, "cc"),
        "today_english": num(&last, "ec"),
        "total_chars": total_chars,
        "active_days": active_days,
        "daily_avg": if active_days > 0 { total_chars / active_days } else { 0 },
        "streak_current": streak_current,
        "streak_max": streak_max,
        "week_chars": week_chars,
        "month_chars": month_chars,
        "max_day_chars": max_day_chars,
        "max_day_date": max_day_date,
        "avg_code_len": if cl_cnt > 0 { cl_sum as f64 / cl_cnt as f64 } else { 0.0 },
        "first_select_rate": if cand_sel > 0 { first_sel as f64 / cand_sel as f64 } else { 0.0 },
        "today_speed": speed(today_chars, num(&last, "as")),
        "overall_speed": speed(window_total, active_sec),
        "max_speed": 118,
    })
}

/// mock 出码：每字取一个占位字母；含 BMP 外字符（扩展区生僻字）时回空串，
/// 用来复现真实出码里「方案未配拆字 → 出不了码」这一支。
fn mock_word_code(text: &str) -> String {
    if text.is_empty() || text.chars().any(|c| c as u32 > 0xFFFF) {
        return String::new();
    }
    text.chars()
        .zip("abcdefghijklmnopqrstuvwxyz".chars().cycle())
        .map(|(_, letter)| letter)
        .collect()
}

/// Mock 模式的完整结果：**先判错再取值**。
///
/// 没有这一层的话 mock 只会 `Ok`，而统一导入通路恰恰靠「`scheme.previewImportText`
/// 报 `not_schema_text:`」来回落片段管线——离线模式下每段文本都会被当成方案信封，
/// 片段导入整条路走不到。
fn mock_result(method: &str, params: &Value) -> Result<Value, String> {
    #[cfg(test)]
    MOCK_CALLS.with(|c| c.borrow_mut().push((method.to_string(), params.clone())));
    match mock_error(method, params) {
        Some(e) => Err(e),
        None => Ok(mock_reply(method, params)),
    }
}

#[cfg(test)]
thread_local! {
    /// Mock 模式下按序记录每次调用的 (method, params)。
    ///
    /// 返回值对不对是一回事，**请求里少个字段**是另一回事——后者只有接上真实 core
    /// 才会暴露，本地怎么点都是绿的。多步编排（导入两法 + applyPatch）尤其需要锁住
    /// 发出去的参数形状与先后顺序。mock 分支不 spawn 线程，与调用方同线程，故
    /// thread_local 够用。
    static MOCK_CALLS: std::cell::RefCell<Vec<(String, Value)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// 取走并清空本线程记录的 mock 调用（取走语义：便于「清场 → 触发 → 断言」）。
#[cfg(test)]
pub(crate) fn take_mock_calls() -> Vec<(String, Value)> {
    MOCK_CALLS.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// mock 侧会「失败」的调用。目前只有文本信封两法：内容不是信封时按跨仓契约回
/// `not_schema_text:` 前缀的错误（见 `package-format.md` §3.4）。
fn mock_error(method: &str, params: &Value) -> Option<String> {
    match method {
        "scheme.previewImportText" | "scheme.importText" => {
            let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
            (!mock_is_schema_text(text))
                .then(|| "not_schema_text: 缺少 package.kind = \"schema_text\"".to_string())
        }
        _ => None,
    }
}

/// mock 的信封判定：只看 `kind` 是否声明为 `schema_text`（真实解析在 core 侧，
/// 这里只需分出「信封 / 不是信封」两支供离线走通两条通路）。
fn mock_is_schema_text(text: &str) -> bool {
    text.contains("kind = \"schema_text\"") || text.contains("kind = 'schema_text'")
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
        // 当前活跃方案（core 的运行时状态）。取值必须在上面的 schema.list 内，
        // 否则 mock 模式下「当前」徽章会无处落脚。
        "schema.active" => json!({ "id": "wubi86" }),
        // mock 无状态：只回 ok，UI 侧自行更新 active_id（真实 core 才有切换副作用）。
        "schema.setActive" => json!({ "ok": true }),
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
        "dict.stats" => json!([]),
        "stats.daily" => mock_stats_daily(params),
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
        // 快捷输入格式表。夹具刻意覆盖四种状态：普通行、被停用的行（`moveIndex` 为 null，
        // 调序按钮应置灰）、调整过的行（`adjusted` 为真，「恢复此条」才可点）、以及**用户
        // 自己加的行**（`user` 为真，可改模板/可删、但「恢复此条」不可点）。
        // 只给两个类别即可——UI 侧的类别集是从数据里现取的，不写死五个。
        "quick.list" => json!([
            { "kind": "date", "id": "date.cn", "text": "$Y年$M月$D日", "displayPos": 1,
              "moveIndex": 0, "enabled": true, "adjusted": false, "user": false, "sample": "2026年8月17日" },
            { "kind": "date", "id": "date.iso", "text": "$YYYY-$MM-$DD", "displayPos": 2,
              "moveIndex": 1, "enabled": true, "adjusted": true, "user": false, "sample": "2026-08-17" },
            { "kind": "date", "id": "date.basic", "text": "$YYYY$MM$DD", "displayPos": 3,
              "moveIndex": null, "enabled": false, "adjusted": true, "user": false, "sample": "20260817" },
            { "kind": "date", "id": "date.u1", "text": "$Y/$M/$D", "displayPos": 4,
              "moveIndex": 2, "enabled": true, "adjusted": false, "user": true, "sample": "2026/8/17" },
            { "kind": "number", "id": "number.amount", "text": "$AMT", "displayPos": 1,
              "moveIndex": 0, "enabled": true, "adjusted": false, "user": false, "sample": "壹仟贰佰叁拾肆元伍角整" }
        ]),
        "quick.move" | "quick.setEnabled" | "quick.resetEntry" | "quick.resetKind"
        | "quick.setText" | "quick.delete" => json!({ "ok": true }),
        "quick.add" => json!({ "ok": true, "id": "date.u2" }),
        // 变量提示：只给两类、每类两条即可——真值来自 core，这里只让离线预览有东西可显。
        "quick.vars" => json!([
            { "kind": "date", "vars": [
                { "name": "Y", "desc": "年，如 2026" },
                { "name": "LMD", "desc": "农历月日，四月廿九" }
            ]},
            { "kind": "number", "vars": [
                { "name": "AMT", "desc": "金额大写，壹仟贰佰叁拾肆元伍角" }
            ]}
        ]),
        "quick.export" => json!({
            "content": "# WindInput 快捷输入 · 用户设置\n\n[[formats]]\nid = 'date.u1'\nkind = 'date'\ntext = '$Y/$M/$D'\nposition = 0\n\n[[adjust]]\nkind = 'date'\ndisabled = ['date.basic']\n"
        }),
        "quick.previewImport" => {
            json!({ "moved": 1, "disabled": 1, "formats": 1, "skipped": [], "kinds": ["date"] })
        }
        "quick.import" => json!({ "moved": 1, "disabled": 1, "formats": 1, "skipped": [] }),
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
        "stats.summary" => mock_stats_summary(),
        "stats.clear" => json!({ "ok": true }),
        "stats.pruneBefore" => json!({ "count": 0, "before": "" }),
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
        // 批量出码（纯词列表导入）。契约要害是 **codes 与 texts 同序等长**，
        // 出不了码回空串而非省略——客户端靠下标把码配回词。mock 用「BMP 外的生僻字
        // 出不了码」来复现真实的部分失败，好让 UI 那条跳过分支也能离线验证。
        "dict.encodeWords" => {
            let codes: Vec<Value> = params
                .get("texts")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|t| json!(mock_word_code(t.as_str().unwrap_or(""))))
                        .collect()
                })
                .unwrap_or_default();
            json!({ "codes": codes })
        }
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
        // 文本信封（§3.4）。响应形状与 zip 两法一致；这里刻意带上 configPatch，
        // 让「将修改的配置」区块（含 Map 条目显示键）离线也能核对。
        "scheme.previewImportText" => json!({
            "package": {
                "package": { "app_version": "0.0.0", "platform": "windows", "created_at": "2026-08-19T00:00:00+08:00" },
                "schema": { "id": "kf", "version": "1.00.0" },
                "refs": { "system": [], "missing": [] }
            },
            "willAdd": ["kf.schema.toml", "flypy/12_kf.dict.yaml"], "conflicts": [], "systemRefs": [], "missing": [],
            "configPatch": {
                "text": "[keys.key_actions]\nbacktick = \"quick_input\"\n",
                "ok": true,
                "entries": [
                    { "key": "keys.key_actions", "mapEntry": "backtick", "next": "quick_input" }
                ]
            }
        }),
        "scheme.importText" => {
            json!({ "imported": ["kf.schema.toml", "flypy/12_kf.dict.yaml"], "conflicts": [], "schemaIds": ["kf"] })
        }
        // 删除方案:级联清理结果(deleted/keptShared 供 toast 汇总)。
        "schema.delete" => json!({
            "ok": true,
            "deleted": ["demo.schema.toml", "demo/d.dict.yaml"],
            "keptShared": [],
            "schemaIds": ["demo"]
        }),
        // 配置片段导入（core 契约：previewPatch 只读逐条校验，current 缺席=未设置；
        // applyPatch 全部合法才落盘，逐键错误时整体为 RPC error）。
        // Map 键（key_actions）刻意给一条：预览逐**条目**报告、显示键为
        // `父键[条目名]`，且 written 回传的是合并后的整表（§2.2 / §2.3）。
        "config.previewPatch" => json!({
            "ok": true,
            "entries": [
                { "key": "ui.candidate.per_page", "current": 5, "next": 9 },
                { "key": "input.initial_mode", "next": "chinese" },
                { "key": "keys.key_actions", "mapEntry": "backtick", "next": "quick_input" }
            ]
        }),
        // written = 落盘后的最终键值：Map 父键带的是合并结果整表（片段只写了
        // backtick，semicolon 是用户原有条目），客户端拼不出来，必须由 core 回传。
        "config.applyPatch" => json!({
            "ok": true, "applied": 3, "needsRestart": false,
            "written": [
                { "key": "ui.candidate.per_page", "value": 9 },
                { "key": "input.initial_mode", "value": "chinese" },
                { "key": "keys.key_actions",
                  "value": { "semicolon": "temp_pinyin", "backtick": "quick_input" } }
            ]
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
