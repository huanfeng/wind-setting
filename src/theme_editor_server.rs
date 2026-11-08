//! 本地主题编辑器 HTTP 服务：只监听 loopback，并限制 Host/Origin。
//!
//! 安全模型（每条路由都一样，新路由不得绕开 [`guard_request`]）：
//! - 只绑 `127.0.0.1`，外机连不进来；
//! - `Host` 必须是本机名（挡 DNS 重绑定：恶意域名解析到 127.0.0.1 后，浏览器发来的
//!   `Host` 仍是那个域名）；
//! - 带了 `Origin` 就必须是官方站或本机开发服，否则 403——浏览器跨源 POST 一定带
//!   `Origin`，网页借用户的浏览器往这里推东西会在这一步被拒；不带 `Origin` 的是本机进程
//!   或同源导航，读不到跨源响应（没有 `Access-Control-Allow-Origin`）。
//! - 校验在读请求体**之前**做；请求体一律限长读取。

use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

use crate::net_task::{ReadError, read_limited};
use crate::rpc::call_core;
use crate::schema_url_import::{DownloadedPackage, save_stream_raw};
use crate::theme_import::{
    MAX_THEME_BYTES, MAX_THEME_PACKAGE_BYTES, THEME_PKG, ThemeImportError, ThemeImportResult,
    import_package_via, import_text,
};

/// `/api/theme/push` 的请求体上限：主题原文 2 MiB，JSON 转义（引号、反斜杠、换行）最多
/// 把它撑大一倍，再留 64 KiB 给其余字段。
const MAX_PUSH_BODY: usize = MAX_THEME_BYTES * 2 + 64 * 1024;

/// 路由依赖的外部能力：core RPC 与临时文件目录。单测换成桩，生产走 [`call_core`] 与
/// 下载用的私有目录。
struct Backend<'a> {
    rpc: &'a (dyn Fn(&str, &Value) -> Result<Value, String> + Sync),
    /// `None` = 生产目录（`schema_url_import::download_dir`，Linux 下 0700）。
    tmp_dir: Option<&'a Path>,
}

impl Backend<'_> {
    fn tmp_dir(&self) -> Result<PathBuf, String> {
        match self.tmp_dir {
            Some(d) => Ok(d.to_path_buf()),
            None => {
                let d = crate::schema_url_import::download_dir()?;
                crate::schema_url_import::purge_stale(&d, THEME_PKG.prefix);
                Ok(d)
            }
        }
    }
}

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
            let backend = Backend {
                rpc: &call_core,
                tmp_dir: None,
            };
            while !stop.load(Ordering::Relaxed) {
                match server.recv_timeout(Duration::from_millis(200)) {
                    Ok(Some(req)) => handle_request(req, &backend),
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

/// 取查询串里的参数值（百分号解码，`+` 视作空格——`URLSearchParams` 就这么编）。
/// 参数不在返回 `None`；在但解码失败返回 `Some(None)`，调用方据此回 400 而不是当它没传。
fn query_param(url: &str, key: &str) -> Option<Option<String>> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == key).then(|| percent_decode(&v.replace('+', " ")))
    })
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

fn handle_request(req: tiny_http::Request, be: &Backend) {
    let method = req.method().as_str().to_string();
    // 只按路径分发：查询串在此剥离，否则 /api/themes?x=1 会落到 404 分支，
    // 且带参的 /api/theme/:slug 会把 "?x=1" 当成 slug 的一部分。
    let raw_url = req.url().to_string();
    let url = route_path(&raw_url);
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

    let package_slug = url
        .strip_prefix("/api/theme/")
        .and_then(|rest| rest.strip_suffix("/package"));
    let reply = if method == "GET" && url == "/api/themes" {
        match (be.rpc)("theme.list", &serde_json::json!({})) {
            Ok(v) => json_reply(200, &v.to_string(), allow_origin),
            Err(e) => json_reply(
                500,
                &serde_json::json!({"ok":false,"error":e}).to_string(),
                allow_origin,
            ),
        }
    } else if let (true, Some(raw_slug)) = (method == "GET", package_slug) {
        handle_export_package(req, raw_slug, allow_origin, be);
        return;
    } else if method == "GET" && url.starts_with("/api/theme/") {
        let raw_slug = url.trim_start_matches("/api/theme/");
        let slug = percent_decode(raw_slug).unwrap_or_default();
        if !validate_slug(&slug) {
            json_reply(400, r#"{"ok":false,"error":"非法主题 slug"}"#, allow_origin)
        } else {
            match (be.rpc)("theme.getText", &serde_json::json!({ "slug": slug })) {
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
    } else if method == "POST" && url == "/api/theme/package" {
        handle_import_package(req, &raw_url, allow_origin, be);
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
    let response = tiny_http::Response::from_string(reply.body)
        .with_status_code(tiny_http::StatusCode(reply.status));
    send(
        req,
        response,
        "application/json; charset=utf-8",
        reply.allow_origin,
    );
}

/// 补上 Content-Type 与 CORS 头后发出。JSON 回执与包字节共用。
fn send<R: Read>(
    req: tiny_http::Request,
    mut response: tiny_http::Response<R>,
    content_type: &str,
    allow_origin: Option<String>,
) {
    if let Ok(header) =
        tiny_http::Header::from_bytes(b"Content-Type" as &[u8], content_type.as_bytes())
    {
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
    if let Some(origin) = allow_origin
        && let Ok(header) = tiny_http::Header::from_bytes(
            b"Access-Control-Allow-Origin" as &[u8],
            origin.as_bytes(),
        )
    {
        response = response.with_header(header);
    }
    let _ = req.respond(response);
}

fn error_reply(status: u16, msg: &str, allow_origin: Option<String>) -> HttpReply {
    json_reply(
        status,
        &serde_json::json!({"ok":false,"error":msg}).to_string(),
        allow_origin,
    )
}

/// 声明的长度已超限：不读请求体，直接回 413。
fn declared_too_large(req: &tiny_http::Request, max: usize) -> bool {
    req.body_length().is_some_and(|n| n > max)
}

/// 推送 / 收包的回执：两条路同形，编辑器的覆盖确认因此只有一套。
fn import_reply(
    result: Result<ThemeImportResult, ThemeImportError>,
    allow_origin: Option<String>,
) -> HttpReply {
    match result.inspect(|_| crate::theme_gallery::note_external_edit()) {
        Ok(result) => json_reply(
            200,
            &serde_json::json!({
                "imported": true,
                // 由 core 判定并回报：推的正好是当前生效主题时它会重解析下发，此处如实透传。
                // 推的是别的主题则为 false —— 那本来也不该动用户正在用的外观。
                "reloaded": result.reloaded,
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
        Err(e) => error_reply(400, &e.user_message(), allow_origin),
    }
}

fn handle_push(mut req: tiny_http::Request, allow_origin: Option<String>) {
    const TOO_LARGE: &str = "主题内容超过 2 MiB 限制";
    if declared_too_large(&req, MAX_PUSH_BODY) {
        respond(req, error_reply(413, TOO_LARGE, allow_origin));
        return;
    }
    let read = read_limited(req.as_reader(), MAX_PUSH_BODY, &AtomicBool::new(false));
    let body = match read.map(String::from_utf8) {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => {
            respond(
                req,
                error_reply(400, "请求不是有效 UTF-8 文本", allow_origin),
            );
            return;
        }
        Err(ReadError::TooLarge) => {
            respond(req, error_reply(413, TOO_LARGE, allow_origin));
            return;
        }
        Err(ReadError::Io(e) | ReadError::Write(e)) => {
            respond(
                req,
                error_reply(400, &format!("读取请求失败：{e}"), allow_origin),
            );
            return;
        }
        Err(ReadError::Cancelled) => unreachable!("取消标志从不置位"),
    };

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
    // 与设置端粘贴 / 链接导入同一条闸（`MAX_THEME_BYTES`）。
    if text.len() > MAX_THEME_BYTES {
        respond(req, error_reply(413, TOO_LARGE, allow_origin));
        return;
    }
    let force = payload
        .get("force")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    // slug：主题唯一 id（编辑器拉取时记住的原始 id）。传入则由 core 就地写回该 id 目录。
    let slug = payload.get("slug").and_then(serde_json::Value::as_str);

    let reply = import_reply(import_text(text, force, slug), allow_origin);
    respond(req, reply);
}

/// `POST /api/theme/package?slug=&force=1`：请求体是 `.wtheme` 原始字节。
///
/// 限长流式落进私有临时文件 → `theme.importPackage`（与设置端「导入主题包」同一条安装
/// 路径，依赖链校验、回滚、热重载都在 core）→ 删临时文件 → 回执同 `/api/theme/push`。
fn handle_import_package(
    mut req: tiny_http::Request,
    raw_url: &str,
    allow_origin: Option<String>,
    be: &Backend,
) {
    let slug = match query_param(raw_url, "slug") {
        None => None,
        Some(Some(s)) if validate_slug(&s) => Some(s),
        Some(_) => {
            respond(req, error_reply(400, "非法主题 slug", allow_origin));
            return;
        }
    };
    let force = matches!(
        query_param(raw_url, "force").flatten().as_deref(),
        Some("1" | "true")
    );
    if declared_too_large(&req, MAX_THEME_PACKAGE_BYTES) {
        respond(req, error_reply(413, THEME_PKG.too_large, allow_origin));
        return;
    }
    let dir = match be.tmp_dir() {
        Ok(d) => d,
        Err(e) => {
            respond(req, error_reply(500, &e, allow_origin));
            return;
        }
    };
    let saved = save_stream_raw(&dir, req.as_reader(), &THEME_PKG, &AtomicBool::new(false));
    let pkg = match saved {
        Ok(pkg) => pkg,
        Err(e) => {
            let (status, msg) = match e {
                ReadError::TooLarge => (413, THEME_PKG.too_large.to_string()),
                ReadError::Io(m) => (400, format!("读取请求失败：{m}")),
                ReadError::Write(m) => (500, format!("写入临时文件失败：{m}")),
                ReadError::Cancelled => unreachable!("取消标志从不置位"),
            };
            respond(req, error_reply(status, &msg, allow_origin));
            return;
        }
    };
    let empty = std::fs::metadata(pkg.path()).is_ok_and(|m| m.len() == 0);
    let reply = if empty {
        error_reply(400, "请求体里没有主题包", allow_origin)
    } else {
        let result = import_package_via(
            be.rpc,
            &pkg.path().to_string_lossy(),
            slug.as_deref(),
            force,
        );
        import_reply(result, allow_origin)
    };
    // 先删临时文件再回：客户端收到回执时，这次推送在本机已不留痕迹。
    drop(pkg);
    respond(req, reply);
}

/// 出包的响应体：边读边发，发完（响应 drop）先关文件再删——Windows 删不掉开着的文件。
struct PackageBody {
    file: std::fs::File,
    _pkg: DownloadedPackage,
}

impl Read for PackageBody {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf)
    }
}

/// `GET /api/theme/:slug/package`：`theme.exportPackage` 打到私有临时文件，回包字节。
fn handle_export_package(
    req: tiny_http::Request,
    raw_slug: &str,
    allow_origin: Option<String>,
    be: &Backend,
) {
    let slug = percent_decode(raw_slug).unwrap_or_default();
    if !validate_slug(&slug) {
        respond(req, error_reply(400, "非法主题 slug", allow_origin));
        return;
    }
    let built = (|| -> Result<(PackageBody, u64), (u16, String)> {
        let dir = be.tmp_dir().map_err(|e| (500, e))?;
        // 先以 O_EXCL 占下一个随机名：core 写同目录的 `.tmp` 再 rename 到这里，
        // 落点因此一定是本进程刚建的文件，而不是别人预先放好的东西。
        let pkg = save_stream_raw(&dir, std::io::empty(), &THEME_PKG, &AtomicBool::new(false))
            .map_err(|e| (500, format!("创建临时文件失败：{e:?}")))?;
        let params = serde_json::json!({ "slug": slug, "path": pkg.path().to_string_lossy() });
        (be.rpc)("theme.exportPackage", &params).map_err(|e| {
            let status = if e.contains("不存在") { 404 } else { 500 };
            (status, e)
        })?;
        let file = std::fs::File::open(pkg.path())
            .map_err(|e| (500, format!("读取导出的主题包失败：{e}")))?;
        let len = file
            .metadata()
            .map_err(|e| (500, format!("读取导出的主题包失败：{e}")))?
            .len();
        Ok((PackageBody { file, _pkg: pkg }, len))
    })();
    match built {
        Ok((body, len)) => {
            let response = tiny_http::Response::new(
                tiny_http::StatusCode(200),
                Vec::new(),
                body,
                usize::try_from(len).ok(),
                None,
            )
            // 长度已知就照实发 Content-Length（tiny_http 默认 32 KiB 起改分块），编辑器
            // 拉大包时据此显示进度。
            .with_chunked_threshold(usize::MAX);
            send(req, response, "application/octet-stream", allow_origin);
        }
        Err((status, msg)) => respond(req, error_reply(status, &msg, allow_origin)),
    }
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

/// 路由端到端：真起一个 tiny_http（端口 0），用裸 TCP 发请求——超限、分块、未读完即回
/// 这些行为只有真连接才测得出来。RPC 与临时目录走 [`Backend`] 注入。
#[cfg(test)]
mod route_tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use std::net::TcpStream;

    type Calls = Mutex<Vec<(String, Value)>>;

    struct Reply {
        status: u16,
        headers: String,
        body: Vec<u8>,
    }

    impl Reply {
        fn json(&self) -> Value {
            serde_json::from_slice(&self.body).unwrap_or(Value::Null)
        }
        fn header(&self, name: &str) -> Option<String> {
            self.headers.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case(name)
                    .then(|| v.trim().to_string())
            })
        }
    }

    /// 读一个完整响应（按 Content-Length 收正文），不依赖对端关连接——超限回 413 时
    /// tiny_http 还在等剩下的请求体，连接不会先关。
    fn read_reply(s: &mut TcpStream) -> Reply {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..end]).into_owned();
                let (status_line, headers) = head.split_once("\r\n").unwrap_or((&head, ""));
                let status = status_line.split(' ').nth(1).unwrap().parse().unwrap();
                let reply = Reply {
                    status,
                    headers: headers.to_string(),
                    body: Vec::new(),
                };
                let len: usize = reply
                    .header("Content-Length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                while buf.len() < end + 4 + len {
                    let n = s.read(&mut chunk).unwrap();
                    assert!(n > 0, "响应正文没收全");
                    buf.extend_from_slice(&chunk[..n]);
                }
                return Reply {
                    body: buf[end + 4..end + 4 + len].to_vec(),
                    ..reply
                };
            }
            let n = s.read(&mut chunk).unwrap();
            assert!(n > 0, "连接在响应头之前就关了");
            buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// 起服务处理 `n` 个请求，`client` 拿端口去发。返回时服务线程已收尾（临时文件已删）。
    fn serve(n: usize, be: &Backend, client: impl FnOnce(u16)) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        std::thread::scope(|s| {
            s.spawn(|| {
                for _ in 0..n {
                    handle_request(server.recv().unwrap(), be);
                }
            });
            client(port);
        });
    }

    fn connect(port: u16) -> TcpStream {
        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        s
    }

    fn request(port: u16, head: &str, body: &[u8]) -> Reply {
        let mut s = connect(port);
        s.write_all(head.replace("{port}", &port.to_string()).as_bytes())
            .unwrap();
        s.write_all(body).unwrap();
        read_reply(&mut s)
    }

    fn post_package(port: u16, query: &str, extra: &str, body: &[u8]) -> Reply {
        request(
            port,
            &format!(
                "POST /api/theme/package{query} HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\n{extra}\
                 Content-Type: application/octet-stream\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body.len()
            ),
            body,
        )
    }

    fn get(port: u16, path: &str) -> Reply {
        request(
            port,
            &format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\nConnection: close\r\n\r\n"
            ),
            b"",
        )
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "wind_setting_editor_srv_{tag}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn is_empty(dir: &Path) -> bool {
        std::fs::read_dir(dir).unwrap().next().is_none()
    }

    /// 程序生成的「包」：zip 签名打头，后面是确定性伪随机字节（服务端不解包，core 才解）。
    fn fixture(len: usize) -> Vec<u8> {
        let mut v = b"PK\x03\x04".to_vec();
        let mut x = 0x9e37_79b9u32;
        while v.len() < len {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            v.push(x as u8);
        }
        v
    }

    fn no_rpc(m: &str, _: &Value) -> Result<Value, String> {
        panic!("这条请求不该走到 RPC：{m}")
    }

    /// 正常收包：字节原样进临时文件交给 `theme.importPackage`（带上 slug / force），
    /// 回执同 push，回之前临时文件已删。
    #[test]
    fn package_upload_imports_and_cleans_up() {
        let dir = scratch("upload_ok");
        let calls = Calls::default();
        let got = Mutex::new(Vec::new());
        let rpc = |m: &str, p: &Value| {
            let path = p["path"].as_str().unwrap();
            *got.lock().unwrap() = std::fs::read(path).unwrap();
            calls.lock().unwrap().push((m.to_string(), p.clone()));
            Ok(json!({"ok": true, "slug": "shuimo", "display_name": "水墨", "reloaded": true}))
        };
        let be = Backend {
            rpc: &rpc,
            tmp_dir: Some(&dir),
        };
        let pkg = fixture(300 * 1024);
        serve(1, &be, |port| {
            let r = post_package(
                port,
                "?slug=%E6%B0%B4%E5%A2%A8&force=1",
                "Origin: https://theme.windinput.com\r\n",
                &pkg,
            );
            assert_eq!(r.status, 200);
            assert_eq!(
                r.json(),
                json!({"imported": true, "reloaded": true, "theme_name": "水墨", "slug": "shuimo"})
            );
            assert_eq!(
                r.header("Access-Control-Allow-Origin").as_deref(),
                Some("https://theme.windinput.com")
            );
            assert!(is_empty(&dir), "回执到时临时文件应已删除");
        });
        let calls = calls.into_inner().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "theme.importPackage");
        assert_eq!(calls[0].1["slug"], "水墨");
        assert_eq!(calls[0].1["force"], true);
        assert!(*got.lock().unwrap() == pkg, "core 拿到的必须是原样的包");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 不带 slug / force：不传 slug，force 为假。
    #[test]
    fn package_upload_without_query_defaults() {
        let dir = scratch("upload_noq");
        let calls = Calls::default();
        let rpc = |m: &str, p: &Value| {
            calls.lock().unwrap().push((m.to_string(), p.clone()));
            Ok(json!({"ok": true, "slug": "a", "display_name": "A"}))
        };
        let be = Backend {
            rpc: &rpc,
            tmp_dir: Some(&dir),
        };
        serve(1, &be, |port| {
            assert_eq!(post_package(port, "", "", &fixture(2000)).status, 200);
        });
        let calls = calls.into_inner().unwrap();
        assert!(calls[0].1.get("slug").is_none());
        assert_eq!(calls[0].1["force"], false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 冲突透传 409、core 报错回 400，两路都不留临时文件。
    #[test]
    fn package_upload_conflict_and_failure_clean_up() {
        let dir = scratch("upload_err");
        let conflict = |_: &str, _: &Value| Ok(json!({"ok": false, "conflict": true, "slug": "a"}));
        let be = Backend {
            rpc: &conflict,
            tmp_dir: Some(&dir),
        };
        serve(1, &be, |port| {
            let r = post_package(port, "?slug=a", "", &fixture(5000));
            assert_eq!(r.status, 409);
            assert_eq!(r.json(), json!({"conflict": true}));
        });
        assert!(is_empty(&dir));

        let broken = |_: &str, _: &Value| Err("主题包缺少 theme.toml".to_string());
        let be = Backend {
            rpc: &broken,
            tmp_dir: Some(&dir),
        };
        serve(1, &be, |port| {
            let r = post_package(port, "", "", &fixture(5000));
            assert_eq!(r.status, 400);
            assert!(r.json()["error"].as_str().unwrap().contains("theme.toml"));
        });
        assert!(is_empty(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 声明长度超限：立刻 413，不等请求体发完、不落盘、不调 RPC。
    #[test]
    fn package_upload_rejects_declared_oversize_before_reading() {
        let dir = scratch("upload_413");
        let be = Backend {
            rpc: &no_rpc,
            tmp_dir: Some(&dir),
        };
        serve(1, &be, |port| {
            let mut s = connect(port);
            write!(
                s,
                "POST /api/theme/package HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
                 Content-Length: {}\r\n\r\nPK\x03\x04",
                MAX_THEME_PACKAGE_BYTES + 1
            )
            .unwrap();
            // 只发了 4 个字节就能收到回执：服务端没有在读请求体。
            let r = read_reply(&mut s);
            assert_eq!(r.status, 413);
            assert!(r.json()["error"].as_str().unwrap().contains("64 MiB"));
            assert!(is_empty(&dir));
            // 断开连接，tiny_http 丢弃剩余请求体的那段读取随之结束。
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 不声明长度（分块传输）：读到上限即停、回 413，半截临时文件删掉。
    #[test]
    fn package_upload_rejects_chunked_oversize() {
        let dir = scratch("upload_chunked");
        let be = Backend {
            rpc: &no_rpc,
            tmp_dir: Some(&dir),
        };
        serve(1, &be, |port| {
            let mut s = connect(port);
            write!(
                s,
                "POST /api/theme/package HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
                 Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let chunk = vec![b'x'; 1024 * 1024];
            for _ in 0..(MAX_THEME_PACKAGE_BYTES / chunk.len() + 1) {
                write!(s, "{:x}\r\n", chunk.len()).unwrap();
                s.write_all(&chunk).unwrap();
                s.write_all(b"\r\n").unwrap();
            }
            s.write_all(b"0\r\n\r\n").unwrap();
            let r = read_reply(&mut s);
            assert_eq!(r.status, 413);
        });
        assert!(is_empty(&dir), "超限的半截文件要删");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 空请求体与非法 slug：400，不调 RPC。
    #[test]
    fn package_upload_rejects_empty_body_and_bad_slug() {
        let dir = scratch("upload_bad");
        let be = Backend {
            rpc: &no_rpc,
            tmp_dir: Some(&dir),
        };
        serve(4, &be, |port| {
            assert_eq!(post_package(port, "", "", b"").status, 400);
            for q in ["?slug=..%2Fdefault", "?slug=", "?slug=%FF"] {
                assert_eq!(post_package(port, q, "", &fixture(100)).status, 400, "{q}");
            }
        });
        assert!(is_empty(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 新路由与旧路由同一套 Host / Origin 校验，且在读请求体、调 RPC 之前就拒。
    #[test]
    fn package_routes_are_guarded_like_the_rest() {
        let dir = scratch("guard");
        let be = Backend {
            rpc: &no_rpc,
            tmp_dir: Some(&dir),
        };
        serve(4, &be, |port| {
            let r = post_package(port, "", "Origin: https://evil.com\r\n", &fixture(100));
            assert_eq!(r.status, 403);
            assert_eq!(r.header("Access-Control-Allow-Origin"), None);
            // DNS 重绑定：Host 是外域名
            let r = request(
                port,
                "POST /api/theme/package HTTP/1.1\r\nHost: evil.com:{port}\r\n\
                 Content-Length: 4\r\nConnection: close\r\n\r\n",
                b"PK\x03\x04",
            );
            assert_eq!(r.status, 403);
            let r = request(
                port,
                "GET /api/theme/default/package HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
                 Origin: http://windinput.com.evil.com\r\nConnection: close\r\n\r\n",
                b"",
            );
            assert_eq!(r.status, 403);
            let r = request(
                port,
                "GET /api/theme/default/package HTTP/1.1\r\nHost: attacker.test\r\n\
                 Connection: close\r\n\r\n",
                b"",
            );
            assert_eq!(r.status, 403);
        });
        assert!(is_empty(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 正常出包：core 打到临时文件的字节原样回传，响应发完临时文件即删。
    #[test]
    fn package_download_streams_export_and_cleans_up() {
        let dir = scratch("download_ok");
        let pkg = fixture(200 * 1024);
        let calls = Calls::default();
        let rpc = |m: &str, p: &Value| {
            calls.lock().unwrap().push((m.to_string(), p.clone()));
            std::fs::write(p["path"].as_str().unwrap(), &pkg).unwrap();
            Ok(json!({"ok": true, "path": p["path"]}))
        };
        let be = Backend {
            rpc: &rpc,
            tmp_dir: Some(&dir),
        };
        serve(1, &be, |port| {
            let r = get(port, "/api/theme/%E6%B0%B4%E5%A2%A8/package");
            assert_eq!(r.status, 200);
            assert_eq!(
                r.header("Content-Type").as_deref(),
                Some("application/octet-stream")
            );
            assert!(r.body == pkg, "回传的必须是 core 打出的那个包");
        });
        assert!(is_empty(&dir), "发完即删");
        let calls = calls.into_inner().unwrap();
        assert_eq!(calls[0].0, "theme.exportPackage");
        assert_eq!(calls[0].1["slug"], "水墨");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 出包：主题不存在 404、slug 非法 400，都不留临时文件；主题恰好叫 package 时
    /// `/api/theme/package` 仍是取原文那条旧路由。
    #[test]
    fn package_download_errors_and_route_shape() {
        let dir = scratch("download_err");
        let calls = Calls::default();
        let rpc = |m: &str, p: &Value| {
            calls.lock().unwrap().push((m.to_string(), p.clone()));
            match m {
                "theme.exportPackage" => Err("主题不存在: nope".to_string()),
                _ => Ok(json!({"slug": "package", "toml": "[meta]"})),
            }
        };
        let be = Backend {
            rpc: &rpc,
            tmp_dir: Some(&dir),
        };
        serve(3, &be, |port| {
            assert_eq!(get(port, "/api/theme/nope/package").status, 404);
            assert_eq!(get(port, "/api/theme/..%2Fx/package").status, 400);
            assert_eq!(get(port, "/api/theme/package").status, 200);
        });
        assert!(is_empty(&dir));
        let calls = calls.into_inner().unwrap();
        assert_eq!(calls.len(), 2, "非法 slug 不该调 RPC");
        assert_eq!(calls[1].0, "theme.getText");
        assert_eq!(calls[1].1["slug"], "package");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn post_push(port: u16, body: &[u8]) -> Reply {
        request(
            port,
            &format!(
                "POST /api/theme/push HTTP/1.1\r\nHost: localhost:{{port}}\r\n\
                 Origin: http://localhost:5173\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            ),
            body,
        )
    }

    /// 旧的 TOML 推送协议照旧可用：成功回执、撞名 409。
    #[test]
    fn legacy_push_still_works() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let be = Backend {
            rpc: &no_rpc,
            tmp_dir: None,
        };
        let mut replies = Vec::new();
        serve(2, &be, |port| {
            replies.push(post_push(
                port,
                json!({"toml": "[meta]\nname = \"测试主题\"\n", "force": false})
                    .to_string()
                    .as_bytes(),
            ));
            replies.push(post_push(
                port,
                json!({"toml": "[meta]\nname = \"MOCK_EXISTING\"\n"})
                    .to_string()
                    .as_bytes(),
            ));
        });
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert_eq!(replies[0].status, 200);
        assert_eq!(replies[0].json()["imported"], true);
        assert_eq!(replies[0].json()["slug"], "test-theme");
        assert_eq!(replies[1].status, 409);
    }

    /// push 的请求体限长：声明超限直接 413；没超请求体上限、但原文超 2 MiB 也 413。
    #[test]
    fn legacy_push_is_size_limited() {
        let be = Backend {
            rpc: &no_rpc,
            tmp_dir: None,
        };
        serve(2, &be, |port| {
            let mut s = connect(port);
            write!(
                s,
                "POST /api/theme/push HTTP/1.1\r\nHost: localhost:{port}\r\n\
                 Content-Length: {}\r\n\r\n{{",
                MAX_PUSH_BODY + 1
            )
            .unwrap();
            assert_eq!(read_reply(&mut s).status, 413);
            drop(s);

            let toml = "a".repeat(MAX_THEME_BYTES + 1);
            let r = post_push(port, json!({ "toml": toml }).to_string().as_bytes());
            assert_eq!(r.status, 413);
            assert!(r.json()["error"].as_str().unwrap().contains("2 MiB"));
        });
    }

    #[test]
    fn query_param_decodes_and_tells_missing_from_malformed() {
        assert_eq!(
            query_param("/p?slug=%E6%B0%B4&force=1", "slug"),
            Some(Some("水".to_string()))
        );
        assert_eq!(
            query_param("/p?slug=a+b", "slug"),
            Some(Some("a b".to_string()))
        );
        assert_eq!(query_param("/p?force=1", "slug"), None);
        assert_eq!(query_param("/p", "slug"), None);
        assert_eq!(query_param("/p?slug=%zz", "slug"), Some(None));
        assert_eq!(query_param("/p?force", "force"), Some(Some(String::new())));
    }
}
