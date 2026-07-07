//! wind_setting 自身配置：只存设置程序运行偏好，不写入 core config。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingConfig {
    #[serde(default)]
    pub theme_editor: ThemeEditorConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThemeEditorConfig {
    #[serde(default)]
    pub auto_start: bool,
}

impl Default for SettingConfig {
    fn default() -> Self {
        Self {
            theme_editor: ThemeEditorConfig::default(),
        }
    }
}

impl Default for ThemeEditorConfig {
    fn default() -> Self {
        Self { auto_start: false }
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
}
