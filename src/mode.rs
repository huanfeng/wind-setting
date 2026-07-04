//! 运行时变体检测：Release / Dev（对齐 core 的 dev 变体命名，已弃用旧 debug 变体）。
//!
//! 检测优先级（从高到低）：
//!   1. 环境变量 `WIND_VARIANT=dev`（开发覆盖，与 core `wind-config::variant` 一致）
//!   2. 命令行参数 `--mode dev`
//!   3. 自身 exe 文件名以 `_dev` 结尾（部署为 `wind_setting_dev.exe`）
//!   4. 程序目录下存在 `wind_input_dev.exe`（与 dev 版 core 同目录部署/开发时）
//!   5. 默认 Release

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Release,
    Dev,
}

static APP_MODE: OnceLock<AppMode> = OnceLock::new();

/// 在 main 最早期调用，传入 `std::env::args().collect()`。
pub fn init(args: &[String]) {
    APP_MODE.get_or_init(|| detect(args));
}

/// 获取当前模式；init 未调用时兜底 Release。
pub fn get() -> AppMode {
    *APP_MODE.get().unwrap_or(&AppMode::Release)
}

/// 管道/数据目录后缀：Release → `""`，Dev → `"_dev"`
/// （与 core `wind-config::variant::pipe_suffix` 一致）。
pub fn pipe_suffix() -> &'static str {
    match get() {
        AppMode::Release => "",
        AppMode::Dev => "_dev",
    }
}

/// 版本号后缀：Release → `""`，Dev → `" (Dev)"`。
pub fn version_suffix() -> &'static str {
    match get() {
        AppMode::Release => "",
        AppMode::Dev => " (Dev)",
    }
}

fn detect(args: &[String]) -> AppMode {
    // 1. 环境变量 WIND_VARIANT（与 core 一致）：=dev 强制 dev，其它值强制 release。
    if let Ok(v) = std::env::var("WIND_VARIANT") {
        return if v.eq_ignore_ascii_case("dev") {
            AppMode::Dev
        } else {
            AppMode::Release
        };
    }

    // 2. 命令行参数 --mode dev
    if let Some(val) = args.iter().skip_while(|a| *a != "--mode").nth(1)
        && val.as_str() == "dev"
    {
        return AppMode::Dev;
    }

    // 3/4. 自身 exe 文件名 _dev 后缀，或同目录存在 wind_input_dev.exe。
    if let Ok(exe) = std::env::current_exe() {
        let self_is_dev = exe
            .file_stem()
            .map(|s| s.to_string_lossy().ends_with("_dev"))
            .unwrap_or(false);
        if self_is_dev {
            return AppMode::Dev;
        }
        if let Some(dir) = exe.parent()
            && dir.join("wind_input_dev.exe").exists()
        {
            return AppMode::Dev;
        }
    }

    AppMode::Release
}
