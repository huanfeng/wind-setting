//! 主题 TOML 导入编排：setting 处理外部输入，core 处理主题语义。

use serde_json::{Value, json};
use std::io::Read;

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
        Ok(Self { slug, display_name })
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

pub fn preview_text(text: &str) -> Result<ThemePreview, ThemeImportError> {
    let v = crate::rpc::call_core("theme.preview", &json!({ "text": text }))
        .map_err(|e| ThemeImportError::Rpc(format!("主题预览失败：{e}")))?;
    ThemePreview::from_value(v)
}

pub fn import_text(text: &str, force: bool) -> Result<ThemeImportResult, ThemeImportError> {
    let v = crate::rpc::call_core(
        "theme.importFromText",
        &json!({ "text": text, "force": force }),
    )
    .map_err(classify_rpc_error)?;
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
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
    }

    #[test]
    fn preview_text_uses_core_preview_rpc() {
        let _guard = ENV_LOCK.lock().unwrap();
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
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let imported = import_text("[meta]\nname = \"测试主题\"\n", false).unwrap();

        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert_eq!(imported.slug, "test-theme");
        assert_eq!(imported.display_name, "测试主题");
    }
}
