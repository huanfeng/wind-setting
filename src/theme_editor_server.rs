//! 本地主题编辑器 HTTP 服务：只监听 loopback，并限制 Host/Origin。

use std::net::IpAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::rpc::call_core;
use crate::theme_import::import_text;

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
        self.stop();
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
    let url = req.url().to_string();
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
        let slug = url.trim_start_matches("/api/theme/");
        if !validate_slug(slug) {
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

    let reply = match import_text(text, force) {
        Ok(result) => json_reply(
            200,
            &serde_json::json!({
                "ok": true,
                "slug": result.slug,
                "display_name": result.display_name,
            })
            .to_string(),
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
