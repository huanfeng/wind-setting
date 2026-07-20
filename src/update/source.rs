//! 更新元数据拉取与校验。
//!
//! 解析与校验是纯函数（[`parse_latest_json`]），网络访问单独一层，便于脱网单测。
//!
//! 校验一律**从严**：`sha256`/`size` 缺失即拒绝升级，而不是退化成"不校验就装"。
//! 旧 Go 版正是因为官网渠道不带 size，让「%TEMP% 里一个被截断的同名文件」可以
//! 被当作完整包直接安装。

use std::io::Read;
use std::time::Duration;

use serde::Deserialize;

use super::{Channel, ReleaseInfo, UpdateError};

const METADATA_TIMEOUT: Duration = Duration::from_secs(10);
const NOTES_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_METADATA_BYTES: usize = 64 * 1024;
const MAX_NOTES_BYTES: usize = 256 * 1024;

/// `latest.json` / `latest-dev.json` 的线上结构（camelCase）。
#[derive(Debug, Deserialize)]
struct LatestJson {
    #[serde(default)]
    version: String,
    #[serde(default)]
    tag: String,
    #[serde(default)]
    channel: String,
    #[serde(default, rename = "exeUrl")]
    exe_url: String,
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    size: u64,
    #[serde(default, rename = "releaseNotesUrl")]
    release_notes_url: String,
    #[serde(default, rename = "publishedAt")]
    published_at: String,
    #[serde(default, rename = "minUpgradableFrom")]
    min_upgradable_from: String,
}

/// 解析并校验更新元数据。`expected` 为本机变体对应的通道。
pub fn parse_latest_json(text: &str, expected: Channel) -> Result<ReleaseInfo, UpdateError> {
    let raw: LatestJson = serde_json::from_str(text)
        .map_err(|e| UpdateError::InvalidMetadata(format!("解析失败：{e}")))?;

    // 通道错配多半是 CDN 缓存串档（stable 用户拿到 dev 包）。宁可不升级也不能装错分支。
    if raw.channel != expected.as_str() {
        return Err(UpdateError::ChannelMismatch {
            expected: expected.as_str().to_string(),
            got: if raw.channel.is_empty() {
                "缺失".to_string()
            } else {
                raw.channel.clone()
            },
        });
    }

    if super::version::parse(&raw.version).is_none() {
        return Err(UpdateError::InvalidMetadata(format!(
            "版本号无法解析：{}",
            if raw.version.is_empty() {
                "缺失"
            } else {
                &raw.version
            }
        )));
    }

    if !raw.exe_url.to_ascii_lowercase().starts_with("https://") {
        return Err(UpdateError::InvalidMetadata(
            "下载地址必须是 https://".to_string(),
        ));
    }

    let sha256 = raw.sha256.trim().to_ascii_lowercase();
    if !is_sha256_hex(&sha256) {
        return Err(UpdateError::InvalidMetadata(
            "缺少有效的 sha256 校验值".to_string(),
        ));
    }

    if raw.size == 0 {
        return Err(UpdateError::InvalidMetadata(
            "缺少安装包大小".to_string(),
        ));
    }

    // notes 地址允许缺失（拉不到就不显示），但给了就必须是 https。
    if !raw.release_notes_url.is_empty()
        && !raw
            .release_notes_url
            .to_ascii_lowercase()
            .starts_with("https://")
    {
        return Err(UpdateError::InvalidMetadata(
            "发布说明地址必须是 https://".to_string(),
        ));
    }

    let tag = if raw.tag.is_empty() {
        format!("v{}", raw.version)
    } else {
        raw.tag.clone()
    };

    Ok(ReleaseInfo {
        version: raw.version,
        tag,
        channel: raw.channel,
        exe_url: raw.exe_url,
        sha256,
        size: raw.size,
        release_notes_url: raw.release_notes_url,
        published_at: raw.published_at,
        min_upgradable_from: raw.min_upgradable_from,
    })
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 拉取指定通道的更新元数据。
pub fn fetch_release_info(channel: Channel) -> Result<ReleaseInfo, UpdateError> {
    let text = http_get_text(channel.metadata_url(), METADATA_TIMEOUT, MAX_METADATA_BYTES)
        .map_err(UpdateError::Network)?;
    parse_latest_json(&text, channel)
}

/// 拉取并清洗发布说明。**失败静默返回空串** —— 说明拉不到不该挡住升级。
pub fn fetch_release_notes(url: &str) -> String {
    if url.is_empty() {
        return String::new();
    }
    match http_get_text(url, NOTES_TIMEOUT, MAX_NOTES_BYTES) {
        Ok(text) => super::notes::clean(&text),
        Err(e) => {
            tracing::warn!("拉取发布说明失败，跳过：{e}");
            String::new()
        }
    }
}

/// 带超时与大小上限的 HTTPS GET。仅允许 https。
fn http_get_text(url: &str, timeout: Duration, max_bytes: usize) -> Result<String, String> {
    if !url.to_ascii_lowercase().starts_with("https://") {
        return Err("仅支持 https 地址".to_string());
    }
    let resp = ureq::AgentBuilder::new()
        .timeout(timeout)
        .redirects(3)
        .build()
        .get(url)
        // CDN 缓存可能滞后于刚上传的元数据，显式绕开
        .set("Cache-Control", "no-cache")
        .call()
        .map_err(|e| format!("{e}"))?;

    let mut reader = resp.into_reader().take((max_bytes + 1) as u64);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读取响应失败：{e}"))?;
    if bytes.len() > max_bytes {
        return Err("响应内容超出大小上限".to_string());
    }
    String::from_utf8(bytes).map_err(|_| "响应不是有效 UTF-8 文本".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_json(channel: &str) -> String {
        format!(
            r#"{{
              "version": "0.107.0",
              "tag": "v0.107.0",
              "channel": "{channel}",
              "exeUrl": "https://dl.windinput.com/WindInput-0.107.0-Setup.exe",
              "sha256": "{}",
              "size": 48234496,
              "releaseNotesUrl": "https://dl.windinput.com/WindInput-0.107.0-Release.md",
              "publishedAt": "2026-07-19T10:00:00Z"
            }}"#,
            "a".repeat(64)
        )
    }

    #[test]
    fn parses_well_formed_metadata() {
        let info = parse_latest_json(&valid_json("stable"), Channel::Stable).unwrap();
        assert_eq!(info.version, "0.107.0");
        assert_eq!(info.tag, "v0.107.0");
        assert_eq!(info.size, 48234496);
        assert_eq!(info.sha256.len(), 64);
        assert!(info.exe_url.starts_with("https://"));
    }

    #[test]
    fn dev_metadata_parses_with_prerelease_version() {
        let json = r#"{
          "version": "0.107.0-dev.3",
          "channel": "dev",
          "exeUrl": "https://dl.windinput.com/WindInputDev-Setup-0.107.0-dev.3.exe",
          "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          "size": 1,
          "releaseNotesUrl": ""
        }"#;
        let info = parse_latest_json(json, Channel::Dev).unwrap();
        assert_eq!(info.version, "0.107.0-dev.3");
        // tag 缺失时按版本号补
        assert_eq!(info.tag, "v0.107.0-dev.3");
    }

    /// CDN 缓存串档：stable 用户拿到 dev 元数据，必须拒绝。
    #[test]
    fn rejects_channel_mismatch() {
        let err = parse_latest_json(&valid_json("dev"), Channel::Stable).unwrap_err();
        assert!(matches!(err, UpdateError::ChannelMismatch { .. }));
    }

    #[test]
    fn rejects_missing_channel_field() {
        let json = valid_json("stable").replace(r#""channel": "stable","#, "");
        let err = parse_latest_json(&json, Channel::Stable).unwrap_err();
        match err {
            UpdateError::ChannelMismatch { got, .. } => assert_eq!(got, "缺失"),
            other => panic!("期望 ChannelMismatch，实际 {other:?}"),
        }
    }

    /// 这是旧 Go 版的实际风险路径：没有 sha256 就装。
    #[test]
    fn rejects_missing_sha256() {
        let json = valid_json("stable").replace(&"a".repeat(64), "");
        let err = parse_latest_json(&json, Channel::Stable).unwrap_err();
        assert!(matches!(err, UpdateError::InvalidMetadata(_)));
        assert!(err.user_message().contains("sha256"));
    }

    #[test]
    fn rejects_malformed_sha256() {
        for bad in ["zz", &"a".repeat(63), &"a".repeat(65), &"g".repeat(64)] {
            let json = valid_json("stable").replace(&"a".repeat(64), bad);
            assert!(
                parse_latest_json(&json, Channel::Stable).is_err(),
                "应拒绝非法 sha256: {bad}"
            );
        }
    }

    #[test]
    fn accepts_uppercase_sha256_and_normalizes() {
        let json = valid_json("stable").replace(&"a".repeat(64), &"A".repeat(64));
        let info = parse_latest_json(&json, Channel::Stable).unwrap();
        assert_eq!(info.sha256, "a".repeat(64), "sha256 应归一化为小写");
    }

    #[test]
    fn rejects_zero_size() {
        let json = valid_json("stable").replace("48234496", "0");
        let err = parse_latest_json(&json, Channel::Stable).unwrap_err();
        assert!(err.user_message().contains("大小"));
    }

    #[test]
    fn rejects_non_https_download_url() {
        let json = valid_json("stable").replace("https://dl.windinput.com/WindInput", "http://dl.windinput.com/WindInput");
        let err = parse_latest_json(&json, Channel::Stable).unwrap_err();
        assert!(err.user_message().contains("https"));
    }

    #[test]
    fn rejects_unparsable_version() {
        let json = valid_json("stable").replace(r#""version": "0.107.0""#, r#""version": "不是版本""#);
        let err = parse_latest_json(&json, Channel::Stable).unwrap_err();
        assert!(err.user_message().contains("版本号"));
    }

    #[test]
    fn rejects_corrupt_json() {
        let err = parse_latest_json("{ 这不是 json", Channel::Stable).unwrap_err();
        assert!(matches!(err, UpdateError::InvalidMetadata(_)));
    }

    /// 契约守护：这段 JSON 是 `WindInput/scripts/dev.ps1` 的 `New-UpdateManifest`
    /// 实际产出的原文（2026-07-19 用 WindInputDev-Setup-0.104.0.exe 实测复制而来）。
    ///
    /// 打包脚本改字段名、改大小写、把 size 写成字符串、给 JSON 加 BOM —— 任一情况
    /// 都会让这个测试红掉，而不是等到用户点「检查更新」才发现升级链路是断的。
    #[test]
    fn parses_real_manifest_produced_by_pack_script() {
        let real = r#"{
  "version": "0.104.0",
  "tag": "v0.104.0",
  "channel": "dev",
  "exeUrl": "https://dl.windinput.com/WindInputDev-Setup-0.104.0.exe",
  "sha256": "d90233c00ca35673d27299be62ce791dbc9b81c76180862891276b0211392382",
  "size": 22659156,
  "releaseNotesUrl": "https://dl.windinput.com/WindInputDev-0.104.0-Release.md",
  "publishedAt": "2026-07-19T12:32:25Z"
}"#;
        let info = parse_latest_json(real, Channel::Dev).expect("打包脚本的产物必须能被解析");
        assert_eq!(info.version, "0.104.0");
        assert_eq!(info.tag, "v0.104.0");
        assert_eq!(info.size, 22_659_156);
        assert_eq!(
            info.sha256,
            "d90233c00ca35673d27299be62ce791dbc9b81c76180862891276b0211392382"
        );
        assert!(info.exe_url.ends_with("WindInputDev-Setup-0.104.0.exe"));

        // 同一份 dev 元数据喂给正式版客户端必须被拒
        assert!(matches!(
            parse_latest_json(real, Channel::Stable),
            Err(UpdateError::ChannelMismatch { .. })
        ));
    }

    /// 该元数据在正式版客户端上应触发升级判定（本地版本更旧时）。
    #[test]
    fn real_manifest_drives_upgrade_decision() {
        let real = r#"{
  "version": "0.104.0", "tag": "v0.104.0", "channel": "dev",
  "exeUrl": "https://dl.windinput.com/WindInputDev-Setup-0.104.0.exe",
  "sha256": "d90233c00ca35673d27299be62ce791dbc9b81c76180862891276b0211392382",
  "size": 22659156, "releaseNotesUrl": "", "publishedAt": "2026-07-19T12:32:25Z"
}"#;
        let info = parse_latest_json(real, Channel::Dev).unwrap();
        let remote = super::super::version::parse(&info.version).unwrap();
        assert!(matches!(
            super::super::version::verdict("v0.103.0", &remote),
            super::super::version::Verdict::Upgrade { .. }
        ));
    }

    #[test]
    fn empty_notes_url_skips_fetch_without_network() {
        assert_eq!(fetch_release_notes(""), "");
    }

    #[test]
    fn http_get_rejects_non_https_before_request() {
        let err = http_get_text("http://example.com/a.json", METADATA_TIMEOUT, 1024).unwrap_err();
        assert!(err.contains("https"));
    }
}
