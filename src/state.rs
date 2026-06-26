//! 全局状态 — 配置加载、清单、字段注册表、保存。
//!
//! 状态绑定改用 wind-ui-rust 的 `Signal`（取代旧 `Rc<Cell>`）。清单引擎
//! [`crate::manifest::field`] 为每个 key 建一个 Signal 并把"写回器/取值器"
//! 登记进 [`FieldRegistry`]；[`AppState::save`] 跑所有写回器把 Signal 值写回
//! Config，再与 base 做 diff，增量 `config.setItems`。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::Value;
use windui::prelude::*;

use crate::config::{Config, diff_config};
use crate::dialogs::schema_manager::SchemaManagerState;
use crate::manifest::Manifest;
use crate::rpc;

/// 一个方案条目（schema.list 返回）。
#[derive(Clone, Default)]
pub struct SchemaInfo {
    pub id: String,
    pub name: String,
    /// 类型（codetable / pinyin / ...），用于主方案下拉过滤；缺省为空。
    pub kind: String,
}

/// 保存写回器：把某 Signal 值写回 Config。
pub type FieldWriter = Box<dyn Fn(&mut Config)>;
/// 当前值取值器：供 enabled_when 联动读取某 key 的当前值。
pub type ValueGetter = Box<dyn Fn() -> Value>;

/// 字段注册表 — 清单引擎登记的保存写回器与联动取值器。
#[derive(Default)]
pub struct FieldRegistry {
    /// 保存时把各 Signal 值写回 Config。
    pub writers: RefCell<Vec<FieldWriter>>,
    /// key → 当前值取值器（供 enabled_when 联动）。
    pub getters: RefCell<HashMap<String, ValueGetter>>,
}

/// 应用全局状态。
pub struct AppState {
    /// 从 core 加载的原始配置（用于 diff 基线）。
    pub base_config: Config,
    /// 当前编辑中的配置（保存时由写回器填充）。
    pub current_config: Config,
    /// 设置清单（system.manifest）。
    pub manifest: Manifest,
    /// 系统信息（system.info）。
    pub system_info: Value,
    /// 方案列表（schema.list）。
    pub schemas: Vec<SchemaInfo>,
    /// 主题名称列表（theme.list）。
    pub theme_names: Vec<String>,
    /// 系统字体列表（system.fonts）。
    pub fonts: Vec<String>,
    /// 字段注册表（清单引擎登记）。
    pub fields: Rc<FieldRegistry>,
    /// 当前 Tab 索引。
    pub tab: Signal<usize>,
    /// 暗色模式（设置窗口自身明暗，运行时切换）。
    pub dark: Signal<bool>,
    /// 状态文字（标题栏右侧反馈）。
    pub status: Signal<String>,
    /// 方案管理对话框状态。
    pub schema_mgr: SchemaManagerState,
}

impl AppState {
    /// 从 core 加载配置、清单与各列表。
    pub fn load() -> Self {
        let config_value = rpc::call_core("config.get", &serde_json::json!({}))
            .unwrap_or(Value::Object(serde_json::Map::new()));
        let system_info = rpc::call_core("system.info", &serde_json::json!({}))
            .unwrap_or(Value::Object(serde_json::Map::new()));
        let manifest_value = rpc::call_core("system.manifest", &serde_json::json!({}))
            .unwrap_or(Value::Object(serde_json::Map::new()));
        let manifest = Manifest::from_value(&manifest_value);

        let schemas = load_schemas();
        let theme_names = load_theme_names();
        let fonts = load_fonts();

        Self {
            base_config: Config::from_value(config_value.clone()),
            current_config: Config::from_value(config_value),
            manifest,
            system_info,
            schemas,
            theme_names,
            fonts,
            fields: Rc::new(FieldRegistry::default()),
            tab: signal(0),
            // 默认主题随系统（不持久化用户切换，后续外观设置项另行处理）。
            dark: signal(crate::theme_def::system_dark()),
            status: signal(String::from("就绪")),
            schema_mgr: SchemaManagerState::new(),
        }
    }

    /// 跑所有写回器把 Signal 值写回 current_config，再 diff 保存。
    pub fn save(&mut self) {
        let reg = self.fields.clone();
        {
            let writers = reg.writers.borrow();
            for w in writers.iter() {
                w(&mut self.current_config);
            }
        }
        let items = diff_config(&self.base_config, &self.current_config);
        if items.is_empty() {
            self.status.set("没有变更".to_string());
            return;
        }
        let items_json: Vec<Value> = items
            .iter()
            .map(|it| serde_json::json!({ "key": it.key, "value": it.value }))
            .collect();
        let params = serde_json::json!({ "items": items_json });
        match rpc::call_core("config.setItems", &params) {
            Ok(_) => {
                self.base_config = self.current_config.clone();
                self.status
                    .set(format!("已保存 {} 项变更", items_json.len()));
            }
            Err(e) => self.status.set(format!("保存失败: {e}")),
        }
    }
}

/// 加载方案列表（schema.list）。
fn load_schemas() -> Vec<SchemaInfo> {
    let list =
        rpc::call_core("schema.list", &serde_json::json!({})).unwrap_or(Value::Array(vec![]));
    list.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let id = s.get("id")?.as_str()?.to_string();
                    let name = s
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&id)
                        .to_string();
                    let kind = s
                        .get("kind")
                        .or_else(|| s.get("type"))
                        .or_else(|| s.get("category"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(SchemaInfo { id, name, kind })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 加载主题名称列表（theme.list）。
fn load_theme_names() -> Vec<String> {
    let list = rpc::call_core("theme.list", &serde_json::json!({})).unwrap_or(Value::Array(vec![]));
    list.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("name").and_then(|v| v.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 加载系统字体列表（system.fonts）。
fn load_fonts() -> Vec<String> {
    let list =
        rpc::call_core("system.fonts", &serde_json::json!({})).unwrap_or(Value::Array(vec![]));
    list.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    f.get("name")
                        .or_else(|| f.get(0))
                        .and_then(|v| v.as_str())
                        .or_else(|| f.as_str())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default()
}
