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
use crate::pages::dict::reactive::Feedback;
use crate::pages::dict::state::DictManagerState;
use crate::rpc;
use crate::setting_config;

/// 一个方案条目（schema.list 返回）。
#[derive(Clone, Default)]
pub struct SchemaInfo {
    pub id: String,
    pub name: String,
    /// 类型（codetable / pinyin / ...），用于主方案下拉过滤；缺省为空。
    pub kind: String,
    /// 拼音方案类型（"full"=全拼，"shuangpin"=双拼，""=未标注/视为全拼）；非拼音方案为空串。
    pub scheme: String,
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
    /// 设置程序自身配置（不写入 core config）。
    pub setting_config: setting_config::SettingConfig,
    /// 启动加载本地设置配置时的非阻断警告。
    pub setting_config_warning: Option<String>,
    /// 主题编辑器本地 HTTP 服务句柄。
    pub theme_editor_server: Rc<RefCell<crate::theme_editor_server::ThemeEditorServer>>,
    /// 主题编辑器连接服务是否运行。
    pub theme_editor_running: Signal<bool>,
    /// 主题编辑器连接服务地址。
    pub theme_editor_url: Signal<String>,
    /// 主题编辑器连接模式：off / once / auto。
    pub theme_editor_mode: Signal<String>,
    /// 主题编辑器连接模式的分段控件索引镜像（0=关闭/1=本次开启/2=自动开启），
    /// 供 `Element::segmented` 双向绑定；实际启停由 reactive_effect 监听其变化触发，
    /// 避免和 `theme_editor_mode`（字符串真相源，供内部方法/测试读取）互相触发死循环。
    pub theme_editor_mode_idx: Signal<usize>,
    /// 待确认的主题导入 URL。
    pub pending_theme_import_url: Signal<String>,
    /// 待确认的主题导入名称。
    pub pending_theme_import_name: Signal<String>,
    /// 主题导入确认对话框显示的来源文本，由 pending URL 统一派生。
    pub theme_import_url_display: Signal<String>,
    /// 主题导入确认对话框显示的名称提示文本，由 pending name 统一派生。
    pub theme_import_name_display: Signal<String>,
    /// 主题导入确认对话框可见标志。
    pub theme_import_dialog_visible: Signal<bool>,
    /// 当前 Tab 索引。
    pub tab: Signal<usize>,
    /// 暗色模式（设置窗口自身明暗，运行时切换）。
    pub dark: Signal<bool>,
    /// 状态文字（底栏左下角连接态：已连接/离线预览/未连接/正在重连）。
    pub status: Signal<String>,
    /// 瞬时反馈（保存成功/没有变更/保存失败/已重新加载/已恢复本页等），走 toast 而非底栏。
    pub feedback: Feedback,
    /// 触发键冲突的实时提示文案（空串=无冲突）；由 `conflict_watcher` 每帧刷新，
    /// 输入页常驻警告条据此显示/隐藏。取代了旧版仅在保存 toast 里追加的冲突后缀。
    pub conflict: Signal<String>,
    /// 方案管理对话框状态。
    pub schema_mgr: SchemaManagerState,
    /// 词库管理页状态（导航/数据加载/写操作）。
    pub dict_mgr: DictManagerState,
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
    /// 主码表方案下拉的响应式选项（schema.list 码表类，apply_loaded 后填充）。
    /// value 首项为空字符串（"自动"），其余为方案 id；label 对应。
    pub codetable_schema_val_sig: Signal<Vec<String>>,
    pub codetable_schema_lab_sig: Signal<Vec<String>>,
    /// 主拼音方案下拉的响应式选项（schema.list 拼音类，apply_loaded 后填充）。
    pub pinyin_schema_val_sig: Signal<Vec<String>>,
    pub pinyin_schema_lab_sig: Signal<Vec<String>>,
}

/// 主题编辑器连接模式字符串 → 分段控件索引（0=off/1=once/2=auto；未知值回落 0）。
fn mode_str_to_idx(mode: &str) -> usize {
    match mode {
        "once" => 1,
        "auto" => 2,
        _ => 0,
    }
}

impl AppState {
    /// 快速初始化：仅读系统主题，其余字段置空。
    /// UI 可立即显示；真实数据由后台线程通过 [`AppState::apply_loaded`] 写入。
    pub fn empty() -> Self {
        let schema_mgr = SchemaManagerState::new();
        let fields: Rc<FieldRegistry> = Rc::new(FieldRegistry::default());
        let setting_load = setting_config::load();
        let initial_mode = if setting_load.config.theme_editor.auto_start {
            "auto"
        } else {
            "off"
        }
        .to_string();

        // 注册 schema.available 写回器：把 enabled_order 序列化为有序字符串数组
        {
            let eo = schema_mgr.enabled_order.clone();
            fields.writers.borrow_mut().push(Box::new(move |cfg| {
                let ids: Vec<Value> = eo
                    .borrow()
                    .iter()
                    .map(|id| Value::String(id.clone()))
                    .collect();
                cfg.set_path("schema.available", Value::Array(ids));
            }));
        }

        Self {
            base_config: Config::from_value(Value::Object(serde_json::Map::new())),
            current_config: Config::from_value(Value::Object(serde_json::Map::new())),
            manifest: Manifest::from_embedded(),
            capabilities: Capabilities::from_embedded(),
            system_info: Value::Object(serde_json::Map::new()),
            schemas: vec![],
            theme_names: vec![],
            fonts: vec![],
            fields,
            setting_config: setting_load.config,
            setting_config_warning: setting_load.warning,
            theme_editor_server: Rc::new(RefCell::new(
                crate::theme_editor_server::ThemeEditorServer::default(),
            )),
            theme_editor_running: signal(false),
            theme_editor_url: signal(String::new()),
            theme_editor_mode_idx: signal(mode_str_to_idx(&initial_mode)),
            theme_editor_mode: signal(initial_mode),
            pending_theme_import_url: signal(String::new()),
            pending_theme_import_name: signal(String::new()),
            theme_import_url_display: signal("来源：".to_string()),
            theme_import_name_display: signal("名称提示：无".to_string()),
            theme_import_dialog_visible: signal(false),
            tab: signal(0),
            dark: signal(crate::theme_def::system_dark()),
            status: signal("正在连接服务...".to_string()),
            feedback: signal(None),
            conflict: signal(String::new()),
            schema_mgr,
            dict_mgr: DictManagerState::new(),
            reload_confirm: signal(false),
            reset_confirm: signal(false),
            connected: false,
            conn_failed: signal(false),
            conn_ok: signal(false),
            theme_label_sig: signal(vec![]),
            theme_value_sig: signal(vec![]),
            font_sig: signal(vec![]),
            font_label_sig: signal(vec![]),
            codetable_schema_val_sig: signal(vec![]),
            codetable_schema_lab_sig: signal(vec![]),
            pinyin_schema_val_sig: signal(vec![]),
            pinyin_schema_lab_sig: signal(vec![]),
        }
    }

    pub fn start_theme_editor_server(&self, auto: bool) {
        match self.theme_editor_server.borrow_mut().start() {
            Ok(s) => {
                self.theme_editor_running.set(s.running);
                self.theme_editor_url.set(s.url.unwrap_or_default());
                self.theme_editor_mode
                    .set(if auto { "auto" } else { "once" }.to_string());
                self.notify_ok("主题编辑器连接已开启");
            }
            Err(e) => {
                // 启动失败（如端口均被占用）：分段控件已被用户切到"开启"档，须退回"关闭"
                // 档，UI 才不会显示一个实际未运行的状态；错误用 toast 提示，比状态文字更醒目。
                self.theme_editor_mode.set("off".to_string());
                self.theme_editor_mode_idx.set(0);
                self.notify_err(e);
            }
        }
    }

    pub fn stop_theme_editor_server(&self) {
        self.theme_editor_server.borrow_mut().stop();
        self.theme_editor_running.set(false);
        self.theme_editor_url.set(String::new());
        self.theme_editor_mode.set("off".to_string());
        self.notify_ok("主题编辑器连接已关闭");
    }

    pub fn save_theme_editor_auto_start(&mut self, auto: bool) {
        self.setting_config.theme_editor.auto_start = auto;
        if let Err(e) = crate::setting_config::save(&self.setting_config) {
            self.notify_err(e);
        }
    }

    /// 应用分段控件（关闭/本次开启/自动开启）当前档位对应的启停+持久化动作。
    /// 由 reactive_effect 在 `theme_editor_mode_idx` 变化时调用；索引变化本身只表达用户
    /// 意图，真正的启停副作用集中在这一处，避免散落在各按钮 on_click 里。
    pub fn apply_theme_editor_mode_idx(&mut self, idx: usize) {
        match idx {
            0 => {
                self.stop_theme_editor_server();
                self.save_theme_editor_auto_start(false);
            }
            1 => {
                self.start_theme_editor_server(false);
                self.save_theme_editor_auto_start(false);
            }
            2 => {
                self.start_theme_editor_server(true);
                self.save_theme_editor_auto_start(true);
            }
            _ => {}
        }
    }

    /// 瞬时操作成功反馈：走全局 feedback/toast（`shell::build` 挂了唯一的 toast_sink）。
    pub fn notify_ok(&self, msg: impl Into<String>) {
        self.feedback.set(Some((ToastKind::Success, msg.into())));
    }

    /// 瞬时操作失败反馈：走全局 feedback/toast。
    pub fn notify_err(&self, msg: impl Into<String>) {
        self.feedback.set(Some((ToastKind::Error, msg.into())));
    }

    pub fn set_pending_theme_import(&self, url: String, name: String) {
        self.pending_theme_import_url.set(url);
        self.pending_theme_import_name.set(name);
        self.sync_theme_import_display();
    }

    pub fn clear_pending_theme_import(&self) {
        Self::clear_theme_import_signals(
            self.pending_theme_import_url,
            self.pending_theme_import_name,
            self.theme_import_url_display,
            self.theme_import_name_display,
        );
    }

    pub fn clear_theme_import_signals(
        url: Signal<String>,
        name: Signal<String>,
        url_display: Signal<String>,
        name_display: Signal<String>,
    ) {
        url.set(String::new());
        name.set(String::new());
        Self::sync_theme_import_display_signals(url, name, url_display, name_display);
    }

    pub fn sync_theme_import_display(&self) {
        Self::sync_theme_import_display_signals(
            self.pending_theme_import_url,
            self.pending_theme_import_name,
            self.theme_import_url_display,
            self.theme_import_name_display,
        );
    }

    pub fn refresh_theme_names(&mut self) {
        self.theme_names = load_theme_names();
        let labels: Vec<String> = self
            .theme_names
            .iter()
            .map(|(_, name)| name.clone())
            .collect();
        let values: Vec<String> = self.theme_names.iter().map(|(id, _)| id.clone()).collect();
        self.theme_label_sig.set(labels);
        self.theme_value_sig.set(values);
    }

    fn sync_theme_import_display_signals(
        url: Signal<String>,
        name: Signal<String>,
        url_display: Signal<String>,
        name_display: Signal<String>,
    ) {
        let url = url.get();
        let name = name.get();
        url_display.set(format!("来源：{url}"));
        name_display.set(if name.is_empty() {
            "名称提示：无".to_string()
        } else {
            format!("名称提示：{name}")
        });
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
        self.theme_label_sig
            .set(self.theme_names.iter().map(|(_, n)| n.clone()).collect());
        self.theme_value_sig
            .set(self.theme_names.iter().map(|(id, _)| id.clone()).collect());
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
        // 方案下拉选项（码表/拼音类各自过滤）：首项空值="自动"，其余为 id/name。
        // 必须在 loaders 跑之前就位，否则 loader 无法在选项列表中定位当前配置值。
        {
            let (ct_vals, ct_labs): (Vec<String>, Vec<String>) =
                std::iter::once((String::new(), "（自动）".to_string()))
                    .chain(
                        self.schemas
                            .iter()
                            .filter(|s| s.kind == "codetable")
                            .map(|s| (s.id.clone(), s.name.clone())),
                    )
                    .unzip();
            self.codetable_schema_val_sig.set(ct_vals);
            self.codetable_schema_lab_sig.set(ct_labs);
            let (py_vals, py_labs): (Vec<String>, Vec<String>) =
                std::iter::once((String::new(), "（自动）".to_string()))
                    .chain(
                        self.schemas
                            .iter()
                            .filter(|s| s.kind == "pinyin")
                            .map(|s| (s.id.clone(), s.name.clone())),
                    )
                    .unzip();
            self.pinyin_schema_val_sig.set(py_vals);
            self.pinyin_schema_lab_sig.set(py_labs);
        }
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
        // 方案启用状态从 schema.available 驱动，同步各行 Signal（build 后才有注册项）
        self.schema_mgr.apply_config(&cfg);
        // 词库页注入方案列表并加载当前类别数据（默认短语域）。
        self.dict_mgr.set_schemas(&self.schemas);
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
        !diff_config(&self.base_config, &probe, &self.capabilities.map_keys()).is_empty()
    }

    /// 实时按键冲突检测：跑写回器到 current 的副本上（反映编辑中的最新 Signal 值），
    /// 分三个场景域各自检测——**跨域不算冲突**（同一物理键在不同场景分时复用是正常设计）：
    /// - 触发键域：无候选/闲时触发模式（临时拼音/英文、混合、特殊模式）。
    /// - 候选键域：有候选时操作候选（次选/翻页/高亮移动/以词定字）。
    /// - 快捷键域：功能组合键（切换方案/标点/工具栏…、置顶/删除词条），归一化后同键即冲突。
    ///
    /// 各域摘要用换行拼成一条文案（可能各一行）；均无冲突返回 None。
    /// 供 `conflict_watcher` 每帧刷新固定顶栏。
    pub fn live_conflicts(&self) -> Option<String> {
        let mut probe = self.current_config.clone();
        for w in self.fields.writers.borrow().iter() {
            w(&mut probe);
        }
        let mut lines = Vec::new();
        if let Some(m) = check_trigger_key_conflicts(&probe) {
            lines.push(m);
        }
        if let Some(m) = check_candidate_key_conflicts(&probe) {
            lines.push(m);
        }
        if let Some(m) = check_hotkey_conflicts(&probe) {
            lines.push(m);
        }
        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
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
        self.feedback.set(Some((
            ToastKind::Info,
            if n == 0 {
                "本页无可恢复的默认项".to_string()
            } else {
                format!("已恢复本页 {n} 项为默认值（未保存）")
            },
        )));
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
        // 方案启用排序也需要从配置恢复，否则未保存的列表变更不会撤销。
        self.schema_mgr.apply_config(&cfg);
        self.base_config = cfg.clone();
        self.current_config = cfg;
        self.feedback.set(Some((ToastKind::Info, "已从服务端重新加载".to_string())));
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
        // 触发键冲突不再在此追加进保存 toast——改由输入页常驻警告条实时呈现（见 conflict_watcher）。
        let items =
            diff_config(&self.base_config, &self.current_config, &self.capabilities.map_keys());
        if items.is_empty() {
            self.feedback.set(Some((ToastKind::Info, "没有变更".to_string())));
            return true;
        }
        let items_json: Vec<Value> = items
            .iter()
            .map(|it| serde_json::json!({ "key": it.key, "value": it.value }))
            .collect();
        let params = serde_json::json!({ "items": items_json });
        match rpc::call_core("config.setItems", &params) {
            Ok(resp) => {
                // 服务端会静默跳过无效项（未登记键/类型/枚举错），不能一律当成功——
                // 否则用户以为已保存、实际配置丢失且无从察觉。
                let skipped: Vec<String> = resp
                    .get("skipped")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|it| it.get("key").and_then(|k| k.as_str()))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                if skipped.is_empty() {
                    self.base_config = self.current_config.clone();
                    self.feedback.set(Some((
                        ToastKind::Success,
                        format!("已保存 {} 项变更", items_json.len()),
                    )));
                    true
                } else {
                    // 不更新 base_config：被拒项保持「未保存」状态，重试仍会带上。
                    self.feedback.set(Some((
                        ToastKind::Error,
                        format!(
                            "已保存 {} 项，{} 项被服务端拒绝: {}",
                            items_json.len() - skipped.len(),
                            skipped.len(),
                            skipped.join(", ")
                        ),
                    )));
                    false
                }
            }
            Err(e) => {
                self.feedback.set(Some((ToastKind::Error, format!("保存失败: {e}"))));
                false
            }
        }
    }
}

/// 收集当前配置里"裸键触发"四组的 (显示名, 触发键列表)，供按键冲突检测用。
fn collect_trigger_key_groups(cfg: &Config) -> Vec<(String, Vec<String>)> {
    fn strs(v: Option<&Vec<Value>>) -> Vec<String> {
        v.map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default()
    }

    let mut groups = vec![
        ("临时拼音".to_string(), strs(cfg.get_array("input.temp_pinyin.trigger_keys"))),
        ("临时英文".to_string(), strs(cfg.get_array("input.temp_english.trigger_keys"))),
    ];

    if let Some(arr) = cfg.get_array("schema.mix_modes") {
        for m in arr {
            let label = m.get("name").and_then(Value::as_str).unwrap_or("混合模式").to_string();
            let keys = m
                .get("trigger_keys")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            groups.push((label, keys));
        }
    }

    if let Some(arr) = cfg.get_array("schema.special_modes") {
        for m in arr {
            let name = m.get("name").and_then(Value::as_str).unwrap_or("");
            let label = format!("特殊模式「{name}」");
            let keys = m
                .get("trigger_keys")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            groups.push((label, keys));
        }
    }

    // 去重每组触发键：mix_modes/special_modes 的 trigger_keys 直取自 core config 数组，
    // 可能含重复值；不去重会让冲突文案对同一键重复呈现。
    for (_, keys) in groups.iter_mut() {
        let mut seen = std::collections::HashSet::new();
        keys.retain(|k| seen.insert(k.clone()));
    }
    groups
}

/// 对一个场景域的分组跑冲突检测，冠 `scene` 标签生成提示；无冲突返回 None。
fn check_domain(groups: Vec<(String, Vec<String>)>, scene: &str) -> Option<String> {
    let hits = crate::key_conflict::find_key_conflicts(&groups);
    crate::key_conflict::conflict_summary(&hits, scene)
}

/// 触发键场景域（无候选/闲时）冲突检测。
fn check_trigger_key_conflicts(cfg: &Config) -> Option<String> {
    check_domain(collect_trigger_key_groups(cfg), "触发键")
}

/// 候选键场景域（有候选时）冲突检测。
fn check_candidate_key_conflicts(cfg: &Config) -> Option<String> {
    check_domain(collect_candidate_key_groups(cfg), "候选键")
}

/// 收集"候选操作"域各组的 (显示名, 物理键列表)，组名经 `expand_candidate_key` 展开
/// （tab/shift_tab 归一、lrshift/lrctrl 拆成单修饰键），供有候选场景下的按键冲突检测。
///
/// 含中英切换键：core 在 KEY_UP 处理中英切换，即便候选窗打开也会触发（`coordinator.rs:3225`），
/// 与次选键(lrshift/lrctrl)争用同一物理修饰键——有候选时按下即选词、切换被抑制，是真冲突。
/// 详见 `docs/key-conflict-detection.md`。
fn collect_candidate_key_groups(cfg: &Config) -> Vec<(String, Vec<String>)> {
    fn expand_strs(v: Option<&Vec<Value>>) -> Vec<String> {
        v.map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .flat_map(crate::key_conflict::expand_candidate_key)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
    }

    vec![
        ("次选/三选键".to_string(), expand_strs(cfg.get_array("keys.select_key_groups"))),
        ("翻页键".to_string(), expand_strs(cfg.get_array("keys.page_keys"))),
        ("高亮移动键".to_string(), expand_strs(cfg.get_array("keys.highlight_keys"))),
        ("以词定字".to_string(), expand_strs(cfg.get_array("keys.select_char_keys"))),
        ("中英切换键".to_string(), expand_strs(cfg.get_array("keys.toggle_mode_keys"))),
    ]
}

/// 收集功能快捷键（组合键）各功能的 (显示名, [归一化组合键])，供组合键冲突检测。
/// 组合键都带修饰键、是显式命令，不涉及裸键的按场景分派，故统一为一个域检测。
/// 含 8 个 hotkey 项 + pin/delete_candidate（select，但值也是 Ctrl+数字 组合键）；
/// 空值与 "none" 视为未设置、跳过。
fn collect_hotkey_groups(cfg: &Config) -> Vec<(String, Vec<String>)> {
    const HOTKEYS: &[(&str, &str)] = &[
        ("keys.switch_engine", "切换输入方案"),
        ("keys.toggle_full_width", "切换全角/半角"),
        ("keys.toggle_punct", "切换中/英文标点"),
        ("keys.toggle_s2t", "切换简入繁出"),
        ("keys.toggle_toolbar", "显示/隐藏工具栏"),
        ("keys.open_settings", "打开设置"),
        ("keys.add_word", "快捷加词"),
        ("keys.activate_ime", "切换到本输入法"),
        ("keys.pin_candidate", "置顶词条"),
        ("keys.delete_candidate", "删除词条"),
    ];
    HOTKEYS
        .iter()
        .map(|(key, label)| {
            let raw = cfg.get_str(key).unwrap_or("");
            let keys = if raw.is_empty() || raw == "none" {
                Vec::new()
            } else {
                vec![crate::key_conflict::canonical_hotkey(raw)]
            };
            (label.to_string(), keys)
        })
        .collect()
}

/// 功能快捷键（组合键）域冲突检测。
fn check_hotkey_conflicts(cfg: &Config) -> Option<String> {
    check_domain(collect_hotkey_groups(cfg), "快捷键")
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
        let h_config = std::thread::spawn(|| rpc::call_core("config.get", &serde_json::json!({})));
        let h_info = std::thread::spawn(|| rpc::call_core("system.info", &serde_json::json!({})));
        let h_caps =
            std::thread::spawn(|| rpc::call_core("system.capabilities", &serde_json::json!({})));
        let h_schemas = std::thread::spawn(load_schemas);
        let h_themes = std::thread::spawn(load_theme_names);
        let h_fonts = std::thread::spawn(load_fonts);

        let config_res = h_config.join().ok().and_then(|r| r.ok());
        let system_info = h_info
            .join()
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_else(Self::empty_obj);
        let caps_res = h_caps.join().ok().and_then(|r| r.ok());
        let schemas = h_schemas.join().unwrap_or_default();
        let theme_names = h_themes.join().unwrap_or_default();
        let fonts = h_fonts.join().unwrap_or_default();

        // config_value 是否为 Some 直接反映管道连接是否成功。
        let connected = config_res.is_some();
        let config_value = config_res.unwrap_or_else(Self::empty_obj);
        let capabilities_value = caps_res.unwrap_or_else(Self::empty_obj);

        tracing::info!(
            "LoadedState::fetch 完成 (connected={connected}, 并行耗时: {:?})",
            t0.elapsed()
        );

        Self {
            config_value,
            system_info,
            capabilities_value,
            schemas,
            theme_names,
            fonts,
            connected,
        }
    }
}

/// 加载方案列表（schema.list）。
pub(crate) fn load_schemas() -> Vec<SchemaInfo> {
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
                        .get("engineType")
                        .or_else(|| s.get("engine_type"))
                        .or_else(|| s.get("kind"))
                        .or_else(|| s.get("type"))
                        .or_else(|| s.get("category"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let scheme = s
                        .get("scheme")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(SchemaInfo {
                        id,
                        name,
                        kind,
                        scheme,
                    })
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

#[cfg(test)]
mod trigger_conflict_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_collect_trigger_key_groups_no_conflict() {
        let cfg = Config::from_value(json!({
            "input": {
                "temp_pinyin": { "trigger_keys": ["backtick"] },
                "temp_english": { "trigger_keys": [] }
            },
            "schema": {
                "mix_modes": [
                    { "id": "quick_mix", "name": "快捷", "trigger_keys": ["semicolon"] }
                ],
                "special_modes": []
            }
        }));
        assert!(check_trigger_key_conflicts(&cfg).is_none());
    }

    #[test]
    fn test_check_trigger_key_conflicts_detects_overlap() {
        let cfg = Config::from_value(json!({
            "input": {
                "temp_pinyin": { "trigger_keys": ["semicolon"] },
                "temp_english": { "trigger_keys": [] }
            },
            "schema": {
                "mix_modes": [
                    { "id": "quick_mix", "name": "快捷", "trigger_keys": ["semicolon"] }
                ],
                "special_modes": []
            }
        }));
        let msg = check_trigger_key_conflicts(&cfg).unwrap();
        assert!(msg.contains(';'), "键名应以符号呈现");
        assert!(msg.contains("临时拼音"));
        assert!(msg.contains("快捷"));
    }

    #[test]
    fn test_check_trigger_key_conflicts_includes_special_modes() {
        let cfg = Config::from_value(json!({
            "input": {
                "temp_pinyin": { "trigger_keys": ["z"] },
                "temp_english": { "trigger_keys": [] }
            },
            "schema": {
                "mix_modes": [],
                "special_modes": [
                    { "id": "fast_symbols", "name": "快符", "trigger_keys": ["z"] }
                ]
            }
        }));
        let msg = check_trigger_key_conflicts(&cfg).unwrap();
        assert!(msg.contains("特殊模式「快符」"));
    }

    #[test]
    fn test_live_trigger_conflict_reflects_current_config() {
        let mut st = AppState::empty();
        st.current_config = Config::from_value(json!({
            "input": {
                "temp_pinyin": { "trigger_keys": ["semicolon"] },
                "temp_english": { "trigger_keys": [] }
            },
            "schema": {
                "mix_modes": [
                    { "id": "quick_mix", "name": "快捷", "trigger_keys": ["semicolon"] }
                ],
                "special_modes": []
            }
        }));
        let msg = st.live_conflicts().expect("编辑态含重叠触发键，应实时检出冲突");
        assert!(msg.contains("触发键冲突"), "应标注触发键场景");
        assert!(msg.contains(';'), "键名应以符号呈现");
        assert!(msg.contains("临时拼音"));
        assert!(msg.contains("快捷"));
        // 无冲突时返回 None。
        st.current_config = Config::from_value(json!({
            "input": { "temp_pinyin": { "trigger_keys": ["backtick"] } }
        }));
        assert!(st.live_conflicts().is_none(), "无重叠应无冲突");
    }

    #[test]
    fn test_candidate_domain_conflict_and_cross_domain_ignored() {
        // 候选域：翻页键与以词定字都用「减号/等号」→ 有候选时按 - 有歧义，应冲突。
        let cfg = Config::from_value(json!({
            "keys": {
                "page_keys": ["minus_equal"],
                "select_char_keys": ["minus_equal"]
            }
        }));
        let msg = check_candidate_key_conflicts(&cfg).expect("候选域应检出冲突");
        assert!(msg.contains("候选键冲突"));
        assert!(msg.contains("翻页键"));
        assert!(msg.contains("以词定字"));

        // tab(高亮移动) 与 shift_tab(翻页) 归一后应冲突。
        let cfg2 = Config::from_value(json!({
            "keys": { "highlight_keys": ["tab"], "page_keys": ["shift_tab"] }
        }));
        let msg2 = check_candidate_key_conflicts(&cfg2).expect("tab/shift_tab 应冲突");
        assert!(msg2.contains("Tab"));

        // 跨域不算冲突：触发键 ; 与 候选次选键 ; ' 分属不同场景，不应报冲突。
        let cfg3 = Config::from_value(json!({
            "input": { "temp_pinyin": { "trigger_keys": ["semicolon"] } },
            "keys": { "select_key_groups": ["semicolon_quote"] }
        }));
        assert!(check_trigger_key_conflicts(&cfg3).is_none(), "触发域无内部重叠");
        assert!(check_candidate_key_conflicts(&cfg3).is_none(), "候选域无内部重叠");
    }

    #[test]
    fn test_hotkey_domain_conflict_normalizes_order() {
        // 两功能绑到同一组合键（写法顺序不同）→ 归一后应检出冲突。
        let cfg = Config::from_value(json!({
            "keys": {
                "switch_engine": "ctrl+shift+e",
                "toggle_toolbar": "shift+ctrl+e"
            }
        }));
        let msg = check_hotkey_conflicts(&cfg).expect("同一组合键应冲突");
        assert!(msg.contains("快捷键冲突"));
        assert!(msg.contains("Ctrl+Shift+E"), "应显示归一化组合键");
        assert!(msg.contains("切换输入方案"));
        assert!(msg.contains("显示/隐藏工具栏"));

        // none / 空值不参与，默认各异不冲突。
        let cfg2 = Config::from_value(json!({
            "keys": { "switch_engine": "none", "toggle_toolbar": "none" }
        }));
        assert!(check_hotkey_conflicts(&cfg2).is_none(), "none 不算占用");

        // pin/delete 同为 Ctrl+数字 → 冲突。
        let cfg3 = Config::from_value(json!({
            "keys": { "pin_candidate": "ctrl+number", "delete_candidate": "ctrl+number" }
        }));
        let msg3 = check_hotkey_conflicts(&cfg3).expect("pin/delete 同键应冲突");
        assert!(msg3.contains("Ctrl+数字"));
    }

    #[test]
    fn test_toggle_mode_vs_select_key_conflict() {
        // 右 Shift 既设为中英切换又作次选键(lrshift 含右Shift)：有候选时按下选词、切换被抑制 → 冲突。
        let cfg = Config::from_value(json!({
            "keys": {
                "toggle_mode_keys": ["lshift", "rshift"],
                "select_key_groups": ["lrshift"]
            }
        }));
        let msg = check_candidate_key_conflicts(&cfg).expect("中英切换键与次选键应冲突");
        assert!(msg.contains("候选键冲突"));
        assert!(msg.contains("中英切换键"));
        assert!(msg.contains("次选/三选键"));

        // 中英切换用 Shift、次选用「分号/引号」→ 不同物理键，不冲突。
        let cfg2 = Config::from_value(json!({
            "keys": {
                "toggle_mode_keys": ["lshift"],
                "select_key_groups": ["semicolon_quote"]
            }
        }));
        assert!(check_candidate_key_conflicts(&cfg2).is_none(), "不同物理键不应冲突");
    }

    #[test]
    fn test_trigger_keys_deduped_avoids_doubled_text() {
        // core config 数组含重复触发键时，去重避免冲突文案对同一键重复呈现。
        let cfg = Config::from_value(json!({
            "input": {
                "temp_pinyin": { "trigger_keys": ["semicolon", "semicolon"] }
            },
            "schema": {
                "mix_modes": [
                    { "id": "quick_mix", "name": "快捷", "trigger_keys": ["semicolon"] }
                ]
            }
        }));
        let msg = check_trigger_key_conflicts(&cfg).expect("应检出冲突");
        // 只应出现一次冲突描述，不因重复键翻倍。
        assert_eq!(msg.matches("同时被").count(), 1, "重复触发键不应产生重复文案");
    }
}

// 测试模块后仍有 load_schemas 等私有辅助函数；局部豁免 toolchain 新增的风格 lint。
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;

    /// 所有涉及 WIND_RPC_MOCK 环境变量的测试必须持有此锁，防止并行 race。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn app_state_initializes_theme_import_wiring() {
        let st = AppState::empty();

        assert_eq!(
            st.setting_config.theme_editor.auto_start,
            st.theme_editor_mode.get() == "auto"
        );
        assert!(!st.theme_editor_running.get());
        assert_eq!(st.theme_editor_url.get(), "");
        assert!(matches!(
            st.theme_editor_mode.get().as_str(),
            "off" | "auto"
        ));
        assert_eq!(st.pending_theme_import_url.get(), "");
        assert_eq!(st.pending_theme_import_name.get(), "");
        assert_eq!(st.theme_import_url_display.get(), "来源：");
        assert_eq!(st.theme_import_name_display.get(), "名称提示：无");
        assert!(!st.theme_import_dialog_visible.get());
    }

    #[test]
    fn theme_import_display_is_derived_from_pending_state() {
        let st = AppState::empty();

        st.set_pending_theme_import(
            "https://example.com/theme.toml".to_string(),
            "Night".to_string(),
        );
        assert_eq!(
            st.pending_theme_import_url.get(),
            "https://example.com/theme.toml"
        );
        assert_eq!(st.pending_theme_import_name.get(), "Night");
        assert_eq!(
            st.theme_import_url_display.get(),
            "来源：https://example.com/theme.toml"
        );
        assert_eq!(st.theme_import_name_display.get(), "名称提示：Night");

        st.set_pending_theme_import(
            "https://example.com/unnamed.toml".to_string(),
            String::new(),
        );
        assert_eq!(
            st.theme_import_url_display.get(),
            "来源：https://example.com/unnamed.toml"
        );
        assert_eq!(st.theme_import_name_display.get(), "名称提示：无");

        st.clear_pending_theme_import();
        assert_eq!(st.pending_theme_import_url.get(), "");
        assert_eq!(st.pending_theme_import_name.get(), "");
        assert_eq!(st.theme_import_url_display.get(), "来源：");
        assert_eq!(st.theme_import_name_display.get(), "名称提示：无");
    }

    #[test]
    fn theme_import_dialog_does_not_duplicate_reset_display_text() {
        let source = include_str!("dialogs/theme_import_dialog.rs");

        assert!(!source.contains(&format!("{}：", "来源")));
        assert!(!source.contains(&format!("{}提示：无", "名称")));
    }

    #[test]
    fn theme_import_dialog_refreshes_theme_names_after_success() {
        let source = include_str!("dialogs/theme_import_dialog.rs");

        assert!(source.contains("refresh_theme_names()"));
        assert!(source.contains("theme_import_dialog_visible.set(false)"));
        assert!(source.contains("已导入主题："));
    }

    #[test]
    fn refresh_theme_names_updates_options_from_core() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();

        st.theme_names = vec![("old".to_string(), "旧主题".to_string())];
        st.theme_value_sig.set(vec!["old".to_string()]);
        st.theme_label_sig.set(vec!["旧主题".to_string()]);

        st.refresh_theme_names();

        assert_eq!(
            st.theme_names,
            vec![
                ("default_light".to_string(), "清风·浅".to_string()),
                ("default_dark".to_string(), "清风·深".to_string()),
                ("mono".to_string(), "墨".to_string()),
            ]
        );
        assert_eq!(
            st.theme_value_sig.get(),
            vec!["default_light", "default_dark", "mono"]
        );
        assert_eq!(st.theme_label_sig.get(), vec!["清风·浅", "清风·深", "墨"]);

        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

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
    lf_height: i32,
    lf_width: i32,
    lf_escapement: i32,
    lf_orientation: i32,
    lf_weight: i32,
    lf_italic: u8,
    lf_underline: u8,
    lf_strike_out: u8,
    lf_char_set: u8,
    lf_out_precision: u8,
    lf_clip_precision: u8,
    lf_quality: u8,
    lf_pitch_and_family: u8,
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
            lf_height: 0,
            lf_width: 0,
            lf_escapement: 0,
            lf_orientation: 0,
            lf_weight: 0,
            lf_italic: 0,
            lf_underline: 0,
            lf_strike_out: 0,
            lf_char_set: 1, // DEFAULT_CHARSET — 枚举所有字体族
            lf_out_precision: 0,
            lf_clip_precision: 0,
            lf_quality: 0,
            lf_pitch_and_family: 0,
            lf_face_name: [0u16; 32],
        };
        let mut families: HashSet<String> = HashSet::new();
        EnumFontFamiliesExW(hdc, &lf, font_enum_cb, &mut families as *mut _ as isize, 0);
        ReleaseDC(std::ptr::null_mut(), hdc);
        let mut v: Vec<String> = families.into_iter().collect();
        v.sort();
        v
    }
}
