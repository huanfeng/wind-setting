//! 主题 TOML 导入编排：setting 处理外部输入，core 处理主题语义。

use serde_json::{Value, json};
use std::io::Read;
use std::path::Path;

const MAX_THEME_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemePreview {
    pub slug: String,
    pub display_name: String,
    pub version: String,
    pub author: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeImportResult {
    pub slug: String,
    pub display_name: String,
    /// 导入的正好是当前生效主题，core 已重解析下发（改动即时可见）。
    /// 由 core 判定并回报——旧版 core 不返回此字段，缺省 false。
    pub reloaded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeImportError {
    InvalidUrl(String),
    Download(String),
    Rpc(String),
    Conflict(String),
    InvalidTheme(String),
}

impl ThemeImportError {
    pub fn user_message(&self) -> String {
        match self {
            Self::InvalidUrl(s)
            | Self::Download(s)
            | Self::Rpc(s)
            | Self::Conflict(s)
            | Self::InvalidTheme(s) => s.clone(),
        }
    }
}

impl ThemePreview {
    fn from_value(v: Value) -> Result<Self, ThemeImportError> {
        let slug = v
            .get("slug")
            .or_else(|| v.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let display_name = v
            .get("display_name")
            .or_else(|| v.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(&slug)
            .to_string();
        if display_name.is_empty() {
            return Err(ThemeImportError::InvalidTheme(
                "主题预览缺少名称".to_string(),
            ));
        }
        Ok(Self {
            slug,
            display_name,
            version: v
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            author: v
                .get("author")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        })
    }
}

impl ThemeImportResult {
    fn from_value(v: Value) -> Result<Self, ThemeImportError> {
        if v.get("ok").and_then(Value::as_bool) == Some(false) {
            return Err(ThemeImportError::InvalidTheme("主题导入失败".to_string()));
        }
        let slug = v
            .get("slug")
            .or_else(|| v.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let display_name = v
            .get("display_name")
            .or_else(|| v.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(&slug)
            .to_string();
        if slug.is_empty() && display_name.is_empty() {
            return Err(ThemeImportError::InvalidTheme(
                "主题导入结果缺少主题标识".to_string(),
            ));
        }
        let reloaded = v.get("reloaded").and_then(Value::as_bool).unwrap_or(false);
        Ok(Self {
            slug,
            display_name,
            reloaded,
        })
    }
}

pub fn validate_https_url(url: &str) -> Result<(), ThemeImportError> {
    if url.to_ascii_lowercase().starts_with("https://") {
        Ok(())
    } else {
        Err(ThemeImportError::InvalidUrl(
            "主题链接必须是 https://".to_string(),
        ))
    }
}

pub fn download_https_text(url: &str) -> Result<String, ThemeImportError> {
    validate_https_url(url)?;
    let response = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(10))
        .redirects(3)
        .build()
        .get(url)
        .call()
        .map_err(|e| ThemeImportError::Download(format!("下载主题失败：{e}")))?;
    let mut reader = response.into_reader().take((MAX_THEME_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| ThemeImportError::Download(format!("读取主题响应失败：{e}")))?;
    if bytes.len() > MAX_THEME_BYTES {
        return Err(ThemeImportError::Download(
            "主题文件超过 2 MiB 限制".to_string(),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| ThemeImportError::Download("主题文件不是有效 UTF-8 文本".to_string()))
}

pub fn read_local_toml(path: &Path) -> Result<String, ThemeImportError> {
    if path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| !s.eq_ignore_ascii_case("toml"))
        .unwrap_or(true)
    {
        return Err(ThemeImportError::InvalidTheme(
            "请选择 .toml 主题文件".to_string(),
        ));
    }
    std::fs::read_to_string(path)
        .map_err(|e| ThemeImportError::InvalidTheme(format!("读取主题文件失败：{e}")))
}

pub fn preview_text(text: &str) -> Result<ThemePreview, ThemeImportError> {
    let v = crate::rpc::call_core("theme.preview", &json!({ "text": text }))
        .map_err(|e| ThemeImportError::Rpc(format!("主题预览失败：{e}")))?;
    ThemePreview::from_value(v)
}

pub fn import_text(
    text: &str,
    force: bool,
    slug: Option<&str>,
) -> Result<ThemeImportResult, ThemeImportError> {
    // core web_theme_import_text 读取 "yaml" 键（内容为 TOML，键名沿用历史契约）；
    // slug 为主题唯一 id：传入则就地写回该 id 目录，否则由 core 按 meta.name 建目录。
    let mut params = json!({ "yaml": text, "force": force });
    if let Some(s) = slug {
        params["slug"] = json!(s);
    }
    let v = crate::rpc::call_core("theme.importFromText", &params).map_err(classify_rpc_error)?;
    ThemeImportResult::from_value(v)
}

fn classify_rpc_error(e: String) -> ThemeImportError {
    if e.contains("已存在") || e.to_ascii_lowercase().contains("conflict") {
        ThemeImportError::Conflict(e)
    } else {
        ThemeImportError::InvalidTheme(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// WIND_RPC_MOCK 是进程全局变量，锁必须与其他模块共用一把（见 rpc.rs 的说明）。
    use crate::rpc::ENV_LOCK;

    #[test]
    fn validate_https_url_accepts_https_only() {
        assert!(validate_https_url("https://theme.windinput.com/a.toml").is_ok());
        assert!(validate_https_url("http://theme.windinput.com/a.toml").is_err());
        assert!(validate_https_url("file:///C:/a.toml").is_err());
    }

    #[test]
    fn download_https_text_rejects_non_https_before_request() {
        assert!(matches!(
            download_https_text("http://theme.windinput.com/a.toml"),
            Err(ThemeImportError::InvalidUrl(_))
        ));
    }

    #[test]
    fn read_local_toml_rejects_non_toml_extension() {
        let path = std::path::Path::new("theme.yaml");
        let err = read_local_toml(path).unwrap_err();
        assert!(err.user_message().contains(".toml"));
    }

    #[test]
    fn parse_preview_supports_display_name_and_name_fallback() {
        let p = ThemePreview::from_value(json!({
            "slug": "blue",
            "display_name": "清风·蓝",
            "version": "1.0",
            "author": "WindInput"
        }))
        .unwrap();
        assert_eq!(p.slug, "blue");
        assert_eq!(p.display_name, "清风·蓝");

        let p = ThemePreview::from_value(json!({ "name": "清风·绿" })).unwrap();
        assert_eq!(p.display_name, "清风·绿");
    }

    #[test]
    fn parse_import_result_requires_success_identity() {
        let r = ThemeImportResult::from_value(json!({
            "ok": true,
            "slug": "blue",
            "display_name": "清风·蓝"
        }))
        .unwrap();
        assert_eq!(r.slug, "blue");
        assert_eq!(r.display_name, "清风·蓝");
        // 旧 core 不返回 reloaded → 缺省 false，不谎报「已生效」
        assert!(!r.reloaded);
    }

    /// core 判定「推的是当前生效主题」时回报 reloaded=true，本层如实透传给编辑器。
    #[test]
    fn parse_import_result_carries_reloaded_flag() {
        let r = ThemeImportResult::from_value(json!({
            "ok": true,
            "slug": "blue",
            "display_name": "清风·蓝",
            "reloaded": true
        }))
        .unwrap();
        assert!(r.reloaded);
    }

    #[test]
    fn parse_import_result_rejects_explicit_failure() {
        assert!(matches!(
            ThemeImportResult::from_value(json!({
                "ok": false,
                "slug": "blue",
                "display_name": "清风·蓝"
            })),
            Err(ThemeImportError::InvalidTheme(_))
        ));
    }

    #[test]
    fn preview_text_uses_core_preview_rpc() {
        // into_inner: 别的测试 panic 毒化了锁时仍继续，避免一处失败连锁放大
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let preview = preview_text("[meta]\nname = \"测试主题\"\n").unwrap();

        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert_eq!(preview.slug, "test-theme");
        assert_eq!(preview.display_name, "测试主题");
        assert_eq!(preview.version, "1.0.0");
        assert_eq!(preview.author, "WindInput");
    }

    #[test]
    fn import_text_uses_core_import_rpc() {
        // into_inner: 别的测试 panic 毒化了锁时仍继续，避免一处失败连锁放大
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let imported = import_text("[meta]\nname = \"测试主题\"\n", false, None).unwrap();

        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert_eq!(imported.slug, "test-theme");
        assert_eq!(imported.display_name, "测试主题");
    }
}
