//! 日志初始化：写入文件，序号轮转最多保留 5 份。
//!
//! 文件命名：wind_setting.1.log（最新）… wind_setting.5.log（最旧）。
//! 每次启动将旧文件后移一位，超出 KEEP 的删除，1 号由 appender 新建。
//!
//! 日志目录优先级：
//!   <本机状态目录>\WindInput\logs  →  <exe 同级>\logs  →  ./logs
//!
//! 本机状态目录即 `dirs::data_local_dir()`：Windows `%LOCALAPPDATA%`、
//! macOS `~/Library/Application Support`，与 core 的 `Config::local_dir()` 同源。
//!
//! 注：应用以 windows_subsystem="windows" 运行，无控制台，仅写文件。
//! 开发时可用 `RUST_LOG=debug cargo run` 覆盖级别。

use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;

const PREFIX: &str = "wind_setting";
const KEEP: usize = 5;

pub fn init() {
    let log_dir = resolve_log_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    rotate(&log_dir);

    let file_appender = tracing_appender::rolling::never(&log_dir, format!("{PREFIX}.1.log"));
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));

    tracing_subscriber::fmt()
        .with_writer(file_writer)
        .with_target(true)
        .with_thread_ids(false)
        .with_ansi(false)
        .with_env_filter(filter)
        .init();

    // guard 必须存活至进程退出，否则 non_blocking 缓冲无法刷盘
    Box::leak(Box::new(guard));

    tracing::info!(
        "wind_setting v{} 启动，日志目录: {}",
        env!("CARGO_PKG_VERSION"),
        log_dir.display()
    );
}

fn resolve_log_dir() -> PathBuf {
    let app_dir = match crate::mode::get() {
        crate::mode::AppMode::Release => "WindInput",
        crate::mode::AppMode::Dev => "WindInputDev",
    };
    // 走 `dirs` 而非直接读 `LOCALAPPDATA`：后者在 Windows 之外恒为空，会一路回落到
    // exe 同级 —— 在 macOS 的 .app 里那就是往 bundle 内部写，签名后直接失败、日志静默丢失。
    if let Some(local) = dirs::data_local_dir() {
        return local.join(app_dir).join("logs");
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("logs")))
        .unwrap_or_else(|| PathBuf::from("logs"))
}

/// 启动时将旧日志后移一位，删除超出 KEEP 的最旧文件。
/// 结果：.1.log 不存在（由 appender 新建），.2~.N 是历史记录。
fn rotate(log_dir: &Path) {
    let _ = std::fs::remove_file(log_dir.join(format!("{PREFIX}.{KEEP}.log")));
    for i in (1..KEEP).rev() {
        let from = log_dir.join(format!("{PREFIX}.{i}.log"));
        let to = log_dir.join(format!("{PREFIX}.{}.log", i + 1));
        let _ = std::fs::rename(from, to);
    }
}
