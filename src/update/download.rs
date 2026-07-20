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

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    progress: impl FnMut(u64, u64),
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

    let resp = ureq::AgentBuilder::new()
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout_read(READ_TIMEOUT)
        .redirects(3)
        .build()
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
