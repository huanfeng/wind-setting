//! 日志初始化：写入文件，序号轮转最多保留 5 份。
//!
//! 文件命名：wind_setting.1.log（最新）… wind_setting.5.log（最旧）。
//! 每次启动将旧文件后移一位，超出 KEEP 的删除，1 号由 appender 新建。
//!
//! 日志目录优先级：
//!   <本机状态目录>\logs  →  <exe 同级>\logs  →  ./logs
//!
//! 本机状态目录由 [`crate::user_dir::local_dir_for_mode`] 决定，与 core 的
//! `Config::local_dir()` 同源：便携部署落 `<exe 目录>\userdata\logs`，
//! 否则 `%LOCALAPPDATA%\WindInput[Dev]\logs`（macOS `~/Library/Application Support/…`）。
//!
//! 注：应用以 windows_subsystem="windows" 运行，无控制台，仅写文件。
//! 开发时可用 `RUST_LOG=debug cargo run` 覆盖级别。

use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;

const PREFIX: &str = "wind_setting";
const KEEP: usize = 5;

/// 初始化日志。
///
/// ⚠️ 调用点必须留在单实例闸门**之后**（`main` 里的 `claim_instance`）：轮转会动首实例
/// 正在写的文件，只有确定自己会活下去的进程才有资格做，理由见 [`rotate`]。
pub fn init() {
    let log_dir = resolve_log_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    rotate(&log_dir);

    let file_name = format!("{PREFIX}.1.log");
    let file_appender = tracing_appender::rolling::never(&log_dir, &file_name);
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
        "wind_setting v{} 启动，日志: {}",
        env!("CARGO_PKG_VERSION"),
        log_dir.join(&file_name).display()
    );
}

fn resolve_log_dir() -> PathBuf {
    // 目录选择整个交给 `user_dir`：便携部署要把日志一并留在 `<exe>\userdata\logs`，
    // 否则"拔盘走人不留痕"落空 —— 配置跟着便携包走了，日志还散在宿主机的
    // `%LOCALAPPDATA%` 里。内部走 `dirs` 而非直接读 `LOCALAPPDATA`：后者在
    // Windows 之外恒为空，会一路回落到 exe 同级 —— 在 macOS 的 .app 里那就是往
    // bundle 内部写，签名后直接失败、日志静默丢失。
    if let Some(local) = crate::user_dir::local_dir_for_mode(crate::mode::get()) {
        return local.join("logs");
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("logs")))
        .unwrap_or_else(|| PathBuf::from("logs"))
}

/// 启动时将旧日志后移一位，删除超出 KEEP 的最旧文件。
/// 结果：.1.log 不存在（由 appender 新建），.2~.N 是历史记录。
///
/// ⚠️ **只有首实例可以调用**，这由 `main` 里「先过单实例闸门再 `logger::init`」的顺序
/// 保证 —— 别把 `init` 挪到闸门之前。二次实例(深链再次启动)若也 rotate，会把首实例正在
/// 写的 .1.log 改名 —— 首实例的 appender 攥着旧 fd 继续写，于是日志内容与文件名对不上，
/// 连着 5 次深链就能把首实例那份轮出去删掉。排查深链问题时这会精准地把人引向错误结论
/// （「首实例没收到 argv」其实是「首实例的日志被挪走了」）。
fn rotate(log_dir: &Path) {
    let _ = std::fs::remove_file(log_dir.join(format!("{PREFIX}.{KEEP}.log")));
    for i in (1..KEEP).rev() {
        let from = log_dir.join(format!("{PREFIX}.{i}.log"));
        let to = log_dir.join(format!("{PREFIX}.{}.log", i + 1));
        let _ = std::fs::rename(from, to);
    }
}
