//! 运行时变体检测：Release / Dev（对齐 core 的 dev 变体命名，已弃用旧 debug 变体），
//! 以及**便携部署**探测（[`is_portable`]）。两者正交：便携包也分 dev/release。
//!
//! 检测优先级（从高到低）：
//!   1. 环境变量 `WIND_VARIANT=dev`（开发覆盖，与 core `wind-config::variant` 一致）
//!   2. 命令行参数 `--mode dev`
//!   3. 自身 exe 文件名以 `_dev` 结尾（部署为 `wind_setting_dev.exe`）
//!   4. 程序目录下存在 `wind_input_dev.exe`（与 dev 版 core 同目录部署/开发时）
//!   5. 默认 Release

use std::path::{Path, PathBuf};
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

/// 应用数据目录名：Release `WindInput`、Dev `WindInputDev`。
///
/// 与 core `variant::app_dir_name()` 同值 —— 两边靠它拼出同一个 `%APPDATA%` 子目录。
/// **便携部署下这个名字不参与路径**（见 [`portable_userdata_dir`]）。
pub fn app_dir_name_of(mode: AppMode) -> &'static str {
    match mode {
        AppMode::Release => "WindInput",
        AppMode::Dev => "WindInputDev",
    }
}

/// 便携模式标记文件名。
///
/// 与 core `wind-config::variant::PORTABLE_MARKER_NAME`、安装器清单 `config/app.toml`
/// 的 `[app] portable_marker` **三处同名**，改名须同步。
pub const PORTABLE_MARKER_NAME: &str = "portable_mode";

/// 旧标记文件名，**仅用于读取兼容**：已发布的存量便携包里是这个名字。
/// 不认它，那些用户的配置会一夜之间落回 `%APPDATA%`（现象是「设置全恢复默认了」）。
/// 新写入一律用 [`PORTABLE_MARKER_NAME`]（本程序不写标记，写入方是安装器与便携启动器）。
pub const LEGACY_PORTABLE_MARKER_NAME: &str = "wind_portable_mode";

/// 目录内是否存在便携标记（新名优先，旧名兼容）。抽出以便单测。
pub fn has_portable_marker_in(dir: &Path) -> bool {
    dir.join(PORTABLE_MARKER_NAME).is_file() || dir.join(LEGACY_PORTABLE_MARKER_NAME).is_file()
}

/// 当前进程是否跑在便携部署里。
///
/// 判定**只看自身 exe 同目录有无标记文件**，与 core `variant::is_portable()` 逐字一致 ——
/// 两边一旦分歧，设置程序与输入法就会各读各的配置文件。故此处不加 `--mode` 那样的
/// 命令行/环境变量旁路：core 没有，加了就是分歧。
///
/// ⚠️ 不要"顺手"复用 [`crate::update::install`] 里那套便携判定。那边取
/// 「标记存在 ∨ `uninstall.exe` 缺失」的**或**，是升级流程宁可误判为便携的保守取向
/// （误判只是多弹一次安装向导）；用在配置路径上后果完全不同 —— `target/debug` 下
/// 从来没有 `uninstall.exe`，`cargo run` 会被判成便携，把开发期配置写进
/// `target/debug/userdata`，而 core 仍读 `%APPDATA%`。
///
/// 用 OnceLock 缓存：进程内结果不变，只探一次盘。
pub fn is_portable() -> bool {
    static IS_PORTABLE: OnceLock<bool> = OnceLock::new();
    *IS_PORTABLE.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(has_portable_marker_in))
            .unwrap_or(false)
    })
}

/// 便携模式下的用户数据根目录：**exe 同目录的 `userdata`**。
///
/// 注意此处**不追加** [`app_dir_name_of`] —— core 的 `portable_userdata_dir()` 也不追加。
/// 便携包里 dev 与 release 的隔离靠「两个包放在不同文件夹」实现，不靠子目录名；
/// 这边一旦自作主张多拼一层 `WindInput`，设置程序就会去读 `userdata\WindInput\`，
/// 而输入法读的是 `userdata\`。
pub fn portable_userdata_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("userdata")))
}

/// 便携模式下的**本机状态**根目录：exe 同目录的 `localdata`（cache / logs / state.toml）。
///
/// 与 [`portable_userdata_dir`] **同级**，不是它的子目录。两者曾经收敛到同一个 `userdata`，
/// 于是备份用户数据要连带拖走上百 MB 的词库缓存和日志（论坛 t120）。
/// 镜像 core 的 `variant::portable_localdata_dir()`，那边有完整理由。
///
/// 同样**不追加** [`app_dir_name_of`]，理由见 [`portable_userdata_dir`]。
pub fn portable_localdata_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("localdata")))
}

/// 版本号后缀：标题栏与「关于」页共用，标示非常规部署形态。
///
/// Release → `""`、Dev → `" (Dev)"`、便携 → `" (便携版)"`，两者叠加 → `" (Dev · 便携版)"`。
/// 便携标识用中文而 Dev 留英文：Dev 只有开发者会看到，便携版是终端用户日常所见。
pub fn version_suffix() -> &'static str {
    version_suffix_of(get(), is_portable())
}

/// 纯函数版，便于单测覆盖四种组合（`is_portable` 依赖 `current_exe`，进程内不可注入）。
fn version_suffix_of(mode: AppMode, portable: bool) -> &'static str {
    match (mode, portable) {
        (AppMode::Release, false) => "",
        (AppMode::Dev, false) => " (Dev)",
        (AppMode::Release, true) => " (便携版)",
        (AppMode::Dev, true) => " (Dev · 便携版)",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 便携标识必须与 Dev 标识**并存**而非互相覆盖：便携包也有 dev 变体，
    /// 若写成 `if portable { "(便携版)" } else { dev_suffix }`，dev 便携包
    /// 就看不出自己是 dev，排查时会拿错日志目录、连错管道。
    #[test]
    fn version_suffix_combines_dev_and_portable() {
        assert_eq!(version_suffix_of(AppMode::Release, false), "");
        assert_eq!(version_suffix_of(AppMode::Dev, false), " (Dev)");
        assert_eq!(version_suffix_of(AppMode::Release, true), " (便携版)");
        assert_eq!(version_suffix_of(AppMode::Dev, true), " (Dev · 便携版)");
    }

    /// 便携标记新旧两名都必须认出。
    ///
    /// 旧名兼容不是洁癖：存量便携包里全是 `wind_portable_mode`，只认新名会让这些
    /// 用户的配置落回 `%APPDATA%`，而 core 认旧名、仍读 `userdata\` —— 两边分家。
    #[test]
    fn portable_marker_accepts_new_and_legacy_names() {
        let base = std::env::temp_dir().join(format!("wind_setting_marker_{}", std::process::id()));

        for (i, name) in [PORTABLE_MARKER_NAME, LEGACY_PORTABLE_MARKER_NAME]
            .iter()
            .enumerate()
        {
            let dir = base.join(format!("case{i}"));
            std::fs::create_dir_all(&dir).unwrap();
            assert!(!has_portable_marker_in(&dir), "无标记时不该判为便携");
            std::fs::write(dir.join(name), "portable=1\n").unwrap();
            assert!(has_portable_marker_in(&dir), "{name} 应被认出");
        }

        // 目录存在但两个名字都没有 → 非便携（安装版与 `cargo run` 的常态）。
        let plain = base.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(!has_portable_marker_in(&plain));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 同名目录不算标记：`is_file()` 而非 `exists()`。
    /// 用 `exists()` 的话，谁在便携包里建了个叫 `portable_mode` 的文件夹就会误判。
    #[test]
    fn portable_marker_directory_is_not_a_marker() {
        let dir =
            std::env::temp_dir().join(format!("wind_setting_markerdir_{}", std::process::id()));
        std::fs::create_dir_all(dir.join(PORTABLE_MARKER_NAME)).unwrap();
        assert!(!has_portable_marker_in(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 标记名与 core / 安装器清单三处同名，改一处必须同步另两处。
    #[test]
    fn portable_marker_name_matches_core_and_installer() {
        assert_eq!(PORTABLE_MARKER_NAME, "portable_mode");
        assert_eq!(LEGACY_PORTABLE_MARKER_NAME, "wind_portable_mode");
    }

    /// 便携数据目录是 exe 同级的 `userdata`，**不含**变体子目录。
    #[test]
    fn portable_userdata_dir_has_no_variant_subdir() {
        let dir = portable_userdata_dir().expect("测试进程必能定位自身 exe");
        assert!(dir.ends_with("userdata"), "{}", dir.display());
        assert!(
            !dir.to_string_lossy().contains("WindInput"),
            "便携路径不该出现变体目录名：{}",
            dir.display()
        );
    }

    #[test]
    fn app_dir_name_matches_core() {
        assert_eq!(app_dir_name_of(AppMode::Release), "WindInput");
        assert_eq!(app_dir_name_of(AppMode::Dev), "WindInputDev");
    }
}
