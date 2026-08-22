//! 全局状态 — 配置加载、清单、字段注册表、保存。
//!
//! 状态绑定改用 wind-ui-rust 的 `Signal`（取代旧 `Rc<Cell>`）。清单引擎
//! [`crate::manifest::field`] 为每个 key 建一个 Signal 并把"写回器/取值器"
//! 登记进 [`FieldRegistry`]；[`AppState::save`] 跑所有写回器把 Signal 值写回
//! Config，再与 base 做 diff，增量 `config.setItems`。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use windui::core::EventCtx;
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
    /// **config.toml 之外**的落点，随主保存一并提交。见 [`SideCommitter`]。
    pub side: RefCell<Vec<SideCommitter>>,
}

impl FieldRegistry {
    /// 清空全部登记项，供 [`crate::shell::build`] 在重建设置树前调用。
    ///
    /// # 为什么必须清
    ///
    /// 本注册表里存的是**上一棵设置树**的 Signal 句柄与闭包。设置树重建过一次之后
    /// （主窗关掉再开），旧的那批就同时是错的和危险的：
    ///
    /// - **错**：`writers` 只追加不清理，保存时同一个键会被写两遍；`global_flags` 同理，
    ///   汇总出的 `keys.global_hotkeys` 会带上重复项。
    /// - **危险**：这些 Signal 是在 `Window::content` 闭包里创建的，归**那个窗口**的
    ///   `SignalScope` 所有。窗口一关就 `dispose()`，槽位还给 arena 等待复用；而句柄
    ///   还留在这里，指向的槽位随后会被下一批信号占用。读它拿到的是别人的值。
    ///
    /// 所以症状是「第一次重建好好的，第二次崩」——第一次时旧句柄还没来得及被复用。
    ///
    /// 这条前提以前由「主窗等于进程」免费保证着（`shell::build` 一辈子只跑一次），
    /// 主窗改成可重建之后就得显式维护了。
    pub fn reset(&self) {
        self.writers.borrow_mut().clear();
        self.getters.borrow_mut().clear();
        self.loaders.borrow_mut().clear();
        self.dialogs.borrow_mut().clear();
        self.global_flags.borrow_mut().clear();
        self.side.borrow_mut().clear();
    }
}

/// 非 `config.toml` 落点的提交器 —— `writers` 之于 config.toml，`SideCommitter` 之于其余。
///
/// 设置界面里不是所有数据都进 `config.toml`：方案 override（`schema_overrides/<id>.toml`）
/// 走 `schema.saveConfig`，词库开关走 `schema.setDictEnabled`，都不在 `config.setItems`
/// 的键空间里，主保存的键 diff 够不着。
///
/// 曾经的做法是让这类界面各自长一个「保存」按钮。那条路的终点是一堆各存各的按钮，
/// 用户无从知道哪个管哪些、点了哪个还有什么没存——所以改成在此登记，由「应用设置 /
/// 保存并退出」统一提交。**新增任何非 config 落点都该登记到这里，而不是再加按钮。**
pub struct SideCommitter {
    /// 是否有待提交改动。汇总进 [`AppState::is_dirty`]，让「有未保存改动」的判断
    /// 覆盖这类数据——否则用户改完方案配置，「恢复本页」不会提醒他即将丢弃什么。
    pub is_dirty: Box<dyn Fn() -> bool>,
    /// 提交。`Err` 使整次保存判失败，文案直接进 toast。
    pub commit: Box<dyn Fn() -> Result<(), String>>,
}

impl FieldRegistry {
    /// 是否有待提交的非 config 改动。
    pub fn side_dirty(&self) -> bool {
        self.side.borrow().iter().any(|s| (s.is_dirty)())
    }

    /// 依次提交所有非 config 落点，**任一失败即中止**并返回其错误。
    ///
    /// 中止而非continue：后续提交多半依赖同一批数据，硬着头皮往下写只会让失败面变大，
    /// 且 toast 只能显示一条错误——用户看到的会是最后一个错，而不是最先出问题的那个。
    ///
    /// 与 [`Self::side_dirty`] 一起抽成方法（而非内联在 `AppState::save`）是为了可测：
    /// `AppState` 的构造走 RPC 与文件系统，测试里造不出来。
    pub fn commit_side(&self) -> Result<(), String> {
        for s in self.side.borrow().iter() {
            (s.commit)()?;
        }
        Ok(())
    }
}

/// 底栏状态点表示的四种运行态。
///
/// 只驱动一个圆点 + 悬停提示，不再有常驻文字：早先底栏常驻「已连接 / 未连接到输入法
/// 服务」，多位用户把它读成"连的是云端服务器"——这里连的其实是本机的输入法进程
/// （命名管道 / Unix socket），说法本身就是误导。文案改到 tooltip 里、并避开"连接"一词。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnState {
    /// 后台加载尚未回来（启动或重连中）。
    Loading,
    /// 输入法服务在跑，配置可读可存。
    Ready,
    /// `WIND_RPC_MOCK=1` 的离线预览，数据是假的、改动存不下去。
    Mock,
    /// 正式模式下服务没起来（同时会弹阻断对话框）。
    Down,
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
    /// 底栏左下角的运行态（驱动状态圆点的颜色与悬停提示）。见 [`ConnState`]。
    pub conn: Signal<ConnState>,
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
    /// 待打开词库窗（`--page dict` 深链、协议路由）。
    ///
    /// 开窗**必须**有个 `EventCtx`，而且得是副作用被采纳的时机：`Tree::call_on_update`
    /// 只把 toast 捞出来、丢弃整个 `EventOutcome`，所以响应式哨兵里 `open_window` 永不
    /// 生效。命令行解析（冷启动）与 `single_instance` 回调（二次实例）都没有 ctx，故先
    /// 在此挂起，由带 ctx 的 channel 回调消费（见 `main` 的两个 channel）。
    pending_dict_window: Cell<bool>,
    /// 待重开主窗（主窗已关闭时二次实例送来 argv）。同 `pending_dict_window`：开窗要
    /// `EventCtx`，命令行/单实例回调都没有，故先挂起、由带 ctx 的通道回调消费。
    pending_main_window: Cell<bool>,
    /// UI 请求通道：让**没有 ctx 的回调**（`single_instance` 的 argv 处理）能把
    /// 「该开词库窗了」送到一个有 ctx 的时机。`main()` 在 App 构建期注入一次。
    pub ui_tx: RefCell<Option<Sender<crate::UiRequest>>>,
    /// 主窗是否已经关闭。**不影响词库窗**——它独立存活（见 [`crate::dict_window::open`]）。
    ///
    /// 用途只有一个：二次实例送来 argv 时，据此判断该「把已有主窗拉到前台」（windui 的
    /// 单实例监听自己会做）还是「重新开一个主窗」。没有它就分不清这两种情形，而两者
    /// 用户看到的差别是「设置窗跳出来」与「点了没反应」。
    ///
    /// 只在主窗真正关闭时置位，`on_close_request` 被拦下（有未保存改动）时不置——
    /// 那次关闭取消了，主窗还在。
    pub main_window_closed: Signal<bool>,
    /// 运行期换肤句柄。重建主窗时 `shell::build` 要用它，而 `App::theme_handle()` 只在
    /// 建窗前拿得到，故建好后存一份进来。
    pub theme_handle: RefCell<Option<ThemeHandle>>,
    /// 词库正作为 `App` 的那个窗口（`--page dict` 冷启动时如此）。
    ///
    /// 这种启动下**不建设置窗**：从托盘点「词库管理」却弹出两个窗口（设置窗 + 词库窗）
    /// 是没道理的，用户要的就只是词库。
    ///
    /// 之所以要记一笔，是因为 `App::run` 建的那个窗口拿不到 `Window::single` 的键——
    /// 单例登记表里没有它。不记的话，再来一次 `--page dict` 就会在它旁边**另开**一个
    /// 词库窗；而记了就能识别出"已经有一个了"，交给单实例监听把它激活到前台。
    dict_is_main: Cell<bool>,
    /// 待应用的词库页深链（`--page dict --schema .. --type ..`）。
    ///
    /// 冷启动解析命令行时方案列表尚未就绪（要等后台 RPC 回来才有），无从把 `--schema`
    /// 解析成数据域下标，故先挂在这里，由 [`Self::apply_loaded`] 在 `set_schemas` 之后消费。
    /// 顺序不能反：`set_schemas_no_reload` 会清空 `sub_tab_memory` 并收敛越界的 domain。
    pending_dict_target: Rc<RefCell<Option<crate::cli::DictTarget>>>,
    /// 备份还原状态（高级页卡片 + 还原确认对话框）。
    pub backup_mgr: crate::pages::backup::BackupState,
    /// 统一配置导入状态（高级页「导入与导出」卡片 + 导入配置对话框）。
    pub config_import: crate::pages::config_import::ConfigImportState,
    /// 桌面集成状态（高级页卡片 + 协议注册的两个确认对话框）。
    pub integration: crate::pages::integration::IntegrationState,
    /// 统计页状态（数据快照 + 清理确认对话框）。
    pub stats_mgr: crate::pages::stats::StatsState,
    /// 「重新加载」二次确认对话框可见标志。
    pub reload_confirm: Signal<bool>,
    /// 「恢复本页」（有未保存改动时）确认对话框可见标志。
    pub reset_confirm: Signal<bool>,
    /// 关窗前「有未保存的更改」确认对话框可见标志。
    pub exit_confirm: Signal<bool>,
    /// 启动时是否成功连接到输入法服务（core）。未连接则设置只读、保存会失败。
    pub connected: bool,
    /// [`Self::apply_loaded`] 是否已跑过一轮。区别于 `connected`：mock / 离线预览下
    /// 数据同样已就位但 `connected` 为 false，故不能拿后者判断"方案列表可用了没"。
    data_loaded: bool,
    /// 正式模式下连接失败标志：true 时显示阻断对话框；mock 模式始终 false。
    pub conn_failed: Signal<bool>,
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
    /// 最新一次检查请求的编号；`CheckDone` 带回的 `seq` 与它不等即为过期结果，丢弃。
    ///
    /// 检查请求会并存：网络慢时用户关掉对话框再点一次（关闭不取消已发出的请求），
    /// 启动时的静默检查也可能与手动检查撞上。先发的那次晚回来若照单全收，就会把
    /// 正在下载的界面打回「发现新版本」——用户再点一次下载即起第二个下载线程。
    /// 只在 UI 线程读写，故 `Cell` 足够。
    update_check_seq: std::cell::Cell<u64>,
    /// 是否已有下载在跑。防重入：`spawn_update_download` 每调用一次就起一个线程，
    /// 两个线程写同一个目标文件，进度还会互相打架。三种终态消息到达时复位。
    update_downloading: std::cell::Cell<bool>,
    /// 正在跑的那次检查是否为静默检查；`None` = 当前没有检查在跑。
    ///
    /// 只挡**同类型**的重复请求（见 [`Self::spawn_update_check`]）。清除条件是"结果的
    /// 请求号仍是最新的"——过期结果（用户中途又发起过检查）不能把新那次的标记抹掉。
    update_checking: std::cell::Cell<Option<bool>>,
}

/// 主题编辑器连接模式字符串 → 分段控件索引（0=off/1=once/2=auto；未知值回落 0）。
fn mode_str_to_idx(mode: &str) -> usize {
    match mode {
        "once" => 1,
        "auto" => 2,
        _ => 0,
    }
}

/// 登记**方案页专有**的写回器与 side 提交器。
///
/// # 必须可重入，且必须在每次 `FieldRegistry::reset()` 之后重新调用
///
/// 这批登记原先写在 `AppState::empty()` 里、一辈子只跑一次。而 `shell::build` 开头会
/// `fields.reset()` 清空整个注册表（主窗可关掉再开，见 `FieldRegistry::reset`）——
/// `empty()` 早于 `shell::build`，于是设置树一建好，这批登记就全没了：
///
/// - `keys.key_actions` writer 没了 ⇒ 方案热键 / 引导键的改动**永远进不了 diff**，
///   保存时提示「没有变更」；
/// - `schema.available` / `schema.active` writer 没了 ⇒ 方案启用状态、「设为当前」同样存不下；
/// - `SideCommitter` 没了 ⇒ 方案 override、词库开关也存不下。
///
/// 而 manifest 那批 writer 是在 `shell::build` 里注册的，reset 后照常重建——**所以症状
/// 是「别的设置能存、只有方案页存不了」**，很容易误判成方案页自己的问题。
///
/// 守卫见 `builtin_registrations_survive_registry_reset`。
pub(crate) fn register_schema_registrations(
    fields: &Rc<FieldRegistry>,
    schema_mgr: &SchemaManagerState,
) {
    // 方案 override / 词库开关不进 config.toml，主保存的键 diff 够不着 —— 在此登记，
    // 随「应用设置 / 保存并退出」一并提交。见 SideCommitter 的文档。
    {
        let dirty_mgr = schema_mgr.clone();
        let commit_mgr = schema_mgr.clone();
        fields.side.borrow_mut().push(SideCommitter {
            is_dirty: Box::new(move || dirty_mgr.has_pending_edits()),
            commit: Box::new(move || commit_mgr.commit_pending_edits()),
        });
    }
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

    // `keys.schema_hotkeys` **不再有写回器**：该键已在 core 废弃并移出登记表
    // （`config.setItems` 会把它当未登记键跳过），方案直达热键统一落进下面的
    // key_actions（动词 `switch_schema:<方案id>`）。留着这个 writer 的话，每次保存
    // 都会往 diff 里塞一个必被服务端 skip 的项——用户看到「部分设置未保存」的提示，
    // 而那一项其实压根不该存在。
    //
    // 注册 keys.key_actions 写回器：全局按键功能表（五c 收编后所有引导键的落点，
    // 以及合并进来的方案直达热键）。
    //
    // 与 schema_hotkeys 同一条路、同一个理由：Map 型键在 diff 里是原子叶子（整对象
    // 产出），在对话框里直接 config.setItems 的话，主页面持有的仍是打开时的旧值，
    // 用户之后按一次保存就会把它整体覆盖回去。
    {
        let ka = schema_mgr.key_actions.clone();
        fields.writers.borrow_mut().push(Box::new(move |cfg| {
            let map: serde_json::Map<String, Value> = ka
                .borrow()
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect();
            cfg.set_path("keys.key_actions", Value::Object(map));
        }));
    }

    // 原先此处还有一个 `schema.special_modes` 写回器（特殊方案的引导键 + 条目维护）。
    // 该键已从 core 删除：实例集合改由「带 [overlay] 段的已安装方案」定义，引导键
    // 落上面的 keys.key_actions，overlay 呈现配置落方案 override（走 SideCommitter）。
    // 于是这里那份「必须整表写回、不能用结构体重建」的易碎逻辑整个消失。

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
}

impl AppState {
    /// 快速初始化：仅读系统主题，其余字段置空。
    /// UI 可立即显示；真实数据由后台线程通过 [`AppState::apply_loaded`] 写入。
    pub fn empty() -> Self {
        let schema_mgr = SchemaManagerState::new();
        let fields: Rc<FieldRegistry> = Rc::new(FieldRegistry::default());
        register_schema_registrations(&fields, &schema_mgr);
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
            conn: signal(ConnState::Loading),
            feedback: signal(None),
            conflict: signal(String::new()),
            schema_mgr,
            dict_mgr: DictManagerState::new(),
            pending_dict_window: Cell::new(false),
            pending_main_window: Cell::new(false),
            ui_tx: RefCell::new(None),
            main_window_closed: signal(false),
            theme_handle: RefCell::new(None),
            dict_is_main: Cell::new(false),
            pending_dict_target: Rc::new(RefCell::new(None)),
            backup_mgr: crate::pages::backup::BackupState::new(),
            config_import: crate::pages::config_import::ConfigImportState::new(),
            integration: crate::pages::integration::IntegrationState::new(),
            stats_mgr: crate::pages::stats::StatsState::new(),
            reload_confirm: signal(false),
            exit_confirm: signal(false),
            reset_confirm: signal(false),
            connected: false,
            data_loaded: false,
            conn_failed: signal(false),
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
            update_check_seq: std::cell::Cell::new(0),
            update_downloading: std::cell::Cell::new(false),
            update_checking: std::cell::Cell::new(None),
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

    // ── windinput:// 协议关联 ─────────────────────────────────────────────

    /// 注册（或修复）协议关联，指向本程序。
    ///
    /// 同时把 `protocol.self_heal` 置回 `true`：用户既然主动注册，就是要这个关联长期有效，
    /// 之后启动自愈该继续兜底（便携版另有一道门，恒不自愈）。
    pub fn register_protocol(&mut self) {
        match crate::protocol::register() {
            Ok(()) => {
                self.set_protocol_self_heal(true);
                self.integration.refresh();
                self.notify_ok("windinput:// 协议已注册");
            }
            Err(e) => self.notify_err(format!("协议注册失败：{e}")),
        }
    }

    /// 解除协议关联。
    ///
    /// **必须**同时把 `protocol.self_heal` 置 `false` —— 否则下次启动自愈立刻把关联
    /// 写回去，用户看到的是"取消了个寂寞"。这也是这个配置项存在的唯一理由。
    pub fn unregister_protocol(&mut self) {
        match crate::protocol::unregister() {
            Ok(()) => {
                self.set_protocol_self_heal(false);
                self.integration.refresh();
                self.notify_ok("windinput:// 协议注册已解除，启动时不再自动注册");
            }
            Err(e) => self.notify_err(format!("解除协议注册失败：{e}")),
        }
    }

    fn set_protocol_self_heal(&mut self, allow: bool) {
        self.setting_config.protocol.self_heal = allow;
        if let Err(e) = crate::setting_config::save(&self.setting_config) {
            self.notify_err(e);
        }
    }

    // ── .wpkg 文件关联 ────────────────────────────────────────────────────

    /// 注册（或修复）`.wpkg` 关联。自愈意愿同步置回 `true`，规则同协议注册。
    pub fn register_file_assoc(&mut self) {
        match crate::file_assoc::register() {
            Ok(()) => {
                self.set_file_assoc_self_heal(true);
                self.integration.refresh();
                self.notify_ok(".wpkg 文件关联已注册");
            }
            Err(e) => self.notify_err(format!("文件关联注册失败：{e}")),
        }
    }

    /// 解除 `.wpkg` 关联。**必须**同时把 `file_assoc.self_heal` 置 `false` ——
    /// 否则下次启动自愈立刻把关联写回去（与协议取消注册同一条教训）。
    pub fn unregister_file_assoc(&mut self) {
        match crate::file_assoc::unregister() {
            Ok(()) => {
                self.set_file_assoc_self_heal(false);
                self.integration.refresh();
                self.notify_ok(".wpkg 文件关联已解除，启动时不再自动注册");
            }
            Err(e) => self.notify_err(format!("解除文件关联失败：{e}")),
        }
    }

    fn set_file_assoc_self_heal(&mut self, allow: bool) {
        self.setting_config.file_assoc.self_heal = allow;
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
    ///
    /// **正在下载 / 已下好待安装时只把对话框显示出来**，不重置任何状态、也不再发起检查：
    /// 此时用户是回来看进度的。原先无条件重置会把后台任务与界面割裂——下载仍在跑，界面
    /// 却回到「检查中」，检查结果一到又变成「发现新版本」，再点一次下载就起了第二个线程。
    pub fn open_update_dialog(&self) {
        use crate::update::UpdatePhase;
        if matches!(
            self.update_phase.get(),
            UpdatePhase::Downloading | UpdatePhase::Ready
        ) {
            self.update_dialog_visible.set(true);
            return;
        }
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

    /// 起一次检查。`silent` 见 [`UpdateMsg::CheckDone`](crate::update::UpdateMsg)。
    ///
    /// **同类型的检查已在跑时直接复用**，不起第二个线程、也**不推进请求号**——推进了的话，
    /// 正在跑的那次回来就成了"过期"被丢弃，而新线程没起，界面会永远停在「检查中」。
    /// 防的是"网络慢 → 用户反复点检查更新"：每点一次都起一个最长 10s 的 HTTP 请求，
    /// 纯属浪费。界面侧的反馈（切到 Checking）由调用方照常做，用户看得到响应。
    ///
    /// 静默与手动**不互相复用**：两者的结果处理完全不同（静默无更新时不打扰、不弹对话框），
    /// 拿静默的结果去满足用户手动点的那一次，界面就会停在检查中不动。此时改为起新线程，
    /// 先前那次由请求号自然作废。
    fn spawn_update_check(&self, silent: bool) {
        let Some(tx) = self.update_tx.borrow().clone() else {
            if !silent {
                self.fail_update("升级通道未初始化");
            }
            return;
        };
        if !self.try_begin_check(silent) {
            tracing::debug!("同类型的检查已在进行中（silent={silent}），复用当前请求");
            return;
        }
        // 请求号自增：这一刻起，之前发出的检查结果一律作废（见 `update_check_seq`）。
        let seq = self.update_check_seq.get().wrapping_add(1);
        self.update_check_seq.set(seq);
        // 网络请求绝不能在 UI 线程做；结果经 channel 回到 UI 线程写 Signal。
        std::thread::spawn(move || {
            // 有更新时必拉说明；「已是最新」时只有手动检查才拉（静默那条不弹界面）。
            let result = crate::update::check_update(!silent).map_err(|e| e.user_message());
            let _ = tx.send(crate::update::UpdateMsg::CheckDone {
                result,
                silent,
                seq,
            });
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
        if !self.try_begin_download() {
            tracing::warn!("已有下载在进行中，忽略重复的下载请求");
            return;
        }

        self.update_cancel.store(false, Ordering::SeqCst);
        let cancel = self.update_cancel.clone();
        self.update_progress.set(0.0);
        self.update_progress_text.set(String::new());
        self.update_message.set("正在下载更新包…".to_string());
        self.update_phase
            .set(crate::update::UpdatePhase::Downloading);

        std::thread::spawn(move || {
            // 文件名正常从 URL 末段取；这个兜底只在 URL 拼不出合法文件名时才用上，
            // 但扩展名必须对 —— macOS 上 `open` 是按扩展名决定交给谁打开的，
            // 一个 .exe 后缀的 pkg 会被 Finder 当成不认识的文件而不是安装包。
            let fallback = if cfg!(windows) {
                "wind-update.exe"
            } else {
                "wind-update.pkg"
            };
            let dest = crate::update::download::cache_path(&check.download_url, fallback);
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

    /// 能否发起一次 `silent` 类型的检查；能则就地占位（防重入）。结果到达时由
    /// [`Self::on_update_msg`] 复位（且只有**最新那次**的结果才复位）。
    ///
    /// 只挡同类型：静默与手动的结果处理完全不同（静默无更新时不打扰、不弹对话框），
    /// 让手动那次复用正在跑的静默检查，界面就会停在「检查中」等一个永远不会切阶段的结果。
    fn try_begin_check(&self, silent: bool) -> bool {
        if self.update_checking.get() == Some(silent) {
            return false;
        }
        self.update_checking.set(Some(silent));
        true
    }

    /// 能否开始一次新下载；能则就地占位（防重入）。终态消息到达时由
    /// [`Self::on_update_msg`] 复位。
    ///
    /// 两个下载线程会写同一个目标文件、进度还互相打架。正常路径下「下载更新」按钮在
    /// Downloading 阶段是隐藏的，但阶段本身可能被别处改回去（过期的检查结果就干过这事），
    /// 所以守卫放在真正起线程的地方，而不是依赖按钮的可见性。
    fn try_begin_download(&self) -> bool {
        !self.update_downloading.replace(true)
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
    pub fn on_core_event(&mut self, ctx: &mut EventCtx, ev: &crate::rpc::CoreEvent) {
        match ev.event.as_str() {
            "schema.activeChanged" => {
                if let Some(id) = ev.data.get("id").and_then(|v| v.as_str()) {
                    tracing::debug!("core 活跃方案变更 → {}", id);
                    self.schema_mgr.set_active_from_core(id);
                }
                // 刻意不提示：切方案多半是用户自己按的快捷键或点的托盘，他知道刚发生了
                // 什么；而这是高频操作，逐次弹提示只会让人学会忽略 toast。
            }
            // core 侧配置落盘。**本端自己保存也会收到这条**（core 在 setItems 后广播同名
            // 事件，且带 key），故不能一见到就当外部改动。
            "config.changed" => {
                let (Some(key), Some(value)) = (
                    ev.data.get("key").and_then(|v| v.as_str()),
                    ev.data.get("value"),
                ) else {
                    return;
                };
                // 与本端当前值比对**必须在回填之前**做：值相同即本端刚写下去的回声，
                // 界面不会有任何跳变，提示纯属噪音（用户在对话框里改一项、点保存，
                // 转头就被告知"已在别处更改"）。
                let differs = self.current_config.get_path(key) != Some(value);
                let hit = self.apply_external_config_change(key, value.clone());
                if differs {
                    self.notify_external_change(ctx, key, hit);
                }
            }
            other => tracing::debug!("忽略 core 事件: {}", other),
        }
    }

    /// 外部改动落到界面上时给一条轻提示。
    ///
    /// `apply_external_config_change` 有个写在注释里的取舍：用户此刻正在设置页编辑同一项
    /// 的话，回填会**覆盖掉他的未保存改动**——当时判断"同时在菜单和设置页改同一项"极少
    /// 发生，不值得为它维护逐项脏状态。取舍本身仍然成立，但"静默"这一半是可以改掉的：
    /// 屏幕上有个开关自己动了却没有任何解释，才是真正让人困惑的部分。
    ///
    /// 两个条件缺一不可，少任一个都会变成噪音：
    /// - **值确实变了**（调用方在回填前比对）——本端自己保存后 core 也会广播 `config.changed`，
    ///   loader 把同一个值再灌一次，控件纹丝不动。只看"有没有控件被回填"会把这条回声
    ///   报成外部改动，用户在对话框里改一项、点保存，转头就被告知"已在别处更改"。
    /// - **界面上确实有这一项**（`hit > 0`）——未在清单登记的 key（拖动落位写的
    ///   `ui.status.custom_x` 之类）本就不在设置页露出，为它弹提示同样是打扰。
    ///
    /// 这条提示过去写不出来：它的位置在 `App::channel` 的回调里，而那个回调直到 windui
    /// 0.12 才拿到 `EventCtx`，toast 是宿主浮层、没有信号接口可绕。
    fn notify_external_change(&self, ctx: &mut EventCtx, key: &str, hit: usize) {
        if hit == 0 {
            return;
        }
        let label = self
            .manifest
            .get(key)
            .map(|it| it.label.clone())
            .unwrap_or_else(|| key.to_string());
        ctx.toast(format!("「{label}」已在别处更改，页面已同步"));
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
    /// 说得通的语义；加了反而要维护逐项脏状态。覆盖不再是**静默**的了——回填命中控件时
    /// 由 [`Self::notify_external_change`] 弹一条提示。
    ///
    /// 返回回填命中的控件数：0 表示这个 key 在设置页上没有露出，界面并未跳变。
    pub fn apply_external_config_change(&mut self, key: &str, value: Value) -> usize {
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
        hit
    }

    /// 后台消息 → Signal。**在 UI 线程回调中执行**（`App::channel` 保证）。
    pub fn on_update_msg(&self, msg: crate::update::UpdateMsg) {
        use crate::update::{UpdateMsg, UpdatePhase, format_size};

        // 过期的检查结果一律丢弃：请求号不是最新的那次，说明用户之后又发起过检查
        //（关掉对话框再点一次、或手动检查撞上启动时的静默检查）。放行的话，先发后到的
        // 那次会把当前状态覆盖掉——正在下载的界面被打回「发现新版本」，再点一次下载
        // 就起了第二个下载线程。
        if let UpdateMsg::CheckDone { seq, silent, .. } = &msg {
            if *seq != self.update_check_seq.get() {
                tracing::debug!(
                    "丢弃过期的检查结果：seq={} 当前={} silent={}",
                    seq,
                    self.update_check_seq.get(),
                    silent
                );
                // 刻意**不**清 `update_checking`：那个标记属于最新那次请求，
                // 被一个已经作废的结果抹掉，会让真正在跑的那次失去防重入保护。
                return;
            }
            // 最新那次有结果了，检查结束，放行后续请求。
            self.update_checking.set(None);
        }
        // 下载的三种终态：复位防重入标志，之后才允许再次下载。
        if matches!(
            msg,
            UpdateMsg::DownloadDone(_)
                | UpdateMsg::DownloadFailed(_)
                | UpdateMsg::DownloadCancelled
        ) {
            self.update_downloading.set(false);
        }

        match msg {
            UpdateMsg::CheckDone {
                result: Ok(r),
                silent: true,
                ..
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
                ..
            } => {
                tracing::info!("静默检查更新失败（已忽略）：{e}");
            }
            UpdateMsg::CheckDone {
                result: Ok(result),
                silent: false,
                ..
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
                ..
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

    /// 注入 UI 请求通道的发送端。`main()` 在 App 构建期调用一次。
    pub fn set_ui_tx(&self, tx: Sender<crate::UiRequest>) {
        *self.ui_tx.borrow_mut() = Some(tx);
    }

    /// 请求打开词库窗。只置标志，真正开窗由带 `EventCtx` 的 channel 回调完成。
    ///
    /// `notify` 决定要不要顺带唤醒 UI 通道：
    /// - 冷启动时传 `false` —— 那一刻方案列表还没到（后台 RPC 在飞），开出来的窗左栏是空的，
    ///   `--schema wb` 也无从解析成域下标。标志由 `apply_loaded` 之后的回调消费，时机天然对。
    /// - 二次实例 / 协议路由传 `true` —— 数据早已就绪，需要立刻推一帧把窗开出来。
    pub fn request_dict_window(&self, notify: bool) {
        // 词库已经是 `App` 的那个窗口时不再开第二个。单实例监听会把它激活到前台，
        // 而深链参数（`--schema` / `--type`）照常经 `route_dict_target` 写进去生效。
        if self.dict_is_main.get() {
            return;
        }
        self.pending_dict_window.set(true);
        if notify && let Some(tx) = self.ui_tx.borrow().clone() {
            // 发送失败 = 窗口已关闭，此时开窗本来也没有意义。
            let _ = tx.send(crate::UiRequest::OpenDictWindow);
        }
    }

    /// 词库是否正作为 `App` 的主窗口。见 [`Self::dict_is_main`] 字段说明。
    pub fn dict_is_main(&self) -> bool {
        self.dict_is_main.get()
    }

    /// 标记词库为 `App` 的主窗口（`--page dict` 冷启动）；它关闭时须复位。
    pub fn set_dict_is_main(&self, on: bool) {
        self.dict_is_main.set(on);
    }

    /// 取走「该开词库窗了」这个请求（取一次即清）。
    pub fn take_dict_window_request(&self) -> bool {
        self.pending_dict_window.replace(false)
    }

    /// 请求重新开出主窗（主窗已关、二次实例又送来 argv 时）。
    ///
    /// 与 [`Self::request_dict_window`] 一样只置标志 + 推一帧：开窗要 `EventCtx`，而
    /// `single_instance` 的回调没有。这条路在主窗已关的情况下仍然通——通道挂在 App 级，
    /// 由还活着的词库窗接手排空。
    pub fn request_main_window(&self) {
        self.pending_main_window.set(true);
        if let Some(tx) = self.ui_tx.borrow().clone() {
            let _ = tx.send(crate::UiRequest::OpenMainWindow);
        }
    }

    /// 取走「该开主窗了」这个请求（取一次即清）。
    pub fn take_main_window_request(&self) -> bool {
        self.pending_main_window.replace(false)
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
        let mut cfg = Config::from_value(data.config_value);
        // diff 基线必须与 writer 的输出同构：punct writer 有「全等内置默认即不落盘」的
        // 裁剪语义，而 core 会原样保留 config.toml 里显式写着默认值的映射行（Map 内部
        // 条目不在注册表，core 的剪枝够不着）。不归一则「什么都没改」也恒判脏，关窗
        // 每次都拦。详见 `manifest::punct_canonical_map`。
        crate::manifest::canonicalize_punct_baseline(&mut cfg);
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
        // 响应式：构建期数据还没回来（总为 Loading），圆点必须靠 Signal 才能在加载后改色。
        if data.connected {
            self.conn.set(ConnState::Ready);
            self.conn_failed.set(false);
        } else if rpc::is_mock() {
            // 兜底分支：mock 下 `LoadedState::fetch` 总能给出配置（connected=true，
            // 走上面的 Ready），只有 mock 数据自身取不到时才落到这里。
            self.conn.set(ConnState::Mock);
            self.conn_failed.set(false);
        } else {
            self.conn.set(ConnState::Down);
            self.conn_failed.set(true);
        }
        tracing::info!("apply_loaded 完成, connected={}", data.connected);
    }

    /// 是否存在未保存改动：跑写回器到 current 的副本上，与 base 比对有无差异。
    pub fn is_dirty(&self) -> bool {
        // config.toml 之外的落点也算——否则改完方案配置点「恢复本页」，
        // 确认框不会提醒用户即将丢弃它们。
        if self.fields.side_dirty() {
            return true;
        }
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
        // ⛔ 曾经是 `Config::from_value({})`（纯 L1、未折算），两处都错：顶码上屏这类
        // 出厂经 L2 置开的项会被误关，而引导键类字段读的 `keys.key_actions` 恒为空
        // ⇒ 快捷输入的 `;` 一恢复就没了（清空即禁用）。见 [`core_defaults_config`]。
        let seed = crate::manifest::key_action_default_bindings(self);
        let def = crate::manifest::core_defaults_config(&seed);
        let loaders = self.fields.loaders.borrow();
        let mut n = 0usize;
        for g in self.current_tab_groups() {
            for (_sec, keys) in self.manifest.sections_of(g) {
                for k in keys {
                    if let Some(l) = loaders.get(&k) {
                        l(&def);
                        n += 1;
                    }
                }
            }
        }
        // keys.global_hotkeys 非清单项（由功能键「全局」勾选经收集器登记），手动纳入「按键」页恢复。
        if self.current_tab_groups().contains(&"keys")
            && let Some(l) = loaders.get("keys.global_hotkeys")
        {
            l(&def);
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
        let mut cfg = Config::from_value(fresh);
        // 与 `apply_loaded` 同一道归一：重新加载后基线同样要与 writer 同构，
        // 否则「重新加载」反而把设置页留在永久判脏的状态。
        crate::manifest::canonicalize_punct_baseline(&mut cfg);
        {
            let loaders = self.fields.loaders.borrow();
            for l in loaders.values() {
                l(&cfg);
            }
        }
        // 方案启用排序也需要从配置恢复，否则未保存的列表变更不会撤销。
        self.schema_mgr.apply_config(&cfg);
        // 待提交的方案 override 同理——「重新加载」是明确的「丢弃未保存改动」，
        // 留着它们的话，下次点保存会把用户以为已丢弃的改动写下去。
        self.schema_mgr.clear_pending_edits();
        self.base_config = cfg.clone();
        self.current_config = cfg;
        self.feedback
            .set(Some((ToastKind::Info, "已从服务端重新加载".to_string())));
    }

    /// 跑所有写回器把 Signal 值写回 current_config，再 diff 保存。
    /// 返回是否「可视为成功」（无错误）：无变更与保存成功均为 true，仅 RPC 报错为 false。
    /// 供「保存&退出」据此决定是否真的退出。
    /// 应用内主动关窗：**绕过**未保存守卫直接关。
    ///
    /// 守卫（`on_close_request`）要拦的是"点 ✕ / Alt+F4"这类可能出于无意的关闭；
    /// 应用内按钮（保存并退出、立即安装、连接失败后退出、确认框里的两个出口）都是明确
    /// 的意思表示，再问一遍只会挡路。
    ///
    /// 「立即安装」那处更是**不能**拦：安装器要等本进程退出才能替换文件，而守卫此时弹一个
    /// 确认框，就成了"安装器等窗口关、窗口等用户回答"——用户看到的是装到一半卡住。
    ///
    /// windui 0.12 的 `force_close` 正是为此而设，故本仓不再自备"已确认"标志位。
    ///
    /// 顺带记下「主窗已关闭」（见 [`Self::mark_main_window_closed`]）：本方法是应用内
    /// 所有主动关窗的统一入口，置位收在这里就不必让每个调用点各记一次。
    pub fn close_window(&self, ctx: &mut EventCtx) {
        self.mark_main_window_closed();
        ctx.force_close();
    }

    /// 记下主窗已关闭。词库窗**不受影响**，照常存活。
    ///
    /// ⚠️ 主窗关闭**不止一条路**，漏掉哪条，那条路之后二次实例就会误判成「主窗还在」，
    /// 于是只去 activate 一个已经不存在的窗口 —— 用户看到的是点了设置没反应。
    /// 已知两类，各自的置位点：
    /// 1. 应用内的主动关窗（底栏与确认框的「保存并退出」、「直接退出」、连接失败后的
    ///    「退出」、升级的「立即安装」）—— 全部经 [`Self::close_window`]，置位收在那里；
    /// 2. 系统 ✕ / Alt+F4 —— 走 `App::on_close_request`，在**放行的那一刻**由 `main` 调
    ///    本方法。不能在拦下时就置位：那次关闭被取消了，主窗还在。
    ///
    /// 第 1 类刻意绕过 `on_close_request`（见该处说明），所以把置位挂在守卫里覆盖不到它们。
    pub fn mark_main_window_closed(&self) {
        self.main_window_closed.set(true);
    }

    /// 是否有未保存的改动（关窗确认用）。
    ///
    /// 两个来源与 [`Self::save`] 一一对应：config.toml 之外的落点（方案 override、词库
    /// 开关），以及 config.toml 的增量 diff。
    ///
    /// diff 跑在**副本**上：`save` 是把 writers 直接写进 `self.current_config` 的——那是
    /// 提交路径的一部分。只为问一句"要不要保存"就改掉当前配置，会让紧接着的"直接退出"
    /// 变成已经改了一半；`live_conflicts` 出于同样的理由也用副本。
    pub fn has_unsaved_changes(&self) -> bool {
        // 数据没到位时一律视为无改动：此刻 `base_config` 还是空的，而 `AppState::empty`
        // 就带着四个内建 writer（方案 available、全局热键收集等），它们照样会写出
        // `keys` / `schema` 两个键——拿这个 diff 去拦关窗纯属误报，用户根本还没机会改
        // 任何东西。core 未响应时开了设置页又想关掉，正是最不该被拦的时候。
        if !self.data_loaded {
            return false;
        }
        if self.fields.side_dirty() {
            return true;
        }
        let mut probe = self.current_config.clone();
        for w in self.fields.writers.borrow().iter() {
            w(&mut probe);
        }
        !diff_config(&self.base_config, &probe, &self.capabilities.map_keys()).is_empty()
    }

    /// 跑一遍 writers 再与 base 做 diff，返回本次保存**会提交的项**。仅供测试。
    ///
    /// 与 [`Self::save`] 共用 `diff_config` 这一步，但**在副本上**跑 writers：save 是把
    /// writers 直接写进 `self.current_config` 的（那是提交路径的一部分），测试若照做，
    /// 一次调用就把状态改成"已保存"的样子，第二次断言便不再成立。同 `has_unsaved_changes`。
    #[cfg(test)]
    pub fn debug_diff_after_writers(&self) -> Vec<crate::config::ConfigItem> {
        let mut probe = self.current_config.clone();
        for w in self.fields.writers.borrow().iter() {
            w(&mut probe);
        }
        diff_config(&self.base_config, &probe, &self.capabilities.map_keys())
    }

    pub fn save(&mut self) -> bool {
        let reg = self.fields.clone();
        // ── 先提交 config.toml 之外的落点（方案 override、词库开关）──
        // 放在最前：它失败时 config.toml 还没动，用户重试不会重复写一遍配置。
        // 反过来先写 config 再失败，两个存储就分叉了。
        let side_changed = reg.side_dirty();
        if let Err(e) = reg.commit_side() {
            self.feedback.set(Some((ToastKind::Error, e)));
            return false;
        }

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
            // side 有改动时不能说「没有变更」——方案配置刚刚才落盘。
            self.feedback.set(Some(if side_changed {
                (ToastKind::Success, "已保存方案配置".to_string())
            } else {
                (ToastKind::Info, "没有变更".to_string())
            }));
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

    // `keys.key_actions` 里的**单键**条目（引导键通路）。
    //
    // ★ 五c 把四处 `trigger_keys` 收编进了这张表，但本检测一直没跟着换源——上面那几组
    // 读的字段在 core 侧已被 `normalize` 折算清空，于是「引导键撞车」实际上很久没被检出来
    // 过了。特殊模式那一路更是随 `schema.special_modes` 删除而彻底断供（原先读
    // `special_modes[].trigger_keys`）。这里把新表补进来。
    //
    // 只收单键：组合键归 `collect_hotkey_groups`（那是另一张 key_down 表），纯修饰键走
    // keyup 轻敲、与引导键不争用。判据用「键串含 `+`」近似 core 的 `route_of_key_action`
    // ——对 `ctrl+shift+u` / `backslash` 这类实际取值足够，且不必在本仓再写一份键名解析器。
    for (key, verb) in key_action_entries(cfg) {
        if key.contains('+') || is_modifier_key_name(&key) {
            continue;
        }
        groups.push(KeyGroup::new(
            &format!("key_action_{key}"),
            key_action_label(&verb),
            vec![key],
        ));
    }

    // 去重每组触发键：mix_modes 的 trigger_keys 直取自 core config 数组，
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

    // 方案直达热键**不再单独收集**：`keys.schema_hotkeys` 已并入 key_actions
    // （动词 `switch_schema:<方案id>`），由下面那段统一纳入检测。core 折算后该表恒为空，
    // 留着这段只会是永不执行的死代码。
    groups.push(KeyGroup::new(
        "temp_pinyin_hotkey",
        "临时拼音直达热键",
        expand_hotkey(cfg.get_str("input.temp_pinyin.hotkey").unwrap_or("")),
    ));

    // `keys.key_actions` 里的**组合键**条目：与上面 11 项、schema_hotkeys 在内核是同一张
    // key_down 表（见本函数文档的判据），必须同域比较。
    //
    // 特殊模式的直达热键此前挂在 `schema.special_modes[].hotkey`，随该键删除后改写在这张
    // 表里（`"ctrl+shift+u" = "special:<方案id>"`）；`toggle_schema:` 也一直在这里却从未
    // 参与过检测。一并补上。
    for (key, verb) in key_action_entries(cfg) {
        if !key.contains('+') {
            continue;
        }
        groups.push(KeyGroup::new(
            &format!("key_action_hotkey_{key}"),
            format!("{}（直达热键）", key_action_label(&verb)),
            expand_hotkey(&key),
        ));
    }

    groups
}

/// 读 `keys.key_actions`（键名 → 动词），按键名排序保证报告顺序可复现。
fn key_action_entries(cfg: &Config) -> Vec<(String, String)> {
    let Some(map) = cfg.get_object("keys.key_actions") else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = map
        .iter()
        .filter_map(|(k, v)| {
            let verb = v.as_str()?.trim();
            // `none` 是「本方案禁用该键」的哨兵，不产生任何绑定，自然不参与争用。
            (!k.is_empty() && !verb.is_empty() && verb != "none")
                .then(|| (k.clone(), verb.to_string()))
        })
        .collect();
    out.sort();
    out
}

/// 纯修饰键名（走 keyup 轻敲通路，与 keydown 的引导键/热键都不争用）。
fn is_modifier_key_name(key: &str) -> bool {
    matches!(key, "lshift" | "rshift" | "lctrl" | "rctrl")
}

/// 动词 → 冲突报告里的人话标签。未知动词原样回显，便于发现拼写错误。
fn key_action_label(verb: &str) -> String {
    if let Some(id) = verb.strip_prefix("special:") {
        return format!("特殊模式「{id}」");
    }
    if let Some(id) = verb.strip_prefix("mix:") {
        return format!("快捷输入「{id}」");
    }
    if let Some(id) = verb.strip_prefix("toggle_schema:") {
        return format!("切换到方案「{id}」（可返回）");
    }
    if let Some(id) = verb.strip_prefix("switch_schema:") {
        return format!("切换到方案「{id}」");
    }
    match verb {
        "temp_pinyin" => "临时拼音".to_string(),
        "temp_english" => "临时英文".to_string(),
        other => other.to_string(),
    }
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
                it = it.trailing_icon("✕", move |_ctx| {
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

    /// `keys.key_actions` 里的**单键**绑定参与触发键冲突检测。
    ///
    /// ★ 五c 把四处 `trigger_keys` 收编进了这张表，但本检测一直没换源——那几年里
    /// 「引导键撞车」实际上检不出来了。特殊模式那一路更随 `schema.special_modes`
    /// 删除而彻底断供。这条钉住新数据源。
    #[test]
    fn test_key_action_single_keys_join_trigger_domain() {
        let cfg = Config::from_value(json!({
            "input": {
                "temp_pinyin": { "trigger_keys": ["backslash"] },
                "temp_english": { "trigger_keys": [] }
            },
            "schema": { "mix_modes": [] },
            "keys": { "key_actions": { "backslash": "special:kf" } }
        }));
        let msg = check_trigger_key_conflicts(&cfg).expect("同一个键配给两处应检出冲突");
        assert!(msg.contains("特殊模式「kf」"), "实际: {msg}");
        assert!(msg.contains("临时拼音"), "实际: {msg}");
    }

    /// 组合键与纯修饰键**不进**触发键域：前者归 key_down 热键表（另一个域），
    /// 后者走 keyup 轻敲，与 keydown 的引导键不争用。
    #[test]
    fn test_key_action_combos_and_modifiers_stay_out_of_trigger_domain() {
        let cfg = Config::from_value(json!({
            "input": { "temp_pinyin": { "trigger_keys": [] } },
            "schema": { "mix_modes": [] },
            "keys": { "key_actions": {
                "ctrl+shift+u": "special:kf",
                "rshift": "toggle_schema:english"
            }}
        }));
        assert!(check_trigger_key_conflicts(&cfg).is_none());
    }

    /// `none` 是「禁用该键」的哨兵，不产生绑定，自然不参与争用。
    #[test]
    fn test_key_action_none_sentinel_does_not_conflict() {
        let cfg = Config::from_value(json!({
            "input": { "temp_pinyin": { "trigger_keys": ["grave"] } },
            "schema": { "mix_modes": [] },
            "keys": { "key_actions": { "grave": "none" } }
        }));
        assert!(check_trigger_key_conflicts(&cfg).is_none());
    }

    #[test]
    fn test_live_trigger_conflict_reflects_current_config() {
        // 本模块唯一碰 `AppState` 的用例：`live_conflicts` 会跑字段写回器，未开 mock 时
        // 会去够真实 core，检出配置里根本没有的冲突。此前它靠**别的测试**先 `set_var`
        // 才偶然变绿——谁先跑到决定成败，`cargo test` 随机红。按仓库惯例自带 mock + 锁。
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
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
                "key_actions": { "ctrl+shift+e": "switch_schema:wubi86" }
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

        // 特殊模式的直达热键现在写在 keys.key_actions 里（原 special_modes[].hotkey）。
        let cfg2 = Config::from_value(json!({
            "keys": {
                "open_settings": "ctrl+shift+p",
                "key_actions": { "ctrl+shift+p": "special:fast_symbols" }
            }
        }));
        let msg2 = check_hotkey_conflicts(&cfg2).expect("特殊模式直达热键应参与检测");
        assert!(msg2.contains("特殊模式「fast_symbols」"), "实际: {msg2}");
        assert!(msg2.contains("打开设置"), "实际: {msg2}");
    }

    /// `toggle_schema:` 一直写在 key_actions 里、却从未参与过检测——与上面那些
    /// 在内核是同一张 key_down 表，撞车同样是先注册者赢、后者静默失效。
    #[test]
    fn test_key_action_toggle_schema_joins_hotkey_domain() {
        let cfg = Config::from_value(json!({
            "keys": {
                "switch_engine": "ctrl+shift+e",
                "key_actions": { "ctrl+shift+e": "toggle_schema:english" }
            }
        }));
        let msg = check_hotkey_conflicts(&cfg).expect("组合键动词应参与热键域检测");
        assert!(msg.contains("切换到方案「english」"), "实际: {msg}");
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

    /// 造一个记录调用次数的 side committer。
    fn probe(dirty: bool, result: Result<(), String>) -> (SideCommitter, Rc<std::cell::Cell<u32>>) {
        let calls = Rc::new(std::cell::Cell::new(0u32));
        let c = calls.clone();
        (
            SideCommitter {
                is_dirty: Box::new(move || dirty),
                commit: Box::new(move || {
                    c.set(c.get() + 1);
                    result.clone()
                }),
            },
            calls,
        )
    }

    /// 非 config 落点的提交必须**任一失败即中止**，且错误原样上抛。
    ///
    /// 继续往下写的话，toast 只显示得了一条错误——用户看到的会是最后一个错，
    /// 而不是最先出问题的那个；后续提交多半还依赖同一批数据，失败面只会更大。
    #[test]
    fn commit_side_stops_at_first_error() {
        let reg = FieldRegistry::default();
        let (ok1, n1) = probe(false, Ok(()));
        let (bad, n2) = probe(false, Err("方案配置保存失败（wubi）".into()));
        let (ok2, n3) = probe(false, Ok(()));
        reg.side.borrow_mut().push(ok1);
        reg.side.borrow_mut().push(bad);
        reg.side.borrow_mut().push(ok2);

        let err = reg.commit_side().unwrap_err();
        assert_eq!(
            err, "方案配置保存失败（wubi）",
            "错误须原样上抛，供 toast 显示"
        );
        assert_eq!(n1.get(), 1, "失败点之前的照常提交");
        assert_eq!(n2.get(), 1);
        assert_eq!(n3.get(), 0, "失败点之后的不该再执行");
    }

    /// 全部成功时每个都跑到，且返回 Ok。
    #[test]
    fn commit_side_runs_every_committer() {
        let reg = FieldRegistry::default();
        let (a, na) = probe(false, Ok(()));
        let (b, nb) = probe(false, Ok(()));
        reg.side.borrow_mut().push(a);
        reg.side.borrow_mut().push(b);
        assert!(reg.commit_side().is_ok());
        assert_eq!((na.get(), nb.get()), (1, 1));
    }

    /// 「有未保存改动」必须覆盖非 config 落点——否则用户改完方案配置点「恢复本页」，
    /// 确认框不弹，改动被静默丢弃。
    #[test]
    fn side_dirty_is_any_not_all() {
        let reg = FieldRegistry::default();
        assert!(!reg.side_dirty(), "没有登记项时不该判脏");

        reg.side.borrow_mut().push(probe(false, Ok(())).0);
        assert!(!reg.side_dirty(), "登记项都干净时不该判脏");

        // 任一为真即为真：多个落点时，只有一个有改动也必须提醒。
        reg.side.borrow_mut().push(probe(true, Ok(())).0);
        assert!(reg.side_dirty());
    }

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

    /// 外部改动落到界面上时要给一条提示，但**只在值真的变了、且界面上有这一项**时给。
    ///
    /// 覆盖用户未保存改动这件事是既定取舍（见 `apply_external_config_change`），
    /// 但"静默"这一半不是必须的：屏幕上有个开关自己动了却没有任何解释才最让人困惑。
    /// 反过来，未在清单露出的 key（拖动落位写的 `ui.status.custom_x` 之类）界面上根本
    /// 没有对应控件，为它弹提示纯属噪音——这条测试把两侧都钉住。
    ///
    /// 直接跑 `on_core_event` 本体（ctx 由 `windui::testing` 借出），测的是这条链路
    /// 的接线，而非某个抽出来的辅助函数。
    #[test]
    fn external_config_change_notifies_only_on_a_real_change() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        // 清单里真实存在的一项，并给它登记一个加载器（正常由 UI 构建时注册）。
        let key = "input.filter_mode";
        let label = st.manifest.get(key).expect("清单应有此项").label.clone();
        let loaded = Rc::new(std::cell::Cell::new(false));
        let l = loaded.clone();
        st.fields
            .loaders
            .borrow_mut()
            .insert(key.to_string(), Box::new(move |_cfg| l.set(true)));

        let ev = |k: &str| crate::rpc::CoreEvent {
            event: "config.changed".to_string(),
            data: serde_json::json!({ "key": k, "value": true }),
        };
        let res = windui::testing::run_with_ctx(|ctx| st.on_core_event(ctx, &ev(key)));
        assert!(loaded.get(), "前提：该 key 的加载器被调到，界面确实跳变了");
        let toast = res.toast.expect("界面跳变了就该给一条提示");
        assert!(
            toast.text.contains(&label),
            "提示应带清单里的人类可读名，实得：{}",
            toast.text
        );

        // 未在清单/加载器登记的 key：界面上没有这一项，不该打扰。
        let res =
            windui::testing::run_with_ctx(|ctx| st.on_core_event(ctx, &ev("ui.status.custom_x")));
        assert!(res.toast.is_none(), "界面上没有这一项就不该弹提示");

        // **本端自己保存的回声**：core 在 setItems 后同样广播 config.changed（带 key），
        // 而此刻 current_config 已经是这个值——loader 把同一个值再灌一次，控件纹丝不动。
        // 上一轮已把 true 落进 current_config，故这条重复事件不该再提示。
        assert_eq!(
            st.current_config.get_path(key),
            Some(&serde_json::json!(true)),
            "前提：上一轮已把该值落进 current_config"
        );
        let res = windui::testing::run_with_ctx(|ctx| st.on_core_event(ctx, &ev(key)));
        assert!(
            res.toast.is_none(),
            "值没变（本端保存的回声）就不该报「已在别处更改」"
        );

        // 真外部改动：值与本端不同，照常提示。
        let ev_off = crate::rpc::CoreEvent {
            event: "config.changed".to_string(),
            data: serde_json::json!({ "key": key, "value": false }),
        };
        let res = windui::testing::run_with_ctx(|ctx| st.on_core_event(ctx, &ev_off));
        assert!(res.toast.is_some(), "值真的变了才是外部改动，应提示");
    }

    /// 关窗守卫的判据：两个来源各自都要能把窗口拦下来。
    ///
    /// 判错任一侧都是实打实的损失——漏判则未保存的改动被静默丢弃，误判则窗口关不掉
    /// 而用户根本没改过东西。另外 diff 必须跑在副本上：`save` 会把 writers 直接写进
    /// `current_config`，判定若走同一条路，问完"要不要保存"就已经改了一半。
    #[test]
    fn unsaved_changes_come_from_both_config_diff_and_side_committers() {
        // 数据未就绪时不判脏：此刻 base 还是空的，而 empty() 自带的四个内建 writer
        // 照样会写出 keys/schema——不豁免的话，core 没响应时开了设置页就再也关不掉。
        let mut st = AppState::empty();
        assert!(!st.has_unsaved_changes(), "数据未加载完不该判脏");

        // 模拟 apply_loaded 之后：base 里已含各内建 writer 的当前输出。
        st.data_loaded = true;
        let mut base = st.current_config.clone();
        for w in st.fields.writers.borrow().iter() {
            w(&mut base);
        }
        st.base_config = base.clone();
        st.current_config = base;
        assert!(!st.has_unsaved_changes(), "刚加载完、没动过，不该判脏");

        // ① config.toml 侧：writer 写出与 base 不同的值。
        st.fields
            .writers
            .borrow_mut()
            .push(Box::new(|cfg: &mut Config| {
                cfg.set_path("input.filter_mode", serde_json::json!("gb18030"));
            }));
        assert!(st.has_unsaved_changes(), "writer 写出的差异应判脏");
        assert!(
            st.current_config.get_path("input.filter_mode").is_none(),
            "判定不得把 writer 的值落进 current_config（那是 save 的提交路径）"
        );

        // ② config.toml 之外的落点（方案 override、词库开关）。
        let mut st2 = AppState::empty();
        st2.data_loaded = true;
        let (side, _) = probe(true, Ok(()));
        st2.fields.side.borrow_mut().push(side);
        assert!(st2.has_unsaved_changes(), "side committer 脏也算未保存");
    }

    fn check_result(remote: &str) -> crate::update::CheckResult {
        crate::update::CheckResult {
            has_update: true,
            local_version: "1.0.0".into(),
            remote_version: remote.into(),
            release_notes: String::new(),
            download_url: "https://example.com/a.exe".into(),
            sha256: "0".repeat(64),
            size: 1024,
            local_unparsable: false,
        }
    }

    /// 过期的检查结果不许覆盖当前状态。
    ///
    /// 复现的那条链：网络慢 → 用户关掉对话框再点一次「检查更新」（关闭并不取消已发出的
    /// 请求，于是两次检查并存）→ 先发的那次晚回来 → 无条件 `set(HasUpdate)` 把正在下载
    /// 的界面打回「发现新版本」→ 用户再点一次下载 → 两个下载线程 → 先完成的那个把界面
    /// 推到「安装」。请求号就是为切断这条链而加的。
    #[test]
    fn stale_check_result_does_not_hijack_current_state() {
        use crate::update::{UpdateMsg, UpdatePhase};
        let st = AppState::empty();
        // 已发起过两次检查，界面此刻在下载中。
        st.update_check_seq.set(2);
        st.update_phase.set(UpdatePhase::Downloading);

        // 第一次检查（seq=1）的结果现在才回来——必须被丢弃。
        st.on_update_msg(UpdateMsg::CheckDone {
            result: Ok(check_result("2.0.0")),
            silent: false,
            seq: 1,
        });
        assert_eq!(
            st.update_phase.get(),
            UpdatePhase::Downloading,
            "过期结果不该打断正在进行的下载"
        );

        // 静默检查同理：它的请求号更早，晚到时不该弹对话框抢界面。
        st.on_update_msg(UpdateMsg::CheckDone {
            result: Ok(check_result("2.0.0")),
            silent: true,
            seq: 1,
        });
        assert!(
            !st.update_dialog_visible.get(),
            "过期的静默检查结果不该弹出对话框"
        );

        // 最新那次（seq=2）照常生效。
        st.on_update_msg(UpdateMsg::CheckDone {
            result: Ok(check_result("2.0.0")),
            silent: false,
            seq: 2,
        });
        assert_eq!(st.update_phase.get(), UpdatePhase::HasUpdate);
    }

    /// 检查不许同类型重入，且**过期结果不得清掉正在跑那次的标记**。
    ///
    /// 前者防的是"网络慢（最长 10s）→ 用户反复点检查更新"：每点一次起一个 HTTP 请求。
    /// 后者是这条防重入自己的陷阱——若过期结果也复位标记，用户中途换过一次请求时，
    /// 真正在跑的那次就失去了保护，狂点又能起线程了。
    ///
    /// 静默与手动**不互相复用**：两者结果处理不同（静默无更新时不弹对话框），
    /// 让手动那次去等静默的结果，界面会永远停在「检查中」。
    #[test]
    fn check_reentry_is_blocked_per_kind_and_survives_stale_results() {
        use crate::update::UpdateMsg;
        let st = AppState::empty();
        assert!(st.try_begin_check(false), "首次手动检查应放行");
        assert!(!st.try_begin_check(false), "同类型重复请求应被挡下");
        assert!(
            st.try_begin_check(true),
            "静默检查不该被手动检查挡住——两者结果处理不同"
        );

        // 过期结果（seq 不是最新）不得清掉标记。
        st.update_check_seq.set(7);
        st.update_checking.set(Some(false));
        st.on_update_msg(UpdateMsg::CheckDone {
            result: Ok(check_result("2.0.0")),
            silent: false,
            seq: 6,
        });
        assert_eq!(
            st.update_checking.get(),
            Some(false),
            "过期结果不该把正在跑那次的防重入标记抹掉"
        );

        // 最新那次到达才算结束，之后可以再发起。
        st.on_update_msg(UpdateMsg::CheckDone {
            result: Ok(check_result("2.0.0")),
            silent: false,
            seq: 7,
        });
        assert_eq!(st.update_checking.get(), None, "最新结果到达即结束");
        assert!(st.try_begin_check(false), "结束后应可再次发起");
    }

    /// 下载不许重入：一次只能有一个下载线程，两个会写同一个目标文件、进度互相打架。
    /// 三种终态消息（完成/失败/取消）到达后才允许再次开始。
    #[test]
    fn download_reentry_is_blocked_until_a_terminal_message() {
        use crate::update::UpdateMsg;
        let st = AppState::empty();
        assert!(st.try_begin_download(), "首次请求应放行");
        assert!(!st.try_begin_download(), "下载进行中，重复请求应被挡下");

        st.on_update_msg(UpdateMsg::DownloadCancelled);
        assert!(st.try_begin_download(), "取消后应可重新开始");

        st.on_update_msg(UpdateMsg::DownloadFailed("网络错误".into()));
        assert!(st.try_begin_download(), "失败后应可重试");

        st.on_update_msg(UpdateMsg::DownloadDone(std::path::PathBuf::from("a.exe")));
        assert!(st.try_begin_download(), "完成后标志也应复位");
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
        // 真的把图标回调跑一遍：ctx 由 `windui::testing` 借出（回调收 `&mut EventCtx`，
        // 只有宿主造得出）。这样测的是回调**挂没挂对**，而不只是它调用的那个函数。
        let on_delete = &items[2].trailing_icon.as_ref().unwrap().1;
        windui::testing::run_with_ctx(|ctx| on_delete(ctx));
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
