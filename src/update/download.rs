//! 升级包下载与完整性校验。
//!
//! 核心流式逻辑 [`stream_to_file`] 泛型于 `Read`，与 HTTP 层解耦 —— 截断、哈希不符、
//! 取消、超长这四条路径都能脱网单测。
//!
//! 校验规则（比旧 Go 版严）：
//! - 累计字节**超过** `expected_size` 立即中止，不等读完（防超长响应撑爆磁盘）
//! - 读完后字节数必须**恰好等于** `expected_size`
//! - 流式 sha256 必须匹配
//! - **缓存复用也要重算 sha256** —— 旧版只比大小，且官网渠道 size 恒为 0，
//!   等于 `%TEMP%` 里一个被截断的同名文件会被当作完整包直接安装
//!
//! 超时用「无数据推进」的读超时而非请求总超时：几十 MB 的包在慢网下耗时不可预估，
//! 总超时会让慢速用户永远升不上去。
//!
//! # 分片并发
//!
//! 体积够大且服务端确实支持 `Range` 时走 [`download_parallel`]：N 个线程各取一段，
//! 按绝对 offset 写进同一个预分配的 `.part`。**任何**不确定都回退单连接
//! ([`stream_to_file`]) —— 分片是性能优化，不该成为升级失败的新来源。
//!
//! 没有引入第三方分片下载库：现存的（manic / parallel_downloader 等）无一例外拖
//! tokio + reqwest，会给这个纯同步、无异步运行时的进程塞进第二套 TLS 栈与上百个
//! 传递依赖，代价远超一个 46 MB 单文件下载所值。ureq 是阻塞 IO，N 个 OS 线程足矣。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::UpdateError;

/// 建连超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 单次读超时 —— 语义是「无数据推进」，不是下载总时长。
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// 进度上报节流间隔。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
/// 读缓冲。
const CHUNK: usize = 64 * 1024;
/// 分片并发数。4 是折中：再多则每片过小、建连开销与服务端并发限制开始吃掉收益。
const PARALLEL_PARTS: u64 = 4;
/// 低于此体积不分片 —— 小文件的建连与探测开销盖过并发收益。
const MIN_PARALLEL_SIZE: u64 = 8 * 1024 * 1024;

/// 同一时刻只允许一个下载。旧 Go 版用互斥锁，第二次调用会**静默阻塞**直到第一次结束，
/// 前端无从感知；这里改为立即返回明确错误。
static DOWNLOAD_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
struct ActiveGuard;

impl ActiveGuard {
    fn acquire() -> Result<Self, UpdateError> {
        DOWNLOAD_ACTIVE
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| UpdateError::Download("已有下载正在进行".to_string()))?;
        Ok(Self)
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        DOWNLOAD_ACTIVE.store(false, Ordering::SeqCst);
    }
}

/// 下载升级包到 `dest`，校验通过后返回其路径。
///
/// `cancel` 置 true 后在下一个数据块边界中止并清理临时文件。
/// `progress` 收到 `(已下载, 总字节)`，按 200ms 节流，结束时补发一次 100%。
pub fn download(
    url: &str,
    expected_sha256: &str,
    expected_size: u64,
    dest: &Path,
    cancel: Arc<AtomicBool>,
    mut progress: impl FnMut(u64, u64),
) -> Result<PathBuf, UpdateError> {
    // 参数校验先于获取独占权：非法输入不该占着下载位，也让该分支可独立测试
    if !url.to_ascii_lowercase().starts_with("https://") {
        return Err(UpdateError::Download("下载地址必须是 https://".to_string()));
    }

    let _guard = ActiveGuard::acquire()?;

    // 缓存命中要重算哈希才敢复用
    if let Some(path) = try_reuse_cached(dest, expected_sha256, expected_size) {
        return Ok(path);
    }

    let agent = build_agent();

    if expected_size >= MIN_PARALLEL_SIZE && supports_ranges(&agent, url, expected_size) {
        match download_parallel(
            &agent,
            url,
            expected_sha256,
            expected_size,
            dest,
            cancel.clone(),
            &mut progress,
        ) {
            Ok(path) => return Ok(path),
            // 用户取消是意图，不是故障 —— 绝不能"回退重试"把取消掉的下载又跑一遍
            Err(UpdateError::Cancelled) => return Err(UpdateError::Cancelled),
            Err(e) => {
                // 校验失败也回退：分片写错 offset 与内容被掉包在 sha256 层面无法区分，
                // 单连接重来一次能把前者自愈，后者仍会在下面再次失败并如实报错。
                tracing::warn!("分片下载失败，回退单连接重试：{}", e.user_message());
                progress(0, expected_size);
            }
        }
    }

    let resp = agent
        .get(url)
        .call()
        .map_err(|e| UpdateError::Download(format!("{e}")))?;

    stream_to_file(
        resp.into_reader(),
        expected_sha256,
        expected_size,
        dest,
        cancel,
        progress,
    )
}

fn build_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout_read(READ_TIMEOUT)
        .redirects(3)
        .build()
}

/// 探测服务端是否真的支持 `Range`：只取 1 字节，任何异常一律当作「不支持」。
///
/// 必须校验 `Content-Range` 而不只看 206 —— 部分 CDN/代理对 Range 请求直接返回
/// **200 全量**，那样每个分片线程都会把整包写到自己的 offset 上，文件彻底损坏。
/// sha256 虽能兜住，但用户只会看到"校验值不匹配"，无从定位真因。
fn supports_ranges(agent: &ureq::Agent, url: &str, expected_size: u64) -> bool {
    match agent.get(url).set("Range", "bytes=0-0").call() {
        Ok(resp) => {
            if resp.status() != 206 {
                tracing::info!("服务端未返回 206（{}），走单连接", resp.status());
                return false;
            }
            match resp.header("Content-Range") {
                Some(cr) => check_content_range(cr, 0, 0, expected_size).is_ok(),
                None => false,
            }
        }
        Err(e) => {
            tracing::info!("Range 探测失败，走单连接：{e}");
            false
        }
    }
}

/// 校验 `Content-Range: bytes START-END/TOTAL` 与本分片的请求区间一致。
///
/// TOTAL 为 `*`（长度未知）时只校验区间 —— 合法但少见，不必因此拒绝。
fn check_content_range(header: &str, start: u64, end: u64, total: u64) -> Result<(), UpdateError> {
    let bad = || UpdateError::Verify(format!("Content-Range 不符：{header}"));
    let rest = header.trim().strip_prefix("bytes ").ok_or_else(bad)?;
    let (range, got_total) = rest.split_once('/').ok_or_else(bad)?;
    let (s, e) = range.split_once('-').ok_or_else(bad)?;
    let s: u64 = s.trim().parse().map_err(|_| bad())?;
    let e: u64 = e.trim().parse().map_err(|_| bad())?;
    if s != start || e != end {
        return Err(bad());
    }
    let got_total = got_total.trim();
    if got_total != "*" && got_total.parse::<u64>().map_err(|_| bad())? != total {
        return Err(bad());
    }
    Ok(())
}

/// 把 `size` 字节切成 `parts` 段闭区间 `[start, end]`，余数摊给靠前的分片。
fn split_parts(size: u64, parts: u64) -> Vec<(u64, u64)> {
    if size == 0 {
        return Vec::new();
    }
    let parts = parts.clamp(1, size);
    let (base, rem) = (size / parts, size % parts);
    let mut out = Vec::with_capacity(parts as usize);
    let mut start = 0u64;
    for i in 0..parts {
        let len = base + u64::from(i < rem);
        out.push((start, start + len - 1));
        start += len;
    }
    out
}

/// 分片并发下载。各线程写同一个预分配 `.part` 的互不重叠区间，全部完成后整体校验。
///
/// 与单连接路径的取舍：sha256 无法再边下边算（字节乱序到达），改为落盘后整读一遍。
/// 46 MB 在 SSD 上约 0.1–0.3s，换取的并发收益远大于此。
fn download_parallel(
    agent: &ureq::Agent,
    url: &str,
    expected_sha256: &str,
    expected_size: u64,
    dest: &Path,
    cancel: Arc<AtomicBool>,
    mut progress: impl FnMut(u64, u64),
) -> Result<PathBuf, UpdateError> {
    let dir = dest.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)
        .map_err(|e| UpdateError::Download(format!("创建下载目录失败：{e}")))?;

    let tmp = dest.with_extension("part");
    // 预分配：分片按绝对 offset 定位写入，文件必须先具备完整长度
    {
        let f = File::create(&tmp)
            .map_err(|e| UpdateError::Download(format!("创建临时文件失败：{e}")))?;
        f.set_len(expected_size)
            .map_err(|e| UpdateError::Download(format!("预分配失败：{e}")))?;
    }

    let downloaded = Arc::new(AtomicU64::new(0));
    // 与用户取消旗分开：一个分片失败要叫停其余分片，但那不是"用户取消"，
    // 混用会让上层把网络故障误报成"已取消下载"而不提示任何错误。
    let abort = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();

    let mut handles = Vec::new();
    for (start, end) in split_parts(expected_size, PARALLEL_PARTS) {
        let agent = agent.clone();
        let url = url.to_string();
        let tmp = tmp.clone();
        let (cancel, abort, downloaded, tx) =
            (cancel.clone(), abort.clone(), downloaded.clone(), tx.clone());
        handles.push(std::thread::spawn(move || {
            let r = fetch_part(
                &agent,
                &url,
                start,
                end,
                expected_size,
                &tmp,
                &cancel,
                &abort,
                &downloaded,
            );
            if r.is_err() {
                abort.store(true, Ordering::SeqCst);
            }
            let _ = tx.send(r);
        }));
    }
    drop(tx); // 唯一的主线程副本，否则 recv 永不断开

    // 边等边报进度：worker 只累加原子量，回调始终在本线程调用，故不要求 FnMut: Send
    let mut remaining = handles.len();
    let mut first_err: Option<UpdateError> = None;
    while remaining > 0 {
        match rx.recv_timeout(PROGRESS_INTERVAL) {
            Ok(res) => {
                remaining -= 1;
                if let Err(e) = res
                    && first_err.is_none()
                {
                    first_err = Some(e);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        progress(
            downloaded.load(Ordering::Relaxed).min(expected_size),
            expected_size,
        );
    }
    for h in handles {
        let _ = h.join();
    }

    let fail = |e: UpdateError| -> UpdateError {
        let _ = std::fs::remove_file(&tmp);
        e
    };

    if let Some(e) = first_err {
        return Err(fail(e));
    }

    let total = downloaded.load(Ordering::Relaxed);
    if total != expected_size {
        return Err(fail(UpdateError::Verify(format!(
            "下载不完整（{total}/{expected_size} 字节）"
        ))));
    }

    let actual = sha256_of_file(&tmp)
        .map_err(|e| UpdateError::Download(format!("校验时读取失败：{e}")))
        .map_err(&fail)?;
    if actual != expected_sha256 {
        return Err(fail(UpdateError::Verify(
            "安装包校验值不匹配，已丢弃".to_string(),
        )));
    }

    // 目标已存在（并发或残留）时 rename 在 Windows 上会失败，先移除
    let _ = std::fs::remove_file(dest);
    std::fs::rename(&tmp, dest)
        .map_err(|e| UpdateError::Download(format!("落盘失败：{e}")))
        .map_err(&fail)?;

    progress(expected_size, expected_size);
    Ok(dest.to_path_buf())
}

/// 取回单个分片并写入 `tmp` 的 `[start, end]` 区间。
///
/// 各线程持有独立文件句柄并 seek 到自己的起点，写区间互不重叠 —— 这样既不需要锁，
/// 也避开了 `seek_write`/`write_at` 的平台差异（测试因此能在非 Windows 上跑）。
#[allow(clippy::too_many_arguments)]
fn fetch_part(
    agent: &ureq::Agent,
    url: &str,
    start: u64,
    end: u64,
    total: u64,
    tmp: &Path,
    cancel: &AtomicBool,
    abort: &AtomicBool,
    downloaded: &AtomicU64,
) -> Result<(), UpdateError> {
    let resp = agent
        .get(url)
        .set("Range", &format!("bytes={start}-{end}"))
        .call()
        .map_err(|e| UpdateError::Download(format!("分片 {start}-{end} 请求失败：{e}")))?;

    if resp.status() != 206 {
        return Err(UpdateError::Verify(format!(
            "分片 {start}-{end} 未返回 206（实际 {}）",
            resp.status()
        )));
    }
    let content_range = resp.header("Content-Range").unwrap_or_default().to_string();
    check_content_range(&content_range, start, end, total)?;

    let mut file = File::options()
        .write(true)
        .open(tmp)
        .map_err(|e| UpdateError::Download(format!("打开临时文件失败：{e}")))?;
    file.seek(SeekFrom::Start(start))
        .map_err(|e| UpdateError::Download(format!("定位到 {start} 失败：{e}")))?;

    let want = end - start + 1;
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; CHUNK];
    let mut got: u64 = 0;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(UpdateError::Cancelled);
        }
        if abort.load(Ordering::Relaxed) {
            return Err(UpdateError::Download("其它分片失败，已中止".to_string()));
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(UpdateError::Download(format!("分片读取失败：{e}"))),
        };
        got += n as u64;
        // 超长立即中止，与单连接路径同规格：不等对方发完
        if got > want {
            return Err(UpdateError::Verify(format!(
                "分片 {start}-{end} 超出预期长度"
            )));
        }
        file.write_all(&buf[..n])
            .map_err(|e| UpdateError::Download(format!("写入失败：{e}")))?;
        downloaded.fetch_add(n as u64, Ordering::Relaxed);
    }

    if got != want {
        return Err(UpdateError::Verify(format!(
            "分片 {start}-{end} 不完整（{got}/{want} 字节）"
        )));
    }
    file.flush()
        .map_err(|e| UpdateError::Download(format!("刷新缓冲失败：{e}")))?;
    Ok(())
}

/// 已存在且哈希匹配的文件可直接复用；否则删掉重下。
fn try_reuse_cached(dest: &Path, expected_sha256: &str, expected_size: u64) -> Option<PathBuf> {
    let meta = std::fs::metadata(dest).ok()?;
    if meta.len() != expected_size {
        let _ = std::fs::remove_file(dest);
        return None;
    }
    match sha256_of_file(dest) {
        Ok(h) if h == expected_sha256 => {
            tracing::info!("复用已下载的升级包：{}", dest.display());
            Some(dest.to_path_buf())
        }
        _ => {
            let _ = std::fs::remove_file(dest);
            None
        }
    }
}

fn sha256_of_file(path: &Path) -> Result<String, std::io::Error> {
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// 流式写盘 + 校验。先写同目录临时文件，全部校验通过后再 rename 到目标名 ——
/// 保证目标路径要么不存在，要么是一个已验证完整的包。
fn stream_to_file(
    mut reader: impl Read,
    expected_sha256: &str,
    expected_size: u64,
    dest: &Path,
    cancel: Arc<AtomicBool>,
    mut progress: impl FnMut(u64, u64),
) -> Result<PathBuf, UpdateError> {
    let dir = dest.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)
        .map_err(|e| UpdateError::Download(format!("创建下载目录失败：{e}")))?;

    let tmp = dest.with_extension("part");
    let mut file =
        File::create(&tmp).map_err(|e| UpdateError::Download(format!("创建临时文件失败：{e}")))?;

    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut downloaded: u64 = 0;
    let mut last_report = Instant::now();

    let cleanup = |file: File, tmp: &Path| {
        drop(file);
        let _ = std::fs::remove_file(tmp);
    };

    loop {
        if cancel.load(Ordering::Relaxed) {
            cleanup(file, &tmp);
            return Err(UpdateError::Cancelled);
        }

        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                cleanup(file, &tmp);
                return Err(UpdateError::Download(format!("读取响应失败：{e}")));
            }
        };

        downloaded += n as u64;
        // 超长立即中止，不等对方发完
        if downloaded > expected_size {
            cleanup(file, &tmp);
            return Err(UpdateError::Verify(format!(
                "下载内容超出预期大小（{expected_size} 字节）"
            )));
        }

        if let Err(e) = file.write_all(&buf[..n]) {
            cleanup(file, &tmp);
            return Err(UpdateError::Download(format!("写入失败：{e}")));
        }
        hasher.update(&buf[..n]);

        if last_report.elapsed() >= PROGRESS_INTERVAL {
            progress(downloaded, expected_size);
            last_report = Instant::now();
        }
    }

    if let Err(e) = file.flush() {
        cleanup(file, &tmp);
        return Err(UpdateError::Download(format!("刷新缓冲失败：{e}")));
    }
    drop(file);

    // 截断：对端提前断开时 read 返回 0，靠字节数兜住
    if downloaded != expected_size {
        let _ = std::fs::remove_file(&tmp);
        return Err(UpdateError::Verify(format!(
            "下载不完整（{downloaded}/{expected_size} 字节）"
        )));
    }

    let actual = hex_lower(&hasher.finalize());
    if actual != expected_sha256 {
        let _ = std::fs::remove_file(&tmp);
        return Err(UpdateError::Verify(
            "安装包校验值不匹配，已丢弃".to_string(),
        ));
    }

    // 目标已存在（并发或残留）时 rename 在 Windows 上会失败，先移除
    let _ = std::fs::remove_file(dest);
    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        UpdateError::Download(format!("落盘失败：{e}"))
    })?;

    // 补发 100%：最后一个数据块多半被节流吞掉
    progress(downloaded, expected_size);
    Ok(dest.to_path_buf())
}

/// 升级包的本地缓存路径：临时目录 + 从 URL 末段取的文件名。
///
/// 文件名经过净化 —— URL 由远端元数据提供，不能让 `../` 之类的内容决定写盘位置。
pub fn cache_path(url: &str, fallback_name: &str) -> PathBuf {
    let raw = url
        .rsplit('/')
        .next()
        .map(|s| s.split(['?', '#']).next().unwrap_or(s))
        .unwrap_or("");
    let name = sanitize_file_name(raw);
    let name = if name.is_empty() {
        sanitize_file_name(fallback_name)
    } else {
        name
    };
    let name = if name.is_empty() {
        "wind-update.exe".to_string()
    } else {
        name
    };
    std::env::temp_dir().join(name)
}

/// 只保留文件名安全字符，杜绝路径分隔符与父目录引用。
fn sanitize_file_name(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
        .collect::<String>()
        .trim_matches('.')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::AtomicUsize;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "wind_update_test_{}_{}_{tag}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn sha256_of(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        hex_lower(&h.finalize())
    }

    fn no_cancel() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn downloads_and_verifies_happy_path() {
        let dir = temp_dir("ok");
        let dest = dir.join("pkg.exe");
        let data = vec![7u8; 300 * 1024];
        let sha = sha256_of(&data);

        let mut seen: Vec<(u64, u64)> = Vec::new();
        let path = stream_to_file(
            Cursor::new(data.clone()),
            &sha,
            data.len() as u64,
            &dest,
            no_cancel(),
            |d, t| seen.push((d, t)),
        )
        .unwrap();

        assert_eq!(path, dest);
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert!(!dest.with_extension("part").exists(), "临时文件应已清理");
        // 结束必定补发一次 100%
        let last = seen.last().copied().unwrap();
        assert_eq!(last, (data.len() as u64, data.len() as u64));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 对端提前断开：读到 EOF 但字节数不足。
    #[test]
    fn rejects_truncated_download() {
        let dir = temp_dir("trunc");
        let dest = dir.join("pkg.exe");
        let full = vec![1u8; 1000];
        let sha = sha256_of(&full);

        let err = stream_to_file(
            Cursor::new(vec![1u8; 400]), // 只发来一部分
            &sha,
            full.len() as u64,
            &dest,
            no_cancel(),
            |_, _| {},
        )
        .unwrap_err();

        assert!(matches!(err, UpdateError::Verify(_)));
        assert!(err.user_message().contains("不完整"));
        assert!(!dest.exists(), "校验失败不得留下目标文件");
        assert!(!dest.with_extension("part").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 内容被掉包：长度对得上但哈希不符。
    #[test]
    fn rejects_hash_mismatch() {
        let dir = temp_dir("hash");
        let dest = dir.join("pkg.exe");
        let expected = vec![1u8; 1000];
        let tampered = vec![2u8; 1000];

        let err = stream_to_file(
            Cursor::new(tampered),
            &sha256_of(&expected),
            1000,
            &dest,
            no_cancel(),
            |_, _| {},
        )
        .unwrap_err();

        assert!(matches!(err, UpdateError::Verify(_)));
        assert!(err.user_message().contains("校验值"));
        assert!(!dest.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 超长响应必须在超出的那一刻就停，而不是读完再判。
    #[test]
    fn aborts_when_exceeding_expected_size() {
        let dir = temp_dir("over");
        let dest = dir.join("pkg.exe");

        let err = stream_to_file(
            Cursor::new(vec![3u8; 500_000]),
            &sha256_of(&[]),
            1000, // 声称只有 1000 字节
            &dest,
            no_cancel(),
            |_, _| {},
        )
        .unwrap_err();

        assert!(matches!(err, UpdateError::Verify(_)));
        assert!(err.user_message().contains("超出"));
        assert!(!dest.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_stops_and_cleans_up() {
        let dir = temp_dir("cancel");
        let dest = dir.join("pkg.exe");
        let data = vec![4u8; 500 * 1024];
        let sha = sha256_of(&data);
        let cancel = Arc::new(AtomicBool::new(true)); // 一进循环就取消

        let err = stream_to_file(
            Cursor::new(data.clone()),
            &sha,
            data.len() as u64,
            &dest,
            cancel,
            |_, _| {},
        )
        .unwrap_err();

        assert!(matches!(err, UpdateError::Cancelled));
        assert!(!dest.exists());
        assert!(!dest.with_extension("part").exists(), "取消也要清理临时文件");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reuses_cached_file_only_when_hash_matches() {
        let dir = temp_dir("reuse");
        let dest = dir.join("pkg.exe");
        let data = vec![9u8; 2048];
        let sha = sha256_of(&data);
        std::fs::write(&dest, &data).unwrap();

        // 哈希一致 → 复用
        assert_eq!(
            try_reuse_cached(&dest, &sha, data.len() as u64),
            Some(dest.clone())
        );

        // 哈希不符 → 不复用且删除，避免下次再被误判
        assert_eq!(try_reuse_cached(&dest, &"f".repeat(64), 2048), None);
        assert!(!dest.exists(), "校验失败的缓存应被清除");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 旧版的实际风险路径：大小对但内容是截断的旧文件，只比大小就会直接安装。
    #[test]
    fn cached_file_with_right_size_but_wrong_content_is_rejected() {
        let dir = temp_dir("stale");
        let dest = dir.join("pkg.exe");
        std::fs::write(&dest, vec![0u8; 2048]).unwrap();

        let expected = sha256_of(&vec![9u8; 2048]);
        assert_eq!(try_reuse_cached(&dest, &expected, 2048), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cached_file_with_wrong_size_is_discarded() {
        let dir = temp_dir("size");
        let dest = dir.join("pkg.exe");
        std::fs::write(&dest, vec![0u8; 100]).unwrap();

        assert_eq!(try_reuse_cached(&dest, &"a".repeat(64), 2048), None);
        assert!(!dest.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 独占权的获取与释放合并为一个测试：`DOWNLOAD_ACTIVE` 是进程全局状态，
    /// 拆成多个测试会在并行跑时互相抢占（正是 `state`/`theme_import` 两处
    /// 各自持有独立 ENV_LOCK 却共享环境变量所犯的错）。
    #[test]
    fn active_guard_is_exclusive_and_releases_on_drop() {
        {
            let _first = ActiveGuard::acquire().expect("首个下载应能获取");

            let second = ActiveGuard::acquire();
            assert!(second.is_err(), "并发下载应立即报错而非静默阻塞");
            assert!(
                second.unwrap_err().user_message().contains("已有下载"),
                "错误应说明原因，供 UI 直接展示"
            );
        }
        // first 已 drop
        assert!(
            ActiveGuard::acquire().is_ok(),
            "上一个下载结束后应可再次下载"
        );
    }

    #[test]
    fn download_rejects_non_https() {
        let dir = temp_dir("scheme");
        let err = download(
            "http://dl.windinput.com/pkg.exe",
            &"a".repeat(64),
            1,
            &dir.join("p.exe"),
            no_cancel(),
            |_, _| {},
        )
        .unwrap_err();
        assert!(err.user_message().contains("https"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 分片并发 ----

    /// 可控的本地 Range 服务器。`honor_ranges = false` 模拟对 Range 请求返回
    /// **200 全量**的坏代理 —— 这是分片下载最危险的真实故障模式。
    struct RangeServer {
        url: String,
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl RangeServer {
        fn start(data: Vec<u8>, honor_ranges: bool) -> Self {
            let server = tiny_http::Server::http("127.0.0.1:0").expect("测试服务器应能监听");
            let port = server.server_addr().to_ip().expect("应为 IP 监听").port();
            let stop = Arc::new(AtomicBool::new(false));
            let s = stop.clone();
            // 串行处理：各 worker 只等自己的响应，无循环依赖，不会死锁
            let handle = std::thread::spawn(move || {
                while !s.load(Ordering::Relaxed) {
                    if let Ok(Some(req)) = server.recv_timeout(Duration::from_millis(50)) {
                        serve(req, &data, honor_ranges);
                    }
                }
            });
            Self {
                url: format!("http://127.0.0.1:{port}/pkg.exe"),
                stop,
                handle: Some(handle),
            }
        }
    }

    impl Drop for RangeServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    fn serve(req: tiny_http::Request, data: &[u8], honor_ranges: bool) {
        let range = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Range"))
            .map(|h| h.value.as_str().to_string());

        match range {
            Some(r) if honor_ranges => {
                let spec = r.trim().trim_start_matches("bytes=");
                let (s, e) = spec.split_once('-').expect("测试只发合法 Range");
                let s: usize = s.parse().expect("起点应可解析");
                let e: usize = if e.is_empty() {
                    data.len() - 1
                } else {
                    e.parse().expect("终点应可解析")
                };
                let resp = tiny_http::Response::from_data(data[s..=e].to_vec())
                    .with_status_code(206)
                    .with_header(
                        tiny_http::Header::from_bytes(
                            &b"Content-Range"[..],
                            format!("bytes {s}-{e}/{}", data.len()).as_bytes(),
                        )
                        .expect("头应合法"),
                    );
                let _ = req.respond(resp);
            }
            _ => {
                let _ = req.respond(tiny_http::Response::from_data(data.to_vec()));
            }
        }
    }

    /// 有规律的载荷：任何 offset 错位都会改变内容，从而被 sha256 与逐字节比对抓住。
    fn patterned(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn split_parts_covers_the_whole_range_without_gaps_or_overlap() {
        for (size, parts) in [(1000u64, 4u64), (1001, 4), (999, 4), (7, 4), (1, 4)] {
            let got = split_parts(size, parts);
            assert_eq!(got[0].0, 0, "首片必须从 0 开始：{size}/{parts}");
            assert_eq!(got.last().unwrap().1, size - 1, "末片必须到末字节");
            // 首尾相接：无空洞也无重叠
            for w in got.windows(2) {
                assert_eq!(w[1].0, w[0].1 + 1, "分片不相接：{got:?}");
            }
            let covered: u64 = got.iter().map(|(s, e)| e - s + 1).sum();
            assert_eq!(covered, size, "覆盖字节数应等于总长");
        }
        // 分片数不得超过字节数，否则会切出空区间
        assert_eq!(split_parts(3, 8).len(), 3);
        assert!(split_parts(0, 4).is_empty());
    }

    #[test]
    fn check_content_range_accepts_matching_and_rejects_everything_else() {
        assert!(check_content_range("bytes 0-99/1000", 0, 99, 1000).is_ok());
        // total 为 * 合法但少见，只校区间
        assert!(check_content_range("bytes 0-99/*", 0, 99, 1000).is_ok());

        assert!(check_content_range("bytes 0-98/1000", 0, 99, 1000).is_err(), "区间不符");
        assert!(check_content_range("bytes 0-99/999", 0, 99, 1000).is_err(), "总长不符");
        assert!(check_content_range("0-99/1000", 0, 99, 1000).is_err(), "缺 bytes 前缀");
        assert!(check_content_range("", 0, 99, 1000).is_err(), "空头");
        assert!(check_content_range("bytes abc/1000", 0, 99, 1000).is_err(), "畸形");
    }

    #[test]
    fn parallel_download_assembles_parts_in_correct_order() {
        let dir = temp_dir("par_ok");
        let dest = dir.join("pkg.exe");
        let data = patterned(300 * 1024);
        let sha = sha256_of(&data);
        let server = RangeServer::start(data.clone(), true);

        let mut seen: Vec<(u64, u64)> = Vec::new();
        let path = download_parallel(
            &build_agent(),
            &server.url,
            &sha,
            data.len() as u64,
            &dest,
            no_cancel(),
            |d, t| seen.push((d, t)),
        )
        .expect("分片下载应成功");

        assert_eq!(path, dest);
        // 逐字节比对而非只看 sha：错位时能直接看出是拼装问题
        assert_eq!(std::fs::read(&dest).unwrap(), data, "分片拼装结果与源不一致");
        assert!(!dest.with_extension("part").exists(), "临时文件应已清理");
        assert_eq!(
            seen.last().copied(),
            Some((data.len() as u64, data.len() as u64)),
            "结束必定补发一次 100%"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 坏代理：对 Range 请求返回 200 全量。不校验就会让每个分片把整包写到自己的
    /// offset 上，文件彻底损坏 —— 必须在 fetch_part 层就拒掉。
    #[test]
    fn parallel_download_rejects_server_that_ignores_range() {
        let dir = temp_dir("par_200");
        let dest = dir.join("pkg.exe");
        let data = patterned(300 * 1024);
        let sha = sha256_of(&data);
        let server = RangeServer::start(data.clone(), false);

        let err = download_parallel(
            &build_agent(),
            &server.url,
            &sha,
            data.len() as u64,
            &dest,
            no_cancel(),
            |_, _| {},
        )
        .expect_err("返回 200 全量时必须拒绝");

        assert!(matches!(err, UpdateError::Verify(_)));
        assert!(err.user_message().contains("206"), "错误应指明真因：{}", err.user_message());
        assert!(!dest.exists(), "失败不得留下目标文件");
        assert!(!dest.with_extension("part").exists(), "失败要清理临时文件");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parallel_download_rejects_hash_mismatch() {
        let dir = temp_dir("par_hash");
        let dest = dir.join("pkg.exe");
        let data = patterned(300 * 1024);
        let server = RangeServer::start(data.clone(), true);

        let err = download_parallel(
            &build_agent(),
            &server.url,
            &"a".repeat(64), // 期望值与实际不符
            data.len() as u64,
            &dest,
            no_cancel(),
            |_, _| {},
        )
        .expect_err("哈希不符必须拒绝");

        assert!(err.user_message().contains("校验值"));
        assert!(!dest.exists());
        assert!(!dest.with_extension("part").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parallel_download_cancel_cleans_up() {
        let dir = temp_dir("par_cancel");
        let dest = dir.join("pkg.exe");
        let data = patterned(300 * 1024);
        let sha = sha256_of(&data);
        let server = RangeServer::start(data.clone(), true);

        let err = download_parallel(
            &build_agent(),
            &server.url,
            &sha,
            data.len() as u64,
            &dest,
            Arc::new(AtomicBool::new(true)), // 一进循环就取消
            |_, _| {},
        )
        .expect_err("取消应返回错误");

        // 必须是 Cancelled 而非 Download：上层据此不弹错误提示
        assert!(matches!(err, UpdateError::Cancelled), "实际：{err:?}");
        assert!(!dest.with_extension("part").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn supports_ranges_distinguishes_honest_and_ignoring_servers() {
        let data = patterned(64 * 1024);
        let ok = RangeServer::start(data.clone(), true);
        assert!(
            supports_ranges(&build_agent(), &ok.url, data.len() as u64),
            "支持 Range 的服务端应被识别"
        );

        let bad = RangeServer::start(data.clone(), false);
        assert!(
            !supports_ranges(&build_agent(), &bad.url, data.len() as u64),
            "返回 200 全量的服务端必须回退单连接"
        );

        // 声称的总长与实际不符时也要回退（元数据 size 与实际资源不一致）
        assert!(
            !supports_ranges(&build_agent(), &ok.url, 999_999),
            "Content-Range 总长与期望不符时应回退"
        );
    }

    #[test]
    fn cache_path_takes_file_name_from_url() {
        let p = cache_path(
            "https://dl.windinput.com/WindInput-0.107.0-Setup.exe",
            "fallback.exe",
        );
        assert_eq!(p.file_name().unwrap(), "WindInput-0.107.0-Setup.exe");
        assert_eq!(p.parent().unwrap(), std::env::temp_dir());
    }

    #[test]
    fn cache_path_strips_query_and_fragment() {
        let p = cache_path("https://dl.windinput.com/pkg.exe?token=abc#x", "f.exe");
        assert_eq!(p.file_name().unwrap(), "pkg.exe");
    }

    /// URL 来自远端元数据，不能让它决定写盘位置。
    #[test]
    fn cache_path_neutralizes_traversal_attempts() {
        for url in [
            "https://evil.example.com/..%2f..%2fwindows%2fsystem32%2fevil.exe",
            "https://evil.example.com/../../evil.exe",
            "https://evil.example.com/",
        ] {
            let p = cache_path(url, "safe.exe");
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            assert!(!name.contains('/') && !name.contains('\\'), "文件名不得含分隔符：{name}");
            assert!(!name.starts_with('.'), "文件名不得以点开头：{name}");
            assert_eq!(p.parent().unwrap(), std::env::temp_dir());
        }
    }
}
