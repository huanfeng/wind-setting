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
/// 字段加载器：把某 Config（或空 Config→取默认值）灌回对应 Signal。
/// 供「重新加载」（从服务端配置回填）与「恢复本页」（空 Config→manifest 默认值）使用。
pub type FieldLoader = Box<dyn Fn(&Config)>;

/// 字段注册表 — 清单引擎登记的保存写回器、联动取值器与回填加载器。
#[derive(Default)]
pub struct FieldRegistry {
    /// 保存时把各 Signal 值写回 Config。
    pub writers: RefCell<Vec<FieldWriter>>,
    /// key → 当前值取值器（供 enabled_when 联动）。
    pub getters: RefCell<HashMap<String, ValueGetter>>,
    /// key → 加载器（把给定 Config 的值灌回 Signal；空 Config 时回落 manifest 默认）。
    pub loaders: RefCell<HashMap<String, FieldLoader>>,
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
    /// 「重新加载」二次确认对话框可见标志。
    pub reload_confirm: Signal<bool>,
    /// 「恢复本页」（有未保存改动时）确认对话框可见标志。
    pub reset_confirm: Signal<bool>,
    /// 启动时是否成功连接到输入法服务（core）。未连接则设置只读、保存会失败。
    pub connected: bool,
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
        // 探测 core 连接：未连接时明确提示（设置仅预览，保存将失败）。
        let connected = rpc::probe_core_running();
        let init_status = if connected {
            "已连接".to_string()
        } else {
            "未连接到输入法服务 · 设置仅供预览".to_string()
        };

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
            status: signal(init_status),
            schema_mgr: SchemaManagerState::new(),
            reload_confirm: signal(false),
            reset_confirm: signal(false),
            connected,
        }
    }

    /// 是否存在未保存改动：跑写回器到 current 的副本上，与 base 比对有无差异。
    pub fn is_dirty(&self) -> bool {
        let mut probe = self.current_config.clone();
        for w in self.fields.writers.borrow().iter() {
            w(&mut probe);
        }
        !diff_config(&self.base_config, &probe).is_empty()
    }

    /// 当前 Tab 对应的 manifest 分组（用于「恢复本页」定位 key 集合）。
    /// 与 `pages::all_tabs` 顺序一致；自定义页（方案/词库/统计/关于）无清单分组。
    fn current_tab_groups(&self) -> &'static [&'static str] {
        match self.tab.get() {
            0 => &["schema"],
            1 => &["input", "general"],
            2 => &["hotkeys"],
            3 => &["ui"],
            6 => &["advanced"],
            _ => &[],
        }
    }

    /// 恢复本页为系统默认值：用空 Config 跑当前页各 key 的加载器（回落 manifest 默认）。
    /// 仅改 Signal（控件即时显示默认值），不落盘——用户再「应用设置」持久化。
    pub fn reset_current_page(&self) {
        let empty = Config::from_value(Value::Object(serde_json::Map::new()));
        let loaders = self.fields.loaders.borrow();
        let mut n = 0usize;
        for g in self.current_tab_groups() {
            for (_sec, keys) in self.manifest.sections_of(g) {
                for k in keys {
                    if let Some(l) = loaders.get(&k) {
                        l(&empty);
                        n += 1;
                    }
                }
            }
        }
        self.status.set(if n == 0 {
            "本页无可恢复的默认项".to_string()
        } else {
            format!("已恢复本页 {n} 项为默认值（未保存）")
        });
    }

    /// 从服务端重新加载配置：丢弃未保存改动，重新读取并灌回所有 Signal。
    pub fn reload(&mut self) {
        let fresh = rpc::call_core("config.get", &serde_json::json!({}))
            .unwrap_or(Value::Object(serde_json::Map::new()));
        let cfg = Config::from_value(fresh);
        {
            let loaders = self.fields.loaders.borrow();
            for l in loaders.values() {
                l(&cfg);
            }
        }
        self.base_config = cfg.clone();
        self.current_config = cfg;
        self.status.set("已从服务端重新加载".to_string());
    }

    /// 跑所有写回器把 Signal 值写回 current_config，再 diff 保存。
    /// 返回是否「可视为成功」（无错误）：无变更与保存成功均为 true，仅 RPC 报错为 false。
    /// 供「保存&退出」据此决定是否真的退出。
    pub fn save(&mut self) -> bool {
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
            return true;
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
                true
            }
            Err(e) => {
                self.status.set(format!("保存失败: {e}"));
                false
            }
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
