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
    // 同名已存在是**可预期的业务性失败**，core 走 result 字段回报（`conflict: true`）而非
    // error 通道——error 通道只有一个 String，靠匹配文案认冲突会随 core 改文案静默失效。
    // 这里先认字段，再交给 from_value 走成功路径。
    if v.get("conflict").and_then(Value::as_bool) == Some(true) {
        let msg = v
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                let name = v
                    .get("display_name")
                    .or_else(|| v.get("slug"))
                    .and_then(Value::as_str)
                    .unwrap_or("该主题");
                format!("主题「{name}」已存在")
            });
        return Err(ThemeImportError::Conflict(msg));
    }
    ThemeImportResult::from_value(v)
}

/// 兼容旧 core：`conflict` 字段是后加的，更早的 core 把「已存在」走 error 通道回报，
/// 只能按文案认。新 core 走不到这里（冲突不再是 RPC 错误），故这条分支只为跨版本兜底。
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

    /// 同名冲突：core 走 **result 字段**（`conflict: true`）回报，本层必须认出它并转成
    /// `Conflict`——UI 据此弹「是否覆盖」。若退回按报错文案匹配，core 改一次文案就会静默
    /// 退化成一条普通错误，覆盖确认再也弹不出来。
    #[test]
    fn import_text_maps_conflict_result_to_conflict_error() {
        // into_inner: 别的测试 panic 毒化了锁时仍继续，避免一处失败连锁放大
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let text = "[meta]\nname = \"MOCK_EXISTING\"\n";
        let conflict = import_text(text, false, None);
        // 同一份原文 + force=true ⇒ 覆盖成功（覆盖确认框点「覆盖」走的就是这条）。
        let forced = import_text(text, true, None);

        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        let err = conflict.expect_err("撞名时不该返回成功");
        assert!(
            matches!(err, ThemeImportError::Conflict(_)),
            "撞名必须归类为 Conflict（UI 据此弹覆盖确认），实际={err:?}"
        );
        assert!(
            err.user_message().contains("已存在"),
            "提示要说清是撞名，实际={}",
            err.user_message()
        );
        assert_eq!(forced.expect("force=true 应能覆盖").slug, "test-theme");
    }

    /// 旧 core 把撞名走 error 通道（只有一个 String）⇒ 仍按文案兜底认出，跨版本不退化。
    #[test]
    fn classify_rpc_error_still_recognizes_legacy_conflict_message() {
        assert!(matches!(
            classify_rpc_error("主题已存在（force=false）: 测试主题".to_string()),
            ThemeImportError::Conflict(_)
        ));
        assert!(matches!(
            classify_rpc_error("主题 meta.name 为空".to_string()),
            ThemeImportError::InvalidTheme(_)
        ));
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
