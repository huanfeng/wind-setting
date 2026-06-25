//! 方案管理对话框 — 安装/删除/启用/禁用输入方案。
//!
//! 注：方案列表受限于 wind-ui-rust 缺口 A（无"可定制行/行内操作"的列表），
//! 暂用 `Element::list` 单行文本 + 下方操作按钮，且改动后需重开对话框刷新。

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

use crate::rpc;
use crate::widgets::flat_panel;

/// 方案管理对话框状态（Signal 为 Copy，可直接 Clone 进闭包）。
#[derive(Clone)]
pub struct SchemaManagerState {
    /// 对话框是否显示。
    pub visible: Signal<bool>,
    /// 方案列表数据 (id, name, enabled, engine_type)。
    pub schemas: Rc<RefCell<Vec<SchemaEntry>>>,
    /// 当前选中的方案索引。
    pub selected: Signal<usize>,
    /// 状态消息。
    pub status: Signal<String>,
}

#[derive(Clone)]
pub struct SchemaEntry {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub engine_type: String,
}

impl SchemaManagerState {
    pub fn new() -> Self {
        let state = Self {
            visible: signal(false),
            schemas: Rc::new(RefCell::new(Vec::new())),
            selected: signal(0),
            status: signal(String::new()),
        };
        state.refresh();
        state
    }

    /// 刷新方案列表。
    pub fn refresh(&self) {
        let list = rpc::call_core("schema.list", &json!({})).unwrap_or(Value::Array(vec![]));
        let entries: Vec<SchemaEntry> = list
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| {
                        Some(SchemaEntry {
                            id: s.get("id")?.as_str()?.to_string(),
                            name: s
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            enabled: s.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
                            engine_type: s
                                .get("engine_type")
                                .or_else(|| s.get("engineType"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                                .to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        *self.schemas.borrow_mut() = entries;
    }

    /// 切换选中方案的启用状态。
    pub fn toggle_selected(&self) {
        let idx = self.selected.get();
        let mut schemas = self.schemas.borrow_mut();
        if let Some(entry) = schemas.get_mut(idx) {
            let new_enabled = !entry.enabled;
            let result = rpc::call_core(
                "schema.setDictEnabled",
                &json!({ "id": entry.id, "enabled": new_enabled }),
            );
            match result {
                Ok(_) => {
                    entry.enabled = new_enabled;
                    self.status.set(format!(
                        "{}: {}",
                        entry.name,
                        if new_enabled {
                            "已启用"
                        } else {
                            "已禁用"
                        }
                    ));
                }
                Err(e) => self.status.set(format!("操作失败: {e}")),
            }
        }
    }

    /// 删除选中方案。
    pub fn delete_selected(&self) {
        let idx = self.selected.get();
        let entry = self.schemas.borrow().get(idx).cloned();
        if let Some(entry) = entry {
            match rpc::call_core("schema.delete", &json!({ "id": entry.id })) {
                Ok(_) => {
                    self.refresh();
                    self.status.set(format!("已删除: {}", entry.name));
                }
                Err(e) => self.status.set(format!("删除失败: {e}")),
            }
        }
    }
}

/// 构建方案管理对话框 UI。
pub fn build_schema_manager_dialog(state: &SchemaManagerState) -> Element {
    let visible = state.visible;
    let selected = state.selected;
    let status = state.status;

    let names: Vec<String> = state
        .schemas
        .borrow()
        .iter()
        .map(|s| {
            format!(
                "{} {} [{}]",
                if s.enabled { "✓" } else { "✗" },
                if s.name.is_empty() { &s.id } else { &s.name },
                s.engine_type
            )
        })
        .collect();

    let list = if names.is_empty() {
        Element::label("暂无方案")
            .font_size(14.0)
            .fg_role(Role::TextMuted)
            .height(30)
            .width_match()
    } else {
        Element::list(names, selected).width_match().height(250)
    };

    let st_toggle = state.clone();
    let st_delete = state.clone();
    let st_refresh = state.clone();

    let content = flat_panel()
        .width(500)
        .padding(18)
        .spacing(12)
        .child(
            Element::label("方案管理")
                .font_size(16.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(26)
                .width_match(),
        )
        .child(Element::divider())
        .child(list)
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .spacing(8)
                .child(
                    Element::button("启用/禁用")
                        .weight(1.0)
                        .on_click(move |_| st_toggle.toggle_selected()),
                )
                .child(
                    Element::button("删除")
                        .weight(1.0)
                        .danger()
                        .on_click(move |_| st_delete.delete_selected()),
                )
                .child(
                    Element::button("刷新")
                        .weight(1.0)
                        .on_click(move |_| st_refresh.refresh()),
                ),
        )
        .child(
            Element::label_rc(status)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .height(20)
                .width_match(),
        )
        .child(
            Element::button("关闭")
                .width_match()
                .on_click(move |_| visible.set(false)),
        );

    Element::dialog(state.visible, content)
}
