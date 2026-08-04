//! 全局状态 — 配置加载、清单、字段注册表、保存。
//!
//! 状态绑定改用 wind-ui-rust 的 `Signal`（取代旧 `Rc<Cell>`）。清单引擎
//! [`crate::manifest::field`] 为每个 key 建一个 Signal 并把"写回器/取值器"
//! 登记进 [`FieldRegistry`]；[`AppState::save`] 跑所有写回器把 Signal 值写回
//! Config，再与 base 做 diff，增量 `config.setItems`。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use windui::prelude::*;

use crate::capabilities::Capabilities;
use crate::config::{Config, diff_config};
use crate::dialogs::schema_manager::SchemaManagerState;
use crate::key_conflict::{Conflict, ConflictLevel, KeyGroup};
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
    /// 特殊方案（core `[schema].hidden`：英文、快符这类，只经引导键/热键进入）。
    ///
    /// 列表**恒含**特殊方案，由各消费点自己决定要不要——主方案下拉要排除（选不了也不该
    /// 显示），词库页与统计页要保留（它们各有独立的用户词库与词频记账）。在这里按显示
    /// 需求过滤的话，一个消费点的口径就成了所有消费点的口径。
    pub hidden: bool,
}

/// 一个主题条目（theme.list 返回）。core 协议：`name`=slug/id，`display_name`=界面显示名，
/// `version` 主题版本（可空），`builtin`=是否内置（内置不可删除，外部主题可删）。
#[derive(Clone, Default)]
pub struct ThemeEntry {
    /// slug/id，保存到 `ui.theme.name`。
    pub id: String,
    /// 界面显示名。
    pub display: String,
    /// 版本号（下拉徽章展示，可空）。
    pub version: String,
    /// 是否内置主题（true 不可删除；false 为外部/用户主题，显示删除按钮）。
    pub builtin: bool,
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
    /// 清单项 key → 加载器（把给定 Config 的值灌回 Signal；空 Config 时回落 manifest 默认）。
    ///
    /// 索引用的是**清单项 key**而非配置路径——`reset_current_page` 拿 manifest 的 key 反查。
    /// 同一配置路径需要两个控件时，第二个用视图 key（`<路径>#<视图名>`）另立清单项，
    /// 两者各有独立加载器；直接用同一个 key insert 两次会静默挤掉先注册的那个。
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
    /// 主题列表（theme.list）：含 id/显示名/版本/是否内置。下拉显示名、保存 id 到 `ui.theme.name`。
    pub theme_entries: Vec<ThemeEntry>,
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
    /// 触发键冲突的实时提示文案（空串=无冲突）；由 `conflict_watcher` 在渲染帧刷新
    /// （交互/动画驱动，空闲不渲染故不求值），输入页常驻警告条据此显示/隐藏。
    /// 取代了旧版仅在保存 toast 里追加的冲突后缀。
    pub conflict: Signal<String>,
    /// 方案管理对话框状态。
    pub schema_mgr: SchemaManagerState,
    /// 词库管理页状态（导航/数据加载/写操作）。
    pub dict_mgr: DictManagerState,
    /// 待应用的词库页深链（`--page dict --schema .. --type ..`）。
    ///
    /// 冷启动解析命令行时方案列表尚未就绪（要等后台 RPC 回来才有），无从把 `--schema`
    /// 解析成数据域下标，故先挂在这里，由 [`Self::apply_loaded`] 在 `set_schemas` 之后消费。
    /// 顺序不能反：`set_schemas_no_reload` 会清空 `sub_tab_memory` 并收敛越界的 domain。
    pending_dict_target: Rc<RefCell<Option<crate::cli::DictTarget>>>,
    /// 备份还原状态（高级页卡片 + 还原确认对话框）。
    pub backup_mgr: crate::pages::backup::BackupState,
    /// 统计页状态（数据快照 + 清理确认对话框）。
    pub stats_mgr: crate::pages::stats::StatsState,
    /// 「重新加载」二次确认对话框可见标志。
    pub reload_confirm: Signal<bool>,
    /// 「恢复本页」（有未保存改动时）确认对话框可见标志。
    pub reset_confirm: Signal<bool>,
    /// 启动时是否成功连接到输入法服务（core）。未连接则设置只读、保存会失败。
    pub connected: bool,
    /// [`Self::apply_loaded`] 是否已跑过一轮。区别于 `connected`：mock / 离线预览下
    /// 数据同样已就位但 `connected` 为 false，故不能拿后者判断"方案列表可用了没"。
    data_loaded: bool,
    /// 正式模式下连接失败标志：true 时显示阻断对话框；mock 模式始终 false。
    pub conn_failed: Signal<bool>,
    /// 连接成功的响应式标志（异步加载后置位）。底栏状态圆点据此响应式着色——
    /// 构建时尚未连接（empty），故圆点必须用 Signal 而非构建期 bool 才能在连接后转绿。
    pub conn_ok: Signal<bool>,
    /// 主题下拉的响应式选项：显示名列表 + 对应 id/slug 值列表（异步加载后填充）。
    /// 下拉构建于异步数据到达前，故必须经 Signal 驱动响应式下拉，数据到达即填充。
    pub theme_label_sig: Signal<Vec<String>>,
    pub theme_value_sig: Signal<Vec<String>>,
    /// 主题下拉的富内容选项（版本徽章 + 外部主题的删除图标）；与 theme_value_sig 同序。
    pub theme_items_sig: Signal<Vec<DropdownItem>>,
    /// 待删除主题的 id（删除确认对话框用）。
    pub theme_delete_id: Signal<String>,
    /// 删除确认对话框正文（含主题名）。
    pub theme_delete_msg: Signal<String>,
    /// 删除确认对话框可见标志。
    pub theme_delete_visible: Signal<bool>,
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

    // ── 在线升级 ──────────────────────────────────────────────────────────
    /// 升级对话框可见标志。
    pub update_dialog_visible: Signal<bool>,
    /// 升级流程阶段。
    pub update_phase: Signal<crate::update::UpdatePhase>,
    /// 下载进度 0.0..=1.0，驱动 `Element::progress`。
    pub update_progress: Signal<f32>,
    /// 进度旁的字节数文案（如「12.3 MB / 46.0 MB」）。
    pub update_progress_text: Signal<String>,
    /// 主文案：版本对比、已是最新、错误原因等，随阶段改写。
    pub update_message: Signal<String>,
    /// 更新包体积（如「更新包 46.0 MB」），下载前告知用户流量开销。
    pub update_size_text: Signal<String>,
    /// 「启动时自动检查更新」勾选框的绑定信号，初值取自 `setting_config.update.auto_check`。
    /// windui 的 checkbox 只写信号无回调，故由对话框挂一个 `reactive_effect` 节点落盘。
    pub update_auto_check_sig: Signal<bool>,
    /// 发布说明（已清洗）。
    pub update_notes: Signal<String>,
    /// 后台线程 → UI 线程的消息发送端，由 `main()` 在 App 构建期注入。
    ///
    /// `App::channel` 只能在构建期调用，而 `AppState` 先于 App 创建，故此处用
    /// `RefCell<Option<_>>` 延迟注入，而不是把它做成构造参数。
    pub update_tx: RefCell<Option<Sender<crate::update::UpdateMsg>>>,
    /// 下载取消旗，置 true 后下载线程在下一个数据块边界中止。
    pub update_cancel: Arc<AtomicBool>,
    /// 最近一次检查结果：下载阶段需要其中的 url / sha256 / size。
    pub update_check: RefCell<Option<crate::update::CheckResult>>,
    /// 已下载并校验通过的安装包路径。
    pub update_installer: RefCell<Option<std::path::PathBuf>>,
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
        // 先取出，下面 setting_load.config 会被移动进结构体
        let setting_load_update_auto_check = setting_load.config.update.auto_check;
        // 明暗：**None = 跟随系统**。不能写成 `unwrap_or(false)` —— 那会让系统处于深色
        // 主题的用户每次启动被强制翻成浅色，且与「用户显式选了浅色」再也分不开。
        let initial_dark = setting_load
            .config
            .ui
            .dark
            .unwrap_or_else(crate::theme_def::system_dark);
        // 方案列表的两个显示过滤：`new()` 内部已 refresh 过一次（当时读到的是默认值），
        // 故 show_hidden 走 set_show_hidden 重建列表，而不是只 set 信号——它决定的是
        // 「哪些方案进列表」，不重建的话开关是打开的、列表里却没有特殊方案。
        schema_mgr
            .hide_disabled
            .set(setting_load.config.ui.hide_disabled_schemas);
        schema_mgr.set_show_hidden(setting_load.config.ui.show_hidden_schemas);
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

        // 注册 keys.schema_hotkeys 写回器：方案直达热键（方案 id → 热键串）。
        //
        // 与方案启用状态同一条路——在方案页改、按主保存提交。**不能在方案对话框里
        // 直接 config.setItems**：该键是 Map，在 diff 里是原子叶子（整对象产出），
        // 主页面持有的仍是打开时的旧值，用户之后按一次保存就会把它整体覆盖回去。
        {
            let hk = schema_mgr.schema_hotkeys.clone();
            fields.writers.borrow_mut().push(Box::new(move |cfg| {
                let map: serde_json::Map<String, Value> = hk
                    .borrow()
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect();
                cfg.set_path("keys.schema_hotkeys", Value::Object(map));
            }));
        }

        // 注册 schema.special_modes 写回器：特殊方案的引导键。
        //
        // 写的是原始 `Value` 数组——条目还带 show_all_on_enter / candidate_layout /
        // 注释模板等只能手写 config.toml 的字段，用结构体重建会把它们静默抹掉。
        // 数组恒整体写回（StructList 语义），故内存表必须始终是「读进来的全量 + 本次改动」。
        {
            let sm = schema_mgr.special_modes.clone();
            fields.writers.borrow_mut().push(Box::new(move |cfg| {
                cfg.set_path("schema.special_modes", Value::Array(sm.borrow().clone()));
            }));
        }

        // 注册 schema.active 写回器：把「设为当前」的选择持久化。
        //
        // core 的 switch_schema 只改运行时活跃方案不写配置，而保存设置触发的
        // reload_from_config 会用配置里的 schema.active 重置它——不在这里写回的话，
        // 用户设为当前后再点一次「应用设置」，活跃方案就被打回原样。
        // 空串跳过：active_id 尚未从 RPC 拉到（离线/未连接）时不该把配置清空。
        {
            let active = schema_mgr.active_id.clone();
            fields.writers.borrow_mut().push(Box::new(move |cfg| {
                let id = active.borrow().clone();
                if !id.is_empty() {
                    cfg.set_path("schema.active", Value::String(id));
                }
            }));
        }

        Self {
            base_config: Config::from_value(Value::Object(serde_json::Map::new())),
            current_config: Config::from_value(Value::Object(serde_json::Map::new())),
            manifest: Manifest::from_embedded(),
            capabilities: Capabilities::from_embedded(),
            system_info: Value::Object(serde_json::Map::new()),
            schemas: vec![],
            theme_entries: vec![],
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
            dark: signal(initial_dark),
            status: signal("正在连接服务...".to_string()),
            feedback: signal(None),
            conflict: signal(String::new()),
            schema_mgr,
            dict_mgr: DictManagerState::new(),
            pending_dict_target: Rc::new(RefCell::new(None)),
            backup_mgr: crate::pages::backup::BackupState::new(),
            stats_mgr: crate::pages::stats::StatsState::new(),
            reload_confirm: signal(false),
            reset_confirm: signal(false),
            connected: false,
            data_loaded: false,
            conn_failed: signal(false),
            conn_ok: signal(false),
            theme_label_sig: signal(vec![]),
            theme_value_sig: signal(vec![]),
            theme_items_sig: signal(vec![]),
            theme_delete_id: signal(String::new()),
            theme_delete_msg: signal(String::new()),
            theme_delete_visible: signal(false),
            font_sig: signal(vec![]),
            font_label_sig: signal(vec![]),
            codetable_schema_val_sig: signal(vec![]),
            codetable_schema_lab_sig: signal(vec![]),
            pinyin_schema_val_sig: signal(vec![]),
            pinyin_schema_lab_sig: signal(vec![]),

            update_dialog_visible: signal(false),
            update_phase: signal(crate::update::UpdatePhase::Checking),
            update_progress: signal(0.0),
            update_progress_text: signal(String::new()),
            update_message: signal(String::new()),
            update_size_text: signal(String::new()),
            update_auto_check_sig: signal(setting_load_update_auto_check),
            update_notes: signal(String::new()),
            update_tx: RefCell::new(None),
            update_cancel: Arc::new(AtomicBool::new(false)),
            update_check: RefCell::new(None),
            update_installer: RefCell::new(None),
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

    // ── 在线升级 ──────────────────────────────────────────────────────────

    /// 注入后台 → UI 的消息发送端。`main()` 在 App 构建期调用一次。
    pub fn set_update_tx(&self, tx: Sender<crate::update::UpdateMsg>) {
        *self.update_tx.borrow_mut() = Some(tx);
    }

    /// 打开升级对话框。已同意联网则直接开始检查，否则先走同意页。
    ///
    /// 已同意路径的初始态取 `Checking` 而非 `Consent`，避免同意页一闪而过。
    pub fn open_update_dialog(&self) {
        self.update_notes.set(String::new());
        self.update_progress.set(0.0);
        self.update_progress_text.set(String::new());
        if self.setting_config.update.network_consent {
            self.begin_update_check();
        } else {
            self.update_phase.set(crate::update::UpdatePhase::Consent);
            self.update_message.set(String::new());
        }
        self.update_dialog_visible.set(true);
    }

    /// 用户同意联网：持久化后立即检查。
    pub fn accept_update_consent(&mut self) {
        self.setting_config.update.network_consent = true;
        if let Err(e) = crate::setting_config::save(&self.setting_config) {
            self.notify_err(e);
        }
        self.begin_update_check();
    }

    /// 切到检查中并起后台线程（用户手动触发）。
    pub fn begin_update_check(&self) {
        self.update_phase.set(crate::update::UpdatePhase::Checking);
        self.update_message.set("正在检查更新…".to_string());
        self.spawn_update_check(false);
    }

    /// 启动时的后台静默检查：不切阶段、不显示对话框，只在发现新版本时才打扰用户。
    pub fn spawn_silent_update_check(&self) {
        self.spawn_update_check(true);
    }

    fn spawn_update_check(&self, silent: bool) {
        let Some(tx) = self.update_tx.borrow().clone() else {
            if !silent {
                self.fail_update("升级通道未初始化");
            }
            return;
        };
        // 网络请求绝不能在 UI 线程做；结果经 channel 回到 UI 线程写 Signal。
        std::thread::spawn(move || {
            // 有更新时必拉说明；「已是最新」时只有手动检查才拉（静默那条不弹界面）。
            let result = crate::update::check_update(!silent).map_err(|e| e.user_message());
            let _ = tx.send(crate::update::UpdateMsg::CheckDone { result, silent });
        });
    }

    /// 记录本次检查时间，用于 24h 节流。
    pub fn mark_update_checked(&mut self, now: i64) {
        self.setting_config.update.last_check_at = now;
        if let Err(e) = crate::setting_config::save(&self.setting_config) {
            // 只影响节流精度，不该打断启动流程
            tracing::warn!("保存检查更新时间失败：{e}");
        }
    }

    /// 持久化界面状态（明暗 / 方案列表两个显示过滤）。
    ///
    /// 三处调用点各自只改一个字段再调它，故先比对再写：这些开关都是高频点击的，
    /// 每次翻转都落一次盘没必要。
    fn save_ui_state(&mut self, mutate: impl FnOnce(&mut crate::setting_config::UiStateConfig)) {
        let before = self.setting_config.ui.clone();
        mutate(&mut self.setting_config.ui);
        if self.setting_config.ui == before {
            return;
        }
        if let Err(e) = crate::setting_config::save(&self.setting_config) {
            // 只影响下次启动的界面状态，不该打断当前操作。
            tracing::warn!("保存界面状态失败：{e}");
        }
    }

    /// 持久化明暗选择。写 `Some` 即表示用户显式选过，此后不再跟随系统。
    pub fn save_dark_mode(&mut self, dark: bool) {
        self.save_ui_state(|ui| ui.dark = Some(dark));
    }

    /// 持久化方案页「隐藏未启用」。
    pub fn save_hide_disabled_schemas(&mut self, on: bool) {
        self.save_ui_state(|ui| ui.hide_disabled_schemas = on);
    }

    /// 持久化方案页「显示特殊方案」。
    pub fn save_show_hidden_schemas(&mut self, on: bool) {
        self.save_ui_state(|ui| ui.show_hidden_schemas = on);
    }

    /// 持久化「启动时自动检查更新」开关。
    pub fn save_update_auto_check(&mut self, enabled: bool) {
        if self.setting_config.update.auto_check == enabled {
            return;
        }
        self.setting_config.update.auto_check = enabled;
        if let Err(e) = crate::setting_config::save(&self.setting_config) {
            self.notify_err(e);
        }
    }

    /// 「跳过此版本」：记下版本号，该版本不再自动提示（手动检查仍会显示）。
    pub fn skip_update_version(&mut self, version: String) {
        self.setting_config.update.skipped_version = version;
        if let Err(e) = crate::setting_config::save(&self.setting_config) {
            self.notify_err(e);
        }
        self.update_dialog_visible.set(false);
    }

    /// 下载已检查到的升级包。
    pub fn spawn_update_download(&self) {
        use crate::update::UpdateMsg;

        let Some(tx) = self.update_tx.borrow().clone() else {
            self.fail_update("升级通道未初始化");
            return;
        };
        let Some(check) = self.update_check.borrow().clone() else {
            self.fail_update("没有可下载的更新");
            return;
        };

        self.update_cancel.store(false, Ordering::SeqCst);
        let cancel = self.update_cancel.clone();
        self.update_progress.set(0.0);
        self.update_progress_text.set(String::new());
        self.update_message.set("正在下载更新包…".to_string());
        self.update_phase
            .set(crate::update::UpdatePhase::Downloading);

        std::thread::spawn(move || {
            let dest = crate::update::download::cache_path(&check.download_url, "wind-update.exe");
            let progress_tx = tx.clone();
            let outcome = crate::update::download::download(
                &check.download_url,
                &check.sha256,
                check.size,
                &dest,
                cancel,
                move |downloaded, total| {
                    let _ = progress_tx.send(UpdateMsg::Progress { downloaded, total });
                },
            );
            let msg = match outcome {
                Ok(path) => UpdateMsg::DownloadDone(path),
                // 取消不是故障，单独成一条消息
                Err(e) if !e.is_failure() => UpdateMsg::DownloadCancelled,
                Err(e) => UpdateMsg::DownloadFailed(e.user_message()),
            };
            let _ = tx.send(msg);
        });
    }

    /// 请求取消下载；下载线程在下一个数据块边界响应。
    pub fn cancel_update_download(&self) {
        self.update_cancel.store(true, Ordering::SeqCst);
    }

    /// 拉起安装器。成功后调用方须立即关闭窗口 —— 安装器要终止本进程才能替换文件。
    ///
    /// 返回 `Ok(true)` 表示已启动，`Ok(false)` 表示用户在 UAC 弹窗点了「否」
    /// （主动取消，不该按错误展示）。
    pub fn launch_update_install(&self) -> Result<bool, String> {
        let installer = self
            .update_installer
            .borrow()
            .clone()
            .ok_or_else(|| "没有可安装的更新包".to_string())?;

        let plan = crate::update::install::plan(&installer).map_err(|e| e.user_message())?;
        if plan.portable {
            tracing::info!("检测到便携/非标准安装，改为拉起完整安装向导");
        }
        match crate::update::install::launch(&plan) {
            Ok(()) => Ok(true),
            Err(e) => {
                let msg = e.user_message();
                // UAC 拒绝不是故障：留在 Ready 态让用户可以再点一次
                if msg.contains("已取消提权") {
                    Ok(false)
                } else {
                    Err(msg)
                }
            }
        }
    }

    fn fail_update(&self, msg: impl Into<String>) {
        self.update_message.set(msg.into());
        self.update_phase.set(crate::update::UpdatePhase::Error);
    }

    /// core 推来的事件 → Signal。**在 UI 线程回调中执行**（`App::channel` 保证）。
    ///
    /// 只认当前用得上的事件，其余记 debug 后忽略。特别是 `config.changed`：core 是在
    /// **本端写配置后**才广播它的（dispatch 的 setItems/reload），拿它去重载配置会变成
    /// 「自己保存 → 自己重载」的回环，故不接。
    pub fn on_core_event(&mut self, ev: &crate::rpc::CoreEvent) {
        match ev.event.as_str() {
            "schema.activeChanged" => {
                if let Some(id) = ev.data.get("id").and_then(|v| v.as_str()) {
                    tracing::debug!("core 活跃方案变更 → {}", id);
                    self.schema_mgr.set_active_from_core(id);
                }
            }
            // core 侧配置落盘（托盘/右键菜单、快捷键、命令栏改的配置）。
            // dispatch 层在 setItems/reload 后也发同名事件，但不带 key，由下面的
            // 取值失败自然跳过——那条是本端自己写的，无需处理。
            "config.changed" => {
                let (Some(key), Some(value)) = (
                    ev.data.get("key").and_then(|v| v.as_str()),
                    ev.data.get("value"),
                ) else {
                    return;
                };
                self.apply_external_config_change(key, value.clone());
            }
            other => tracing::debug!("忽略 core 事件: {}", other),
        }
    }

    /// 把 core 侧落盘的单个配置项同步进界面。
    ///
    /// 两件事缺一不可：
    /// - 更新 `base_config`，否则保存时的增量 diff 会把这一项当成"本端改的"再写一遍；
    /// - 调该 key 的 loader 把值灌回 Signal，界面才跟着变。
    ///
    /// 未在清单登记的 key（如拖动落位写的 `ui.status.custom_x`）只更新 base，不找
    /// loader——它们本就不在设置页露出。
    ///
    /// 取舍：若用户此刻正在设置页编辑同一项，这里会覆盖掉他的未保存改动。没有为此
    /// 加脏标记，因为"同时在菜单和设置页改同一项"几乎不会发生，而外部改动优先也是
    /// 说得通的语义；加了反而要维护逐项脏状态。
    pub fn apply_external_config_change(&mut self, key: &str, value: Value) {
        self.base_config.set_path(key, value.clone());
        self.current_config.set_path(key, value);

        // 同一配置路径可能有多个控件（视图 key 形如 `<路径>#<视图名>`），逐个灌回。
        let cfg = self.base_config.clone();
        let loaders = self.fields.loaders.borrow();
        let mut hit = 0usize;
        for (k, l) in loaders.iter() {
            if loader_key_matches(k, key) {
                l(&cfg);
                hit += 1;
            }
        }
        tracing::debug!("外部配置变更 {} → 回填 {} 个控件", key, hit);
    }

    /// 后台消息 → Signal。**在 UI 线程回调中执行**（`App::channel` 保证）。
    pub fn on_update_msg(&self, msg: crate::update::UpdateMsg) {
        use crate::update::{UpdateMsg, UpdatePhase, format_size};

        match msg {
            UpdateMsg::CheckDone {
                result: Ok(r),
                silent: true,
            } => {
                // 静默检查：无更新、已跳过的版本、开发构建，一律不打扰
                let skipped = !self.setting_config.update.skipped_version.is_empty()
                    && self.setting_config.update.skipped_version == r.remote_version;
                if !r.has_update || skipped {
                    // 四个维度都要记：只打 has_update 无法区分「已是最新」「无 tag 的开发
                    // 构建被跳过」「命中跳过版本」，而静默路径本就没有任何界面反馈。
                    tracing::info!(
                        "静默检查完成（不打扰）：local={} remote={} has_update={} local_unparsable={} skipped={}",
                        r.local_version,
                        r.remote_version,
                        r.has_update,
                        r.local_unparsable,
                        skipped
                    );
                    *self.update_check.borrow_mut() = Some(r);
                    return;
                }
                self.update_message.set(format!(
                    "发现新版本 {}（当前 {}）",
                    r.remote_version, r.local_version
                ));
                self.update_notes.set(r.release_notes.clone());
                self.update_size_text
                    .set(format!("更新包 {}", r.size_display()));
                self.update_phase.set(UpdatePhase::HasUpdate);
                *self.update_check.borrow_mut() = Some(r);
                self.update_dialog_visible.set(true);
            }
            // 静默检查失败：只记日志。启动时弹一个"检查更新失败"对用户毫无价值
            UpdateMsg::CheckDone {
                result: Err(e),
                silent: true,
            } => {
                tracing::info!("静默检查更新失败（已忽略）：{e}");
            }
            UpdateMsg::CheckDone {
                result: Ok(result),
                silent: false,
            } => {
                if result.has_update {
                    self.update_message.set(format!(
                        "发现新版本 {}（当前 {}）",
                        result.remote_version, result.local_version
                    ));
                    self.update_notes.set(result.release_notes.clone());
                    self.update_size_text
                        .set(format!("更新包 {}", result.size_display()));
                    self.update_phase.set(UpdatePhase::HasUpdate);
                } else if result.local_unparsable {
                    // 无 tag 的开发构建：说明为何不提示，而不是谎称"已是最新"
                    self.update_message
                        .set("当前为开发构建，已跳过版本比较".to_string());
                    self.update_notes.set(result.release_notes.clone());
                    self.update_phase.set(UpdatePhase::NoUpdate);
                } else {
                    self.update_message
                        .set(format!("已是最新版本（{}）", result.remote_version));
                    // 已是最新也展示最新版的更新记录——用户点开检查更新，多半也想知道
                    // 自己手上这个版本都带了什么。
                    self.update_notes.set(result.release_notes.clone());
                    self.update_phase.set(UpdatePhase::NoUpdate);
                }
                *self.update_check.borrow_mut() = Some(result);
            }
            UpdateMsg::CheckDone {
                result: Err(e),
                silent: false,
            } => self.fail_update(e),
            UpdateMsg::Progress { downloaded, total } => {
                let ratio = if total > 0 {
                    downloaded as f32 / total as f32
                } else {
                    0.0
                };
                self.update_progress.set(ratio.clamp(0.0, 1.0));
                self.update_progress_text.set(format!(
                    "{} / {}",
                    format_size(downloaded),
                    format_size(total)
                ));
            }
            UpdateMsg::DownloadDone(path) => {
                *self.update_installer.borrow_mut() = Some(path);
                self.update_progress.set(1.0);
                // 预告窗口会关闭：安装器要终止本进程才能替换文件，事后再提示已来不及
                self.update_message.set(
                    "下载完成，完整性校验通过。\n点击安装后设置窗口将关闭，安装完成后可重新打开。"
                        .to_string(),
                );
                self.update_phase.set(UpdatePhase::Ready);
            }
            UpdateMsg::DownloadFailed(e) => self.fail_update(e),
            UpdateMsg::DownloadCancelled => {
                // 退回到可重试的状态，不显示错误
                self.update_progress.set(0.0);
                self.update_progress_text.set(String::new());
                if let Some(c) = self.update_check.borrow().as_ref() {
                    self.update_message.set(format!(
                        "已取消下载。发现新版本 {}（当前 {}）",
                        c.remote_version, c.local_version
                    ));
                }
                self.update_phase.set(UpdatePhase::HasUpdate);
            }
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
        self.theme_entries = load_theme_entries();
        self.apply_theme_entries();
    }

    /// 依 `theme_entries` 刷新三个下拉信号：显示名（label）、id（value）、富内容项（版本徽章 +
    /// 外部主题删除图标）。删除图标点击 → 填充删除确认对话框的待删 id/正文并弹出。
    fn apply_theme_entries(&self) {
        let labels: Vec<String> = self
            .theme_entries
            .iter()
            .map(|e| e.display.clone())
            .collect();
        let values: Vec<String> = self.theme_entries.iter().map(|e| e.id.clone()).collect();
        let items = build_theme_items(
            &self.theme_entries,
            self.theme_delete_id,
            self.theme_delete_msg,
            self.theme_delete_visible,
        );
        self.theme_label_sig.set(labels);
        self.theme_value_sig.set(values);
        self.theme_items_sig.set(items);
    }

    /// 执行主题删除（删除确认对话框「删除」按钮触发）：调 `theme.delete`，成功后刷新列表并反馈。
    pub fn confirm_delete_theme(&mut self) {
        self.theme_delete_visible.set(false);
        let id = self.theme_delete_id.get();
        if id.is_empty() {
            return;
        }
        match rpc::call_core("theme.delete", &serde_json::json!({ "name": id })) {
            Ok(_) => {
                self.refresh_theme_names();
                self.notify_ok("已删除主题");
            }
            Err(e) => self.notify_err(format!("删除主题失败：{e}")),
        }
        self.theme_delete_id.set(String::new());
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

    /// 词库页深链统一入口：方案列表已就绪则立即定位，否则挂起等 [`Self::apply_loaded`] 消费。
    /// 冷启动（列表未就绪）与二次实例激活（已就绪）共用此入口，避免两条易走偏的路径。
    pub fn route_dict_target(&self, target: crate::cli::DictTarget) {
        if target.is_empty() {
            return;
        }
        if self.data_loaded {
            self.apply_dict_target(&target);
        } else {
            *self.pending_dict_target.borrow_mut() = Some(target);
        }
    }

    /// 消费挂起的深链（`apply_loaded` 末尾调用）。无挂起则无操作。
    fn consume_pending_dict_target(&self) {
        let pending = self.pending_dict_target.borrow_mut().take();
        if let Some(t) = pending {
            self.apply_dict_target(&t);
        }
    }

    /// 执行深链定位并汇总提示。偏差一律降级 + toast，不阻断——用户的意图是"打开看看"，
    /// 把他挡在词库页外面没有意义。多条偏差合并成一条 toast，免得连弹两次。
    fn apply_dict_target(&self, t: &crate::cli::DictTarget) {
        let mut notes: Vec<String> = Vec::new();
        if !t.bad_tab.is_empty() {
            notes.push(format!("未知的词库类型「{}」", t.bad_tab));
        }
        if let Some(n) = self.dict_mgr.apply_deep_link(&t.schema, t.tab) {
            notes.push(n);
        }
        if !notes.is_empty() {
            self.feedback.set(Some((ToastKind::Info, notes.join("；"))));
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
        // 统计页「方案占比」的 id→名映射（页面树早于本次加载建成，故走信号而非快照）。
        self.stats_mgr.schema_names.set(
            self.schemas
                .iter()
                .map(|s| (s.id.clone(), s.name.clone()))
                .collect(),
        );
        self.theme_entries = data.theme_entries;
        self.fonts = data.fonts;
        self.apply_theme_entries();
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
        // 方案下拉选项（码表/拼音类各自过滤）：首项空值="自动"/"默认：全拼"，其余为 id/name。
        // 必须在 loaders 跑之前就位，否则 loader 无法在选项列表中定位当前配置值。
        //
        // 走 manifest::schema_options —— 与清单 options_source 路径**同一份实现**。此处曾内联
        // 手写一遍，与那份漂成了两套语义（空值标签、kind 为空时是否排除），且生效的是这份，
        // 于是改另一份的人会发现"改了没反应"。
        {
            let (ct_vals, ct_labs) =
                crate::manifest::schema_options(&self.schemas, Some("codetable"));
            self.codetable_schema_val_sig.set(ct_vals);
            self.codetable_schema_lab_sig.set(ct_labs);
            let (py_vals, py_labs) = crate::manifest::schema_options(&self.schemas, Some("pinyin"));
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
        // 词库页数据域顺序（仅作用于词库页；不改 self.schemas，故主码表/主拼音下拉、方案管理列表均不受影响）。
        let dict_schemas =
            order_schemas_for_dict(&self.schemas, &self.schema_mgr.enabled_order.borrow());
        self.dict_mgr.set_schemas(&dict_schemas);
        // 深链必须在 set_schemas 之后：那之前 folded_domains 为空，--schema 无从解析；
        // 且 set_schemas_no_reload 会清空 sub_tab_memory、把越界 domain 收敛回 0。
        self.data_loaded = true;
        self.consume_pending_dict_target();
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
    /// 结果按**级别**分流，两者呈现方式不同（见 [`ConflictLevel`]）：
    /// - `blocking`：各域摘要用换行拼成一条文案（可能各一行），驱动常驻警告顶栏；
    /// - `coexist`：可用的分时复用，交给 `conflict_watcher` 在**刚配出来那一刻** Toast 一次。
    ///
    /// 供 `conflict_watcher` 在渲染帧刷新——每次求值 clone 配置并跑全部写回器，
    /// 但渲染帧由交互/动画驱动（空闲不渲染），故非 60fps 常态开销。
    pub fn live_conflicts(&self) -> LiveConflicts {
        let mut probe = self.current_config.clone();
        for w in self.fields.writers.borrow().iter() {
            w(&mut probe);
        }
        let mut lines = Vec::new();
        let mut coexist = Vec::new();
        for (groups, scene) in [
            (collect_trigger_key_groups(&probe), "触发键"),
            (collect_candidate_key_groups(&probe), "候选键"),
            (collect_hotkey_groups(&probe), "快捷键"),
        ] {
            let (blocking, mut co) = check_domain(groups, scene);
            if let Some(m) = blocking {
                lines.push(m);
            }
            coexist.append(&mut co);
        }
        LiveConflicts {
            blocking: lines.join("\n"),
            coexist,
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
        self.feedback
            .set(Some((ToastKind::Info, "已从服务端重新加载".to_string())));
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
        let items = diff_config(
            &self.base_config,
            &self.current_config,
            &self.capabilities.map_keys(),
        );
        if items.is_empty() {
            self.feedback
                .set(Some((ToastKind::Info, "没有变更".to_string())));
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
                self.feedback
                    .set(Some((ToastKind::Error, format!("保存失败: {e}"))));
                false
            }
        }
    }
}

/// 收集当前配置里"裸键触发"四组，供按键冲突检测用。
fn collect_trigger_key_groups(cfg: &Config) -> Vec<KeyGroup> {
    fn strs(v: Option<&Vec<Value>>) -> Vec<String> {
        v.map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
    }
    fn nested_keys(m: &Value) -> Vec<String> {
        m.get("trigger_keys")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    let mut groups = vec![
        KeyGroup::new(
            "temp_pinyin",
            "临时拼音",
            strs(cfg.get_array("input.temp_pinyin.trigger_keys")),
        ),
        KeyGroup::new(
            "temp_english",
            "临时英文",
            strs(cfg.get_array("input.temp_english.trigger_keys")),
        ),
    ];

    if let Some(arr) = cfg.get_array("schema.mix_modes") {
        for (i, m) in arr.iter().enumerate() {
            let label = m
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("混合模式")
                .to_string();
            groups.push(KeyGroup::new(&format!("mix_{i}"), label, nested_keys(m)));
        }
    }

    if let Some(arr) = cfg.get_array("schema.special_modes") {
        for (i, m) in arr.iter().enumerate() {
            let name = m.get("name").and_then(Value::as_str).unwrap_or("");
            groups.push(KeyGroup::new(
                &format!("special_{i}"),
                format!("特殊模式「{name}」"),
                nested_keys(m),
            ));
        }
    }

    // 去重每组触发键：mix_modes/special_modes 的 trigger_keys 直取自 core config 数组，
    // 可能含重复值；不去重会让冲突文案对同一键重复呈现。
    for g in groups.iter_mut() {
        let mut seen = std::collections::HashSet::new();
        g.keys.retain(|k| seen.insert(k.clone()));
    }
    groups
}

/// 一次实时检测的结果，按呈现方式分流（见 [`AppState::live_conflicts`]）。
#[derive(Debug, Clone, Default)]
pub struct LiveConflicts {
    /// 常驻警告顶栏文案；空串 = 无需警告。
    pub blocking: String,
    /// 可共存的分时复用，供 Toast 一次性告知实际行为。
    pub coexist: Vec<Conflict>,
}

/// 对一个场景域跑冲突检测，把 Blocking 摘要（冠 `scene` 标签）与 Coexist 明细分开返回。
fn check_domain(groups: Vec<KeyGroup>, scene: &str) -> (Option<String>, Vec<Conflict>) {
    let hits = crate::key_conflict::find_key_conflicts(&groups);
    let coexist = hits
        .iter()
        .filter(|c| c.level == ConflictLevel::Coexist)
        .cloned()
        .collect();
    (crate::key_conflict::conflict_summary(&hits, scene), coexist)
}

/// 收集"候选操作"域各组的 (显示名, 物理键列表)，组名经 `expand_candidate_key` 展开
/// （tab/shift_tab 归一、lrshift/lrctrl 拆成单修饰键），供有候选场景下的按键冲突检测。
///
/// 含中英切换键：core 在 KEY_UP 处理中英切换，即便候选窗打开也会触发（`coordinator.rs:3225`），
/// 与次选键(lrshift/lrctrl)争用同一物理修饰键——有候选时按下即选词、切换被抑制，是真冲突。
/// 详见 `docs/key-conflict-detection.md`。
fn collect_candidate_key_groups(cfg: &Config) -> Vec<KeyGroup> {
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
        KeyGroup::new(
            "select_keys",
            "次选/三选键",
            expand_strs(cfg.get_array("keys.select_key_groups")),
        ),
        KeyGroup::new(
            "page_keys",
            "翻页键",
            expand_strs(cfg.get_array("keys.page_keys")),
        ),
        KeyGroup::new(
            "highlight_keys",
            "高亮移动键",
            expand_strs(cfg.get_array("keys.highlight_keys")),
        ),
        KeyGroup::new(
            "select_char",
            "以词定字",
            expand_strs(cfg.get_array("keys.select_char_keys")),
        ),
        KeyGroup::new(
            "toggle_mode",
            "中英切换键",
            expand_strs(cfg.get_array("keys.toggle_mode_keys")),
        ),
    ]
}

/// 收集功能快捷键（组合键）各功能的分组，供组合键冲突检测。
/// 组合键都带修饰键、是显式命令，不涉及裸键的按场景分派，故统一为一个域检测。
///
/// 覆盖三类来源——**它们在内核是同一张 key_down 表**（`wind-config/src/hotkey.rs` 的
/// `compile`），匹配走 `.find()`，即先注册者赢、后者静默失效，所以必须放在一个域里比：
/// 1. `[keys]` 下的 9 个 hotkey 项 + `pin_candidate`/`delete_candidate`（模板值，见 `expand_hotkey`）；
/// 2. `keys.schema_hotkeys`（方案直达热键，方案管理对话框可编辑）；
/// 3. `input.temp_pinyin.hotkey` 与 `schema.special_modes[].hotkey`（模式直达热键）。
///
/// 2、3 两类此前**完全没有参与检测**：把某方案的直达热键配成 `Ctrl+Shift+E`（= 切换方案的
/// 出厂值）在对话框里是点得出来的，配完毫无提示、按下去也只有一个生效。
fn collect_hotkey_groups(cfg: &Config) -> Vec<KeyGroup> {
    use crate::key_conflict::expand_hotkey;

    const HOTKEYS: &[(&str, &str)] = &[
        ("keys.switch_engine", "切换输入方案"),
        ("keys.toggle_full_width", "切换全角/半角"),
        ("keys.toggle_punct", "切换中/英文标点"),
        ("keys.toggle_s2t", "切换简入繁出"),
        ("keys.toggle_toolbar", "显示/隐藏工具栏"),
        ("keys.open_settings", "打开设置"),
        ("keys.add_word", "快捷加词"),
        ("keys.open_add_word_dialog", "打开加词界面"),
        ("keys.activate_ime", "切换到本输入法"),
        ("keys.pin_candidate", "置顶词条"),
        ("keys.delete_candidate", "删除词条"),
    ];
    let mut groups: Vec<KeyGroup> = HOTKEYS
        .iter()
        .map(|(key, label)| {
            KeyGroup::new(key, *label, expand_hotkey(cfg.get_str(key).unwrap_or("")))
        })
        .collect();

    // 方案直达热键：Map<方案 id, 热键串>。按 id 排序保证同一份配置每次得到同样的报告顺序
    // （内核编译期也排序，见 hotkey.rs 里 schema_hotkeys 的 sort_by）。
    if let Some(map) = cfg.get_object("keys.schema_hotkeys") {
        let mut entries: Vec<(&String, &Value)> = map.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (id, v) in entries {
            if id.is_empty() {
                continue;
            }
            groups.push(KeyGroup::new(
                &format!("schema_hotkey_{id}"),
                format!("切换到方案「{id}」"),
                expand_hotkey(v.as_str().unwrap_or("")),
            ));
        }
    }

    groups.push(KeyGroup::new(
        "temp_pinyin_hotkey",
        "临时拼音直达热键",
        expand_hotkey(cfg.get_str("input.temp_pinyin.hotkey").unwrap_or("")),
    ));

    if let Some(arr) = cfg.get_array("schema.special_modes") {
        for (i, m) in arr.iter().enumerate() {
            // 内核对 id 为空的条目直接跳过编译（hotkey.rs），故这里也不参与检测——
            // 报一个根本不会注册的热键的冲突，只会让用户去改一个无关的地方。
            if m.get("id").and_then(Value::as_str).unwrap_or("").is_empty() {
                continue;
            }
            let name = m.get("name").and_then(Value::as_str).unwrap_or("");
            groups.push(KeyGroup::new(
                &format!("special_hotkey_{i}"),
                format!("特殊模式「{name}」直达热键"),
                expand_hotkey(m.get("hotkey").and_then(Value::as_str).unwrap_or("")),
            ));
        }
    }

    groups
}

/// 后台线程从 core 拉取的全部数据（所有字段均 `Send`，可跨线程传递）。
pub struct LoadedState {
    pub config_value: Value,
    pub system_info: Value,
    pub capabilities_value: Value,
    pub schemas: Vec<SchemaInfo>,
    pub theme_entries: Vec<ThemeEntry>,
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
        let h_themes = std::thread::spawn(load_theme_entries);
        let h_fonts = std::thread::spawn(load_fonts);

        let config_res = h_config.join().ok().and_then(|r| r.ok());
        let system_info = h_info
            .join()
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_else(Self::empty_obj);
        let caps_res = h_caps.join().ok().and_then(|r| r.ok());
        let schemas = h_schemas.join().unwrap_or_default();
        let theme_entries = h_themes.join().unwrap_or_default();
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
            theme_entries,
            fonts,
            connected,
        }
    }
}

/// 词库页数据域下拉的方案排序（三段式，仅供词库页；不影响主码表/主拼音下拉与方案管理列表）：
///   ① 已启用的拼音方案置顶；
///   ② 其余已启用方案，按 `enabled_order`（即 `schema.available`）配置顺序；
///   ③ 未启用方案，按类型分组「拼音→码表→混输」，组内按 id 字典序。
///
/// `enabled_order` 为空表示「全部启用」（与 `SchemaManagerState` 语义一致），此时段①②内部按 `schemas` 原始序。
/// 排序稳定：段号相同者保持相对顺序。
/// 清单项 key 是否对应配置路径 `path`。
///
/// 命中两种形态：路径本身，以及同一路径的视图 key `<路径>#<视图名>`（同一配置项
/// 需要两个控件时的约定，见 [`FieldRegistry::loaders`]）。**`#` 分隔符不可省**——
/// 直接用 `starts_with(path)` 会让 `ui.status.display_mode` 误伤
/// `ui.status.display_mode_extra` 这类同前缀的邻居键，且错了是静默的。
fn loader_key_matches(loader_key: &str, path: &str) -> bool {
    loader_key
        .strip_prefix(path)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('#'))
}

pub(crate) fn order_schemas_for_dict(
    schemas: &[SchemaInfo],
    enabled_order: &[String],
) -> Vec<SchemaInfo> {
    let all_enabled = enabled_order.is_empty();
    let type_rank = |k: &str| match k {
        "pinyin" => 0i64,
        "codetable" => 1,
        "mixed" => 2,
        _ => 3,
    };
    let mut v: Vec<SchemaInfo> = schemas.to_vec();
    v.sort_by_cached_key(|s| {
        let enabled_pos = enabled_order.iter().position(|id| id == &s.id);
        let is_enabled = all_enabled || enabled_pos.is_some();
        if is_enabled {
            // 段①拼音 / 段②其余；段内：有配置按配置位置，全部启用按原始序。
            let seg = if s.kind == "pinyin" { 0u8 } else { 1 };
            let within = enabled_pos
                .or_else(|| schemas.iter().position(|x| x.id == s.id))
                .unwrap_or(usize::MAX) as i64;
            (seg, within, String::new())
        } else {
            // 段③未启用：类型分组 + id 字典序。
            (2u8, type_rank(&s.kind), s.id.clone())
        }
    });
    v
}

/// 加载方案列表（schema.list）。
pub(crate) fn load_schemas() -> Vec<SchemaInfo> {
    // includeHidden：特殊方案（英文、快符）也要拿到——它们有独立的用户词库与词频，
    // 词库页与统计页都得能选中。不需要它们的消费点按 `hidden` 自行排除。
    let list = rpc::call_core("schema.list", &serde_json::json!({ "includeHidden": true }))
        .unwrap_or(Value::Array(vec![]));
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
                    let hidden = s.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false);
                    Some(SchemaInfo {
                        id,
                        name,
                        kind,
                        scheme,
                        hidden,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 加载主题列表（theme.list）→ [`ThemeEntry`]。
/// id 取 `id`/`slug`/`name`（保存到 `ui.theme.name`）；显示名取 `display_name`/`name`（回退 id）；
/// 版本取 `version`（下拉徽章展示）；`builtin` 缺省视为 true（保守：未知来源不给删除按钮）。
fn load_theme_entries() -> Vec<ThemeEntry> {
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
                        .unwrap_or(&id)
                        .to_string();
                    let version = t
                        .get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let builtin = t.get("builtin").and_then(|v| v.as_bool()).unwrap_or(true);
                    Some(ThemeEntry {
                        id,
                        display,
                        version,
                        builtin,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 由主题条目构建下拉富内容项：版本号作尾随徽章（中性色），外部主题（`builtin==false`）追加
/// 可点击的删除图标——点击只填充删除确认对话框的待删 id / 正文并弹出，不选中该项。
fn build_theme_items(
    entries: &[ThemeEntry],
    del_id: Signal<String>,
    del_msg: Signal<String>,
    del_visible: Signal<bool>,
) -> Vec<DropdownItem> {
    entries
        .iter()
        .map(|e| {
            let mut it = DropdownItem::new(e.display.clone());
            if !e.version.is_empty() {
                it = it.badge(format!("v{}", e.version), Intent::Neutral);
            }
            if !e.builtin {
                let (id, disp) = (e.id.clone(), e.display.clone());
                it = it.trailing_icon("✕", move || {
                    del_id.set(id.clone());
                    del_msg.set(format!("确定删除外部主题「{disp}」？删除后不可恢复。"));
                    del_visible.set(true);
                });
            }
            it
        })
        .collect()
}

#[cfg(test)]
mod trigger_conflict_tests {
    use super::*;
    use serde_json::json;

    // 三个域的 Blocking 摘要快捷方式（历史用例按域断言文案，保留这层薄封装免得逐个改写）。
    fn check_trigger_key_conflicts(cfg: &Config) -> Option<String> {
        check_domain(collect_trigger_key_groups(cfg), "触发键").0
    }
    fn check_candidate_key_conflicts(cfg: &Config) -> Option<String> {
        check_domain(collect_candidate_key_groups(cfg), "候选键").0
    }
    fn check_hotkey_conflicts(cfg: &Config) -> Option<String> {
        check_domain(collect_hotkey_groups(cfg), "快捷键").0
    }
    /// 某域的可共存明细。
    fn coexist_of(groups: Vec<KeyGroup>) -> Vec<Conflict> {
        check_domain(groups, "候选键").1
    }

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
        let msg = st.live_conflicts().blocking;
        assert!(!msg.is_empty(), "编辑态含重叠触发键，应实时检出冲突");
        assert!(msg.contains("触发键冲突"), "应标注触发键场景");
        assert!(msg.contains(';'), "键名应以符号呈现");
        assert!(msg.contains("临时拼音"));
        assert!(msg.contains("快捷"));
        // 无冲突时警告文案为空。
        st.current_config = Config::from_value(json!({
            "input": { "temp_pinyin": { "trigger_keys": ["backtick"] } }
        }));
        let clean = st.live_conflicts();
        assert!(clean.blocking.is_empty(), "无重叠应无冲突");
        assert!(clean.coexist.is_empty());
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
        assert!(
            check_trigger_key_conflicts(&cfg3).is_none(),
            "触发域无内部重叠"
        );
        assert!(
            check_candidate_key_conflicts(&cfg3).is_none(),
            "候选域无内部重叠"
        );
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

        // pin/delete 同为 Ctrl+数字 → 冲突。模板展开成十个具体键（见 `expand_hotkey`），
        // 文案按「等 N 个键」缩写，而不是逐个列出十行。
        let cfg3 = Config::from_value(json!({
            "keys": { "pin_candidate": "ctrl+number", "delete_candidate": "ctrl+number" }
        }));
        let msg3 = check_hotkey_conflicts(&cfg3).expect("pin/delete 同键应冲突");
        assert!(msg3.contains("等 10 个键"), "实际: {msg3}");
        assert!(msg3.contains("置顶词条") && msg3.contains("删除词条"));
    }

    /// 中英切换键 × 修饰键次选键：**可共存**，不进警告条、只给行为说明。
    ///
    /// 这条用例的断言方向被反转过：内核 2026-08-03 前，次选吃掉 KEY_DOWN、切换在 KEY_UP
    /// 等不到 `had_pending`，右 Shift 的切换意图确实静默丢失，是真冲突；现在修饰键选词改走
    /// KEY_UP 轻敲并明确裁决为「有候选选词、无候选切换」，两个功能都在。
    /// 若哪天内核又改回去，这里会先红——这正是它存在的意义。
    #[test]
    fn test_toggle_mode_vs_select_key_coexists() {
        let cfg = Config::from_value(json!({
            "keys": {
                "toggle_mode_keys": ["lshift", "rshift"],
                "select_key_groups": ["lrshift"]
            }
        }));
        assert!(
            check_candidate_key_conflicts(&cfg).is_none(),
            "可共存的分时复用不该进常驻警告条"
        );
        let co = coexist_of(collect_candidate_key_groups(&cfg));
        assert_eq!(co.len(), 2, "左右 Shift 各一条");
        let note = crate::key_conflict::coexist_note(&co[0]).expect("应给出行为说明");
        assert!(note.contains("中英切换键") && note.contains("次选/三选键"));

        // 中英切换用 Shift、次选用「分号/引号」→ 不同物理键，两级都不该有。
        let cfg2 = Config::from_value(json!({
            "keys": {
                "toggle_mode_keys": ["lshift"],
                "select_key_groups": ["semicolon_quote"]
            }
        }));
        assert!(check_candidate_key_conflicts(&cfg2).is_none());
        assert!(coexist_of(collect_candidate_key_groups(&cfg2)).is_empty());
    }

    /// 方案直达热键此前**完全没参与检测**：在方案管理对话框里把某方案配成
    /// `Ctrl+Shift+E`（＝切换方案的出厂值）毫无提示，实际只有一个会生效。
    #[test]
    fn test_schema_hotkey_conflicts_with_builtin_hotkey() {
        let cfg = Config::from_value(json!({
            "keys": {
                "switch_engine": "ctrl+shift+e",
                "schema_hotkeys": { "wubi86": "ctrl+shift+e" }
            }
        }));
        let msg = check_hotkey_conflicts(&cfg).expect("方案直达热键应参与快捷键域检测");
        assert!(msg.contains("Ctrl+Shift+E"));
        assert!(msg.contains("切换输入方案"));
        assert!(msg.contains("切换到方案「wubi86」"));
    }

    /// 模式直达热键（临拼 / 特殊模式）同样进快捷键域——它们与 `[keys]` 那批
    /// 在内核是同一张 key_down 表，撞了就是先注册者赢。
    #[test]
    fn test_mode_direct_hotkeys_join_hotkey_domain() {
        let cfg = Config::from_value(json!({
            "keys": { "toggle_s2t": "ctrl+shift+j" },
            "input": { "temp_pinyin": { "hotkey": "ctrl+shift+j" } },
            "schema": { "special_modes": [] }
        }));
        let msg = check_hotkey_conflicts(&cfg).expect("临拼直达热键应参与检测");
        assert!(msg.contains("临时拼音直达热键"));
        assert!(msg.contains("切换简入繁出"));

        let cfg2 = Config::from_value(json!({
            "keys": { "open_settings": "ctrl+shift+p" },
            "schema": { "special_modes": [
                { "id": "fast_symbols", "name": "快符", "hotkey": "ctrl+shift+p" }
            ]}
        }));
        let msg2 = check_hotkey_conflicts(&cfg2).expect("特殊模式直达热键应参与检测");
        assert!(msg2.contains("特殊模式「快符」直达热键"));
    }

    /// id 为空的特殊模式内核根本不注册热键，报它只会把用户引去改无关的地方。
    #[test]
    fn test_special_mode_without_id_is_not_reported() {
        let cfg = Config::from_value(json!({
            "keys": { "open_settings": "ctrl+shift+p" },
            "schema": { "special_modes": [
                { "id": "", "name": "没写 id 的", "hotkey": "ctrl+shift+p" }
            ]}
        }));
        assert!(check_hotkey_conflicts(&cfg).is_none());
    }

    /// `Ctrl+数字` 模板占满整段数字键，与具体的 `Ctrl+1` 重叠——GUI 主键列表里有 0-9，
    /// 这是点得出来的配置。
    #[test]
    fn test_number_template_overlaps_specific_digit_in_domain() {
        let cfg = Config::from_value(json!({
            "keys": { "pin_candidate": "ctrl+number", "open_settings": "ctrl+1" }
        }));
        let msg = check_hotkey_conflicts(&cfg).expect("模板与具体数字键应检出重叠");
        assert!(msg.contains("Ctrl+1"));
        assert!(msg.contains("置顶词条") && msg.contains("打开设置"));
    }

    /// **出厂默认必须两级皆空**：既不该有警告条，也不该一打开设置页就弹共存 toast。
    ///
    /// 基线取 `mockdata/config.json`，它由 `regenerate_mock_config` 从主仓
    /// （`wind-config` 代码默认 ⊕ `data/config.toml`）现算，已有守护测试钉住其与 core 一致。
    /// 于是本用例实际断言的是**主仓的出厂配置本身没有按键打架**——某天有人把某个默认值
    /// 改成与别处撞车的键，这里会红，而不是等用户装完发现某个功能按下去没反应。
    ///
    /// 注意默认配置里 `;` 既是内置「快捷」的触发键、又在次选键组 `semicolon_quote` 里：
    /// 那是跨域分时复用（空缓冲触发 / 有候选选词），按 §1 原则本就不该报。
    #[test]
    fn test_factory_defaults_have_no_conflicts() {
        let cfg = Config::from_value(
            serde_json::from_str(include_str!("mockdata/config.json"))
                .expect("mockdata/config.json 应是合法 JSON"),
        );
        assert_eq!(
            check_trigger_key_conflicts(&cfg),
            None,
            "出厂默认的触发键不应互撞"
        );
        assert_eq!(
            check_candidate_key_conflicts(&cfg),
            None,
            "出厂默认的候选操作键不应互撞"
        );
        assert_eq!(
            check_hotkey_conflicts(&cfg),
            None,
            "出厂默认的功能快捷键不应互撞"
        );
        assert!(
            coexist_of(collect_candidate_key_groups(&cfg)).is_empty(),
            "出厂默认不该触发共存提示——否则每个新用户第一次打开设置页都会看到它"
        );
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
        assert_eq!(
            msg.matches("同时被").count(),
            1,
            "重复触发键不应产生重复文案"
        );
    }
}

// 测试模块后仍有 load_schemas 等私有辅助函数；局部豁免 toolchain 新增的风格 lint。
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;

    /// 所有涉及 WIND_RPC_MOCK 环境变量的测试必须持有此锁，防止并行 race。
    /// 锁定义在 `rpc` 模块并全 crate 共用 —— 本模块曾自建一把，与 `theme_import`
    /// 的另一把互不相识，造成间歇性失败（见 rpc.rs 的说明）。
    use crate::rpc::ENV_LOCK;

    fn si(id: &str, kind: &str) -> SchemaInfo {
        SchemaInfo {
            id: id.to_string(),
            name: id.to_string(),
            kind: kind.to_string(),
            scheme: String::new(),
            hidden: false,
        }
    }

    #[test]
    fn dict_order_three_segments() {
        // 原始序刻意打乱，验证三段式重排结果与启用/类型无关于输入顺序。
        let schemas = vec![
            si("zhengma", "codetable"), // 未启用码表
            si("wubi98", "codetable"),  // 已启用码表
            si("cangjie", "codetable"), // 未启用码表
            si("pinyin", "pinyin"),     // 已启用拼音
            si("wubi_py", "mixed"),     // 已启用混输
            si("shuangpin", "pinyin"),  // 未启用拼音
        ];
        // 配置顺序（已启用集合，含先后）：wubi98 在 pinyin 之前，但拼音应被段①提前。
        let enabled = vec![
            "wubi98".to_string(),
            "pinyin".to_string(),
            "wubi_py".to_string(),
        ];
        let ordered: Vec<String> = order_schemas_for_dict(&schemas, &enabled)
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(
            ordered,
            vec![
                "pinyin",    // 段①：已启用拼音置顶
                "wubi98",    // 段②：其余已启用，按配置顺序
                "wubi_py",   //       （配置里 wubi98 → wubi_py）
                "shuangpin", // 段③：未启用，类型分组 拼音在前
                "cangjie",   //       未启用码表，组内 id 字典序 cangjie < zhengma
                "zhengma",
            ]
        );
    }

    #[test]
    fn dict_order_all_enabled_keeps_original_order_pinyin_first() {
        // enabled_order 为空 = 全部启用：段①②内部按原始序，仅拼音整体提前。
        let schemas = vec![
            si("wubi98", "codetable"),
            si("pinyin", "pinyin"),
            si("wubi_py", "mixed"),
        ];
        let ordered: Vec<String> = order_schemas_for_dict(&schemas, &[])
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ordered, vec!["pinyin", "wubi98", "wubi_py"]);
    }

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

    /// 守护：外部配置变更回填时的 key 匹配规则。
    /// 匹配少了 → 界面不更新；匹配多了 → 改 A 却顺手覆盖了同前缀的 B。两种都是静默的。
    #[test]
    fn loader_key_matches_path_and_view_keys() {
        let p = "ui.status.display_mode";
        assert!(loader_key_matches(p, p), "路径本身");
        assert!(
            loader_key_matches("ui.status.display_mode#compact", p),
            "视图 key"
        );
        // 同前缀的邻居键不可误伤——少了 '#' 判断就会中招。
        assert!(!loader_key_matches("ui.status.display_mode_extra", p));
        assert!(!loader_key_matches("ui.status.display", p));
        assert!(!loader_key_matches("ui.theme.name", p));
    }

    #[test]
    fn refresh_theme_names_updates_options_from_core() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();

        st.theme_entries = vec![ThemeEntry {
            id: "old".to_string(),
            display: "旧主题".to_string(),
            ..Default::default()
        }];
        st.theme_value_sig.set(vec!["old".to_string()]);
        st.theme_label_sig.set(vec!["旧主题".to_string()]);

        st.refresh_theme_names();

        // 值 = id，label = 显示名（不再拼版本，版本改走徽章）。
        assert_eq!(
            st.theme_value_sig.get(),
            vec!["default_light", "default_dark", "mono"]
        );
        assert_eq!(st.theme_label_sig.get(), vec!["清风·浅", "清风·深", "墨"]);
        // 条目携带版本与内置标记。
        assert_eq!(st.theme_entries[0].version, "3.2.1");
        assert!(st.theme_entries[0].builtin, "default_light 为内置");
        assert!(!st.theme_entries[2].builtin, "mono 为外部主题");
        // 富内容项：版本作徽章；仅外部主题（mono）带删除图标。
        let items = st.theme_items_sig.get();
        assert_eq!(
            items[0].badge.as_ref().map(|(t, _)| t.as_str()),
            Some("v3.2.1")
        );
        assert!(items[0].trailing_icon.is_none(), "内置主题无删除图标");
        assert!(items[2].trailing_icon.is_some(), "外部主题应有删除图标");

        // 点外部主题删除图标 → 填充删除确认对话框（待删 id + 正文）并弹出。
        (items[2].trailing_icon.as_ref().unwrap().1)();
        assert!(st.theme_delete_visible.get(), "点删除图标应弹出确认对话框");
        assert_eq!(st.theme_delete_id.get(), "mono");
        assert!(st.theme_delete_msg.get().contains("墨"));

        // 确认删除 → 调 theme.delete（mock ok）→ 关闭对话框 + 刷新列表 + 清空待删 id。
        st.confirm_delete_theme();
        assert!(!st.theme_delete_visible.get(), "删除后关闭对话框");
        assert!(st.theme_delete_id.get().is_empty(), "删除后清空待删 id");
        assert_eq!(
            st.theme_value_sig.get().len(),
            3,
            "删除后按 mock 重新拉取列表"
        );

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
            theme_entries: vec![],
            fonts: vec![],
            connected: false,
        };
        st.apply_loaded(data);
        assert!(st.conn_failed.get(), "非 mock 未连接应置 conn_failed");
    }

    /// 冷启动时机守护：命令行解析发生在方案列表就绪之前，深链只能先挂起，
    /// 由 `apply_loaded` 在 `set_schemas` **之后**消费。若哪天顺序被调换（或消费点
    /// 提前到 set_schemas 前），folded_domains 还是空的，`--schema` 会解析失败，
    /// 这条测试就会红。
    #[test]
    fn pending_dict_target_applied_after_schemas_arrive() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();

        // 冷启动：此刻方案列表为空，深链必须挂起而非当场失败
        st.route_dict_target(crate::cli::DictTarget {
            schema: "wb".into(),
            tab: Some(crate::pages::dict::spec::LogicalTab::Shadow),
            bad_tab: String::new(),
        });
        assert_eq!(st.dict_mgr.domain.get(), 0, "方案未就绪时不应先跳到某个域");

        let data = LoadedState {
            config_value: Value::Object(Default::default()),
            system_info: Value::Object(Default::default()),
            capabilities_value: Value::Object(Default::default()),
            schemas: vec![SchemaInfo {
                id: "wb".into(),
                name: "五笔86".into(),
                kind: "codetable".into(),
                scheme: String::new(),
                hidden: false,
            }],
            theme_entries: vec![],
            fonts: vec![],
            connected: true,
        };
        st.apply_loaded(data);

        assert_eq!(st.dict_mgr.current_schema_id().as_deref(), Some("wb"));
        assert_eq!(
            st.dict_mgr.current_category(),
            crate::pages::dict::spec::WordCategory::Shadow,
            "方案到位后应完成深链定位"
        );
        // 完全命中不该弹提示（提示只留给降级场景）
        assert!(st.feedback.get().is_none(), "命中时不应有多余 toast");
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
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
            theme_entries: vec![],
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
