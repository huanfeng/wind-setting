//! 统一配置导入（高级页「导入与导出」卡片 + 可编辑文本区对话框）。
//!
//! 入口可以多（剪贴板 / 本地文件 / 直接手敲），通路只有一条：文本进可编辑文本区
//! → 「校验预览」→ 只读预览 → 「应用」。校验全部在 core 侧（REGISTRY 权威在主仓），
//! 本页只渲染结果；校验失败可在文本区就地改好后再点「校验预览」。
//!
//! 「校验预览」按内容分两支（`package-format.md` §4 侦测规则 4/5）：先问
//! `scheme.previewImportText`——文本是方案信封（`package.kind = "schema_text"`）就
//! 转交方案包导入确认对话框，与 zip 包同一个界面；只有 [`falls_back_to_fragment`]
//! 认可的错误才回落片段管线 `config.previewPatch`。信封解析出的**真实**错误如实
//! 回显，不回落——否则「信封里某个 path 非法」会被片段管线的「未知配置键」淹没。
//!
//! 「应用」门控：预览通过 **且** 文本区内容与最后一次校验时的原文逐字相同
//! （见 [`can_apply`]）——文本一改动应用自动失效，杜绝「校验的是 A、应用的是 B」。
//!
//! 「导入文件…」是统一分派器：`.toml` 填入文本区；`.zip` / `.wpkg` 经
//! `scheme.previewImport` 探测后转交方案包导入确认（复用 schema_manager 的
//! 既有确认对话框与导入逻辑，这里只路由）。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{Value, json};
use windui::core::EventCtx;
use windui::prelude::*;

use crate::dialogs::schema_manager::SchemaManagerState;
use crate::pages::dict::reactive::Feedback;
use crate::rpc;
use crate::state::AppState;
use crate::widgets::{flat_panel, section, setting_row};

/// core 判定「这段文本不是方案信封」时的错误前缀（跨仓契约，见
/// `package-format.md` §3.4）。
pub const NOT_SCHEMA_TEXT_PREFIX: &str = "not_schema_text:";

/// core 对未注册方法的错误前缀（跨仓错误串契约，主仓 `wind-webdata/src/lib.rs`
/// 与 `wind-rpc/src/dispatch.rs` 两层同款 `unknown method: <方法名>`）。
///
/// 认它是为了**版本斜错窗口**：新设置端配旧 core 时 `scheme.previewImportText`
/// 尚未注册，若把这条也当成「信封的真实错误」，片段导入整条路会连带死掉，用户
/// 看到的还是「方案文本解析失败」这种指错方向的提示。
///
/// **按前缀而非包含匹配**：已查证生产路径上这个串没有任何外层包装——core 侧是
/// `anyhow::bail!` 直出，`dispatch.rs` 用 `e.to_string()`（anyhow 只取最外层消息）
/// 装进 `Response::error`，本仓 `call_on_stream` 再原样透传。用包含匹配就多开了
/// 一个误吃面：信封真实错误里回显的用户路径只要字面含这个串，就会被静默回落成
/// 一串「未知配置键」——正是下面注释说要避免的那种噪音。
const UNKNOWN_METHOD_PREFIX: &str = "unknown method: ";

/// 这个 RPC 错误是否应当回落片段管线。
///
/// 两种情形回落：**这段文本不是信封**，或**这个 core 还不认识信封两法**（版本
/// 斜错）。两者都按前缀判定。其余一律不回落——信封自己写错了（files 路径非法、
/// 超限额等）必须如实报，回落的话真正的原因会被片段管线的一串「未知配置键」淹没。
fn falls_back_to_fragment(err: &str) -> bool {
    err.starts_with(NOT_SCHEMA_TEXT_PREFIX) || err.starts_with(UNKNOWN_METHOD_PREFIX)
}

/// 预览区一行（渲染前已把值格式化成显示串，host_signal 的数据项要求 `Clone`）。
#[derive(Clone)]
pub struct PreviewRow {
    /// 显示键：普通条目为点分配置键，Map 条目为 `父键[条目名]`——条目名不并入点分
    /// 键，`custom_mappings` 的条目名本身可含 `.`（见 `package-format.md` §2.2）。
    pub key: String,
    /// 正常条目为「当前值 → 新值」，错误条目为错误原因。
    pub detail: String,
    pub is_error: bool,
}

/// 分发内容的说明元信息（`[package]` 段的 `title` / `description`，
/// `package-format.md` §2.3）。字段缺省即空串。
///
/// 分发者提供的**任意文本**，core 限额 title ≤ 200 字符、description ≤ 4000 字符——
/// 4000 字符足以撑爆对话框，故渲染一律走 [`info_block`]，不要另拼裸 label。
#[derive(Clone, Default)]
pub struct PackageInfo {
    pub title: String,
    pub description: String,
}

/// 分发包携带的配置片段预览（`previewImport` / `previewImportText` 响应的
/// 可选 `configPatch`）。确认导入后由第二步 `config.applyPatch` 应用。
#[derive(Clone)]
pub struct ConfigPatchPreview {
    /// 片段原文，确认后原样交给 `config.applyPatch`（客户端不重新拼装）。
    pub text: String,
    /// 整片段是否通过校验。为假即**整包禁止导入**（`package-format.md` §3.3）。
    pub ok: bool,
    pub rows: Vec<PreviewRow>,
    /// **该片段自己**的说明（`configPatch.info`），与包级说明各显各的。
    pub info: PackageInfo,
}

/// 配置导入状态（Signal 为 Copy，可直接 Clone 进闭包）。
#[derive(Clone)]
pub struct ConfigImportState {
    /// 「导入配置」对话框可见标志。
    pub visible: Signal<bool>,
    /// 片段来源（剪贴板 / 文件名），空串 = 尚未读取。
    pub source_desc: Signal<String>,
    /// 可编辑文本区内容（片段全文，用户可就地修改）。
    pub text: Signal<String>,
    /// 最后一次「校验预览」时的原文，空串 = 尚未校验。「应用」要求当前文本区
    /// 内容与它逐字相同（enabled_when 逐帧比较），文本一改动应用自动失效。
    pub validated_text: Signal<String>,
    /// 预览行（previewPatch 逐条结果）。
    pub rows: Signal<Vec<PreviewRow>>,
    /// 片段自带的说明（previewPatch 响应的 `info`）。**属于预览结果**，与 rows
    /// 同生同灭——换文本 / 重新校验 / 关对话框都清掉，否则 A 的说明会挂在 B 的预览上。
    pub info_title: Signal<String>,
    pub info_desc: Signal<String>,
    /// 整体错误回显（TOML 解析失败等 RPC error / 应用失败），空串 = 无。
    pub error_msg: Signal<String>,
    /// 「应用」可点的校验半边：previewPatch ok 且至少一条可应用条目
    /// （另一半是「文本未改动」，见 [`can_apply`]）。
    pub apply_ok: Signal<bool>,
    pub feedback: Feedback,
}

impl Default for ConfigImportState {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigImportState {
    pub fn new() -> Self {
        Self {
            visible: signal(false),
            source_desc: signal(String::new()),
            text: signal(String::new()),
            validated_text: signal(String::new()),
            rows: signal(Vec::new()),
            info_title: signal(String::new()),
            info_desc: signal(String::new()),
            error_msg: signal(String::new()),
            apply_ok: signal(false),
            feedback: signal(None),
        }
    }

    fn notify(&self, kind: ToastKind, msg: impl Into<String>) {
        self.feedback.set(Some((kind, msg.into())));
    }

    /// 打开对话框（清空上次的文本、预览与错误）。
    pub fn open(&self) {
        self.source_desc.set(String::new());
        self.text.set(String::new());
        self.clear_validation();
        self.error_msg.set(String::new());
        self.visible.set(true);
    }

    pub fn cancel(&self) {
        self.visible.set(false);
    }

    /// 作废上次校验：清预览区（含说明区块）、禁应用。文本内容不动。
    fn clear_validation(&self) {
        self.rows.set(Vec::new());
        self.set_info(&PackageInfo::default());
        self.apply_ok.set(false);
        self.validated_text.set(String::new());
    }

    /// 说明区块取值（空 PackageInfo 即清空，区块随之不占位）。
    fn set_info(&self, info: &PackageInfo) {
        self.info_title.set(info.title.clone());
        self.info_desc.set(info.description.clone());
    }

    /// 校验失败（读不到内容 / 整体解析失败）：清空预览区并回显原因，「应用」禁用。
    fn fail(&self, msg: impl Into<String>) {
        self.clear_validation();
        self.error_msg.set(msg.into());
    }

    /// 片段文本填入文本区（剪贴板 / 文件读取共用）。不自动校验——用户可先就地
    /// 编辑，改好后点「校验预览」；上次的校验结果随之作废。
    pub fn set_text(&self, source: impl Into<String>, text: String) {
        self.source_desc.set(source.into());
        self.text.set(text);
        self.clear_validation();
        self.error_msg.set(String::new());
    }

    /// 「校验预览」：先按方案信封试（`scheme.previewImportText`），不是信封才走片段
    /// 管线（`config.previewPatch`）刷新预览区。
    ///
    /// 三个出口互斥：
    /// - **是信封且预览成功** → 关本对话框，转交 `schema_mgr` 的导入确认（与 zip 包
    ///   同一个界面，含 config_patch 逐键 diff）；
    /// - **是信封但解析出错** → 对话框内回显该错误，**不**回落片段管线；
    /// - **不是信封 / core 不认识信封两法**（见 [`falls_back_to_fragment`]）→ 片段
    ///   管线，行为不变：通过（含逐条错误的「通过解析」）即记录 validated_text；
    ///   RPC 整体失败走 [`Self::fail`]。
    pub fn validate(&self, schema_mgr: &SchemaManagerState) {
        let text = self.text.get();
        match rpc::call_core("scheme.previewImportText", &json!({ "text": text })) {
            Ok(v) => {
                self.clear_validation();
                self.error_msg.set(String::new());
                self.visible.set(false);
                schema_mgr.open_import_text_confirm(text, &v);
                return;
            }
            Err(e) if !falls_back_to_fragment(&e) => {
                return self.fail(format!("方案文本解析失败: {e}"));
            }
            // 不是信封（或 core 还不认识信封两法）：继续片段管线。
            Err(_) => {}
        }
        match rpc::call_core("config.previewPatch", &json!({ "text": text })) {
            Ok(v) => {
                let (ok, rows) = parse_preview(&v);
                self.error_msg.set(if rows.is_empty() {
                    "片段为空，没有可导入的配置项".to_string()
                } else {
                    String::new()
                });
                self.rows.set(rows);
                self.set_info(&parse_info(&v));
                self.apply_ok.set(ok);
                self.validated_text.set(text);
            }
            // 整体 TOML 解析失败等是 RPC error（区别于逐条 error），在对话框内回显。
            Err(e) => self.fail(format!("预览失败: {e}")),
        }
    }

    /// 读剪贴板填入文本区。走 `EventCtx::clipboard_get`（windui 平台剪贴板，无新依赖）。
    pub fn read_clipboard(&self, ctx: &mut EventCtx) {
        match ctx.clipboard_get() {
            Some(text) if !text.trim().is_empty() => self.set_text("剪贴板", text),
            _ => {
                self.source_desc.set("剪贴板".to_string());
                self.fail("剪贴板没有文本内容");
            }
        }
    }

    /// 应用：把（与校验原文逐字相同的）文本区内容提交 `config.applyPatch`。
    /// 成功关对话框并 toast，返回响应里的 `written`（**落盘后**的最终键值）供调用方
    /// 回灌配置镜像；失败对话框内回显（core 的错误串逐键点名，用户就地改好后重新校验）。
    pub fn do_apply(&self) -> Option<Vec<(String, Value)>> {
        let text = self.text.get();
        if !can_apply(self.apply_ok.get(), &text, &self.validated_text.get()) {
            return None;
        }
        match rpc::call_core("config.applyPatch", &json!({ "text": text })) {
            Ok(v) => {
                let applied = v.get("applied").and_then(|x| x.as_u64()).unwrap_or(0);
                let restart = v
                    .get("needsRestart")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                let msg = if restart {
                    format!("已应用 {applied} 项，部分设置重启后生效")
                } else {
                    format!("已应用 {applied} 项")
                };
                self.visible.set(false);
                self.notify(ToastKind::Success, msg);
                Some(parse_written(&v))
            }
            Err(e) => {
                self.error_msg.set(format!("应用失败: {e}"));
                None
            }
        }
    }
}

/// 「应用」可点判定：校验半边通过 + 文本区内容与校验原文逐字相同。
/// 纯函数：enabled_when 逐帧调用，也便于单测门控语义。
fn can_apply(preview_ok: bool, current: &str, validated: &str) -> bool {
    preview_ok && !validated.is_empty() && current == validated
}

// ──────────────────────── 文件分派 ────────────────────────

/// 「导入文件…」的路由结果（按扩展名侦测，纯函数便于单测）。
#[derive(Debug, PartialEq, Eq)]
enum ImportRoute {
    /// `.toml` 配置片段 → 片段预览对话框。
    Fragment,
    /// `.zip` / `.wpkg` → `scheme.previewImport` 探测后走方案包导入。
    Package,
    Unknown,
}

fn route_of(path: &Path) -> ImportRoute {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("toml") => ImportRoute::Fragment,
        Some("zip") | Some("wpkg") => ImportRoute::Package,
        _ => ImportRoute::Unknown,
    }
}

/// 统一「导入文件…」入口：文件选择器（异步请求，禁同步弹框）→ 按类型分派。
pub fn pick_import_file(
    ctx: &mut EventCtx,
    import: &ConfigImportState,
    schema_mgr: &SchemaManagerState,
) {
    let import = import.clone();
    let schema_mgr = schema_mgr.clone();
    ctx.request_pick_file(
        PickDialog::new()
            .title("导入文件")
            .filter("可导入文件", &["toml", "zip", "wpkg"])
            .filter("配置片段", &["toml"])
            .filter("方案包", &["zip", "wpkg"]),
        move |path| {
            let Some(path) = path else { return };
            dispatch_import_path(&import, &schema_mgr, path);
        },
    );
}

/// 分派器只路由不重写域逻辑：片段进本模块预览；方案包转交 schema_manager
/// 既有的确认对话框（`open_import_confirm`）；备份包给出去向提示。
///
/// pub：除本页两个按钮外，`.wpkg` 文件关联双击（argv 路径经 main 的导入通道）
/// 也走这里，保证双击与页内选文件行为完全一致。
pub fn dispatch_import_path(
    import: &ConfigImportState,
    schema_mgr: &SchemaManagerState,
    path: PathBuf,
) {
    match route_of(&path) {
        ImportRoute::Fragment => {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !import.visible.get() {
                import.open();
            }
            match std::fs::read_to_string(&path) {
                Ok(text) => import.set_text(name, text),
                Err(e) => {
                    import.source_desc.set(name);
                    import.fail(format!("读取文件失败: {e}"));
                }
            }
        }
        ImportRoute::Package => {
            // 从对话框内选到方案包也统一转走：关掉片段对话框再进方案包确认。
            import.visible.set(false);
            match rpc::call_core(
                "scheme.previewImport",
                &json!({ "path": path.to_string_lossy() }),
            ) {
                Ok(v) => schema_mgr.open_import_confirm(path, &v),
                // core 对备份包给针对性错误（含「备份包」字样），转成去向提示。
                Err(e) if e.contains("备份") => {
                    import.notify(ToastKind::Info, "该文件是备份包，请到「备份与还原」使用")
                }
                Err(e) => import.notify(ToastKind::Error, format!("读取方案包失败: {e}")),
            }
        }
        ImportRoute::Unknown => import.notify(ToastKind::Error, "不支持的文件类型"),
    }
}

/// 设置窗口左侧栏 / 底栏空白处的右键菜单项：直达导入（比进高级页找卡片更快）。
/// 构建器每次右击都会重跑，闭包捕获的是当次的克隆。
pub fn import_menu_items(
    import: &ConfigImportState,
    schema_mgr: &SchemaManagerState,
) -> Vec<MenuItem> {
    let ci_clip = import.clone();
    let ci_file = import.clone();
    let mgr_file = schema_mgr.clone();
    vec![
        MenuItem::run(
            "从剪贴板导入配置",
            move |ctx| {
                ci_clip.open();
                ci_clip.read_clipboard(ctx);
            },
            false,
        ),
        MenuItem::run(
            "导入文件…",
            move |ctx| pick_import_file(ctx, &ci_file, &mgr_file),
            false,
        ),
    ]
}

// ──────────────────────── 纯函数（便于单测） ────────────────────────

/// previewPatch 响应 → (可应用, 预览行)。可应用 = `ok:true` 且至少一条条目
/// （空片段 applyPatch 虽是合法 no-op，但「应用」没有意义，禁用防误触）。
fn parse_preview(v: &Value) -> (bool, Vec<PreviewRow>) {
    let rows = parse_entries(v);
    let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
    (ok && !rows.is_empty(), rows)
}

/// `{ entries: [...] }` → 预览行。previewPatch 与包内 `configPatch` 的 entries
/// 同形（`package-format.md` §2.3），故两处共用本函数。
fn parse_entries(v: &Value) -> Vec<PreviewRow> {
    let mut rows = Vec::new();
    if let Some(entries) = v.get("entries").and_then(|e| e.as_array()) {
        for e in entries {
            let key = e
                .get("key")
                .and_then(|k| k.as_str())
                .unwrap_or("?")
                .to_string();
            // Map 条目：条目名以 `父键[条目名]` 显示，不并进点分键。
            let key = match e.get("mapEntry").and_then(|k| k.as_str()) {
                Some(entry) => format!("{key}[{entry}]"),
                None => key,
            };
            match e.get("error").and_then(|x| x.as_str()) {
                Some(err) => rows.push(PreviewRow {
                    key,
                    detail: err.to_string(),
                    is_error: true,
                }),
                None => {
                    let next = fmt_value(e.get("next").unwrap_or(&Value::Null));
                    // current 缺席 = 白名单键未设置（core 契约 skip_serializing_if）。
                    let detail = match e.get("current") {
                        Some(c) => format!("{} → {}", fmt_value(c), next),
                        None => format!("未设置 → {next}"),
                    };
                    rows.push(PreviewRow {
                        key,
                        detail,
                        is_error: false,
                    });
                }
            }
        }
    }
    rows
}

/// `previewImport` / `previewImportText` 响应 → 包内配置片段预览。
/// 缺 `configPatch` = 包不带片段（纯方案包），返回 `None`。
pub fn parse_config_patch(v: &Value) -> Option<ConfigPatchPreview> {
    let cp = v.get("configPatch")?;
    Some(ConfigPatchPreview {
        text: cp
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
        ok: cp.get("ok").and_then(|x| x.as_bool()).unwrap_or(false),
        rows: parse_entries(cp),
        info: parse_info(cp),
    })
}

/// 一个带 `title` / `description` 的对象 → 说明（字段缺席即空串）。
fn read_info(v: &Value) -> PackageInfo {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    PackageInfo {
        title: s("title"),
        description: s("description"),
    }
}

/// 片段自己的说明：`previewPatch` 响应 / 包内 `configPatch` 的可选 `info` 段
/// （`package-format.md` §2.4）。两字段都缺省时 core 不发 `info`，这里回空。
pub fn parse_info(v: &Value) -> PackageInfo {
    v.get("info").map(read_info).unwrap_or_default()
}

/// 包级说明：`previewImport` / `previewImportText` 响应**顶层**的 `title` /
/// `description`（来自 package.toml / 信封 `[package]`）。
///
/// 与片段自己的 [`parse_info`] 是两个独立取值点，**互不回落、不做优先级**——
/// §2.3 定稿：两级说的不是同一件事，一方缺席时另一方顶上会把「包是什么」说成
/// 「这段配置改什么」。
pub fn parse_package_info(v: &Value) -> PackageInfo {
    read_info(v)
}

/// applyPatch 响应的 `written` → 逐条 (key, value)。
///
/// **必须用 core 回传的 written 而不是预览 entries**：Map 键逐条合并后，落盘的是
/// 合并结果整表，客户端从 entries 拼不出来（`package-format.md` §2.3）。
/// 应用成功后用它回灌 AppState 的配置镜像：core 对每个写入键会广播 `config.changed`
/// 回声，镜像里已有同值则被识别为本端写入，不弹「已在别处更改」。
pub fn parse_written(v: &Value) -> Vec<(String, Value)> {
    let Some(written) = v.get("written").and_then(|e| e.as_array()) else {
        return Vec::new();
    };
    written
        .iter()
        .filter_map(|e| {
            let key = e.get("key").and_then(|k| k.as_str())?;
            Some((
                key.to_string(),
                e.get("value").cloned().unwrap_or(Value::Null),
            ))
        })
        .collect()
}

/// 配置值 → 显示串：字符串裸显（与 TOML 书写一致），其余用紧凑 JSON。
fn fmt_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ──────────────────────── UI 构建 ────────────────────────

/// 配置片段预览的一行。片段对话框与方案包导入确认对话框**共用**——两处各写一份的
/// 话，宽度分配这类坑就要各踩一次。
///
/// 宽度分配：key 按自然宽度（弹性给 key 会被长 detail 挤到一字宽），detail 占余宽
/// 并折行。key 只设上界防畸形长键（错误条目的键是用户手敲的任意串），中段省略同时
/// 保住段前缀与叶子名——两端都是识别一个配置键要看的部分。
pub fn preview_row_element(row: &PreviewRow) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Start)
        .spacing(12)
        .padding_xy(10, 5)
        .child(
            Element::label(&row.key)
                .font_size(12.5)
                .fg_role(if row.is_error {
                    Role::Danger
                } else {
                    Role::Text
                })
                .max_width(230)
                .max_lines(1)
                .truncate(Truncate::Middle),
        )
        .child(
            Element::label(&row.detail)
                .font_size(12.5)
                .fg_role(if row.is_error {
                    Role::Danger
                } else {
                    Role::TextMuted
                })
                .weight(1.0)
                .max_lines(3),
        )
}

/// 说明正文区的高度上限（px）。超出部分在区块内滚动，不外溢到对话框高度上。
///
/// 有这个上限是因为说明是**分发者提供的任意文本**：core 只把 description 限到
/// 4000 字符，那已经够把对话框顶出屏幕。区块限高与预览列表限高互不相干，两者
/// 各自封顶——共用一份余高的话，长说明会把预览列表挤没。
pub const INFO_DESC_MAX_H: i32 = 84;

/// 说明区块（`[package]` 的 title/description，`package-format.md` §2.3）：
/// 标题单行截断、正文限高滚动；两者皆空时整块不占位。
///
/// 三处渲染共用（片段对话框、包级说明、包内片段说明），`title_size` 是唯一差异——
/// 包内片段的说明挨着「将修改的配置」小标题，字号要比它低一档才不抢层级。
///
/// **一次调用只渲染一份说明**：两级说明各显各的，合并是调用方也不该做的事。
pub fn info_block(title: Signal<String>, desc: Signal<String>, title_size: f32) -> Element {
    Element::col()
        .width_match()
        .spacing(4)
        .child(
            Element::label_signal(title)
                .font_size(title_size)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                // 单行封顶 + 末尾省略：标题最长可达 200 字符，任其折行就是三四行
                // 加粗大字压在预览上方。
                .max_lines(1)
                .truncate(Truncate::End)
                .visible_when(move || !title.get().is_empty()),
        )
        .child(
            Element::scroll()
                .width_match()
                .max_height(INFO_DESC_MAX_H)
                .child(
                    Element::label_signal(desc)
                        .font_size(12.5)
                        .fg_role(Role::TextMuted)
                        .width_match(),
                )
                .visible_when(move || !desc.get().is_empty()),
        )
        .visible_when(move || !title.get().is_empty() || !desc.get().is_empty())
}

/// 高级页「导入与导出」卡片：统一导入文件 + 剪贴板导入两个入口。
pub fn build_import_export_section(
    import: &ConfigImportState,
    schema_mgr: &SchemaManagerState,
) -> Element {
    let m_file = import.clone();
    let mgr_file = schema_mgr.clone();
    let m_clip = import.clone();

    let file_ctl = Element::button("导入文件…")
        .small()
        .outline_soft()
        .on_click(move |ctx| pick_import_file(ctx, &m_file, &mgr_file));

    let clip_ctl = Element::button("从剪贴板导入配置")
        .small()
        .outline_soft()
        .on_click(move |ctx| {
            m_clip.open();
            m_clip.read_clipboard(ctx);
        });

    section(
        "导入与导出",
        vec![
            setting_row(
                "导入文件",
                Some("导入配置片段或方案包，按内容自动识别"),
                file_ctl,
            ),
            setting_row(
                "剪贴板导入",
                Some("读取剪贴板中的配置片段，可编辑后校验应用"),
                clip_ctl,
            ),
            crate::pages::dict::reactive::toast_sink(import.feedback),
        ],
    )
}

/// 「导入配置」对话框：可编辑文本区 + 校验预览。
/// 顶部读取入口 → 文本区（片段全文，可就地编辑）→「校验预览」→ 逐条预览
/// （错误条目警示色）→ 底部应用/取消。`app` 用于应用成功后回灌配置镜像
/// （识别 core 回声为本端写入，见 [`ConfigImportState::do_apply`]）。
pub fn build_config_import_dialog(
    app: &Rc<RefCell<AppState>>,
    import: &ConfigImportState,
    schema_mgr: &SchemaManagerState,
) -> Element {
    let m_clip = import.clone();
    let m_file = import.clone();
    let mgr_file = schema_mgr.clone();
    let m_validate = import.clone();
    // 「校验预览」也可能落到方案信封那一支，需要转交方案包导入确认对话框。
    let mgr_validate = schema_mgr.clone();
    let m_apply = import.clone();
    let app_apply = app.clone();
    let m_cancel = import.clone();
    let apply_ok = import.apply_ok;
    let text_sig = import.text;
    let validated_sig = import.validated_text;
    let rows_sig = import.rows;
    let err_sig = import.error_msg;
    let src_sig = import.source_desc;

    // 来源行：读过内容才显示。
    let source_line = Element::label_signal(src_sig)
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .width_match()
        .visible_when(move || !src_sig.get().is_empty());

    // 可编辑文本区：片段全文可见、可就地修改（等宽字体便于对齐 TOML；字体缺失
    // 时 windui 回退系统默认，不报错）。
    let editor = Element::text_input(text_sig, "在此粘贴或编辑配置片段（TOML）")
        .multiline()
        .wrap(true)
        .font_family("Consolas")
        .width_match()
        .height(180);

    // 片段自带的说明（§2.3：片段导入对话框在预览列表上方显示）。校验前 / 片段没带
    // 说明时整块不占位。
    let info = info_block(import.info_title, import.info_desc, 13.5);

    // 预览列表（动态内容一律走 Signal，构建期读取会被永久冻结）。
    let list = Element::host_signal(rows_sig, |row: PreviewRow| preview_row_element(&row));
    let rows_empty = move || rows_sig.get().is_empty();
    let placeholder = Element::label("点击「校验预览」查看逐项变更")
        .font_size(12.5)
        .fg_role(Role::TextMuted)
        .width_match()
        .padding_xy(10, 8)
        .visible_when(move || rows_empty() && err_sig.get().is_empty());
    let preview = Element::scroll()
        .width_match()
        .height(150)
        .border_role(Role::Divider, 1)
        .child(Element::col().width_match().child(placeholder).child(list));

    // 错误回显（整体解析失败 / 应用失败）。
    let error_line = Element::label_signal(err_sig)
        .font_size(12.5)
        .fg_role(Role::Danger)
        .width_match()
        .max_lines(6)
        .visible_when(move || !err_sig.get().is_empty());

    let content = flat_panel()
        .width(520)
        .padding(20)
        .spacing(14)
        .child(
            Element::label("导入配置")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(24)
                .width_match(),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(
                    Element::button("从剪贴板粘贴")
                        .small()
                        .outline_soft()
                        .on_click(move |ctx| m_clip.read_clipboard(ctx)),
                )
                .child(
                    Element::button("选择文件…")
                        .small()
                        .outline_soft()
                        .on_click(move |ctx| pick_import_file(ctx, &m_file, &mgr_file)),
                )
                .child(Element::leaf().weight(1.0)),
        )
        .child(source_line)
        .child(editor)
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .child(
                    Element::button("校验预览")
                        .small()
                        .outline_soft()
                        .on_click(move |_| m_validate.validate(&mgr_validate)),
                )
                .child(Element::leaf().weight(1.0)),
        )
        .child(info)
        .child(preview)
        .child(error_line)
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| m_cancel.cancel()),
                )
                .child(
                    Element::button("应用")
                        .small()
                        // 门控两半：校验通过 + 文本未再改动（改动即失效，须重新校验）。
                        .enabled_when(move || {
                            can_apply(apply_ok.get(), &text_sig.get(), &validated_sig.get())
                        })
                        .on_click(move |_| {
                            if let Some(applied) = m_apply.do_apply() {
                                // 回灌配置镜像 + 设置页控件（复用外部变更的既有回填
                                // 通路）：core 的逐键 config.changed 回声到达时镜像已
                                // 持有同值，被识别为本端写入，不再弹「已在别处更改」。
                                let mut st = app_apply.borrow_mut();
                                for (key, value) in applied {
                                    st.apply_external_config_change(&key, value);
                                }
                            }
                        }),
                ),
        );
    Element::dialog(import.visible, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_preview_normal_entries() {
        let v = json!({ "ok": true, "entries": [
            { "key": "ui.candidate.per_page", "current": 5, "next": 9 },
            { "key": "input.initial_mode", "next": "chinese" }
        ]});
        let (ok, rows) = parse_preview(&v);
        assert!(ok);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].detail, "5 → 9");
        assert!(!rows[0].is_error);
        // current 缺席（白名单键未设置）→ 显示「未设置」。
        assert_eq!(rows[1].detail, "未设置 → chinese");
    }

    #[test]
    fn parse_preview_error_entries_disable_apply() {
        let v = json!({ "ok": false, "entries": [
            { "key": "ui.candidate.per_page", "current": 5, "next": 9 },
            { "key": "input.foo.bar", "next": 1, "error": "未知配置键" }
        ]});
        let (ok, rows) = parse_preview(&v);
        assert!(!ok, "任一条目有错即禁用应用");
        assert_eq!(rows.len(), 2);
        assert!(rows[1].is_error);
        assert_eq!(rows[1].detail, "未知配置键");
    }

    #[test]
    fn parse_preview_empty_fragment_disables_apply() {
        let (ok, rows) = parse_preview(&json!({ "ok": true, "entries": [] }));
        assert!(!ok, "空片段没有可应用内容");
        assert!(rows.is_empty());
    }

    #[test]
    fn route_by_extension_case_insensitive() {
        assert_eq!(route_of(Path::new("a/b/frag.toml")), ImportRoute::Fragment);
        assert_eq!(route_of(Path::new("pkg.ZIP")), ImportRoute::Package);
        assert_eq!(route_of(Path::new("pkg.wpkg")), ImportRoute::Package);
        assert_eq!(route_of(Path::new("readme.txt")), ImportRoute::Unknown);
        assert_eq!(route_of(Path::new("noext")), ImportRoute::Unknown);
    }

    /// 侧栏/底栏右键菜单恒两项、文案钉死——`shell::build` 两个挂接处只调本函数，
    /// 文案改动或项目增减都应先红在这里。
    #[test]
    fn import_menu_has_clipboard_and_file_entries() {
        let import = ConfigImportState::new();
        let mgr = SchemaManagerState::new();
        let labels: Vec<String> = import_menu_items(&import, &mgr)
            .iter()
            .filter(|i| !i.separator)
            .map(|i| i.label.clone())
            .collect();
        assert_eq!(labels, vec!["从剪贴板导入配置", "导入文件…"]);
    }

    #[test]
    fn fmt_value_strings_bare_others_json() {
        assert_eq!(fmt_value(&json!("chinese")), "chinese");
        assert_eq!(fmt_value(&json!(9)), "9");
        assert_eq!(fmt_value(&json!(true)), "true");
        assert_eq!(fmt_value(&json!(["a", "b"])), r#"["a","b"]"#);
    }

    /// 「应用」门控：校验通过还不够，文本区内容必须与校验原文**逐字**相同——
    /// 一改动（哪怕只是空白）就失效，杜绝「校验的是 A、应用的是 B」。
    #[test]
    fn can_apply_requires_ok_and_unmodified_text() {
        assert!(can_apply(true, "a = 1", "a = 1"));
        assert!(!can_apply(false, "a = 1", "a = 1"), "校验没过不能应用");
        assert!(!can_apply(true, "a = 2", "a = 1"), "校验后改过文本即失效");
        assert!(!can_apply(true, "a = 1 ", "a = 1"), "空白差异同样算改动");
        assert!(
            !can_apply(true, "", ""),
            "尚未校验（validated 为空）不能应用"
        );
    }

    /// 换文本区内容（剪贴板/文件读取）作废上次校验：预览清空、「应用」失效，
    /// 直到对新内容重新点「校验预览」。
    #[test]
    fn set_text_invalidates_previous_validation() {
        let import = ConfigImportState::new();
        // 模拟一次已通过的校验现场。
        import.rows.set(vec![PreviewRow {
            key: "ui.candidate.per_page".to_string(),
            detail: "5 → 9".to_string(),
            is_error: false,
        }]);
        import.apply_ok.set(true);
        import.validated_text.set("old = 1".to_string());

        import.set_text("剪贴板", "new = 2".to_string());

        assert_eq!(import.text.get(), "new = 2");
        assert!(import.rows.get().is_empty(), "旧预览必须清空");
        assert!(!import.apply_ok.get());
        assert!(
            !can_apply(
                import.apply_ok.get(),
                &import.text.get(),
                &import.validated_text.get()
            ),
            "换内容后「应用」必须失效"
        );
    }

    /// 应用成功后回灌配置镜像用的 (key, value) 取自 core 的 `written`——**落盘后**的
    /// 最终键值。Map 父键携带合并后的整表，客户端从 entries 拼不出它（§2.3）。
    #[test]
    fn parse_written_takes_final_values_from_core() {
        let v = json!({ "ok": true, "applied": 2, "needsRestart": false, "written": [
            { "key": "ui.candidate.per_page", "value": 9 },
            // Map 父键：core 合并后的整表，含用户原有的 `x`（片段里只写了 `z`）。
            { "key": "keys.key_actions", "value": { "x": "temp_english", "z": "temp_pinyin" } }
        ]});
        let pairs = parse_written(&v);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("ui.candidate.per_page".to_string(), json!(9)));
        assert_eq!(
            pairs[1].1,
            json!({ "x": "temp_english", "z": "temp_pinyin" }),
            "Map 父键必须原样带回合并后的整表"
        );
    }

    /// 老 core 不回 `written` 时不能 panic，只是没有镜像回灌（退化为「已在别处更改」
    /// 提示一次），不影响配置本身已经落盘。
    #[test]
    fn parse_written_absent_is_empty() {
        assert!(parse_written(&json!({ "ok": true, "applied": 2 })).is_empty());
    }

    /// Map 条目的显示键为 `父键[条目名]`：条目名不并进点分键——
    /// `custom_mappings` 的条目名本身可含 `.`，并进去就分不清层级了。
    #[test]
    fn parse_preview_map_entry_display_key() {
        let v = json!({ "ok": true, "entries": [
            { "key": "keys.key_actions", "mapEntry": "z", "next": "temp_pinyin" },
            { "key": "input.punct.custom_mappings", "mapEntry": ".", "current": "。", "next": "." }
        ]});
        let (ok, rows) = parse_preview(&v);
        assert!(ok);
        assert_eq!(rows[0].key, "keys.key_actions[z]");
        assert_eq!(rows[0].detail, "未设置 → temp_pinyin");
        assert_eq!(rows[1].key, "input.punct.custom_mappings[.]");
        assert_eq!(rows[1].detail, "。 → .");
    }

    /// 回落片段管线的两种情形：文本不是信封（前缀契约），或 core 还不认识信封两法
    /// （版本斜错窗口）。除此之外一律不回落——信封自己写错了（files 路径非法等）
    /// 必须如实报，回落片段管线会让真正的原因被一串「未知配置键」淹没。
    #[test]
    fn falls_back_only_on_not_envelope_or_unknown_method() {
        // 形一：不是信封。前缀契约，只认前缀不认包含。
        assert!(falls_back_to_fragment(&format!(
            "{NOT_SCHEMA_TEXT_PREFIX} 缺少 package.kind"
        )));
        assert!(falls_back_to_fragment("not_schema_text:"));
        assert!(
            !falls_back_to_fragment("解析失败: not_schema_text: 混在中段"),
            "前缀契约只认开头，不认包含"
        );

        // 形二：旧 core 没注册这两法。core 侧 `anyhow::bail!` 直出、dispatch 用
        // `e.to_string()` 装进 Response::error、本仓 call_on_stream 原样透传——
        // 已查证生产路径上无任何外层包装，故同样按前缀判定。
        assert!(falls_back_to_fragment(
            "unknown method: scheme.previewImportText"
        ));
        assert!(
            !falls_back_to_fragment("RPC 失败: unknown method: scheme.importText"),
            "带外层包装的形态不在契约里；认它等于把判据放宽成包含匹配"
        );

        // 信封的真实错误：一条都不许回落。末一条是收紧为前缀匹配要挡的那个误吃面
        // ——用户路径字面含这个串时，包含匹配会把它静默回落成「未知配置键」噪音。
        assert!(!falls_back_to_fragment("files[0].path 非法: ../evil.toml"));
        assert!(!falls_back_to_fragment("信封超限: files 65 条"));
        assert!(!falls_back_to_fragment(
            "format_version 3 高于本版支持的 2，请升级应用"
        ));
        assert!(
            !falls_back_to_fragment("files[1].path 非法: docs/unknown method: 备忘.txt"),
            "路径回显里含该串的信封真实错误必须如实报，不能被当成旧 core"
        );
    }

    // ──────────────── 说明元信息（`package-format.md` §2.3） ────────────────

    /// 片段自己的说明取自可选 `info` 段。两字段都缺省时 core 不发 `info`，此时
    /// 两个字段都要留空——区块靠「两者皆空」判定不占位。
    #[test]
    fn parse_info_reads_the_optional_info_section() {
        let full = json!({ "ok": true, "entries": [],
                           "info": { "title": "九候选", "description": "把候选栏调成每页九个。" } });
        let i = parse_info(&full);
        assert_eq!(i.title, "九候选");
        assert_eq!(i.description, "把候选栏调成每页九个。");

        let none = parse_info(&json!({ "ok": true, "entries": [] }));
        assert!(none.title.is_empty() && none.description.is_empty());

        // 单字段：缺的那个留空，不拿在场的那个补——标题和说明不是一回事。
        assert_eq!(
            parse_info(&json!({ "info": { "title": "只有标题" } })).description,
            ""
        );
        assert_eq!(
            parse_info(&json!({ "info": { "description": "只有说明" } })).title,
            ""
        );
    }

    /// 两级说明取自**两个**位置：包级在响应顶层，片段自己的在 `configPatch.info`。
    ///
    /// 同一个响应里刻意给不同取值：写成同源就分不清 UI 是各显各的、还是有一处串了源。
    /// 单边在场的两形也各测一遍——§2.3 定稿两级说的不是同一件事，缺一方时另一方顶上
    /// 会把「这个包是什么」说成「这段配置改什么」。
    #[test]
    fn package_level_and_fragment_info_are_read_independently() {
        let both = json!({
            "title": "快符方案", "description": "分号引导的符号表。",
            "willAdd": ["kf.schema.toml"],
            "configPatch": { "text": "x = 1\n", "ok": true, "entries": [],
                             "info": { "title": "快符引导键", "description": "把反引号设为引导键。" } }
        });
        let pkg = parse_package_info(&both);
        assert_eq!(pkg.title, "快符方案");
        assert_eq!(pkg.description, "分号引导的符号表。");
        let frag = parse_config_patch(&both).expect("含 configPatch").info;
        assert_eq!(frag.title, "快符引导键");
        assert_eq!(frag.description, "把反引号设为引导键。");

        // 只有包级说明：片段那份保持空，不拿包级的顶上。
        let pkg_only = json!({ "title": "快符方案",
                               "configPatch": { "text": "", "ok": true, "entries": [] } });
        assert_eq!(parse_package_info(&pkg_only).title, "快符方案");
        assert!(
            parse_config_patch(&pkg_only).unwrap().info.title.is_empty(),
            "包级说明不得回落成片段说明"
        );

        // 只有片段说明：包级保持空，方向反过来同样不许串。
        let frag_only = json!({ "configPatch": { "text": "", "ok": true, "entries": [],
                                                 "info": { "title": "快符引导键" } } });
        assert!(parse_package_info(&frag_only).title.is_empty());
        assert_eq!(
            parse_config_patch(&frag_only).unwrap().info.title,
            "快符引导键"
        );
    }

    /// 说明属于**预览结果**：校验后填上，换文本 / 重开对话框随预览一起清空。
    /// 不清的话，A 片段的说明会挂在 B 片段的预览上方，而两者毫无关系。
    #[test]
    fn info_is_filled_by_preview_and_cleared_with_it() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let import = ConfigImportState::new();
        let mgr = SchemaManagerState::new();

        import.set_text("剪贴板", "[ui.candidate]\nper_page = 9\n".to_string());
        assert!(import.info_title.get().is_empty(), "校验前没有说明可显示");
        import.validate(&mgr);
        assert!(
            !import.info_title.get().is_empty(),
            "片段带说明时须显示标题"
        );
        assert!(!import.info_desc.get().is_empty());

        import.set_text(
            "剪贴板",
            "[input]\ninitial_mode = \"chinese\"\n".to_string(),
        );
        assert!(
            import.info_title.get().is_empty() && import.info_desc.get().is_empty(),
            "换文本须与预览一起清空说明"
        );

        import.validate(&mgr);
        assert!(!import.info_title.get().is_empty());
        import.open();
        assert!(
            import.info_title.get().is_empty() && import.info_desc.get().is_empty(),
            "重开对话框须清空说明"
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    // ──────────── 显示防护：说明是分发者提供的任意文本，UI 侧必须自己封顶 ────────────

    /// 片段导入对话框宽度（`build_config_import_dialog` 的 `flat_panel().width(520)`），
    /// 使 label 的换行条件贴近真实。
    const DLG_W: i32 = 520;

    /// 布局说明区块，返回 (整块高, 标题高, 正文占位高, 正文完整内容高)。
    fn layout_info(title: &str, desc: &str) -> (i32, i32, i32, i32) {
        use windui::core::Tree;
        use windui::geometry::Size;
        let mut tree = Tree::new();
        let root = Element::col()
            .width(DLG_W)
            .child(info_block(
                signal(title.to_string()),
                signal(desc.to_string()),
                13.5,
            ))
            .build(&mut tree);
        tree.root = Some(root);
        // NullTextEngine 恒返回单行高，测不出「内容变长 → 溢出 → 可滚动」。
        tree.layout_root(
            Size::new(DLG_W, 2000),
            &mut windui::text::LineAwareTextEngine,
        );
        let block = tree.get(root).unwrap().children[0];
        let kids = tree.get(block).unwrap().children.clone();
        let h = |id| tree.get(id).unwrap().measured.h;
        (
            h(block),
            h(kids[0]),
            h(kids[1]),
            tree.get(kids[1]).unwrap().content_h,
        )
    }

    /// 布局说明区块，返回 (整块是否可见, 可见子节点数)。
    fn info_visibility(title: &str, desc: &str) -> (bool, usize) {
        use windui::core::Tree;
        use windui::geometry::Size;
        let mut tree = Tree::new();
        let root = Element::col()
            .width(DLG_W)
            .child(info_block(
                signal(title.to_string()),
                signal(desc.to_string()),
                13.5,
            ))
            .build(&mut tree);
        tree.root = Some(root);
        tree.layout_root(
            Size::new(DLG_W, 2000),
            &mut windui::text::LineAwareTextEngine,
        );
        let block = tree.get(root).unwrap().children[0];
        let n = tree.get(block).unwrap();
        let visible = n.effective_visible();
        let kids = n
            .children
            .iter()
            .filter(|c| tree.get(**c).is_some_and(|k| k.effective_visible()))
            .count();
        (visible, kids)
    }

    /// 没有说明时整块不占位——留一个空框比不显示更糟，用户会以为说明加载失败了。
    #[test]
    fn info_block_is_absent_when_there_is_nothing_to_show() {
        assert_eq!(info_visibility("", ""), (false, 0));
    }

    /// 单字段在场时只显示在场的那个，另一半不留空行。
    #[test]
    fn info_block_shows_only_the_present_field() {
        assert_eq!(info_visibility("只有标题", ""), (true, 1));
        assert_eq!(info_visibility("", "只有说明"), (true, 1));
        assert_eq!(info_visibility("标题", "说明"), (true, 2));
    }

    /// 限额上限的 description（4000 字符）须封顶在 [`INFO_DESC_MAX_H`] 并留下可滚动量。
    ///
    /// 这是本功能最容易翻车的地方：core 只管到 4000 字符这个限额，那已经够把对话框
    /// 顶出屏幕；不封顶的话预览列表、底部按钮会一起被推走。
    #[test]
    fn limit_length_description_is_capped_and_scrollable() {
        let desc: String = (1..=200)
            .map(|i| format!("第 {i} 行说明：分发者提供的任意文本。\n"))
            .collect::<String>()
            .chars()
            .take(4000)
            .collect();
        let (block_h, _, desc_h, content_h) = layout_info("说明标题", &desc);
        assert_eq!(desc_h, INFO_DESC_MAX_H, "超长说明须封顶在上限高度");
        assert!(
            content_h > INFO_DESC_MAX_H,
            "溢出部分须成为可滚动量（content_h={content_h}）"
        );
        // 整块高 = 标题一行 + 间距 + 正文上限，与 description 有多长无关。
        let (short_block_h, ..) = layout_info("说明标题", "一行短说明。");
        assert!(
            block_h - short_block_h <= INFO_DESC_MAX_H,
            "说明变长只能吃掉正文区那点余量，实际长/短两形差 {}",
            block_h - short_block_h
        );
    }

    /// 限额上限的 title（200 字符）须恒为一行——任其折行就是三四行加粗大字压在
    /// 预览上方，把真正要看的逐键 diff 挤出可视区。
    #[test]
    fn limit_length_title_stays_on_one_line() {
        let long: String = "标题可以一直写到限额上限"
            .chars()
            .cycle()
            .take(200)
            .collect();
        let (_, long_h, ..) = layout_info(&long, "");
        let (_, one_h, ..) = layout_info("短标题", "");
        assert_eq!(long_h, one_h, "200 字符标题须与一行短标题等高（单行封顶）");
    }

    /// 说明区块与预览列表**各自独立限高**：说明再长也不改变预览区高度。
    ///
    /// 两者是 `flat_panel` 里的兄弟节点、各自封顶，共用一份余高的话长说明会把预览
    /// 列表挤没——`preview_row_element` 的宽度分配就是同类问题栽的跟头。
    #[test]
    fn info_block_does_not_squeeze_the_preview_list() {
        use windui::core::Tree;
        use windui::geometry::Size;
        let long: String = std::iter::repeat_n("说明", 2000).collect();
        let measure_preview = |desc: &str| {
            let mut tree = Tree::new();
            // 与对话框同构：说明区块 + 定高预览区，同一个 col 的两个兄弟。
            let root = Element::col()
                .width(DLG_W)
                .spacing(14)
                .child(info_block(
                    signal("说明标题".to_string()),
                    signal(desc.to_string()),
                    13.5,
                ))
                .child(
                    Element::scroll()
                        .width_match()
                        .height(150)
                        .child(Element::label("预览行").width_match()),
                )
                .build(&mut tree);
            tree.root = Some(root);
            tree.layout_root(
                Size::new(DLG_W, 2000),
                &mut windui::text::LineAwareTextEngine,
            );
            let preview = tree.get(root).unwrap().children[1];
            tree.get(preview).unwrap().measured.h
        };
        assert_eq!(measure_preview("一行短说明。"), 150);
        assert_eq!(measure_preview(&long), 150, "说明变长不得改变预览区高度");
    }

    /// 端到端：mock 产出限额上限的说明 → 解析 → 落信号 → 布局，全程不 panic 且封顶。
    ///
    /// 上面几条各自测一段；这条走的是用户真会走的那条路（校验预览 → 显示），
    /// 中间任一环把说明漏掉或没接上封顶都会红在这里。
    #[test]
    fn limit_length_info_survives_the_whole_preview_path() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let import = ConfigImportState::new();
        let mgr = SchemaManagerState::new();
        // 记号让 mock 把说明撑到 core 限额上限（title 200 / description 4000）。
        import.set_text("剪贴板", "# mock_long_info\nper_page = 9\n".to_string());
        import.validate(&mgr);

        assert_eq!(import.info_title.get().chars().count(), 200);
        assert_eq!(import.info_desc.get().chars().count(), 4000);
        let (_, title_h, desc_h, content_h) =
            layout_info(&import.info_title.get(), &import.info_desc.get());
        let (_, one_line_h, ..) = layout_info("短标题", "");
        assert_eq!(title_h, one_line_h, "限额标题仍是一行");
        assert_eq!(desc_h, INFO_DESC_MAX_H, "限额说明仍封顶");
        assert!(content_h > INFO_DESC_MAX_H, "且留下可滚动量");
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 包内 `configPatch` 与 previewPatch 的 entries 同形，缺席即纯方案包。
    #[test]
    fn parse_config_patch_reads_text_ok_and_rows() {
        let v = json!({
            "willAdd": ["kf.schema.toml"],
            "configPatch": { "text": "[keys]\n", "ok": true, "entries": [
                { "key": "keys.key_actions", "mapEntry": "z", "next": "temp_pinyin" }
            ]}
        });
        let p = parse_config_patch(&v).expect("含 configPatch");
        assert_eq!(p.text, "[keys]\n");
        assert!(p.ok);
        assert_eq!(p.rows.len(), 1);
        assert_eq!(p.rows[0].key, "keys.key_actions[z]");

        assert!(
            parse_config_patch(&json!({ "willAdd": [] })).is_none(),
            "纯方案包无 configPatch"
        );
    }
}
