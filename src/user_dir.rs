//! 用户数据目录定位：**逐条镜像** core 的 `Config::user_config_dir()`。
//!
//! 设置程序自己的 `wind_setting.toml` 与 core 的 `config.toml` 必须落在同一个目录 ——
//! 用户认知里它们是"同一份设置"，一旦分家，便携包插上另一台机器后设置全空，
//! 而 `%APPDATA%` 里躺着一份谁也读不到的旧配置。
//!
//! 优先级（与 core `wind-config::variant` / `config.rs` 完全一致，改动须同步两仓）：
//!   1. **便携**：exe 同目录有 `portable_mode`（或旧名 `wind_portable_mode`）
//!      → `<exe 目录>\userdata\`
//!   2. **自定义数据目录**：安装向导选定、安装器写入
//!      `%LOCALAPPDATA%\WindInput[Dev]\datadir.conf` → 文件里那行路径
//!   3. **默认**：`dirs::config_dir()\WindInput[Dev]`（Windows 上即漫游 `%APPDATA%`）
//!
//! 便携**高于**配置文件这一条是刻意的：便携包自带 `userdata`，若还去读机器全局的
//! `datadir.conf`，在一台装过正式版的机器上，便携包会把数据写回安装版的目录，
//! 「便携」即名存实亡。

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::mode::AppMode;

/// 安装器写下的自定义用户数据目录配置文件名。
///
/// 真源是 `WindInput/config/app.toml` 的 `[datadir] conf_file`（安装器据此写盘），
/// core `variant::DATADIR_CONF_NAME` 与本常量都是读端副本 —— 三仓无法共享常量，
/// 改名时须同步。
const DATADIR_CONF_NAME: &str = "datadir.conf";

/// 覆盖 `datadir.conf` 的完整路径。仅供测试与开发排查，生产部署严禁设置。
/// core 认这个变量，本侧不认的话，排查时两个进程会读不同的配置。
const DATADIR_CONF_ENV: &str = "WIND_DATADIR_CONF";

/// 当前进程该用的用户数据目录。`None` = 平台上定位不出任何可用目录（调用方须放弃写盘，
/// 绝不能回落到程序目录 —— 那在 Program Files 下会失败，在 macOS 的 .app 里会破坏签名）。
///
/// 变体 `mode` 只影响第 2、3 层的目录名，便携层与变体无关（见模块文档）。
/// 显式传参而非内部调 `mode::get()`：调用方 `setting_config` 已持有变体，
/// 且测试需要对两个变体分别断言。
pub fn user_config_dir_for_mode(mode: AppMode) -> Option<PathBuf> {
    // 便携时直接返回，**不因 `None` 回落到 `%APPDATA%`**：core 也不回落
    // （`current_exe()` 失败时两边一起返回 `None`），回落会让两边分家。
    if crate::mode::is_portable() {
        return crate::mode::portable_userdata_dir();
    }
    if let Some(d) = custom_userdata_dir(mode) {
        return Some(d);
    }
    default_user_dir_from_base(dirs::config_dir(), mode)
}

/// 第 3 层：默认位置。纯拼接，无副作用。
pub fn default_user_dir_from_base(base: Option<PathBuf>, mode: AppMode) -> Option<PathBuf> {
    base.map(|d| d.join(crate::mode::app_dir_name_of(mode)))
}

/// 本机状态目录（日志、缓存等机器相关数据），镜像 core 的 `Config::local_dir()`：
/// - 便携：`<exe 目录>\localdata\` —— 便携包本就该自包含，日志留在 `%LOCALAPPDATA%`
///   会让"拔盘走人不留痕"落空；但它与用户数据的 `userdata\` **分开**（同级，论坛 t120），
///   否则备份用户数据要连带拖走上百 MB 的缓存与日志。
/// - 否则：`%LOCALAPPDATA%\WindInput[Dev]`（本机数据，不随漫游同步到别的机器）
///
/// ⚠️ **刻意不接 `datadir.conf`**，这与 [`user_config_dir_for_mode`] 的不对称是有意的：
/// core 的 `local_dir()` 压根不看 `custom_userdata_dir()`，安装器卸载侧的
/// `local_cache_dir()` 也恒为 `%LOCALAPPDATA%\{id}\cache`。三边口径必须一致 ——
/// 谁"顺手"把自定义数据目录也接到这里，卸载时就会漏删（或删错）目录。
pub fn local_dir_for_mode(mode: AppMode) -> Option<PathBuf> {
    if crate::mode::is_portable() {
        return crate::mode::portable_localdata_dir();
    }
    default_local_dir_from_base(dirs::data_local_dir(), mode)
}

/// 本机状态目录的默认位置。纯拼接，无副作用。
///
/// 与 [`default_user_dir_from_base`] 眼下拼法相同但**不合并**：两者的 `base` 来自
/// 不同的 `dirs` 函数（漫游 vs 本机），语义各自独立；core 那边也是两条各自演进的路径，
/// 合成一个函数会让将来任一侧分化时改错另一侧。
pub fn default_local_dir_from_base(base: Option<PathBuf>, mode: AppMode) -> Option<PathBuf> {
    base.map(|d| d.join(crate::mode::app_dir_name_of(mode)))
}

/// `datadir.conf` 的所在路径：`%LOCALAPPDATA%\WindInput[Dev]\datadir.conf`。
///
/// 注意是**本机**目录（`data_local_dir`）而非漫游目录 —— 数据目录是这台机器的选择，
/// 不该跟着漫游配置同步到别的机器上去。dev 与正式版各读各的，故带变体目录名。
fn datadir_conf_path(mode: AppMode) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(DATADIR_CONF_ENV) {
        return Some(PathBuf::from(p));
    }
    dirs::data_local_dir().map(|d| {
        d.join(crate::mode::app_dir_name_of(mode))
            .join(DATADIR_CONF_NAME)
    })
}

/// 解析 `datadir.conf` 的内容（纯函数，不碰盘，便于单测）。
///
/// 文件内容就是一行数据目录绝对路径明文（安装器 `write_datadir_conf` 直接写
/// `to_string_lossy()`，无引号无键名）。校验不通过一律返回 `None` → 调用方回落默认位置：
/// 这个路径会被当成用户全部配置的家，宁可退回默认，也不能拿一个可疑路径去建目录。
fn parse_datadir_conf(content: &str) -> Option<PathBuf> {
    // 安装器用 UTF-8 无 BOM 写入，但用户可能用记事本改过（记事本另存为 UTF-8 会加 BOM）。
    let s = content.trim_start_matches('\u{feff}').trim();
    if s.is_empty() {
        return None;
    }
    let p = PathBuf::from(s);
    // 必须是绝对路径。这一条同时挡住 Windows 的**驱动器相对路径** `X:name` ——
    // 它看着像绝对路径，`is_absolute()` 却为 false，解析时会落到该盘的当前目录上。
    if !p.is_absolute() {
        return None;
    }
    // `..` 在这里没有正当用途，出现即视为可疑。
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    Some(p)
}

/// 纯决策：要不要用自定义目录、用哪个。副作用（读盘、建目录）留在调用方。
/// `conf` 为 `None` 表示配置文件不存在或读不出来。
fn decide_custom_dir(portable: bool, conf: Option<&str>) -> Option<PathBuf> {
    if portable {
        return None;
    }
    parse_datadir_conf(conf?)
}

/// 第 2 层：用户自定义的数据目录。
///
/// 返回 `None` 表示「用默认位置」：便携模式、无配置文件、内容非法、目录建不出来，
/// 四种情况都归到这一个出口 —— 与 core 的四个出口一一对应。用 OnceLock 缓存，
/// 进程内只读一次盘（`mode` 在进程内恒定，缓存不会串味）。
fn custom_userdata_dir(mode: AppMode) -> Option<PathBuf> {
    static CUSTOM_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    CUSTOM_DIR
        .get_or_init(|| {
            let conf = datadir_conf_path(mode)?;
            // 便携判定先于读盘：读不到文件与「便携故不该读」是两件事，日志要分得清。
            let content = if crate::mode::is_portable() {
                None
            } else {
                std::fs::read_to_string(&conf).ok()
            };
            let Some(dir) = decide_custom_dir(crate::mode::is_portable(), content.as_deref())
            else {
                if content.is_some() {
                    tracing::warn!(
                        "datadir.conf 内容非法，回落默认用户配置目录: {}",
                        conf.display()
                    );
                }
                return None;
            };
            // 安装器只写配置、不建目录，故首次启动时它多半还不存在。建不出来
            // （盘符不存在、无权限、被占用）就回落默认 —— 这一步必须与 core 同进退：
            // core 建不出来也回落，若本侧不建目录就直接采用，两边会落在不同位置。
            if let Err(e) = std::fs::create_dir_all(&dir) {
                tracing::warn!(
                    "自定义用户数据目录不可用（{}），回落默认: {}",
                    e,
                    dir.display()
                );
                return None;
            }
            tracing::info!("使用自定义用户数据目录: {}", dir.display());
            Some(dir)
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用的绝对路径样例，按**编译目标**取 —— `Path::is_absolute()` 是平台语义的，
    /// `D:\MyData` 只在 Windows 目标下算绝对路径。写死 Windows 路径会让「接受合法路径」
    /// 的断言在非 Windows 上全挂，而「拒绝非法路径」的断言变成不走被测分支的假绿。
    #[cfg(windows)]
    const ABS_ROOT: &str = r"D:\MyData";
    #[cfg(not(windows))]
    const ABS_ROOT: &str = "/mnt/MyData";

    fn abs(tail: &str) -> String {
        if cfg!(windows) {
            format!("{ABS_ROOT}\\{tail}")
        } else {
            format!("{ABS_ROOT}/{tail}")
        }
    }

    /// 安装器写的就是一行裸路径，尾随换行由 trim 吃掉。
    #[test]
    fn datadir_conf_plain_path_accepted() {
        let p = abs("WindInput");
        assert_eq!(parse_datadir_conf(&p), Some(PathBuf::from(&p)));
        assert_eq!(
            parse_datadir_conf(&format!("{p}\r\n")),
            Some(PathBuf::from(&p))
        );
    }

    /// 记事本另存为 UTF-8 会加 BOM，不能因此把整份用户配置判废。
    #[test]
    fn datadir_conf_bom_stripped() {
        assert_eq!(
            parse_datadir_conf(&format!("\u{feff}{ABS_ROOT}")),
            Some(PathBuf::from(ABS_ROOT))
        );
    }

    #[test]
    fn datadir_conf_empty_rejected() {
        assert_eq!(parse_datadir_conf(""), None);
        assert_eq!(parse_datadir_conf("   \r\n"), None);
    }

    /// 驱动器相对路径 `X:name` 看着像绝对路径，实则解析到该盘当前目录。
    /// `Path::is_absolute` 对 `C:foo` 返回 false，正是靠它挡住。
    #[cfg(windows)]
    #[test]
    fn datadir_conf_drive_relative_rejected() {
        assert_eq!(parse_datadir_conf("C:data"), None);
    }

    #[test]
    fn datadir_conf_relative_rejected() {
        assert_eq!(parse_datadir_conf(r"data\WindInput"), None);
        assert_eq!(parse_datadir_conf("data/WindInput"), None);
    }

    /// `..` 必须挡在 `ParentDir` 那一关，而不是被 `is_absolute` 顺带挡掉 ——
    /// 故样例必须是**本平台的合法绝对路径**，否则测试绿了也没走到被测分支。
    #[test]
    fn datadir_conf_parent_traversal_rejected() {
        let p = abs("..");
        assert!(
            PathBuf::from(&p).is_absolute(),
            "样例须是本平台绝对路径，否则测不到 ParentDir 分支"
        );
        assert_eq!(parse_datadir_conf(&p), None);
    }

    /// 便携模式自带 userdata，绝不能再去读机器全局的 datadir.conf ——
    /// 否则一台装过正式版的机器上，便携包会把配置写回安装版的目录。
    ///
    /// 断言的是纯决策函数而非 `custom_userdata_dir()`：后者的便携判定来自
    /// `current_exe()`，测试进程里恒为非便携，写成条件断言就成了永不执行的假测试。
    #[test]
    fn portable_beats_datadir_conf() {
        let conf = Some(ABS_ROOT);
        assert_eq!(decide_custom_dir(true, conf), None, "便携必须忽略配置文件");
        assert_eq!(
            decide_custom_dir(false, conf),
            Some(PathBuf::from(ABS_ROOT)),
            "非便携才认配置文件"
        );
    }

    /// 无配置文件 = 用默认位置，这是绝大多数用户的路径。
    #[test]
    fn absent_conf_falls_back_to_default() {
        assert_eq!(decide_custom_dir(false, None), None);
    }

    /// 默认层按变体分目录。期望值用 `join` 拼而非字面量：路径分隔符随平台变。
    #[test]
    fn default_dir_uses_variant_subdir() {
        let base = PathBuf::from("user-config-root");
        assert_eq!(
            default_user_dir_from_base(Some(base.clone()), AppMode::Release).unwrap(),
            base.join("WindInput")
        );
        assert_eq!(
            default_user_dir_from_base(Some(base.clone()), AppMode::Dev).unwrap(),
            base.join("WindInputDev")
        );
    }

    /// 定位不出基准目录时返回 `None`，绝不回落到程序目录。
    #[test]
    fn missing_base_returns_none() {
        assert!(default_user_dir_from_base(None, AppMode::Release).is_none());
        assert!(default_user_dir_from_base(None, AppMode::Dev).is_none());
        assert!(default_local_dir_from_base(None, AppMode::Release).is_none());
    }

    /// 本机状态目录的默认层同样按变体分目录。
    #[test]
    fn default_local_dir_uses_variant_subdir() {
        let base = PathBuf::from("local-root");
        assert_eq!(
            default_local_dir_from_base(Some(base.clone()), AppMode::Release).unwrap(),
            base.join("WindInput")
        );
        assert_eq!(
            default_local_dir_from_base(Some(base.clone()), AppMode::Dev).unwrap(),
            base.join("WindInputDev")
        );
    }

    /// 本机状态目录（日志/缓存）与漫游配置目录取自**不同**的 `dirs` 基准，
    /// 非便携下必须分居两处 —— 合成一处会让日志跟着漫游同步到别的机器。
    #[test]
    fn local_and_roaming_defaults_are_distinct_on_windows() {
        let local = default_local_dir_from_base(dirs::data_local_dir(), AppMode::Release);
        let roaming = default_user_dir_from_base(dirs::config_dir(), AppMode::Release);
        if cfg!(windows) {
            assert_ne!(
                local, roaming,
                "Windows 上 %LOCALAPPDATA% 与 %APPDATA% 必须是两个目录"
            );
        }
    }

    /// 便携下两者**分开**：用户数据在 `userdata`，本机状态在 `localdata`（论坛 t120）。
    ///
    /// 这条曾经断言的正相反——两者收敛到同一个 `userdata`。代价是备份用户数据要连带拖走
    /// 上百 MB 的词库缓存和日志，故 core 的 `local_dir()` 便携分支已改走
    /// `portable_localdata_dir()`，这边必须跟上：三边口径不一致就是「设置页打开的日志
    /// 目录是空的」。
    #[test]
    fn portable_splits_local_from_roaming() {
        let user = crate::mode::portable_userdata_dir().expect("测试进程必能定位自身 exe");
        let local = crate::mode::portable_localdata_dir().expect("测试进程必能定位自身 exe");
        assert!(user.ends_with("userdata"), "{}", user.display());
        assert!(local.ends_with("localdata"), "{}", local.display());
        assert_ne!(user, local, "两者不能再收敛到同一个目录");
        assert_eq!(
            user.parent(),
            local.parent(),
            "须同级：localdata 不是 userdata 的子目录，深度不变"
        );
        assert!(local.join("logs").ends_with("localdata/logs"));
    }
}
