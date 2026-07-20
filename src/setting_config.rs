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

pub fn config_path_for_mode(mode: crate::mode::AppMode) -> Option<PathBuf> {
    config_path_from_appdata(mode, std::env::var_os("APPDATA").map(PathBuf::from))
}

fn config_path_from_appdata(
    mode: crate::mode::AppMode,
    appdata: Option<PathBuf>,
) -> Option<PathBuf> {
    let dir = match mode {
        crate::mode::AppMode::Release => "WindInput",
        crate::mode::AppMode::Dev => "WindInputDev",
    };
    appdata.map(|base| base.join(dir).join("wind_setting.toml"))
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
    fn config_path_uses_release_user_dir() {
        let base = std::path::PathBuf::from(r"C:\Users\tester\AppData\Roaming");
        let path = config_path_from_appdata(AppMode::Release, Some(base));
        assert_eq!(
            path.unwrap(),
            std::path::PathBuf::from(
                r"C:\Users\tester\AppData\Roaming\WindInput\wind_setting.toml"
            )
        );
    }

    #[test]
    fn config_path_uses_dev_user_dir() {
        let base = std::path::PathBuf::from(r"C:\Users\tester\AppData\Roaming");
        let path = config_path_from_appdata(AppMode::Dev, Some(base));
        assert_eq!(
            path.unwrap(),
            std::path::PathBuf::from(
                r"C:\Users\tester\AppData\Roaming\WindInputDev\wind_setting.toml"
            )
        );
    }

    #[test]
    fn missing_appdata_returns_none_instead_of_exe_fallback() {
        assert!(config_path_from_appdata(AppMode::Release, None).is_none());
        assert!(config_path_from_appdata(AppMode::Dev, None).is_none());
    }

    #[test]
    fn default_config_keeps_theme_editor_disabled() {
        let cfg = SettingConfig::default();
        assert!(!cfg.theme_editor.auto_start);
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
        let cfg: SettingConfig =
            toml::from_str("[update]\nnetwork_consent = true\n").unwrap();
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
        };
        save_to_path(&config, &path).unwrap();

        let saved = fs::read_to_string(&path).unwrap();
        let loaded: SettingConfig = toml::from_str(&saved).unwrap();
        assert_eq!(loaded, config);

        let _ = fs::remove_dir_all(&dir);
    }
}
