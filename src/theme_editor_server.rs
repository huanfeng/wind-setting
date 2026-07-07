//! 本地主题编辑器 HTTP 服务：只监听 loopback，并限制 Host/Origin。

use std::net::IpAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

#[allow(unused_imports)]
use crate::rpc::call_core;
#[allow(unused_imports)]
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
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                selected = Some(port);
                break;
            }
        }
        let port = selected
            .ok_or_else(|| "无法启动主题编辑器连接：29731-29733 端口均被占用".to_string())?;
        let status = ThemeEditorServerStatus {
            running: true,
            url: Some(format!("http://localhost:{port}")),
            port: Some(port),
        };
        *self
            .status
            .lock()
            .map_err(|_| "主题编辑器状态锁异常".to_string())? = status.clone();
        Ok(status)
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut s) = self.status.lock() {
            *s = ThemeEditorServerStatus::default();
        }
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
        let status = server.start().expect("服务骨架应能选择可用端口");

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
