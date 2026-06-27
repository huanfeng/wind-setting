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
        format!(r"\\.\pipe\wind_input{suffix}_ctrl")
    }
    #[cfg(unix)]
    {
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

/// 探测核心是否在运行。
pub fn probe_core_running() -> bool {
    call_core("system.status", &json!({})).is_ok()
}

/// 是否处于离线 mock 模式。
pub fn is_mock() -> bool {
    std::env::var("WIND_RPC_MOCK").as_deref() == Ok("1")
}

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
            { "id": "wubi86", "name": "五笔 86", "kind": "codetable" },
            { "id": "wubi98", "name": "五笔 98", "kind": "codetable" },
            { "id": "pinyin_simp", "name": "全拼·简", "kind": "pinyin" },
            { "id": "double_pinyin", "name": "自然码双拼", "kind": "pinyin" }
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
        "phrase.list" | "shadow.list" | "dict.stats" | "temp.list" | "stats.daily" => json!([]),
        "config.getDefaults" | "schema.getConfig" | "schema.references" | "theme.preview" => {
            json!({})
        }
        "system.status" => json!({ "running": true }),
        "stats.summary" => json!({ "today": 0, "week": 0, "month": 0, "total": 0, "streak": 0 }),
        "dict.listPaged" | "freq.listPaged" => json!({ "items": [], "total": 0 }),
        "dict.search" => json!([]),
        _ => Value::Null,
    }
}
