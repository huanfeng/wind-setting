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
use windui::core::EventCtx;
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
    pub engine_type: String,      // "codetable" | "pinyin" | "mixed"
    pub scheme: String,           // 拼音子方案："full" | "shuangpin"（非拼音为空串）
    pub shuangpin_layout: String, // 双拼当前布局 id（仅 scheme=="shuangpin"；行名后缀显示）
    pub source: String,           // "builtin" | "user"
    pub version: String,
    pub description: String,
    pub author: String,
    pub error: Option<String>,
}

// 双拼布局清单不再硬编码：运行时经 `schema.layouts` RPC 动态拉取（合并安装目录
// 与用户目录 schemas/shuangpin/*.toml），存于 SchemaManagerState::layouts。
// 见 SchemaManagerState::layout_label / refresh。

/// `list_signal` 的数据项：行/分节标题/行间分隔线。
#[derive(Clone)]
pub enum SchemaListItem {
    Row {
        entry: SchemaEntry,
        is_enabled: bool,
    },
    /// 未启用区分节标题（自带上方分隔线，visible_when !hide_disabled）。
    SectionHeader { count: usize },
    /// 行间分隔线；hidden_with_disabled=true 时随 hide_disabled 一起隐藏。
    Divider { hidden_with_disabled: bool },
}

/// 方案设置对话框的一个配置分节。新增一种方案设置 = 加一个变体 + build_section 分支。
#[derive(Clone)]
pub enum SettingsSection {
    /// 方案元信息头部（名称/类型/来源/版本/作者/描述）。恒为首节。
    Header,
    ShuangpinLayout,
    ExtensionDicts,
    Empty,
}

/// 扩展词库行（UI 响应式包装：每行一个开关信号 + 打开时的初始值）。
/// 拨动只翻转 `on`（暂存），点「保存」时对比 `initial` 提交变更。
#[derive(Clone)]
pub struct DictRow {
    pub id: String,
    pub label: String,
    pub description: String,
    /// 打开对话框时的启用状态（用于保存时计算变更差异）。
    pub initial: bool,
    pub on: Signal<bool>,
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
    /// 当前选中方案 ID（工具栏按钮的作用对象；None=无选中）。
    pub selected_id: Rc<RefCell<Option<String>>>,
    /// 选中变更计数：每次变更自增，触发重布局使行高亮/工具栏 enabled_when 重估。
    pub sel_ver: Signal<u64>,
    /// 静态状态文字（列表计数等长效信息）。
    pub status: Signal<String>,
    /// 操作反馈（三态 kind + 消息）：经 toast_sink 弹单条 toast，替代原 status 式提示。
    pub feedback: crate::pages::dict::reactive::Feedback,
    pub confirm_visible: Signal<bool>,
    pub confirm_msg: Signal<String>,
    pub confirm_id: Rc<RefCell<String>>,
    // ── 方案包导入确认（previewImport 摘要 + 合并/替换选择）──
    pub import_confirm_visible: Signal<bool>,
    pub import_preview_msg: Signal<String>,
    /// 待导入的方案包路径（预览确认后执行导入）。
    pending_import_path: Rc<RefCell<Option<std::path::PathBuf>>>,
    pub settings_visible: Signal<bool>,
    /// 设置对话框当前方案条目（驱动结构化头部渲染）。
    pub settings_entry: Rc<RefCell<Option<SchemaEntry>>>,
    /// 设置对话框当前方案 ID（保存时用）。
    pub settings_schema_id: Rc<RefCell<String>>,
    /// 设置对话框打开时经 schema.getConfig 拉取的完整方案配置（保存时改写后回传）。
    pub settings_cfg: Rc<RefCell<Value>>,
    /// 当前方案是否为双拼（决定「双拼布局」行与保存按钮的可见性）。
    pub settings_is_shuangpin: Signal<bool>,
    /// 双拼布局下拉选中索引（对应 [`SchemaManagerState::layouts`] 的顺序）。
    pub settings_layout: Signal<usize>,
    /// 双拼布局清单 (id, 显示名)：refresh() 时经 `schema.layouts` RPC 动态拉取
    /// （合并安装目录与用户目录），取代原硬编码；下拉选项/索引映射/行名后缀均以此为准。
    pub layouts: Rc<RefCell<Vec<(String, String)>>>,
    /// 当前打开方案应显示的配置分节（open_settings_at_id 重算，驱动对话框 body 重建）。
    pub settings_sections: Signal<Vec<SettingsSection>>,
    /// 扩展词库行（每行含独立受控开关信号）；ExtensionDicts 分节据此渲染。
    pub settings_dicts: Rc<RefCell<Vec<DictRow>>>,
    /// 是否存在需批量提交的分节（当前=双拼布局），驱动 footer「保存」按钮可见性。
    pub settings_has_save: Signal<bool>,
}

impl SchemaManagerState {
    pub fn new() -> Self {
        let available = Self::read_available();
        let state = Self {
            schemas: Rc::new(RefCell::new(Vec::new())),
            enabled_order: Rc::new(RefCell::new(available)),
            list_data: signal(vec![]),
            hide_disabled: signal(false),
            selected_id: Rc::new(RefCell::new(None)),
            sel_ver: signal(0),
            status: signal(String::new()),
            feedback: signal(None),
            confirm_visible: signal(false),
            confirm_msg: signal(String::new()),
            confirm_id: Rc::new(RefCell::new(String::new())),
            import_confirm_visible: signal(false),
            import_preview_msg: signal(String::new()),
            pending_import_path: Rc::new(RefCell::new(None)),
            settings_visible: signal(false),
            settings_entry: Rc::new(RefCell::new(None)),
            settings_schema_id: Rc::new(RefCell::new(String::new())),
            settings_cfg: Rc::new(RefCell::new(Value::Null)),
            settings_is_shuangpin: signal(false),
            settings_layout: signal(0),
            layouts: Rc::new(RefCell::new(Vec::new())),
            settings_sections: signal(vec![]),
            settings_dicts: Rc::new(RefCell::new(Vec::new())),
            settings_has_save: signal(false),
        };
        state.refresh();
        state
    }

    /// 双拼布局显示名（未知布局回退 id 原文）。数据源为 layouts（schema.layouts RPC）。
    fn layout_label(&self, id: &str) -> String {
        self.layouts
            .borrow()
            .iter()
            .find(|(lid, _)| lid == id)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    /// 同步读取 config.get → schema.available。
    fn read_available() -> Vec<String> {
        rpc::call_core("config.get", &json!({}))
            .ok()
            .and_then(|v| {
                v.get("schema")
                    .and_then(|s| s.get("available"))
                    .and_then(|a| a.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
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
            entries
                .into_iter()
                .filter(|e| !order.contains(&e.id))
                .collect()
        };

        let n_disabled = disabled_entries.len();
        let mut items: Vec<SchemaListItem> = Vec::new();

        // 已启用区
        for (i, entry) in enabled_entries.into_iter().enumerate() {
            if i > 0 {
                items.push(SchemaListItem::Divider {
                    hidden_with_disabled: false,
                });
            }
            items.push(SchemaListItem::Row {
                entry,
                is_enabled: true,
            });
        }

        // 未启用区（SectionHeader 自带上方分隔线，随 hide_disabled 一起隐藏）
        if n_disabled > 0 {
            items.push(SchemaListItem::SectionHeader { count: n_disabled });
            for (j, entry) in disabled_entries.into_iter().enumerate() {
                if j > 0 {
                    items.push(SchemaListItem::Divider {
                        hidden_with_disabled: true,
                    });
                }
                items.push(SchemaListItem::Row {
                    entry,
                    is_enabled: false,
                });
            }
        }

        self.list_data.set(items);
    }

    /// apply_loaded 时调用：从配置更新启用排序，并重建列表。
    pub fn apply_config(&self, cfg: &Config) {
        let available: Vec<String> = cfg
            .get_array("schema.available")
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        // available 为空表示全部启用：展开为全部方案 ID，使后续 toggle 逻辑无歧义。
        let all_ids: Vec<String> = self.schemas.borrow().iter().map(|e| e.id.clone()).collect();
        let new_order = if available.is_empty() {
            all_ids
        } else {
            available
        };
        *self.enabled_order.borrow_mut() = new_order;
        self.rebuild_list();
    }

    /// 刷新方案列表（调用 schema.list RPC）。
    pub fn refresh(&self) {
        let list = rpc::call_core("schema.list", &json!({})).unwrap_or(Value::Array(vec![]));

        // 双拼布局清单（动态，取代硬编码）：供下拉选项/索引映射/行名后缀。
        let layouts: Vec<(String, String)> = rpc::call_core("schema.layouts", &json!({}))
            .ok()
            .and_then(|v| {
                v.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|it| {
                            let id = it.get("id")?.as_str()?.to_string();
                            let name = it
                                .get("name")
                                .and_then(|v| v.as_str())
                                .map(str::to_string)
                                .unwrap_or_else(|| id.clone());
                            Some((id, name))
                        })
                        .collect()
                })
            })
            .unwrap_or_default();
        *self.layouts.borrow_mut() = layouts;

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
                            s.get("source")
                                .and_then(|v| v.as_str())
                                .unwrap_or("builtin")
                                .to_string()
                        };
                        let enabled = all_enabled || order.contains(&id);
                        let scheme = s
                            .get("scheme")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        // 双拼方案补拉当前布局（行名显示「名称·布局」；通常仅一个双拼方案）。
                        let shuangpin_layout = if scheme == "shuangpin" {
                            rpc::call_core("schema.getConfig", &json!({ "id": id.clone() }))
                                .ok()
                                .and_then(|cfg| {
                                    cfg.pointer("/engine/pinyin/shuangpin/layout")
                                        .and_then(|v| v.as_str())
                                        .map(str::to_string)
                                })
                                .unwrap_or_default()
                        } else {
                            String::new()
                        };
                        Some(SchemaEntry {
                            id,
                            name: s
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            enabled,
                            engine_type,
                            scheme,
                            shuangpin_layout,
                            source,
                            version: s
                                .get("version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            description: s
                                .get("description")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            author: s
                                .get("author")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            error: s.get("error").and_then(|v| v.as_str()).map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        drop(order);

        let count = entries.len();
        *self.schemas.borrow_mut() = entries;

        // 选中项已不存在（如被删除/刷新后消失）则清空选中。
        let stale = self
            .selected_id
            .borrow()
            .as_ref()
            .is_some_and(|id| !self.schemas.borrow().iter().any(|e| &e.id == id));
        if stale {
            self.clear_selection();
        }
        self.status.set(format!("共 {count} 个已安装方案"));
        self.rebuild_list();
    }

    // ── 选中模型（工具栏按钮的作用对象） ──────────────────────

    /// 选中指定方案（行点击回调）。
    pub fn select(&self, id: &str) {
        *self.selected_id.borrow_mut() = Some(id.to_string());
        self.bump_sel();
    }

    /// 清空选中。
    pub fn clear_selection(&self) {
        *self.selected_id.borrow_mut() = None;
        self.bump_sel();
    }

    fn bump_sel(&self) {
        self.sel_ver.set(self.sel_ver.get().wrapping_add(1));
    }

    /// 该 ID 是否为当前选中（行高亮用）。
    pub fn is_selected(&self, id: &str) -> bool {
        self.selected_id.borrow().as_deref() == Some(id)
    }

    /// 当前选中的方案条目。
    pub fn selected_entry(&self) -> Option<SchemaEntry> {
        let id = self.selected_id.borrow().clone()?;
        self.schemas.borrow().iter().find(|e| e.id == id).cloned()
    }

    /// 选中项在启用区的位置 (idx, total)；未选中或未启用返回 None。
    /// enabled_order 为空表示「全部启用」，按 schemas 原始序折算。
    pub fn selected_enabled_pos(&self) -> Option<(usize, usize)> {
        let id = self.selected_id.borrow().clone()?;
        let order = self.enabled_order.borrow();
        if order.is_empty() {
            let schemas = self.schemas.borrow();
            let idx = schemas.iter().position(|e| e.id == id)?;
            return Some((idx, schemas.len()));
        }
        let idx = order.iter().position(|x| *x == id)?;
        Some((idx, order.len()))
    }

    /// 选中项是否为用户安装（删除按钮可用条件）。
    pub fn selected_is_user(&self) -> bool {
        self.selected_entry().is_some_and(|e| e.source == "user")
    }

    /// 上/下移选中方案。enabled_order 为空（全部启用）时先展开为全部 ID，
    /// 与 toggle_schema_enabled 的展开策略一致，保证重排有承载体。
    pub fn move_selected(&self, up: bool) {
        let Some(id) = self.selected_id.borrow().clone() else {
            return;
        };
        {
            let mut order = self.enabled_order.borrow_mut();
            if order.is_empty() {
                *order = self.schemas.borrow().iter().map(|e| e.id.clone()).collect();
            }
        }
        if up {
            self.move_schema_up(&id);
        } else {
            self.move_schema_down(&id);
        }
    }

    /// 打开选中方案的设置对话框。
    pub fn open_settings_selected(&self) {
        let Some(id) = self.selected_id.borrow().clone() else {
            return;
        };
        self.open_settings_at_id(&id);
    }

    /// 请求删除选中方案（弹确认框）。
    pub fn request_delete_selected(&self) {
        let Some(id) = self.selected_id.borrow().clone() else {
            return;
        };
        self.request_delete_at_id(&id);
    }

    /// 操作反馈：弹单条 toast（经页面挂载的 toast_sink）。
    fn notify(&self, kind: ToastKind, msg: impl Into<String>) {
        self.feedback.set(Some((kind, msg.into())));
    }

    /// 上移指定方案在 enabled_order 中的位置（立即刷新列表）。
    pub fn move_schema_up(&self, schema_id: &str) {
        {
            let mut order = self.enabled_order.borrow_mut();
            if let Some(idx) = order.iter().position(|id| id == schema_id) {
                if idx > 0 {
                    order.swap(idx, idx - 1);
                }
            }
        }
        self.notify(ToastKind::Success, "排序已调整（保存后生效）");
        self.rebuild_list();
    }

    /// 下移指定方案在 enabled_order 中的位置（立即刷新列表）。
    pub fn move_schema_down(&self, schema_id: &str) {
        {
            let mut order = self.enabled_order.borrow_mut();
            if let Some(idx) = order.iter().position(|id| id == schema_id) {
                if idx + 1 < order.len() {
                    order.swap(idx, idx + 1);
                }
            }
        }
        self.notify(ToastKind::Success, "排序已调整（保存后生效）");
        self.rebuild_list();
    }

    /// 切换方案启用状态（更新 enabled_order，立即刷新列表）。
    /// 禁用最后一个启用中的方案会被拒绝（返回 false，调用方回滚复选框）。
    pub fn toggle_schema_enabled(&self, schema_id: &str, new_enabled: bool) -> bool {
        if new_enabled {
            let mut order = self.enabled_order.borrow_mut();
            if !order.contains(&schema_id.to_string()) {
                order.push(schema_id.to_string());
            }
        } else {
            let mut order = self.enabled_order.borrow_mut();
            // 若当前全部启用（order 为空），需先展开为全部 ID 再逐一 retain
            if order.is_empty() {
                let all_ids: Vec<String> =
                    self.schemas.borrow().iter().map(|e| e.id.clone()).collect();
                *order = all_ids;
            }
            // 守护：至少保留一个启用方案（否则 order 变空又回到「全部启用」语义，
            // 表现为"禁用最后一个反而全开"）。
            if order.len() <= 1 {
                drop(order);
                self.notify(ToastKind::Error, "至少需保留一个启用的方案");
                return false;
            }
            order.retain(|id| id != schema_id);
        }
        self.notify(
            ToastKind::Success,
            if new_enabled {
                "已启用（保存后生效）"
            } else {
                "已禁用（保存后生效）"
            },
        );
        self.rebuild_list();
        true
    }

    /// 打开指定 ID 方案的设置对话框。双拼方案额外经 schema.getConfig 拉取
    /// 当前布局，展示「双拼布局」下拉（保存走 schema.saveConfig 稀疏 override）。
    pub fn open_settings_at_id(&self, schema_id: &str) {
        let entry = {
            let schemas = self.schemas.borrow();
            let Some(e) = schemas.iter().find(|e| e.id == schema_id) else {
                return;
            };
            e.clone()
        };

        // 方案配置（三层合并结果）；失败按空对象处理（布局下拉回退首项）。
        let cfg = rpc::call_core("schema.getConfig", &json!({ "id": schema_id }))
            .unwrap_or_else(|_| json!({}));
        let is_sp = entry.scheme == "shuangpin";
        if is_sp {
            let layout = cfg
                .pointer("/engine/pinyin/shuangpin/layout")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let idx = self
                .layouts
                .borrow()
                .iter()
                .position(|(id, _)| id == layout)
                .unwrap_or(0);
            self.settings_layout.set(idx);
        }
        *self.settings_schema_id.borrow_mut() = entry.id.clone();
        *self.settings_cfg.borrow_mut() = cfg;
        self.settings_is_shuangpin.set(is_sp);
        *self.settings_entry.borrow_mut() = Some(entry);

        // ── 计算配置分节（可扩展体系）：Header 恒为首节，其后按方案类型追加配置节 ──
        let mut sections: Vec<SettingsSection> = vec![SettingsSection::Header];
        let mut has_config = false; // 是否有需「保存」按钮提交的配置节
        if is_sp {
            sections.push(SettingsSection::ShuangpinLayout);
            has_config = true;
        }
        let dict_infos = parse_extension_dicts(&self.settings_cfg.borrow());
        let rows: Vec<DictRow> = dict_infos
            .into_iter()
            .map(|d| DictRow {
                id: d.id,
                label: d.label,
                description: d.description,
                initial: d.enabled,
                on: signal(d.enabled),
            })
            .collect();
        if !rows.is_empty() {
            sections.push(SettingsSection::ExtensionDicts);
            has_config = true;
        }
        *self.settings_dicts.borrow_mut() = rows;
        if !has_config {
            sections.push(SettingsSection::Empty);
        }
        // 扩展词库与双拼布局都经「保存」按钮统一提交。
        self.settings_has_save.set(has_config);
        self.settings_sections.set(sections);

        self.settings_visible.set(true);
    }

    /// 统一保存设置对话框的改动：扩展词库变更逐个走 schema.setDictEnabled，
    /// 双拼布局走 schema.saveConfig（稀疏 override）。任一环失败即中止、不关窗、
    /// 保留已暂存的翻转供重试。
    pub fn save_settings(&self) {
        let id = self.settings_schema_id.borrow().clone();
        if id.is_empty() {
            return;
        }

        // 1) 扩展词库：仅提交相对打开时发生变化的行（当前值 != initial）。
        //    core 将其写入 override 层并热插拔即时生效（不重熔大词库）。
        let changed: Vec<(String, bool)> = self
            .settings_dicts
            .borrow()
            .iter()
            .filter(|r| r.on.get() != r.initial)
            .map(|r| (r.id.clone(), r.on.get()))
            .collect();
        for (dict_id, want) in &changed {
            if let Err(e) = rpc::call_core(
                "schema.setDictEnabled",
                &json!({ "id": id, "dictId": dict_id, "enabled": *want }),
            ) {
                self.notify(ToastKind::Error, format!("扩展词库保存失败: {e}"));
                return;
            }
        }

        // 2) 双拼布局：改写拉取的完整配置后回传 saveConfig。
        if self.settings_is_shuangpin.get() {
            if self.layouts.borrow().is_empty() {
                self.notify(ToastKind::Error, "双拼布局清单未加载，无法保存");
                return;
            }
            let layout = {
                let layouts = self.layouts.borrow();
                let idx = self.settings_layout.get().min(layouts.len() - 1);
                layouts[idx].0.clone()
            };
            let mut cfg = self.settings_cfg.borrow().clone();
            if !cfg.is_object() {
                cfg = json!({});
            }
            set_json_path(
                &mut cfg,
                &["engine", "pinyin", "shuangpin", "layout"],
                json!(layout),
            );
            if let Err(e) = rpc::call_core("schema.saveConfig", &json!({ "id": id, "cfg": cfg })) {
                self.notify(ToastKind::Error, format!("保存失败: {e}"));
                return;
            }
        }

        self.settings_visible.set(false);
        // 行名可能含布局后缀，保存后重拉列表刷新显示（refresh 保留选中）。
        self.refresh();
        self.notify(ToastKind::Success, "方案配置已保存");
    }

    /// 请求删除指定 ID 方案（弹出确认对话框）。
    pub fn request_delete_at_id(&self, schema_id: &str) {
        let schemas = self.schemas.borrow();
        if let Some(entry) = schemas.iter().find(|e| e.id == schema_id) {
            if entry.source == "builtin" {
                self.notify(ToastKind::Error, "内置方案不可删除");
                return;
            }
            let display_name = if entry.name.is_empty() {
                entry.id.clone()
            } else {
                entry.name.clone()
            };
            *self.confirm_id.borrow_mut() = entry.id.clone();
            self.confirm_msg.set(format!(
                "确定要删除方案「{display_name}」吗？\n此操作将删除方案文件，不可恢复。"
            ));
            self.confirm_visible.set(true);
        }
    }

    pub fn do_delete(&self) {
        let id = self.confirm_id.borrow().clone();
        self.confirm_visible.set(false);
        if id.is_empty() {
            return;
        }
        match rpc::call_core("schema.delete", &json!({ "id": id })) {
            Ok(_) => {
                // refresh 内会发现选中 ID 已消失并清空选中。
                self.refresh();
                self.notify(ToastKind::Success, "方案已删除");
            }
            Err(e) => self.notify(ToastKind::Error, format!("删除失败: {e}")),
        }
    }

    // ── 方案包导入导出（scheme.exportPackage/previewImport/importPackage）──

    /// 导出选中方案为方案包 zip。对话框经 `ctx.request_save_file` 异步请求
    /// （禁止在点击回调里同步弹模态框，见 dict::do_export_phrases 注释）。
    pub fn do_export_selected(&self, ctx: &mut EventCtx) {
        let Some(entry) = self.selected_entry() else {
            self.notify(ToastKind::Error, "请先选中方案");
            return;
        };
        let state = self.clone();
        // 文件名带方案版本便于区分（schema.toml 未标版本时省略）。
        let file_name = if entry.version.is_empty() {
            format!("{}.zip", entry.id)
        } else {
            format!("{}-{}.zip", entry.id, entry.version)
        };
        ctx.request_save_file(
            PickDialog::new()
                .title("导出方案包")
                .file_name(&file_name)
                .filter("方案包", &["zip"]),
            move |path| {
                let Some(path) = path else { return };
                match rpc::call_core(
                    "scheme.exportPackage",
                    &json!({ "id": entry.id, "path": path.to_string_lossy() }),
                ) {
                    Ok(v) => {
                        let n = |k: &str| {
                            v.get(k)
                                .and_then(|x| x.as_array())
                                .map(|a| a.len())
                                .unwrap_or(0)
                        };
                        let (packed, missing) = (n("packed"), n("missing"));
                        let msg = if missing > 0 {
                            format!("已导出 {packed} 个文件（缺失引用 {missing} 个）")
                        } else {
                            format!("已导出 {packed} 个文件")
                        };
                        state.notify(ToastKind::Success, msg);
                    }
                    Err(e) => state.notify(ToastKind::Error, format!("导出失败: {e}")),
                }
            },
        );
    }

    /// 导入方案包：选 zip → `scheme.previewImport` 预览 → 确认对话框选合并/替换。
    pub fn do_import_package(&self, ctx: &mut EventCtx) {
        let state = self.clone();
        ctx.request_pick_file(
            PickDialog::new()
                .title("导入方案包")
                .filter("方案包", &["zip"]),
            move |path| {
                let Some(path) = path else { return };
                match rpc::call_core(
                    "scheme.previewImport",
                    &json!({ "path": path.to_string_lossy() }),
                ) {
                    Ok(v) => {
                        state
                            .import_preview_msg
                            .set(Self::format_import_preview(&v));
                        *state.pending_import_path.borrow_mut() = Some(path);
                        state.import_confirm_visible.set(true);
                    }
                    Err(e) => state.notify(ToastKind::Error, format!("预览失败: {e}")),
                }
            },
        );
    }

    /// previewImport 响应 → 确认对话框正文（纯函数，便于单测）。
    fn format_import_preview(v: &Value) -> String {
        let n = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_array())
                .map(|a| a.len())
                .unwrap_or(0)
        };
        let created = v
            .get("manifest")
            .and_then(|m| m.get("created_at"))
            .and_then(|x| x.as_str())
            .unwrap_or("未知");
        format!(
            "创建于 {created}\n新增文件 {} · 已存在 {} · 系统引用 {} · 缺失 {}\n\n\
             合并跳过已存在文件；替换覆盖同名文件。",
            n("willAdd"),
            n("conflicts"),
            n("systemRefs"),
            n("missing"),
        )
    }

    /// 确认导入（strategy: "merge" | "replace"）。
    pub fn confirm_import(&self, strategy: &str) {
        self.import_confirm_visible.set(false);
        let Some(path) = self.pending_import_path.borrow_mut().take() else {
            return;
        };
        match rpc::call_core(
            "scheme.importPackage",
            &json!({ "path": path.to_string_lossy(), "strategy": strategy }),
        ) {
            Ok(v) => {
                let n = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0)
                };
                let (imported, conflicts) = (n("imported"), n("conflicts"));
                self.refresh();
                let msg = if conflicts > 0 {
                    format!("已导入 {imported} 个文件（跳过已存在 {conflicts} 个）")
                } else {
                    format!("已导入 {imported} 个文件")
                };
                self.notify(ToastKind::Success, msg);
            }
            Err(e) => self.notify(ToastKind::Error, format!("导入失败: {e}")),
        }
    }

    pub fn cancel_import(&self) {
        *self.pending_import_path.borrow_mut() = None;
        self.import_confirm_visible.set(false);
    }
}

// ──────────────────────── 显示辅助 ────────────────────────

/// 沿路径逐层确保对象存在后写入叶子值（中间层非对象时覆盖为对象；根非对象则不写）。
fn set_json_path(v: &mut Value, path: &[&str], leaf: Value) {
    let Some((last, mids)) = path.split_last() else {
        return;
    };
    let mut cur = v;
    for k in mids {
        let Some(obj) = cur.as_object_mut() else {
            return;
        };
        let slot = obj.entry((*k).to_string()).or_insert_with(|| json!({}));
        if !slot.is_object() {
            *slot = json!({});
        }
        cur = slot;
    }
    if let Some(o) = cur.as_object_mut() {
        o.insert((*last).to_string(), leaf);
    }
}

pub fn engine_label(t: &str) -> &'static str {
    match t {
        "codetable" => "码表",
        "pinyin" => "拼音",
        "mixed" => "混输",
        _ => "未知",
    }
}

/// 附加词库解析结果（纯数据，供 UI 层包装为响应式行）。
pub struct DictInfo {
    pub id: String,
    pub label: String,
    pub description: String,
    pub enabled: bool,
}

/// 从方案配置 `cfg["dictionaries"]` 解析可开关的附加词库（过滤主库）。
/// 生效值：enabled.unwrap_or(default_enabled.unwrap_or(true))。
fn parse_extension_dicts(cfg: &Value) -> Vec<DictInfo> {
    let Some(arr) = cfg.get("dictionaries").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter(|d| !d.get("default").and_then(|v| v.as_bool()).unwrap_or(false))
        .map(|d| {
            let default_enabled = d
                .get("default_enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let enabled = d
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(default_enabled);
            DictInfo {
                id: d
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                label: d
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                description: d
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                enabled,
            }
        })
        .collect()
}

// ──────────────────────── 内联复合列表 ────────────────────────

/// 构建内联方案列表，供 `scheme_page` 直接嵌入卡片。
///
/// 使用 `Element::host_signal`（非滚动的响应式 col）驱动：启用/禁用/排序变更后
/// list_data 更新，列表即时重建。列表**按自然高度展开**，随外层页面滚动一起滚动
/// ——刻意不用 `list_signal`（自带固定高度滚动容器）：设置页本身已是滚动容器，
/// 内嵌固定高度的内层滚动会造成"滚轮在内外层间打架、列表滚不动"，且内层滚动条
/// 压在卡片行上遮挡内容。改为自然高度后只保留页面这一层滚动，两个问题一并消除。
pub fn build_schema_inline_list(state: &SchemaManagerState) -> Element {
    let mgr = state.clone();
    let hide_sig = state.hide_disabled;

    Element::host_signal(state.list_data, move |item| match item {
        SchemaListItem::Row { entry, is_enabled } => build_compound_row(entry, is_enabled, &mgr),
        SchemaListItem::SectionHeader { count } => Element::col()
            .width_match()
            .child(Element::divider())
            .child(
                Element::row().width_match().padding_xy(8, 5).child(
                    Element::label(format!("其他已安装方案（共 {count} 个）"))
                        .font_size(10.5)
                        .fg_role(Role::TextMuted)
                        .width_match()
                        .max_lines(1),
                ),
            )
            .visible_when(move || !hide_sig.get()),
        SchemaListItem::Divider {
            hidden_with_disabled,
        } => {
            let d = Element::divider();
            if hidden_with_disabled {
                d.visible_when(move || !hide_sig.get())
            } else {
                d
            }
        }
    })
    .width_match()
}

/// 构建单个方案复合行（工具栏+选中模式：行内无操作按钮）。
///
/// 行整体 clickable → 点击选中（事件沿祖先链派发，点 checkbox 会同时切换启用并选中，
/// 符合预期）；选中视觉 = stack 底层 accent 淡底 + 左缘指示条覆盖层（与侧栏导航同语言），
/// visible_when 轮询 `sel_ver`。行内保留：启用复选 · 名称/描述 · 类型/来源徽章 · 版本。
fn build_compound_row(entry: SchemaEntry, is_enabled: bool, mgr: &SchemaManagerState) -> Element {
    let enabled_sig = signal(is_enabled);

    // ── Checkbox ──
    let mgr_toggle = mgr.clone();
    let toggle_id = entry.id.clone();
    let checkbox = Element::checkbox("", enabled_sig)
        .small()
        .on_toggle(move |_| {
            let new_val = !enabled_sig.get();
            // 被拒绝（最后一个启用项不可禁用）时不写 signal，勾选态保持不变。
            if mgr_toggle.toggle_schema_enabled(&toggle_id, new_val) {
                enabled_sig.set(new_val);
            }
        });

    // ── 名称列（weight-1）：名称 + 可选描述；双拼方案名称后缀当前布局（如「双拼·小鹤双拼」）──
    let base_name = if entry.name.is_empty() {
        entry.id.clone()
    } else {
        entry.name.clone()
    };
    let name_text = if entry.scheme == "shuangpin" && !entry.shuangpin_layout.is_empty() {
        format!("{base_name}·{}", mgr.layout_label(&entry.shuangpin_layout))
    } else {
        base_name
    };
    let name_fg = if is_enabled {
        Role::Text
    } else {
        Role::TextMuted
    };
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
    let source_badge = Element::label(if entry.source == "user" {
        "用户"
    } else {
        "内置"
    })
    .font_size(10.5)
    .fg_role(Role::TextMuted)
    .padding_xy(5, 2)
    .corner(4.0)
    .bg_role(Role::SurfaceAlt)
    .border_role(Role::Border, 1);

    // ── 行内容：点击选中 ──
    let mgr_sel = mgr.clone();
    let sel_id = entry.id.clone();
    let mut row = Element::row()
        .clickable()
        .on_click(move |_| mgr_sel.select(&sel_id))
        .width_match()
        .cross(Align::Center)
        .padding_xy(10, 8)
        .spacing(6)
        .corner(8.0)
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

    // ── 选中视觉：淡蓝底 + accent 整条描边（置于行下，行 clickable 吞命中不受影响）
    //    + 左缘指示条（无背景穿透容器包裹，垂直居中）。
    //    stack 子节点铺满须用 .align(Align::Stretch)（Frame 布局只认 align，
    //    width/height_match 在此测不出父尺寸——上一版指示条不居中即此因）。
    let sel_when = |m: &SchemaManagerState, id: &str| {
        let m = m.clone();
        let id = id.to_string();
        move || {
            m.sel_ver.get();
            m.is_selected(&id)
        }
    };
    let sel_bg = Element::leaf()
        .align(Align::Stretch)
        .corner(8.0)
        .bg(Color::hex(crate::theme_def::ACCENT_LIGHT).scale_alpha(0.08))
        .visible_when(sel_when(mgr, &entry.id));
    let sel_border = Element::leaf()
        .align(Align::Stretch)
        .corner(8.0)
        .border_role(Role::Accent, 1)
        .visible_when(sel_when(mgr, &entry.id));
    let indicator = Element::row()
        .align(Align::Stretch)
        .cross(Align::Center)
        .child(
            Element::leaf()
                .width(3)
                .height(16)
                .corner(1.5)
                .bg_role(Role::Accent),
        )
        .visible_when(sel_when(mgr, &entry.id));

    // 两侧留 2px：选中描边贴列表容器边缘会被裁/被滚动条压住右缘。
    let mut item = Element::stack()
        .width_match()
        .margin_xy(2, 0)
        .child(sel_bg)
        .child(sel_border)
        .child(row)
        .child(indicator);

    // 未启用行随 hide_disabled 隐藏（list_signal 重建后无需 enabled_sig 联动）
    if !is_enabled {
        let hide_sig = mgr.hide_disabled;
        item = item.visible_when(move || !hide_sig.get());
    }

    item
}

// ──────────────────────── UI 构建 ────────────────────────

/// 构建方案相关浮层对话框：删除确认 + 方案详情/设置。
pub fn build_schema_manager_dialog(state: &SchemaManagerState) -> Element {
    let confirm_dlg = build_confirm_dialog(state);
    let settings_dlg = build_schema_settings_dialog(state);
    let import_dlg = build_import_confirm_dialog(state);
    Element::stack()
        .fill()
        .child(confirm_dlg)
        .child(settings_dlg)
        .child(import_dlg)
}

/// 方案包导入确认：previewImport 摘要正文 + 合并（默认）/ 替换（danger）二选一。
fn build_import_confirm_dialog(state: &SchemaManagerState) -> Element {
    let merge = state.clone();
    let replace = state.clone();
    let cancel = state.clone();
    let content = flat_panel()
        .width(400)
        .padding(20)
        .spacing(14)
        .child(
            Element::label("导入方案包")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(24)
                .width_match(),
        )
        .child(Element::divider())
        .child(
            Element::label_rc(state.import_preview_msg)
                .font_size(13.0)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(6),
        )
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
                        .on_click(move |_| cancel.cancel_import()),
                )
                .child(
                    Element::button("替换导入")
                        .small()
                        .danger()
                        .on_click(move |_| replace.confirm_import("replace")),
                )
                .child(
                    Element::button("合并导入")
                        .small()
                        .on_click(move |_| merge.confirm_import("merge")),
                ),
        );
    Element::dialog(state.import_confirm_visible, content)
}

fn build_confirm_dialog(state: &SchemaManagerState) -> Element {
    let st_confirm = state.clone();
    let st_cancel = state.clone();

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
                        .outline_soft()
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

/// 按分节类型构建对话框内的一个配置区块。新增方案设置只需在此加一个 match 分支。
/// 方案设置对话框的结构化头部：标题（名称 + 类型/来源徽章）+ 元信息行 + 描述 + 分隔线。
/// 取代原先 `\n` 拼接塞进单 label 的杂乱纯文本块。
fn build_settings_header(entry: &SchemaEntry) -> Element {
    let name = if entry.name.is_empty() {
        entry.id.clone()
    } else {
        entry.name.clone()
    };

    let type_badge = Element::label(engine_label(&entry.engine_type))
        .font_size(10.5)
        .fg_role(Role::Accent)
        .padding_xy(6, 2)
        .corner(4.0)
        .border_role(Role::Accent, 1);
    let source_badge = Element::label(if entry.source == "user" {
        "用户"
    } else {
        "内置"
    })
    .font_size(10.5)
    .fg_role(Role::TextMuted)
    .padding_xy(6, 2)
    .corner(4.0)
    .bg_role(Role::SurfaceAlt)
    .border_role(Role::Border, 1);

    let title = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::label(name)
                .font_size(16.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .weight(1.0)
                .max_lines(1)
                .truncate(Truncate::End),
        )
        .child(type_badge)
        .child(source_badge);

    // 元信息行：版本 · 作者 · ID（仅拼接存在项）。
    let mut parts: Vec<String> = Vec::new();
    if !entry.version.is_empty() {
        parts.push(format!("版本 {}", entry.version));
    }
    if !entry.author.is_empty() {
        parts.push(format!("作者 {}", entry.author));
    }
    parts.push(format!("ID {}", entry.id));
    let meta = Element::label(parts.join("　·　"))
        .font_size(11.0)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(1)
        .truncate(Truncate::End);

    let mut col = Element::col()
        .width_match()
        .spacing(6)
        .child(title)
        .child(meta);
    if !entry.description.is_empty() {
        col = col.child(
            Element::label(entry.description.clone())
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3),
        );
    }
    col.child(Element::divider())
}

fn build_section(kind: SettingsSection, state: &SchemaManagerState) -> Element {
    match kind {
        SettingsSection::Header => {
            let entry = state.settings_entry.borrow().clone().unwrap_or_default();
            build_settings_header(&entry)
        }
        SettingsSection::ShuangpinLayout => {
            let labels: Vec<String> = state
                .layouts
                .borrow()
                .iter()
                .map(|(_, name)| name.clone())
                .collect();
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(12)
                .padding_xy(0, 8)
                .child(
                    Element::label("双拼布局")
                        .font_size(13.0)
                        .fg_role(Role::Text)
                        .weight(1.0)
                        .max_lines(1),
                )
                .child(Element::dropdown(labels, state.settings_layout).width(170))
        }
        SettingsSection::ExtensionDicts => {
            let mut col = Element::col()
                .width_match()
                .spacing(4)
                .padding_xy(0, 4)
                .child(
                    Element::label("扩展词库")
                        .font_size(12.0)
                        .font_weight(600)
                        .fg_role(Role::TextMuted)
                        .width_match()
                        .padding_xy(0, 2),
                );
            for row in state.settings_dicts.borrow().iter() {
                // 默认点击即翻转绑定的 on 信号（暂存）；实际提交在「保存」按钮统一进行。
                let cb = Element::checkbox(row.label.clone(), row.on);
                let mut item = Element::col()
                    .width_match()
                    .spacing(2)
                    .padding_xy(0, 4)
                    .child(cb);
                if !row.description.is_empty() {
                    item = item.child(
                        Element::label(row.description.clone())
                            .font_size(11.0)
                            .fg_role(Role::TextMuted)
                            .width_match()
                            .padding_xy(24, 0)
                            .max_lines(2),
                    );
                }
                col = col.child(item);
            }
            col
        }
        SettingsSection::Empty => Element::label("该方案暂无可配置项。")
            .font_size(11.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .padding_xy(0, 4),
    }
}

fn build_schema_settings_dialog(state: &SchemaManagerState) -> Element {
    let visible = state.settings_visible;
    let has_save = state.settings_has_save;

    // 配置区：按 settings_sections 响应式重建（每次打开对话框按方案类型刷新）；
    // Header 恒为首节，元信息与各配置节统一由此渲染。
    let st_body = state.clone();
    let sections = Element::host_signal(state.settings_sections, move |kind| {
        build_section(kind, &st_body)
    });

    let body = Element::col().width_match().spacing(6).child(sections);

    let st_save = state.clone();
    let footer = Element::row()
        .width_match()
        .spacing(8)
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("关闭")
                .small()
                .outline_soft()
                .on_click(move |_| visible.set(false)),
        )
        .child(
            Element::button("保存")
                .small()
                .on_click(move |_| st_save.save_settings())
                .visible_when(move || has_save.get()),
        );

    Element::dialog_panel(
        visible,
        "方案设置",
        440,
        move |_| visible.set(false),
        body,
        footer,
    )
}

#[cfg(test)]
mod ext_dict_tests {
    use super::*;

    #[test]
    fn scheme_import_preview_msg() {
        let v = json!({
            "manifest": { "created_at": "2026-07-13T00:00:00+08:00" },
            "willAdd": ["a", "b"], "conflicts": ["c"], "systemRefs": [], "missing": []
        });
        let msg = SchemaManagerState::format_import_preview(&v);
        assert!(msg.contains("2026-07-13"));
        assert!(msg.contains("新增文件 2"));
        assert!(msg.contains("已存在 1"));
    }

    #[test]
    fn parse_filters_main_and_resolves_enabled() {
        let cfg = json!({ "dictionaries": [
            { "id": "m",  "label": "主库", "default": true },
            { "id": "a",  "label": "扩展A", "description": "描述A", "default_enabled": true },
            { "id": "b",  "label": "扩展B", "default_enabled": false },
            { "id": "c",  "label": "扩展C", "default_enabled": true, "enabled": false },
            { "id": "d",  "label": "扩展D" }
        ]});
        let rows = parse_extension_dicts(&cfg);
        // 主库被过滤，剩 4 条附加库。
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c", "d"]
        );
        assert!(rows[0].enabled, "a: default_enabled=true");
        assert!(!rows[1].enabled, "b: default_enabled=false");
        assert!(
            !rows[2].enabled,
            "c: enabled=false 覆盖 default_enabled=true"
        );
        assert!(rows[3].enabled, "d: 无字段 → 默认 true");
        assert_eq!(rows[0].description, "描述A");
    }

    #[test]
    fn parse_missing_dictionaries_is_empty() {
        assert!(parse_extension_dicts(&json!({})).is_empty());
        assert!(parse_extension_dicts(&json!({ "dictionaries": [] })).is_empty());
    }
}
