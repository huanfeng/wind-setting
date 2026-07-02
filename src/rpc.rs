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
    std::env::var("WIND_PIPE_SUFFIX")
        .unwrap_or_else(|_| crate::mode::pipe_suffix().to_string())
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

/// 连接核心并完成一次 RPC 调用。
pub fn call_core(method: &str, params: &Value) -> Result<Value, String> {
    if std::env::var("WIND_RPC_MOCK").as_deref() == Ok("1") {
        return Ok(mock_reply(method));
    }
    let endpoint = ctrl_endpoint();
    let t = std::time::Instant::now();
    let result = call_core_inner(&endpoint, method, params);
    match &result {
        Ok(_)  => tracing::debug!("rpc {} ok ({:?})", method, t.elapsed()),
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

    #[test]
    fn test_ctrl_endpoint_format() {
        let ep = ctrl_endpoint();
        assert!(ep.contains("wind_input"));
        assert!(ep.contains("_ctrl"));
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
}

/// 内置 capability 快照（与 src/assets 同一文件），mock 模式离线提供能力契约。
const MOCK_CAPABILITIES: &str = include_str!("assets/capabilities.snapshot.json");
const MOCK_CONFIG: &str = include_str!("mockdata/config.json");

/// Mock 模式：返回形状合理的默认值；manifest/config 用内置真实数据，
/// 方案/主题/字体/系统信息给代表性样例，便于离线核对 UI 保真度。
fn mock_reply(method: &str) -> Value {
    match method {
        "system.capabilities" => serde_json::from_str(MOCK_CAPABILITIES).unwrap_or_else(|_| json!({})),
        "config.get" => serde_json::from_str(MOCK_CONFIG).unwrap_or_else(|_| json!({})),
        "schema.list" => json!([
            { "id": "wubi86", "name": "五笔 86", "engineType": "codetable", "builtin": true,
              "version": "2.1.0", "description": "标准五笔 86 版码表方案", "author": "王永民" },
            { "id": "wubi98", "name": "五笔 98", "engineType": "codetable", "builtin": true,
              "version": "1.0.0", "description": "标准五笔 98 版码表方案" },
            { "id": "wubi_mixed", "name": "五笔拼音混输", "engineType": "mixed", "builtin": true,
              "version": "1.2.0", "description": "五笔 86 + 拼音混合输入" },
            { "id": "pinyin_simp", "name": "全拼·简", "engineType": "pinyin", "builtin": true,
              "description": "简体全拼方案" },
            { "id": "double_pinyin", "name": "自然码双拼", "engineType": "pinyin", "builtin": false,
              "version": "0.5.0", "description": "自然码双拼布局" }
        ]),
        "theme.list" => json!([
            { "id": "default_light", "name": "清风·浅" },
            { "id": "default_dark", "name": "清风·深" },
            { "id": "mono", "name": "墨" }
        ]),
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
        "temp.list" => json!([
            { "code": "nihao", "text": "你好", "count": 3 },
            { "code": "women", "text": "我们", "count": 7 }
        ]),
        "shadow.list" => json!([
            { "code": "wm", "word": "我们", "candId": "c1", "type": "pin", "position": 0 },
            { "code": "nh", "word": "你好", "candId": null, "type": "delete" }
        ]),
        "config.getDefaults" | "schema.getConfig" | "schema.references" | "theme.preview" => {
            json!({})
        }
        "system.status" => json!({ "running": true }),
        "stats.summary" => json!({ "today": 0, "week": 0, "month": 0, "total": 0, "streak": 0 }),
        "dict.listPaged" => json!({ "items": [
            { "code": "aaaa", "text": "工", "weight": 0 },
            { "code": "ggkg", "text": "王中", "weight": 5 },
            { "code": "wghg", "text": "程序", "weight": 3 }
        ], "total": 3 }),
        "freq.listPaged" => json!({ "items": [
            { "code": "nihao", "text": "你好", "count": 12, "lastUsed": "2026-06-30" },
            { "code": "women", "text": "我们", "count": 5, "lastUsed": "2026-06-28" }
        ], "total": 2 }),
        "dict.search" => json!([
            { "code": "aaaa", "text": "工", "weight": 0 }
        ]),
        // 加词自动出码：mock 返回占位码，便于对话框演示。
        "dict.encode" => json!("aaaa"),
        "shadow.addRule" | "shadow.removeRule" => json!({ "ok": true }),
        _ => Value::Null,
    }
}
