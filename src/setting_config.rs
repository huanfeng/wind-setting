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
}

/// 设置窗口自身的界面状态。纯呈现偏好，与输入法功能无关，故留在本文件而非 core config
/// （也就不必动 manifest / capabilities 快照）。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct UiStateConfig {
    /// 暗色模式。**`None` = 跟随系统**，用户显式切过之后才落成 `Some`。
    ///
    /// 不能用 `bool`：默认 `false` 会让系统处于深色主题的用户每次启动都被强制翻成浅色，
    /// 而「跟随系统」与「我就要浅色」在配置里必须是两件事。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dark: Option<bool>,
    /// 方案页「隐藏未启用」。
    #[serde(default)]
    pub hide_disabled_schemas: bool,
    /// 方案页「显示特殊方案」。
    #[serde(default)]
    pub show_hidden_schemas: bool,
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
        }
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

/// 本文件落在**用户配置目录**下，与 core 的 `config.toml` 同一个目录：
/// Windows `%APPDATA%\WindInput[Dev]`、macOS `~/Library/Application Support/WindInput[Dev]`。
///
/// 必须走 `dirs`，不能直接读 `APPDATA` 环境变量 —— 后者在 Windows 之外恒为空，
/// 结果是整个函数返回 `None`，设置项每次启动都回到默认值（连"已同意联网"都记不住）。
/// core 的 `Config::user_config_dir()` 用的就是 `dirs::config_dir()`，两边必须同源。
pub fn config_path_for_mode(mode: crate::mode::AppMode) -> Option<PathBuf> {
    config_path_from_base(mode, dirs::config_dir())
}

fn config_path_from_base(mode: crate::mode::AppMode, base: Option<PathBuf>) -> Option<PathBuf> {
    let dir = match mode {
        crate::mode::AppMode::Release => "WindInput",
        crate::mode::AppMode::Dev => "WindInputDev",
    };
    base.map(|base| base.join(dir).join("wind_setting.toml"))
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

    /// 期望值用 `join` 拼而非字面量：路径分隔符随平台变，写死 `\` 会让这条测试
    /// 在 macOS/Linux 上无关地红掉，掩盖它真正要守的东西（目录名与文件名）。
    #[test]
    fn config_path_uses_release_user_dir() {
        let base = std::path::PathBuf::from("user-config-root");
        let path = config_path_from_base(AppMode::Release, Some(base.clone()));
        assert_eq!(
            path.unwrap(),
            base.join("WindInput").join("wind_setting.toml")
        );
    }

    #[test]
    fn config_path_uses_dev_user_dir() {
        let base = std::path::PathBuf::from("user-config-root");
        let path = config_path_from_base(AppMode::Dev, Some(base.clone()));
        assert_eq!(
            path.unwrap(),
            base.join("WindInputDev").join("wind_setting.toml")
        );
    }

    #[test]
    fn missing_base_returns_none_instead_of_exe_fallback() {
        assert!(config_path_from_base(AppMode::Release, None).is_none());
        assert!(config_path_from_base(AppMode::Dev, None).is_none());
    }

    /// 配置目录必须由 `dirs` 解析，不能直接读 `APPDATA`。
    ///
    /// 曾经的写法是 `env::var_os("APPDATA")`，在 macOS/Linux 上恒为 `None`，
    /// 于是 `save()` 每次都失败、`load()` 每次都回默认值 —— 表现为"设置里的
    /// 选项不记忆"。这条测的就是非 Windows 平台上路径**能被解析出来**。
    #[test]
    fn config_path_resolves_on_current_platform() {
        let path = config_path_for_mode(AppMode::Release)
            .expect("当前平台必须能定位用户配置目录（旧实现只认 APPDATA，非 Windows 恒为 None）");
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
        assert_eq!(config_path_for_mode(AppMode::Dev).unwrap(), expected);
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
        };
        save_to_path(&config, &path).unwrap();

        let saved = fs::read_to_string(&path).unwrap();
        let loaded: SettingConfig = toml::from_str(&saved).unwrap();
        assert_eq!(loaded, config);

        let _ = fs::remove_dir_all(&dir);
    }
}
