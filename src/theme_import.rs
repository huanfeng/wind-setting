//! 主题 TOML 导入编排：setting 处理外部输入，core 处理主题语义。

use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::net_task::{ReadError, read_limited};

/// 单份主题原文的上限（2 MiB）。下载与粘贴两条路共用——只闸一条的话，另一条粘错东西
/// 要等 core 才报，而它说的是 TOML 解析失败，指不到「你粘的东西太大了」。
pub const MAX_THEME_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemePreview {
    pub slug: String,
    pub display_name: String,
    pub version: String,
    pub author: String,
    /// 包内资源文件数（仅主题包预览有；URL/文本那条路恒 0）。
    pub asset_count: u64,
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
            asset_count: v.get("asset_count").and_then(Value::as_u64).unwrap_or(0),
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

/// 待导入主题的来源。
///
/// 三个入口——HTTPS 链接、本地 `.toml`、本地 `.wtheme` 包——在这里汇合，于是「撞名了
/// 要不要覆盖」只有一套实现。此前 URL 与本地文件各写了一遍那段冲突处理，包再来一遍
/// 就是第三份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeSource {
    /// 单文件主题的原文。URL 下载与本地 `.toml` 都归到这里：到这一步它们手上都是文本，
    /// 已经没有区别。
    Text(String),
    /// 主题包路径。包没法像文本那样揣在手里等用户确认，只能记路径、导入时再由 core
    /// 解包——这也是覆盖确认必须存「来源」而不是「原文」的原因。
    Package(String),
}

impl ThemeSource {
    /// 落盘。`force` 为真时覆盖同名主题。
    pub fn import(&self, force: bool) -> Result<ThemeImportResult, ThemeImportError> {
        match self {
            Self::Text(text) => import_text(text, force, None),
            Self::Package(path) => import_package(path, force),
        }
    }

    /// 给用户看的来源说明（覆盖确认框里要说清「要用什么覆盖」）。
    pub fn describe(&self) -> String {
        match self {
            Self::Text(_) => "新的主题文件".to_string(),
            Self::Package(path) => std::path::Path::new(path)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "主题包".to_string()),
        }
    }
}

/// 导入主题包。响应形状与 [`import_text`] 一致，冲突同样走 `conflict` 字段。
pub fn import_package(path: &str, force: bool) -> Result<ThemeImportResult, ThemeImportError> {
    let params = json!({ "path": path, "force": force });
    let v = crate::rpc::call_core("theme.importPackage", &params).map_err(classify_rpc_error)?;
    if v.get("conflict").and_then(Value::as_bool) == Some(true) {
        let msg = v
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "该主题已存在".to_string());
        return Err(ThemeImportError::Conflict(msg));
    }
    ThemeImportResult::from_value(v)
}

impl ThemePreview {
    /// 确认框正文：装之前先说清这是什么。
    ///
    /// 主题包多半是从网上下回来的，双击就直接改掉当前外观不合适——方案包那边双击
    /// 同样是先预览再确认，两种包的双击体验不该一个有一个没有。
    pub fn confirm_text(&self) -> String {
        let mut s = format!("主题：{}", self.display_name);
        if !self.author.is_empty() {
            s.push_str(&format!("\n作者：{}", self.author));
        }
        if !self.version.is_empty() {
            s.push_str(&format!("\n版本：{}", self.version));
        }
        s.push_str(&format!("\n资源：{} 个文件", self.asset_count));
        s.push_str("\n\n导入后可在「外观」里切换到它。");
        s
    }
}

/// 当前生效的主题 id（配置键 `ui.theme.name`）。
///
/// ⚠️ **`config.get` 不接受 `key` 参数**：它无视参数、恒返回整份配置对象。传
/// `{"key": "ui.theme.name"}` 再去读回执里的 `value` 字段，拿到的是 `None` 而**不是
/// 错误**——调用方只会得到一个空串，症状是「功能默默不工作」。core 那边现在会把多余的
/// `key` 参数当场拒掉（见 `wind-rpc::dispatch` 的 `config.get`），但别指望那道闸：
/// 取单个键的正确形态就是这里这样，拉整份下来按点路径下钻。
///
/// 好踩在于**命令栏里那个同名的 `config.get(key)` 函数是按 key 取值的**——同名不同源，
/// 一个是 cmdbar 函数、一个是 RPC 方法。
pub fn current_theme_id() -> Result<String, ThemeImportError> {
    let v = crate::rpc::call_core("config.get", &json!({}))
        .map_err(|e| ThemeImportError::Rpc(format!("读取当前主题失败：{e}")))?;
    crate::config::Config(v)
        .get_str("ui.theme.name")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ThemeImportError::InvalidTheme("配置里没有当前主题（ui.theme.name）".into()))
}

/// **待导出**的主题 id：优先用设置页下拉里选中的那个，取不到才回落到 core 生效值。
///
/// 为什么不直接用生效值：用户在下拉里换了主题、还没点保存就去导出，拿生效值导出的是
/// 上一个主题——回执里的名字还是对的，他不会发现自己导错了。下拉里列的主题都已安装，
/// core 按 slug 找目录必然找得到，所以「选中但未保存」照样导得出；导出本就是只读操作，
/// 与「哪个正在生效」无关。
///
/// 回落仍然保留：页面刚建、清单还没就位时 getter 可能取不到值，那时生效值是对的答案。
pub fn theme_id_for_export(ui_selected: Option<String>) -> Result<String, ThemeImportError> {
    match ui_selected.filter(|s| !s.is_empty()) {
        Some(id) => Ok(id),
        None => current_theme_id(),
    }
}

/// 只读预览一个主题包。设置端在「要不要装」之前得先说得出包里是什么。
pub fn preview_package(path: &str) -> Result<ThemePreview, ThemeImportError> {
    let v = crate::rpc::call_core("theme.previewPackage", &json!({ "path": path }))
        .map_err(|e| ThemeImportError::InvalidTheme(format!("读取主题包失败：{e}")))?;
    ThemePreview::from_value(v)
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

/// 连接 / 单次读 / 整体三道超时：慢但持续有数据的下载不被整体 10s 误杀，卡死的连接
/// 也不必等满整体超时才失败。
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const TOTAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 下载 https 文本。**阻塞**，只能在工作线程里调（见 [`crate::net_task`]）；`cancel` 置位后
/// 读取循环在下一个分块停下。
pub fn download_https_text(url: &str, cancel: &AtomicBool) -> Result<String, ThemeImportError> {
    validate_https_url(url)?;
    let response = ureq::AgentBuilder::new()
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout_read(READ_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .redirects(3)
        .build()
        .get(url)
        .call()
        .map_err(|e| ThemeImportError::Download(format!("下载主题失败：{e}")))?;
    let bytes = read_limited(response.into_reader(), MAX_THEME_BYTES, cancel).map_err(|e| {
        ThemeImportError::Download(match e {
            ReadError::TooLarge => "主题文件超过 2 MiB 限制".to_string(),
            ReadError::Cancelled => "已取消下载".to_string(),
            ReadError::Io(m) | ReadError::Write(m) => format!("读取主题响应失败：{m}"),
        })
    })?;
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

    /// 确认框要说得出包里装的是什么 —— 它是用户决定装不装的**唯一**依据。
    #[test]
    fn confirm_text_says_what_is_in_the_package() {
        let p = ThemePreview {
            slug: "shuimo".to_string(),
            display_name: "水墨风".to_string(),
            author: "某人".to_string(),
            version: "1.1".to_string(),
            asset_count: 2,
        };
        let t = p.confirm_text();
        for must in ["水墨风", "某人", "1.1", "2 个文件"] {
            assert!(t.contains(must), "确认框缺了「{must}」：{t}");
        }
    }

    /// 覆盖确认要说清「要用什么覆盖」，两种来源的说法不同：文本来源手里只有一份原文，
    /// 包来源能指名道姓说是哪个文件。
    #[test]
    fn source_describes_itself_for_the_overwrite_prompt() {
        assert_eq!(
            ThemeSource::Text("[meta]\n".to_string()).describe(),
            "新的主题文件"
        );
        // 路径来自文件对话框 / 命令行，是本机原生形态：Windows 的反斜杠在 unix 上
        // 只是文件名里的普通字符，拿它测 unix 构建等于测一条走不到的路。
        let pkg = if cfg!(windows) {
            r"C:\down\水墨.wtheme"
        } else {
            "/home/me/down/水墨.wtheme"
        };
        assert_eq!(
            ThemeSource::Package(pkg.to_string()).describe(),
            "水墨.wtheme"
        );
        // 路径取不出文件名时也得有话说，不能是空串
        assert!(!ThemeSource::Package(String::new()).describe().is_empty());
    }

    /// 两种来源都要把「撞名」认成 Conflict 而不是普通错误 —— 认错了的话，界面上
    /// 就没有「要不要覆盖」这条路，用户等于无法更新已装过的主题。
    #[test]
    fn both_sources_report_conflict_as_conflict() {
        // into_inner: 别的测试 panic 毒化了锁时仍继续，避免一处失败连锁放大
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let text = ThemeSource::Text("[meta]\nname = \"MOCK_EXISTING\"\n".to_string());
        let pkg = ThemeSource::Package(r"C:\down\MOCK_EXISTING.wtheme".to_string());
        let (t_first, t_forced) = (text.import(false), text.import(true));
        let (p_first, p_forced) = (pkg.import(false), pkg.import(true));

        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert!(
            matches!(t_first, Err(ThemeImportError::Conflict(_))),
            "文本来源撞名该归为 Conflict"
        );
        assert!(
            matches!(p_first, Err(ThemeImportError::Conflict(_))),
            "包来源撞名该归为 Conflict"
        );
        // 两条路的回执刻意不同：只断言「成功了」的话，包来源误走 importFromText
        // 也照样绿（撞名哨兵在两边同样生效）。
        assert_eq!(t_forced.expect("force 应成功").slug, "test-theme");
        assert_eq!(
            p_forced.expect("force 应成功").slug,
            "test-theme-pkg",
            "包来源必须走 theme.importPackage"
        );
    }

    #[test]
    fn validate_https_url_accepts_https_only() {
        assert!(validate_https_url("https://theme.windinput.com/a.toml").is_ok());
        assert!(validate_https_url("http://theme.windinput.com/a.toml").is_err());
        assert!(validate_https_url("file:///C:/a.toml").is_err());
    }

    #[test]
    fn download_https_text_rejects_non_https_before_request() {
        assert!(matches!(
            download_https_text("http://theme.windinput.com/a.toml", &AtomicBool::new(false)),
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

    /// 下拉里选中什么就导出什么，**不因为没保存而退回生效值**。
    #[test]
    fn export_takes_the_selected_theme_even_when_unsaved() {
        assert_eq!(
            theme_id_for_export(Some("水墨".into())).unwrap(),
            "水墨",
            "选中的主题必须原样用上，不许被生效值顶掉"
        );
    }

    /// 取不到选中值（页面刚建、清单未就位）才回落到 core 的生效值。
    #[test]
    fn export_falls_back_to_the_effective_theme_when_nothing_selected() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let (none, empty) = (
            theme_id_for_export(None),
            theme_id_for_export(Some(String::new())),
        );
        let effective = current_theme_id();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };

        let effective = effective.expect("mock 下生效值必须取得到");
        assert_eq!(none.unwrap(), effective);
        assert_eq!(empty.unwrap(), effective, "空串等同于没选");
    }

    /// `config.get` 的回执形状：**整份配置，没有 `value` 字段**。
    ///
    /// 这条用例存在的理由是一个真实缺陷：导出按钮照着 CLI 里一段同样错的代码写成
    /// `config.get {"key": ...}` 再读 `value`，于是恒空，用户点导出只看到「没有当前
    /// 主题可导出」。错法不报错、只是静默给空值，所以要靠断言真取到了值来钉住。
    #[test]
    fn current_theme_id_reads_the_whole_config_not_a_value_field() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let got = current_theme_id();
        // mock 回执里没有 `value` 字段：照错法写必然拿到空串 → Err，这条会红。
        let raw = crate::rpc::call_core("config.get", &json!({})).unwrap();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };

        assert!(
            raw.get("value").is_none(),
            "前置条件:config.get 的回执不该有 value 字段，否则本用例测不到那个错法"
        );
        let id = got.expect("当前主题 id 必须取得到");
        assert!(!id.is_empty());
        assert_eq!(
            raw.pointer("/ui/theme/name").and_then(Value::as_str),
            Some(id.as_str()),
            "取到的必须就是 ui.theme.name 那个值"
        );
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
