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
/// 自定义标点表的单元格信号：每行 `[中半, 英全, 中全, 英半]`，行序同
/// `manifest::PUNCT_DEFAULTS`。见 [`FieldRegistry::punct_cells`]。
pub type PunctCells = Rc<Vec<[Signal<String>; 4]>>;
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
    /// bool 清单项 key → 它的 Signal，供**互斥联动**跨控件改对方的值
    /// （`MItem::exclusive_with`，见 `manifest::build_toggle`）。
    ///
    /// 与 `getters` 分开：那张表是只读取值器（`enabled_when` 用），拿不到写入端。
    ///
    /// ⚠️ 与 `global_flags` 同一条纪律：**必须**在 [`FieldRegistry::reset`] 里清空，
    /// Signal 句柄留过页面重建就是悬垂。
    pub bool_signals: RefCell<HashMap<String, Signal<bool>>>,
    /// 响应式 select（`options_source`）的**选中索引**信号：key → `Signal<usize>`。
    ///
    /// 这类下拉的选中态是**索引**而不是值，而选项列表是异步到达、且会被整份替换的
    /// （主题列表在导入/删除之后重扫）。列表一换、索引没跟着走，同一个下标就指到了
    /// 别的主题上——现象是「下拉显示的不是我选的那个」，而点一次保存会把那个错的值
    /// 写进配置。登记在这里，好让重扫列表的那一侧（[`AppState::refresh_theme_names`]）
    /// 按**值**把索引重新定位回去。
    ///
    /// ⚠️ 与 `bool_signals` 同一条纪律：**必须**在 [`FieldRegistry::reset`] 里清空。
    pub select_indices: RefCell<HashMap<String, Signal<usize>>>,
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
    /// 日志级别下拉的当前值探针，供「隐私提示」判断此刻要不要示警。
    ///
    /// 三个模块的级别落点各不相同（core config / tsf_log_config / wind_setting.toml，
    /// 见 `manifest::local_field`），没有一个共同的读取面；而提示要跟着用户**尚未保存**
    /// 的选择实时变化，也就不能去读盘。故各控件构建时各自登记一个读当前值的闭包。
    ///
    /// ⚠️ 闭包捕获 Signal 句柄，与 `global_flags` 同一条纪律：**必须**在
    /// [`FieldRegistry::reset`] 里清空，留着就是悬垂。
    pub log_level_probes: RefCell<Vec<Box<dyn Fn() -> String>>>,
    /// `system.*` 的**加载态**基线（键 → 加载时的值）。
    ///
    /// 这些键写进 config.toml 不等于生效——真正的落点在 HKLM，得由提权进程去写。
    /// [`SideCommitter`] 靠「当前值 != 加载态」决定要不要执行那次提权动作，所以基线
    /// 必须是**加载时**的值，且其确立时刻不能受用户操作影响。由
    /// `AppState::apply_loaded` 播种（见 [`FieldRegistry::seed_system_baseline`]）。
    ///
    /// ⚠️ 刻意**不随 [`FieldRegistry::reset`] 清空**：它描述的是系统当前状态，与设置树
    /// 的生死无关——关窗重开不会让 HKLM 里登记的名字变回去。
    pub system_baseline: RefCell<HashMap<String, Value>>,
    /// 对话框键 → 该对话框的可见信号，供搜索跳转**程序化打开**它。
    ///
    /// 键与 `search::index` 派生的 `JumpTarget::RowInDialog::dialog` 一致：寄居式对话框
    /// 用宿主项的清单 key。（方案页那四个全局配置**已不再是对话框**——2026-09 起铺成页内
    /// 卡片，`scheme_cfg:<标签>` 那批登记随之取消，那些项现在走普通的 `Row` 跳转。）
    ///
    /// 这些 `visible` 信号原本是各构建函数里的局部变量、只被按钮闭包捕获，外界拿不到；
    /// 登记一份出来，搜索命中框内项时才能替用户把框打开。
    pub dialog_opens: RefCell<HashMap<String, Signal<bool>>>,
    /// 自定义标点表的单元格信号（每行 `[中半, 英全, 中全, 英半]`，行序同
    /// `manifest::PUNCT_DEFAULTS`），由 [`crate::manifest::punct_cells`] 惰性建好后共享。
    ///
    /// ★ **共享是硬要求，不是省事**：表格对话框与「`/` 键输出顿号」快捷开关改的是同一份
    /// 配置（`input.punct.custom_mappings`）。各持一份就有两个都 `set_path` 整张表的
    /// 覆盖式写入者，谁后写谁赢——用户在表格里改的行会被开关那份悄悄抹掉。共享之后
    /// 写入者只剩表格 writer 一个，快捷开关只是替用户改格子。
    ///
    /// ⚠️ 与 `bool_signals` 同一条纪律：**必须**在 [`FieldRegistry::reset`] 里清空，
    /// Signal 句柄留过页面重建就是悬垂。
    pub punct_cells: RefCell<Option<PunctCells>>,
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
        self.bool_signals.borrow_mut().clear();
        self.select_indices.borrow_mut().clear();
        self.loaders.borrow_mut().clear();
        self.dialogs.borrow_mut().clear();
        self.global_flags.borrow_mut().clear();
        self.side.borrow_mut().clear();
        self.log_level_probes.borrow_mut().clear();
        self.dialog_opens.borrow_mut().clear();
        *self.punct_cells.borrow_mut() = None;
        // ⛔ 不清 `system_baseline`：见其字段注释。
    }

    /// 播种 `system.*` 加载态基线。见 [`FieldRegistry::system_baseline`]。
    ///
    /// ★ 键**不在** `cfg` 里是**常态而非异常**：配置层写回会剔除「等于默认值」的键，
    /// 所以把开关关掉再保存之后，整个 `[system]` 段都会从 config.toml 里消失。
    /// 此时基线必须回落到默认值——留空会让 [`system_needs_apply`] 恒判 false，
    /// 开关就再也拧不动了（且毫无提示，和这次修掉的老毛病表现一模一样）。
    ///
    /// `caps` 也拿不到默认值时按键的类型兜底（见 [`SYSTEM_BASELINE_KEYS`] 的第二列）：
    /// 布尔键回 `false`（未启用 = 不对系统做任何事），字符串键回空串（core 侧把空串
    /// 定义为「用出厂值」）。兜底值只在快照与 core 漂移时才用得上。
    ///
    /// ★ `sys` 是**注册表实况**（`dota2_compat::probe()`），有就以它为准 —— 基线要回答的
    /// 是「系统现在是什么样」，而那件事 config.toml 并不知道。拿不到（core 没装好、
    /// 命令失败）时退回按配置播种，即此前的老行为：宁可漂移不可见，也不能让开关失灵。
    pub fn seed_system_baseline(
        &self,
        cfg: &Config,
        caps: &Capabilities,
        sys: Option<&crate::dota2_compat::SystemAlias>,
    ) {
        let mut b = self.system_baseline.borrow_mut();
        b.clear();
        for (key, fallback) in SYSTEM_BASELINE_KEYS {
            let v = cfg
                .get_path(key)
                .cloned()
                .unwrap_or_else(|| caps.default_for(key));
            let v = if v.is_null() { fallback.value() } else { v };
            b.insert((*key).to_string(), v);
        }
        let Some(sys) = sys else { return };
        // 开关的实况：登记名是不是已经不等于真实名。
        b.insert(DOTA2_KEY.to_string(), Value::Bool(sys.enabled_now()));
        // 名称的实况只在**开着**时有意义：关着的时候注册表里就是真实名，拿它当名称基线
        // 会把「关着 + 界面上填了个名字」误判成改名（见 `system_needs_apply`，名称只在
        // 开着时参与比较）。故关着时保留上面按配置播下的值。
        if let Some(alias) = sys.alias_now() {
            b.insert(DOTA2_NAME_KEY.to_string(), Value::String(alias.to_string()));
        }
    }

    /// 读一个 `system.*` 键的加载态。`None` = 尚未播种（数据还没加载完）。
    pub fn system_baseline_bool(&self, key: &str) -> Option<bool> {
        self.system_baseline
            .borrow()
            .get(key)
            .and_then(|v| v.as_bool())
    }

    /// 同上，字符串键。
    pub fn system_baseline_str(&self, key: &str) -> Option<String> {
        self.system_baseline
            .borrow()
            .get(key)
            .and_then(|v| v.as_str().map(str::to_string))
    }

    /// 落地成功后把基线推到新值——否则同一次会话里再点一次保存会再弹一次 UAC。
    pub fn set_system_baseline(&self, key: &str, value: Value) {
        self.system_baseline
            .borrow_mut()
            .insert(key.to_string(), value);
    }
}

/// `caps` 与 config 都给不出值时，一个 `system.*` 键的兜底基线。
#[derive(Clone, Copy)]
pub(crate) enum SystemBaselineFallback {
    Bool(bool),
    Str(&'static str),
}

impl SystemBaselineFallback {
    fn value(self) -> Value {
        match self {
            Self::Bool(b) => Value::Bool(b),
            Self::Str(s) => Value::String(s.to_string()),
        }
    }
}

/// 需要跟踪「加载态」的 `system.*` 键，及其兜底基线。
///
/// 新增 `system.*` 键时**必须**登记在此，否则它的 [`SideCommitter`] 判不出脏、落地动作
/// 永不执行——而界面上开关照样拧得动，用户只会以为设置坏了。
pub(crate) const DOTA2_KEY: &str = "system.dota2_compat";
pub(crate) const DOTA2_NAME_KEY: &str = "system.dota2_compat_name";

pub(crate) const SYSTEM_BASELINE_KEYS: &[(&str, SystemBaselineFallback)] = &[
    (DOTA2_KEY, SystemBaselineFallback::Bool(false)),
    (DOTA2_NAME_KEY, SystemBaselineFallback::Str("")),
];

/// Dota 2 兼容的落地意图：开关 + 要登记的名称。
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Dota2Intent {
    pub enabled: bool,
    pub name: String,
}

/// 是否需要执行落地动作。
///
/// 抽成纯函数是为了**可测**：`commit` 本身会弹 UAC，测试里跑不了；判据留在闭包内
/// 就只能靠肉眼守。`is_dirty` 与 `commit` 共用它，两者不会各判各的。
///
/// `base` 为 `None` 表示加载态还不知道（数据没到）——此时一律不动系统。
///
/// **名称只在开着的时候算数**：关闭状态下注册表里就是真实名称，改名字改不到任何东西，
/// 此时判脏只会白弹一次 UAC。反过来，开关没动、只改了名字**必须**落地——否则用户
/// 换了个名字点应用，界面上写着新名字、系统里还是旧的，且没有任何提示。
fn system_needs_apply(base: Option<&Dota2Intent>, now: &Dota2Intent) -> bool {
    let Some(base) = base else { return false };
    if base.enabled != now.enabled {
        return true;
    }
    now.enabled && base.name != now.name
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
    /// 这个落点在「未保存项」清单里的名字（见 [`AppState::unsaved_items`]）。
    ///
    /// config.toml 的项报得出配置键，非 config 落点没有键可报，只能各自起一个。
    /// 缺了它就会出现最糟的一种：`side_dirty()` 为真而 config diff 为空，对话框
    /// 说着「有未保存的更改」却一条也列不出来——比不列更让人无从下手。
    pub label: &'static str,
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

    /// 有待提交改动的非 config 落点的名字，供「未保存项」清单展示。
    ///
    /// 与 [`Self::side_dirty`] 判的是同一件事（前者即本方法非空），拆成两个是因为
    /// `save` 只需要那个 bool，而每次保存都去拼一遍字符串没有意义。
    pub fn side_dirty_labels(&self) -> Vec<String> {
        self.side
            .borrow()
            .iter()
            .filter(|s| (s.is_dirty)())
            .map(|s| s.label.to_string())
            .collect()
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

/// 悬停清单最多逐条列出几项。
///
/// 超过就截断：tooltip 的行数不设上限（见 windui `Element::tooltip` 的说明），几十项
/// 会铺满一屏，而那时该做的是点一下把全量复制走、贴给维护者，不是在浮层里读。
/// 取 8 而不是三五条——误报现场通常就那么几个键，一眼能看完的概率越高越好。
const TIP_MAX_ITEMS: usize = 8;

/// 「未保存项」悬停文案：一行一项，超量截断，末尾提示可点。
///
/// 抽成纯函数是为了**可测**：它的两条边界（恰好 `TIP_MAX_ITEMS` 项不截断、多一项就
/// 截断）都是差一错易发处，而对话框本身在测试里建不出来。
fn unsaved_tip_text(items: &[String]) -> String {
    let mut lines: Vec<String> = items.iter().take(TIP_MAX_ITEMS).cloned().collect();
    if items.len() > TIP_MAX_ITEMS {
        lines.push(format!("…… 等 {} 项", items.len()));
    }
    // 角标没有按钮外观，不写这一句没人知道它能点（光标手型只在指到时才看得见，
    // 而那时提示已经弹出来了）。
    lines.push("（点击复制全部）".to_string());
    lines.join("\n")
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

/// 设置向导所处的段：未保存修改的拦截页 → 欢迎页 → 4 步 → 二次确认 → 失败结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardPhase {
    Dirty,
    /// 欢迎页：说明向导做什么、指向官网文档与主题站。不计入步数。
    Welcome,
    /// 1..=4
    Step(u8),
    Confirm,
    Result,
}

/// 一次向导会话：打开时按当前配置预填，关闭或写入成功后清空。
pub struct WizardSession {
    /// 打开时的当前配置（摘要的「旧值」一侧）。
    pub cur: crate::wizard::model::Draft,
    pub draft: crate::wizard::model::Draft,
    /// 双拼布局 (id, 显示名)，取自 `schema.layouts`。
    pub layouts: Vec<(String, String)>,
    pub outcome: Option<crate::wizard::apply::ApplyOutcome>,
}

/// 应用全局状态。
/// 主题下载的发起方：两个主题导入对话框共用同一个任务槽，取消时靠它认主。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeDlOwner {
    /// 用户主动打开的「导入主题」对话框。
    Picker,
    /// 协议链接唤起的主题导入确认框。
    Protocol,
}

pub struct AppState {
    /// 从 core 加载的原始配置（用于 diff 基线）。
    pub base_config: Config,
    /// 当前编辑中的配置（保存时由写回器填充）。
    pub current_config: Config,
    /// 设置清单（内嵌清单，启动即就位；已按定制版 `[settings] hide` 折叠，见 [`crate::custom_hide`]）。
    pub manifest: Manifest,
    /// `manifest` 折叠时「显示定制版隐藏的设置」的取值，见 [`Self::refold_manifest_if_needed`]。
    pub manifest_show_all: bool,
    /// core 能力契约（system.capabilities，异步加载后就位）。
    pub capabilities: Capabilities,
    /// 系统信息（system.info）。
    pub system_info: Value,
    /// 配置降级提示文案（顶栏）；空串 = 本次加载没有降级，横幅不显示。
    ///
    /// 来自 `config.degradation`。降级 = 某一段配置解析失败、已整段回落出厂默认，
    /// 用户写在配置文件里的那一段**没有生效**——而他在界面上只会看到「我的设置怎么
    /// 变回默认了」，唯一的线索此前只在日志文件里。见 [`crate::pages::degradation_bar`]。
    pub degradation: Signal<String>,
    /// 定制版徽章文案（关于页）；空串 = 非定制版，徽章整个不显示。
    ///
    /// ★ **必须是 Signal，不能让关于页直接读 `system_info`。** 页面树由 `shell::build`
    /// 在窗口 content 构建时一次性建好，而 `system_info` 要等后台线程 fetch 完、经
    /// channel 回调 [`AppState::apply_loaded`] 才到位——构建期读它恒为空。关于页此前
    /// 全是静态内容（参数一直叫 `_state`），是本仓第一个消费 RPC 数据的手写页，
    /// 于是第一次撞上这个时序。徽章不出现、测试全绿、clippy 干净，只有 `--screenshot` 看得见。
    pub custom_edition: Signal<String>,
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
    /// 「导入主题」对话框（用户**主动**发起的那个）是否可见。
    ///
    /// 与 [`Self::theme_import_dialog_visible`] 是两回事：那个由 `windinput://import/theme`
    /// 从浏览器唤起，问的是「这个来源可信吗」；这个是用户自己点开的，问的是「你要从哪导」。
    pub theme_picker_visible: Signal<bool>,
    /// 「导入主题」对话框里的链接输入。
    pub theme_picker_url: Signal<String>,
    /// 「导入主题」对话框里粘贴的主题原文。
    pub theme_picker_text: Signal<String>,
    /// 主题链接下载空闲（`true` = 可发起）。两个主题导入对话框共用，同一时刻只会开一个。
    /// 下载中按钮置灰：慢网下用户以为没点着、反复点，会排出一串重复导入。
    pub theme_download_idle: Signal<bool>,
    /// 在途的主题下载任务号及其属主，关框 / 换新请求时取消用。属主必须记：两个对话框共用
    /// 这一个槽，没有属主的话「一个没在下载的框被收起」也会取消另一个框的下载。
    theme_download_task: Cell<Option<(crate::net_task::TaskId, ThemeDlOwner)>>,
    /// 在途的协议方案包下载任务号；非空时拒绝再起一个（方案包最大 64 MB，不叠着下）。
    schema_download_task: Cell<Option<crate::net_task::TaskId>>,
    /// 「正在下载方案包」对话框。协议导入没有别的界面可挂取消按钮，慢网下要给个出口。
    pub schema_download_visible: Signal<bool>,
    /// 后台网络任务（HTTP 下载）。`main()` 在 App 构建期接上 channel。
    /// `Rc`：channel 回调要在放掉 `AppState` 借用之后再跑续体，见 `main` 里的注册处。
    pub net: Rc<crate::net_task::NetTasks>,
    /// 当前 Tab 索引。
    pub tab: Signal<usize>,
    /// 全局搜索状态（索引 + 查询 + 结果 + 浮层开合）。
    pub search: crate::search::SearchState,
    /// 待执行的跳转请求：搜索确认时挂上，由目标位置的锚点认领后清空。
    ///
    /// 分两步而不是当场滚过去，是因为目标那一刻还不在布局里——它所在的页上一帧
    /// 还不可见，`scroll_into_view` 拿到的会是过期 bounds。
    pub jump: Signal<Option<crate::search::JumpRequest>>,
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
    /// 日志打包通道：压缩放在后台线程（日志多时上百 MB，在 UI 线程做会卡住窗口），
    /// 结果经此回 UI 线程弹提示。`main()` 在 App 构建期注入一次。
    /// `Rc` 包一层：设置页按钮的回调要 `'static`，拿不到 `&AppState`。
    pub log_pack_tx: Rc<RefCell<Option<Sender<crate::log_tools::PackResult>>>>,
    /// 正在打包。连点不会起第二个线程。
    pub log_pack_busy: Rc<Cell<bool>>,
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
    /// 底栏「主题色」下拉的选中下标（[`crate::accent::AccentMode::index`]）。
    pub accent_idx: Signal<usize>,
    /// 「界面细节」试验选项（见 [`crate::look`]）。子窗分段控件直接绑这组信号。
    pub look: crate::look::LookSignals,
    /// 跟随模式下取到的输入法主题色（未修正的两档原色）；`None` = 还没取 / 取不到。
    ime_accent: RefCell<Option<crate::accent::ThemeAccent>>,
    /// 上一次取色请求的主题名（空串 = 当前主题）。与此刻要跟随的不同才重取；`None` = 没取过。
    accent_fetched_for: RefCell<Option<String>>,
    /// 底栏角标提示：跟随模式下说明用了什么 / 为什么没用；默认模式为空串（角标收起）。
    pub accent_note: Signal<String>,
    /// 上一次换上的（明暗, 强调色）。相同就不再 `ThemeHandle::set`——那会整窗重绘。
    applied_theme: Cell<Option<(bool, Option<crate::accent::AccentSet>)>>,
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
    /// 高级页「性能与诊断」里的内存诊断（摘要行 + 诊断信息对话框）。
    pub memory: crate::pages::memory::MemoryDiag,
    /// 「应用兼容」窗的状态（每次改动立即经 `compat.*` 落盘，不走 `FieldRegistry`）。
    pub compat_mgr: crate::pages::compat::CompatState,
    /// 「重新加载」二次确认对话框可见标志。
    pub reload_confirm: Signal<bool>,
    /// 「恢复本页」（有未保存改动时）确认对话框可见标志。
    pub reset_confirm: Signal<bool>,
    /// 关窗前「有未保存的更改」确认对话框可见标志。
    pub exit_confirm: Signal<bool>,
    /// 关窗确认框正文那行「共 N 项未保存」。
    ///
    /// 由 [`AppState::prime_exit_confirm`] 在弹框那一刻填好。对话框在 `shell::build`
    /// 时就建好了，而清单要到关窗才知道，故只能走 Signal。
    pub unsaved_note: Signal<String>,
    /// 关窗确认框角标的悬停文案（逐行列出未保存项，超量截断）。
    pub unsaved_tip: Signal<String>,
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
    /// 主题包「要装这个吗」确认框的可见标志。
    ///
    /// 与覆盖确认分开两组 signal 而不是复用一组：两者会**接连出现**（先确认装、装的
    /// 时候撞名再确认覆盖），共用一组的话第二个框会把第一个的内容冲掉。
    pub theme_pkg_confirm_visible: Signal<bool>,
    /// 确认框正文：包里装的是什么（见 `ThemePreview::confirm_text`）。
    pub theme_pkg_confirm_msg: Signal<String>,
    /// 待确认导入的主题包路径。
    ///
    /// ⚠️ **取消不清它**（通用确认框没有取消回调，`confirm_dialog_msg` 只管确认那一侧）。
    /// 因此每个拉起这个确认框的入口**都必须先写 path 再置 visible**——少写一步的现象是
    /// 「确认之后装上的是上一次放弃掉的那个包」。现有入口（argv/协议分派、导入对话框选到
    /// `.wtheme`）都照做了。
    pub theme_pkg_confirm_path: Signal<String>,
    /// 待覆盖导入的**来源**：撞上同名主题时暂存于此，用户确认后以 `force=true` 重推。
    ///
    /// 文本来源存的是全文而不是地址——URL 导入重下要多一次网络往返，且拿到的可能已不是
    /// 用户刚才确认过的那一份。包来源只能存路径：包揣不进内存等确认，而本地文件在这几秒
    /// 里被换掉的风险远低于重新下载一次。
    pub theme_overwrite_source: Signal<Option<crate::theme_import::ThemeSource>>,
    /// 覆盖确认对话框正文（含主题名）。
    pub theme_overwrite_msg: Signal<String>,
    /// 覆盖确认对话框可见标志。
    pub theme_overwrite_visible: Signal<bool>,
    /// 候选窗字体下拉的响应式选项（system.fonts）：value 与 label 都是字体族名。
    ///
    /// 不再有空值「默认」首项：「跟随主题」由勾选行表达（R8.1），未勾选时控件整个藏起、
    /// 落盘空串，下拉里再放一个空值档就是同一件事的第二个入口。
    pub font_sig: Signal<Vec<String>>,
    pub font_label_sig: Signal<Vec<String>>,
    /// 当前主题对外观可覆盖键解析后的值（`theme.resolved`），外观页「跟随主题（值）」读它。
    /// `None` = 还没拉到，或两级主题都加载失败——文案退化为「跟随主题」。
    ///
    /// 页面树早于数据建成，故走信号；切主题保存后由 [`AppState::refresh_theme_follow`] 重拉。
    pub theme_follow: Signal<Option<crate::theme_follow::ThemeFollow>>,
    /// 「已被 N 个方案覆盖」（F9/R8.6）：`schema.overrideSummary` 的回包，
    /// `配置键 → 覆盖它的方案 id 列表`（只含可见方案，按方案列表序）。
    ///
    /// 页面树早于数据建成，故走信号（同 [`Self::theme_follow`]）；`manifest::field_with`
    /// 用它现算角标文案（见 `manifest::override_badge_text`）。方案设置对话框保存
    /// （落 `schema_overrides/<id>.toml`）后由 [`Self::refresh_override_summary`] 重拉——
    /// 那一次保存正是这份汇总唯一会变的时刻。
    pub override_summary: Signal<HashMap<String, Vec<String>>>,
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
    /// 设置向导可见标志。
    pub wizard_visible: Signal<bool>,
    /// 向导「下次不再弹出」勾选态，初值取自 `[ui] wizard_dismissed`。
    pub wizard_dont_show: Signal<bool>,
    pub wizard_phase: Signal<WizardPhase>,
    /// 当前向导会话；`None` = 向导没开。
    pub wizard: Rc<RefCell<Option<WizardSession>>>,
    /// 草稿每次修改 +1，驱动向导正文重算。
    pub wizard_rev: Signal<u64>,
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
    /// 「打开设置时自动检查更新」勾选框的绑定信号，初值取自 `setting_config.update.auto_check`。
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

/// 登记 `[system]` 域的落地动作。
///
/// # 必须可重入，且必须在每次 `FieldRegistry::reset()` 之后重新调用
/// 与 [`register_schema_registrations`] 同一约束、同一原因（见其文档）。
///
/// `system.*` 的键有个共同点：**写进 config.toml 不等于生效**——真正的落点在
/// HKLM，得由提权进程去写。所以每个这样的键都要在这里挂一个 SideCommitter，
/// 否则界面上拧了、配置也存了，系统里什么都没变（config-design-rules 明令禁止
/// 这种「拧了没反应的旋钮」）。
pub(crate) fn register_system_registrations(fields: &Rc<FieldRegistry>) {
    // Dota 2 兼容：改的是本输入法在系统里登记的显示名。
    //
    // ★ 只在**值确实变了**时才执行——落地要弹 UAC，每次保存都弹一次不可接受。
    // 基线取「本次加载时的值」，由 `AppState::apply_loaded` 播种进 `system_baseline`；
    // is_dirty 拿当前值与它比。用户改回原值再保存 ⇒ 不脏 ⇒ 不弹窗，符合直觉。
    //
    // ⛔ 别改回「首次问到值时惰性播种」。那版看着更省事，实际上开关**永远不生效**：
    // 惰性播种唯一的触发点是 `is_dirty`，而它在真实 UI 路径上只有关窗确认与
    // 「恢复本页」两个调用点，正常的「拧开关 → 点应用」全程不经过它；于是 `save()`
    // 里 `side_dirty()` 先跑，在那一刻才把**用户刚改的新值**当成基线，紧接着
    // `commit_side()` 一比就判「没变」而跳过落地。症状：config.toml 写了、注册表
    // 纹丝不动、UAC 也不弹。见 `dota2_compat_toggle_is_dirty_after_load`。
    const KEY: &str = DOTA2_KEY;
    const NAME_KEY: &str = DOTA2_NAME_KEY;

    // 名称键可能**尚未注册 getter**：它由 manifest 驱动，用户没翻到「高级」页时那批
    // 控件还没建出来。此时回落到基线（= 加载时的值），意思是「名字没被人动过」，
    // 于是只按开关判脏。⛔ 别回落到空串——那会让「开着 + 没翻页 + 点应用」误判成
    // 改名，白弹一次 UAC 还把名字改成出厂值。
    let read_now = {
        let fields = fields.clone();
        move || -> Option<Dota2Intent> {
            let enabled = {
                let g = fields.getters.borrow();
                g.get(KEY).and_then(|f| f().as_bool())?
            };
            let name = {
                let g = fields.getters.borrow();
                g.get(NAME_KEY)
                    .and_then(|f| f().as_str().map(str::to_string))
            };
            let name = match name {
                Some(v) => v,
                None => fields.system_baseline_str(NAME_KEY).unwrap_or_default(),
            };
            // 首尾空白在这里就剪掉：core 的清洗规则头一条就是 trim，不先剪的话
            // 「在名字后面多敲了一个空格」会被判成改名，白弹一次 UAC 去写一个与
            // 现值完全相同的注册表项。⛔ 只做 trim，别在这儿复制 core 的其余规则。
            let name = name.trim().to_string();
            Some(Dota2Intent { enabled, name })
        }
    };
    let base_now = {
        let fields = fields.clone();
        move || -> Option<Dota2Intent> {
            Some(Dota2Intent {
                enabled: fields.system_baseline_bool(KEY)?,
                name: fields.system_baseline_str(NAME_KEY).unwrap_or_default(),
            })
        }
    };

    let dirty_read = read_now.clone();
    let dirty_base = base_now.clone();
    let commit_read = read_now;
    let commit_base = base_now;
    let commit_fields = fields.clone();

    fields.side.borrow_mut().push(SideCommitter {
        label: "Valve 游戏兼容模式",
        is_dirty: Box::new(move || {
            let Some(now) = dirty_read() else {
                return false;
            };
            system_needs_apply(dirty_base().as_ref(), &now)
        }),
        commit: Box::new(move || {
            let Some(now) = commit_read() else {
                return Ok(());
            };
            if !system_needs_apply(commit_base().as_ref(), &now) {
                return Ok(());
            }
            crate::dota2_compat::apply(now.enabled, &now.name)?;
            // ⚠️ 推进基线用的是**界面上那个串**，而注册表里落的是 core 清洗过的串。
            // 两者只在首尾空白上可能不同，而 `read_now` 已经先 trim 过一道（见那里）；
            // 其余清洗规则都是「要么原样通过、要么整条拒绝」，不会改写内容。
            commit_fields.set_system_baseline(KEY, Value::Bool(now.enabled));
            commit_fields.set_system_baseline(NAME_KEY, Value::String(now.name));
            Ok(())
        }),
    });
}

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
            label: "方案配置与词库开关",
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
        let loaded = schema_mgr.key_actions_loaded.clone();
        fields.writers.borrow_mut().push(Box::new(move |cfg| {
            // ★ 从没成功读到过就不写 —— 与下面 schema.active 的空串跳过同一个理由。
            // 本 writer 是**整表覆盖**，而构造期那次同步 RPC 在离线/服务未就绪时会失败并
            // 落一张空表。不设这道闸的话，用户点一次「应用设置」就把 keys.key_actions 整
            // 体写成 `{}` ⇒ 方案直达热键 / 往返热键 / special / 五c 收编后的全部引导键
            // 一次清空；又因 `{}` 恰好等于出厂默认，core 的 `set_user_value` 会判「等于
            // 默认」把整个键删掉，连痕迹都不留。
            if !loaded.get() {
                tracing::warn!("keys.key_actions 尚未成功加载，跳过写回（避免整表清空）");
                return;
            }
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
        let setting_load_wizard_dismissed = setting_load.config.ui.wizard_dismissed;
        // 定制版隐藏的设置项（`[settings] hide`）。页面与搜索索引必须用同一份清单：
        // 各折各的，就会搜到一项、点过去却什么都没有。
        let manifest_show_all = setting_load.config.ui.show_custom_hidden_settings;
        let manifest = crate::custom_hide::settings_manifest(
            crate::custom_hide::installed(),
            manifest_show_all,
        );
        let search = crate::search::SearchState::new(&manifest);
        // 方案设置对话框里的方案级项复用全局 key，也要按同一份折叠结果藏。
        schema_mgr.set_manifest(Rc::new(manifest.clone()));
        // 明暗：**None = 跟随系统**。不能写成 `unwrap_or(false)` —— 那会让系统处于深色
        // 主题的用户每次启动被强制翻成浅色，且与「用户显式选了浅色」再也分不开。
        let initial_dark = setting_load
            .config
            .ui
            .dark
            .unwrap_or_else(crate::theme_def::system_dark);
        let initial_accent =
            crate::accent::AccentMode::from_config(setting_load.config.ui.accent.as_deref());
        let initial_look = setting_load.config.ui.look.indices();
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
            manifest,
            manifest_show_all,
            capabilities: Capabilities::from_embedded(),
            system_info: Value::Object(serde_json::Map::new()),
            degradation: signal(String::new()),
            custom_edition: signal(String::new()),
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
            theme_picker_visible: signal(false),
            theme_picker_url: signal(String::new()),
            theme_picker_text: signal(String::new()),
            theme_download_idle: signal(true),
            theme_download_task: Cell::new(None),
            schema_download_task: Cell::new(None),
            schema_download_visible: signal(false),
            net: Rc::new(crate::net_task::NetTasks::default()),
            tab: signal(0),
            // 搜索索引在上面构建：清单是内嵌的、启动即就位，不必等 RPC。
            search,
            jump: signal(None),
            dark: signal(initial_dark),
            conn: signal(ConnState::Loading),
            feedback: signal(None),
            conflict: signal(String::new()),
            schema_mgr,
            dict_mgr: DictManagerState::new(),
            pending_dict_window: Cell::new(false),
            pending_main_window: Cell::new(false),
            ui_tx: RefCell::new(None),
            log_pack_tx: Rc::new(RefCell::new(None)),
            log_pack_busy: Rc::new(Cell::new(false)),
            main_window_closed: signal(false),
            theme_handle: RefCell::new(None),
            accent_idx: signal(initial_accent.index()),
            look: crate::look::LookSignals::new(initial_look),
            ime_accent: RefCell::new(None),
            accent_fetched_for: RefCell::new(None),
            accent_note: signal(String::new()),
            applied_theme: Cell::new(None),
            dict_is_main: Cell::new(false),
            pending_dict_target: Rc::new(RefCell::new(None)),
            backup_mgr: crate::pages::backup::BackupState::new(),
            config_import: crate::pages::config_import::ConfigImportState::new(),
            integration: crate::pages::integration::IntegrationState::new(),
            stats_mgr: crate::pages::stats::StatsState::new(),
            memory: crate::pages::memory::MemoryDiag::new(),
            compat_mgr: crate::pages::compat::CompatState::new(),
            reload_confirm: signal(false),
            exit_confirm: signal(false),
            unsaved_note: signal(String::new()),
            unsaved_tip: signal(String::new()),
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
            theme_pkg_confirm_visible: signal(false),
            theme_pkg_confirm_msg: signal(String::new()),
            theme_pkg_confirm_path: signal(String::new()),
            theme_overwrite_source: signal(None),
            theme_overwrite_msg: signal(String::new()),
            theme_overwrite_visible: signal(false),
            font_sig: signal(vec![]),
            font_label_sig: signal(vec![]),
            theme_follow: signal(None),
            override_summary: signal(HashMap::new()),
            codetable_schema_val_sig: signal(vec![]),
            codetable_schema_lab_sig: signal(vec![]),
            pinyin_schema_val_sig: signal(vec![]),
            pinyin_schema_lab_sig: signal(vec![]),

            update_dialog_visible: signal(false),
            wizard_visible: signal(false),
            wizard_dont_show: signal(setting_load_wizard_dismissed),
            wizard_phase: signal(WizardPhase::Welcome),
            wizard: Rc::new(RefCell::new(None)),
            wizard_rev: signal(0),
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
    pub fn register_file_assoc(&mut self, idx: usize) {
        let a = &crate::file_assoc::ALL[idx];
        match crate::file_assoc::register_one(a) {
            Ok(()) => {
                crate::file_assoc::notify_shell_assoc_changed();
                self.set_file_assoc_self_heal(true);
                self.integration.refresh();
                self.notify_ok(format!("{} 文件关联已注册", a.ext));
            }
            Err(e) => self.notify_err(format!("文件关联注册失败：{e}")),
        }
    }

    /// 解除 `.wpkg` 关联。**必须**同时把 `file_assoc.self_heal` 置 `false` ——
    /// 否则下次启动自愈立刻把关联写回去（与协议取消注册同一条教训）。
    pub fn unregister_file_assoc(&mut self, idx: usize) {
        let a = &crate::file_assoc::ALL[idx];
        match crate::file_assoc::unregister_one(a) {
            Ok(()) => {
                crate::file_assoc::notify_shell_assoc_changed();
                // 自愈门控是**整体**的一个开关：解除任一关联都关掉它，否则下次启动
                // 自愈会把刚解除的那个写回去（与协议取消注册同一条教训）。
                self.set_file_assoc_self_heal(false);
                self.integration.refresh();
                self.notify_ok(format!("{} 文件关联已解除，启动时不再自动注册", a.ext));
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

    /// 持久化主题色来源。
    pub fn save_accent_mode(&mut self, mode: crate::accent::AccentMode) {
        self.save_ui_state(|ui| ui.accent = mode.to_config());
    }

    /// 持久化界面细节选项（`[ui.look]`）。
    pub fn save_look(&mut self) {
        let cfg = crate::look::LookConfig::from_indices(self.look.snapshot());
        self.save_ui_state(|ui| ui.look = cfg);
    }

    pub fn accent_mode(&self) -> crate::accent::AccentMode {
        crate::accent::AccentMode::from_index(self.accent_idx.get())
    }

    /// 跟随模式要取色的主题：外观页下拉此刻选中的（未保存也算，换主题即可预览），下拉还没
    /// 建好时取配置值；都没有时为空串，由 core 取当前主题。
    fn accent_target_theme(&self) -> String {
        self.selected_theme_id()
            .or_else(|| {
                self.current_config
                    .get_str("ui.theme.name")
                    .map(str::to_string)
            })
            .unwrap_or_default()
    }

    /// 此刻该用的强调色组；默认模式、没取到色、近灰都为 `None`（用设置窗默认蓝）。
    fn accent_set(&self) -> Option<crate::accent::AccentSet> {
        if self.accent_mode() != crate::accent::AccentMode::Ime {
            return None;
        }
        self.ime_accent
            .borrow()
            .as_ref()?
            .derive(self.dark.get())?
            .set()
    }

    /// 此刻该用的整窗主题（明暗 + 强调色）。建窗时用它，运行期经 [`Self::apply_ui_theme`]。
    pub fn ui_theme(&self) -> Theme {
        crate::theme_def::theme_with_accent(self.dark.get(), self.accent_set())
    }

    /// 把（明暗, 强调色）换上；与上次相同则不动。还没有换肤句柄（建窗前）时什么都不做。
    pub fn apply_ui_theme(&self) {
        let key = (self.dark.get(), self.accent_set());
        if self.applied_theme.get() == Some(key) {
            return;
        }
        if let Some(th) = self.theme_handle.borrow().as_ref() {
            th.set(crate::theme_def::theme_with_accent(key.0, key.1));
            self.applied_theme.set(Some(key));
        }
    }

    /// 主题色同步：跟随模式下按需取色、刷新角标提示，再换上主题。
    ///
    /// 模式切换、外观页换主题（含未保存）、明暗切换都经这里（见 `shell::accent_watcher`），
    /// 取色只在要跟随的主题变了时发生。
    pub fn sync_accent(&self) {
        use crate::accent::AccentMode;
        if self.accent_mode() == AccentMode::Ime {
            let target = self.accent_target_theme();
            let fresh = self.accent_fetched_for.borrow().as_deref() == Some(target.as_str())
                // 启动时按「当前主题」（空串）预取过，配置到达后目标变成了具体 id——
                // 取回的就是它，不必再取一次。
                || self
                    .ime_accent
                    .borrow()
                    .as_ref()
                    .is_some_and(|t| t.theme_id == target);
            if !fresh {
                *self.ime_accent.borrow_mut() = load_theme_accent(&target);
                *self.accent_fetched_for.borrow_mut() = Some(target);
            }
            let fetched = self.ime_accent.borrow();
            let id = fetched.as_ref().map(|t| t.theme_id.as_str()).unwrap_or("");
            let name = self
                .theme_entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.display.as_str())
                .unwrap_or(id);
            let note = crate::accent::follow_note(fetched.as_ref(), name, self.dark.get());
            if self.accent_note.with(|n| *n != note) {
                self.accent_note.set(note);
            }
        } else if self.accent_note.with(|n| !n.is_empty()) {
            self.accent_note.set(String::new());
        }
        self.apply_ui_theme();
    }

    /// 建窗前预取当前主题的强调色（仅跟随模式），让窗口一亮就是跟随色、不先闪一下默认蓝。
    ///
    /// 只等 [`ACCENT_PREFETCH_TIMEOUT`]：core 忙时宁可先用默认色，数据到达后
    /// [`Self::apply_loaded`] 会再同步一次。
    pub fn prefetch_accent(&self) {
        if self.accent_mode() != crate::accent::AccentMode::Ime {
            return;
        }
        *self.ime_accent.borrow_mut() = load_theme_accent_within("", ACCENT_PREFETCH_TIMEOUT);
        *self.accent_fetched_for.borrow_mut() = Some(String::new());
    }

    /// 让下一次 [`Self::sync_accent`] 重新取色：主题列表重扫（覆盖导入可能换了同名主题的
    /// 颜色）、上次没取到（输入法后来才起来）时用。
    pub fn invalidate_accent(&self) {
        *self.accent_fetched_for.borrow_mut() = None;
        *self.ime_accent.borrow_mut() = None;
    }

    /// 持久化方案页「隐藏未启用」。
    pub fn save_hide_disabled_schemas(&mut self, on: bool) {
        self.save_ui_state(|ui| ui.hide_disabled_schemas = on);
    }

    /// 持久化方案页「显示特殊方案」。
    pub fn save_show_hidden_schemas(&mut self, on: bool) {
        self.save_ui_state(|ui| ui.show_hidden_schemas = on);
    }

    /// 持久化「显示定制版隐藏的设置」。只落盘、不重建当前页面：清单折叠发生在页面树构建时，
    /// 重开设置窗口后由 [`Self::refold_manifest_if_needed`] 生效（开关旁的说明写明了这一点）。
    pub fn save_show_custom_hidden_settings(&mut self, on: bool) {
        self.save_ui_state(|ui| ui.show_custom_hidden_settings = on);
    }

    /// 清单按「显示定制版隐藏的设置」的当前值重折；与上次折叠时一致就什么都不做。
    ///
    /// 由 `shell::build` 在建树之前调用。只在 `AppState::empty()` 里折一次是不够的：
    /// 主窗可以关掉再开而进程不退（词库窗还开着），那时开关切了也不会生效。
    /// 搜索只换索引数据（[`crate::search::SearchState::reindex`]），不重建那批进程级 Signal。
    pub fn refold_manifest_if_needed(&mut self) {
        let show_all = self.setting_config.ui.show_custom_hidden_settings;
        if show_all == self.manifest_show_all {
            return;
        }
        self.manifest =
            crate::custom_hide::settings_manifest(crate::custom_hide::installed(), show_all);
        self.search.reindex(&self.manifest);
        self.schema_mgr.set_manifest(Rc::new(self.manifest.clone()));
        self.manifest_show_all = show_all;
    }

    /// 持久化设置向导「下次不再弹出」。
    pub fn save_wizard_dismissed(&mut self, on: bool) {
        // 单测不落盘：`wind_setting.toml` 的路径没有测试覆盖口，落盘就写进开发机的真实用户目录。
        if cfg!(test) {
            self.setting_config.ui.wizard_dismissed = on;
            return;
        }
        self.save_ui_state(|ui| ui.wizard_dismissed = on);
    }

    /// 持久化「打开设置时自动检查更新」开关。
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
            // 兼容规则被改了（设置端自己写的会收到回声；右键菜单写的只能靠它得知）。
            // 回声无害：`CompatState::reload` 只在内容真的变了才动信号，不会因此重建界面。
            "compat.changed" => self.compat_mgr.request_refresh(),
            // 方案配置被改了（命令直通开关扩展词库等）：开着的方案设置对话框跟着刷新。
            // 本端保存的回声在 `sync_dicts_from` 里按值相同滤掉，不提示。
            "schema.configChanged" => {
                let Some(id) = ev.data.get("id").and_then(|v| v.as_str()) else {
                    return;
                };
                let changed = self.schema_mgr.on_schema_config_changed(id);
                if !changed.is_empty() {
                    ctx.toast(format!(
                        "「{}」已在别处更改，页面已同步",
                        changed.join("」「")
                    ));
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
        // 外部送来的取值同样可能不在下拉列表里（托盘菜单切到一个设置页没扫到的主题）。
        self.ensure_unlisted_option_values(&cfg);
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
                self.update_size_text.set(Self::update_size_line(&r));
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
                    self.update_size_text.set(Self::update_size_line(&result));
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

    /// 后台下载主题链接，**不阻塞 UI**：点击立即返回，下载完成后在 UI 线程调 `on_text`。
    ///
    /// 对话框在下载期间被关掉（含 ESC）时结果直接丢弃——那是用户改了主意，不该再弹覆盖
    /// 确认或错误 toast。取消由各对话框挂的 `reactive_effect` 调 [`Self::cancel_theme_download`]。
    pub fn start_theme_download(
        app: &Rc<RefCell<AppState>>,
        owner: ThemeDlOwner,
        url: String,
        dialog_visible: Signal<bool>,
        on_text: impl FnOnce(&Rc<RefCell<AppState>>, String) + 'static,
    ) {
        let st = app.borrow();
        if !st.theme_download_idle.get() {
            return;
        }
        st.feedback
            .set(Some((ToastKind::Info, "正在下载主题…".to_string())));
        let net = st.net.clone();
        let app_done = app.clone();
        let task = net.spawn(
            move |cancel| crate::theme_import::download_https_text(&url, cancel),
            move |res| {
                {
                    let st = app_done.borrow();
                    st.theme_download_task.set(None);
                    st.theme_download_idle.set(true);
                }
                if !dialog_visible.get() {
                    return;
                }
                match res {
                    Some(Ok(text)) => on_text(&app_done, text),
                    Some(Err(e)) => app_done.borrow().notify_err(e.user_message()),
                    None => app_done.borrow().notify_err("下载线程异常中断"),
                }
            },
        );
        st.theme_download_task.set(Some((task, owner)));
        st.theme_download_idle.set(false);
    }

    /// 取消 `owner` 在途的主题下载（它的对话框关闭时调）。没有在途任务、或在途的是另一个
    /// 对话框发起的，都无副作用。
    pub fn cancel_theme_download(&self, owner: ThemeDlOwner) {
        match self.theme_download_task.get() {
            Some((id, o)) if o == owner => {
                self.theme_download_task.set(None);
                self.net.cancel(id);
            }
            _ => return,
        }
        self.theme_download_idle.set(true);
    }

    /// 收起协议主题导入框。写前先比：`Signal::set` 无条件升版本，而「本来就关着」时再写
    /// 一次，会让挂在它上面的 `reactive_effect` 以为刚被关掉。
    pub fn hide_protocol_theme_dialog(&self) {
        if self.theme_import_dialog_visible.get() {
            self.theme_import_dialog_visible.set(false);
        }
    }

    /// 取消在途的协议方案包下载（下载框关闭时调，含 ESC）。没有在途任务时无副作用。
    pub fn cancel_schema_download(&self) {
        if let Some(id) = self.schema_download_task.take() {
            self.net.cancel(id);
        }
        if self.schema_download_visible.get() {
            self.schema_download_visible.set(false);
        }
    }

    /// 后台下载协议链接带来的方案包并让 core 预览，完成后在 UI 线程弹导入确认框。
    ///
    /// 已有一个在下时拒绝新的——64 MB 的包叠着下既占带宽又让确认框次序错乱。
    pub fn start_schema_download(app: &Rc<RefCell<AppState>>, url: String) {
        let st = app.borrow();
        if st.schema_download_task.get().is_some() {
            if st.schema_download_visible.get() {
                st.notify_err("上一个方案包还在下载，请稍候再试");
                return;
            }
            // 下载框已被 ESC 收起、取消还在等下一帧的 effect：视为已取消，不拒绝新请求。
            st.cancel_schema_download();
        }
        // 非官方域在发请求之前就拒掉：不必起线程，反馈也是即时的。
        if let Err(e) = crate::schema_url_import::check_official_url(&url) {
            st.notify_err(e);
            return;
        }
        st.schema_download_visible.set(true);
        let net = st.net.clone();
        let app_done = app.clone();
        let task = net.spawn(
            move |cancel| crate::schema_url_import::download_and_preview(&url, cancel),
            move |res| {
                let st = app_done.borrow();
                st.schema_download_task.set(None);
                // 框已被收起（ESC）= 用户取消了：包随 drop 删除，不再弹确认框。
                if !st.schema_download_visible.get() {
                    return;
                }
                // 先收下载框再弹确认框：两个全窗模态同时可见会互相遮挡。
                st.schema_download_visible.set(false);
                match res {
                    Some(Ok((pkg, preview))) => {
                        st.schema_mgr.open_downloaded_import_confirm(pkg, &preview)
                    }
                    Some(Err(e)) => st.notify_err(e),
                    None => st.notify_err("下载线程异常中断"),
                }
            },
        );
        st.schema_download_task.set(Some(task));
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
        // 列表整份替换**之前**先记住此刻选中的是哪个主题：下拉的选中态是索引不是值，
        // 而 core 给的顺序是「内置在前 + 用户主题按 id 排序」，导入一个 id 靠前的主题、
        // 或删掉中间某个，都会让同一个主题换一个下标。不重定位的话，下拉显示的就不再是
        // 用户选的那个——导出会导错，而点一次保存会把那个错的值写进 ui.theme.name。
        let selected = self.selected_theme_id();

        self.theme_entries = load_theme_entries();
        self.apply_theme_entries();
        // 刚被删掉 / 这一轮没扫到的主题，如果正是配置里那个，它此刻就成了「列表未列举的
        // 取值」——不补，点一次保存就把 ui.theme.name 换成列表第一项。
        let cur = self
            .current_config
            .get_str("ui.theme.name")
            .unwrap_or_default()
            .to_string();
        self.push_unlisted_theme(&cur);
        // 补项可能又往列表末尾加了一条，故重定位放在它之后。
        self.relocate_theme_index(selected.as_deref());
        // 覆盖导入 / 删除当前主题都会经这里刷新列表，但跟随值（`theme.resolved`）此前
        // 不重拉：外观页的「跟随主题（值）」文案与勾选初值继续挂着旧主题的数，直到用户
        // 手动切一次主题或重开设置页才更新。
        self.refresh_theme_follow();
        // 同理：覆盖导入换了同名主题的颜色，强调色也要重取（列表信号已变，监视节点会接着同步）。
        self.invalidate_accent();
    }

    /// 主题下拉此刻选中的那个 id（拿不到索引信号或越界时为 `None`）。
    fn selected_theme_id(&self) -> Option<String> {
        let idx = self
            .fields
            .select_indices
            .borrow()
            .get("ui.theme.name")
            .map(|s| s.get())?;
        self.theme_value_sig.with(|v| v.get(idx).cloned())
    }

    /// 选项列表换过之后，把选中索引按**值**挪回原来那个主题。
    ///
    /// 找不到（那个主题刚被删掉）就不动索引：此时配置里的取值已由
    /// [`Self::push_unlisted_theme`] 补进列表末尾，接着由 loader 那条路定位，
    /// 这里再猜一个下标只会盖掉它。
    fn relocate_theme_index(&self, want: Option<&str>) {
        let sig = self
            .fields
            .select_indices
            .borrow()
            .get("ui.theme.name")
            .copied();
        if let Some(sig) = sig {
            relocate_index(self.theme_value_sig, sig, want);
        }
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

    /// 把配置里那些**选项列表没有列举**的当前取值补进四个响应式下拉的选项信号。
    ///
    /// 静态 select 由 [`crate::manifest::select_index_for`] 在本地 `Vec` 上补这一项；响应式
    /// select 的选项是共享信号，补项只能在填充处做（见
    /// [`crate::manifest::push_unlisted_option`] 的说明）。不补就是一次静默的配置丢失：
    /// 卸掉的第三方主题、系统里缺的字体、改过 id 的方案，都会在「打开设置页、点一次保存」
    /// 之间被换成下拉第一项。
    ///
    /// ⚠️ 必须排在 loaders **之前**：loader 也按 `position(...)` 在值列表里重定位索引，
    /// 选项还没补齐时它同样回落 0。
    fn ensure_unlisted_option_values(&self, cfg: &Config) {
        self.push_unlisted_theme(cfg.get_str("ui.theme.name").unwrap_or_default());
        crate::manifest::push_unlisted_option(
            self.font_sig,
            self.font_label_sig,
            cfg.get_str("ui.font.family").unwrap_or_default(),
        );
        crate::manifest::push_unlisted_option(
            self.codetable_schema_val_sig,
            self.codetable_schema_lab_sig,
            cfg.get_str("schema.primary_codetable").unwrap_or_default(),
        );
        crate::manifest::push_unlisted_option(
            self.pinyin_schema_val_sig,
            self.pinyin_schema_lab_sig,
            cfg.get_str("schema.primary_pinyin").unwrap_or_default(),
        );
    }

    /// 换上新的主题跟随值。外观页的跟随行经信号自己刷新文案与勾选初值。
    ///
    /// 主题字体不在系统字体列表里（主题写的是英文族名、列表是本地化名，或干脆没装）时
    /// 补进列表：否则勾选瞬间下拉按「找不到 ⇒ 首项」落到别的字体上，跟随文案说的是
    /// A、勾上却成了 B。标签就是族名本身——它不是「当前配置」，不套 `unlisted_label`。
    pub fn set_theme_follow(&self, tf: Option<crate::theme_follow::ThemeFollow>) {
        if let Some(fam) = tf.as_ref().and_then(|t| t.font_family.clone())
            && !self.font_sig.with(|v| v.contains(&fam))
        {
            self.font_sig.update(|v| v.push(fam.clone()));
            self.font_label_sig.update(|l| l.push(fam));
        }
        self.theme_follow.set(tf);
    }

    /// 重拉 `theme.resolved`。在保存了 `ui.theme.*`（换主题、明暗）之后调——跟随值是
    /// core 按**已生效**的配置解析的，保存前拉到的还是旧主题的值。
    pub fn refresh_theme_follow(&self) {
        self.set_theme_follow(load_theme_follow());
    }

    /// 重拉 `schema.overrideSummary`。在方案设置对话框保存（落 `schema_overrides/<id>.toml`）
    /// 之后调——覆盖汇总只在那一刻真的变过，全局页此前显示的角标文案才会与盘上不一致。
    pub fn refresh_override_summary(&self) {
        self.override_summary.set(load_override_summary());
    }

    /// 主题下拉比其余三个多一份富内容信号（`theme_items_sig`，带版本徽章与删除图标），
    /// 而索引↔取值的映射读的是 `theme_value_sig` —— 两者错位一项，下拉显示的就不是选中的
    /// 那个主题。故补项必须同时补进去，且不带徽章、不带删除图标（它不是一个装着的主题）。
    fn push_unlisted_theme(&self, cur: &str) {
        if !crate::manifest::push_unlisted_option(self.theme_value_sig, self.theme_label_sig, cur) {
            return;
        }
        let label = self
            .theme_label_sig
            .with(|l| l.last().cloned())
            .unwrap_or_default();
        self.theme_items_sig
            .update(|items| items.push(DropdownItem::new(label)));
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

    /// 导入撞上同名主题：暂存来源并弹覆盖确认框。
    ///
    /// 静态方法（收 Signal 句柄而非 `&self`）：几个导入入口都在闭包里触发，其中本地文件
    /// 导入的回调只拿得到 Signal，借不到 `AppState`——它的 `&mut` 借用在确认按钮那侧。
    pub fn ask_overwrite_theme(
        src_sig: Signal<Option<crate::theme_import::ThemeSource>>,
        msg_sig: Signal<String>,
        visible: Signal<bool>,
        source: crate::theme_import::ThemeSource,
        conflict_msg: &str,
    ) {
        msg_sig.set(format!(
            "{conflict_msg}。覆盖会用{}替换原主题，原内容不可恢复。确定覆盖？",
            source.describe()
        ));
        src_sig.set(Some(source));
        visible.set(true);
    }

    /// 主题包确认框点「导入」后的落点。
    ///
    /// 撞名会接着弹覆盖确认（`try_import_theme` 负责），于是用户看到的是两步：
    /// 「要装吗」→「已经有同名的，要覆盖吗」。
    pub fn confirm_theme_package_import(&mut self) {
        self.theme_pkg_confirm_visible.set(false);
        let path = self.theme_pkg_confirm_path.get();
        if path.is_empty() {
            return;
        }
        self.theme_pkg_confirm_path.set(String::new());
        let outcome = Self::try_import_theme(
            crate::theme_import::ThemeSource::Package(path),
            self.theme_overwrite_source,
            self.theme_overwrite_msg,
            self.theme_overwrite_visible,
        );
        match outcome {
            // 已转覆盖确认，等用户拍板。
            None => {}
            Some(Ok(r)) => {
                self.refresh_theme_names();
                self.notify_ok(format!("已导入主题：{}", r.display_name));
            }
            Some(Err(e)) => self.notify_err(e.user_message()),
        }
    }

    /// 几个导入入口的共同落点：试着导入，撞名则转成覆盖确认。
    ///
    /// 返回 `None` = 已转覆盖确认，这次既没成也没败，等用户拍板；`Some(..)` = 有结论，
    /// 由调用方按自己的反馈渠道去报（对话框用 toast、设置行用 feedback signal）。
    ///
    /// 抽出来是因为「撞名不是失败而是要不要更新」这条判断此前在两个入口各写了一遍，
    /// 包进来会是第三遍——而它恰恰是最容易写漏的那一条（早先有入口把冲突和读取失败
    /// 共用一条 toast，用户只看到「主题已存在」却没有任何前进的路）。
    pub fn try_import_theme(
        source: crate::theme_import::ThemeSource,
        src_sig: Signal<Option<crate::theme_import::ThemeSource>>,
        msg_sig: Signal<String>,
        visible: Signal<bool>,
    ) -> Option<Result<crate::theme_import::ThemeImportResult, crate::theme_import::ThemeImportError>>
    {
        match source.import(false) {
            Err(crate::theme_import::ThemeImportError::Conflict(msg)) => {
                Self::ask_overwrite_theme(src_sig, msg_sig, visible, source, &msg);
                None
            }
            other => Some(other),
        }
    }

    /// 执行覆盖导入（覆盖确认框「覆盖」按钮触发）：以 `force=true` 重推暂存的原文。
    ///
    /// slug 传 None，与首次导入那次尝试一致——core 会同样按 `meta.name` 推出目录 id，
    /// 也就是刚刚判定冲突的那个 id；显式传别的 slug 反而可能覆盖到另一个主题上。
    pub fn confirm_overwrite_theme(&mut self) {
        self.theme_overwrite_visible.set(false);
        let Some(source) = self.theme_overwrite_source.get() else {
            return;
        };
        match source.import(true) {
            Ok(result) => {
                self.refresh_theme_names();
                self.notify_ok(format!("已覆盖主题：{}", result.display_name));
            }
            Err(e) => self.notify_err(e.user_message()),
        }
        self.theme_overwrite_source.set(None);
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

    /// 请求开应用兼容窗（二次实例带 `--page compat`）。本回调拿不到 ctx，经通道让 UI 线程开。
    pub fn request_compat_window(&self) {
        if let Some(tx) = self.ui_tx.borrow().clone() {
            let _ = tx.send(crate::UiRequest::OpenCompatWindow);
        }
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
        // 「已被 N 个方案覆盖」（F9/R8.6）角标现算用的两份数据——先方案名后覆盖汇总，
        // 保证 `override_summary` 的 map 闭包读到的 `schema_names` 已经是新的（同一次
        // 加载内，两者一起变，不该有一帧新旧不一致）。
        self.override_summary.set(data.override_summary);
        self.theme_entries = data.theme_entries;
        self.fonts = data.fonts;
        self.apply_theme_entries();
        // value 与 label 同为字体族名。没有空值首项，见 `font_sig` 字段注释。
        self.font_sig.set(self.fonts.clone());
        self.font_label_sig.set(self.fonts.clone());
        // 必须在字体列表之后（主题字体要补进列表）、loaders 之前（跟随行按它播种）。
        self.set_theme_follow(data.theme_follow);
        // 每次加载都重取一次强调色并当场同步：启动预取时输入法可能还没起来，「重新加载」前
        // 主题编辑器可能改了当前主题的颜色。不能只指望监视节点——词库窗 / 兼容窗当主窗时
        // 不建设置外壳，根本没有监视节点。默认模式下 `sync_accent` 不碰 core。
        self.invalidate_accent();
        self.sync_accent();
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
        // 选项列表已就位，补上配置里那些列表没列举的取值（必须早于 loaders，见方法文档）。
        self.ensure_unlisted_option_values(&cfg);
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
        // `system.*` 的落地基线只能在此确立——这是「本次加载的值」唯一确定的时刻。
        // 放晚了（比如惰性到第一次 `is_dirty`）会把用户的改动当成基线，开关永不生效。
        // ⚠️ 必须排在 `capabilities` **之后**：键被写回剪枝掉时要靠它取默认值。
        // ★ 基线以**注册表实况**为准，不以 config.toml 为准：要回答的是「系统现在是
        // 什么样」，而配置并不知道那件事（升级重注册、别的输入法、手工改注册表都会让
        // 两者分家）。拿不到实况时 probe() 回 None，退回按配置播种的老行为。
        // 拉一次子进程（只读 HKLM、不提权），在本方法这一处、每次加载一次。
        let sys = crate::dota2_compat::probe();
        self.fields
            .seed_system_baseline(&self.base_config, &self.capabilities, sys.as_ref());
        self.system_info = data.system_info;
        // 降级顶栏。与下面的定制版徽章同理，页面树早已建好，只能靠 Signal 送进去。
        self.degradation.set(degradation_text(&data.degradation));
        // 关于页的定制版徽章：页面树早已建好，只能靠 Signal 送进去（见字段注释）。
        self.custom_edition
            .set(crate::pages::custom_edition_badge(&self.system_info).unwrap_or_default());
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

    /// 未保存项清单：非 config 落点的名字 + config.toml 的差异**键名**。
    ///
    /// 判脏与展示共用这一处：[`Self::is_dirty`]、[`Self::has_unsaved_changes`] 都是它
    /// 的"非空"判定，关窗确认框列出的也是它。三者各算各的，迟早分家成「说有未保存却
    /// 一条列不出」或反过来——而这个提示本就是用户报障的高发区。
    ///
    /// 只给**键名**不给值：值里可能有用户的短语/热键等私人内容（见日志隐私约定），
    /// 且排查要回答的是"哪个键在动"，值帮不上忙。
    ///
    /// 键名原样给出、**不翻译成界面名称**。查不到对应控件的键恰恰是最可疑的那一类
    /// （writer 写了一个界面上没有的键），翻译会把这条线索吃掉。
    ///
    /// 不带 `data_loaded` 门 —— 那是关窗守卫专有的，见 [`Self::has_unsaved_changes`]。
    fn unsaved_items_unguarded(&self) -> Vec<String> {
        // config.toml 之外的落点排在前面：它们是整块的（方案配置、词库开关），
        // 比单个配置键更能说明"我改过什么"。
        let mut out = self.fields.side_dirty_labels();
        let mut probe = self.current_config.clone();
        for w in self.fields.writers.borrow().iter() {
            w(&mut probe);
        }
        let items = diff_config(&self.base_config, &probe, &self.capabilities.map_keys());
        out.extend(items.into_iter().map(|i| i.key));
        if !out.is_empty() {
            // ★ warn 而非 debug：用户报「什么都没动就提示有未保存的更改」时，**默认**
            // 日志里就得有这一行。要他专门开 debug 级再复现一遍，排查链条太长——这个
            // 问题已经反复过很多次了。
            //
            // `untouched_settings_produce_no_diff` 只覆盖 mock 数据（出厂默认配置），
            // 而误报一律出在**升级上来的存量 config.toml**上：老顺序、历史遗留值、
            // core 已删的键。那个交集离线测试够不着，现场自述是唯一入口
            // （2026-08-27 现场就是这样）。
            tracing::warn!("未保存项({}): {:?}", out.len(), out);
        }
        out
    }

    /// 未保存项清单（关窗确认框展示用），数据未就位时为空。
    ///
    /// 与 [`Self::has_unsaved_changes`] 同一个门：两者在关窗路径上一前一后，判据不同
    /// 就会出现「拦下了，却说没有未保存项」。
    pub fn unsaved_items(&self) -> Vec<String> {
        // 数据没到位时一律视为无改动：此刻 `base_config` 还是空的，而 `AppState::empty`
        // 就带着四个内建 writer（方案 available、全局热键收集等），它们照样会写出
        // `keys` / `schema` 两个键——拿这个 diff 去拦关窗纯属误报，用户根本还没机会改
        // 任何东西。core 未响应时开了设置页又想关掉，正是最不该被拦的时候。
        if !self.data_loaded {
            return Vec::new();
        }
        self.unsaved_items_unguarded()
    }

    /// 填好关窗确认框的「未保存项」文案并置位可见。
    ///
    /// 与守卫合一（守卫把算好的清单传进来），清单只算一次：分开写的话，守卫先
    /// `has_unsaved_changes()`、对话框再 `unsaved_items()`，同一份 diff 要跑两遍
    /// 全部 writer，而这条路上还夹着 `SideCommitter` 那类有代价的判据。
    pub fn prime_exit_confirm(&self, items: &[String]) {
        self.unsaved_note
            .set(format!("共 {} 项未保存", items.len()));
        self.unsaved_tip.set(unsaved_tip_text(items));
        self.exit_confirm.set(true);
    }

    /// 是否存在未保存改动：跑写回器到 current 的副本上，与 base 比对有无差异。
    ///
    /// config.toml 之外的落点也算——否则改完方案配置点「恢复本页」，确认框不会
    /// 提醒用户即将丢弃它们。
    pub fn is_dirty(&self) -> bool {
        !self.unsaved_items_unguarded().is_empty()
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
    /// 自定义页（词库/关于）无清单分组，返回空切片。
    ///
    /// ⚠ **按页 id 查，不许写数字**。这里曾经是 `match self.tab.get() { 0 => .., 5 => .. }`
    /// 配一句「方案0/输入1/…/高级5」的注释，而那份顺序在两轮改动后早已作废：`5` 变成了
    /// 词库那个永不可达的占位页，高级页落到 `_ => &[]`。症状是**在高级页点「恢复本页」
    /// 一项都不恢复**，只弹一句「本页无可恢复的默认项」——不报错、没堆栈，纯静默失效。
    /// （更早还有反向的一版：在统计页点「恢复本页」会把整个 advanced 分组重置掉。）
    ///
    /// 全仓别处要 Tab 下标一律走 `cli::page_to_index`，这里是最后一处漏网的字面量。
    fn current_tab_groups(&self) -> &'static [&'static str] {
        let tab = self.tab.get();
        let is = |id: &str| crate::cli::page_to_index(id) == Some(tab);
        if is("schema") {
            &["schema"]
        } else if is("input") {
            // general 组没有自己的页，跟着输入页渲染（见 `pages::input_page`）。
            &["input", "general"]
        } else if is("keys") {
            &["keys"]
        } else if is("ui") {
            &["ui"]
        } else if is("stats") {
            &["stats"]
        } else if is("advanced") {
            &["advanced"]
        } else {
            &[]
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
        // 服务端这份配置里的取值未必都在当前选项列表里（主题在别处被删、配置被别的工具
        // 改过），补项同样要早于 loaders。
        self.ensure_unlisted_option_values(&cfg);
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
    ///
    /// 顺带停掉诊断信息对话框的自动刷新：词库 / 兼容窗当首窗时，诊断定时器挂在那个窗口上，
    /// 主窗关了它还在跑（见 `pages::memory`）。
    pub fn mark_main_window_closed(&self) {
        self.main_window_closed.set(true);
        self.memory.visible.set(false);
        self.memory.auto.set(false);
    }

    /// 是否有未保存的改动（关窗确认用）。
    ///
    /// 两个来源与 [`Self::save`] 一一对应：config.toml 之外的落点（方案 override、词库
    /// 开关），以及 config.toml 的增量 diff。
    ///
    /// diff 跑在**副本**上：`save` 是把 writers 直接写进 `self.current_config` 的——那是
    /// 提交路径的一部分。只为问一句"要不要保存"就改掉当前配置，会让紧接着的"直接退出"
    /// 变成已经改了一半；`live_conflicts` 出于同样的理由也用副本。
    ///
    /// 具体是哪些项由 [`Self::unsaved_items`] 给出（关窗确认框列的就是它）。本方法
    /// 即它的"非空"判定，两者不会分家。
    pub fn has_unsaved_changes(&self) -> bool {
        !self.unsaved_items().is_empty()
    }

    /// 「发现新版本」对话框里那一行体积说明。Linux 不自动安装（装 .deb 要 root），
    /// 这一行顺带告诉用户怎么装。
    fn update_size_line(r: &crate::update::CheckResult) -> String {
        if crate::update::self_update_supported() {
            format!("更新包 {}", r.size_display())
        } else {
            format!(
                "安装包 {}：在下载页下载对应架构的 .deb，用 sudo apt install 覆盖安装",
                r.size_display()
            )
        }
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
        // 方案设置对话框保存落的是 `schema_overrides/<id>.toml`，走的正是上面这个
        // SideCommitter——覆盖汇总只在这一刻才可能变，重拉一次让全局页角标跟上。
        if side_changed {
            self.refresh_override_summary();
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
        // 诊断：「没有变更」的最终判据就在这里。配合上面两条日志可定位到具体环节：
        // 内存表变了、writer 也写了，diff 却为空 ⇒ base_config 已经含有新值
        // （基线在保存前被刷新过），那是另一类 bug。
        tracing::debug!(
            "save: diff {} 项 {:?} (side_changed={side_changed})",
            items.len(),
            items.iter().map(|it| it.key.as_str()).collect::<Vec<_>>()
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
                    if touches_theme(&items) {
                        self.refresh_theme_follow();
                    }
                    // 启用/停用方案落在 config.toml 的 `schema.available`，不走 side 落点，
                    // 上面 `side_changed` 那一枪不会打；而覆盖汇总只列已启用方案，
                    // 不重拉的话新启用方案的覆盖角标要等重开设置才出现。
                    if touches_schema_available(&items) {
                        self.refresh_override_summary();
                    }
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

/// 这次保存有没有动到主题选择（`ui.theme.*`：主题名、明暗模式）。动了就得重拉跟随值。
fn touches_theme(items: &[crate::config::ConfigItem]) -> bool {
    items
        .iter()
        .any(|it| it.key == "ui.theme" || it.key.starts_with("ui.theme."))
}

/// 这次保存有没有动到方案启用表（`schema.available`）。动了就得重拉覆盖汇总。
///
/// 也认整段 `schema` 项：基线里没有 `schema` 表时（全新配置），`diff_config` 不下钻，
/// 把整个对象作为一项产出，`available` 就藏在它的 value 里。
fn touches_schema_available(items: &[crate::config::ConfigItem]) -> bool {
    items.iter().any(|it| {
        it.key == "schema.available" || (it.key == "schema" && it.value.get("available").is_some())
    })
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

/// 设置向导用：目标配置在触发键域**新增**的 Blocking 冲突摘要。
///
/// 基线自带的冲突与向导无关（用户早就配成那样了），算进来会让向导「下一步」永远点不动，
/// 故按冲突记录与基线做差。判定与设置页常驻警告条同源（[`check_domain`] 的两半）。
pub(crate) fn new_trigger_conflicts(base: &Config, target: &Config) -> Option<String> {
    let old = crate::key_conflict::find_key_conflicts(&collect_trigger_key_groups(base));
    let fresh: Vec<Conflict> =
        crate::key_conflict::find_key_conflicts(&collect_trigger_key_groups(target))
            .into_iter()
            .filter(|c| c.level == ConflictLevel::Blocking && !old.contains(c))
            .collect();
    crate::key_conflict::conflict_summary(&fresh, "触发键")
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
    // 字段表提到了 `key_conflict`：全局按键编辑器的只读组要认同一批字段，两处各抄一份
    // 的话，加一个热键字段只改一处的表现是「冲突检测报得出、列表里查无此项」。
    use crate::key_conflict::{HOTKEY_FIELDS, MODE_HOTKEY_FIELDS, expand_hotkey};

    let mut groups: Vec<KeyGroup> = HOTKEY_FIELDS
        .iter()
        .map(|(key, label)| {
            KeyGroup::new(key, *label, expand_hotkey(cfg.get_str(key).unwrap_or("")))
        })
        .collect();

    // 方案直达热键**不再单独收集**：`keys.schema_hotkeys` 已并入 key_actions
    // （动词 `switch_schema:<方案id>`），由下面那段统一纳入检测。core 折算后该表恒为空，
    // 留着这段只会是永不执行的死代码。
    for (key, label) in MODE_HOTKEY_FIELDS {
        groups.push(KeyGroup::new(
            key,
            *label,
            expand_hotkey(cfg.get_str(key).unwrap_or("")),
        ));
    }

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
    // 命令先判：表达式可能很长，原样回显会把报告那一行撑爆；截断口径与按键编辑器同源。
    if let Some(label) = crate::dialogs::key_binding::command_verb_label(verb) {
        return label;
    }
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
        "rare_char" => "生僻字".to_string(),
        "reverse" => "反查模式".to_string(),
        other => other.to_string(),
    }
}

/// 后台线程从 core 拉取的全部数据（所有字段均 `Send`，可跨线程传递）。
pub struct LoadedState {
    pub config_value: Value,
    pub system_info: Value,
    /// `config.degradation` 的原始回包 `{degraded, sections, totalFallback}`。
    ///
    /// ⚠️ 它**现读盘现算**（core 侧每次调用跑一次 `Config::load`），语义是「此刻盘上的
    /// 配置试加载一遍会不会降级」，不是「正在跑的那份降没降级」。所以用户把配置改好后
    /// 点「重新加载」，横幅会自己消失——这正是选它的理由。
    pub degradation: Value,
    pub capabilities_value: Value,
    pub schemas: Vec<SchemaInfo>,
    pub theme_entries: Vec<ThemeEntry>,
    pub fonts: Vec<String>,
    /// `theme.resolved`；取不到（老 core / 连接失败 / 主题全坏）为 `None`。
    pub theme_follow: Option<crate::theme_follow::ThemeFollow>,
    /// `schema.overrideSummary`；取不到时为空表（见 [`load_override_summary`]）。
    pub override_summary: HashMap<String, Vec<String>>,
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

        // 8 路并行：函数项自动满足 Send + 'static，直接传递无需 move 闭包。
        let h_config = std::thread::spawn(|| rpc::call_core("config.get", &serde_json::json!({})));
        let h_info = std::thread::spawn(|| rpc::call_core("system.info", &serde_json::json!({})));
        // 并行拉取 ⇒ 零额外往返（wall-clock 仍是 max(各调用延迟)）。它在 core 侧要读盘，
        // 故不并进 system.info——那个方法当前不碰磁盘，见 dispatch.rs 里的理由。
        let h_degr =
            std::thread::spawn(|| rpc::call_core("config.degradation", &serde_json::json!({})));
        let h_caps =
            std::thread::spawn(|| rpc::call_core("system.capabilities", &serde_json::json!({})));
        let h_schemas = std::thread::spawn(load_schemas);
        let h_themes = std::thread::spawn(load_theme_entries);
        let h_fonts = std::thread::spawn(load_fonts);
        let h_theme_follow = std::thread::spawn(load_theme_follow);
        let h_override_summary = std::thread::spawn(load_override_summary);

        let config_res = h_config.join().ok().and_then(|r| r.ok());
        let system_info = h_info
            .join()
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_else(Self::empty_obj);
        // 取不到（老 core / 连接失败）⇒ 空对象 ⇒ 折算出空文案 ⇒ 横幅不显示。
        // 这是对的：拿不到答案时不该吓唬用户说他的配置坏了。
        let degradation = h_degr
            .join()
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_else(Self::empty_obj);
        let caps_res = h_caps.join().ok().and_then(|r| r.ok());
        let schemas = h_schemas.join().unwrap_or_default();
        let theme_entries = h_themes.join().unwrap_or_default();
        let fonts = h_fonts.join().unwrap_or_default();
        let theme_follow = h_theme_follow.join().ok().flatten();
        let override_summary = h_override_summary.join().unwrap_or_default();

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
            degradation,
            capabilities_value,
            schemas,
            theme_entries,
            fonts,
            theme_follow,
            override_summary,
            connected,
        }
    }
}

/// 建窗前预取强调色最多等多久（见 [`AppState::prefetch_accent`]）。本机 RPC 正常几毫秒。
const ACCENT_PREFETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(400);

/// 拉主题强调色（`theme.accent`）；`name` 为空串取当前主题。RPC 失败与回包 `null` 同样视为
/// 拿不到。
fn load_theme_accent(name: &str) -> Option<crate::accent::ThemeAccent> {
    load_theme_accent_within(name, std::time::Duration::from_secs(5))
}

fn load_theme_accent_within(
    name: &str,
    timeout: std::time::Duration,
) -> Option<crate::accent::ThemeAccent> {
    let params = if name.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::json!({ "name": name })
    };
    rpc::call_core_within("theme.accent", &params, timeout)
        .ok()
        .and_then(|v| crate::accent::ThemeAccent::from_value(&v))
}

/// 拉当前主题的跟随值（`theme.resolved`）。RPC 失败与回包 `null` 同样视为拿不到。
fn load_theme_follow() -> Option<crate::theme_follow::ThemeFollow> {
    rpc::call_core("theme.resolved", &serde_json::json!({}))
        .ok()
        .and_then(|v| crate::theme_follow::ThemeFollow::from_value(&v))
}

/// 拉「已被 N 个方案覆盖」汇总（`schema.overrideSummary`）：`配置键 → 覆盖它的方案 id 列表`。
/// RPC 失败或回包不是对象（管道未连接/老 core）同样视为「拿不到」，退回空表——
/// 角标退化成静态说明，不是报错也不是显示假数据。
fn load_override_summary() -> HashMap<String, Vec<String>> {
    let Ok(v) = rpc::call_core("schema.overrideSummary", &serde_json::json!({})) else {
        return HashMap::new();
    };
    parse_override_summary(&v)
}

/// [`load_override_summary`] 的纯解析部分：抽出来是为了不靠 RPC/mock 就能测「回包不是
/// 对象」（`null`、老 core 给了别的形状）与「值不是字符串数组」这两条退化路径——
/// 拉不到（`Err`）时 [`load_override_summary`] 也是喂一个等价形状（`Value::Null`）
/// 进来，两条路共用同一份「非对象 ⇒ 空表」逻辑，不会出现「拉不到」与「拉到空」两种
/// 退化各写一遍、迟早漂移的情况。
fn parse_override_summary(v: &Value) -> HashMap<String, Vec<String>> {
    let Some(obj) = v.as_object() else {
        return HashMap::new();
    };
    obj.iter()
        .map(|(k, v)| {
            let ids = v
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            (k.clone(), ids)
        })
        .collect()
}

/// 把 `config.degradation` 的回包折成顶栏文案；未降级（或取不到）返回空串。
///
/// 段名**原样列出点分路径**（`keys.key_actions` 而不是「按键设置」）：用户拿着这一行
/// 要去配置文件里找那一段，译成界面用语反而对不上。降级粒度能细到子表，那份精度是
/// core 侧刻意保留的（见 `config.degradation` 的注释），这里不该再截成顶层段名。
pub(crate) fn degradation_text(v: &Value) -> String {
    if !v.get("degraded").and_then(Value::as_bool).unwrap_or(false) {
        return String::new();
    }
    // ★ 语法故障**优先**于段级降级来讲，两条都成立时只讲这一条。
    //
    // 它们是两类不同的故障，修法也不同：段级降级是「某个键的类型不对」，去改那个键；
    // 语法故障是「文件根本读不完」，去改那一行。混在一起讲会把用户支使到错误的地方，
    // 而且语法故障更靠前——文件读不完时，后面所有判断都建立在一份残缺内容上。
    if let Some(u) = v
        .get("unparsable")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    {
        let path = u.get("path").and_then(Value::as_str).unwrap_or("配置文件");
        if u.get("notUtf8").and_then(Value::as_bool).unwrap_or(false) {
            return format!(
                "⚠ 配置文件不是 UTF-8 编码，本次未能加载，你的设置都没有生效：{path}\n\
                 用记事本等编辑器另存为「UTF-8」后点右下角「重新加载」。原文件不会被覆盖。"
            );
        }
        let all: Vec<u64> = u
            .get("skippedLines")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        // ★ 紧凑形态，阈值与 core 的 `UnparsableLayer::lines_phrase` 保持一致（6）。
        // 一份完全不是 TOML 的文件会攒到 32 个行号，全列进横幅会把版面撑爆，
        // 而那种情形下具体哪几行本来就没意义（整份都没加载）。
        // 量词「第…行」跟着内容走：留给模板拼的话，折叠形态会拼出「等 32 处 行」的语病。
        const LINES_LISTED_MAX: usize = 6;
        let listed: Vec<String> = all
            .iter()
            .take(LINES_LISTED_MAX)
            .map(|n| n.to_string())
            .collect();
        let lines_phrase = if all.is_empty() {
            String::new()
        } else if all.len() > LINES_LISTED_MAX {
            format!("第 {} 行等 {} 处", listed.join("、"), all.len())
        } else {
            format!("第 {} 行", listed.join("、"))
        };
        // ★ 判据是 `salvagedKeys > 0`（真的救回了内容），**不是** 行号非空。
        // 「跳过了几行」与「救回来了」是两件事：core 啃到上限仍失败时两者同时成立，
        // 那时说「其余设置已正常加载」是在撒谎——一个字都没加载。
        let salvaged = u.get("salvagedKeys").and_then(Value::as_u64).unwrap_or(0) > 0;
        // 行号是用户唯一能直接下手的线索——「配置有语法错误」他无从找起，
        // 「第 6 行」可以直接跳过去。
        return if !salvaged {
            let tried = if all.is_empty() {
                String::new()
            } else {
                format!("（已尝试跳过{lines_phrase}仍无法解析）")
            };
            format!(
                "⚠ 配置文件语法错误，本次未能加载，你的设置都没有生效：{path}{tried}\n\
                 修好语法后点右下角「重新加载」。原文件不会被覆盖。"
            )
        } else {
            format!(
                "⚠ 配置文件{lines_phrase}语法错误，相应设置本次没有生效：{path}\n\
                 其余设置已正常加载。修好后点右下角「重新加载」。原文件不会被覆盖。"
            )
        };
    }
    if v.get("totalFallback")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return "⚠ 配置文件整体解析失败，本次全部回落出厂默认值——你的设置本次都没有生效。\
                请检查配置文件的语法，修好后点右下角「重新加载」。"
            .into();
    }
    let sections: Vec<&str> = v
        .get("sections")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if sections.is_empty() {
        // degraded 为真却没有段名：core 的两个字段本该互斥，走到这里说明契约变了。
        // 仍要出提示——「有问题但说不清哪一段」也比一声不吭强。
        return "⚠ 部分配置本次未生效，已回落出厂默认值。详见日志。".into();
    }
    format!(
        "⚠ 配置的 {} 段解析失败，已整段回落出厂默认值——你在这一段的设置本次没有生效。\
         修好配置文件后点右下角「重新加载」。",
        sections.join(" / ")
    )
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

/// 选项列表换过之后，把选中索引按**值**挪回原来那一项。
///
/// 抽成自由函数是为了测得到：这条判据错了的现象是「下拉显示的不是我选的那个」，只有
/// 真人点开设置页才看得见，而它要的全部输入就是两个信号——不必为此造一整个 `AppState`。
///
/// `want` 为 `None`（列表此前未就位）或找不到（那一项刚被删掉）时**不动索引**：后者的
/// 取值已由 [`AppState::push_unlisted_theme`] 补进列表末尾、再由 loader 那条路定位，
/// 这里再猜一个下标只会盖掉它。
fn relocate_index(values: Signal<Vec<String>>, index: Signal<usize>, want: Option<&str>) {
    let Some(want) = want else { return };
    if let Some(i) = values.with(|v| v.iter().position(|x| x == want)) {
        index.set(i);
    }
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

    /// 冲突报告里生僻字动词要显示人话，而不是原样回显 `rare_char`（GH#146 同批）。
    #[test]
    fn key_action_label_knows_rare_char() {
        assert_eq!(key_action_label("rare_char"), "生僻字");
        assert_eq!(key_action_label("reverse"), "反查模式");
        assert_eq!(
            key_action_label("bogus_verb"),
            "bogus_verb",
            "未知动词原样回显"
        );
    }

    /// 命令直通在冲突报告里显示「运行命令：…」，不原样回显一整条表达式。
    #[test]
    fn key_action_label_names_commands() {
        assert_eq!(
            key_action_label("command:proc.run(\"charmap.exe\")"),
            "运行命令：proc.run(\"charmap.exe\")"
        );
        let long = format!("command:open(\"https://example.com/{}\")", "a".repeat(80));
        assert!(key_action_label(&long).ends_with('…'), "过长应截断");
    }

    /// 「恢复本页」的分组映射必须覆盖每一个**有清单分组的**页，且不漏不错。
    ///
    /// 这条测试是补上一次真实失效的：`current_tab_groups` 曾按数字 match Tab 下标，
    /// `cli::PAGES` 一改顺序，高级页就静默落到 `_ => &[]`——点「恢复本页」一项都不恢复，
    /// 而这件事没有任何报错、也没有任何测试会红。现在改成按页 id 查，并由本测试钉住
    /// 「每个页 id 都解析得出下标、且该页拿到的正是它自己的分组」。
    #[test]
    fn reset_page_groups_follow_page_ids() {
        let expect: &[(&str, &[&str])] = &[
            ("schema", &["schema"]),
            ("input", &["input", "general"]),
            ("keys", &["keys"]),
            ("ui", &["ui"]),
            ("stats", &["stats"]),
            ("advanced", &["advanced"]),
            // 自定义页没有清单分组。
            ("dict", &[]),
            ("about", &[]),
        ];
        let st = AppState::empty();
        for (page, groups) in expect {
            let idx = crate::cli::page_to_index(page).expect("页 id 应在 PAGES 里");
            st.tab.set(idx);
            assert_eq!(
                st.current_tab_groups(),
                *groups,
                "页 {page}（下标 {idx}）的恢复分组不对"
            );
        }
    }

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
                label: "测试落点",
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

    /// `save()` 提交了 side 落点（方案设置对话框保存那一类）之后要重拉一次
    /// `schema.overrideSummary`——否则全局页的角标还在显示保存前的覆盖状态（F9/R8.6）。
    ///
    /// 用 [`probe`] 造一个「脏且提交成功」的 SideCommitter 模拟方案设置对话框保存，
    /// 用 `rpc::take_mock_calls()` 断言这次 `save()` 里确实发出过该调用——不断言调用
    /// 的先后位置/次数，只断言「发过」，避免和 `commit_side()` 内建落点（方案管理
    /// `refresh()`）等无关调用的次序耦合。
    #[test]
    fn save_refreshes_override_summary_when_side_committer_ran() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        st.data_loaded = true;
        let (side, _n) = probe(true, Ok(()));
        st.fields.side.borrow_mut().push(side);
        crate::rpc::take_mock_calls(); // 清空 empty() 期间可能已有的调用记录。

        assert!(st.save(), "side 提交成功应保存成功");
        let calls = crate::rpc::take_mock_calls();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert!(
            calls.iter().any(|(m, _)| m == "schema.overrideSummary"),
            "保存了 side 改动却没有重拉 overrideSummary，实际调用: {calls:?}"
        );
    }

    /// 反例：没有任何 side 改动、config.toml 这边也没动 `schema.available` 时不应重拉——
    /// 覆盖汇总只在方案设置对话框保存、或启用表变了时才会变，其余 config.toml 保存
    /// 不该多打这一枪。
    ///
    /// 基线取 writer 跑过之后的配置，再只改一个无关键：空基线会让 `diff_config` 把整段
    /// `schema` 表（含 `available`）当新增项产出，那就不是「没动启用表」了。
    #[test]
    fn save_does_not_refresh_override_summary_without_side_changes() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        st.data_loaded = true;
        let mut settled = st.current_config.clone();
        for w in st.fields.writers.borrow().iter() {
            w(&mut settled);
        }
        st.base_config = settled.clone();
        settled.set_path("ui.candidate.font_size", serde_json::json!(21));
        st.current_config = settled;
        crate::rpc::take_mock_calls();

        assert!(st.save(), "无 side 改动也应正常保存成功");
        let calls = crate::rpc::take_mock_calls();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert!(
            calls.iter().any(|(m, _)| m == "config.setItems"),
            "前提：这次保存确实写了 config.toml（无关键），实际调用: {calls:?}"
        );
        assert!(
            !calls.iter().any(|(m, _)| m == "schema.overrideSummary"),
            "没有 side 改动却重拉了 overrideSummary，实际调用: {calls:?}"
        );
    }

    /// 全局页启用/停用方案只改 config.toml 的 `schema.available`（不经 side 落点），
    /// 保存成功后也要重拉 `schema.overrideSummary`——汇总只列已启用方案，不重拉的话
    /// 新启用方案的覆盖角标要等重开设置才出现（最终审查 Important 3）。
    #[test]
    fn save_refreshes_override_summary_when_schema_available_changed() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        st.data_loaded = true;
        // 基线 = writer 跑过之后的配置（否则别的键也会进 diff），再把启用表改成「只有
        // 五笔」——等价于加载后用户在全局页多启用了一个方案（`enabled_order` 那份）。
        let mut settled = st.current_config.clone();
        for w in st.fields.writers.borrow().iter() {
            w(&mut settled);
        }
        settled.set_path("schema.available", serde_json::json!(["wubi86"]));
        st.base_config = settled;
        crate::rpc::take_mock_calls(); // 清空 empty() 期间可能已有的调用记录。

        assert!(st.save(), "改启用表应保存成功");
        let calls = crate::rpc::take_mock_calls();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert!(
            calls.iter().any(|(m, p)| m == "config.setItems"
                && p["items"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|it| it["key"] == "schema.available"))),
            "前提：这次保存确实写了 schema.available，实际调用: {calls:?}"
        );
        assert!(
            calls.iter().any(|(m, _)| m == "schema.overrideSummary"),
            "保存了 schema.available 却没有重拉 overrideSummary，实际调用: {calls:?}"
        );
    }

    /// 只有 `schema.available` 触发重拉；别的 config.toml 键不该多打这一枪。
    #[test]
    fn only_schema_available_triggers_override_summary_refresh() {
        let item = |k: &str| crate::config::ConfigItem {
            key: k.to_string(),
            value: Value::Null,
        };
        assert!(touches_schema_available(&[
            item("ui.candidate.font_size"),
            item("schema.available")
        ]));
        assert!(!touches_schema_available(&[item("schema.active")]));
        // 基线没有 `schema` 表时整段产出，`available` 在 value 里。
        assert!(touches_schema_available(&[crate::config::ConfigItem {
            key: "schema".to_string(),
            value: serde_json::json!({ "available": ["wubi86"] }),
        }]));
        assert!(!touches_schema_available(&[crate::config::ConfigItem {
            key: "schema".to_string(),
            value: serde_json::json!({ "active": "wubi86" }),
        }]));
        assert!(!touches_schema_available(&[item("ui.candidate.font_size")]));
    }

    /// [`parse_override_summary`] 的退化路径：回包不是对象（`null`、老 core 给了别的
    /// 形状）时退回空表，不是报错也不是显示假数据——[`load_override_summary`] 拉不到
    /// 时也是喂一个等价形状（`Value::Null`）进来，这条断言同时覆盖了那条退化路径。
    #[test]
    fn parse_override_summary_treats_non_object_reply_as_empty() {
        assert!(parse_override_summary(&Value::Null).is_empty());
        assert!(parse_override_summary(&serde_json::json!([1, 2, 3])).is_empty());
        assert!(parse_override_summary(&serde_json::json!("not an object")).is_empty());
    }

    /// 正常形状：值不是字符串数组的键退回空列表（而不是整表报废），数组里混进非字符串
    /// 元素时逐个跳过——防的是 core 契约漂移时界面直接 panic 或整页数据消失。
    #[test]
    fn parse_override_summary_reads_ids_and_tolerates_malformed_values() {
        let v = serde_json::json!({
            "ui.candidate.layout": ["wubi86", "pinyin", 1],
            "input.some.key": "not-an-array",
        });
        let got = parse_override_summary(&v);
        assert_eq!(
            got.get("ui.candidate.layout"),
            Some(&vec!["wubi86".to_string(), "pinyin".to_string()]),
            "非字符串元素应被跳过，不是整条报废"
        );
        assert_eq!(
            got.get("input.some.key"),
            Some(&vec![]),
            "值不是数组时退回空列表，键仍保留"
        );
    }

    /// 拉不到（管道未连/老 core）时退回空表——不 mock、真去连一个不存在的管道，
    /// 直接验证 [`load_override_summary`] 的 `Err` 分支，同 `non_mock_disconnect_sets_conn_failed`
    /// 的连接失败路径。
    #[test]
    fn load_override_summary_returns_empty_map_when_core_unreachable() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        let got = load_override_summary();
        assert!(got.is_empty(), "core 连不上时应退回空表：{got:?}");
    }

    /// 只列**脏的**落点。干净的一并列出来的话，用户会以为自己改了从没碰过的东西。
    #[test]
    fn side_dirty_labels_lists_only_the_dirty_ones() {
        let reg = FieldRegistry::default();
        reg.side.borrow_mut().push(SideCommitter {
            label: "没动过的",
            is_dirty: Box::new(|| false),
            commit: Box::new(|| Ok(())),
        });
        reg.side.borrow_mut().push(SideCommitter {
            label: "改过的",
            is_dirty: Box::new(|| true),
            commit: Box::new(|| Ok(())),
        });
        assert_eq!(reg.side_dirty_labels(), vec!["改过的".to_string()]);
    }

    /// 悬停清单的两条边界：恰好 `TIP_MAX_ITEMS` 项不截断，多一项就截断。
    ///
    /// 差一错在这里不会报错，只会让用户看到「…… 等 8 项」而上面正好列了 8 条，
    /// 或者反过来把最后一项悄悄吃掉。
    #[test]
    fn unsaved_tip_text_truncates_past_the_cap() {
        let items = |n: usize| -> Vec<String> { (0..n).map(|i| format!("k{i}")).collect() };

        let full = unsaved_tip_text(&items(TIP_MAX_ITEMS));
        assert!(
            !full.contains("等"),
            "恰好 {TIP_MAX_ITEMS} 项不该截断：{full}"
        );
        assert_eq!(
            full.lines().count(),
            TIP_MAX_ITEMS + 1,
            "每项一行 + 末尾那句「点击复制全部」"
        );

        let cut = unsaved_tip_text(&items(TIP_MAX_ITEMS + 1));
        assert!(
            cut.contains(&format!("等 {} 项", TIP_MAX_ITEMS + 1)),
            "「等 N 项」报的是**总数**，不是没列出来的那几条：{cut}"
        );
        assert_eq!(
            cut.lines().count(),
            TIP_MAX_ITEMS + 2,
            "前 N 行 + 省略行 + 提示行"
        );
    }

    /// 一项都没有时不该拼出个只剩提示语的空壳——那种提示弹出来纯属噪音。
    #[test]
    fn unsaved_tip_text_of_nothing_is_just_the_hint() {
        // 调用方（`prime_exit_confirm`）只在清单非空时才走到这里，故这里只钉住
        // 「不会崩、也不会凭空造出条目」这一条。
        let t = unsaved_tip_text(&[]);
        assert!(!t.contains("等"), "空清单不该出现省略行：{t}");
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

    /// ⛔ 回归：「Valve 游戏兼容模式」拧了却什么都没发生。
    ///
    /// 旧实现把基线惰性播种在 `is_dirty` 里，而 `AppState::save` 的顺序是
    /// `side_dirty()` 先、`commit_side()` 后——保存动作自己把**用户刚改的新值**当成了
    /// 基线，commit 随即判「没变」跳过落地。加上 `is_dirty` 在真实 UI 路径上只有关窗
    /// 确认与「恢复本页」两个调用点（正常的「拧开关 → 点应用」全程不经过），于是
    /// 症状是：config.toml 写了 `dota2_compat = true`、注册表纹丝不动、UAC 也不弹。
    #[test]
    fn dota2_compat_toggle_is_dirty_after_load() {
        let reg = Rc::new(FieldRegistry::default());
        let val = Rc::new(std::cell::Cell::new(false));
        {
            let v = val.clone();
            reg.getters.borrow_mut().insert(
                "system.dota2_compat".into(),
                Box::new(move || Value::Bool(v.get())),
            );
        }
        // 加载态 = 关闭。播种走的正是 `apply_loaded` 那条路。
        let cfg = Config::from_value(serde_json::json!({"system": {"dota2_compat": false}}));
        // 传 None = 拿不到注册表实况，走「按配置播种」那条回退路径 —— 本条测的正是它。
        reg.seed_system_baseline(&cfg, &Capabilities::default(), None);
        register_system_registrations(&reg);

        assert!(!reg.side_dirty(), "刚加载完、没动过，不该判脏");

        // 用户在「高级」页拧开开关。此前没有任何东西问过脏——这正是真实路径。
        val.set(true);
        assert!(
            reg.side_dirty(),
            "拧开关后必须判脏，否则 commit 认为无事发生、apply 永不执行"
        );

        // 改回原值 ⇒ 不脏 ⇒ 不该白弹一次 UAC。
        val.set(false);
        assert!(!reg.side_dirty(), "改回原值不该再判脏");
    }

    /// ⛔ 回归：开关关掉一次之后就再也打不开。
    ///
    /// 配置层写回会剔除「等于默认值」的键——关掉开关保存后，`[system]` 段整段从
    /// config.toml 里消失。若基线在键缺失时留空，`system_needs_apply` 便恒判 false，
    /// 之后无论怎么拧都不落地，且没有任何提示。2026-09-07 真机上就是这么复现的：
    /// 关闭成功后 `grep dota2 config.toml` 什么都搜不到。
    #[test]
    fn dota2_compat_survives_pruned_key() {
        let reg = Rc::new(FieldRegistry::default());
        let val = Rc::new(std::cell::Cell::new(false));
        {
            let v = val.clone();
            reg.getters.borrow_mut().insert(
                "system.dota2_compat".into(),
                Box::new(move || Value::Bool(v.get())),
            );
        }
        // 配置里**没有** [system] 段——等于默认值的键被写回剪枝掉了。
        let cfg = Config::from_value(serde_json::json!({}));
        // 传 None = 拿不到注册表实况，走「按配置播种」那条回退路径 —— 本条测的正是它。
        reg.seed_system_baseline(&cfg, &Capabilities::default(), None);
        register_system_registrations(&reg);

        assert!(
            !reg.side_dirty(),
            "键缺失时的基线应是默认值（关），没动过不该判脏"
        );

        val.set(true);
        assert!(
            reg.side_dirty(),
            "键被剪枝掉之后仍须能开启，否则开关关一次就永久失灵"
        );
    }

    fn intent(enabled: bool, name: &str) -> Dota2Intent {
        Dota2Intent {
            enabled,
            name: name.to_string(),
        }
    }

    /// 落地判据的边界。`commit` 会弹 UAC 测不了，判据因此抽成纯函数由本条直接守。
    #[test]
    fn system_needs_apply_holds_off_until_loaded() {
        const N: &str = "中文 (简体) - 郑码";
        assert!(
            !system_needs_apply(None, &intent(true, N)),
            "加载态未知时一律不得动系统——那时任何「差异」都是假的"
        );
        assert!(!system_needs_apply(None, &intent(false, N)));
        assert!(
            system_needs_apply(Some(&intent(false, N)), &intent(true, N)),
            "关→开要落地"
        );
        assert!(
            system_needs_apply(Some(&intent(true, N)), &intent(false, N)),
            "开→关同样要落地"
        );
        assert!(
            !system_needs_apply(Some(&intent(true, N)), &intent(true, N)),
            "同值不该重复落地，否则每次保存都白弹一次 UAC"
        );
    }

    fn sys(desc: Option<&str>, rec: Option<&str>) -> crate::dota2_compat::SystemAlias {
        crate::dota2_compat::SystemAlias {
            description: desc.map(str::to_string),
            recorded: rec.map(str::to_string),
            real: "清风输入法".to_string(),
        }
    }

    /// ⛔ 回归：配置与注册表漂移之后，设置页再也修不回来。
    ///
    /// 基线此前全部来自 config.toml，从不看注册表。于是注册表一旦被外部改掉
    /// （升级重注册、别的输入法、手工改），`base == now` 恒成立 ⇒ 不判脏 ⇒ 落地直接
    /// return。用户唯一的出路是「关掉再打开」两次 UAC，而界面没有任何提示说要这么做。
    #[test]
    fn baseline_follows_the_registry_not_the_config() {
        let reg = Rc::new(FieldRegistry::default());
        let caps = Capabilities::default();
        // 配置说「开着，用甲」——但系统里其实已经被还原成真实名（= 兼容实际是关的）。
        let cfg = Config::from_value(serde_json::json!({
            "system": { "dota2_compat": true, "dota2_compat_name": "甲" }
        }));

        reg.seed_system_baseline(&cfg, &caps, Some(&sys(Some("清风输入法"), None)));
        assert_eq!(
            reg.system_baseline_bool(DOTA2_KEY),
            Some(false),
            "基线该说「系统里是关的」，而不是照抄配置里的 true"
        );
        // 于是「配置说开着」与基线不符 ⇒ 判脏 ⇒ 用户点一次应用就修好了。
        assert!(
            system_needs_apply(Some(&intent(false, "甲")), &intent(true, "甲")),
            "漂移必须能被判出来，否则用户永远修不回去"
        );
    }

    /// 名称基线取记录值，而不是当前 Description。
    ///
    /// 两者在老装机上会不一致：那时还没有记录值，只有 Description。记录值是我们自己
    /// 写的、跨重注册也在，故优先；Description 只作回退。
    #[test]
    fn baseline_name_prefers_the_recorded_alias() {
        let reg = Rc::new(FieldRegistry::default());
        let caps = Capabilities::default();
        let cfg = Config::from_value(serde_json::json!({}));

        reg.seed_system_baseline(&cfg, &caps, Some(&sys(Some("甲"), Some("乙"))));
        assert_eq!(
            reg.system_baseline_str(DOTA2_NAME_KEY).as_deref(),
            Some("乙")
        );

        // 老装机：只有 Description，没有记录值 ⇒ 回退到 Description。
        reg.seed_system_baseline(&cfg, &caps, Some(&sys(Some("甲"), None)));
        assert_eq!(
            reg.system_baseline_str(DOTA2_NAME_KEY).as_deref(),
            Some("甲")
        );
    }

    /// 关着的时候不拿注册表里的名字当名称基线。
    ///
    /// 关着时注册表里就是真实名，拿它当基线会把「关着 + 界面上填了个名字」误判成改名。
    #[test]
    fn baseline_name_is_left_alone_while_disabled() {
        let reg = Rc::new(FieldRegistry::default());
        let cfg = Config::from_value(serde_json::json!({
            "system": { "dota2_compat": false, "dota2_compat_name": "甲" }
        }));
        reg.seed_system_baseline(
            &cfg,
            &Capabilities::default(),
            Some(&sys(Some("清风输入法"), None)),
        );
        assert_eq!(
            reg.system_baseline_str(DOTA2_NAME_KEY).as_deref(),
            Some("甲"),
            "关着时名称基线应保留配置值，不该被真实名覆盖"
        );
    }

    /// 名称是落地意图的一部分：开着的时候改名必须落地，关着的时候改名不许落地。
    #[test]
    fn system_needs_apply_tracks_the_name_only_while_enabled() {
        assert!(
            system_needs_apply(Some(&intent(true, "甲")), &intent(true, "乙")),
            "开关没动、只换了名字也必须落地，否则界面写着新名字、系统里还是旧的"
        );
        assert!(
            !system_needs_apply(Some(&intent(false, "甲")), &intent(false, "乙")),
            "关着的时候注册表里是真实名称，改名改不到任何东西，判脏只会白弹一次 UAC"
        );
        assert!(
            system_needs_apply(Some(&intent(false, "甲")), &intent(true, "乙")),
            "同时改了开关与名字，按开关那条判脏即可"
        );
    }

    /// **选项列表整份换过之后，选中索引要跟着值走。**
    ///
    /// 主题下拉的选中态是索引不是值，而 core 给的顺序是「内置在前 + 用户主题按 id 排序」
    /// ——导入一个 id 靠前的主题会把后面所有项往后挪一格。索引不跟着走的话：下拉显示成
    /// 别的主题、导出导错、点一次保存还会把那个错的值写进 `ui.theme.name`。
    #[test]
    fn selected_index_follows_the_value_when_options_are_replaced() {
        let values = signal(vec!["default".to_string(), "zen".to_string()]);
        let index = signal(1); // 选中 zen

        // 导入 ash（id 排在 zen 前）→ core 重扫，列表整份替换。
        values.set(vec![
            "default".to_string(),
            "ash".to_string(),
            "zen".to_string(),
        ]);
        // ★ 前置条件：不重定位的话这个下标此刻指的是 ash，不是用户选的 zen。
        //   没有这条断言，下面那句「等于 2」在「插入不会打乱下标」的假设下也会绿。
        assert_eq!(
            values.with(|v| v[index.get()].clone()),
            "ash",
            "前置条件：列表换过之后，原下标确实指到了别的主题上"
        );

        relocate_index(values, index, Some("zen"));
        assert_eq!(index.get(), 2);
        assert_eq!(values.with(|v| v[index.get()].clone()), "zen");
    }

    /// 选中的那一项被删掉时**不动索引**：配置里的取值已由 `push_unlisted_theme` 补到
    /// 列表末尾，这里再猜一个下标只会盖掉它。
    #[test]
    fn relocate_leaves_the_index_alone_when_the_value_is_gone() {
        let values = signal(vec!["default".to_string(), "ash".to_string()]);
        let index = signal(1);

        relocate_index(values, index, Some("zen")); // zen 已被删
        assert_eq!(index.get(), 1, "找不到就别动");

        relocate_index(values, index, None); // 列表此前未就位
        assert_eq!(index.get(), 1);
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

    /// 几个导入入口的共同落点：撞名要转成「要不要覆盖」，而不是报一条失败了事。
    ///
    /// 这条判断此前在 URL 与本地文件两个入口各写了一遍，主题包进来会是第三遍——它也
    /// 恰恰是最容易写漏的那条（早先有入口把冲突和读取失败共用一条 toast，用户只看到
    /// 「主题已存在」却没有任何前进的路）。收进一处之后，这里是它唯一的护栏。
    #[test]
    fn try_import_theme_turns_conflict_into_overwrite_prompt() {
        // into_inner: 别的测试 panic 毒化了锁时仍继续，避免一处失败连锁放大
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let st = AppState::empty();
        let outcome = AppState::try_import_theme(
            crate::theme_import::ThemeSource::Package(r"C:\d\MOCK_EXISTING.wtheme".to_string()),
            st.theme_overwrite_source,
            st.theme_overwrite_msg,
            st.theme_overwrite_visible,
        );

        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert!(outcome.is_none(), "撞名既没成也没败，要等用户拍板");
        assert!(st.theme_overwrite_visible.get(), "该弹覆盖确认");
        assert!(
            matches!(
                st.theme_overwrite_source.get(),
                Some(crate::theme_import::ThemeSource::Package(p)) if p.contains("MOCK_EXISTING")
            ),
            "来源要原样暂存下来供 force 重推"
        );
    }

    /// 队列 sink：测试代替 channel 回调去 `on_done`。
    type NetQueue = Arc<std::sync::Mutex<Vec<crate::net_task::NetDone>>>;

    fn state_with_queue_sink() -> (Rc<RefCell<AppState>>, NetQueue) {
        let st = Rc::new(RefCell::new(AppState::empty()));
        let queue: NetQueue = Default::default();
        let q = queue.clone();
        st.borrow().net.set_sink(move |m| q.lock().unwrap().push(m));
        (st, queue)
    }

    /// 等一条结果；有截止时间，线程不回话时测试失败而不是挂住。
    fn wait_net(queue: &NetQueue) -> crate::net_task::NetDone {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(m) = queue.lock().unwrap().pop() {
                return m;
            }
            assert!(std::time::Instant::now() < deadline, "后台线程没有回结果");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// 主题下载走后台：点击只置「下载中」，结果回 UI 线程才进续体；失败走 toast 并恢复空闲。
    /// 用 http:// 地址让 `download_https_text` 在发请求前就失败，测试不需要网络。
    #[test]
    fn theme_download_is_async_and_failure_restores_idle() {
        let (st, queue) = state_with_queue_sink();
        let visible = signal(true);

        AppState::start_theme_download(
            &st,
            ThemeDlOwner::Picker,
            "http://example.com/a.toml".to_string(),
            visible,
            |_, _| panic!("下载失败不该进续体"),
        );
        assert!(!st.borrow().theme_download_idle.get(), "下载中按钮应置灰");

        let net = st.borrow().net.clone();
        net.on_done(wait_net(&queue));

        let s = st.borrow();
        assert!(s.theme_download_idle.get(), "完成后恢复空闲");
        let (kind, _) = s.feedback.get().expect("应有失败反馈");
        assert_eq!(kind, ToastKind::Error);
    }

    /// 下载期间对话框被关（含 ESC）：结果直接丢弃，不弹错误也不进续体。
    #[test]
    fn theme_download_result_is_dropped_when_dialog_closed() {
        let (st, queue) = state_with_queue_sink();
        let visible = signal(true);

        AppState::start_theme_download(
            &st,
            ThemeDlOwner::Picker,
            "http://example.com/a.toml".to_string(),
            visible,
            |_, _| panic!("对话框已关，不该进续体"),
        );
        st.borrow().feedback.set(None);
        visible.set(false);

        let net = st.borrow().net.clone();
        net.on_done(wait_net(&queue));

        let s = st.borrow();
        assert!(s.theme_download_idle.get());
        assert!(s.feedback.get().is_none(), "关框后不该再弹错误");
    }

    /// 取消 = 摘掉续体：就算结果已经在路上，`on_done` 也不会再跑它（续体里 panic 为证）。
    /// 先让工作线程确实跑完，再取消、再投递，覆盖「迟到结果」这条路。
    #[test]
    fn cancelled_theme_download_drops_late_result() {
        let (st, queue) = state_with_queue_sink();
        AppState::start_theme_download(
            &st,
            ThemeDlOwner::Picker,
            "http://example.com/a.toml".to_string(),
            signal(true),
            |_, _| panic!("已取消，不该进续体"),
        );
        // 工作线程不查取消就会把结果发回：等它发回（线程已结束），再取消、再投递。
        let msg = wait_net(&queue);
        st.borrow().cancel_theme_download(ThemeDlOwner::Picker);
        assert!(st.borrow().theme_download_idle.get(), "取消后按钮恢复可用");
        st.borrow().feedback.set(None);
        let net = st.borrow().net.clone();
        net.on_done(msg);
        assert!(st.borrow().feedback.get().is_none(), "迟到结果不得再弹提示");
    }

    /// 两个对话框共用一个任务槽：没在下载的那个被收起，不能取消另一个的下载。
    #[test]
    fn closing_the_other_theme_dialog_does_not_cancel_my_download() {
        let st = Rc::new(RefCell::new(AppState::empty()));
        AppState::start_theme_download(
            &st,
            ThemeDlOwner::Picker,
            "http://example.com/a.toml".to_string(),
            signal(true),
            |_, _| {},
        );
        st.borrow().cancel_theme_download(ThemeDlOwner::Protocol);
        assert!(
            !st.borrow().theme_download_idle.get(),
            "别家对话框的关闭不该动我的下载"
        );
        st.borrow().cancel_theme_download(ThemeDlOwner::Picker);
        assert!(st.borrow().theme_download_idle.get());
    }

    /// 协议路由「收起」本来就关着的主题确认框：不得升版本，否则它的 effect 会把这当成
    /// 刚被关掉、去取消别人的下载。
    #[test]
    fn hiding_an_already_hidden_protocol_dialog_does_not_bump_its_version() {
        let st = AppState::empty();
        let before = st.theme_import_dialog_visible.version();
        st.hide_protocol_theme_dialog();
        assert_eq!(st.theme_import_dialog_visible.version(), before);

        st.theme_import_dialog_visible.set(true);
        st.hide_protocol_theme_dialog();
        assert!(!st.theme_import_dialog_visible.get(), "开着时要真收起");
    }

    /// 协议方案包下载中可取消：框收起、任务摘掉，随后可以再发起新的。
    /// 不设 sink：任务只排队不起线程，测试不碰网络。
    #[test]
    fn schema_download_can_be_cancelled_and_restarted() {
        let st = Rc::new(RefCell::new(AppState::empty()));
        AppState::start_schema_download(&st, "https://windinput.com/a.zip".to_string());
        assert!(st.borrow().schema_download_visible.get(), "应弹出下载框");

        AppState::start_schema_download(&st, "https://windinput.com/b.zip".to_string());
        let (kind, _) = st.borrow().feedback.get().expect("在下时应拒绝新的");
        assert_eq!(kind, ToastKind::Error);

        st.borrow().cancel_schema_download();
        assert!(!st.borrow().schema_download_visible.get());
        st.borrow().feedback.set(None);
        AppState::start_schema_download(&st, "https://windinput.com/c.zip".to_string());
        assert!(
            st.borrow().feedback.get().is_none(),
            "取消后应能再起新的下载"
        );
        assert!(
            st.borrow().schema_download_visible.get(),
            "新的下载应弹出下载框"
        );
    }

    /// ESC 只把可见性置 false，取消要等下一帧的 effect：这段空窗里来的新请求
    /// 不能被误判成「上一个还在下载」。
    #[test]
    fn schema_download_after_esc_is_not_rejected_as_busy() {
        let st = Rc::new(RefCell::new(AppState::empty()));
        AppState::start_schema_download(&st, "https://windinput.com/a.zip".to_string());
        st.borrow().schema_download_visible.set(false); // ESC，effect 尚未跑
        st.borrow().feedback.set(None);

        AppState::start_schema_download(&st, "https://windinput.com/b.zip".to_string());
        assert!(st.borrow().feedback.get().is_none(), "不该被拒");
        assert!(st.borrow().schema_download_visible.get());
    }

    /// 没撞名时不该弹任何框，结论直接交回调用方。
    #[test]
    fn try_import_theme_passes_through_when_no_conflict() {
        // into_inner: 别的测试 panic 毒化了锁时仍继续，避免一处失败连锁放大
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let st = AppState::empty();
        let outcome = AppState::try_import_theme(
            crate::theme_import::ThemeSource::Package(r"C:\d\fresh.wtheme".to_string()),
            st.theme_overwrite_source,
            st.theme_overwrite_msg,
            st.theme_overwrite_visible,
        );

        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert!(!st.theme_overwrite_visible.get(), "没撞名不该弹框");
        assert_eq!(st.theme_overwrite_source.get(), None, "也不该留暂存");
        assert_eq!(
            outcome.expect("该有结论").expect("该成功").slug,
            "test-theme-pkg",
            "包来源必须走 theme.importPackage"
        );
    }

    /// 撞上同名主题 ⇒ 暂存原文 + 弹覆盖确认（而不是只报一条「已存在」）。
    #[test]
    fn ask_overwrite_theme_stashes_source_and_opens_dialog() {
        let st = AppState::empty();
        assert!(!st.theme_overwrite_visible.get(), "初始不该弹覆盖确认");

        AppState::ask_overwrite_theme(
            st.theme_overwrite_source,
            st.theme_overwrite_msg,
            st.theme_overwrite_visible,
            crate::theme_import::ThemeSource::Text("[meta]\nname = \"测试主题\"\n".to_string()),
            "主题「测试主题」已存在",
        );

        assert!(st.theme_overwrite_visible.get());
        // 文本来源暂存的是**原文**：确认后直接 force 重推，不重新下载/读盘。
        assert!(matches!(
            st.theme_overwrite_source.get(),
            Some(crate::theme_import::ThemeSource::Text(t)) if t.contains("测试主题")
        ));
        let msg = st.theme_overwrite_msg.get();
        assert!(msg.contains("测试主题"), "正文要点名是哪个主题，实际={msg}");
        assert!(
            msg.contains("覆盖"),
            "正文要说清确认后会发生什么，实际={msg}"
        );
    }

    /// 确认覆盖 ⇒ 以 force=true 重推暂存原文，收框并清暂存。
    #[test]
    fn confirm_overwrite_theme_force_pushes_and_clears_stash() {
        // into_inner: 别的测试 panic 毒化了锁时仍继续，避免一处失败连锁放大
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let mut st = AppState::empty();
        // 哨兵：mock 对 force=false 回冲突、force=true 回成功。暂存的这份仍带哨兵，
        // 因此这条断言同时证明了重推用的是 force=true——否则 mock 会再回一次冲突。
        AppState::ask_overwrite_theme(
            st.theme_overwrite_source,
            st.theme_overwrite_msg,
            st.theme_overwrite_visible,
            crate::theme_import::ThemeSource::Text(
                "[meta]\nname = \"MOCK_EXISTING\"\n".to_string(),
            ),
            "主题「测试主题」已存在",
        );
        st.confirm_overwrite_theme();

        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert!(!st.theme_overwrite_visible.get(), "确认后应收起对话框");
        assert_eq!(
            st.theme_overwrite_source.get(),
            None,
            "确认后应清掉暂存来源"
        );
        let (kind, msg) = st.feedback.get().expect("应有一条结果反馈");
        assert!(
            matches!(kind, ToastKind::Success),
            "force 重推应成功，实际={msg}"
        );
        assert!(msg.contains("测试主题"), "反馈要点名主题，实际={msg}");
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
        // 判出"有"还不够，清单里得报得出是什么：否则对话框说着「有未保存的更改」却
        // 一条也列不出来——比不列更让人无从下手（见 `SideCommitter::label`）。
        assert!(
            st2.unsaved_items().iter().any(|i| i == "测试落点"),
            "脏的 side 落点须带名字进清单：{:?}",
            st2.unsaved_items()
        );
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

    /// **重扫主题列表之后，下拉选中的还得是原来那个主题。**
    ///
    /// 与 `selected_index_follows_the_value_when_options_are_replaced` 的分工：那条测
    /// [`relocate_index`] 这个判据本身，这条测 [`AppState::refresh_theme_names`] **真的
    /// 会去调它** —— 判据再对，接线断了一样出事，而删掉那行调用只有这条会红。
    ///
    /// 场景就是真实缺陷：选中 mono（此刻下标 1）→ 重扫后列表变成
    /// `[default, jade, mono]`，mono 挪到了下标 2。索引不跟着走的话，下拉显示的会是
    /// jade，导出导 jade，点保存把 `ui.theme.name` 写成 jade。
    #[test]
    fn refresh_theme_names_keeps_the_selection_on_the_same_theme() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();

        // 重扫前：列表里只有两项，选中末项 mono。
        st.theme_value_sig
            .set(vec!["jade".to_string(), "mono".to_string()]);
        st.theme_label_sig
            .set(vec!["青玉".to_string(), "墨".to_string()]);
        let idx = signal(1);
        st.fields
            .select_indices
            .borrow_mut()
            .insert("ui.theme.name".to_string(), idx);

        st.refresh_theme_names();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };

        // 前置条件：重扫确实把 mono 挪了位，否则这条用例测不到任何东西。
        assert_eq!(st.theme_value_sig.get(), vec!["default", "jade", "mono"]);
        assert_eq!(idx.get(), 2, "索引要跟着 mono 走到新下标");
        assert_eq!(
            st.theme_value_sig.with(|v| v[idx.get()].clone()),
            "mono",
            "选中的还得是 mono —— 不重定位的话这里是 jade"
        );
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
        assert_eq!(st.theme_value_sig.get(), vec!["default", "jade", "mono"]);
        assert_eq!(st.theme_label_sig.get(), vec!["清风·浅", "青玉", "墨"]);
        // 条目携带版本与内置标记。
        assert_eq!(st.theme_entries[0].version, "3.2.1");
        assert!(st.theme_entries[0].builtin, "default 为内置");
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

    /// 配置里的主题已经不在列表里（外部主题被删 / 这一轮没扫到）时，它必须仍留在下拉里
    /// 被选中——否则打开设置页点一次保存，`ui.theme.name` 就被静默换成列表第一项，用户
    /// 只会说「我的主题怎么自己变了」。响应式路径的补项语义见
    /// [`crate::manifest::push_unlisted_option`]。
    #[test]
    fn refresh_theme_names_keeps_a_theme_that_is_gone_from_the_list() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        st.current_config
            .set_path("ui.theme.name", Value::String("gone".to_string()));

        st.refresh_theme_names();

        let values = st.theme_value_sig.get();
        assert_eq!(
            values.last().map(String::as_str),
            Some("gone"),
            "被删的当前主题该补在列表末尾，实际: {values:?}"
        );
        assert!(
            st.theme_label_sig.get().last().unwrap().contains("gone"),
            "标签应能看出这是当前配置"
        );
        // 富内容项与值列表错位一项，下拉显示的就不是选中的那个主题。
        let items = st.theme_items_sig.get();
        assert_eq!(items.len(), values.len(), "富内容项必须与值列表同序等长");
        assert!(
            items.last().unwrap().trailing_icon.is_none(),
            "补出来的那项不是装着的主题，不该给删除图标"
        );

        // 列表里有的主题不补：补了会多出一个重复项。
        st.current_config
            .set_path("ui.theme.name", Value::String("jade".to_string()));
        st.refresh_theme_names();
        assert_eq!(st.theme_value_sig.get(), vec!["default", "jade", "mono"]);

        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 覆盖导入 / 删除当前主题都经 `refresh_theme_names` 刷新列表，但跟随值
    /// （`theme.resolved`）此前不重拉——外观页「跟随主题（值）」的文案与勾选初值会继续
    /// 挂着旧主题的数（Task 11 控制者裁定项 C）。
    ///
    /// 用 `rpc::take_mock_calls()` 断言确实发出过 `theme.resolved`，再断言 `theme_follow`
    /// 信号被刷新为 mock 值（`font_size = 18`）——先把信号设成一个不同的值，只有真的
    /// 重拉才会变回 18。
    #[test]
    fn refresh_theme_names_also_refreshes_theme_follow() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        st.theme_follow.set(Some(crate::theme_follow::ThemeFollow {
            font_size: 99,
            font_family: None,
            font_weight: 0,
            pager_bar_display: "auto".into(),
            page_number_display: "show".into(),
            langbar_text: Default::default(),
        }));
        crate::rpc::take_mock_calls(); // 清空 empty() 期间可能已有的调用记录。

        st.refresh_theme_names();

        let calls = crate::rpc::take_mock_calls();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert!(
            calls.iter().any(|(m, _)| m == "theme.resolved"),
            "刷新主题列表却没有重拉 theme.resolved，实际调用: {calls:?}"
        );
        assert_eq!(
            st.theme_follow.get().map(|t| t.font_size),
            Some(18),
            "theme_follow 该被刷成 mock 的 theme.resolved 值（18），不是刷新前的 99"
        );
    }

    /// 跟随输入法主题色：同一主题只取一次色；换上的是修正后的颜色；切回默认即恢复默认蓝。
    #[test]
    fn accent_follow_fetches_once_per_theme_and_applies() {
        use crate::accent::{AccentMode, derive};
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        let th = ThemeHandle::detached(Theme::default());
        *st.theme_handle.borrow_mut() = Some(th.clone());
        st.dark.set(false);
        st.current_config
            .set_path("ui.theme.name", Value::String("jade".into()));
        st.accent_idx.set(AccentMode::Ime.index());
        crate::rpc::take_mock_calls();

        st.sync_accent();
        st.sync_accent();

        let calls = crate::rpc::take_mock_calls();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        let fetches: Vec<_> = calls.iter().filter(|(m, _)| m == "theme.accent").collect();
        assert_eq!(fetches.len(), 1, "同一主题不该重复取色: {calls:?}");
        assert_eq!(fetches[0].1["name"], "jade");
        // mock jade 亮档 accentText #059669 压白字不足，换上的是调深后的颜色。
        let want = derive(Color::hex(0x059669), false).set().unwrap().accent;
        assert_eq!(th.current().palette.accent, want);
        assert!(
            st.accent_note.get().contains("已调深"),
            "{}",
            st.accent_note.get()
        );

        st.accent_idx.set(AccentMode::Default.index());
        st.sync_accent();
        assert_eq!(
            th.current().palette.accent,
            crate::theme_def::theme_for(false).palette.accent
        );
        assert_eq!(st.accent_note.get(), "", "默认模式不挂说明");
    }

    /// 默认模式根本不碰 core：不开跟随的用户不该为这个功能多付一次 RPC。
    #[test]
    fn accent_default_mode_never_calls_core() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let st = AppState::empty();
        st.accent_idx
            .set(crate::accent::AccentMode::Default.index());
        crate::rpc::take_mock_calls();
        st.sync_accent();
        let calls = crate::rpc::take_mock_calls();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert!(!calls.iter().any(|(m, _)| m == "theme.accent"), "{calls:?}");
    }

    /// 灰色主题（mock 的 mono）回落默认蓝，并说明原因。
    #[test]
    fn accent_follow_neutral_theme_falls_back_with_note() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        let th = ThemeHandle::detached(Theme::default());
        *st.theme_handle.borrow_mut() = Some(th.clone());
        st.dark.set(true);
        st.current_config
            .set_path("ui.theme.name", Value::String("mono".into()));
        st.accent_idx.set(crate::accent::AccentMode::Ime.index());
        st.sync_accent();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert_eq!(
            th.current().palette.accent,
            crate::theme_def::theme_for(true).palette.accent
        );
        assert!(
            st.accent_note.get().contains("接近灰色"),
            "{}",
            st.accent_note.get()
        );
    }

    /// 取过一次之后 `invalidate_accent` 才会重取（底栏角标「点击重新读取」、主题列表重扫）。
    #[test]
    fn invalidate_accent_forces_refetch() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let st = AppState::empty();
        st.accent_idx.set(crate::accent::AccentMode::Ime.index());
        st.sync_accent();
        crate::rpc::take_mock_calls();
        st.sync_accent();
        st.invalidate_accent();
        st.sync_accent();
        let calls = crate::rpc::take_mock_calls();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        let n = calls.iter().filter(|(m, _)| m == "theme.accent").count();
        assert_eq!(n, 1, "{calls:?}");
    }

    /// ★★★ 打开设置、**什么都不动**就关闭，不该提示「有未保存的更改」。
    ///
    /// `is_dirty` 的做法是「把全部 writer 跑到 current 的副本上，再与 base 比 diff」。
    /// 于是任何一个 writer 只要写出了与 base **不完全相同**的值，就恒判脏——用户看到的是
    /// 每次关窗都被拦一次，而他确实什么也没改。
    ///
    /// 最容易犯的一类：base 里**根本没有那个键**（core 侧未落盘 / 出厂预置文件里没写），
    /// 而 writer 无条件写一个值（哪怕是空数组）⇒ `diff_value` 判为「新增」⇒ 恒脏。
    ///
    /// 本测试把清单里每一项都建一遍（等价于用户把每个页面都翻到），断言 diff 为空。
    #[test]
    fn untouched_settings_produce_no_diff() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        st.apply_loaded(LoadedState::fetch());
        // 建出每一项 = 注册它的 writer。顺序按 manifest 声明序，与真实渲染一致。
        let keys = st.manifest.order.clone();
        for k in &keys {
            let _ = crate::manifest::field(&st, k);
        }
        let items = st.debug_diff_after_writers();
        let names: Vec<&str> = items.iter().map(|i| i.key.as_str()).collect();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert!(
            items.is_empty(),
            "什么都没动却产生了 diff（这些键的 writer 写出了与 base 不同的值）：{names:?}"
        );
    }

    /// ★★ 论坛 t158：同一条守门，但 `ui.comment_dicts` 换成**真实 core 回包的形状**。
    ///
    /// 出厂 mock 里这个数组是空的，上面那条因此拦不住：误报只出在配了注释词库的存量
    /// 配置上——core 序列化出的空壳带 `path: ""` / `schemas: []`、认领不到的空壳、
    /// 省略了 `id` 的手写条目。这里把这几种形态一起塞进去。
    #[test]
    fn untouched_comment_dicts_in_core_shape_produce_no_diff() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let spec = |id: &str, label: &str, path: &str, enabled: bool, auto: bool| {
            serde_json::json!({
                "id": id, "label": label, "path": path,
                "enabled": enabled, "auto": auto, "schemas": [],
            })
        };
        let mut data = LoadedState::fetch();
        // mock 的 `comment.sources` 只检测出 rime_frost 这一个自动源。
        data.config_value["ui"]["comment_dicts"] = serde_json::json!([
            spec("auto:pinyin/rime_frost.dict.yaml", "", "", true, true),
            spec("en", "简明英汉", "comments/en.dict.yaml", true, false),
            spec("auto:已停用.dict.yaml", "", "", false, true),
            spec("", "", "comments/emoji.dict.yaml", false, false),
        ]);
        let mut st = AppState::empty();
        st.apply_loaded(data);
        let keys = st.manifest.order.clone();
        for k in &keys {
            let _ = crate::manifest::field(&st, k);
        }
        let items = st.debug_diff_after_writers();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert!(
            items.is_empty(),
            "什么都没动却产生了 diff：{:?}",
            items
                .iter()
                .map(|i| format!("{} = {}", i.key, i.value))
                .collect::<Vec<_>>()
        );
    }

    /// 外观页「跟随主题（值）」的数据来源：加载时并行拉 `theme.resolved` 并在 loaders
    /// 之前就位；字体下拉不再有空值「默认」首项（跟随由勾选行表达）。
    #[test]
    fn apply_loaded_syncs_accent_without_watcher() {
        // 词库窗 / 兼容窗当主窗时没有设置外壳、也就没有 accent_watcher：数据到达本身就要把
        // 跟随色取回来换上，哪怕启动预取那次没取到（输入法晚于设置程序起来）。
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        let th = ThemeHandle::detached(Theme::default());
        *st.theme_handle.borrow_mut() = Some(th.clone());
        st.dark.set(false);
        st.accent_idx.set(crate::accent::AccentMode::Ime.index());
        crate::rpc::take_mock_calls();
        st.apply_loaded(LoadedState::fetch());
        let calls = crate::rpc::take_mock_calls();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert!(calls.iter().any(|(m, _)| m == "theme.accent"), "{calls:?}");
        // mock 当前主题 default 的亮档 accentText #2563EB 本就达标，原样换上。
        assert_eq!(th.current().palette.accent, Color::hex(0x2563EB));
        assert!(!st.accent_note.get().is_empty());
    }

    #[test]
    fn apply_loaded_sets_theme_follow_and_drops_default_font_entry() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        st.apply_loaded(LoadedState::fetch());
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert_eq!(st.theme_follow.get().map(|t| t.font_size), Some(18));
        let fonts = st.font_sig.get();
        assert!(!fonts.is_empty(), "mock 有字体列表");
        assert!(
            !fonts.iter().any(String::is_empty),
            "不该再有空值首项: {fonts:?}"
        );
        assert_eq!(fonts, st.font_label_sig.get(), "值与标签同为族名");
    }

    /// 只有保存了主题选择（主题名 / 明暗）才重拉跟随值。
    #[test]
    fn only_theme_keys_trigger_theme_follow_refresh() {
        let item = |k: &str| crate::config::ConfigItem {
            key: k.to_string(),
            value: Value::Null,
        };
        assert!(touches_theme(&[item("ui.theme.name")]));
        assert!(touches_theme(&[
            item("ui.candidate.font_size"),
            item("ui.theme")
        ]));
        assert!(!touches_theme(&[item("ui.candidate.font_size")]));
        assert!(!touches_theme(&[item("ui.theme_editor")]));
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
            degradation: Value::Object(Default::default()),
            capabilities_value: Value::Object(Default::default()),
            schemas: vec![],
            theme_entries: vec![],
            fonts: vec![],
            theme_follow: None,
            override_summary: HashMap::new(),
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
            degradation: Value::Object(Default::default()),
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
            theme_follow: None,
            override_summary: HashMap::new(),
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

    /// 未降级、或根本拿不到回包时**不出横幅**。
    ///
    /// 后者尤其要紧：老 core 没有 `config.degradation` 方法、或连接失败时回包是空对象，
    /// 此时吓唬用户说他的配置坏了是最糟的——那条提示会让人去改一个本来没问题的文件。
    #[test]
    fn degradation_text_is_empty_when_healthy_or_unknown() {
        assert_eq!(degradation_text(&serde_json::json!({})), "");
        assert_eq!(
            degradation_text(&serde_json::json!({ "degraded": false, "sections": [] })),
            ""
        );
    }

    #[test]
    fn degradation_text_lists_sections_verbatim() {
        let t = degradation_text(&serde_json::json!({
            "degraded": true,
            "totalFallback": false,
            "sections": ["keys.key_actions", "ui.font"],
        }));
        // 点分路径原样列出：用户拿着这一行要去配置文件里找那一段，译成界面用语就对不上了。
        // 子表粒度（`ui.font` 而不是 `ui`）是 core 刻意保留的精度，这里不该截成顶层段名。
        assert!(t.contains("keys.key_actions"), "段名必须原样出现：{t}");
        assert!(t.contains("ui.font"), "子表粒度不能被截成顶层段名：{t}");
        assert!(
            t.contains("没有生效"),
            "必须说清后果，而不只是说「解析失败」：{t}"
        );
    }

    #[test]
    fn degradation_text_handles_total_fallback_and_empty_sections() {
        let total = degradation_text(&serde_json::json!({
            "degraded": true, "totalFallback": true, "sections": [],
        }));
        assert!(total.contains("整体"), "整份回落要说清是整份：{total}");
        // degraded 为真却没给段名：契约本该互斥，走到这里说明 core 变了。
        // 仍要出提示——「有问题但说不清哪一段」也比一声不吭强。
        let odd = degradation_text(&serde_json::json!({
            "degraded": true, "totalFallback": false, "sections": [],
        }));
        assert!(!odd.is_empty(), "说不清哪一段也不能一声不吭");
    }

    /// ★ 语法故障要盖过段级降级来讲，并且必须带上行号。
    ///
    /// 两者修法不同（改那一行 vs 改那个键的类型），合成一句会把用户支使到错误的地方；
    /// 而「配置有语法错误」这种说法用户无从下手，行号是他唯一能直接用的线索。
    #[test]
    fn degradation_text_prefers_syntax_error_and_names_the_line() {
        let t = degradation_text(&serde_json::json!({
            "degraded": true,
            "totalFallback": false,
            // 刻意同时给出段级降级：语法故障必须赢。
            "sections": ["ui.font"],
            "unparsable": [{
                "layer": "user",
                "path": "C:\\Users\\me\\AppData\\Roaming\\WindInput\\config.toml",
                "error": "duplicate key `per_page`",
                "skippedLines": [6, 12],
                "salvagedKeys": 3,
            }],
        }));
        assert!(t.contains('6') && t.contains("12"), "行号必须出现：{t}");
        assert!(t.contains("config.toml"), "要说清是哪个文件：{t}");
        assert!(
            !t.contains("ui.font"),
            "语法故障成立时不该再讲段级降级——修法不同，混讲会误导：{t}"
        );
        assert!(
            t.contains("不会被覆盖"),
            "用户最担心的是配置被改坏，这句必须在：{t}"
        );

        // 一行都没救回来时退回笼统说法，但仍要给出文件名。
        let none = degradation_text(&serde_json::json!({
            "degraded": true, "totalFallback": false, "sections": [],
            "unparsable": [{ "path": "x/config.toml", "skippedLines": [] }],
        }));
        assert!(none.contains("x/config.toml"), "{none}");
        assert!(none.contains("都没有生效"), "{none}");

        // 编码不对时讲「另存为 UTF-8」，不讲「修语法」。
        let enc = degradation_text(&serde_json::json!({
            "degraded": true, "totalFallback": false, "sections": [],
            "unparsable": [{ "path": "x/config.toml", "skippedLines": [], "notUtf8": true }],
        }));
        assert!(
            enc.contains("UTF-8") && enc.contains("x/config.toml"),
            "{enc}"
        );
        assert!(!enc.contains("语法"), "{enc}");
        assert!(!none.contains("UTF-8"), "对照：语法错误不该讲编码：{none}");

        // ★ 跳过了行**却什么都没救回来**（core 啃到上限仍失败）：这两个条件同时成立，
        // 判据若写成「有行号 ⇒ 救回来了」就会说「其余设置已正常加载」——撒谎。
        let tried = degradation_text(&serde_json::json!({
            "degraded": true, "totalFallback": false, "sections": [],
            "unparsable": [{
                "path": "x/config.toml", "skippedLines": [1, 2, 3], "salvagedKeys": 0,
            }],
        }));
        assert!(
            !tried.contains("其余设置已正常加载"),
            "一个键都没救回来时不许说其余正常：{tried}"
        );
        assert!(tried.contains("都没有生效"), "{tried}");
        assert!(
            tried.contains('1') && tried.contains('3'),
            "尝试跳过的行号仍要给出：{tried}"
        );

        // ★ 行号多到一定程度必须折叠：core 啃不动的文件会攒到 32 个行号，
        // 全列进横幅会撑爆版面（截图实测过），且那时具体哪几行本就没意义。
        // 阈值须与 core 的 `UnparsableLayer::lines_brief` 一致。
        let many = degradation_text(&serde_json::json!({
            "degraded": true, "totalFallback": false, "sections": [],
            "unparsable": [{
                "path": "x/config.toml",
                "skippedLines": (1..=32).collect::<Vec<u32>>(),
                "salvagedKeys": 4,
            }],
        }));
        assert!(many.contains("等 32 处"), "超阈值须折叠并给出总数：{many}");
        assert!(!many.contains("30"), "不该把第 30 个行号也列出来：{many}");
    }

    #[test]
    fn mock_disconnect_does_not_block() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        let data = LoadedState {
            config_value: Value::Object(Default::default()),
            system_info: Value::Object(Default::default()),
            degradation: Value::Object(Default::default()),
            capabilities_value: Value::Object(Default::default()),
            schemas: vec![],
            theme_entries: vec![],
            fonts: vec![],
            theme_follow: None,
            override_summary: HashMap::new(),
            connected: false,
        };
        st.apply_loaded(data);
        assert!(!st.conn_failed.get(), "mock 模式不应阻断");
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 设置页与 core 挑本地化字体名的规则必须一致（见 [`super::pick_font_display_name`]）。
    #[test]
    fn font_name_pick_matches_core() {
        let cases: Vec<Vec<(&str, &str)>> = vec![
            vec![("en-us", "Source Han Serif SC"), ("zh-CN", "思源宋体")],
            vec![("ja-jp", "源ノ明朝"), ("en-us", "Source Han Serif")],
            vec![("ja-jp", "源ノ明朝")],
            vec![("zh-cn", " "), ("en-us", "Foo")],
            vec![],
        ];
        for c in cases {
            let names: Vec<(String, String)> = c
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect();
            assert_eq!(
                super::pick_font_display_name(&names),
                wind_config::font_name::pick_localized_name(&names),
                "{names:?}"
            );
        }
    }

    /// 字体下拉排序：中文名（含 CJK 字符）整组排在英文名前面，组内维持
    /// [`super::sort_font_names`] 用的 `String` 默认序不变。
    #[test]
    fn sort_font_names_puts_cjk_group_first() {
        // 混合中英
        let mixed = vec![
            "Arial".to_string(),
            "微软雅黑".to_string(),
            "Calibri".to_string(),
            "思源宋体".to_string(),
            "Consolas".to_string(),
        ];
        assert_eq!(
            super::sort_font_names(mixed),
            vec!["微软雅黑", "思源宋体", "Arial", "Calibri", "Consolas"]
        );

        // 全英：保持原有 String 序，不受分组影响
        let all_en = vec!["Consolas".to_string(), "Arial".to_string()];
        assert_eq!(super::sort_font_names(all_en), vec!["Arial", "Consolas"]);

        // 全中：保持原有 String 序
        let all_zh = vec!["思源宋体".to_string(), "微软雅黑".to_string()];
        assert_eq!(super::sort_font_names(all_zh), vec!["微软雅黑", "思源宋体"]);

        // 空列表
        assert_eq!(super::sort_font_names(vec![]), Vec::<String>::new());
    }

    /// `@` 竖排名（如 `@微软雅黑`）在 [`super::load_fonts_from_dwrite`] 里已被现有过滤逻辑
    /// 挡在 `sort_font_names` 之外；这里确认它本身若混进来仍按含 CJK 字符归入中文组，
    /// 不会因为前缀 `@` 被误判成英文名。
    #[test]
    fn sort_font_names_vertical_name_still_counts_as_cjk() {
        let names = vec!["Arial".to_string(), "@微软雅黑".to_string()];
        assert_eq!(super::sort_font_names(names), vec!["@微软雅黑", "Arial"]);
    }

    #[test]
    fn contains_cjk_detects_common_ranges() {
        assert!(super::contains_cjk("思源宋体"));
        assert!(super::contains_cjk("Arial 微软"));
        assert!(!super::contains_cjk("Arial"));
        assert!(!super::contains_cjk(""));
    }

    /// `system.fonts` 按 core 的真实形状解析：取本地化名、缺了退回英文族名、重名去重、
    /// 中文组在前。旧实现只认 mock 编出来的 `name` 键，对真回包恒产出空表。
    #[test]
    fn font_names_from_rpc_reads_core_shape() {
        let v = serde_json::json!([
            { "family": "DejaVu Sans", "display_name": "DejaVu Sans" },
            { "family": "Noto Sans CJK SC", "display_name": "思源黑体" },
            { "family": "Source Han Sans SC", "display_name": "思源黑体" },
            { "family": "WenQuanYi Micro Hei", "display_name": "" },
            { "family": "", "display_name": "" },
        ]);
        assert_eq!(
            super::font_names_from_rpc(&v),
            vec!["思源黑体", "DejaVu Sans", "WenQuanYi Micro Hei"]
        );
        assert!(super::font_names_from_rpc(&serde_json::json!(null)).is_empty());
    }
}

/// 加载系统字体列表：Windows 用 DirectWrite 系统字体集枚举字体**家族**；
/// 非 Windows 或枚举失败时回退 system.fonts RPC。
///
/// # 为什么不用 GDI（论坛 t95 / core 看板 A2-1）
///
/// 候选窗渲染端用 DirectWrite `FindFamilyName` 查字体。早先这里用 GDI
/// `EnumFontFamiliesExW` 供给下拉，而 GDI 每个 family 最多 4 个 face，七字重的「思源宋体」
/// 被拆成「思源宋体 SemiBold」这类独立 face name 摆进下拉——DirectWrite 里查不到，
/// 选了一律静默回落默认字体。GDI 名还受 `lfFaceName` 31 字符截断。
/// 现在下拉列 DirectWrite family（不含字重），字重是单独一项 `ui.font.weight`；
/// 老配置里的 GDI 名由 core 渲染端解析兼容，这里不迁移。
fn load_fonts() -> Vec<String> {
    #[cfg(windows)]
    {
        let fonts = load_fonts_from_dwrite();
        if !fonts.is_empty() {
            return fonts;
        }
    }
    let list =
        rpc::call_core("system.fonts", &serde_json::json!({})).unwrap_or(Value::Array(vec![]));
    font_names_from_rpc(&list)
}

/// `system.fonts` 回包 → 下拉用的族名表（去重，按 [`sort_font_names`] 排序）。
///
/// core 的形状是 `[{"family": 英文族名, "display_name": 本地化名}]`（wind-rpc
/// `dispatch.rs`）。此前这里只认 `name` 键——那是 mock 自己编的形状，与 core 从未对上，
/// 于是非 Windows（没有 DirectWrite 可先查的平台）上字体下拉恒为空。
///
/// 存盘取 `display_name`：与 Windows 存 zh-cn 本地化名同一口径；Linux 渲染端按
/// fontconfig 查族名时本地化别名同样认（`wind-ui` `text/linux/fontconfig.rs`
/// `family_exists`）。缺它才退回 `family`。
fn font_names_from_rpc(list: &Value) -> Vec<String> {
    let mut names: Vec<String> = list
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    f.get("display_name")
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| f.get("family").and_then(Value::as_str))
                        .or_else(|| f.as_str())
                        .filter(|s| !s.trim().is_empty())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();
    // core 按英文族名去重，两个族可能共用同一个本地化名。
    names.sort();
    names.dedup();
    sort_font_names(names)
}

/// 从 DirectWrite 本地化名表 `[(locale, 名字)]` 里挑下拉显示 / 存盘用的那个：
/// zh-cn 优先、其次 en-us、都没有取第一条。
///
/// ⚠️ 与 core `wind_config::font_name::pick_localized_name` **同一份规则**（core 渲染端
/// 按 face 名反查 family 时用它报名字）。wind-config 在本仓只是 dev 依赖，发布二进制不带它，
/// 故这里抄一份，由测试 `font_name_pick_matches_core` 钉住两边不漂移。
#[cfg_attr(not(windows), allow(dead_code))]
fn pick_font_display_name(names: &[(String, String)]) -> Option<String> {
    let by = |loc: &str| {
        names
            .iter()
            .find(|(l, n)| l.eq_ignore_ascii_case(loc) && !n.trim().is_empty())
            .map(|(_, n)| n.clone())
    };
    by("zh-cn").or_else(|| by("en-us")).or_else(|| {
        names
            .iter()
            .find(|(_, n)| !n.trim().is_empty())
            .map(|(_, n)| n.clone())
    })
}

/// 字体下拉排序：中文名（含 CJK 字符）排在英文名前面——输入法以中文为主，维护者要求
/// 中文字体优先可见。组内保持原有排序规则（`String` 的默认序）。
///
/// 纯函数，便于单测；`load_fonts_from_dwrite` 去重后调用它排序。
fn sort_font_names(mut names: Vec<String>) -> Vec<String> {
    names.sort_by(|a, b| contains_cjk(b).cmp(&contains_cjk(a)).then_with(|| a.cmp(b)));
    names
}

/// 名字里是否含 CJK 字符（CJK 统一表意文字及扩展 A、兼容表意文字）。
fn contains_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        let c = c as u32;
        (0x4E00..=0x9FFF).contains(&c)
            || (0x3400..=0x4DBF).contains(&c)
            || (0xF900..=0xFAFF).contains(&c)
    })
}

/// DirectWrite 系统字体集的全部 family 名：每族取一个本地化名（见
/// [`pick_font_display_name`]），去重后按 [`sort_font_names`] 排序。
///
/// 存进配置的就是这个名字：`FindFamilyName` 跨全部 locale 匹配，任何一个本地化名都能被
/// 渲染端查回来。
#[cfg(windows)]
fn load_fonts_from_dwrite() -> Vec<String> {
    use windows::Win32::Graphics::DirectWrite::{
        DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection,
        IDWriteLocalizedStrings,
    };

    fn localized(strings: &IDWriteLocalizedStrings) -> Vec<(String, String)> {
        let mut out = Vec::new();
        unsafe {
            for i in 0..strings.GetCount() {
                let loc = strings.GetLocaleNameLength(i).ok().and_then(|len| {
                    let mut buf = vec![0u16; len as usize + 1];
                    strings
                        .GetLocaleName(i, &mut buf)
                        .ok()
                        .map(|_| String::from_utf16_lossy(&buf[..len as usize]))
                });
                let name = strings.GetStringLength(i).ok().and_then(|len| {
                    let mut buf = vec![0u16; len as usize + 1];
                    strings
                        .GetString(i, &mut buf)
                        .ok()
                        .map(|_| String::from_utf16_lossy(&buf[..len as usize]))
                });
                if let (Some(l), Some(n)) = (loc, name) {
                    out.push((l, n));
                }
            }
        }
        out
    }

    unsafe {
        let Ok(factory) = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) else {
            return vec![];
        };
        let mut collection: Option<IDWriteFontCollection> = None;
        if factory
            .GetSystemFontCollection(&mut collection, false)
            .is_err()
        {
            return vec![];
        }
        let Some(collection) = collection else {
            return vec![];
        };
        let mut families = std::collections::BTreeSet::new();
        for i in 0..collection.GetFontFamilyCount() {
            let Ok(names) = collection.GetFontFamily(i).and_then(|f| f.GetFamilyNames()) else {
                continue;
            };
            if let Some(name) = pick_font_display_name(&localized(&names))
                && !name.starts_with('@')
            {
                families.insert(name);
            }
        }
        sort_font_names(families.into_iter().collect())
    }
}
