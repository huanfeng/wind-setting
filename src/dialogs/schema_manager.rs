//! 方案管理对话框 — 查看/启用/禁用/重排输入方案，导入占位。
//!
//! 布局策略：
//! - 已启用方案（schema.available 中的 ID）按序显示在列表上方，可用 ↑/↓ 调整顺序
//! - 未启用方案在下方，可通过「隐藏未启用」开关收起
//! - 启用/禁用/排序变更**即时刷新列表**（Element::list_signal 驱动）
//! - 「应用设置」时写入 schema.available（经 FieldRegistry writer）

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

use crate::config::Config;
use crate::rpc;
use crate::widgets::flat_panel;

/// 方案条目（含从 schema.list 解析的完整元信息）。
#[derive(Clone, Default)]
pub struct SchemaEntry {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub engine_type: String,   // "codetable" | "pinyin" | "mixed"
    pub source: String,        // "builtin" | "user"
    pub version: String,
    pub description: String,
    pub author: String,
    pub error: Option<String>,
}

/// `list_signal` 的数据项：行/分节标题/行间分隔线。
#[derive(Clone)]
pub enum SchemaListItem {
    Row {
        entry: SchemaEntry,
        idx: usize,    // 在已启用列表中的位置（未启用行传 0）
        total: usize,  // 已启用方案总数（未启用行传 0）
        is_enabled: bool,
    },
    /// 未启用区分节标题（自带上方分隔线，visible_when !hide_disabled）。
    SectionHeader { count: usize },
    /// 行间分隔线；hidden_with_disabled=true 时随 hide_disabled 一起隐藏。
    Divider { hidden_with_disabled: bool },
}

/// 方案管理状态（Signal 为 Copy，可直接 Clone 进闭包）。
#[derive(Clone)]
pub struct SchemaManagerState {
    /// 方案列表原始数据。
    pub schemas: Rc<RefCell<Vec<SchemaEntry>>>,
    /// 已启用方案的有序 ID 列表（对应 schema.available 配置键）。
    /// 空 vec 表示"全部启用"（首次加载且配置为空时的默认状态）。
    pub enabled_order: Rc<RefCell<Vec<String>>>,
    /// 驱动 list_signal 的数据信号；任何启用/排序变更后调 rebuild_list() 更新。
    pub list_data: Signal<Vec<SchemaListItem>>,
    /// 是否隐藏未启用方案行（响应式，立即生效）。
    pub hide_disabled: Signal<bool>,
    pub selected: Signal<usize>,
    pub status: Signal<String>,
    pub confirm_visible: Signal<bool>,
    pub confirm_msg: Signal<String>,
    pub confirm_id: Rc<RefCell<String>>,
    pub settings_visible: Signal<bool>,
    pub settings_content: Signal<String>,
}

impl SchemaManagerState {
    pub fn new() -> Self {
        let available = Self::read_available();
        let state = Self {
            schemas: Rc::new(RefCell::new(Vec::new())),
            enabled_order: Rc::new(RefCell::new(available)),
            list_data: signal(vec![]),
            hide_disabled: signal(false),
            selected: signal(0),
            status: signal(String::new()),
            confirm_visible: signal(false),
            confirm_msg: signal(String::new()),
            confirm_id: Rc::new(RefCell::new(String::new())),
            settings_visible: signal(false),
            settings_content: signal(String::new()),
        };
        state.refresh();
        state
    }

    /// 同步读取 config.get → schema.available。
    fn read_available() -> Vec<String> {
        rpc::call_core("config.get", &json!({}))
            .ok()
            .and_then(|v| {
                v.get("schema")
                 .and_then(|s| s.get("available"))
                 .and_then(|a| a.as_array())
                 .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            })
            .unwrap_or_default()
    }

    /// 根据当前 schemas + enabled_order 重建 list_data 信号（即时刷新列表）。
    fn rebuild_list(&self) {
        let entries = self.schemas.borrow().clone();
        let order = self.enabled_order.borrow().clone();
        let is_all_enabled = order.is_empty();

        let mut enabled_entries: Vec<SchemaEntry> = Vec::new();
        if is_all_enabled {
            enabled_entries = entries.clone();
        } else {
            for id in &order {
                if let Some(e) = entries.iter().find(|e| &e.id == id) {
                    enabled_entries.push(e.clone());
                }
            }
        }

        let disabled_entries: Vec<SchemaEntry> = if is_all_enabled {
            vec![]
        } else {
            entries.into_iter().filter(|e| !order.contains(&e.id)).collect()
        };

        let n_enabled = enabled_entries.len();
        let n_disabled = disabled_entries.len();
        let mut items: Vec<SchemaListItem> = Vec::new();

        // 已启用区
        for (i, entry) in enabled_entries.into_iter().enumerate() {
            if i > 0 {
                items.push(SchemaListItem::Divider { hidden_with_disabled: false });
            }
            items.push(SchemaListItem::Row { entry, idx: i, total: n_enabled, is_enabled: true });
        }

        // 未启用区（SectionHeader 自带上方分隔线，随 hide_disabled 一起隐藏）
        if n_disabled > 0 {
            items.push(SchemaListItem::SectionHeader { count: n_disabled });
            for (j, entry) in disabled_entries.into_iter().enumerate() {
                if j > 0 {
                    items.push(SchemaListItem::Divider { hidden_with_disabled: true });
                }
                items.push(SchemaListItem::Row { entry, idx: 0, total: 0, is_enabled: false });
            }
        }

        self.list_data.set(items);
    }

    /// apply_loaded 时调用：从配置更新启用排序，并重建列表。
    pub fn apply_config(&self, cfg: &Config) {
        let available: Vec<String> = cfg
            .get_array("schema.available")
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();

        // available 为空表示全部启用：展开为全部方案 ID，使后续 toggle 逻辑无歧义。
        let all_ids: Vec<String> = self.schemas.borrow().iter().map(|e| e.id.clone()).collect();
        let new_order = if available.is_empty() { all_ids } else { available };
        *self.enabled_order.borrow_mut() = new_order;
        self.rebuild_list();
    }

    /// 刷新方案列表（调用 schema.list RPC）。
    pub fn refresh(&self) {
        let list = rpc::call_core("schema.list", &json!({}))
            .unwrap_or(Value::Array(vec![]));

        let order = self.enabled_order.borrow();
        let all_enabled = order.is_empty();

        let entries: Vec<SchemaEntry> = list
            .as_array()
            .map(|arr| {
                arr.iter()
                   .filter_map(|s| {
                       let id = s.get("id")?.as_str()?.to_string();
                       let engine_type = s
                           .get("engineType")
                           .or_else(|| s.get("engine_type"))
                           .or_else(|| s.get("kind"))
                           .and_then(|v| v.as_str())
                           .unwrap_or("unknown")
                           .to_string();
                       let source = if let Some(b) = s.get("builtin").and_then(|v| v.as_bool()) {
                           if b { "builtin" } else { "user" }.to_string()
                       } else {
                           s.get("source").and_then(|v| v.as_str()).unwrap_or("builtin").to_string()
                       };
                       let enabled = all_enabled || order.contains(&id);
                       Some(SchemaEntry {
                           id,
                           name: s.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                           enabled,
                           engine_type,
                           source,
                           version: s.get("version").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                           description: s.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                           author: s.get("author").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                           error: s.get("error").and_then(|v| v.as_str()).map(str::to_string),
                       })
                   })
                   .collect()
            })
            .unwrap_or_default();
        drop(order);

        let count = entries.len();
        *self.schemas.borrow_mut() = entries;

        if count > 0 && self.selected.get() >= count {
            self.selected.set(count - 1);
        }
        self.status.set(format!("共 {count} 个已安装方案"));
        self.rebuild_list();
    }

    /// 上移指定方案在 enabled_order 中的位置（立即刷新列表）。
    pub fn move_schema_up(&self, schema_id: &str) {
        {
            let mut order = self.enabled_order.borrow_mut();
            if let Some(idx) = order.iter().position(|id| id == schema_id) {
                if idx > 0 { order.swap(idx, idx - 1); }
            }
        }
        self.status.set("排序已调整（保存后生效）".into());
        self.rebuild_list();
    }

    /// 下移指定方案在 enabled_order 中的位置（立即刷新列表）。
    pub fn move_schema_down(&self, schema_id: &str) {
        {
            let mut order = self.enabled_order.borrow_mut();
            if let Some(idx) = order.iter().position(|id| id == schema_id) {
                if idx + 1 < order.len() { order.swap(idx, idx + 1); }
            }
        }
        self.status.set("排序已调整（保存后生效）".into());
        self.rebuild_list();
    }

    /// 切换方案启用状态（更新 enabled_order，立即刷新列表）。
    pub fn toggle_schema_enabled(&self, schema_id: &str, new_enabled: bool) {
        if new_enabled {
            let mut order = self.enabled_order.borrow_mut();
            if !order.contains(&schema_id.to_string()) {
                order.push(schema_id.to_string());
            }
        } else {
            let mut order = self.enabled_order.borrow_mut();
            // 若当前全部启用（order 为空），需先展开为全部 ID 再逐一 retain
            if order.is_empty() {
                let all_ids: Vec<String> = self.schemas.borrow().iter().map(|e| e.id.clone()).collect();
                *order = all_ids;
            }
            order.retain(|id| id != schema_id);
        }
        self.status.set(if new_enabled { "已启用（保存后生效）" } else { "已禁用（保存后生效）" }.into());
        self.rebuild_list();
    }

    /// 打开指定 ID 方案的设置对话框。
    pub fn open_settings_at_id(&self, schema_id: &str) {
        let schemas = self.schemas.borrow();
        if let Some(entry) = schemas.iter().find(|e| e.id == schema_id) {
            let content = format!(
                "ID：{}\n名称：{}\n类型：{}\n来源：{}\n版本：{}\n作者：{}\n\n{}",
                entry.id,
                entry.name,
                engine_label(&entry.engine_type),
                if entry.source == "user" { "用户安装" } else { "内置" },
                if entry.version.is_empty() { "—".to_string() } else { entry.version.clone() },
                if entry.author.is_empty() { "—".to_string() } else { entry.author.clone() },
                entry.description.clone(),
            );
            self.settings_content.set(content);
            self.settings_visible.set(true);
        }
    }

    /// 请求删除指定 ID 方案（弹出确认对话框）。
    pub fn request_delete_at_id(&self, schema_id: &str) {
        let schemas = self.schemas.borrow();
        if let Some(entry) = schemas.iter().find(|e| e.id == schema_id) {
            if entry.source == "builtin" {
                self.status.set("内置方案不可删除".into());
                return;
            }
            let display_name =
                if entry.name.is_empty() { entry.id.clone() } else { entry.name.clone() };
            *self.confirm_id.borrow_mut() = entry.id.clone();
            self.confirm_msg.set(format!(
                "确定要删除方案「{display_name}」吗？\n此操作将删除方案文件，不可恢复。"
            ));
            self.confirm_visible.set(true);
        }
    }

    // ── 兼容旧调用路径 ──────────────────────────────────────

    pub fn open_settings_at(&self, idx: usize) {
        let id_opt = self.schemas.borrow().get(idx).map(|e| e.id.clone());
        if let Some(id) = id_opt { self.open_settings_at_id(&id); }
    }

    pub fn request_delete_at(&self, idx: usize) {
        self.selected.set(idx);
        self.request_delete();
    }

    pub fn open_settings(&self) {
        let idx = self.selected.get();
        let id_opt = self.schemas.borrow().get(idx).map(|e| e.id.clone());
        if let Some(id) = id_opt { self.open_settings_at_id(&id); }
    }

    pub fn request_delete(&self) {
        let idx = self.selected.get();
        let id_opt = self.schemas.borrow().get(idx).map(|e| e.id.clone());
        if let Some(id) = id_opt { self.request_delete_at_id(&id); }
    }

    pub fn do_delete(&self) {
        let id = self.confirm_id.borrow().clone();
        self.confirm_visible.set(false);
        if id.is_empty() { return; }
        match rpc::call_core("schema.delete", &json!({ "id": id })) {
            Ok(_) => {
                self.refresh();
                self.status.set("方案已删除".into());
            }
            Err(e) => self.status.set(format!("删除失败: {e}")),
        }
    }
}

// ──────────────────────── 显示辅助 ────────────────────────

pub fn engine_label(t: &str) -> &'static str {
    match t {
        "codetable" => "码表",
        "pinyin"    => "拼音",
        "mixed"     => "混输",
        _           => "未知",
    }
}

// ──────────────────────── 内联复合列表 ────────────────────────

/// 构建内联方案列表，供 `scheme_page` 直接嵌入卡片。
///
/// 使用 `Element::list_signal` 驱动：启用/禁用/排序变更后 list_data 更新，列表即时刷新。
pub fn build_schema_inline_list(state: &SchemaManagerState) -> Element {
    let mgr = state.clone();
    let hide_sig = state.hide_disabled;

    Element::list_signal(
        state.list_data,
        |_| (),
        move |item| match item {
            SchemaListItem::Row { entry, idx, total, is_enabled } => {
                build_compound_row(entry, idx, total, is_enabled, &mgr)
            }
            SchemaListItem::SectionHeader { count } => {
                Element::col()
                    .width_match()
                    .child(Element::divider())
                    .child(
                        Element::row()
                            .width_match()
                            .padding_xy(8, 5)
                            .child(
                                Element::label(format!("其他已安装方案（共 {count} 个）"))
                                    .font_size(10.5)
                                    .fg_role(Role::TextMuted)
                                    .width_match()
                                    .max_lines(1),
                            ),
                    )
                    .visible_when(move || !hide_sig.get())
            }
            SchemaListItem::Divider { hidden_with_disabled } => {
                let d = Element::divider();
                if hidden_with_disabled {
                    d.visible_when(move || !hide_sig.get())
                } else {
                    d
                }
            }
        },
    )
    .height(280)
    .width_match()
}

/// 构建单个方案复合行。
///
/// 所有元素（复选框·名称·徽章·版本·↑↓·设置·删除）均为外层 row 的直接子节点，
/// 由 `cross(Align::Center)` 统一垂直居中。`idx/total` 由 rebuild_list 在每次重建时
/// 传入正确值，↑/↓ 的 enabled_when 无需关心历史状态。
fn build_compound_row(
    entry: SchemaEntry,
    idx: usize,
    total: usize,
    is_enabled: bool,
    mgr: &SchemaManagerState,
) -> Element {
    let enabled_sig = signal(is_enabled);

    // ── Checkbox ──
    let mgr_toggle = mgr.clone();
    let toggle_id  = entry.id.clone();
    let checkbox = Element::checkbox("", enabled_sig)
        .small()
        .on_toggle(move |_| {
            let new_val = !enabled_sig.get();
            enabled_sig.set(new_val);
            mgr_toggle.toggle_schema_enabled(&toggle_id, new_val);
        });

    // ── 名称列（weight-1）：名称 + 可选描述 ──
    let name_text = if entry.name.is_empty() { entry.id.clone() } else { entry.name.clone() };
    let name_fg   = if is_enabled { Role::Text } else { Role::TextMuted };
    let name_label = Element::label(name_text)
        .font_size(13.0)
        .font_weight(600)
        .fg_role(name_fg)
        .width_match()
        .max_lines(1)
        .truncate(Truncate::End);

    let sub_text = if !entry.description.is_empty() {
        entry.description.clone()
    } else if !entry.author.is_empty() {
        format!("作者：{}", entry.author)
    } else {
        String::new()
    };

    let name_col = if sub_text.is_empty() {
        Element::col().weight(1.0).child(name_label)
    } else {
        Element::col()
            .weight(1.0)
            .spacing(2)
            .child(name_label)
            .child(
                Element::label(sub_text)
                    .font_size(11.0)
                    .fg_role(Role::TextMuted)
                    .width_match()
                    .max_lines(1)
                    .truncate(Truncate::End),
            )
    };

    // ── 类型徽章（outline 风格，直接子节点保证垂直居中）──
    let type_badge = Element::label(engine_label(&entry.engine_type))
        .font_size(10.5)
        .fg_role(Role::Accent)
        .padding_xy(6, 2)
        .corner(4.0)
        .border_role(Role::Accent, 1);

    // ── 来源徽章 ──
    let source_badge = Element::label(if entry.source == "user" { "用户" } else { "内置" })
        .font_size(10.5)
        .fg_role(Role::TextMuted)
        .padding_xy(5, 2)
        .corner(4.0)
        .bg_role(Role::SurfaceAlt)
        .border_role(Role::Border, 1);

    // ── 操作按钮 ──
    let is_user   = entry.source == "user";
    let schema_id = entry.id.clone();
    let del_id    = entry.id.clone();

    let st_set = mgr.clone();
    let settings_btn = Element::button("设置…")
        .small()
        .outline()
        .on_click(move |_| st_set.open_settings_at_id(&schema_id));

    // ── 整行：所有子节点平铺，cross:Center 统一垂直居中 ──
    let mut row = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(10, 8)
        .spacing(6)
        .child(checkbox)
        .child(name_col)
        .child(type_badge)
        .child(source_badge);

    if !entry.version.is_empty() {
        row = row.child(
            Element::label(entry.version.clone())
                .font_size(10.5)
                .fg_role(Role::TextMuted),
        );
    }

    // ↑/↓ 仅已启用行；idx/total 由 rebuild_list 保证正确
    if is_enabled {
        let up_id  = entry.id.clone();
        let dn_id  = entry.id.clone();
        let mgr_up = mgr.clone();
        let mgr_dn = mgr.clone();
        row = row
            .child(
                Element::button("↑")
                    .small()
                    .outline()
                    .enabled_when(move || idx > 0)
                    .on_click(move |_| mgr_up.move_schema_up(&up_id)),
            )
            .child(
                Element::button("↓")
                    .small()
                    .outline()
                    .enabled_when(move || idx + 1 < total)
                    .on_click(move |_| mgr_dn.move_schema_down(&dn_id)),
            );
    }

    row = row.child(settings_btn);
    if is_user {
        let st_del = mgr.clone();
        row = row.child(
            Element::button("删除")
                .small()
                .danger()
                .on_click(move |_| st_del.request_delete_at_id(&del_id)),
        );
    }

    // 未启用行随 hide_disabled 隐藏（list_signal 重建后无需 enabled_sig 联动）
    if !is_enabled {
        let hide_sig = mgr.hide_disabled;
        row = row.visible_when(move || !hide_sig.get());
    }

    row
}

// ──────────────────────── UI 构建 ────────────────────────

/// 构建方案相关浮层对话框：删除确认 + 方案详情/设置。
pub fn build_schema_manager_dialog(state: &SchemaManagerState) -> Element {
    let confirm_dlg  = build_confirm_dialog(state);
    let settings_dlg = build_schema_settings_dialog(state);
    Element::stack().fill().child(confirm_dlg).child(settings_dlg)
}

fn build_confirm_dialog(state: &SchemaManagerState) -> Element {
    let st_confirm = state.clone();
    let st_cancel  = state.clone();

    let content = flat_panel()
        .width(380)
        .padding(20)
        .spacing(14)
        .child(
            Element::label("确认删除")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(24)
                .width_match(),
        )
        .child(Element::divider())
        .child(
            Element::label_rc(state.confirm_msg)
                .font_size(13.0)
                .fg_role(Role::Text)
                .width_match(),
        )
        .child(
            Element::row()
                .width_match()
                .spacing(8)
                .child(
                    Element::button("取消")
                        .small()
                        .weight(1.0)
                        .outline()
                        .on_click(move |_| st_cancel.confirm_visible.set(false)),
                )
                .child(
                    Element::button("删除")
                        .small()
                        .weight(1.0)
                        .danger()
                        .on_click(move |_| st_confirm.do_delete()),
                ),
        );

    Element::dialog(state.confirm_visible, content)
}

fn build_schema_settings_dialog(state: &SchemaManagerState) -> Element {
    let visible = state.settings_visible;
    let content = state.settings_content;

    let body = Element::col()
        .width_match()
        .spacing(6)
        .child(
            Element::label_rc(content)
                .font_size(13.0)
                .fg_role(Role::Text)
                .width_match(),
        )
        .child(
            Element::label("方案高级配置将在核心暴露配置键后提供。")
                .font_size(11.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .padding_xy(0, 4),
        );

    let footer = Element::row()
        .width_match()
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("关闭")
                .small()
                .on_click(move |_| visible.set(false)),
        );

    Element::dialog_panel(visible, "方案设置", 440, move |_| visible.set(false), body, footer)
}
