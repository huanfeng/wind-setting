//! wind_setting 自身配置：只存设置程序运行偏好，不写入 core config。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingConfig {
    #[serde(default)]
    pub theme_editor: ThemeEditorConfig,
    #[serde(default)]
    pub update: UpdateConfig,
    #[serde(default)]
    pub ui: UiStateConfig,
    #[serde(default)]
    pub protocol: ProtocolConfig,
    #[serde(default)]
    pub file_assoc: FileAssocConfig,
    #[serde(default)]
    pub log: LogConfig,
}

/// 设置程序**自身**的日志。
///
/// 落在本文件而非 core config：这是设置程序的运行偏好，与输入法功能无关；更要紧的是
/// [`crate::logger::init`] 跑在 main 最早期，那时 RPC 还没连上，只能读得到本地文件。
/// 同理，TSF 的日志配置落在它自己读得到的 `tsf_log_config`（见 [`crate::tsf_log`]）——
/// 三个模块各自的日志开关，各放在各自**启动那一刻就能打开**的地方。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogConfig {
    /// 级别：`off` / `error` / `warn` / `info` / `debug` / `trace`。默认 `info`。
    ///
    /// `off` = 一个文件都不建（不是"建了但空"）。此前这里是硬编码的 `debug` 且完全
    /// 不可配——设置程序每次启动都写一份 debug 级日志，用户既关不掉也调不动。
    ///
    /// ⚠️ `RUST_LOG` 仍然优先于本项，与 core 服务同一条优先级链。
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

/// `windinput://` 协议关联的偏好。
///
/// 与「注册表当前指向谁」是两回事：注册表是**事实**（每次都现读，见
/// [`crate::protocol::current_registration`]），这里只记**用户的意愿**。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtocolConfig {
    /// 允许启动时自愈协议注册。
    ///
    /// 用户在高级页点「取消注册」时置 `false` —— 否则下次启动 `self_heal` 又把注册
    /// 写回去，「取消注册」变成一个点了没用的按钮。手动「注册」时置回 `true`。
    /// 便携版不看这个字段（恒不自愈），但仍如实记录，便于同一份配置在两种形态间迁移。
    #[serde(default = "default_true")]
    pub self_heal: bool,
}

/// `.wpkg` 文件关联的偏好。语义同 [`ProtocolConfig`]：注册表是事实（每次现读，见
/// [`crate::file_assoc::current_registration`]），这里只记用户意愿。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileAssocConfig {
    /// 允许启动时自愈 `.wpkg` 关联。「取消注册」置 `false`，手动「注册」置回 `true`，
    /// 便携版不看这个字段（恒不自愈）。
    #[serde(default = "default_true")]
    pub self_heal: bool,
}

/// 设置窗口自身的界面状态。纯呈现偏好，与输入法功能无关，故留在本文件而非 core config
/// （也就不必动 manifest / capabilities 快照）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiStateConfig {
    /// 暗色模式。**`None` = 跟随系统**，用户显式切过之后才落成 `Some`。
    ///
    /// 不能用 `bool`：默认 `false` 会让系统处于深色主题的用户每次启动都被强制翻成浅色，
    /// 而「跟随系统」与「我就要浅色」在配置里必须是两件事。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dark: Option<bool>,
    /// 主题色来源，见 [`crate::accent::AccentMode`]。`None` = 默认（不落盘）；`"ime"` =
    /// 跟随输入法主题。键名给将来的自定义色（`"#RRGGBB"`）留着。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    /// 方案页「隐藏未启用」。
    #[serde(default)]
    pub hide_disabled_schemas: bool,
    /// 方案页「显示特殊方案」。
    #[serde(default)]
    pub show_hidden_schemas: bool,
    /// 设置向导「下次不再弹出」/ 已完成。缺省 false ⇒ 所有用户（含升级上来的）弹一次。
    #[serde(default)]
    pub wizard_dismissed: bool,
    /// 「显示定制版隐藏的设置」：打开后忽略 `data_custom/custom.toml` 的 `[settings] hide`。
    /// 非定制版或定制版没隐藏任何东西时，这个开关根本不出现。见 [`crate::custom_hide`]。
    #[serde(default)]
    pub show_custom_hidden_settings: bool,
    /// 界面细节试验选项，见 [`crate::look`]。全缺省时整节不落盘。
    #[serde(default, skip_serializing_if = "crate::look::LookConfig::is_default")]
    pub look: crate::look::LookConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThemeEditorConfig {
    #[serde(default)]
    pub auto_start: bool,
}

/// 在线升级偏好。刻意放在设置程序自身配置里，不进 core config ——
/// 这是设置程序的行为偏好，与输入法功能无关，也就不必动 manifest / capabilities 快照。
///
/// 不提供「升级通道」项：通道由二进制变体决定（见 `update::Channel`），用户切不了。
/// 也不提供「自动安装」持久开关：以管理员权限替换系统输入法组件的动作，
/// 不应变成用户可能忘记自己开过的长期设置（旧 Go 版有该字段但从未被读取）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateConfig {
    /// 用户已同意联网。未同意前不发起任何网络请求。
    #[serde(default)]
    pub network_consent: bool,
    /// 启动时自动检查（仍受 `network_consent` 门控）。
    #[serde(default = "default_true")]
    pub auto_check: bool,
    /// 用户点「跳过此版本」记下的版本号；该版本不再提示。
    #[serde(default)]
    pub skipped_version: String,
    /// 上次检查的 Unix 秒，用于节流。
    #[serde(default)]
    pub last_check_at: i64,
}

fn default_true() -> bool {
    true
}

impl Default for SettingConfig {
    fn default() -> Self {
        Self {
            theme_editor: ThemeEditorConfig::default(),
            update: UpdateConfig::default(),
            ui: UiStateConfig::default(),
            protocol: ProtocolConfig::default(),
            file_assoc: FileAssocConfig::default(),
            log: LogConfig::default(),
        }
    }
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self { self_heal: true }
    }
}

impl Default for FileAssocConfig {
    fn default() -> Self {
        Self { self_heal: true }
    }
}

impl Default for ThemeEditorConfig {
    fn default() -> Self {
        Self { auto_start: false }
    }
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            network_consent: false,
            auto_check: true,
            skipped_version: String::new(),
            last_check_at: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadResult {
    pub config: SettingConfig,
    pub path: Option<PathBuf>,
    pub warning: Option<String>,
}

/// 配置文件名。目录由 [`crate::user_dir`] 决定 —— 三种部署形态各落一处，见那里的文档。
const CONFIG_FILE_NAME: &str = "wind_setting.toml";

/// 本文件与 core 的 `config.toml` **同目录**，目录选择完全交给 [`crate::user_dir`]：
/// 便携 → `<exe 目录>\userdata\`；自定义数据目录 → `datadir.conf` 里那个路径；
/// 否则 → `%APPDATA%\WindInput[Dev]`（macOS `~/Library/Application Support/WindInput[Dev]`）。
///
/// 这里不再自己拼 `dirs::config_dir()`：那样只覆盖第三种形态，便携包与自定义数据目录
/// 的用户会出现「输入法读 A、设置程序写 B」，表现为设置项存了但下次启动读不回来。
pub fn config_path_for_mode(mode: crate::mode::AppMode) -> Option<PathBuf> {
    crate::user_dir::user_config_dir_for_mode(mode).map(config_path_in_dir)
}

/// 目录 → 完整文件路径。抽出以便在测试里对任意目录断言文件名。
fn config_path_in_dir(dir: PathBuf) -> PathBuf {
    dir.join(CONFIG_FILE_NAME)
}

pub fn load() -> LoadResult {
    let path = config_path_for_mode(crate::mode::get());
    let Some(path) = path else {
        return LoadResult {
            config: SettingConfig::default(),
            path: None,
            warning: Some("无法定位用户配置目录，已使用默认设置；不会写入程序目录".to_string()),
        };
    };
    if !path.exists() {
        return LoadResult {
            config: SettingConfig::default(),
            path: Some(path),
            warning: None,
        };
    }
    match fs::read_to_string(&path)
        .ok()
        .and_then(|s| toml::from_str::<SettingConfig>(&s).ok())
    {
        Some(config) => LoadResult {
            config,
            path: Some(path),
            warning: None,
        },
        None => LoadResult {
            config: SettingConfig::default(),
            path: Some(path),
            warning: Some("wind_setting.toml 读取失败，已使用默认设置".to_string()),
        },
    }
}

pub fn save(config: &SettingConfig) -> Result<(), String> {
    let path = config_path_for_mode(crate::mode::get())
        .ok_or_else(|| "无法定位用户配置目录，设置未保存".to_string())?;
    save_to_path(config, &path)
}

fn save_to_path(config: &SettingConfig, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败：{e}"))?;
    }
    let text = toml::to_string_pretty(config).map_err(|e| format!("序列化配置失败：{e}"))?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).map_err(|e| format!("写入临时配置失败：{e}"))?;
    if path.exists() {
        fs::remove_file(path).map_err(|e| format!("删除旧配置文件失败：{e}"))?;
    }
    fs::rename(&tmp, path).map_err(|e| format!("替换配置文件失败：{e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::AppMode;

    #[test]
    fn wizard_dismissed_defaults_false_for_old_files() {
        let c: SettingConfig = toml::from_str("[ui]\nhide_disabled_schemas = true\n").unwrap();
        assert!(!c.ui.wizard_dismissed);
    }

    /// 期望值用 `join` 拼而非字面量：路径分隔符随平台变，写死 `\` 会让这条测试
    /// 在 macOS/Linux 上无关地红掉，掩盖它真正要守的东西（目录名与文件名）。
    #[test]
    fn config_path_uses_release_user_dir() {
        let base = std::path::PathBuf::from("user-config-root");
        let dir = crate::user_dir::default_user_dir_from_base(Some(base.clone()), AppMode::Release);
        assert_eq!(
            config_path_in_dir(dir.unwrap()),
            base.join("WindInput").join("wind_setting.toml")
        );
    }

    #[test]
    fn config_path_uses_dev_user_dir() {
        let base = std::path::PathBuf::from("user-config-root");
        let dir = crate::user_dir::default_user_dir_from_base(Some(base.clone()), AppMode::Dev);
        assert_eq!(
            config_path_in_dir(dir.unwrap()),
            base.join("WindInputDev").join("wind_setting.toml")
        );
    }

    /// 便携部署下配置与 core 的 `config.toml` 并排躺在 `<exe 目录>\userdata\`，
    /// **不带**变体子目录 —— 多拼一层 `WindInput` 就会与输入法读的位置错开。
    #[test]
    fn portable_config_path_sits_in_userdata_root() {
        let dir = crate::mode::portable_userdata_dir().expect("测试进程必能定位自身 exe");
        let path = config_path_in_dir(dir.clone());
        assert_eq!(path, dir.join("wind_setting.toml"));
        assert!(
            path.ends_with("userdata/wind_setting.toml"),
            "{}",
            path.display()
        );
    }

    #[test]
    fn missing_base_returns_none_instead_of_exe_fallback() {
        assert!(crate::user_dir::default_user_dir_from_base(None, AppMode::Release).is_none());
        assert!(crate::user_dir::default_user_dir_from_base(None, AppMode::Dev).is_none());
    }

    /// 配置目录必须由 `dirs` 解析，不能直接读 `APPDATA`。
    ///
    /// 曾经的写法是 `env::var_os("APPDATA")`，在 macOS/Linux 上恒为 `None`，
    /// 于是 `save()` 每次都失败、`load()` 每次都回默认值 —— 表现为"设置里的
    /// 选项不记忆"。这条测的就是非 Windows 平台上路径**能被解析出来**。
    ///
    /// 走默认层的纯函数而非 `config_path_for_mode`：后者会读本机真实的
    /// `datadir.conf`，开发机上若恰好装了自定义数据目录的版本，这条测试就会因为
    /// 环境而非代码变红 —— 它要守的是「`dirs` 在本平台能解析出目录」，与那无关。
    #[test]
    fn config_path_resolves_on_current_platform() {
        let dir = crate::user_dir::default_user_dir_from_base(dirs::config_dir(), AppMode::Release)
            .expect("当前平台必须能定位用户配置目录（旧实现只认 APPDATA，非 Windows 恒为 None）");
        let path = config_path_in_dir(dir);
        // Path::ends_with 按**组件**比较，两个平台上都成立。
        assert!(
            path.ends_with("WindInput/wind_setting.toml"),
            "{}",
            path.display()
        );
        assert!(
            path.is_absolute(),
            "配置路径须为绝对路径：{}",
            path.display()
        );
    }

    /// macOS 上落在 `~/Library/Application Support/WindInput[Dev]`，
    /// 与 core 的 `Config::user_config_dir()` 同一个目录 —— 两边不得各写各的。
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_config_path_matches_core_user_config_dir() {
        let home = std::env::var("HOME").expect("macOS 必有 HOME");
        let expected = std::path::PathBuf::from(home)
            .join("Library/Application Support/WindInputDev/wind_setting.toml");
        let dir = crate::user_dir::default_user_dir_from_base(dirs::config_dir(), AppMode::Dev);
        assert_eq!(config_path_in_dir(dir.unwrap()), expected);
    }

    /// 自愈默认开：绝大多数用户装完就该能点一键导入链接。
    /// 缺字段的**旧配置文件**也必须读成 `true` —— `#[serde(default)]` 用的是
    /// `Default::default()`，若这里写成 `false`，所有存量用户升级后协议就悄悄失效了。
    #[test]
    fn protocol_self_heal_defaults_on_including_legacy_files() {
        assert!(SettingConfig::default().protocol.self_heal);
        let legacy: SettingConfig = toml::from_str("[ui]\n").expect("旧文件应能读入");
        assert!(legacy.protocol.self_heal, "缺 [protocol] 段时须回到开启");
        let explicit: SettingConfig =
            toml::from_str("[protocol]\nself_heal = false\n").expect("显式关闭应保留");
        assert!(!explicit.protocol.self_heal);
    }

    /// 用户的取消意愿必须能跨重启存活，否则下次启动自愈把注册写回去。
    #[test]
    fn protocol_self_heal_survives_roundtrip() {
        let mut cfg = SettingConfig::default();
        cfg.protocol.self_heal = false;
        let s = toml::to_string(&cfg).expect("序列化");
        let back: SettingConfig = toml::from_str(&s).expect("反序列化");
        assert!(!back.protocol.self_heal);
    }

    /// `.wpkg` 关联的自愈门控与协议同规则：默认开（含缺段的旧文件）、
    /// 用户的取消意愿跨重启存活。
    #[test]
    fn file_assoc_self_heal_defaults_on_and_survives_roundtrip() {
        assert!(SettingConfig::default().file_assoc.self_heal);
        let legacy: SettingConfig = toml::from_str("[ui]\n").expect("旧文件应能读入");
        assert!(
            legacy.file_assoc.self_heal,
            "缺 [file_assoc] 段时须回到开启"
        );
        let mut cfg = SettingConfig::default();
        cfg.file_assoc.self_heal = false;
        let s = toml::to_string(&cfg).expect("序列化");
        let back: SettingConfig = toml::from_str(&s).expect("反序列化");
        assert!(!back.file_assoc.self_heal);
    }

    #[test]
    fn default_config_keeps_theme_editor_disabled() {
        let cfg = SettingConfig::default();
        assert!(!cfg.theme_editor.auto_start);
    }

    /// 明暗是**三态**：未设过（跟随系统）/ 显式浅色 / 显式深色。
    ///
    /// 写成 `bool` 的话，「没设过」与「设了浅色」在配置里长得一模一样，系统处于深色
    /// 主题的用户每次启动都会被强制翻成浅色。这条测的就是那个区分不会在序列化往返中丢失。
    #[test]
    fn dark_mode_distinguishes_unset_from_explicit_light() {
        // 未设过：不写进文件，读回仍是 None。
        let cfg = SettingConfig::default();
        let s = toml::to_string(&cfg).expect("序列化");
        assert!(
            !s.contains("dark"),
            "未设过的明暗不该落盘（否则下次启动就不再跟随系统了）：\n{s}"
        );
        let back: SettingConfig = toml::from_str(&s).expect("反序列化");
        assert_eq!(back.ui.dark, None);

        // 显式浅色：必须落盘且读回 Some(false)——与上面的 None 是两件事。
        let mut cfg2 = SettingConfig::default();
        cfg2.ui.dark = Some(false);
        let s2 = toml::to_string(&cfg2).expect("序列化");
        let back2: SettingConfig = toml::from_str(&s2).expect("反序列化");
        assert_eq!(
            back2.ui.dark,
            Some(false),
            "显式浅色不得退化成 None：\n{s2}"
        );
    }

    /// 主题色：默认不落盘，跟随输入法落 `accent = "ime"` 并能读回。
    #[test]
    fn accent_mode_unset_by_default_and_roundtrips() {
        let s = toml::to_string(&SettingConfig::default()).expect("序列化");
        assert!(!s.contains("accent"), "默认主题色不该落盘：\n{s}");

        let mut cfg = SettingConfig::default();
        cfg.ui.accent = crate::accent::AccentMode::Ime.to_config();
        let s = toml::to_string(&cfg).expect("序列化");
        let back: SettingConfig = toml::from_str(&s).expect("反序列化");
        assert_eq!(
            crate::accent::AccentMode::from_config(back.ui.accent.as_deref()),
            crate::accent::AccentMode::Ime,
            "{s}"
        );
    }

    /// 方案列表两个显示过滤的往返。出厂都关：新用户第一眼该看到全部普通方案，
    /// 且不该看到英文/快符这类特殊方案。
    #[test]
    fn schema_list_filters_default_off_and_roundtrip() {
        let cfg = SettingConfig::default();
        assert!(!cfg.ui.hide_disabled_schemas);
        assert!(!cfg.ui.show_hidden_schemas);

        let mut cfg2 = SettingConfig::default();
        cfg2.ui.hide_disabled_schemas = true;
        cfg2.ui.show_hidden_schemas = true;
        let s = toml::to_string(&cfg2).expect("序列化");
        let back: SettingConfig = toml::from_str(&s).expect("反序列化");
        assert!(back.ui.hide_disabled_schemas);
        assert!(back.ui.show_hidden_schemas);
    }

    /// 界面细节：全缺省时 `[ui.look]` 不落盘；非缺省值能读回。
    #[test]
    fn look_section_absent_when_default_and_roundtrips() {
        let s = toml::to_string(&SettingConfig::default()).expect("序列化");
        assert!(!s.contains("look"), "全缺省不该落盘：\n{s}");

        let mut cfg = SettingConfig::default();
        cfg.ui.look = crate::look::LookConfig::from_indices([0, 0, 1, 1, 0, 0]);
        let s = toml::to_string(&cfg).expect("序列化");
        let back: SettingConfig = toml::from_str(&s).expect("反序列化");
        assert_eq!(back.ui.look.indices(), [0, 0, 1, 1, 0, 0], "{s}");
    }

    /// 手改 / 未来版本写入的未知取值不能让整个文件加载失败（明暗、升级偏好会跟着丢）。
    #[test]
    fn unknown_look_value_does_not_break_loading() {
        let back: SettingConfig =
            toml::from_str("[ui.look]\nrow_divider = \"zigzag\"\n").expect("未知取值也应能读");
        assert_eq!(back.ui.look.indices()[crate::look::ROW_DIVIDER], 0);
    }

    /// 旧版本写的文件没有 `[ui]` 段，必须能读（否则升级即报配置损坏）。
    #[test]
    fn config_without_ui_section_still_loads() {
        let old = "[theme_editor]\nauto_start = true\n";
        let cfg: SettingConfig = toml::from_str(old).expect("缺 [ui] 段应能读");
        assert!(cfg.theme_editor.auto_start);
        assert_eq!(cfg.ui.dark, None);
        assert!(!cfg.ui.show_hidden_schemas);
    }

    #[test]
    fn update_defaults_require_consent_before_any_network_access() {
        let cfg = UpdateConfig::default();
        assert!(
            !cfg.network_consent,
            "首次运行必须先取得联网同意，输入法产品不得默认联网"
        );
        assert!(cfg.auto_check, "取得同意后应默认自动检查");
        assert!(cfg.skipped_version.is_empty());
        assert_eq!(cfg.last_check_at, 0);
    }

    /// 老配置文件没有 [update] 段，必须能原样加载并取到默认值。
    #[test]
    fn legacy_config_without_update_section_loads() {
        let cfg: SettingConfig = toml::from_str("[theme_editor]\nauto_start = true\n").unwrap();
        assert!(cfg.theme_editor.auto_start);
        assert_eq!(cfg.update, UpdateConfig::default());
    }

    /// [update] 段存在但字段不全时，缺失字段走字段级默认 —— auto_check 不能塌成 false。
    #[test]
    fn partial_update_section_keeps_auto_check_default() {
        let cfg: SettingConfig = toml::from_str("[update]\nnetwork_consent = true\n").unwrap();
        assert!(cfg.update.network_consent);
        assert!(
            cfg.update.auto_check,
            "缺省 auto_check 应回落到 true，而非 bool 的 Default::default()"
        );
    }

    #[test]
    fn update_config_survives_roundtrip() {
        let cfg = SettingConfig {
            theme_editor: ThemeEditorConfig { auto_start: false },
            update: UpdateConfig {
                network_consent: true,
                auto_check: false,
                skipped_version: "0.107.0-dev.3".to_string(),
                last_check_at: 1_750_000_000,
            },
            ui: UiStateConfig::default(),
            protocol: ProtocolConfig::default(),
            file_assoc: FileAssocConfig::default(),
            log: LogConfig::default(),
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: SettingConfig = toml::from_str(&text).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn save_to_path_overwrites_existing_config() {
        let dir = std::env::temp_dir().join(format!("wind_setting_test_{}", std::process::id()));
        let path = dir.join("wind_setting.toml");

        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "[theme_editor]\nauto_start = false\n").unwrap();

        let config = SettingConfig {
            theme_editor: ThemeEditorConfig { auto_start: true },
            update: UpdateConfig::default(),
            ui: UiStateConfig::default(),
            protocol: ProtocolConfig::default(),
            file_assoc: FileAssocConfig::default(),
            log: LogConfig::default(),
        };
        save_to_path(&config, &path).unwrap();

        let saved = fs::read_to_string(&path).unwrap();
        let loaded: SettingConfig = toml::from_str(&saved).unwrap();
        assert_eq!(loaded, config);

        let _ = fs::remove_dir_all(&dir);
    }
}
