//! 本地主题编辑器 HTTP 服务：只监听 loopback，并限制 Host/Origin。

use std::net::IpAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::rpc::call_core;
use crate::theme_import::{import_text, ThemeImportError};

#[derive(Debug, Clone, Default)]
pub struct ThemeEditorServerStatus {
    pub running: bool,
    pub url: Option<String>,
    pub port: Option<u16>,
}

pub struct ThemeEditorServer {
    status: Arc<Mutex<ThemeEditorServerStatus>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpReply {
    status: u16,
    body: String,
    allow_origin: Option<String>,
}

impl Default for ThemeEditorServer {
    fn default() -> Self {
        Self {
            status: Arc::new(Mutex::new(ThemeEditorServerStatus::default())),
            stop: Arc::new(AtomicBool::new(false)),
            handle: None,
        }
    }
}

impl ThemeEditorServer {
    pub fn status(&self) -> ThemeEditorServerStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn start(&mut self) -> Result<ThemeEditorServerStatus, String> {
        if self.status().running {
            return Ok(self.status());
        }

        let mut selected = None;
        for port in 29731..=29733 {
            match tiny_http::Server::http(("127.0.0.1", port)) {
                Ok(server) => {
                    selected = Some((port, server));
                    break;
                }
                Err(_) => continue,
            }
        }
        let (port, server) = selected
            .ok_or_else(|| "无法启动主题编辑器连接：29731-29733 端口均被占用".to_string())?;

        self.stop.store(false, Ordering::Relaxed);
        let stop = Arc::clone(&self.stop);
        let status_handle = Arc::clone(&self.status);
        let handle = std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match server.recv_timeout(Duration::from_millis(200)) {
                    Ok(Some(req)) => handle_request(req),
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
            if let Ok(mut s) = status_handle.lock() {
                *s = ThemeEditorServerStatus::default();
            }
        });

        let status = ThemeEditorServerStatus {
            running: true,
            url: Some(format!("http://localhost:{port}")),
            port: Some(port),
        };
        *self
            .status
            .lock()
            .map_err(|_| "主题编辑器状态锁异常".to_string())? = status.clone();
        self.handle = Some(handle);
        Ok(status)
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        if let Ok(mut s) = self.status.lock() {
            *s = ThemeEditorServerStatus::default();
        }
    }
}

impl Drop for ThemeEditorServer {
    fn drop(&mut self) {
        // 进程退出路径:只发停止信号并 detach 工作线程,绝不 join。
        // 工作线程可能正阻塞在请求处理中(handle_request → call_core),join 会把整个
        // 进程退出拖住数秒(即观察到的"退出延时")。线程会在下个循环边界自行退出;
        // 即便未及,进程退出时也由 OS 回收。运行时手动关闭仍走 stop()(需 join 释放端口)。
        self.stop.store(true, Ordering::Relaxed);
        self.handle = None;
    }
}

pub fn is_loopback_host(host: &str) -> bool {
    let host = host
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let host = host
        .strip_prefix('[')
        .and_then(|s| s.split_once(']').map(|(h, _)| h))
        .unwrap_or_else(|| host.split(':').next().unwrap_or(host));
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

pub fn is_allowed_origin(origin: &str) -> bool {
    let lower = origin.trim().to_ascii_lowercase();
    if lower.starts_with("http://localhost:") || lower == "http://localhost" {
        return true;
    }
    if lower.starts_with("http://127.0.0.1:") || lower == "http://127.0.0.1" {
        return true;
    }
    if lower.starts_with("http://[::1]:") || lower == "http://[::1]" {
        return true;
    }
    if lower == "https://windinput.com" {
        return true;
    }
    lower.starts_with("https://") && lower.ends_with(".windinput.com")
}

// 取 request-target 中的路径部分（丢弃 ?query）。fragment 不会由客户端发出，无需处理。
pub fn route_path(url: &str) -> String {
    match url.split_once('?') {
        Some((path, _)) => path.to_string(),
        None => url.to_string(),
    }
}

// URL 路径段的百分号解码。tiny_http 不做解码，中文主题名（如 QQ五笔风格）
// 会以 %E4%BA%94 形式到达，必须先还原再查主题目录。
// 解码后的结果仍需过 validate_slug —— %2F 之类的编码分隔符要在校验前展开，
// 否则可被用来绕过路径穿越检查。
pub fn percent_decode(s: &str) -> Option<String> {
    let raw = s.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            if i + 2 >= raw.len() {
                return None;
            }
            let hex = std::str::from_utf8(&raw[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

pub fn validate_slug(slug: &str) -> bool {
    !slug.is_empty()
        && !slug.contains("..")
        && !slug.contains('/')
        && !slug.contains('\\')
        && !Path::new(slug).is_absolute()
}

fn json_reply(status: u16, body: &str, allow_origin: Option<String>) -> HttpReply {
    HttpReply {
        status,
        body: body.to_string(),
        allow_origin,
    }
}

fn guard_request(host: &str, origin: Option<&str>) -> Result<HttpReply, HttpReply> {
    if !is_loopback_host(host) {
        return Err(json_reply(
            403,
            "{\"ok\":false,\"error\":\"拒绝非本机 Host\"}",
            None,
        ));
    }
    if let Some(origin) = origin {
        if !is_allowed_origin(origin) {
            return Err(json_reply(
                403,
                "{\"ok\":false,\"error\":\"拒绝未授权 Origin\"}",
                None,
            ));
        }
        return Ok(json_reply(200, "", Some(origin.to_string())));
    }
    Ok(json_reply(200, "", None))
}

fn handle_request(req: tiny_http::Request) {
    let method = req.method().as_str().to_string();
    // 只按路径分发：查询串在此剥离，否则 /api/themes?x=1 会落到 404 分支，
    // 且带参的 /api/theme/:slug 会把 "?x=1" 当成 slug 的一部分。
    let url = route_path(req.url());
    let host = header_value(&req, "Host").unwrap_or_default();
    let origin = header_value(&req, "Origin");

    let guard = guard_request(&host, origin.as_deref());
    let allow_origin = match guard {
        Ok(r) => r.allow_origin,
        Err(r) => {
            respond(req, r);
            return;
        }
    };

    if method == "OPTIONS" {
        respond(
            req,
            HttpReply {
                status: 204,
                body: String::new(),
                allow_origin,
            },
        );
        return;
    }

    let reply = if method == "GET" && url == "/api/themes" {
        match call_core("theme.list", &serde_json::json!({})) {
            Ok(v) => json_reply(200, &v.to_string(), allow_origin),
            Err(e) => json_reply(
                500,
                &serde_json::json!({"ok":false,"error":e}).to_string(),
                allow_origin,
            ),
        }
    } else if method == "GET" && url.starts_with("/api/theme/") {
        let raw_slug = url.trim_start_matches("/api/theme/");
        let slug = percent_decode(raw_slug).unwrap_or_default();
        if !validate_slug(&slug) {
            json_reply(400, r#"{"ok":false,"error":"非法主题 slug"}"#, allow_origin)
        } else {
            match call_core("theme.getText", &serde_json::json!({ "slug": slug })) {
                Ok(v) => json_reply(200, &v.to_string(), allow_origin),
                Err(e) if e.contains("不存在") => json_reply(
                    404,
                    &serde_json::json!({"ok":false,"error":e}).to_string(),
                    allow_origin,
                ),
                Err(e) => json_reply(
                    500,
                    &serde_json::json!({"ok":false,"error":e}).to_string(),
                    allow_origin,
                ),
            }
        }
    } else if method == "POST" && url == "/api/theme/push" {
        handle_push(req, allow_origin);
        return;
    } else {
        json_reply(404, r#"{"ok":false,"error":"接口不存在"}"#, allow_origin)
    };
    respond(req, reply);
}

fn header_value(req: &tiny_http::Request, name: &str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.to_string().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

fn respond(req: tiny_http::Request, reply: HttpReply) {
    let mut response = tiny_http::Response::from_string(reply.body)
        .with_status_code(tiny_http::StatusCode(reply.status));
    if let Ok(header) = tiny_http::Header::from_bytes(
        b"Content-Type" as &[u8],
        b"application/json; charset=utf-8" as &[u8],
    ) {
        response = response.with_header(header);
    }
    if let Ok(header) = tiny_http::Header::from_bytes(
        b"Access-Control-Allow-Methods" as &[u8],
        b"GET, POST, OPTIONS" as &[u8],
    ) {
        response = response.with_header(header);
    }
    if let Ok(header) = tiny_http::Header::from_bytes(
        b"Access-Control-Allow-Headers" as &[u8],
        b"Content-Type" as &[u8],
    ) {
        response = response.with_header(header);
    }
    if let Some(origin) = reply.allow_origin {
        if let Ok(header) = tiny_http::Header::from_bytes(
            b"Access-Control-Allow-Origin" as &[u8],
            origin.as_bytes(),
        ) {
            response = response.with_header(header);
        }
    }
    let _ = req.respond(response);
}

fn handle_push(mut req: tiny_http::Request, allow_origin: Option<String>) {
    let mut body = String::new();
    if let Err(e) = req.as_reader().read_to_string(&mut body) {
        respond(
            req,
            json_reply(
                400,
                &serde_json::json!({"ok":false,"error":format!("读取请求失败：{e}")}).to_string(),
                allow_origin,
            ),
        );
        return;
    }

    let payload: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            respond(
                req,
                json_reply(
                    400,
                    &serde_json::json!({"ok":false,"error":format!("解析 JSON 失败：{e}")})
                        .to_string(),
                    allow_origin,
                ),
            );
            return;
        }
    };
    let text = payload
        .get("toml")
        .or_else(|| payload.get("yaml"))
        .and_then(serde_json::Value::as_str);
    let Some(text) = text else {
        respond(
            req,
            json_reply(
                400,
                r#"{"ok":false,"error":"缺少 toml 字段"}"#,
                allow_origin,
            ),
        );
        return;
    };
    let force = payload
        .get("force")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    // slug：主题唯一 id（编辑器拉取时记住的原始 id）。传入则由 core 就地写回该 id 目录。
    let slug = payload.get("slug").and_then(serde_json::Value::as_str);

    let reply = match import_text(text, force, slug) {
        Ok(result) => json_reply(
            200,
            &serde_json::json!({
                "imported": true,
                // core 导入仅落盘；是否热重载由输入法自身监听/重启决定，这里不谎报已重载。
                "reloaded": false,
                "theme_name": result.display_name,
                // 落地的主题 id（可能是就地写回的旧目录名），供前端回填记忆。
                "slug": result.slug,
            })
            .to_string(),
            allow_origin,
        ),
        // 同名已存在（force=false）：回 409，前端据此弹「覆盖确认」再以 force=true 重推。
        Err(ThemeImportError::Conflict(_)) => json_reply(
            409,
            &serde_json::json!({ "conflict": true }).to_string(),
            allow_origin,
        ),
        Err(e) => json_reply(
            400,
            &serde_json::json!({"ok":false,"error":e.user_message()}).to_string(),
            allow_origin,
        ),
    };
    respond(req, reply);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_host_accepts_local_only() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("localhost:29731"));
        assert!(is_loopback_host("127.0.0.1:29731"));
        assert!(is_loopback_host("[::1]:29731"));
        assert!(!is_loopback_host("example.com"));
        assert!(!is_loopback_host("192.168.1.10:29731"));
    }

    #[test]
    fn allowed_origin_accepts_windinput_and_local_dev() {
        assert!(is_allowed_origin("https://windinput.com"));
        assert!(is_allowed_origin("https://theme.windinput.com"));
        assert!(is_allowed_origin("http://localhost:5173"));
        assert!(is_allowed_origin("http://127.0.0.1:5173"));
        assert!(!is_allowed_origin("https://evil.com"));
        assert!(!is_allowed_origin("http://windinput.com.evil.com"));
    }

    #[test]
    fn slug_rejects_path_traversal() {
        assert!(validate_slug("default"));
        assert!(validate_slug("qingfeng-blue"));
        assert!(!validate_slug(""));
        assert!(!validate_slug("../default"));
        assert!(!validate_slug("a/b"));
        assert!(!validate_slug(r"a\b"));
    }

    #[test]
    fn route_path_strips_query_string() {
        assert_eq!(route_path("/api/themes?t=1"), "/api/themes");
        assert_eq!(route_path("/api/themes"), "/api/themes");
        assert_eq!(route_path("/api/theme/default?v=2"), "/api/theme/default");
        // 查询串里的 ? 不影响首次切分
        assert_eq!(route_path("/api/theme/x?a=1?b=2"), "/api/theme/x");
        // 空查询串
        assert_eq!(route_path("/api/theme/push?"), "/api/theme/push");
    }

    #[test]
    fn percent_decode_restores_cjk_slug() {
        assert_eq!(
            percent_decode("QQ%E4%BA%94%E7%AC%94%E9%A3%8E%E6%A0%BC").as_deref(),
            Some("QQ五笔风格")
        );
        assert_eq!(percent_decode("default").as_deref(), Some("default"));
    }

    #[test]
    fn percent_decode_rejects_malformed_input() {
        assert_eq!(percent_decode("a%"), None);
        assert_eq!(percent_decode("a%zz"), None);
        // 非法 UTF-8 字节序列不得还原成 slug
        assert_eq!(percent_decode("%FF%FE"), None);
    }

    #[test]
    fn decoded_separators_still_rejected_by_validate() {
        // 先解码再校验：%2F / %5C 展开后必须被路径穿越检查拦下
        let decoded = percent_decode("..%2Fdefault").unwrap();
        assert!(!validate_slug(&decoded));
        let decoded = percent_decode("a%5Cb").unwrap();
        assert!(!validate_slug(&decoded));
    }

    #[test]
    fn start_stop_updates_status_without_handling_requests() {
        let mut server = ThemeEditorServer::default();
        let status = server.start().expect("服务应能选择可用端口并启动");

        assert!(status.running);
        assert!(matches!(status.port, Some(29731..=29733)));
        assert_eq!(
            status.url,
            status.port.map(|port| format!("http://localhost:{port}"))
        );
        assert!(server.status().running);

        server.stop();

        let stopped = server.status();
        assert!(!stopped.running);
        assert_eq!(stopped.url, None);
        assert_eq!(stopped.port, None);
    }
}

#[cfg(test)]
mod handler_tests {
    use super::*;

    #[test]
    fn reject_foreign_host() {
        let r = guard_request("evil.com", None);
        assert_eq!(r.unwrap_err().status, 403);
    }

    #[test]
    fn reject_foreign_origin() {
        let r = guard_request("localhost:29731", Some("https://evil.com"));
        assert_eq!(r.unwrap_err().status, 403);
    }

    #[test]
    fn allow_windinput_origin() {
        let r = guard_request("localhost:29731", Some("https://theme.windinput.com")).unwrap();
        assert_eq!(
            r.allow_origin.as_deref(),
            Some("https://theme.windinput.com")
        );
    }
}
