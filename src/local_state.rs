//! 本机状态文件 `wind_setting_state.toml`：只放「这台机器自己的」运行状态，不随设备组同步。
//!
//! 目前只有 `[update] last_check_at`（检查更新节流时间）。它每次检查都变，放进会同步的
//! `wind_setting.toml` 会让各设备不停互相覆盖。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "wind_setting_state.toml";

#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    update: UpdateState,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct UpdateState {
    #[serde(default)]
    last_check_at: i64,
}

fn state_path_in_dir(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// 读状态文件里的检查时间；文件缺失、损坏或没有该键都当「无记录」。
fn read_in(dir: &Path) -> Option<i64> {
    let text = fs::read_to_string(state_path_in_dir(dir)).ok()?;
    let file: StateFile = toml::from_str(&text).ok()?;
    (file.update.last_check_at > 0).then_some(file.update.last_check_at)
}

/// 原子写（临时文件 + 改名）。
fn write_in(dir: &Path, now: i64) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("创建本机状态目录失败：{e}"))?;
    let text = toml::to_string_pretty(&StateFile {
        update: UpdateState { last_check_at: now },
    })
    .map_err(|e| format!("序列化本机状态失败：{e}"))?;
    let path = state_path_in_dir(dir);
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).map_err(|e| format!("写入本机状态临时文件失败：{e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("替换本机状态文件失败：{e}"))
}

fn effective_in(dir: Option<&Path>, legacy: i64) -> i64 {
    dir.and_then(read_in).unwrap_or(legacy)
}

fn local_dir() -> Option<PathBuf> {
    crate::user_dir::local_dir_for_mode(crate::mode::get())
}

/// 上次检查时间：优先本机状态文件，没有时退回 `wind_setting.toml` 里的旧值。
pub fn last_check_at(legacy: i64) -> i64 {
    effective_in(local_dir().as_deref(), legacy)
}

/// 记录检查时间。失败只记日志（只影响节流精度）。
pub fn mark_update_checked(now: i64) {
    let Some(dir) = local_dir() else {
        tracing::warn!("无法定位本机状态目录，检查更新时间未保存");
        return;
    };
    if let Err(e) = write_in(&dir, now) {
        tracing::warn!("保存检查更新时间失败：{e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setting_config::SettingConfig;

    fn tmp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("wind_local_state_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn write_goes_to_state_file_not_setting_toml() {
        let dir = tmp("write");
        write_in(&dir, 1_750_000_000).unwrap();
        let state = fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(state.contains("last_check_at = 1750000000"));

        let mut cfg = SettingConfig::default();
        cfg.update.last_check_at = 1_750_000_000;
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(!text.contains("last_check_at"), "{text}");
    }

    #[test]
    fn legacy_key_still_deserializes() {
        let cfg: SettingConfig = toml::from_str("[update]\nlast_check_at = 42\n").unwrap();
        assert_eq!(cfg.update.last_check_at, 42);
    }

    #[test]
    fn state_file_wins_over_legacy() {
        let dir = tmp("prio");
        write_in(&dir, 200).unwrap();
        assert_eq!(effective_in(Some(&dir), 100), 200);
    }

    #[test]
    fn legacy_used_when_no_state_file() {
        let dir = tmp("legacy");
        assert_eq!(effective_in(Some(&dir), 100), 100);
        assert_eq!(effective_in(None, 100), 100);
    }

    #[test]
    fn corrupt_state_file_is_treated_as_no_record() {
        let dir = tmp("corrupt");
        fs::write(dir.join(FILE_NAME), "this is [not toml").unwrap();
        assert_eq!(read_in(&dir), None);
        assert_eq!(effective_in(Some(&dir), 100), 100);
    }
}
