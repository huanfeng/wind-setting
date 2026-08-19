//! 统一配置导入（高级页「导入与导出」卡片 + 可编辑文本区对话框）。
//!
//! 入口可以多（剪贴板 / 本地文件 / 直接手敲），通路只有一条：片段文本进可编辑
//! 文本区 → 「校验预览」调 `config.previewPatch` 刷新只读预览 → 「应用」发
//! `config.applyPatch`。校验全部在 core 侧（REGISTRY 权威在主仓），本页只渲染
//! 结果；校验失败可在文本区就地改好后再点「校验预览」。
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

/// 预览区一行（渲染前已把值格式化成显示串，host_signal 的数据项要求 `Clone`）。
#[derive(Clone)]
pub struct PreviewRow {
    /// 点分配置键。
    pub key: String,
    /// 正常条目为「当前值 → 新值」，错误条目为错误原因。
    pub detail: String,
    pub is_error: bool,
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
    /// 整体错误回显（TOML 解析失败等 RPC error / 应用失败），空串 = 无。
    pub error_msg: Signal<String>,
    /// 「应用」可点的校验半边：previewPatch ok 且至少一条可应用条目
    /// （另一半是「文本未改动」，见 [`can_apply`]）。
    pub apply_ok: Signal<bool>,
    /// 校验通过时的逐条 (key, next)。应用成功后交给调用方回灌 AppState 的
    /// 配置镜像，让 core 的 `config.changed` 回声被识别为本端写入而不弹提示。
    validated_entries: Rc<RefCell<Vec<(String, Value)>>>,
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
            error_msg: signal(String::new()),
            apply_ok: signal(false),
            validated_entries: Rc::new(RefCell::new(Vec::new())),
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

    /// 作废上次校验：清预览区、禁应用。文本内容不动。
    fn clear_validation(&self) {
        self.rows.set(Vec::new());
        self.apply_ok.set(false);
        self.validated_text.set(String::new());
        self.validated_entries.borrow_mut().clear();
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

    /// 「校验预览」：取文本区当前内容调 `config.previewPatch` 刷新预览区。
    /// 通过（含逐条错误的"通过解析"）即记录 validated_text；RPC 整体失败走 [`Self::fail`]。
    pub fn validate(&self) {
        let text = self.text.get();
        match rpc::call_core("config.previewPatch", &json!({ "text": text })) {
            Ok(v) => {
                let (ok, rows) = parse_preview(&v);
                self.error_msg.set(if rows.is_empty() {
                    "片段为空，没有可导入的配置项".to_string()
                } else {
                    String::new()
                });
                self.rows.set(rows);
                self.apply_ok.set(ok);
                self.validated_text.set(text);
                *self.validated_entries.borrow_mut() = if ok {
                    parse_entry_values(&v)
                } else {
                    Vec::new()
                };
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
    /// 成功关对话框并 toast，返回校验时的逐条 (key, next) 供调用方回灌配置镜像；
    /// 失败对话框内回显（core 的错误串逐键点名，用户就地改好后重新校验）。
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
                Some(std::mem::take(&mut *self.validated_entries.borrow_mut()))
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
    let mut rows = Vec::new();
    if let Some(entries) = v.get("entries").and_then(|e| e.as_array()) {
        for e in entries {
            let key = e
                .get("key")
                .and_then(|k| k.as_str())
                .unwrap_or("?")
                .to_string();
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
    let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
    (ok && !rows.is_empty(), rows)
}

/// previewPatch 响应 → 逐条 (key, next)（跳过错误条目）。应用成功后用它回灌
/// AppState 的配置镜像：core 对每个写入键会广播 `config.changed` 回声，镜像里
/// 已有同值则被识别为本端写入，不弹「已在别处更改」（与 setItems 保存同一豁免机制）。
fn parse_entry_values(v: &Value) -> Vec<(String, Value)> {
    let Some(entries) = v.get("entries").and_then(|e| e.as_array()) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter(|e| e.get("error").is_none())
        .filter_map(|e| {
            let key = e.get("key").and_then(|k| k.as_str())?;
            Some((
                key.to_string(),
                e.get("next").cloned().unwrap_or(Value::Null),
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

    // 预览列表（动态内容一律走 Signal，构建期读取会被永久冻结）。
    let list = Element::host_signal(rows_sig, move |row: PreviewRow| {
        let role = if row.is_error {
            Role::Danger
        } else {
            Role::Text
        };
        // 宽度分配：key 按自然宽度（弹性给 key 会被长 detail 挤到一字宽），detail
        // 占余宽并折行。key 只设上界防畸形长键（错误条目的键是用户手敲的任意串），
        // 中段省略同时保住段前缀与叶子名——两端都是识别一个配置键要看的部分。
        Element::row()
            .width_match()
            .cross(Align::Start)
            .spacing(12)
            .padding_xy(10, 5)
            .child(
                Element::label(&row.key)
                    .font_size(12.5)
                    .fg_role(role)
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
    });
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
                        .on_click(move |_| m_validate.validate()),
                )
                .child(Element::leaf().weight(1.0)),
        )
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

    /// 应用成功后回灌配置镜像用的 (key, next)：跳过错误条目、保留合法条目原值。
    #[test]
    fn parse_entry_values_skips_error_entries() {
        let v = json!({ "ok": false, "entries": [
            { "key": "ui.candidate.per_page", "current": 5, "next": 9 },
            { "key": "input.initial_mode", "next": "chinese" },
            { "key": "input.foo.bar", "next": 1, "error": "未知配置键" }
        ]});
        let pairs = parse_entry_values(&v);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("ui.candidate.per_page".to_string(), json!(9)));
        assert_eq!(
            pairs[1],
            ("input.initial_mode".to_string(), json!("chinese"))
        );
    }
}
