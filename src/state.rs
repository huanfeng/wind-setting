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

use crate::capabilities::Capabilities;
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
    /// dialog_button 的对话框主体。须挂到**根窗口** stack 才能正确全窗模态遮罩；
    /// 若内联在设置行的小 stack 内，`Element::dialog` 的遮罩只填满那一格，表现为
    /// 「显示在行下方」而非居中模态。构建页面时登记，由 `shell::build` 取出挂到根。
    pub dialogs: RefCell<Vec<Element>>,
    /// 全局热键收集器：(功能短名, 是否全局 Signal)。支持全局的功能键各登记一项，
    /// 由 `register_global_hotkeys_collector` 汇总成 `keys.global_hotkeys` strlist 写回/回填
    /// （多个功能键写同一 key，故需单一收集器，模式同 checkbox_group）。
    pub global_flags: RefCell<Vec<(String, Signal<bool>)>>,
}

/// 应用全局状态。
pub struct AppState {
    /// 从 core 加载的原始配置（用于 diff 基线）。
    pub base_config: Config,
    /// 当前编辑中的配置（保存时由写回器填充）。
    pub current_config: Config,
    /// 设置清单（内嵌清单，启动即就位）。
    pub manifest: Manifest,
    /// core 能力契约（system.capabilities，异步加载后就位）。
    pub capabilities: Capabilities,
    /// 系统信息（system.info）。
    pub system_info: Value,
    /// 方案列表（schema.list）。
    pub schemas: Vec<SchemaInfo>,
    /// 主题列表（theme.list）：(id/slug, 显示名)。下拉显示名、保存 id 到 `ui.theme.name`。
    pub theme_names: Vec<(String, String)>,
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
    /// 正式模式下连接失败标志：true 时显示阻断对话框；mock 模式始终 false。
    pub conn_failed: Signal<bool>,
    /// 连接成功的响应式标志（异步加载后置位）。底栏状态圆点据此响应式着色——
    /// 构建时尚未连接（empty），故圆点必须用 Signal 而非构建期 bool 才能在连接后转绿。
    pub conn_ok: Signal<bool>,
    /// 主题下拉的响应式选项：显示名列表 + 对应 id/slug 值列表（异步加载后填充）。
    /// 下拉构建于异步数据到达前，故必须经 Signal 驱动响应式下拉，数据到达即填充。
    pub theme_label_sig: Signal<Vec<String>>,
    pub theme_value_sig: Signal<Vec<String>>,
    /// 候选窗字体下拉的响应式选项（system.fonts）。
    /// value：空字符串 = 系统默认，其余为字体族名；label：首项"默认"，其余同值。
    pub font_sig: Signal<Vec<String>>,
    pub font_label_sig: Signal<Vec<String>>,
}

impl AppState {
    /// 快速初始化：仅读系统主题，其余字段置空。
    /// UI 可立即显示；真实数据由后台线程通过 [`AppState::apply_loaded`] 写入。
    pub fn empty() -> Self {
        Self {
            base_config: Config::from_value(Value::Object(serde_json::Map::new())),
            current_config: Config::from_value(Value::Object(serde_json::Map::new())),
            manifest: Manifest::from_embedded(),
            capabilities: Capabilities::from_embedded(),
            system_info: Value::Object(serde_json::Map::new()),
            schemas: vec![],
            theme_names: vec![],
            fonts: vec![],
            fields: Rc::new(FieldRegistry::default()),
            tab: signal(0),
            dark: signal(crate::theme_def::system_dark()),
            status: signal("正在连接服务...".to_string()),
            schema_mgr: SchemaManagerState::new(),
            reload_confirm: signal(false),
            reset_confirm: signal(false),
            connected: false,
            conn_failed: signal(false),
            conn_ok: signal(false),
            theme_label_sig: signal(vec![]),
            theme_value_sig: signal(vec![]),
            font_sig: signal(vec![]),
            font_label_sig: signal(vec![]),
        }
    }

    /// 后台数据就绪后在 UI 线程调用：更新非 Signal 字段，并通过 Signal 通知界面刷新。
    ///
    /// 主题/字体下拉经响应式选项信号（theme_label_sig/font_sig 等）填充，异步数据到达即刷新；
    /// 其余 manifest 字段经已注册加载器回填 Signal。
    pub fn apply_loaded(&mut self, data: LoadedState) {
        let cfg = Config::from_value(data.config_value);
        // 先填充列表数据与响应式下拉选项 —— 必须早于 loader：themes 的 loader 需按配置值在
        // 选项列表中定位选中索引，故选项信号要在 loader 跑之前就位。
        self.schemas = data.schemas;
        self.theme_names = data.theme_names;
        self.fonts = data.fonts;
        self.theme_label_sig.set(self.theme_names.iter().map(|(_, n)| n.clone()).collect());
        self.theme_value_sig.set(self.theme_names.iter().map(|(id, _)| id.clone()).collect());
        // value：空字符串 = 使用默认字体；label：首项显示"默认"。
        self.font_sig.set(
            std::iter::once(String::new())
                .chain(self.fonts.iter().cloned())
                .collect(),
        );
        self.font_label_sig.set(
            std::iter::once("默认".to_string())
                .chain(self.fonts.iter().cloned())
                .collect(),
        );
        // 注：选项变更后下拉的宽度 re-measure 依赖"数据到达帧为整窗重绘"——本方法在 pump 内
        // set 信号会请求整窗重绘，启动场景(无进行中的局部动画)该帧即整窗重排，宽度正常刷新；
        // 这是 windui pump→signal→repaint 既有性质（现有字段 loader 的文本刷新同此机制）。
        // 再重新填充已注册字段的 Signal（首次加载时 manifest 为空、无 loader，是空循环）。
        {
            let loaders = self.fields.loaders.borrow();
            for l in loaders.values() {
                l(&cfg);
            }
        }
        self.base_config = cfg.clone();
        self.current_config = cfg;
        self.capabilities = Capabilities::from_value(&data.capabilities_value);
        self.system_info = data.system_info;
        self.connected = data.connected;
        self.conn_ok.set(data.connected); // 响应式：底栏圆点连接后转绿
        if data.connected {
            self.status.set("已连接".to_string());
            self.conn_failed.set(false);
        } else if rpc::is_mock() {
            self.status.set("离线预览（mock）".to_string());
            self.conn_failed.set(false);
        } else {
            self.status.set("未连接到输入法服务".to_string());
            self.conn_failed.set(true);
        }
        tracing::info!("apply_loaded 完成, connected={}", data.connected);
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
    /// 与 `pages::all_tabs` 顺序一致（统计页已隐藏）：方案0/输入1/按键2/外观3/词库4/高级5/关于6。
    /// 自定义页（词库/关于）无清单分组。
    fn current_tab_groups(&self) -> &'static [&'static str] {
        match self.tab.get() {
            0 => &["schema"],
            1 => &["input", "general"],
            2 => &["keys"],
            3 => &["ui"],
            5 => &["advanced"],
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
        // keys.global_hotkeys 非清单项（由功能键「全局」勾选经收集器登记），手动纳入「按键」页恢复。
        if self.current_tab_groups().contains(&"keys")
            && let Some(l) = loaders.get("keys.global_hotkeys")
        {
            l(&empty);
            n += 1;
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

/// 后台线程从 core 拉取的全部数据（所有字段均 `Send`，可跨线程传递）。
pub struct LoadedState {
    pub config_value: Value,
    pub system_info: Value,
    pub capabilities_value: Value,
    pub schemas: Vec<SchemaInfo>,
    pub theme_names: Vec<(String, String)>,
    pub fonts: Vec<String>,
    pub connected: bool,
}

impl LoadedState {
    fn empty_obj() -> Value {
        Value::Object(serde_json::Map::new())
    }

    /// 并行发起全部 RPC 调用（各自独立短连接），wall-clock = max(各调用延迟)。
    /// 管道不存在时 fast-fail（无重试），整体几乎立即返回。
    pub fn fetch() -> Self {
        let t0 = std::time::Instant::now();

        // 6 路并行：函数项自动满足 Send + 'static，直接传递无需 move 闭包。
        let h_config  = std::thread::spawn(|| rpc::call_core("config.get",          &serde_json::json!({})));
        let h_info    = std::thread::spawn(|| rpc::call_core("system.info",         &serde_json::json!({})));
        let h_caps    = std::thread::spawn(|| rpc::call_core("system.capabilities", &serde_json::json!({})));
        let h_schemas = std::thread::spawn(load_schemas);
        let h_themes  = std::thread::spawn(load_theme_names);
        let h_fonts   = std::thread::spawn(load_fonts);

        let config_res  = h_config.join().ok().and_then(|r| r.ok());
        let system_info = h_info.join().ok().and_then(|r| r.ok()).unwrap_or_else(Self::empty_obj);
        let caps_res    = h_caps.join().ok().and_then(|r| r.ok());
        let schemas     = h_schemas.join().unwrap_or_default();
        let theme_names = h_themes.join().unwrap_or_default();
        let fonts       = h_fonts.join().unwrap_or_default();

        // config_value 是否为 Some 直接反映管道连接是否成功。
        let connected         = config_res.is_some();
        let config_value      = config_res.unwrap_or_else(Self::empty_obj);
        let capabilities_value = caps_res.unwrap_or_else(Self::empty_obj);

        tracing::info!(
            "LoadedState::fetch 完成 (connected={connected}, 并行耗时: {:?})",
            t0.elapsed()
        );

        Self { config_value, system_info, capabilities_value, schemas, theme_names, fonts, connected }
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

/// 加载主题列表（theme.list）：返回 (id/slug, 显示名) 对。
/// id 取 `id`/`slug`（保存到 `ui.theme.name`），显示名取 `name`/`display_name`（缺省回退 id）。
fn load_theme_names() -> Vec<(String, String)> {
    let result = rpc::call_core("theme.list", &serde_json::json!({}));
    match &result {
        Ok(v) => tracing::info!("theme.list RPC 返回: {v}"),
        Err(e) => tracing::warn!("theme.list RPC 失败: {e}"),
    }
    let list = result.unwrap_or(Value::Array(vec![]));
    list.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    // core 协议：name = slug/id，display_name = 界面显示名
                    let id = t
                        .get("id")
                        .or_else(|| t.get("slug"))
                        .or_else(|| t.get("name"))
                        .and_then(|v| v.as_str())?
                        .to_string();
                    let display = t
                        .get("display_name")
                        .or_else(|| t.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(&id);
                    let ver = t.get("version").and_then(|v| v.as_str()).unwrap_or("");
                    let name = if ver.is_empty() {
                        display.to_string()
                    } else {
                        format!("{display} v{ver}")
                    };
                    Some((id, name))
                })
                .collect()
        })
        .unwrap_or_default()
}

// 测试模块后仍有 load_schemas 等私有辅助函数；局部豁免 toolchain 新增的风格 lint。
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;

    /// 所有涉及 WIND_RPC_MOCK 环境变量的测试必须持有此锁，防止并行 race。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn loaded_state_uses_embedded_manifest_in_mock() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 启用 mock，fetch 不依赖真实 core
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let data = LoadedState::fetch();
        assert!(data.connected, "mock 下应视为已连接");
        // capabilities 从 mock 的 system.capabilities 返回，非空
        let caps = Capabilities::from_value(&data.capabilities_value);
        assert!(caps.entries.len() >= 114, "mock capabilities 应含全部 key");
        // 测试结束前恢复现场
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    #[test]
    fn non_mock_disconnect_sets_conn_failed() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        let mut st = AppState::empty();
        let data = LoadedState {
            config_value: Value::Object(Default::default()),
            system_info: Value::Object(Default::default()),
            capabilities_value: Value::Object(Default::default()),
            schemas: vec![],
            theme_names: vec![],
            fonts: vec![],
            connected: false,
        };
        st.apply_loaded(data);
        assert!(st.conn_failed.get(), "非 mock 未连接应置 conn_failed");
    }

    #[test]
    fn mock_disconnect_does_not_block() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        let data = LoadedState {
            config_value: Value::Object(Default::default()),
            system_info: Value::Object(Default::default()),
            capabilities_value: Value::Object(Default::default()),
            schemas: vec![],
            theme_names: vec![],
            fonts: vec![],
            connected: false,
        };
        st.apply_loaded(data);
        assert!(!st.conn_failed.get(), "mock 模式不应阻断");
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }
}

/// GDI 字体枚举所需的 LOGFONTW 结构（与 Windows SDK 内存布局一致）。
#[cfg(windows)]
#[repr(C)]
struct GdiLogFontW {
    lf_height: i32, lf_width: i32, lf_escapement: i32, lf_orientation: i32,
    lf_weight: i32, lf_italic: u8, lf_underline: u8, lf_strike_out: u8,
    lf_char_set: u8, lf_out_precision: u8, lf_clip_precision: u8,
    lf_quality: u8, lf_pitch_and_family: u8,
    lf_face_name: [u16; 32],
}

#[cfg(windows)]
#[link(name = "gdi32")]
unsafe extern "system" {
    fn GetDC(hwnd: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    fn ReleaseDC(hwnd: *mut core::ffi::c_void, hdc: *mut core::ffi::c_void) -> i32;
    fn EnumFontFamiliesExW(
        hdc: *mut core::ffi::c_void,
        lplf: *const GdiLogFontW,
        lpproc: unsafe extern "system" fn(
            *const GdiLogFontW,
            *const core::ffi::c_void,
            u32,
            isize,
        ) -> i32,
        l_param: isize,
        dw_flags: u32,
    ) -> i32;
}

/// 加载系统字体列表：Windows 用 GDI EnumFontFamiliesExW 获取本地化字体名；
/// 非 Windows 或 GDI 不可用时回退 system.fonts RPC。
fn load_fonts() -> Vec<String> {
    #[cfg(windows)]
    {
        let gdi = load_fonts_from_gdi();
        if !gdi.is_empty() {
            return gdi;
        }
    }
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

/// Windows GDI 字体族枚举：EnumFontFamiliesExW（DEFAULT_CHARSET）直接返回系统
/// 当前 locale 的字体显示名，中文系统得"微软雅黑"而非注册表英文键名。
/// 跳过 '@' 开头的竖排 CJK 变体字体，HashSet 去重后排序。
#[cfg(windows)]
fn load_fonts_from_gdi() -> Vec<String> {
    use std::collections::HashSet;

    unsafe extern "system" fn font_enum_cb(
        lplf: *const GdiLogFontW,
        _ntm: *const core::ffi::c_void,
        _font_type: u32,
        lp: isize,
    ) -> i32 {
        unsafe {
            let lf = &*lplf;
            let families = &mut *(lp as *mut HashSet<String>);
            let len = lf.lf_face_name.iter().position(|&c| c == 0).unwrap_or(32);
            let name = String::from_utf16_lossy(&lf.lf_face_name[..len]).to_string();
            if !name.starts_with('@') && !name.is_empty() {
                families.insert(name);
            }
        }
        1
    }

    unsafe {
        let hdc = GetDC(std::ptr::null_mut());
        if hdc.is_null() {
            return vec![];
        }
        let lf = GdiLogFontW {
            lf_height: 0, lf_width: 0, lf_escapement: 0, lf_orientation: 0,
            lf_weight: 0, lf_italic: 0, lf_underline: 0, lf_strike_out: 0,
            lf_char_set: 1, // DEFAULT_CHARSET — 枚举所有字体族
            lf_out_precision: 0, lf_clip_precision: 0, lf_quality: 0,
            lf_pitch_and_family: 0,
            lf_face_name: [0u16; 32],
        };
        let mut families: HashSet<String> = HashSet::new();
        EnumFontFamiliesExW(
            hdc,
            &lf,
            font_enum_cb,
            &mut families as *mut _ as isize,
            0,
        );
        ReleaseDC(std::ptr::null_mut(), hdc);
        let mut v: Vec<String> = families.into_iter().collect();
        v.sort();
        v
    }
}
