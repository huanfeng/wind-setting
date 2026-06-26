//! 运行时模式检测：Release / Dev / Debug。
//!
//! 检测优先级（从高到低）：
//!   1. 命令行参数 `--mode dev` 或 `--mode debug`
//!   2. 程序目录下存在 `wind_input_dev.exe`   → Dev
//!      程序目录下存在 `wind_input_debug.exe`  → Debug
//!   3. 默认 Release

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Release,
    Dev,
    Debug,
}

static APP_MODE: OnceLock<AppMode> = OnceLock::new();

/// 在 main 最早期调用，传入 std::env::args().collect()。
pub fn init(args: &[String]) {
    APP_MODE.get_or_init(|| detect(args));
}

/// 获取当前模式；init 未调用时兜底 Release。
pub fn get() -> AppMode {
    *APP_MODE.get().unwrap_or(&AppMode::Release)
}

/// 管道后缀：Release → ""，Dev → "_dev"，Debug → "_debug"。
pub fn pipe_suffix() -> &'static str {
    match get() {
        AppMode::Release => "",
        AppMode::Dev => "_dev",
        AppMode::Debug => "_debug",
    }
}

/// 版本号后缀：Release → ""，Dev → " (dev)"，Debug → " (debug)"。
pub fn version_suffix() -> &'static str {
    match get() {
        AppMode::Release => "",
        AppMode::Dev => " (dev)",
        AppMode::Debug => " (debug)",
    }
}

fn detect(args: &[String]) -> AppMode {
    // 1. 命令行参数
    if let Some(val) = args.iter().skip_while(|a| *a != "--mode").nth(1) {
        match val.as_str() {
            "dev" => return AppMode::Dev,
            "debug" => return AppMode::Debug,
            _ => {}
        }
    }

    // 2. 程序目录文件检测
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir.join("wind_input_dev.exe").exists() {
                return AppMode::Dev;
            }
            if dir.join("wind_input_debug.exe").exists() {
                return AppMode::Debug;
            }
        }
    }

    AppMode::Release
}
