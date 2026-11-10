//! 本地主题编辑器 HTTP 服务：只监听 loopback，并限制 Host/Origin。
//!
//! 安全模型（每条路由都一样，新路由不得绕开 [`guard_request`] 与 [`authorize`]）：
//! - 只绑 `127.0.0.1`，外机连不进来；
//! - `Host` 必须是本机名（挡 DNS 重绑定：恶意域名解析到 127.0.0.1 后，浏览器发来的
//!   `Host` 仍是那个域名）；
//! - 带了 `Origin` 就必须在精确白名单里（[`is_allowed_origin`]），否则 403；
//! - 除 OPTIONS 预检外都要配对 token（`X-Wind-Token` 头，常量时间比较）。token 每次开服务
//!   随机生成，经「打开编辑器」按钮的 URL fragment 交给编辑器（fragment 不随请求发出、
//!   不进服务器日志）。**过渡**：已上线的编辑器不会带 token，三条旧路由在缺 token 时
//!   退回「`Origin` 必须存在且正好是线上编辑器」（[`LEGACY_ORIGINS`]）；收包 / 出包两条
//!   新路由只认 token；
//! - 校验在读请求体**之前**做；请求体一律限长读取；连接有读写超时（vendored tiny_http）。

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

/// 配对 token 的请求头。
pub const TOKEN_HEADER: &str = "X-Wind-Token";

/// 线上编辑器的源。已发布的编辑器不带 token，只有它在缺 token 时还能用三条旧路由。
///
/// **撤除条件**：带 token 的编辑器（W3.4 发包那版）上线后，再过一个设置端发布版本就删掉
/// 这条过渡（连同 [`Access::Legacy`]）——届时线上编辑器都带 token，留着它只剩攻击面。
const LEGACY_ORIGINS: &[&str] = &["https://theme.windinput.com"];

/// CORS 白名单里的线上源（精确匹配，不认任意子域）：编辑器 `theme.windinput.com`
/// （见编辑器仓 README 的部署表）与官网 `windinput.com`。本机开发服另判。
const PROD_ORIGINS: &[&str] = &["https://theme.windinput.com", "https://windinput.com"];

/// 读 push / 收包请求体的总时限。每次读另有 tiny_http 的 30 秒读超时，但滴灌（隔一会儿
/// 发几个字节）每次读都不超时，靠这一道收手。截止时间只在两次读之间查，所以最坏情况是
/// 总时限加一次读超时：约 60 + 30 = 90 秒（之后 tiny_http 丢弃余量最多再约 2 + 30 秒）。
/// 本机回环上 65 MiB 一秒都用不了。
const BODY_TIME_LIMIT: Duration = Duration::from_secs(60);

/// 带总截止时间的读：过了截止时间，下一次读直接报超时。
struct DeadlineReader<R> {
    inner: R,
    deadline: std::time::Instant,
}

impl<R: Read> Read for DeadlineReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if std::time::Instant::now() > self.deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "请求体传输超时",
            ));
        }
        self.inner.read(buf)
    }
}

/// 停服务时最多等工作线程这么久；它卡在一个慢请求上就放手不等，免得把界面拖住。
const STOP_WAIT: Duration = Duration::from_millis(1500);

/// `/api/theme/push` 的请求体上限：主题原文 2 MiB，JSON 转义（引号、反斜杠、换行）最多
/// 把它撑大一倍，再留 64 KiB 给其余字段。
const MAX_PUSH_BODY: usize = MAX_THEME_BYTES * 2 + 64 * 1024;

/// 路由依赖的外部能力：core RPC 与临时文件目录。单测换成桩，生产走 [`call_core`] 与
/// 下载用的私有目录。
struct Backend<'a> {
    rpc: &'a (dyn Fn(&str, &Value) -> Result<Value, String> + Sync),
    /// 本次服务的配对 token。
    token: &'a str,
    /// `None` = 生产目录（`schema_url_import::download_dir`，Linux 下 0700）。
    tmp_dir: Option<&'a Path>,
    /// 读请求体的总时限（生产 [`BODY_TIME_LIMIT`]）。
    body_time: Duration,
}

impl Backend<'_> {
    fn body_reader<'r>(&self, req: &'r mut tiny_http::Request) -> DeadlineReader<&'r mut dyn Read> {
        DeadlineReader {
            inner: req.as_reader(),
            deadline: std::time::Instant::now() + self.body_time,
        }
    }

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
    /// 「打开编辑器」用的地址：编辑器站点 + fragment 里的端口与配对 token。
    pub editor_url: Option<String>,
}

pub struct ThemeEditorServer {
    status: Arc<Mutex<ThemeEditorServerStatus>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    /// 工作线程退出时发一声；`stop` 据此限时等待。
    done: Option<std::sync::mpsc::Receiver<()>>,
}

/// 16 字节随机数的十六进制串。
fn new_token() -> Result<String, String> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).map_err(|e| format!("生成配对码失败：{e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// 编辑器连接地址。token 放 fragment：浏览器不会把它发给 theme.windinput.com 的服务器。
pub fn editor_connect_url(port: u16, token: &str) -> String {
    format!(
        "{}/#wind-port={port}&wind-token={token}",
        crate::pages::THEME_EDITOR_URL
    )
}

/// 常量时间比较（长度固定为 32，长度不同直接判否不泄露内容）。
fn token_matches(given: &str, expected: &str) -> bool {
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 请求凭什么被放行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    /// 带了正确的配对 token。
    Paired,
    /// 没带 token，但 `Origin` 正好是线上编辑器（过渡，只对旧路由有效）。
    Legacy,
}

/// 过渡规则只放已发布编辑器用到的三条旧路由（白名单）：`GET /api/themes`、
/// `GET /api/theme/:slug`（不含 `/package`）、`POST /api/theme/push`。以后加的路由默认只认 token。
fn is_legacy_route(method: &str, url: &str, is_package: bool) -> bool {
    match method {
        "GET" => {
            url == "/api/themes"
                || (!is_package
                    && url
                        .strip_prefix("/api/theme/")
                        .is_some_and(|slug| !slug.is_empty() && !slug.contains('/')))
        }
        "POST" => url == "/api/theme/push",
        _ => false,
    }
}

/// 认证：token 头优先（带了就必须对）；没带时只认线上编辑器的 `Origin`。
fn authorize(token: Option<&str>, origin: Option<&str>, expected: &str) -> Option<Access> {
    match token {
        Some(t) => token_matches(t.trim(), expected).then_some(Access::Paired),
        None => origin
            .filter(|o| LEGACY_ORIGINS.contains(o))
            .map(|_| Access::Legacy),
    }
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
            done: None,
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
        let token = new_token()?;

        // 每次开服务都换新的停止标志与状态：上一次 `stop` 没等到的旧线程（卡在慢请求上）
        // 仍持有旧的那一份，它收尾时不会把新服务的状态清掉，也不会因为新服务清了停止
        // 标志而继续接活。
        self.stop = Arc::new(AtomicBool::new(false));
        self.status = Arc::new(Mutex::new(ThemeEditorServerStatus::default()));
        let stop = Arc::clone(&self.stop);
        let status_handle = Arc::clone(&self.status);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let thread_token = token.clone();
        let handle = std::thread::spawn(move || {
            let backend = Backend {
                rpc: &call_core,
                token: &thread_token,
                tmp_dir: None,
                body_time: BODY_TIME_LIMIT,
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
            let _ = done_tx.send(());
        });

        let status = ThemeEditorServerStatus {
            running: true,
            url: Some(format!("http://localhost:{port}")),
            port: Some(port),
            editor_url: Some(editor_connect_url(port, &token)),
        };
        *self
            .status
            .lock()
            .map_err(|_| "主题编辑器状态锁异常".to_string())? = status.clone();
        self.handle = Some(handle);
        self.done = Some(done_rx);
        Ok(status)
    }

    /// 停服务。限时等工作线程退出（释放端口）；它卡在一个慢请求上（读请求体、等 core）
    /// 就放手不等——界面线程不能被一个本机请求拖住。没等到的线程处理完手上那个请求就会
    /// 退出，端口随之释放；在那之前再开服务会落到下一个端口。
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let exited = self.done.take().is_none_or(|rx| {
                !matches!(
                    rx.recv_timeout(STOP_WAIT),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                )
            });
            if exited {
                let _ = handle.join();
            } else {
                tracing::warn!(
                    "主题编辑器服务线程 {STOP_WAIT:?} 内未退出（还在处理请求），不再等待"
                );
            }
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

/// CORS 白名单，精确匹配：线上源只认 [`PROD_ORIGINS`]（不认任意 `*.windinput.com`——
/// 任何一个子域出问题都不该连带这里）；本机开发服认 `http://localhost` / `127.0.0.1` /
/// `[::1]` 加可选的纯数字端口。浏览器发的 `Origin` 本就是规范小写形式，不做大小写折叠。
pub fn is_allowed_origin(origin: &str) -> bool {
    if PROD_ORIGINS.contains(&origin) {
        return true;
    }
    ["http://localhost", "http://127.0.0.1", "http://[::1]"]
        .iter()
        .any(|base| match origin.strip_prefix(base) {
            Some("") => true,
            Some(rest) => rest.strip_prefix(':').is_some_and(|p| {
                !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit())
            }),
            None => false,
        })
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

/// 主题 id 只能是普通名字。与 core `wind_theme::theme::is_plain_slug` 同一判据（不依赖
/// wind-theme 是为了不把整套主题解析拉进设置端）；core 那边改了这里要跟着改。
pub fn validate_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug != "."
        && !slug.contains("..")
        && !slug.ends_with(['.', ' '])
        && !is_windows_reserved_name(slug)
        && !slug
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':') || c.is_control())
        && !Path::new(slug).is_absolute()
}

/// Windows 保留设备名（不分大小写，带不带扩展名都算）。同 core。
fn is_windows_reserved_name(s: &str) -> bool {
    let stem = s
        .split('.')
        .next()
        .unwrap_or(s)
        .trim_end()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
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

    let token = header_value(&req, TOKEN_HEADER);
    let Some(access) = authorize(token.as_deref(), origin.as_deref(), be.token) else {
        respond(req, unpaired_reply(allow_origin));
        return;
    };
    let package_slug = url
        .strip_prefix("/api/theme/")
        .and_then(|rest| rest.strip_suffix("/package"));
    if access == Access::Legacy && !is_legacy_route(&method, &url, package_slug.is_some()) {
        respond(req, unpaired_reply(allow_origin));
        return;
    }
    let reply = if method == "GET" && url == "/api/themes" {
        match (be.rpc)("theme.list", &serde_json::json!({})) {
            Ok(v) => json_reply(200, &editor_theme_list(v).to_string(), allow_origin),
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
    } else if method == "GET" && url == "/api/info" {
        json_reply(200, &info_body(be).to_string(), allow_origin)
    } else if method == "POST" && url == "/api/theme/push" {
        handle_push(req, allow_origin, be);
        return;
    } else if method == "POST" && url == "/api/theme/package" {
        handle_import_package(req, &raw_url, allow_origin, be);
        return;
    } else {
        json_reply(404, r#"{"ok":false,"error":"接口不存在"}"#, allow_origin)
    };
    respond(req, reply);
}

/// `GET /api/info` 的回包：编辑器据此比对预览引擎与输入法是否同一构建。
/// 核心没起 / 老核心没有 `build` 时对应字段为 null，不算错。
fn info_body(be: &Backend) -> Value {
    let info = (be.rpc)("system.info", &serde_json::json!({})).unwrap_or(Value::Null);
    let field = |k: &str| info.get(k).filter(|v| v.is_string()).cloned();
    serde_json::json!({
        "core_version": field("version"),
        "core_build": field("build"),
        "setting_version": env!("WIND_SETTING_VERSION"),
    })
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
        b"Content-Type, X-Wind-Token" as &[u8],
    ) {
        response = response.with_header(header);
    }
    // ACAO 是按请求的 Origin 反射的：告诉缓存这份响应随 Origin 而变，别拿给别的源。
    if let Ok(header) = tiny_http::Header::from_bytes(b"Vary" as &[u8], b"Origin" as &[u8]) {
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

/// 401：没配对或配对码不对。`pairing_required` 给编辑器认（据此提示「从输入法设置里
/// 点『打开编辑器』重新连接」）。
fn unpaired_reply(allow_origin: Option<String>) -> HttpReply {
    json_reply(
        401,
        &serde_json::json!({
            "ok": false,
            "pairing_required": true,
            "error": "未配对：请从输入法设置的「主题编辑器」里点「打开编辑器」重新连接",
        })
        .to_string(),
        allow_origin,
    )
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

fn handle_push(mut req: tiny_http::Request, allow_origin: Option<String>, be: &Backend) {
    const TOO_LARGE: &str = "主题内容超过 2 MiB 限制";
    if declared_too_large(&req, MAX_PUSH_BODY) {
        respond(req, error_reply(413, TOO_LARGE, allow_origin));
        return;
    }
    let read = read_limited(
        be.body_reader(&mut req),
        MAX_PUSH_BODY,
        &AtomicBool::new(false),
    );
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
    if slug.is_some_and(|s| !validate_slug(s)) {
        respond(req, error_reply(400, "非法主题 slug", allow_origin));
        return;
    }

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
    let saved = save_stream_raw(
        &dir,
        be.body_reader(&mut req),
        &THEME_PKG,
        &AtomicBool::new(false),
    );
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

/// `GET /api/themes` 的回包：`theme.list` 每项只留编辑器用到的字段（白名单）。这条是免 token
/// 的过渡路由，core 给设置端自己界面加的字段（用户主题的本机目录 `dir` 等）不该经它交给网页。
const EDITOR_THEME_FIELDS: &[&str] = &["name", "display_name", "author", "version", "builtin"];

fn editor_theme_list(v: Value) -> Value {
    let items = v.as_array().map_or(&[][..], Vec::as_slice);
    Value::Array(
        items
            .iter()
            .filter_map(Value::as_object)
            .map(|o| {
                Value::Object(
                    o.iter()
                        .filter(|(k, _)| EDITOR_THEME_FIELDS.contains(&k.as_str()))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                )
            })
            .collect(),
    )
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

    /// 测试用配对 token（与生产同为 32 位十六进制）。
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

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
        serve_with_timeout(n, be, Duration::from_secs(10), client)
    }

    fn serve_with_timeout(n: usize, be: &Backend, timeout: Duration, client: impl FnOnce(u16)) {
        let server = tiny_http::Server::http_with_socket_timeout("127.0.0.1:0", timeout).unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        std::thread::scope(|s| {
            s.spawn(|| {
                // 限时等：客户端那侧断言失败时请求不会来齐，死等会让用例挂死而不是变红。
                for _ in 0..n {
                    match server.recv_timeout(Duration::from_secs(10)) {
                        Ok(Some(req)) => handle_request(req, be),
                        _ => break,
                    }
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

    /// 发请求，自动在请求行后补上正确的配对 token。
    fn request(port: u16, head: &str, body: &[u8]) -> Reply {
        let (line, rest) = head.split_once("\r\n").unwrap();
        request_raw(
            port,
            &format!("{line}\r\n{TOKEN_HEADER}: {TOKEN}\r\n{rest}"),
            body,
        )
    }

    /// 原样发请求（不补 token）。
    fn request_raw(port: u16, head: &str, body: &[u8]) -> Reply {
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
            token: TOKEN,
            body_time: Duration::from_secs(60),
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
            token: TOKEN,
            body_time: Duration::from_secs(60),
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
            token: TOKEN,
            body_time: Duration::from_secs(60),
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
            token: TOKEN,
            body_time: Duration::from_secs(60),
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
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &no_rpc,
            tmp_dir: Some(&dir),
        };
        serve(1, &be, |port| {
            let mut s = connect(port);
            write!(
                s,
                "POST /api/theme/package HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Wind-Token: {TOKEN}\r\n\
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
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &no_rpc,
            tmp_dir: Some(&dir),
        };
        serve(1, &be, |port| {
            let mut s = connect(port);
            write!(
                s,
                "POST /api/theme/package HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Wind-Token: {TOKEN}\r\n\
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
            token: TOKEN,
            body_time: Duration::from_secs(60),
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
            token: TOKEN,
            body_time: Duration::from_secs(60),
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
            token: TOKEN,
            body_time: Duration::from_secs(60),
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
            token: TOKEN,
            body_time: Duration::from_secs(60),
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
            token: TOKEN,
            body_time: Duration::from_secs(60),
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
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &no_rpc,
            tmp_dir: None,
        };
        serve(2, &be, |port| {
            let mut s = connect(port);
            write!(
                s,
                "POST /api/theme/push HTTP/1.1\r\nHost: localhost:{port}\r\nX-Wind-Token: {TOKEN}\r\n\
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

    /// 回给网页的主题列表只留编辑器用的字段：用户主题的本机目录（`dir`）等不出本机。
    #[test]
    fn theme_list_for_editor_drops_local_fields() {
        let v = editor_theme_list(json!([{
            "name": "mono", "display_name": "墨", "author": "a", "version": "1",
            "builtin": false, "dir": "/home/x/themes/mono", "base": "_base",
            "skin": true, "animated": false,
        }]));
        let keys: Vec<&str> = v[0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["author", "builtin", "display_name", "name", "version"]
        );
        assert!(v[0].get("dir").is_none());
        assert_eq!(
            editor_theme_list(json!({"x": 1})),
            json!([]),
            "不是数组就给空表"
        );
    }

    fn themes_rpc(m: &str, _: &Value) -> Result<Value, String> {
        assert_eq!(m, "theme.list");
        Ok(json!([]))
    }

    /// 配对：没 token 也不是线上编辑器 → 401；token 不对 → 401（即便 Origin 是线上编辑器）；
    /// 本机开发服也得带 token。
    #[test]
    fn routes_require_pairing_token() {
        let be = Backend {
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &no_rpc,
            tmp_dir: None,
        };
        serve(4, &be, |port| {
            let head = |extra: &str| {
                format!(
                    "GET /api/themes HTTP/1.1\r\nHost: localhost:{{port}}\r\n{extra}\
                     Connection: close\r\n\r\n"
                )
            };
            for extra in [
                String::new(),
                "Origin: http://localhost:5173\r\n".to_string(),
                format!("{TOKEN_HEADER}: {}\r\n", "0".repeat(32)),
                format!("{TOKEN_HEADER}: wrong\r\nOrigin: https://theme.windinput.com\r\n"),
            ] {
                let r = request_raw(port, &head(&extra), b"");
                assert_eq!(r.status, 401, "{extra:?}");
                assert_eq!(r.json()["pairing_required"], true);
            }
        });
    }

    /// 过渡：已上线的编辑器（`https://theme.windinput.com`）不带 token 仍能用三条旧路由，
    /// 但新的收包 / 出包路由不行。
    #[test]
    fn legacy_editor_origin_keeps_old_routes_only() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let be = Backend {
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &themes_rpc,
            tmp_dir: None,
        };
        let legacy = "Origin: https://theme.windinput.com\r\n";
        let mut got = Vec::new();
        serve(4, &be, |port| {
            got.push(request_raw(
                port,
                &format!(
                    "GET /api/themes HTTP/1.1\r\nHost: localhost:{{port}}\r\n{legacy}\
                     Connection: close\r\n\r\n"
                ),
                b"",
            ));
            let body = json!({"toml": "[meta]\nname = \"测试主题\"\n"}).to_string();
            got.push(request_raw(
                port,
                &format!(
                    "POST /api/theme/push HTTP/1.1\r\nHost: localhost:{{port}}\r\n{legacy}\
                     Content-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n",
                    body.len()
                ),
                body.as_bytes(),
            ));
            got.push(request_raw(
                port,
                &format!(
                    "POST /api/theme/package HTTP/1.1\r\nHost: localhost:{{port}}\r\n{legacy}\
                     Content-Length: 4\r\nConnection: close\r\n\r\n"
                ),
                b"PK\x03\x04",
            ));
            got.push(request_raw(
                port,
                &format!(
                    "GET /api/theme/default/package HTTP/1.1\r\nHost: localhost:{{port}}\r\n\
                     {legacy}Connection: close\r\n\r\n"
                ),
                b"",
            ));
        });
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert_eq!(got[0].status, 200);
        assert_eq!(got[1].status, 200);
        assert_eq!(got[1].json()["slug"], "test-theme");
        assert_eq!(got[2].status, 401, "收包只认 token");
        assert_eq!(got[3].status, 401, "出包只认 token");
    }

    /// `/api/info`：要 token、不进 legacy；字段齐全，核心不可用时 core_* 为 null。
    #[test]
    fn info_route_needs_token_and_reports_core_fields() {
        let rpc = |m: &str, _: &Value| {
            assert_eq!(m, "system.info");
            Ok(
                json!({"version": "0.131.0", "build": "7b5ec94b-dirty", "dataDir": "/x", "variant": "dev"}),
            )
        };
        let be = Backend {
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &rpc,
            tmp_dir: None,
        };
        let req = |extra: &str| {
            format!(
                "GET /api/info HTTP/1.1\r\nHost: localhost:{{port}}\r\n{extra}\
                 Connection: close\r\n\r\n"
            )
        };
        serve(3, &be, |port| {
            let legacy = request_raw(port, &req("Origin: https://theme.windinput.com\r\n"), b"");
            assert_eq!(legacy.status, 401, "新路由不进 legacy");
            assert_eq!(request_raw(port, &req(""), b"").status, 401);
            let ok = request_raw(port, &req(&format!("{TOKEN_HEADER}: {TOKEN}\r\n")), b"");
            assert_eq!(ok.status, 200);
            let j = ok.json();
            assert_eq!(j["core_version"], "0.131.0");
            assert_eq!(j["core_build"], "7b5ec94b-dirty");
            assert!(j["setting_version"].is_string());
            let mut keys: Vec<_> = j.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            assert_eq!(keys, ["core_build", "core_version", "setting_version"]);
        });
        let down = |_: &str, _: &Value| Err("连接失败".to_string());
        let be = Backend { rpc: &down, ..be };
        serve(1, &be, |port| {
            let r = request_raw(port, &req(&format!("{TOKEN_HEADER}: {TOKEN}\r\n")), b"");
            assert_eq!(r.status, 200);
            assert!(r.json()["core_version"].is_null());
            assert!(r.json()["core_build"].is_null());
        });
    }

    /// 预检不要 token，且放行 `X-Wind-Token` 头；反射 ACAO 时带 `Vary: Origin`。
    #[test]
    fn preflight_allows_token_header_and_varies_on_origin() {
        let be = Backend {
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &no_rpc,
            tmp_dir: None,
        };
        serve(1, &be, |port| {
            let r = request_raw(
                port,
                "OPTIONS /api/theme/package HTTP/1.1\r\nHost: localhost:{port}\r\n\
                 Origin: https://theme.windinput.com\r\n\
                 Access-Control-Request-Method: POST\r\n\
                 Access-Control-Request-Headers: content-type, x-wind-token\r\n\
                 Connection: close\r\n\r\n",
                b"",
            );
            assert_eq!(r.status, 204);
            assert!(
                r.header("Access-Control-Allow-Headers")
                    .unwrap()
                    .contains("X-Wind-Token")
            );
            assert_eq!(
                r.header("Access-Control-Allow-Origin").as_deref(),
                Some("https://theme.windinput.com")
            );
            assert_eq!(r.header("Vary").as_deref(), Some("Origin"));
        });
    }

    /// 离谱的 Content-Length：回 413、进程不 abort（原 tiny_http 按声明长度一次性分配），
    /// 服务接着能处理下一个请求。
    #[test]
    fn absurd_content_length_does_not_take_the_server_down() {
        let be = Backend {
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &themes_rpc,
            tmp_dir: None,
        };
        serve_with_timeout(2, &be, Duration::from_millis(500), |port| {
            let mut s = connect(port);
            write!(
                s,
                "POST /api/theme/package HTTP/1.1\r\nHost: localhost:{port}\r\n\
                 X-Wind-Token: {TOKEN}\r\nContent-Length: {}\r\n\r\nPK",
                1u64 << 60
            )
            .unwrap();
            assert_eq!(read_reply(&mut s).status, 413);
            // 不断开、也不再发：服务端丢弃余量的那段读要靠读超时收手，不能挂死线程。
            let r = get(port, "/api/themes");
            assert_eq!(r.status, 200, "服务应仍然可用");
            drop(s);
        });
    }

    /// 请求体发一半就不动了：读超时到点就回错误，工作线程不被无限期占住。
    #[test]
    fn stalled_request_body_times_out() {
        let dir = scratch("stalled");
        let be = Backend {
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &no_rpc,
            tmp_dir: Some(&dir),
        };
        serve_with_timeout(1, &be, Duration::from_millis(300), |port| {
            let mut s = connect(port);
            write!(
                s,
                "POST /api/theme/package HTTP/1.1\r\nHost: localhost:{port}\r\n\
                 X-Wind-Token: {TOKEN}\r\nContent-Length: 100000\r\n\r\nPK\x03\x04"
            )
            .unwrap();
            let t = std::time::Instant::now();
            let r = read_reply(&mut s);
            assert_eq!(r.status, 400);
            assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        });
        assert!(is_empty(&dir), "半截临时文件要删");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 旧 push 带的 slug 也过校验。
    #[test]
    fn legacy_push_rejects_bad_slug() {
        let be = Backend {
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &no_rpc,
            tmp_dir: None,
        };
        serve(2, &be, |port| {
            for slug in ["../x", "CON"] {
                let r = post_push(
                    port,
                    json!({"toml": "[meta]\n", "slug": slug})
                        .to_string()
                        .as_bytes(),
                );
                assert_eq!(r.status, 400, "{slug}");
            }
        });
    }

    /// 卡在一个慢请求上的工作线程不拖住 `stop()`。
    #[test]
    fn stop_does_not_wait_for_a_stuck_request() {
        let mut server = ThemeEditorServer::default();
        let st = server.start().expect("应能启动");
        let port = st.port.unwrap();
        let token = st
            .editor_url
            .as_deref()
            .and_then(|u| u.split("wind-token=").nth(1))
            .unwrap()
            .to_string();
        let mut s = connect(port);
        write!(
            s,
            "POST /api/theme/package HTTP/1.1\r\nHost: localhost:{port}\r\n\
             X-Wind-Token: {token}\r\nContent-Length: 100000\r\n\r\nPK"
        )
        .unwrap();
        // 等服务线程收下请求、进入读请求体。
        std::thread::sleep(Duration::from_millis(500));
        let t = std::time::Instant::now();
        server.stop();
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        assert!(!server.status().running);
        drop(s);
    }

    /// 在一条连接上收请求直到对端关、或 `idle` 内没有新请求，返回处理了几个。
    fn serve_counting(be: &Backend, client: impl FnOnce(u16)) -> usize {
        let server =
            tiny_http::Server::http_with_socket_timeout("127.0.0.1:0", Duration::from_secs(5))
                .unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let count = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|s| {
            s.spawn(|| {
                while let Ok(Some(req)) = server.recv_timeout(Duration::from_millis(1500)) {
                    count.fetch_add(1, Ordering::Relaxed);
                    handle_request(req, be);
                }
            });
            client(port);
        });
        count.into_inner()
    }

    /// 藏在请求体里的第二个请求：线上编辑器的 Origin + 本机 Host，走过渡规则就能用旧路由。
    const SMUGGLED: &str = "GET /api/themes HTTP/1.1\r\nHost: localhost\r\n\
                            Origin: https://theme.windinput.com\r\n\r\n";

    fn recording_rpc(calls: &Calls) -> impl Fn(&str, &Value) -> Result<Value, String> + Sync + '_ {
        move |m: &str, p: &Value| {
            calls.lock().unwrap().push((m.to_string(), p.clone()));
            Ok(json!([]))
        }
    }

    /// 请求走私（声明长度）：被拒的请求体超过丢弃预算（1 MiB），剩下的字节不得被当成
    /// 同一连接上的下一个请求。
    #[test]
    fn body_past_discard_budget_is_not_parsed_as_next_request() {
        let calls = Calls::default();
        let rpc = recording_rpc(&calls);
        let be = Backend {
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &rpc,
            tmp_dir: None,
        };
        let n = serve_counting(&be, |port| {
            let mut body = vec![b'a'; 1024 * 1024];
            body.extend_from_slice(SMUGGLED.as_bytes());
            let mut s = connect(port);
            // 外域 Origin：被 guard 拒，请求体一字不读，交给 tiny_http 丢弃。
            write!(
                s,
                "POST /api/theme/push HTTP/1.1\r\nHost: localhost:{port}\r\n\
                 Origin: https://evil.com\r\nContent-Type: text/plain\r\n\
                 Content-Length: {}\r\n\r\n",
                body.len() + 4096
            )
            .unwrap();
            // 先收 403 再发请求体：服务端在读请求体前就回了，等它关连接时再发会被 RST，
            // 而 Windows 收到 RST 可能把已到达、还没读的 403 一并丢掉。
            let r = read_reply(&mut s);
            assert_eq!(r.status, 403);
            assert_eq!(r.header("Connection").as_deref(), Some("close"));
            // 服务端此刻正在丢弃余量：发过去的前 1 MiB 被丢掉，后面就是藏着的请求。
            let _ = s.write_all(&body);
            // 连接随后被服务端关掉，读不到第二个响应。
            let mut rest = Vec::new();
            let _ = s.read_to_end(&mut rest);
            assert!(
                rest.is_empty(),
                "不该有第二个响应：{}",
                String::from_utf8_lossy(&rest)
            );
        });
        assert_eq!(n, 1, "服务端只该看到一个请求");
        assert!(calls.lock().unwrap().is_empty(), "走私的请求不得走到 RPC");
    }

    /// 请求走私（分块）：分块请求体不经丢弃逻辑，残留字节同样不得被当成下一个请求。
    #[test]
    fn chunked_leftover_is_not_parsed_as_next_request() {
        let calls = Calls::default();
        let rpc = recording_rpc(&calls);
        let be = Backend {
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &rpc,
            tmp_dir: None,
        };
        let n = serve_counting(&be, |port| {
            let mut s = connect(port);
            write!(
                s,
                "POST /api/theme/push HTTP/1.1\r\nHost: localhost:{port}\r\n\
                 Origin: https://evil.com\r\nTransfer-Encoding: chunked\r\n\r\n"
            )
            .unwrap();
            // 先收 403 再发残留字节（理由同上一条）。
            let r = read_reply(&mut s);
            assert_eq!(r.status, 403);
            let _ = s.write_all(SMUGGLED.as_bytes());
            let mut rest = Vec::new();
            let _ = s.read_to_end(&mut rest);
            assert!(
                rest.is_empty(),
                "不该有第二个响应：{}",
                String::from_utf8_lossy(&rest)
            );
        });
        assert_eq!(n, 1);
        assert!(calls.lock().unwrap().is_empty());
    }

    /// 滴灌最多持续这么久（看门狗）：修复失效时服务端会一直读下去，没有它用例会挂死
    /// 而不是变红；有它则滴灌停下、读超时收尾，由耗时断言报红。
    const TRICKLE_WATCHDOG: Duration = Duration::from_secs(8);

    /// `Connection: close` 只加在最终响应上：`100 Continue` 是中间响应，带上它客户端会以为
    /// 连接要关、不再发请求体。
    #[test]
    fn continue_response_does_not_say_close() {
        let dir = scratch("expect100");
        let rpc = |_: &str, _: &Value| Ok(json!({"ok": true, "slug": "a", "display_name": "A"}));
        let be = Backend {
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &rpc,
            tmp_dir: Some(&dir),
        };
        let pkg = fixture(4096);
        serve(1, &be, |port| {
            let mut s = connect(port);
            write!(
                s,
                "POST /api/theme/package HTTP/1.1\r\nHost: localhost:{port}\r\n\
                 X-Wind-Token: {TOKEN}\r\nExpect: 100-continue\r\n\
                 Content-Length: {}\r\n\r\n",
                pkg.len()
            )
            .unwrap();
            let r = read_reply(&mut s);
            assert_eq!(r.status, 100);
            assert_eq!(r.header("Connection"), None);
            s.write_all(&pkg).unwrap();
            let r = read_reply(&mut s);
            assert_eq!(r.status, 200);
            assert_eq!(r.header("Connection").as_deref(), Some("close"));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 不支持的 HTTP 版本：回 505 后关连接，后面跟着的请求不得被接着解析。版本检查在读请求头
    /// 之前，所以紧跟请求行的那一行在「接着解析」时会被当成下一个请求行——藏的请求就放那里。
    #[test]
    fn unsupported_version_closes_the_connection() {
        let calls = Calls::default();
        let rpc = recording_rpc(&calls);
        let be = Backend {
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &rpc,
            tmp_dir: None,
        };
        let n = serve_counting(&be, |port| {
            let mut s = connect(port);
            write!(s, "POST /api/theme/push HTTP/2.0\r\n{SMUGGLED}").unwrap();
            let r = read_reply(&mut s);
            assert_eq!(r.status, 505);
            let mut rest = Vec::new();
            let _ = s.read_to_end(&mut rest);
            assert!(
                rest.is_empty(),
                "不该有第二个响应：{}",
                String::from_utf8_lossy(&rest)
            );
        });
        assert_eq!(n, 0);
        assert!(calls.lock().unwrap().is_empty());
    }

    /// 起一个线程每隔 `every` 往连接上滴一个字节，直到写失败、`stop` 置位或看门狗到点。
    fn trickle(s: &TcpStream, every: Duration) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        let mut w = s.try_clone().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let started = std::time::Instant::now();
        let h = std::thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) && started.elapsed() < TRICKLE_WATCHDOG {
                if w.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(every);
            }
        });
        (stop, h)
    }

    /// 应用侧总时限：请求体滴灌（每次读都不超时）也会在截止时间到点后回错误。
    #[test]
    fn trickled_body_hits_the_body_time_limit() {
        let dir = scratch("trickle_body");
        let be = Backend {
            token: TOKEN,
            body_time: Duration::from_millis(500),
            rpc: &no_rpc,
            tmp_dir: Some(&dir),
        };
        let t = std::time::Instant::now();
        serve_with_timeout(1, &be, Duration::from_secs(5), |port| {
            let mut s = connect(port);
            write!(
                s,
                "POST /api/theme/package HTTP/1.1\r\nHost: localhost:{port}\r\n\
                 X-Wind-Token: {TOKEN}\r\nContent-Length: 100000\r\n\r\nPK"
            )
            .unwrap();
            let (stop, h) = trickle(&s, Duration::from_millis(50));
            let r = read_reply(&mut s);
            assert_eq!(r.status, 400);
            assert!(r.json()["error"].as_str().unwrap().contains("超时"));
            stop.store(true, Ordering::Relaxed);
            h.join().unwrap();
        });
        // 回错误之后 tiny_http 丢弃余量也有总时限（2 秒），工作线程不会被滴灌拖住。
        assert!(t.elapsed() < Duration::from_secs(6), "{:?}", t.elapsed());
        assert!(is_empty(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// tiny_http 丢弃余量的总时限：回了 413 之后对端还在滴灌，丢弃也会在约 2 秒后收手。
    #[test]
    fn trickled_discard_stops_after_time_limit() {
        let be = Backend {
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &no_rpc,
            tmp_dir: None,
        };
        let t = std::time::Instant::now();
        let mut keep = None;
        serve_with_timeout(1, &be, Duration::from_secs(5), |port| {
            let mut s = connect(port);
            write!(
                s,
                "POST /api/theme/package HTTP/1.1\r\nHost: localhost:{port}\r\n\
                 X-Wind-Token: {TOKEN}\r\nContent-Length: {}\r\n\r\n",
                MAX_THEME_PACKAGE_BYTES + 1
            )
            .unwrap();
            assert_eq!(read_reply(&mut s).status, 413);
            keep = Some((trickle(&s, Duration::from_millis(50)), s));
        });
        // serve 返回 = 工作线程已处理完这个请求（含丢弃余量）。
        let elapsed = t.elapsed();
        if let Some(((stop, h), _s)) = keep {
            stop.store(true, Ordering::Relaxed);
            h.join().unwrap();
        }
        assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    }

    /// 过渡规则是白名单：线上编辑器的 Origin 不带 token，除三条旧路由外一律 401。
    #[test]
    fn legacy_origin_is_limited_to_the_three_old_routes() {
        // 记录而不 panic：放行规则退化时这里照常回包，断言当场变红而不是让服务线程挂掉。
        let calls = Calls::default();
        let rpc = recording_rpc(&calls);
        let be = Backend {
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &rpc,
            tmp_dir: None,
        };
        let cases = [
            ("GET", "/api/nope"),
            ("GET", "/api/theme/"),
            ("GET", "/api/theme/a/b"),
            ("GET", "/api/theme/x/package"),
            ("POST", "/api/themes"),
            ("POST", "/api/theme/package"),
            ("PUT", "/api/theme/push"),
            ("DELETE", "/api/theme/x"),
        ];
        serve(cases.len(), &be, |port| {
            for (m, path) in cases {
                let r = request_raw(
                    port,
                    &format!(
                        "{m} {path} HTTP/1.1\r\nHost: localhost:{{port}}\r\n\
                         Origin: https://theme.windinput.com\r\nContent-Length: 0\r\n\
                         Connection: close\r\n\r\n"
                    ),
                    b"",
                );
                assert_eq!(r.status, 401, "{m} {path}");
            }
        });
        assert!(calls.lock().unwrap().is_empty(), "被拒的请求不得走到 RPC");
        assert!(is_legacy_route("GET", "/api/themes", false));
        assert!(is_legacy_route("GET", "/api/theme/水墨", false));
        assert!(is_legacy_route("POST", "/api/theme/push", false));
    }

    /// 请求头上限：单行超 16 KiB、或超过 100 条，tiny_http 直接回 400 并关连接，请求到不了
    /// 处理函数。
    #[test]
    fn oversized_request_head_is_rejected_before_routing() {
        let be = Backend {
            token: TOKEN,
            body_time: Duration::from_secs(60),
            rpc: &no_rpc,
            tmp_dir: None,
        };
        let n = serve_counting(&be, |port| {
            let long_line = format!(
                "GET /api/themes HTTP/1.1\r\nHost: localhost:{port}\r\nX-Pad: {}\r\n\r\n",
                "a".repeat(17 * 1024)
            );
            let long_target = format!(
                "GET /api/themes?{} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n",
                "a".repeat(17 * 1024)
            );
            let many: String = (0..101).map(|i| format!("X-H{i}: v\r\n")).collect();
            let many = format!("GET /api/themes HTTP/1.1\r\nHost: localhost:{port}\r\n{many}\r\n");
            // 先只发到刚越过上限处，收到 400 再发剩下的（写失败不管）：服务端回完 400 就关
            // 连接，那时接收缓冲里还有没读的字节会触发 RST，Windows 收到 RST 可能把已到达
            // 的 400 一并丢掉（同走私用例）。
            let over =
                |head: &str, from: &str| head.find(from).unwrap() + from.len() + 16 * 1024 + 64;
            let cuts = [
                over(&long_line, "X-Pad: "),
                over(&long_target, "?"),
                // 第 101 条请求头的行尾：服务端读到这里就拒，末尾空行留到后面再发。
                many.len() - 2,
            ];
            for (head, cut) in [long_line, long_target, many].iter().zip(cuts) {
                let mut s = connect(port);
                s.write_all(&head.as_bytes()[..cut]).unwrap();
                let r = read_reply(&mut s);
                assert_eq!(r.status, 400);
                let _ = s.write_all(&head.as_bytes()[cut..]);
                let mut rest = Vec::new();
                let _ = s.read_to_end(&mut rest);
                assert!(rest.is_empty(), "连接应已关闭");
            }
            // 正好 100 条的照常进来（到路由，因没 token 回 401）。
            let ok: String = (0..98).map(|i| format!("X-H{i}: v\r\n")).collect();
            let r = request_raw(
                port,
                &format!(
                    "GET /api/themes HTTP/1.1\r\nHost: localhost:{{port}}\r\n{ok}\
                     Connection: close\r\n\r\n"
                ),
                b"",
            );
            assert_eq!(r.status, 401);
        });
        assert_eq!(n, 1, "超限的三个请求都不该到处理函数");
    }
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    #[test]
    fn origin_whitelist_is_exact() {
        for ok in [
            "https://theme.windinput.com",
            "https://windinput.com",
            "http://localhost",
            "http://localhost:5173",
            "http://127.0.0.1:8080",
            "http://[::1]:3000",
        ] {
            assert!(is_allowed_origin(ok), "{ok}");
        }
        for bad in [
            "https://market.windinput.com",
            "https://evil.windinput.com",
            "https://windinput.com.evil.com",
            "http://windinput.com",
            "https://theme.windinput.com:8443",
            "http://localhost:5173.evil.com",
            "http://localhost:",
            "http://localhost:123456",
            "http://localhost.evil.com",
            "https://localhost:5173",
            "null",
            "",
        ] {
            assert!(!is_allowed_origin(bad), "{bad}");
        }
    }

    #[test]
    fn token_comparison_and_shape() {
        let t = new_token().unwrap();
        assert_eq!(t.len(), 32);
        assert!(t.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(t, new_token().unwrap(), "每次都是新的");
        assert!(token_matches(&t, &t));
        assert!(!token_matches(&t[..31], &t));
        assert!(!token_matches("", &t));
        let mut other = t.clone().into_bytes();
        other[31] = if other[31] == b'0' { b'1' } else { b'0' };
        assert!(!token_matches(std::str::from_utf8(&other).unwrap(), &t));
    }

    #[test]
    fn authorize_prefers_token_and_limits_legacy_to_live_editor() {
        let t = "0123456789abcdef0123456789abcdef";
        assert_eq!(authorize(Some(t), None, t), Some(Access::Paired));
        assert_eq!(
            authorize(Some("bad"), Some("https://theme.windinput.com"), t),
            None,
            "带了错的 token 不退回过渡规则"
        );
        assert_eq!(
            authorize(None, Some("https://theme.windinput.com"), t),
            Some(Access::Legacy)
        );
        for o in [
            None,
            Some("https://windinput.com"),
            Some("http://localhost:5173"),
        ] {
            assert_eq!(authorize(None, o, t), None, "{o:?}");
        }
    }

    #[test]
    fn editor_url_carries_port_and_token_in_fragment() {
        assert_eq!(
            editor_connect_url(29732, "ab"),
            "https://theme.windinput.com/#wind-port=29732&wind-token=ab"
        );
    }

    /// 与 core `is_plain_slug` 同一判据。
    #[test]
    fn slug_rules_match_core() {
        for ok in ["default", "水墨", "a.b", "CONSOLE", "COM0", "my theme"] {
            assert!(validate_slug(ok), "{ok}");
        }
        for bad in [
            "", ".", "..", "a..b", "a/b", r"a\b", "C:x", "a.", "a ", "CON", "con.txt", "Nul",
            "COM1", "lpt9.x", "a\u{0}b", "a\nb",
        ] {
            assert!(!validate_slug(bad), "{bad:?}");
        }
    }
}
