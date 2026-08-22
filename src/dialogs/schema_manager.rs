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
use crate::pages::config_import::{
    ConfigPatchPreview, PackageInfo, PreviewRow, parse_package_info,
};
use crate::rpc;
use crate::state::AppState;
use crate::widgets::flat_panel;

/// 待导入的分发包来源。zip 与文本信封走**同一个确认对话框**，只在最后执行导入时
/// 分派到各自的 RPC（`package-format.md` §3.4：信封的响应形状与 zip 一致）。
#[derive(Clone)]
enum PendingImport {
    /// zip / `.wpkg` 文件路径。
    Package(std::path::PathBuf),
    /// 文本信封原文（`package.kind = "schema_text"`）。
    Text(String),
}

/// 引导键候选（与临时拼音 / 临时英文 / 快捷输入清单里的那组逐字一致）。
///
/// 只列符号键：字母键作引导键会吞掉以该字母开头的所有编码（`z` 是历史教训），
/// 这里不给这种选项。
pub const TRIGGER_KEY_OPTIONS: &[(&str, &str)] = &[
    ("backtick", "反引号 ( ` )"),
    ("semicolon", "分号 ( ; )"),
    ("quote", "单引号 ( ' )"),
    ("comma", "逗号 ( , )"),
    ("period", "句号 ( . )"),
    ("slash", "斜杠 ( / )"),
    ("backslash", "反斜杠 ( \\ )"),
    ("lbracket", "左方括号 ( [ )"),
    ("rbracket", "右方括号 ( ] )"),
];

/// 把某个动词的绑定键**整体换成** `new_keys`（纯函数，便于单测）。
///
/// 「先摘掉所有指向它的旧键，再写入新键」——只增不减的话，用户取消勾选毫无效果，
/// 且旧键还占着位置让别的模式配不上（一个键只能有一个动词）。
fn apply_verb_rebind(
    map: &mut std::collections::BTreeMap<String, String>,
    verb: &str,
    new_keys: &[String],
) {
    map.retain(|_, v| v.as_str() != verb);
    for k in new_keys {
        let k = k.trim();
        if !k.is_empty() {
            map.insert(k.to_string(), verb.to_string());
        }
    }
}

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
    /// 特殊方案（core `[schema].hidden`）：英文、快符这类。默认不在列表显示，
    /// 由工具栏「显示特殊方案」开关放出来。已启用的即使 hidden 也恒显示——
    /// 用户既然启用了它，藏起来只会让人找不到怎么停用。
    pub hidden: bool,
    /// **overlay 方案**（core 方案文件带 `[overlay]` 段）：可由引导键/直达热键临时叠加
    /// 进入的快符/生僻字那类。
    ///
    /// ⚠️ 与 `hidden` **正交**，不能互推：hidden 只说「不列进方案切换列表」，隐藏的
    /// 码表方案也可能只是 mix 成员、没有 overlay 生命周期。
    /// 判据取自 core 的 `schema.list`，不在本仓再推一遍——跨仓契约无编译期约束。
    pub overlay: bool,
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
    /// 分区标题（自带上方分隔线）。`hidden_with_disabled` 为真时随 hide_disabled 一起隐藏
    /// ——未启用区是这样，特殊方案区不是（它跟「启用」无关）。
    SectionHeader {
        title: String,
        hidden_with_disabled: bool,
    },
    /// 行间分隔线；hidden_with_disabled=true 时随 hide_disabled 一起隐藏。
    Divider { hidden_with_disabled: bool },
}

/// 方案设置对话框的一个配置分节。新增一种方案设置 = 加一个变体 + build_section 分支
/// + [`SettingsSection::tab`] 里指定它归哪个 TAB。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsSection {
    /// 方案元信息头部（名称/类型/来源/版本/作者/描述）。恒为首节。
    Header,
    /// 进入方式：切到该方案的直达热键（`keys.schema_hotkeys`）。所有方案都有。
    EntryKeys,
    ShuangpinLayout,
    /// 辅助码：本方案是否启用（tri-state 覆盖全局基线）。仅**声明了辅助码表**的方案有。
    AuxCode,
    /// 码表配置：总开关 +（开启后）子对话框。仅码表方案有。
    CodeTable,
    /// 特殊模式（overlay）：把本方案变成「引导键临时进入」的快符/生僻字那类。
    /// 仅码表方案有——overlay 的价值在于一张自成体系的小码表。
    Overlay,
    ExtensionDicts,
    /// 节间分隔线（Header 自带结尾分隔，故只在其后的各节之间插入）。
    Divider,
    Empty,
}

/// 方案设置对话框的两个 TAB。
///
/// 曾经是三个（基本信息 / 按键配置 / 方案自定义）。实测下来「按键配置」独立成一页并不好用：
/// 它只有四行，单开一页显得空，而用户从「这个方案是什么」看到「它的键怎么按」本就是
/// 一口气的事，中间隔一次点击反而打断。故并回基本配置。
pub const SETTINGS_TABS: [&str; 2] = ["基本配置", "方案自定义"];

impl SettingsSection {
    /// 本节归到哪个 TAB。
    ///
    /// 分组判据是**用户想做什么**，不是数据落在哪个文件：
    /// - 基本配置：这个方案是什么、键怎么按、带哪些词库——日常会看的都在这一页
    /// - 方案自定义：这个方案的引擎行为要不要偏离全局（双拼布局 / 码表覆盖 / 特殊模式）
    ///
    /// `Divider` 不参与分组：原先那份扁平列表里的分隔线是按「相邻两节之间」插的，分完 TAB
    /// 后相邻关系全变了，故丢弃重插（见 `split_sections_by_tab`）。
    fn tab(self) -> Option<usize> {
        match self {
            Self::Header | Self::EntryKeys | Self::ExtensionDicts => Some(0),
            Self::ShuangpinLayout | Self::AuxCode | Self::CodeTable | Self::Overlay => Some(1),
            Self::Divider | Self::Empty => None,
        }
    }
}

/// 把扁平分节表拆成两个 TAB 各自的列表，并在**组内**重插分隔线。
///
/// ★ 丢弃原有 `Divider` 而不是搬运：它表达的是「相邻两节之间」，分完 TAB 后相邻关系变了，
/// 照搬会出现开头就是一条分隔线、或两节之间没有线。
///
/// `Header` 后面不插：它自带结尾分隔（见 `SettingsSection::Divider` 的文档）。
fn split_sections_by_tab(flat: &[SettingsSection]) -> [Vec<SettingsSection>; 2] {
    let mut out: [Vec<SettingsSection>; 2] = [Vec::new(), Vec::new()];
    for s in flat {
        let Some(t) = s.tab() else { continue };
        let bucket = &mut out[t];
        if let Some(prev) = bucket.last()
            && *prev != SettingsSection::Header
        {
            bucket.push(SettingsSection::Divider);
        }
        bucket.push(*s);
    }
    out
}

/// 辅助码方案级覆盖的三档。`[engine.aux_code].enabled` 是 tri-state `Option<bool>`：
/// `None` 回落全局 `[schema.pinyin.aux_code].enabled`，`Some` 覆盖它。
///
/// ★ 用三档分段而不是复选框：复选框只有两态，表达不出「跟随全局」——而那恰恰是出厂态，
/// 也是绝大多数方案该停在的地方。用复选框就必须替用户在开/关里二选一，一打开设置页
/// 就等于把每个方案都钉死成显式值。
const AUX_CODE_MODES: [&str; 3] = ["跟随全局", "开启", "关闭"];

/// 这个方案**有没有可用的辅助码表**。建节与落盘共用**同一条**判据。
///
/// ★ 判据是 `files` 为非空数组，不是「这个键存在」：`files = []` 是「声明了段但没给表」，
/// 与没声明等价。两处各写一份的后果是它们中间夹着一道缝——建节按非空、落盘按存在，
/// 于是 `files = []` 的方案不出这一节，却会被上一个方案留在信号里的档位写进 override。
///
/// 也不按引擎类型：「这个方案用哪张辅助码表」是方案作者的声明（全拼配笔画、双拼配小鹤
/// 形码），没声明就意味着压根没有可用的表，给它一个开关只会让人开了发现没反应。
///
/// ⚠️ `files` 非空 ≠ 功能开启，那是本节那个开关的事，见 core 的 `AuxCodeSpec`。
fn has_aux_code_table(cfg: &Value) -> bool {
    cfg.pointer("/engine/aux_code/files")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty())
}

fn aux_code_idx(v: Option<bool>) -> usize {
    match v {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    }
}

/// 下标 → 落盘值。`Null` 即「不写这一行」：core 的 `json_to_toml` 跳过 null，于是该键不进
/// override，取值回落全局——这正是「跟随全局」的落地方式，不是丢失。
fn aux_code_value(idx: usize) -> Value {
    match idx {
        1 => json!(true),
        2 => json!(false),
        _ => Value::Null,
    }
}

/// `[overlay] candidate_layout` 的字符串值 ↔ 分段控件下标。
///
/// 值域与 core 的 `LayoutIntent` 逐字一致（`follow` / `vertical` / `horizontal`）——
/// 跨仓契约无编译期约束，拼错的表现是 core 解析回落默认值、用户改了没反应。
const OVERLAY_LAYOUTS: [&str; 3] = ["follow", "vertical", "horizontal"];

fn overlay_layout_idx(v: &str) -> usize {
    OVERLAY_LAYOUTS.iter().position(|x| *x == v).unwrap_or(0)
}

/// 分段控件下标 → `[overlay] candidate_layout` 的字符串值。
///
/// ★ 越界回落**首项**（跟随全局）而不是钳到末项：钳会把用户没选过的「横排」塞给他。
/// 与 [`overlay_layout_idx`] 对未知值的处理、以及 core `layout.rs`「下标越界回落
/// Follow——跟随全局是安全的默认，不猜方向」同口径。
fn overlay_layout_value(idx: usize) -> &'static str {
    OVERLAY_LAYOUTS
        .get(idx)
        .copied()
        .unwrap_or(OVERLAY_LAYOUTS[0])
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

/// 一个方案待提交的 override 改动。
///
/// `cfg` 是**改写后的整份方案配置**而非增量：core 的 `saveConfig` 拿它与方案文件 diff
/// 后全量重写 override，喂增量会把没带上的字段当成「已删除」。
#[derive(Clone, Default)]
struct PendingSchemaEdit {
    cfg: Option<Value>,
    /// 词库开关变更 (dict_id, enabled)。
    dicts: Vec<(String, bool)>,
}

/// 方案管理状态（Signal 为 Copy，可直接 Clone 进闭包）。
#[derive(Clone)]
pub struct SchemaManagerState {
    /// 方案列表原始数据。
    pub schemas: Rc<RefCell<Vec<SchemaEntry>>>,
    /// 已启用方案的有序 ID 列表（对应 schema.available 配置键）。
    /// 空 vec 表示"全部启用"（首次加载且配置为空时的默认状态）。
    pub enabled_order: Rc<RefCell<Vec<String>>>,
    /// 已启用区的行数据（**按启用顺序**）。单独成一个信号是因为这一段是可拖拽重排的
    /// `reorder_list_signal`，而它要求数据即顺序；未启用区/特殊方案区不参与排序，
    /// 仍走 `list_data`。两者都由 rebuild_list() 一并更新。
    pub enabled_rows: Signal<Vec<SchemaEntry>>,
    /// 已启用区**之后**的列表数据（未启用区、特殊方案区，含各自的分区标题与分隔线）；
    /// 任何启用/排序变更后调 rebuild_list() 更新。
    pub list_data: Signal<Vec<SchemaListItem>>,
    /// 是否隐藏未启用方案行（响应式，立即生效）。
    pub hide_disabled: Signal<bool>,
    /// 是否显示特殊方案（core `[schema].hidden`：英文、快符）。默认关。
    /// 列表数据恒含它们（refresh 走 includeHidden），此开关只控制渲染，切换即时生效。
    pub show_hidden: Signal<bool>,
    /// 当前选中方案 ID（工具栏按钮的作用对象；None=无选中）。
    pub selected_id: Rc<RefCell<Option<String>>>,
    /// 选中变更计数：每次变更自增，触发重布局使行高亮/工具栏 enabled_when 重估。
    pub sel_ver: Signal<u64>,
    /// 当前活跃方案 ID（core `schema.active`）。refresh() 时经 RPC 拉运行时值；
    /// 「设为当前」时双写（setActive 即时生效 + 由 state.rs 的 writer 随保存持久化）。
    pub active_id: Rc<RefCell<String>>,
    /// 当前方案变更计数：与 sel_ver 同理，驱动工具栏「设为当前」按钮 enabled_when 重估。
    pub active_ver: Signal<u64>,
    /// 操作反馈（三态 kind + 消息）：经 toast_sink 弹单条 toast，替代原 status 式提示。
    pub feedback: crate::pages::dict::reactive::Feedback,
    pub confirm_visible: Signal<bool>,
    pub confirm_msg: Signal<String>,
    pub confirm_id: Rc<RefCell<String>>,
    // ── 分发包导入确认（previewImport 摘要 + config_patch diff + 合并/替换选择）──
    pub import_confirm_visible: Signal<bool>,
    /// 元信息一行：`方案 <id> v<版本> · <创建时间>`。报表内部用。
    pub import_preview_msg: Signal<String>,
    /// 文件统计一行（只列非零项）。
    pub import_counts_msg: Signal<String>,
    /// 导入报表（富文本）：包名、说明、元信息、文件统计、逐条配置改动合成一份文档，
    /// 层级与配色由报表自己的样式表统一定义。见 [`SchemaManagerState::build_import_report`]。
    pub import_report: Signal<RichDoc>,
    /// 待导入的分发包（zip 路径或信封原文），预览确认后执行导入。
    pending_import: Rc<RefCell<Option<PendingImport>>>,
    /// 包内配置片段的预览结果（`configPatch`），确认后由第二步 `config.applyPatch`
    /// 应用。`None` = 纯方案包，确认对话框不显示「将修改的配置」区块。
    pending_config_patch: Rc<RefCell<Option<ConfigPatchPreview>>>,
    /// 「将修改的配置」区块的逐键 diff 行（动态内容走 Signal）。
    pub import_patch_rows: Signal<Vec<PreviewRow>>,
    /// **包级**说明（响应顶层 title/description，来自 package.toml / 信封
    /// `[package]`），显示在对话框顶部。
    pub import_pkg_title: Signal<String>,
    pub import_pkg_desc: Signal<String>,
    /// **包内片段自己**的说明（`configPatch.info`），显示在「将修改的配置」处。
    ///
    /// 与上面那对分开存而不是合成一份：§2.3 定稿两级说的不是同一件事，缺一方时
    /// 另一方顶上会把「这个包是什么」说成「这段配置改什么」。
    pub import_patch_title: Signal<String>,
    pub import_patch_desc: Signal<String>,
    /// 导入按钮可点：见 [`import_allowed`]（片段有错即整包拒绝）。
    pub import_can_confirm: Signal<bool>,
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
    /// 两个 TAB 各自的分节表（由 [`split_sections_by_tab`] 从扁平表拆出）。
    ///
    /// 拆成三个信号而不是「一个信号 + 渲染时过滤」：`host_signal` 拿到的是整份 Vec、
    /// 对每个元素建一个元素，没有过滤的入口；要过滤就得在数据侧分好。
    pub settings_sections: [Signal<Vec<SettingsSection>>; 2],
    /// 当前选中的 TAB 下标。
    pub settings_tab: Signal<usize>,
    /// 扩展词库行（每行含独立受控开关信号）；ExtensionDicts 分节据此渲染。
    pub settings_dicts: Rc<RefCell<Vec<DictRow>>>,

    // ── 方案直达热键：**没有独立字段**，就存在下面的 `key_actions` 里 ──
    // core 的 `keys.schema_hotkeys` 已废弃并移出登记表（`config.setItems` 不再接受它），
    // 直达热键统一是 `key_actions` 的 `switch_schema:<方案id>`。「进入方式」这一节据此
    // 反查/写入，与「按键功能」那张表共用同一份内存态——**同一个键不可能再被两处各配
    // 一次**，而那正是合并前 `.find()` 先注册者赢、后者静默失效的根源。
    //
    // 两节的分工按**动词**切开，互不越界：本节只增删 `switch_schema:`（单向），
    // 「按键功能」管 `toggle_schema:`（往返）与其余动词。
    /// 全局按键功能表 `keys.key_actions`（键名 → 动词）的内存态。
    ///
    /// 五c 收编后，五处 `trigger_keys` 都折算到了这张表，core 侧 `trigger_keys` 恒为空
    /// （`Config::normalize`）。「进入方式」勾选框若还读写旧字段，用户勾了只是往一个
    /// 没有消费者的字段里写——**配了不生效**。故改为读写本表。
    ///
    /// 用 BTreeMap 而非 HashMap：写回顺序要可复现，否则每次保存 config.toml 的这一段
    /// 都在洗牌，diff 全是噪音。与 core 侧 `keys.key_actions` 的 BTreeMap 也对得上。
    pub key_actions: Rc<RefCell<std::collections::BTreeMap<String, String>>>,
    /// 引导键复选工作态（固定 9 个候选键，与临拼/临英同一组）。
    pub settings_trigger_sigs: Vec<(String, Signal<bool>)>,
    pub settings_trigger_visible: Signal<bool>,
    /// 行内摘要（「未设置」/「已选 N 个」）。与勾选同步更新，不等 commit 落表——
    /// 否则用户勾完看不到变化。
    pub settings_trigger_disp: Signal<String>,

    /// 热键编辑对话框的可见性与工作态（打开方案设置时按当前方案播种）。
    pub settings_hotkey_visible: Signal<bool>,
    pub settings_hotkey_raw: Signal<String>,
    pub settings_hotkey_disp: Signal<String>,
    pub settings_hotkey_enabled: Signal<bool>,
    pub settings_hotkey_mods: [Signal<bool>; 4],
    pub settings_hotkey_key_idx: Signal<usize>,

    // ── 方案级码表配置（`engine.codetable`，见 dialogs::schema_codetable）──
    /// 设置清单：方案级码表配置与全局页**共用同一份条目定义**（label/hint/范围/联动）。
    /// 本对话框不持有 `AppState`，故自带一份内嵌清单。
    pub manifest: Rc<crate::manifest::Manifest>,
    /// 「自定义码表配置」总开关。初值由配置内容推导（`schema_codetable::has_custom`），
    /// 不另存布尔键——多一个键就多一处可能与实际内容不一致的状态。
    // ── 特殊模式（方案文件 `[overlay]` 段）──
    //
    // 本节**只在方案已声明 `[overlay]` 时出现**，故没有「是不是特殊方案」的总开关：
    // 那是方案的固有属性，由方案文件决定。这里只调段内的行为参数。
    /// 进入模式即展示候选（`[overlay] show_all_on_enter`）。
    /// 辅助码方案级覆盖的三档下标（见 [`AUX_CODE_MODES`]）。
    pub settings_aux_idx: Signal<usize>,
    /// 打开时的档位快照，供脏判定。
    settings_aux_at_open: Rc<std::cell::Cell<usize>>,
    pub settings_ov_show_all: Signal<bool>,
    /// 候选排列（0=跟随全局 / 1=竖排 / 2=横排），对应 `[overlay] candidate_layout`。
    pub settings_ov_layout: Signal<usize>,
    /// 打开对话框时的两项初值，用于脏判定——没有它，打开看一眼再关掉也会入队写盘。
    settings_ov_at_open: Rc<std::cell::Cell<(bool, usize)>>,
    pub settings_ct_enabled: Signal<bool>,
    /// 子对话框标题（「码表配置 - 五笔」）：同一个浮层服务于任意方案，
    /// 标题里不带方案名的话，点进去就不知道自己在配哪个表了。
    /// 行内摘要（「跟随全局配置」/「已自定义」）。**定宽**——文本随勾选变化，
    /// 若靠显隐表达状态，右侧的「设置」按钮会跟着左右跳。
    pub settings_ct_disp: Signal<String>,
    /// 各配置项的工作态信号（打开时按生效值播种，保存时落盘）。
    pub settings_ct: crate::dialogs::schema_codetable::CtFields,
    // ── 方案级按键功能表（方案顶层 `key_actions`，见 dialogs::schema_key_actions）──
    pub settings_ka: crate::dialogs::schema_key_actions::KeyActionFields,
    pub settings_ka_visible: Signal<bool>,
    pub settings_ka_title: Signal<String>,
    pub settings_ka_disp: Signal<String>,
    // ── 方案级会话态按键表（方案顶层 `session_actions`，见 dialogs::schema_session_actions）──
    //
    // 与上面那张是对称的两张表：`key_actions` 管无会话态的引导键，本张管**输入中**的
    // 翻页/选词/取消。分开而不是合成一张带状态维度的表，判据见 core 仓
    // `docs/design/session-key-actions.md` §2。
    pub settings_sa: crate::dialogs::schema_session_actions::SchemaSessionActionFields,
    pub settings_sa_visible: Signal<bool>,
    pub settings_sa_title: Signal<String>,
    pub settings_sa_disp: Signal<String>,
    // ── 按键总览（**只读**，见 dialogs::keys_overview）──
    //
    // 不参与 commit：它没有 is_dirty/apply。集中视图若可写回就得决定写哪一层，那是第二个
    // 真相源。判据见 core 仓 `docs/design/key-resolver-unification.md` §4.3。
    pub settings_ko: crate::dialogs::keys_overview::KeysOverview,
    pub settings_ko_visible: Signal<bool>,
    pub settings_ko_title: Signal<String>,
    pub settings_ko_disp: Signal<String>,
    /// 子对话框的渲染计划：对话框结构在选中方案**之前**就建好，信号要等播种，
    /// 中间靠这个信号驱动 `host_signal` 重建。
    pub settings_ct_plan: Signal<Vec<crate::dialogs::schema_codetable::CtItem>>,
    /// 打开时的生效值快照（core 随 `schema.getConfig` 给的 `effectiveCodetable`）。
    /// 保存时用来补齐没有控件的隐藏项。
    pub settings_ct_eff: Rc<RefCell<Value>>,

    /// 打开对话框那一刻的双拼布局索引 / 码表总开关值。脏判定用——没有它就分不清
    /// 「用户拨过」和「本来就是这个值」，于是打开看一眼再关掉也会写一次盘。
    settings_layout_at_open: Rc<std::cell::Cell<usize>>,
    settings_ct_enabled_at_open: Rc<std::cell::Cell<bool>>,
    /// 待提交的方案 override 改动（方案 id → 变更）。关窗时暂存，随主保存提交。
    ///
    /// **按方案 id 存**而不是只留最后一次的工作态：用户可以连着配好几个方案再点
    /// 「应用设置」，只留一份的话先配的那几个会静默丢。
    pending_edits: Rc<RefCell<std::collections::HashMap<String, PendingSchemaEdit>>>,
}

impl SchemaManagerState {
    pub fn new() -> Self {
        let available = Self::read_available();
        let state = Self {
            schemas: Rc::new(RefCell::new(Vec::new())),
            enabled_order: Rc::new(RefCell::new(available)),
            key_actions: Rc::new(RefCell::new(Self::read_key_actions())),
            settings_trigger_sigs: TRIGGER_KEY_OPTIONS
                .iter()
                .map(|(k, _)| (k.to_string(), signal(false)))
                .collect(),
            settings_trigger_visible: signal(false),
            settings_trigger_disp: signal("未设置".to_string()),
            settings_hotkey_visible: signal(false),
            settings_hotkey_raw: signal(String::new()),
            settings_hotkey_disp: signal(String::new()),
            settings_hotkey_enabled: signal(false),
            settings_hotkey_mods: [signal(false), signal(false), signal(false), signal(false)],
            settings_hotkey_key_idx: signal(0),
            enabled_rows: signal(vec![]),
            list_data: signal(vec![]),
            hide_disabled: signal(false),
            show_hidden: signal(false),
            selected_id: Rc::new(RefCell::new(None)),
            sel_ver: signal(0),
            active_id: Rc::new(RefCell::new(String::new())),
            active_ver: signal(0),
            feedback: signal(None),
            confirm_visible: signal(false),
            confirm_msg: signal(String::new()),
            confirm_id: Rc::new(RefCell::new(String::new())),
            import_confirm_visible: signal(false),
            import_preview_msg: signal(String::new()),
            import_counts_msg: signal(String::new()),
            import_report: signal(RichDoc::new()),
            pending_import: Rc::new(RefCell::new(None)),
            pending_config_patch: Rc::new(RefCell::new(None)),
            import_patch_rows: signal(vec![]),
            import_pkg_title: signal(String::new()),
            import_pkg_desc: signal(String::new()),
            import_patch_title: signal(String::new()),
            import_patch_desc: signal(String::new()),
            import_can_confirm: signal(true),
            settings_visible: signal(false),
            settings_entry: Rc::new(RefCell::new(None)),
            settings_schema_id: Rc::new(RefCell::new(String::new())),
            settings_cfg: Rc::new(RefCell::new(Value::Null)),
            settings_is_shuangpin: signal(false),
            settings_layout: signal(0),
            layouts: Rc::new(RefCell::new(Vec::new())),
            settings_sections: [signal(vec![]), signal(vec![])],
            settings_tab: signal(0),
            settings_dicts: Rc::new(RefCell::new(Vec::new())),
            manifest: Rc::new(crate::manifest::Manifest::from_embedded()),
            settings_aux_idx: signal(0),
            settings_aux_at_open: Rc::new(std::cell::Cell::new(0)),
            settings_ov_show_all: signal(false),
            settings_ov_layout: signal(0),
            settings_ov_at_open: Rc::new(std::cell::Cell::new((false, 0))),
            settings_ct_enabled: signal(false),
            settings_ct_disp: signal(String::new()),
            settings_ct: Default::default(),
            settings_ka: Default::default(),
            settings_ka_visible: signal(false),
            settings_ka_title: signal("按键自定义".to_string()),
            settings_ka_disp: signal(String::new()),
            settings_sa: Default::default(),
            settings_sa_visible: signal(false),
            settings_sa_title: signal("输入中按键自定义".to_string()),
            settings_sa_disp: signal(String::new()),
            settings_ko: Default::default(),
            settings_ko_visible: signal(false),
            settings_ko_title: signal("按键总览".to_string()),
            settings_ko_disp: signal(String::new()),
            settings_ct_plan: signal(vec![]),
            settings_ct_eff: Rc::new(RefCell::new(Value::Null)),
            settings_layout_at_open: Rc::new(std::cell::Cell::new(0)),
            settings_ct_enabled_at_open: Rc::new(std::cell::Cell::new(false)),
            pending_edits: Rc::new(RefCell::new(std::collections::HashMap::new())),
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

    /// 读取全局 `keys.key_actions`（键名 → 动词）。
    ///
    /// 这张表是五c 收编后**所有引导键的唯一落点**：core 的 `Config::normalize` 会把
    /// 五处 `trigger_keys` 折算进来并清空原字段，故 `config.get` 拿到的 `trigger_keys`
    /// 恒为空——读那边只会得到一张空表。
    fn read_key_actions() -> std::collections::BTreeMap<String, String> {
        rpc::call_core("config.get", &json!({}))
            .ok()
            .and_then(|v| {
                v.get("keys")
                    .and_then(|k| k.get("key_actions"))
                    .and_then(|m| m.as_object())
                    .map(|o| {
                        o.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                            .collect()
                    })
            })
            .unwrap_or_default()
    }

    /// 指向某个动词的全部键名（如 `special:kf` → `["backslash"]`）。
    pub fn keys_bound_to(&self, verb: &str) -> Vec<String> {
        self.key_actions
            .borrow()
            .iter()
            .filter(|(_, v)| v.as_str() == verb)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// 全局 `keys.key_actions` 的动词表 = 方案级那份 ⊕ 单向 `switch_schema:<id>`。
    ///
    /// ★ **单向切换只在全局合法**。方案级表按活跃方案查，单向切走之后目标方案没有这条
    /// 绑定，那个键就再也按不动了（core 侧 `bound_key_decision` 为此专门让位并 warn）。
    /// 全局表在所有方案下都生效，不存在这个问题，故这里补上、方案级那份仍不列。
    /// 见 core 仓 docs/design/schema-key-actions.md §5。
    ///
    /// ⚠️ 调用前确保方案列表已加载（`refresh()`）：`special:` / `toggle_schema:` /
    /// `switch_schema:` 三类都从 `self.schemas` 枚举，列表空时下拉里就只剩固定动词，
    /// 用户会以为「这个版本不支持切方案」。
    pub(crate) fn build_global_key_action_verbs(
        &self,
    ) -> Vec<crate::dialogs::schema_key_actions::Verb> {
        use crate::dialogs::schema_key_actions::Verb;
        let mut out = self.build_key_action_verbs();
        // 与上面 `toggle_schema` 那段同样跳过 hidden：隐藏方案不进循环切换列表，
        // 给它配直达键属于高级用法，手写配置仍然认（`seed` 会把未知值保留成当前值）。
        for e in self.schemas.borrow().iter() {
            if e.id.is_empty() || e.hidden {
                continue;
            }
            let name = if e.name.is_empty() { &e.id } else { &e.name };
            out.push(Verb {
                value: format!("switch_schema:{}", e.id),
                label: format!("切到方案（单向） - {name}"),
            });
        }
        out
    }

    /// 把某个动词的绑定键**整体换成** `new_keys`：先摘掉所有指向它的旧键，再写入新键。
    ///
    /// 「先摘再写」而不是只增不减——用户取消勾选某个键时，那条绑定必须真的消失。
    /// 只增的话取消勾选毫无效果，且旧键还占着位置让别的模式配不上。
    pub fn rebind_verb(&self, verb: &str, new_keys: &[String]) {
        apply_verb_rebind(&mut self.key_actions.borrow_mut(), verb, new_keys);
    }

    /// 组装按键功能表的动词选项。
    ///
    /// 固定动词 + 实例动词（`mix:<id>` / `special:<id>`）。实例随已安装方案增删，故每次打开
    /// 设置都重新读一遍 `config.get`，不缓存——用户可能刚在别的页加了个特殊方案。
    ///
    /// ⚠️ 首项恒为「不启用」：`Element::dropdown` 的选中值是**索引**，新建行的索引为 0，
    /// 若首项是个真动作，「添加一行还没选」就等于配了那个动作。
    ///
    /// `toggle_schema:<id>` 刻意不列：方案级表里只允许有夺取回路或可往返的动词，
    /// 见 core 仓 docs/design/schema-key-actions.md §5（方案级不允许单向 switch_schema，
    /// 而 toggle_schema 属 C 类、四期才进方案级表）。
    pub(crate) fn build_key_action_verbs(&self) -> Vec<crate::dialogs::schema_key_actions::Verb> {
        use crate::dialogs::schema_key_actions::Verb;
        let mut out = vec![
            Verb {
                value: "none".into(),
                label: "不启用".into(),
            },
            Verb {
                value: "temp_pinyin".into(),
                label: "临时拼音".into(),
            },
            Verb {
                value: "temp_english".into(),
                label: "临时英文".into(),
            },
            // 辅助码：core 的 `BoundAction::AuxCode` 早已接通（`enter_bound_action` 两条
            // 通路都有臂），但此前设置页从没列出来 —— 触发键只能手改配置文件。
            //
            // ⚠️ 全拼方案默认 `separator = "auto"`，反引号恒为音节分隔符，绑在它上面
            // 进不去（core 的 `enter_aux_code` 会 warn 后返回 None）。选别的键。
            Verb {
                value: "aux_code".into(),
                label: "辅助码筛选（拼音候选二次筛选）".into(),
            },
        ];
        let root = rpc::call_core("config.get", &json!({})).ok();
        let arr = |seg: &str| -> Vec<Value> {
            root.as_ref()
                .and_then(|v| v.get("schema"))
                .and_then(|s| s.get(seg))
                .and_then(|a| a.as_array())
                .cloned()
                .unwrap_or_default()
        };
        for m in arr("mix_modes") {
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if id.is_empty() {
                continue;
            }
            // ★ 必须 filter 非空：core 的 MixModeConfig 带 #[serde(default)]，未填的
            // name 序列化成 ""（不是缺失），unwrap_or 对 Some("") 不生效 —— 表现为
            // 下拉里显示「快捷输入 - 」这样的半截标签。
            let name = m
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(id);
            out.push(Verb {
                value: format!("mix:{id}"),
                label: format!("快捷输入 - {name}"),
            });
        }
        // 特殊模式：**实例即方案**，故从方案列表里挑带 `[overlay]` 段的，不再读
        // 已删除的 `schema.special_modes` 数组。`special:<id>` 的 `<id>` 就是方案 id。
        for e in self.schemas.borrow().iter() {
            if !e.overlay || e.id.is_empty() {
                continue;
            }
            let name = if e.name.is_empty() { &e.id } else { &e.name };
            out.push(Verb {
                value: format!("special:{}", e.id),
                label: format!("特殊模式 - {name}"),
            });
        }
        // C 类：切到目标方案，再按回来。**只在修饰键上可用**——它必须在英文模式下也按得动
        // （否则切到英文方案就回不来），而有字符的键走的 keydown 链在英文态根本到不了。
        // 这里照常列出，由 `KeyActionFields` 在行内按所选键给提示（core 侧同样有裁决）。
        //
        // 刻意不提供全局那个单向的 `switch_schema:<id>`：方案级绑定 + 单向切换会锁死用户
        // （A 里配了去 B，B 没配回程 ⇒ 只能去点工具栏）。往返版由运行时来源兜底，
        // 不要求目标方案配对称的绑定。见 docs/design/schema-key-actions.md §5。
        for e in self.schemas.borrow().iter() {
            if e.id.is_empty() || e.hidden {
                continue;
            }
            let name = if e.name.is_empty() { &e.id } else { &e.name };
            out.push(Verb {
                value: format!("toggle_schema:{}", e.id),
                label: format!("切换方案 - {name}"),
            });
        }
        // A 类状态切换。取值必须与 core `BoundAction::DISPATCH_ACTIONS` 逐字一致——
        // 那边是白名单校验，拼错即静默落回「不启用」。
        //
        // `add_word` / `open_add_word_dialog` 不在此列：它们要返回占位 composition
        // 激活 C++ 转发，不走 `dispatch_hotkey`，列出来就是个按了没反应的选项。
        for (value, label) in [
            ("toggle_mode", "中英文切换"),
            ("switch_engine", "循环切换方案"),
            ("toggle_full_width", "全角/半角切换"),
            ("toggle_punct", "中/英标点切换"),
            ("toggle_s2t", "简体/繁体切换"),
            ("toggle_toolbar", "显示/隐藏工具栏"),
            ("open_settings", "打开设置"),
            ("take_screenshot", "截图"),
        ] {
            out.push(Verb {
                value: value.into(),
                label: label.into(),
            });
        }
        out
    }

    /// 用某方案已配的引导键播种复选工作态。
    ///
    /// 与热键工作态同理：这组信号是所有方案复用的，不逐个重置就会把上一个方案的
    /// 勾选带到下一个方案。
    fn seed_trigger_keys(&self, schema_id: &str) {
        let cur = self.keys_bound_to(&format!("special:{schema_id}"));
        for (k, sig) in &self.settings_trigger_sigs {
            sig.set(cur.contains(k));
        }
    }

    /// 把引导键工作态写回**全局 `keys.key_actions`**（内存；随主保存提交）。
    ///
    /// ★ 这里曾是**双写**：除了 key_actions，还要维护一条 `schema.special_modes` 条目，
    /// 因为 `special:<id>` 得指向一个真实存在的实例定义。那条条目随该键删除一并消失——
    /// 实例即方案，`<id>` 就是方案 id，指向的东西本来就一直在。
    /// 于是「用户配一个引导键，config.toml 里凭空长出一段空壳」这个副作用也没了。
    pub fn commit_trigger_keys_edit(&self) {
        let id = self.settings_schema_id.borrow().clone();
        if id.is_empty() {
            return;
        }
        let keys: Vec<String> = self
            .settings_trigger_sigs
            .iter()
            .filter(|(_, s)| s.get())
            .map(|(k, _)| k.clone())
            .collect();
        self.rebind_verb(&format!("special:{id}"), &keys);
    }

    /// 某方案当前已配的引导键条数（行内摘要用）。
    ///
    /// 与 `seed_trigger_keys` 同源查全局表——读旧的 `trigger_keys` 会恒为 0，
    /// 行内摘要显示「未设置」而对话框里明明勾着，两处自相矛盾。
    pub fn trigger_key_count(&self, schema_id: &str) -> usize {
        self.keys_bound_to(&format!("special:{schema_id}")).len()
    }

    /// 某方案当前绑定的直达热键（未绑定返回空串）。
    /// 本方案的直达热键（`key_actions` 里 value == `switch_schema:<本方案>` 的那个键）。
    ///
    /// 反查而非按方案索引：合并后这张表是「键 → 动词」的。理论上可能有多个键指向同一
    /// 方案（手写配置做得到），此时取键名升序的第一个——**必须是确定性的**，否则同一份
    /// 配置每次打开对话框显示的键都可能不同。`key_actions` 是 `BTreeMap`，迭代即有序。
    pub fn hotkey_of(&self, schema_id: &str) -> String {
        let want = format!("switch_schema:{schema_id}");
        self.key_actions
            .borrow()
            .iter()
            .find(|(_, v)| *v == &want)
            .map(|(k, _)| k.clone())
            .unwrap_or_default()
    }

    /// 用某个绑定串播种热键编辑器的工作态。
    ///
    /// 空串 = 未绑定：启用开关关、修饰位清空、主键回到列表首项。这些信号是**对话框
    /// 复用的同一组**，不重置的话打开 A 方案配了键、再打开 B 方案会看到 A 的值。
    fn seed_hotkey_editor(&self, binding: &str) {
        use crate::manifest::{format_hotkey, hotkey_keys, parse_hotkey};
        self.settings_hotkey_enabled.set(!binding.is_empty());
        self.settings_hotkey_raw.set(binding.to_string());
        // format_hotkey 对空串/"none" 已返回「未设置」，无需在此再分支。
        self.settings_hotkey_disp.set(format_hotkey(binding));
        let (mods, key) = parse_hotkey(binding);
        // parse_hotkey 返回的修饰序与对话框的 [Ctrl, Shift, Alt, Win] 一致。
        for (sig, on) in self.settings_hotkey_mods.iter().zip(mods.iter()) {
            sig.set(*on);
        }
        let idx = hotkey_keys()
            .iter()
            .position(|(v, _)| v == &key)
            .unwrap_or(0);
        self.settings_hotkey_key_idx.set(idx);
    }

    /// 把热键编辑器的当前工作态写回 `schema_hotkeys`（内存；随主保存提交）。
    ///
    /// 关掉启用开关 = 移除该方案的条目，而不是写空串——空串会在 config.toml 里留下
    /// 一条 `方案id = ""` 的死条目，core 侧 `parse_hotkey` 虽然会忽略它，但用户翻开
    /// 配置文件会以为自己设过什么。
    pub fn commit_hotkey_edit(&self) {
        let id = self.settings_schema_id.borrow().clone();
        let raw = self.settings_hotkey_raw.get();
        apply_hotkey_edit(
            &mut self.key_actions.borrow_mut(),
            &id,
            self.settings_hotkey_enabled.get(),
            &raw,
        );
    }

    /// 根据当前 schemas + enabled_order 重建 list_data 信号（即时刷新列表）。
    fn rebuild_list(&self) {
        let order = self.enabled_order.borrow().clone();
        let is_all_enabled = order.is_empty();
        let show_hidden = self.show_hidden.get();

        // 三个区，**特殊方案先摘出去**：它没有「启用/未启用」这回事——不进 available、
        // 不参与循环切换，只靠引导键或直达热键进入，所以既不该出现在已启用区、也不该
        // 出现在未启用区（更不该随「隐藏未启用」一起消失，那个开关管的是普通方案）。
        let (special, normal): (Vec<SchemaEntry>, Vec<SchemaEntry>) = self
            .schemas
            .borrow()
            .iter()
            .cloned()
            .partition(|e| e.hidden);

        let mut enabled_entries: Vec<SchemaEntry> = Vec::new();
        if is_all_enabled {
            enabled_entries = normal.clone();
        } else {
            for id in &order {
                if let Some(e) = normal.iter().find(|e| &e.id == id) {
                    enabled_entries.push(e.clone());
                }
            }
        }

        let disabled_entries: Vec<SchemaEntry> = if is_all_enabled {
            vec![]
        } else {
            normal
                .into_iter()
                .filter(|e| !order.contains(&e.id))
                .collect()
        };

        let n_disabled = disabled_entries.len();
        let mut items: Vec<SchemaListItem> = Vec::new();

        // 已启用区独立成一条信号，交给可拖拽重排的列表渲染（行间分隔线做进行内，
        // 见 build_compound_row）。它必须先于 list_data 落值——两者在同一帧被各自的
        // 宿主消费，先后顺序无所谓，但都得在本函数返回前写完。
        self.enabled_rows.set(enabled_entries);

        // 未启用区（SectionHeader 自带上方分隔线，随 hide_disabled 一起隐藏）
        if n_disabled > 0 {
            items.push(SchemaListItem::SectionHeader {
                title: format!("其他已安装方案（共 {n_disabled} 个）"),
                hidden_with_disabled: true,
            });
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

        // 特殊方案区：整区由「显示特殊方案」控制，**不受「隐藏未启用」影响**。
        // 在这里过滤而非靠 visible_when：区标题的计数按 items 数算，隐藏行若仍在数据里，
        // 标题会报出一个比实际可见行数大的数字。
        if show_hidden && !special.is_empty() {
            items.push(SchemaListItem::SectionHeader {
                title: format!("特殊方案（共 {} 个）", special.len()),
                hidden_with_disabled: false,
            });
            for (k, entry) in special.into_iter().enumerate() {
                if k > 0 {
                    items.push(SchemaListItem::Divider {
                        hidden_with_disabled: false,
                    });
                }
                // is_enabled 对特殊方案无意义（行内不渲染启用勾选），传 true 只是让
                // 名称按正常色显示——它们随时可用，不该是「未启用」那种灰。
                items.push(SchemaListItem::Row {
                    entry,
                    is_enabled: true,
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
        // 恒取全集（含特殊方案），显示与否由 `show_hidden` 在渲染层过滤——
        // 开关切换无需重新走 RPC，与 `hide_disabled` 同一套做法。
        let list = rpc::call_core("schema.list", &json!({ "includeHidden": true }))
            .unwrap_or(Value::Array(vec![]));

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

        // 当前活跃方案：取 core 的**运行时**状态（`schema.active` RPC），而非配置里的
        // 同名键——后者只是启动初值，用户可能已在别处（托盘/快捷键）切过方案。
        // 拉不到时保持原值，避免"当前"标记因一次 RPC 抖动而消失。
        if let Some(id) = rpc::call_core("schema.active", &json!({}))
            .ok()
            .and_then(|v| v.get("id").and_then(|x| x.as_str()).map(str::to_string))
        {
            *self.active_id.borrow_mut() = id;
        }

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
                            hidden: s.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false),
                            overlay: s.get("overlay").and_then(|v| v.as_bool()).unwrap_or(false),
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
        // 活跃方案可能在 core 侧被切换过（托盘/快捷键），bump 让工具栏按钮重估。
        self.bump_active();
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

    fn bump_active(&self) {
        self.active_ver.set(self.active_ver.get().wrapping_add(1));
    }

    // ── 当前方案（schema.active） ─────────────────────────────

    /// 该 ID 是否为当前活跃方案（行内「当前」徽章用）。
    pub fn is_active(&self, id: &str) -> bool {
        self.active_id.borrow().as_str() == id
    }

    /// 该 ID 是否处于启用状态。以 enabled_order 为准（含未保存的改动），
    /// 而非 SchemaEntry::enabled——后者是 refresh 时的快照，toggle 后不更新。
    fn is_enabled(&self, id: &str) -> bool {
        schema_is_enabled(&self.enabled_order.borrow(), id)
    }

    /// 选中项能否「设为当前」：有选中 · 已启用 · 且尚不是当前方案。
    pub fn can_set_active(&self) -> bool {
        can_activate(
            &self.enabled_order.borrow(),
            &self.active_id.borrow(),
            self.selected_id.borrow().as_deref(),
        )
    }

    /// core 侧活跃方案已变更（事件驱动，非本端发起）：只同步显示。
    ///
    /// 刻意不复用 [`set_active`]——那会再往 core 发一次 setActive，形成回环。
    /// 本端自己点「设为当前」时 core 也会广播回来，此时 active_id 已是新值，
    /// 首行判断直接短路，天然幂等。
    pub fn set_active_from_core(&self, id: &str) {
        if self.is_active(id) {
            return;
        }
        *self.active_id.borrow_mut() = id.to_string();
        self.bump_active();
        self.rebuild_list();
    }

    /// 将选中方案设为当前（工具栏按钮）。
    pub fn set_active_selected(&self) {
        let Some(id) = self.selected_id.borrow().clone() else {
            return;
        };
        self.set_active(&id);
    }

    /// 将指定方案设为当前输入方案：**即时生效 + 随保存持久化**（双写）。
    ///
    /// 双写是必需的，不是保险：core 的 `EngineManager::switch_schema` 只改运行时的
    /// 活跃方案，**不写配置**；而「应用设置」触发的 `reload_from_config` 会用配置里的
    /// `schema.active` 重置活跃方案。只调 RPC 的话，用户设为当前后再保存一次设置，
    /// 活跃方案就会跳回去。故此处 RPC 让它立刻生效，同时更新 `active_id`——
    /// state.rs 注册的 writer 会在保存时把它写进 `schema.active`。
    pub fn set_active(&self, schema_id: &str) {
        if self.is_active(schema_id) {
            return;
        }
        if !self.is_enabled(schema_id) {
            self.notify(ToastKind::Error, "请先启用该方案，再设为当前");
            return;
        }
        match rpc::call_core("schema.setActive", &json!({ "id": schema_id })) {
            Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => {
                *self.active_id.borrow_mut() = schema_id.to_string();
                self.bump_active();
                self.rebuild_list(); // 「当前」徽章跟随移动
                self.notify(ToastKind::Success, "已切换当前方案");
            }
            // ok=false：core 未能加载该方案（如方案文件损坏，或启用状态尚未保存到
            // 配置、core 侧还不认这个方案）。
            Ok(_) => self.notify(
                ToastKind::Error,
                "切换失败：方案未能加载，若刚启用请先「应用设置」",
            ),
            Err(e) => self.notify(ToastKind::Error, format!("切换失败: {e}")),
        }
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

    /// 选中方案在**已启用区行序**里的位置 (idx, total)；未选中或不在该区返回 None。
    ///
    /// 与 [`selected_enabled_pos`](Self::selected_enabled_pos) 的区别在真相源：那个读
    /// `enabled_order`（可能为空 = 全部启用，也可能含界面上根本不出现的 id），够工具栏
    /// ↑/↓ 判可用性；本函数读 `enabled_rows`——界面上真实那几行，与拖拽重排
    /// [`reorder_enabled`](Self::reorder_enabled) 同一套下标，故算出的位置能直接喂给它。
    pub fn selected_row_pos(&self) -> Option<(usize, usize)> {
        let id = self.selected_id.borrow().clone()?;
        let rows = self.enabled_rows.get();
        let idx = rows.iter().position(|e| e.id == id)?;
        Some((idx, rows.len()))
    }

    /// 把选中方案移到已启用区的首位 / 末位（右键菜单「移到最前 / 最后」）。
    ///
    /// 方案一多，靠 ↑/↓ 一格格挪到头要点十几次——这正是把操作搬进右键菜单要解决的问题，
    /// 只搬「上移/下移」等于把同样的点击次数换个地方点。
    pub fn move_selected_to(&self, to_top: bool) {
        let Some((idx, total)) = self.selected_row_pos() else {
            return;
        };
        let to = if to_top { 0 } else { total.saturating_sub(1) };
        if idx != to {
            self.reorder_enabled(idx, to);
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

    /// 拖拽重排：把已启用区第 `from` 行挪到第 `to` 位（下标都相对**已启用区**）。
    ///
    /// 不直接对 `enabled_order` 用下标——它可能是空的（表示"全部启用"），也可能含着
    /// 已启用区里不出现的 id（特殊方案、已卸载但还留在 available 里的条目）。一律以
    /// `enabled_rows`（界面上真实那几行）为准算出新序，再把 order 里的其余 id 原样接在
    /// 后面：拖一次顺序不该把 available 里的别的条目悄悄抹掉。
    pub fn reorder_enabled(&self, from: usize, to: usize) {
        let visible: Vec<String> = self
            .enabled_rows
            .get()
            .iter()
            .map(|e| e.id.clone())
            .collect();
        let next = {
            let order = self.enabled_order.borrow();
            match reordered_enabled_order(&visible, &order, from, to) {
                Some(v) => v,
                None => return,
            }
        };
        *self.enabled_order.borrow_mut() = next;
        self.notify(ToastKind::Success, "排序已调整（保存后生效）");
        self.rebuild_list();
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
    /// 切换「显示特殊方案」。列表数据本身已含特殊方案（refresh 走 includeHidden），
    /// 这里只需按新值重建 items——过滤在 `rebuild_list` 里做，分节计数才跟得上。
    pub fn set_show_hidden(&self, on: bool) {
        self.show_hidden.set(on);
        self.rebuild_list();
    }

    pub fn toggle_schema_enabled(&self, schema_id: &str, new_enabled: bool) -> bool {
        if new_enabled {
            let mut order = self.enabled_order.borrow_mut();
            if !order.contains(&schema_id.to_string()) {
                order.push(schema_id.to_string());
            }
        } else {
            // 守护：当前方案不可禁用。放在展开/计数守护之前——它的成因更具体，
            // 提示也更有指导性（告诉用户该先做什么，而不是笼统说"至少留一个"）。
            if self.is_active(schema_id) {
                self.notify(ToastKind::Error, "当前方案不可禁用，请先将其他方案设为当前");
                return false;
            }
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
            self.settings_layout_at_open.set(idx);
        }
        *self.settings_schema_id.borrow_mut() = entry.id.clone();
        *self.settings_cfg.borrow_mut() = cfg;
        self.settings_is_shuangpin.set(is_sp);
        *self.settings_entry.borrow_mut() = Some(entry);

        // 直达热键 / 引导键工作态按当前方案播种（对话框复用同一组信号，必须逐个重置）。
        let sid = self.settings_schema_id.borrow().clone();
        let bound = self.hotkey_of(&sid);
        self.seed_hotkey_editor(&bound);
        self.seed_trigger_keys(&sid);

        // ── 计算配置分节（可扩展体系）：Header 恒为首节，其后按方案类型追加配置节 ──
        let mut sections: Vec<SettingsSection> = vec![SettingsSection::Header];
        // 除「进入方式」外还有没有别的配置节——只影响是否显示「暂无其它可配置项」。
        // 进入方式：所有方案都有（切到该方案的直达热键），故紧随 Header。
        sections.push(SettingsSection::EntryKeys);
        if is_sp {
            sections.push(SettingsSection::Divider);
            sections.push(SettingsSection::ShuangpinLayout);
        }
        // 辅助码：**只在方案声明了辅助码表时**建节。判据见 `has_aux_code_table`——
        // 落盘那侧用的是同一个函数，两处必须同判据。
        {
            let cfg = self.settings_cfg.borrow();
            if has_aux_code_table(&cfg) {
                let idx = aux_code_idx(
                    cfg.pointer("/engine/aux_code/enabled")
                        .and_then(|v| v.as_bool()),
                );
                drop(cfg);
                self.settings_aux_idx.set(idx);
                self.settings_aux_at_open.set(idx);
                sections.push(SettingsSection::Divider);
                sections.push(SettingsSection::AuxCode);
            }
        }
        // 码表配置：码表方案才有（含快符这类特殊方案——它们本质就是码表方案，
        // 只是靠引导键进入）。混输方案暂不给：它的码表部分与拼音部分怎么分账是另一件事，
        // core 侧 `effectiveCodetable` 也只对 codetable 类给出。
        {
            let cfg = self.settings_cfg.borrow();
            let is_ct = cfg.pointer("/engine/type").and_then(|v| v.as_str()) == Some("codetable");
            if is_ct {
                let eff = cfg.get("effectiveCodetable").cloned().unwrap_or(json!({}));
                self.settings_ct.seed(&self.manifest, &eff);
                self.settings_ct_plan.set(self.settings_ct.plan());
                let custom = crate::dialogs::schema_codetable::has_custom(&cfg);
                self.settings_ct_enabled.set(custom);
                self.settings_ct_enabled_at_open.set(custom);
                *self.settings_ct_eff.borrow_mut() = eff;
                drop(cfg);
                self.settings_ct_disp.set(
                    ct_summary(self.settings_ct_enabled.get(), self.settings_entry_hidden())
                        .to_string(),
                );
                sections.push(SettingsSection::Divider);
                sections.push(SettingsSection::CodeTable);

                // 特殊模式：**只在方案已声明 `[overlay]` 段时**才建节。
                //
                // ★ 「是不是 overlay 方案」是方案的**固有属性**，由方案文件（作者或手写）
                // 决定，不是普通用户随手勾选的东西。给每个码表方案都挂一节「要不要变成
                // 特殊模式」，只会让常规码表配置变乱——而那是绝大多数人唯一会看的地方。
                // 故这里没有总开关：本节只调 `[overlay]` 段内的行为参数。
                let cfg = self.settings_cfg.borrow();
                let ov = cfg.get("overlay").filter(|v| v.is_object());
                if let Some(ov) = ov {
                    let show_all = ov
                        .get("show_all_on_enter")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let layout = ov
                        .get("candidate_layout")
                        .and_then(|v| v.as_str())
                        .map(overlay_layout_idx)
                        .unwrap_or(0);
                    drop(cfg);
                    self.settings_ov_show_all.set(show_all);
                    self.settings_ov_layout.set(layout);
                    self.settings_ov_at_open.set((show_all, layout));
                    sections.push(SettingsSection::Divider);
                    sections.push(SettingsSection::Overlay);
                }
            }
        }
        // 按键功能表：所有方案都有（按键功能与引擎类型无关）。放在码表配置之后、
        // 扩展词库之前——它同属「这个方案怎么用」，而词库是「这个方案有什么内容」。
        {
            self.settings_ka.set_verbs(self.build_key_action_verbs());
            self.settings_ka.seed(&self.settings_cfg.borrow());
            let who = self
                .settings_entry
                .borrow()
                .as_ref()
                .map(|e| e.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| sid.clone());
            self.settings_ka_title.set(format!("按键自定义 - {who}"));
            self.settings_ka_disp.set(self.settings_ka.summary());
            // 会话态那张同批播种：两张表读的是同一份 cfg，分开播种没有意义，且漏掉一处
            // 的表现是「打开设置页看到的是上一个方案的绑定」。
            self.settings_sa.seed(&self.settings_cfg.borrow());
            self.settings_sa_title
                .set(format!("输入中按键自定义 - {who}"));
            self.settings_sa_disp.set(self.settings_sa.summary());
            // 总览同批播种。数据整份来自 core 的 `keysOverview` 旁路字段，本仓不做仲裁——
            // 全局那份要经 `effective_session_actions` 折算，展开规则只该有一处。
            //
            // ★ 动词表用**全局**那份（`build_global_key_action_verbs`），不是方案级那份。
            // 总览逐行标了「本方案 / 全局」，即它**同时展示两层**；而单向 `switch_schema:`
            // 只在全局表里合法，方案级那份刻意不列它。用方案级表去翻译的表现是：
            // 「切换到本方案按键」配出来的那条（落点正是全局 `switch_schema:<id>`）在总览里
            // 翻不出中文名，显示成原始的 `switch_schema:wubi86`。
            self.settings_ko.seed(
                &self.settings_cfg.borrow(),
                &self.build_global_key_action_verbs(),
            );
            self.settings_ko_title.set(format!("按键总览 - {who}"));
            self.settings_ko_disp.set(self.settings_ko.summary());
            // 不单独成节：内容并进 EntryKeys 那节（组名已改「按键设置」）——
            // 「切换热键 / 引导键 / 按键绑定」三行讲的是同一件事，分成两个组标题
            // 只会让人以为它们互不相干。
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
            sections.push(SettingsSection::Divider);
            sections.push(SettingsSection::ExtensionDicts);
        }
        *self.settings_dicts.borrow_mut() = rows;
        // 注：曾有 `has_config` 与 `SettingsSection::Empty`——「没有任何可配置项」的空状态。
        // 「按键功能」对所有方案都有之后，那个分支恒不可达，故一并退役（枚举项保留，
        // 将来若又出现真的空方案可直接复用）。
        let [basic, custom] = split_sections_by_tab(&sections);
        self.settings_sections[0].set(basic);
        self.settings_sections[1].set(custom);
        // 每次打开都回到首个 TAB：对话框服务于任意方案，停在上一个方案看过的 TAB 上
        // 会让人以为「这个方案也有那些项」，而它可能整个 TAB 都是空的。
        self.settings_tab.set(0);

        self.settings_visible.set(true);
    }

    /// 当前打开的方案是否为特殊方案（决定摘要说「跟随内置基线」还是「跟随全局配置」）。
    fn settings_entry_hidden(&self) -> bool {
        self.settings_entry
            .borrow()
            .as_ref()
            .map(|e| e.hidden)
            .unwrap_or(false)
    }

    /// 把对话框的工作态**暂存**进待提交队列（关窗时调用）。
    ///
    /// 不在这里落盘：方案 override 与 config.toml 里的热键/引导键属于同一次编辑，
    /// 分两个时机写会让用户看到「一半存了一半没存」。统一由「应用设置 / 保存并退出」
    /// 提交，见 [`Self::commit_pending_edits`] 与 `state::SideCommitter`。
    ///
    /// 无改动则不入队——否则打开看一眼再关掉也会让整个设置界面变成「有未保存改动」。
    pub fn stage_settings_edits(&self) {
        let id = self.settings_schema_id.borrow().clone();
        if id.is_empty() {
            return;
        }

        // 扩展词库：仅相对打开时发生变化的行。
        let dicts: Vec<(String, bool)> = self
            .settings_dicts
            .borrow()
            .iter()
            .filter(|r| r.on.get() != r.initial)
            .map(|r| (r.id.clone(), r.on.get()))
            .collect();

        // 方案配置（双拼布局 + 码表配置）：两者必须写进**同一份 cfg**。core 那边是
        // 「整份 cfg 与方案文件 diff，结果全量重写 override」——分两次提交的话，
        // 后一次会拿自己那份 cfg 的 diff 把前一次的结果整个覆盖掉。
        let mut cfg = self.settings_cfg.borrow().clone();
        if !cfg.is_object() {
            cfg = json!({});
        }
        let mut cfg_changed = false;

        if cfg.pointer("/engine/type").and_then(|v| v.as_str()) == Some("codetable") {
            let enabled = self.settings_ct_enabled.get();
            // 脏判定要在 apply 之前：apply 会把所有字段写实，之后再比就永远「有改动」。
            if enabled != self.settings_ct_enabled_at_open.get() || self.settings_ct.is_dirty() {
                // 关掉总开关时写入的是一串 null——core 的 json_to_toml 跳过 null，于是
                // override 里不含这些键 = 跟随基线。这是「取消自定义」唯一的落地方式。
                self.settings_ct.apply(&mut cfg, enabled);
                if enabled {
                    let eff = self.settings_ct_eff.borrow().clone();
                    self.settings_ct.apply_hidden(&mut cfg, &eff);
                }
                cfg_changed = true;
            }
        }

        // 按键功能表：写进同一份 cfg（理由同上——core 是整份 diff 后全量重写 override）。
        // 无总开关，脏判定即内容比对；空表也要写，那是「删光所有绑定」的落地方式。
        if self.settings_ka.is_dirty() {
            self.settings_ka.apply(&mut cfg);
            cfg_changed = true;
        }

        // 会话态按键表：同一份 cfg，同样是内容比对定脏、空表也要写。
        // ⚠️ 两张表必须都在这一次 saveConfig 里落地：core 是整份 cfg 与方案文件 diff 后
        // 全量重写 override，分两次提交会互相覆盖（后一次的基线里没有前一次的改动）。
        if self.settings_sa.is_dirty() {
            self.settings_sa.apply(&mut cfg);
            cfg_changed = true;
        }

        // 辅助码方案级覆盖：与其余各节写进**同一份 cfg**，理由同上。
        // 只在本节出现过（`at_open` 被播种过）且档位变了时才写——否则给没有辅助码表的方案
        // 也塞一个 `engine.aux_code` 段，那会让 `json_diff` 认定方案文件缺这个键，把整份
        // 快照写进 override。
        if self.settings_aux_idx.get() != self.settings_aux_at_open.get()
            && has_aux_code_table(&cfg)
        {
            set_json_path(
                &mut cfg,
                &["engine", "aux_code", "enabled"],
                aux_code_value(self.settings_aux_idx.get()),
            );
            cfg_changed = true;
        }

        // 特殊模式（`[overlay]` 段）：与码表配置写进**同一份 cfg**，理由同上。
        // 仅当方案已声明该段时才有工作态可提交（无该段时本节根本不出现）。
        {
            let now = (
                self.settings_ov_show_all.get(),
                self.settings_ov_layout.get(),
            );
            if now != self.settings_ov_at_open.get()
                && cfg.get("overlay").is_some_and(|v| v.is_object())
            {
                // ★ **在原对象上改这两个字段，不重建整个 overlay**。段里还可能有本 UI 不管的
                // 项（`kind`、`comment_template_vertical/_horizontal`，以及将来新增的）——
                // 用结构体/字面量重建会把它们静默抹掉，用户改一次候选排列就丢配置。
                // 这正是已删除的 `special_modes.rs` 当年那条教训的同形。
                if let Some(ov) = cfg.get_mut("overlay").and_then(|v| v.as_object_mut()) {
                    ov.insert("show_all_on_enter".into(), json!(now.0));
                    ov.insert(
                        "candidate_layout".into(),
                        json!(overlay_layout_value(now.1)),
                    );
                }
                cfg_changed = true;
            }
        }

        if self.settings_is_shuangpin.get()
            && self.settings_layout.get() != self.settings_layout_at_open.get()
            && !self.layouts.borrow().is_empty()
        {
            let layout = {
                let layouts = self.layouts.borrow();
                let idx = self.settings_layout.get().min(layouts.len() - 1);
                layouts[idx].0.clone()
            };
            set_json_path(
                &mut cfg,
                &["engine", "pinyin", "shuangpin", "layout"],
                json!(layout),
            );
            cfg_changed = true;
        }

        if dicts.is_empty() && !cfg_changed {
            return;
        }
        merge_pending(
            &mut self.pending_edits.borrow_mut(),
            &id,
            cfg_changed.then_some(cfg),
            dicts,
        );
    }

    /// 是否有待提交的方案 override 改动（供主保存的 `is_dirty` 汇总）。
    pub fn has_pending_edits(&self) -> bool {
        !self.pending_edits.borrow().is_empty()
    }

    /// 提交待提交队列（由主保存调用）。
    pub fn commit_pending_edits(&self) -> Result<(), String> {
        drain_pending(&self.pending_edits, |id, edit| {
            for (dict_id, want) in &edit.dicts {
                rpc::call_core(
                    "schema.setDictEnabled",
                    &json!({ "id": id, "dictId": dict_id, "enabled": *want }),
                )
                .map_err(|e| format!("扩展词库保存失败（{id}）: {e}"))?;
            }
            if let Some(cfg) = &edit.cfg {
                rpc::call_core("schema.saveConfig", &json!({ "id": id, "cfg": cfg }))
                    .map_err(|e| format!("方案配置保存失败（{id}）: {e}"))?;
            }
            Ok(())
        })?;
        // 行名可能含双拼布局后缀，提交后重拉列表刷新显示（refresh 保留选中）。
        self.refresh();
        Ok(())
    }

    /// 丢弃待提交队列（「重新加载」时调用——那是明确的「丢弃未保存改动」操作）。
    pub fn clear_pending_edits(&self) {
        self.pending_edits.borrow_mut().clear();
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
                "确定要删除方案「{display_name}」吗？\n将同时删除其词库数据与未被其它方案共享的资源文件，不可恢复。"
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
            Ok(v) => {
                // refresh 内会发现选中 ID 已消失并清空选中。
                self.refresh();
                let n = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0)
                };
                let (deleted, kept) = (n("deleted"), n("keptShared"));
                let msg = if kept > 0 {
                    format!("方案已删除（清理 {deleted} 个文件，共享保留 {kept} 个）")
                } else if deleted > 0 {
                    format!("方案已删除（清理 {deleted} 个文件）")
                } else {
                    "方案已删除".to_string()
                };
                self.notify(ToastKind::Success, msg);
            }
            Err(e) => self.notify(ToastKind::Error, format!("删除失败: {e}")),
        }
    }

    // ── 方案包导入导出（scheme.exportPackage/previewImport/importPackage）──

    /// 导出选中方案为方案包 zip。
    pub fn do_export_selected(&self, ctx: &mut EventCtx) {
        let Some(entry) = self.selected_entry() else {
            self.notify(ToastKind::Error, "请先选中方案");
            return;
        };
        self.export_schema(ctx, &entry.id);
    }

    /// 导出指定方案为方案包 zip：弹保存框取落点 → `scheme.exportPackage` → 报结果。
    ///
    /// 走 `ctx.defer_blocking` 而非 `ctx.request_save_file`：两者时机语义相同（都在事件
    /// 分发完全返回后执行，绝不在回调栈内同步弹原生模态框），但前者把"选路径 + 调用
    /// core + 报结果"整条流程留在一处，不必把后半段拆进对话框回调里。
    /// windui 0.12 起菜单项动作也收 `&mut EventCtx`，故工具栏按钮与右键菜单项仍共用
    /// **同一条**路径（此前是靠已废弃的自由函数 `app::defer_blocking` 才做到的）。
    pub fn export_schema(&self, ctx: &mut EventCtx, schema_id: &str) {
        let entry = {
            let schemas = self.schemas.borrow();
            let Some(e) = schemas.iter().find(|e| e.id == schema_id) else {
                return;
            };
            e.clone()
        };
        // 文件名带方案版本便于区分（schema.toml 未标版本时省略）。
        let file_name = if entry.version.is_empty() {
            format!("{}.zip", entry.id)
        } else {
            format!("{}-{}.zip", entry.id, entry.version)
        };
        let state = self.clone();
        ctx.defer_blocking(move || {
            let Some(path) = PickDialog::new()
                .title("导出方案包")
                .file_name(&file_name)
                .filter("方案包", &["zip"])
                .save_file()
            else {
                return;
            };
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
        });
    }

    /// 导入方案包：选 zip → `scheme.previewImport` 预览 → 确认对话框选合并/替换。
    /// 延迟通道的选型同 [`export_schema`](Self::export_schema)。
    pub fn do_import_package(&self, ctx: &mut EventCtx) {
        let state = self.clone();
        ctx.defer_blocking(move || {
            let Some(path) = PickDialog::new()
                .title("导入方案包")
                .filter("方案包", &["zip"])
                .pick_file()
            else {
                return;
            };
            match rpc::call_core(
                "scheme.previewImport",
                &json!({ "path": path.to_string_lossy() }),
            ) {
                Ok(v) => state.open_import_confirm(path, &v),
                Err(e) => state.notify(ToastKind::Error, format!("预览失败: {e}")),
            }
        });
    }

    /// 打开导入确认对话框（`scheme.previewImport` 已由调用方完成）。
    /// 统一导入分派器（高级页「导入文件…」）复用此入口，路由不重写域逻辑。
    pub fn open_import_confirm(&self, path: std::path::PathBuf, preview: &Value) {
        self.open_import(PendingImport::Package(path), preview);
    }

    /// 打开导入确认对话框——文本信封分支（`scheme.previewImportText` 已由调用方完成，
    /// 见 `config_import::ConfigImportState::validate`）。与 zip 共用同一个对话框：
    /// 响应形状一致，用户看到的确认内容也就该一致（`package-format.md` §3.4）。
    pub fn open_import_text_confirm(&self, text: String, preview: &Value) {
        self.open_import(PendingImport::Text(text), preview);
    }

    /// 两个分支的共同落点：摘要 + 两级说明 + config_patch 逐键 diff + 导入门控。
    fn open_import(&self, pending: PendingImport, preview: &Value) {
        self.import_preview_msg
            .set(Self::format_import_preview(preview));
        self.import_counts_msg
            .set(Self::format_import_counts(preview));
        let patch = crate::pages::config_import::parse_config_patch(preview);
        self.import_report
            .set(Self::build_import_report(preview, patch.as_ref()));
        self.import_patch_rows
            .set(patch.as_ref().map(|p| p.rows.clone()).unwrap_or_default());
        // 两级说明各取各的源：包级在响应顶层，片段的在 configPatch.info 里。
        // 恒设值（含空串），否则上一个包的说明会留在下一个包的对话框上。
        self.set_import_info(
            &parse_package_info(preview),
            patch.as_ref().map(|p| &p.info),
        );
        self.import_can_confirm.set(import_allowed(patch.as_ref()));
        *self.pending_config_patch.borrow_mut() = patch;
        *self.pending_import.borrow_mut() = Some(pending);
        self.import_confirm_visible.set(true);
    }

    /// previewImport 响应 → 一份**导入报表**（富文本）。
    ///
    /// 此前是六个控件各管一段（包名 / 说明 / 元信息 / 统计 / 片段说明 / diff 列表），
    /// 每段自带字号与配色，凑在一起谁也不比谁显眼。改用单份 `RichDoc` 之后，
    /// 层级由**一张样式表**统一定义，读者扫一眼就能分出「标题 / 正文 / 次要信息 /
    /// 警示」，而不必逐段辨认。
    ///
    /// 分段与顺序即阅读顺序：**这是什么 → 装哪些文件 → 改哪些配置 → 有无问题**。
    fn build_import_report(preview: &Value, patch: Option<&ConfigPatchPreview>) -> RichDoc {
        let pkg = parse_package_info(preview);
        let mut doc = RichDoc::new()
            .style("h1", SpanStyle::new().size(15.0).bold())
            .style("h2", SpanStyle::new().size(13.0).bold())
            .style("body", SpanStyle::new().size(12.5))
            .style("muted", SpanStyle::new().size(12.0).fg(RichColor::Muted))
            .style("key", SpanStyle::new().size(12.0).bold())
            .style("from", SpanStyle::new().size(12.0).fg(RichColor::Muted))
            .style("to", SpanStyle::new().size(12.0).fg(RichColor::Accent))
            .style("bad", SpanStyle::new().size(12.0).fg(RichColor::Danger))
            .style(
                "badge",
                SpanStyle::new()
                    .size(11.0)
                    .bold()
                    .chip()
                    .fg(RichColor::Muted),
            );

        // ① 这是什么：包名（缺说明时回落到方案 id，标题栏不能空着）
        let head = if pkg.title.trim().is_empty() {
            Self::format_import_preview(preview)
        } else {
            pkg.title.clone()
        };
        doc = doc.para(Para::new().styled("h1", head));
        if !pkg.description.trim().is_empty() {
            // 说明可长达 4000 字符：默认排 4 行，末尾缀「… 展开」由用户决定看不看，
            // 不能让分发者的一段长文把后面的改动清单挤出视野。
            doc = doc.para(
                // 不截断：说明就是分发者要用户读的那段话，折起来等于劝人别读。
                // 长了由外层滚动区兜底。
                Para::new().styled("body", pkg.description.trim()),
            );
        }

        // ② 装哪些文件：元信息与统计各一行，弱化——核对用，不抢注意力
        doc = doc
            .para(Para::new().styled("muted", Self::format_import_preview(preview)))
            .para(Para::new().styled("muted", Self::format_import_counts(preview)));

        // ③ 改哪些配置：只有带 config_patch 的包才有这一节
        if let Some(p) = patch {
            doc = doc.divider().para(Para::new().styled("h2", "将修改的配置"));
            if !p.info.title.trim().is_empty() {
                doc = doc.para(Para::new().styled("body", p.info.title.trim()));
            }
            if !p.info.description.trim().is_empty() {
                doc = doc.para(Para::new().styled("muted", p.info.description.trim()));
            }
            for row in &p.rows {
                // 键与取值分行：键名可长达数十字符，与取值挤一行必然折行错位
                // （宽度分配那个坑已经踩过一次）。悬挂缩进让续行对齐到取值列。
                doc = doc.para(Para::new().styled("key", &row.key).indent(4));
                doc = doc.para(
                    Para::new()
                        .styled(if row.is_error { "bad" } else { "to" }, &row.detail)
                        .indent(18),
                );
            }
            if !p.ok {
                doc =
                    doc.para(Para::new().styled("bad", "配置片段有无法应用的项，整个包已拒绝导入"));
            }
        }
        doc
    }

    /// 两级说明取值（`None` / 空 `PackageInfo` 即清空，对应区块随之不占位）。
    fn set_import_info(&self, pkg: &PackageInfo, patch: Option<&PackageInfo>) {
        let empty = PackageInfo::default();
        let patch = patch.unwrap_or(&empty);
        self.import_pkg_title.set(pkg.title.clone());
        self.import_pkg_desc.set(pkg.description.clone());
        self.import_patch_title.set(patch.title.clone());
        self.import_patch_desc.set(patch.description.clone());
    }

    /// ISO 8601 时间戳 → `YYYY-MM-DD HH:MM`。
    ///
    /// 包里存的是 `2026-08-20T15:30:00+08:00` 这种机器格式，原样摆进对话框又长又吵，
    /// 而用户只关心「哪天打的包」。不解析时区、不做换算——只截取，因为**要显示的正是
    /// 打包者当地的时间**，换算到本地反而让「作者说的 15:30」对不上号。
    /// 认不出格式就原样返回，不猜。
    fn format_created_at(raw: &str) -> String {
        let (date, rest) = match raw.split_once('T') {
            Some(v) => v,
            None => return raw.to_string(),
        };
        let hhmm: String = rest.chars().take(5).collect();
        if date.len() == 10 && hhmm.len() == 5 && hhmm.as_bytes()[2] == b':' {
            format!("{date} {hhmm}")
        } else {
            raw.to_string()
        }
    }

    /// previewImport 响应 → 文件统计行。**只列非零项**：一串「系统引用 0 · 缺失 0」
    /// 占着位置却不含信息，真正要紧的「已存在 2」反而被淹没。全为零时说明包是空的。
    fn format_import_counts(v: &Value) -> String {
        let n = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_array())
                .map(|a| a.len())
                .unwrap_or(0)
        };
        let parts: Vec<String> = [
            ("新增文件", n("willAdd")),
            ("已存在", n("conflicts")),
            ("系统引用", n("systemRefs")),
            ("缺失", n("missing")),
        ]
        .iter()
        .filter(|(_, c)| *c > 0)
        .map(|(label, c)| format!("{label} {c}"))
        .collect();
        if parts.is_empty() {
            "包内没有方案文件".to_string()
        } else {
            parts.join(" · ")
        }
    }

    /// previewImport 响应 → 确认对话框正文（纯函数，便于单测）。
    /// v2 契约：元信息在可选 `package`（package.toml 缺失时各字段为空串）。
    fn format_import_preview(v: &Value) -> String {
        let pkg = v.get("package");
        let s = |a: &str, b: &str| {
            pkg.and_then(|x| x.get(a))
                .and_then(|x| x.get(b))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string()
        };
        let (id, version) = (s("schema", "id"), s("schema", "version"));
        let head = match (id.is_empty(), version.is_empty()) {
            (false, false) => format!("方案 {id} v{version}"),
            (false, true) => format!("方案 {id}"),
            _ => "方案包".to_string(),
        };
        let created = s("package", "created_at");
        let created = if created.is_empty() {
            "创建时间未知".to_string()
        } else {
            Self::format_created_at(&created)
        };
        // 只出「方案 + 时间」这一行。文件统计走 [`Self::format_import_counts`]、
        // 合并/替换的差别贴在按钮旁——三类信息挤成一段多行文本时，
        // 谁也不比谁显眼，用户得逐行读才知道哪句和自己有关。
        format!("{head} · {created}")
    }

    /// 确认导入（strategy: "merge" | "replace"），两步编排（`package-format.md` §3.3）：
    /// ① 写方案文件——zip 走 `scheme.importPackage`、信封走 `scheme.importText`；
    /// ② 包若带 config_patch 且预览通过 → `config.applyPatch` 应用片段。
    ///
    /// 返回 applyPatch 的 `written`（落盘后的最终键值）供调用方回灌配置镜像；无片段
    /// 或第一步失败时为空。
    ///
    /// **两步之间非原子**是规格定稿的取舍：文件已装而片段失败时如实报错（方案已导入、
    /// 配置未应用），**不回滚文件**——patch 管线的热重载与事件广播住在 RPC 分发层，
    /// 在文件层复刻它们是第二份真相源。
    pub fn confirm_import(&self, strategy: &str) -> Vec<(String, Value)> {
        self.import_confirm_visible.set(false);
        let patch = self.pending_config_patch.borrow_mut().take();
        let Some(pending) = self.pending_import.borrow_mut().take() else {
            return Vec::new();
        };
        let (method, params) = match &pending {
            PendingImport::Package(path) => (
                "scheme.importPackage",
                json!({ "path": path.to_string_lossy(), "strategy": strategy }),
            ),
            PendingImport::Text(text) => (
                "scheme.importText",
                json!({ "text": text, "strategy": strategy }),
            ),
        };
        let v = match rpc::call_core(method, &params) {
            Ok(v) => v,
            Err(e) => {
                self.notify(ToastKind::Error, format!("导入失败: {e}"));
                return Vec::new();
            }
        };
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

        // 第二步：配置片段。预览不通过的包在对话框里就点不了导入（见 import_allowed），
        // 这里的 `p.ok` 是同一判据的兜底。
        let Some(patch) = patch.filter(|p| p.ok) else {
            self.notify(ToastKind::Success, msg);
            return Vec::new();
        };
        match rpc::call_core("config.applyPatch", &json!({ "text": patch.text })) {
            Ok(r) => {
                let applied = r.get("applied").and_then(|x| x.as_u64()).unwrap_or(0);
                let restart = r
                    .get("needsRestart")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                let tail = if restart {
                    format!("，配置已应用 {applied} 项（部分设置重启后生效）")
                } else {
                    format!("，配置已应用 {applied} 项")
                };
                self.notify(ToastKind::Success, format!("{msg}{tail}"));
                crate::pages::config_import::parse_written(&r)
            }
            Err(e) => {
                self.notify(
                    ToastKind::Error,
                    format!("方案已导入，配置片段应用失败: {e}"),
                );
                Vec::new()
            }
        }
    }

    pub fn cancel_import(&self) {
        *self.pending_import.borrow_mut() = None;
        *self.pending_config_patch.borrow_mut() = None;
        self.import_patch_rows.set(Vec::new());
        self.set_import_info(&PackageInfo::default(), None);
        self.import_confirm_visible.set(false);
    }
}

// ──────────────────────── 启用/当前 判定（纯函数） ────────────────────────

/// 导入确认对话框的「合并/替换导入」是否可点。
///
/// 包不带 config_patch 时恒可点；带则要求片段**整体**通过——预览有任一错误条目即
/// 整包拒绝（`package-format.md` §3.3：分发侧应出厂前测过）。半装一个包比不装更糟：
/// 方案文件已落盘、配置却没跟上，用户看到的是一个行为对不上说明的方案。
fn import_allowed(patch: Option<&ConfigPatchPreview>) -> bool {
    match patch {
        None => true,
        Some(p) => p.ok,
    }
}

/// 方案是否启用。**空 order 表示「全部启用」**——这是全仓一致的语义
/// （见 `rebuild_list` / `toggle_schema_enabled`），漏掉它会让首次加载、
/// 配置为空时所有方案都被判成未启用。
fn schema_is_enabled(order: &[String], id: &str) -> bool {
    order.is_empty() || order.iter().any(|x| x == id)
}

/// 选中项能否「设为当前」：有选中 · 已启用 · 且尚不是当前方案。
fn can_activate(order: &[String], active: &str, selected: Option<&str>) -> bool {
    let Some(id) = selected else { return false };
    id != active && schema_is_enabled(order, id)
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

/// 把一次「进入方式」的热键编辑落到 `key_actions`（键 → 动词）上（纯函数，供测试）。
///
/// # 只碰 `switch_schema:` 这一个动词
///
/// 同一张表里还躺着「按键功能」配的 `toggle_schema:`（往返）、`special:`（overlay）
/// 以及别的方案的绑定。**先按动词+方案精确清掉旧条目，再插新的**——不能按方案粗暴清、
/// 更不能整表重建：那会把用户在另一节配的往返绑定连带抹掉，而他只是在这一节改了个键。
///
/// # 未绑定 = 表里没有这一条
///
/// 而不是一条空串。写空串会在 config.toml 里留下死条目：core 侧 `parse_hotkey` 确实
/// 会忽略它，但用户翻开配置看见自己的方案挂在那儿，只会以为设过什么却没生效。
///
/// `raw` 为空（用户在对话框里把主键选成「（未设置）」）同样按解绑处理——
/// 只看 `enabled` 会写进一条 `"none"` 或空串。
fn apply_hotkey_edit(
    map: &mut std::collections::BTreeMap<String, String>,
    schema_id: &str,
    enabled: bool,
    raw: &str,
) {
    if schema_id.is_empty() {
        return;
    }
    let verb = format!("switch_schema:{schema_id}");
    // 旧条目可能不止一条（手写配置能给同一方案配多个键），全部清掉再插新的，
    // 否则改键之后旧键仍然生效——「改了没反应」的又一种形态。
    map.retain(|_, v| v != &verb);
    if enabled && !raw.is_empty() && raw != "none" {
        map.insert(raw.to_string(), verb);
    }
}

pub fn engine_label(t: &str) -> &'static str {
    match t {
        "codetable" => "码表",
        "pinyin" => "拼音",
        "mixed" => "混输",
        "english" => "英文",
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
/// 分成两段：
/// - **已启用区**用 `Element::reorder_list_signal`——按住行首手柄上下拖即可调优先级，
///   顺序的真相源是 `enabled_rows` 信号，落定后经 `reorder_enabled` 写回 `enabled_order`。
///   只有这一段可拖：未启用方案没有"优先级"可言，特殊方案更不进循环切换，把它们混进
///   同一个可拖列表只会让用户拖出无意义（且要被 rebuild 打回原形）的落点。
/// - **其余区**（未启用、特殊方案）仍用 `Element::host_signal` 按 `list_data` 渲染。
///
/// 两段都**按自然高度展开**，随外层页面滚动一起滚动——刻意不用 `list_signal`
///（自带固定高度滚动容器）：设置页本身已是滚动容器，内嵌固定高度的内层滚动会造成
/// "滚轮在内外层间打架、列表滚不动"，且内层滚动条压在卡片行上遮挡内容。
pub fn build_schema_inline_list(state: &SchemaManagerState) -> Element {
    let mgr = state.clone();
    let hide_sig = state.hide_disabled;

    let mgr_drag = state.clone();
    let mgr_move = state.clone();
    let enabled =
        Element::reorder_list_signal(state.enabled_rows, move |entry: SchemaEntry, handle| {
            build_draggable_row(entry, &mgr_drag, handle)
        })
        .on_reorder(move |_ctx, from, to| mgr_move.reorder_enabled(from, to));

    let rest = Element::host_signal(state.list_data, move |item| match item {
        SchemaListItem::Row { entry, is_enabled } => {
            build_compound_row(entry, is_enabled, &mgr, None)
        }
        SchemaListItem::SectionHeader {
            title,
            hidden_with_disabled,
        } => {
            let el = Element::col()
                .width_match()
                .child(Element::divider())
                .child(
                    Element::row().width_match().padding_xy(8, 5).child(
                        Element::label(title)
                            .font_size(10.5)
                            .fg_role(Role::TextMuted)
                            .width_match()
                            .max_lines(1),
                    ),
                );
            if hidden_with_disabled {
                el.visible_when(move || !hide_sig.get())
            } else {
                el
            }
        }
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
    .width_match();

    Element::col().width_match().child(enabled).child(rest)
}

/// 方案行的右键菜单。**先把该行设为选中**，再按选中态复用工具栏那套判定
///（`can_set_active` / `selected_row_pos` / `selected_is_user`）——右击即选中是桌面惯例，
/// 也让菜单与工具栏共享同一个"作用对象"模型，不必为菜单另写一套按 id 的判定。
///
/// 项的启用态在**每次右击时**现算（`on_context_menu` 的构建器每次都跑），故"设为当前"
/// 在当前方案上是灰的、"上移"在首行是灰的，都无需额外的响应式包装。
fn row_menu_items(mgr: &SchemaManagerState, entry: &SchemaEntry) -> Vec<MenuItem> {
    mgr.select(&entry.id);

    let mut items = Vec::new();

    // 特殊方案（英文/快符）不进 available、不参与循环切换，故没有「设为当前 / 启用 / 排序」
    // 这几件事可做——列出来只会造出一个用户点了也不知道发生什么的状态。
    if !entry.hidden {
        let m = mgr.clone();
        items.push(
            MenuItem::run("设为当前", move |_ctx| m.set_active_selected(), false)
                .enabled(mgr.can_set_active()),
        );

        let enabled = mgr.is_enabled(&entry.id);
        let (m, id) = (mgr.clone(), entry.id.clone());
        items.push(
            MenuItem::run(
                "启用",
                move |_ctx| {
                    m.toggle_schema_enabled(&id, !enabled);
                },
                enabled,
            )
            // 当前方案不可禁用（toggle 里会拒并弹提示）。菜单里直接置灰更早一步说清楚。
            .enabled(!enabled || !mgr.is_active(&entry.id)),
        );

        // 排序仅对已启用区有意义：未启用方案不在循环里，没有"优先级"可言。
        if let Some((idx, total)) = mgr.selected_row_pos() {
            items.push(MenuItem::separator());
            let m = mgr.clone();
            items.push(
                MenuItem::run("上移", move |_ctx| m.move_selected(true), false)
                    .shortcut("↑")
                    .enabled(idx > 0),
            );
            let m = mgr.clone();
            items.push(
                MenuItem::run("下移", move |_ctx| m.move_selected(false), false)
                    .shortcut("↓")
                    .enabled(idx + 1 < total),
            );
            let m = mgr.clone();
            items.push(
                MenuItem::run("移到最前", move |_ctx| m.move_selected_to(true), false)
                    .enabled(idx > 0),
            );
            let m = mgr.clone();
            items.push(
                MenuItem::run("移到最后", move |_ctx| m.move_selected_to(false), false)
                    .enabled(idx + 1 < total),
            );
        }
    }

    if !items.is_empty() {
        items.push(MenuItem::separator());
    }
    let m = mgr.clone();
    items.push(MenuItem::run(
        "方案设置…",
        move |_ctx| m.open_settings_selected(),
        false,
    ));
    let (m, id) = (mgr.clone(), entry.id.clone());
    items.push(MenuItem::run(
        "导出方案包…",
        // 菜单动作的 `ctx` 正是这里要用的：导出要弹原生保存框，必须排到事件分发
        // 完全返回之后（`export_schema` 内走 `ctx.defer_blocking`）。
        move |ctx| m.export_schema(ctx, &id),
        false,
    ));
    items.push(MenuItem::separator());
    let m = mgr.clone();
    items.push(
        MenuItem::run("删除", move |_ctx| m.request_delete_selected(), false)
            // 与工具栏「删除」按钮的 danger 描边同语义：菜单里各项长得一样，
            // 颜色是唯一能在扫读时把不可逆操作与旁边那些拦开的信号。
            .danger()
            .enabled(entry.source == "user"),
    );
    items
}

/// 方案列表**空白处**的右键菜单：作用于整个列表而非某一行。
///
/// 挂在工具栏+列表那个外层容器上，故行上右击命中行菜单（更靠近命中点的节点先接），
/// 落在列表下方空白、工具栏空隙才落到这里。
pub fn list_menu_items(
    mgr: &SchemaManagerState,
    on_hide_disabled: Rc<dyn Fn(bool)>,
    on_show_hidden: Rc<dyn Fn(bool)>,
) -> Vec<MenuItem> {
    let m = mgr.clone();
    let m_r = mgr.clone();
    let (hide, show) = (mgr.hide_disabled.get(), mgr.show_hidden.get());
    let (m_h, m_s) = (mgr.clone(), mgr.clone());
    vec![
        MenuItem::run("导入方案包…", move |ctx| m.do_import_package(ctx), false),
        MenuItem::run("刷新列表", move |_ctx| m_r.refresh(), false),
        MenuItem::separator(),
        // 粘滞项：连点两个过滤开关不必反复右击。勾选态每次右击重建时现取。
        MenuItem::run(
            "隐藏未启用",
            move |_ctx| {
                let v = !m_h.hide_disabled.get();
                m_h.hide_disabled.set(v);
                on_hide_disabled(v);
            },
            hide,
        )
        .stay_open(),
        MenuItem::run(
            "显示特殊方案",
            move |_ctx| {
                let v = !m_s.show_hidden.get();
                // set_show_hidden 内会重建列表——特殊方案的显隐决定「进不进列表」，
                // 不是单纯的行可见性（分区标题的计数按数据项算）。
                m_s.set_show_hidden(v);
                on_show_hidden(v);
            },
            show,
        )
        .stay_open(),
    ]
}

/// 拖拽落定后的新 `enabled_order`：把 `visible`（已启用区当前那几行的 id，界面顺序）
/// 的第 `from` 项挪到第 `to` 位，再把 `order` 里**不在可见行中**的 id 原样接在后面。
/// `from` 越界返回 None（调用方不动数据）。
///
/// 不直接对 `order` 用下标：它可能是空的（表示"全部启用"，此时可见行才是真顺序），
/// 也可能含着已启用区里不出现的 id——特殊方案、或已卸载但仍留在 available 里的条目。
/// 尾巴那一段必须原样保留：拖一次顺序不该把它们悄悄从 available 里抹掉。
fn reordered_enabled_order(
    visible: &[String],
    order: &[String],
    from: usize,
    to: usize,
) -> Option<Vec<String>> {
    if from >= visible.len() {
        return None;
    }
    let mut ids = visible.to_vec();
    let id = ids.remove(from);
    ids.insert(to.min(ids.len()), id);
    let rest: Vec<String> = order.iter().filter(|x| !ids.contains(x)).cloned().collect();
    ids.extend(rest);
    Some(ids)
}

/// 行首手柄槽宽（含手柄本体 20px 的余量）。所有行都留这一格——未启用/特殊方案行
/// 不可拖、没有手柄，但仍要留槽，否则各区之间的复选框、名称列会错开一截。
const HANDLE_SLOT_W: i32 = 18;

/// 构建单个方案复合行（工具栏+选中模式：行内无操作按钮）。
///
/// 行整体 clickable → 点击选中（事件沿祖先链派发，点 checkbox 会同时切换启用并选中，
/// 符合预期）；选中视觉 = stack 底层 accent 淡底 + 左缘指示条覆盖层（与侧栏导航同语言），
/// visible_when 轮询 `sel_ver`。行内保留：启用复选 · 名称/描述 · 类型/来源徽章 · 版本。
///
/// `handle` 为 `Some` 时该行可拖拽重排（已启用区）。手柄**不能放进 clickable 那个 row**：
/// `Clickable` 对 Down/Up 一律消费，冒泡断在它那里，`ReorderList` 收不到事件、拖动
/// 根本起不来。故它作为 stack 的同级覆盖层叠在行首的占位槽上——顺带让选中底色/描边
/// 也覆盖到手柄这一列，而不是从手柄右侧才开始。
fn build_compound_row(
    entry: SchemaEntry,
    is_enabled: bool,
    mgr: &SchemaManagerState,
    handle: Option<Element>,
) -> Element {
    let enabled_sig = signal(is_enabled);

    // ── Checkbox ──
    // 特殊方案（英文/快符）没有「启用」这回事：它们不进 available、不参与循环切换，
    // 只靠引导键或直达热键进入。给它们一个勾选框会凭空造出一个不存在的状态，用户勾了
    // 也不知道自己勾出了什么。占位保持与上方各行的列对齐（宽度取自 small checkbox）。
    let checkbox = if entry.hidden {
        Element::leaf().width(18).height(18)
    } else {
        let mgr_toggle = mgr.clone();
        let toggle_id = entry.id.clone();
        Element::checkbox("", enabled_sig)
            .small()
            .on_toggle(move |_| {
                let new_val = !enabled_sig.get();
                // 被拒绝（最后一个启用项不可禁用）时不写 signal，勾选态保持不变。
                if mgr_toggle.toggle_schema_enabled(&toggle_id, new_val) {
                    enabled_sig.set(new_val);
                }
            })
    };

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

    // ── 「当前」徽章：仅活跃方案显示，accent 实心（与 outline 的类型徽章拉开层次，
    //    一眼能从整列里挑出来）。列表由 rebuild_list 整体重建，故无需响应式包装。
    let current_badge = mgr.is_active(&entry.id).then(|| {
        Element::label("当前")
            .font_size(10.5)
            .font_weight(600)
            .fg_role(Role::OnAccent)
            .padding_xy(6, 2)
            .corner(4.0)
            .bg_role(Role::Accent)
    });

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
        // 手柄槽占位：手柄本体是 stack 的覆盖层，画在这一格上（见函数文档）。
        .child(Element::leaf().width(HANDLE_SLOT_W).height(1))
        .child(checkbox)
        .child(name_col);

    if let Some(badge) = current_badge {
        row = row.child(badge);
    }

    row = row.child(type_badge).child(source_badge);

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

    // 手柄覆盖层排在最后 = 画在最上、命中最优先（stack 倒序命中），落在行首占位槽上。
    // 包一层无背景容器仅为垂直居中——透明容器不吞命中，行的其余区域照常可点选中。
    if let Some(h) = handle {
        item = item.child(
            Element::row()
                .align(Align::Stretch)
                .cross(Align::Center)
                .padding_xy(8, 0)
                .child(h),
        );
    }

    // 右键菜单挂 **stack 整体**而非内层那个 clickable 的 row：手柄是 stack 的同级覆盖层，
    // 挂内层 row 的话右击手柄冒泡不到它（右击落在手柄上就没菜单，而那正是行首一整格）。
    // 右键不会被 Clickable 吞掉——框架在分发层只把非左键发给接右键的节点，故内层 row
    // 的 clickable 不构成阻碍（与拖拽手柄那个坑正相反）。
    let (m_menu, e_menu) = (mgr.clone(), entry.clone());
    item = item.on_context_menu(move || row_menu_items(&m_menu, &e_menu));

    // 未启用行随 hide_disabled 隐藏（list_signal 重建后无需 enabled_sig 联动）。
    // 注：特殊方案的显隐不在这里——它决定的是「进不进列表」，走 rebuild_list 过滤，
    // 否则分节标题的计数会把看不见的行也算进去。
    if !is_enabled {
        let hide_sig = mgr.hide_disabled;
        item = item.visible_when(move || !hide_sig.get());
    }

    item
}

/// 已启用区的可拖行 = 顶部分隔线 + 行本体。
///
/// 分隔线做进**每一行**（含首行）而不是行与行之间：拖动中行会重新堆叠，若首行特殊、
/// 其余带线，动画期间就会出现"顶上多一条、中间少一条"的错位。每行长得一样，任何排列
/// 都对。首行那条同时承担"工具栏与列表之间的分隔"，故卡片不再自动插那一条
///（见 `scheme_page` 把工具栏与列表并成一个卡片子项）。
fn build_draggable_row(entry: SchemaEntry, mgr: &SchemaManagerState, handle: Element) -> Element {
    Element::col()
        .width_match()
        .child(Element::divider())
        .child(build_compound_row(entry, true, mgr, Some(handle)))
}

// ──────────────────────── UI 构建 ────────────────────────

/// 构建方案相关浮层对话框：删除确认 + 方案详情/设置。
/// `app` 只为导入确认那一支所需（片段应用后回灌配置镜像）。
pub fn build_schema_manager_dialog(
    app: &Rc<RefCell<AppState>>,
    state: &SchemaManagerState,
) -> Element {
    let confirm_dlg = build_confirm_dialog(state);
    let settings_dlg = build_schema_settings_dialog(state);
    let import_dlg = build_import_confirm_dialog(app, state);
    let hotkey_dlg = build_schema_hotkey_dialog(state);
    let trigger_dlg = build_trigger_keys_dialog(state);
    let ka_dlg = build_key_actions_dialog(state);
    let sa_dlg = build_session_actions_dialog(state);
    let ko_dlg = build_keys_overview_dialog(state);
    Element::stack()
        .fill()
        .child(confirm_dlg)
        .child(settings_dlg)
        .child(import_dlg)
        // 排在方案设置之后 = 叠在它上面：这几个对话框都是从方案设置里打开的。
        .child(hotkey_dlg)
        .child(trigger_dlg)
        .child(ka_dlg)
        .child(sa_dlg)
        .child(ko_dlg)
        // 按键选择浮层是**第四级**：从按键表里点「添加 / 修改」打开，故排在两张表之后。
        // 顺序反了的表现是点了没反应——浮层建出来了，被压在按键表下面。
        .child(state.settings_ka.build_picker())
        .child(state.settings_sa.build_picker())
}

/// 方案级按键功能表浮层（第三级）。内容由 `KeyActionFields` 自带响应式列表驱动，
/// 无需像码表配置那样按 plan 重建——行元素绑的是 `Signal<Vec<Row>>`，播种时 `set`
/// 即触发 `host_signal` 重建，天然跟得上换方案。
fn build_key_actions_dialog(state: &SchemaManagerState) -> Element {
    // 尺寸与内边距由 `build_binding_dialog` 统一决定——全局那两个也走它，五处一致。
    // 「添加」进底部按钮栏（与「关闭」同排）；放列表下方会随列表一起滚走。
    // 无「保存」：与码表配置同路，改动随方案设置对话框的「保存」一并提交。
    crate::dialogs::key_binding::build_binding_dialog(
        state.settings_ka_visible,
        state.settings_ka_title,
        state.settings_ka.build_body(),
    )
}

/// 按键总览浮层（第三级，**只读**）。没有「添加」按钮，底部只有壳自带的「关闭」。
fn build_keys_overview_dialog(state: &SchemaManagerState) -> Element {
    crate::dialogs::key_binding::build_binding_dialog(
        state.settings_ko_visible,
        state.settings_ko_title,
        state.settings_ko.build_body(),
    )
}

/// 方案级会话态按键表浮层（第三级）。结构与 [`build_key_actions_dialog`] 对称，
/// 内容同样由自带响应式列表驱动。
fn build_session_actions_dialog(state: &SchemaManagerState) -> Element {
    crate::dialogs::key_binding::build_binding_dialog(
        state.settings_sa_visible,
        state.settings_sa_title,
        state.settings_sa.build_body(),
    )
}

/// 特殊方案的引导键多选浮层（复用清单那套 pairs 对话框）。
///
/// 写回目标是全局 `keys.key_actions` 里指向 `special:<方案id>` 的那几个键，与
/// 「一个 key 一个控件」的清单模型对不上，故不经 FieldRegistry 注册；落表由
/// `commit_trigger_keys_edit` → `key_actions` → AppState 注册的 writer 完成。
fn build_trigger_keys_dialog(state: &SchemaManagerState) -> Element {
    use crate::dialogs::field_dialogs::build_pairs_dialog;

    let labels: Vec<String> = TRIGGER_KEY_OPTIONS
        .iter()
        .map(|(_, l)| l.to_string())
        .collect();
    build_pairs_dialog(
        state.settings_trigger_visible,
        "引导键",
        "在中文输入中按下即临时进入本方案，打完一段自动退回。可多选，全部取消即禁用。",
        state.settings_trigger_sigs.clone(),
        labels,
        // 无「恢复默认」可回落：出厂不给任何方案预置引导键。
        Vec::new(),
    )
}

/// 方案直达热键的编辑浮层（复用 manifest 那套热键对话框组件）。
///
/// 与清单里的功能热键共用同一个 UI，但**不经 FieldRegistry 注册**——它编辑的是
/// `keys.schema_hotkeys` 里按方案 id 索引的一条，而清单模型是「一个 key 一个控件」。
/// 写回由 `commit_hotkey_edit` → `schema_hotkeys` map → AppState 注册的 writer 完成。
fn build_schema_hotkey_dialog(state: &SchemaManagerState) -> Element {
    use crate::dialogs::field_dialogs::build_hotkey_dialog;
    use crate::manifest::hotkey_keys;

    let keys = hotkey_keys();
    let key_vals: Vec<String> = keys.iter().map(|(v, _)| v.clone()).collect();
    let key_labels: Vec<String> = keys.iter().map(|(_, l)| l.clone()).collect();

    build_hotkey_dialog(
        state.settings_hotkey_visible,
        "方案切换热键",
        state.settings_hotkey_raw,
        state.settings_hotkey_disp,
        // 方案热键出厂即空表，没有「默认键位」可回落，故默认 = 未绑定。
        // 必须传 core 的 sentinel "none" 而不是空串：对话框的「恢复默认」按
        // `default != "none"` 反推启用态，传空串会把「启用」勾亮、而热键显示「未设置」。
        "none".to_string(),
        state.settings_hotkey_mods,
        state.settings_hotkey_key_idx,
        key_vals,
        key_labels,
        // 不提供「全局拦截」勾选：那要写 keys.global_hotkeys，而该列表只认固定的
        // 7 个动作名，塞不进按方案变化的条目。
        None,
        state.settings_hotkey_enabled,
    )
}

/// 方案包导入确认：一份可滚动的**富文本导入报表**（见
/// [`SchemaManagerState::build_import_report`]）+ 合并（默认）/ 替换（danger）二选一。
/// zip 与文本信封共用本对话框。
///
/// 报表把包名、说明、元信息、文件统计、逐条配置改动收进**同一份文档**，层级与配色
/// 由它自己的样式表统一定义——此前是六个控件各管一段、各自带字号，凑在一起谁也不
/// 比谁显眼。
///
/// `app` 用于第二步 `config.applyPatch` 成功后回灌配置镜像——core 的逐键
/// `config.changed` 回声到达时镜像已持有同值，被识别为本端写入，不弹「已在别处更改」。
fn build_import_confirm_dialog(app: &Rc<RefCell<AppState>>, state: &SchemaManagerState) -> Element {
    let merge = state.clone();
    let replace = state.clone();
    let cancel = state.clone();
    let (app_merge, app_replace) = (app.clone(), app.clone());
    let can_confirm = state.import_can_confirm;

    // 报表整体放进一个限高滚动区：内容多寡由包决定（说明可长、改动可多），
    // 对话框高度却必须可控——短包时自然收缩，长包时封顶并可滚。
    let report = Element::scroll()
        .width_match()
        .max_height(420)
        .child(Element::rich_signal(state.import_report).width_match());

    let content = flat_panel()
        .width(520)
        .padding(20)
        .spacing(12)
        .child(
            Element::label("导入方案包")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(24)
                .width_match(),
        )
        .child(Element::divider())
        .child(report)
        // 合并/替换的差别紧贴按钮上方**独占一行**：这句话是按下前那一刻才用得上的，
        // 但与三个按钮挤同一排时剩余宽度不够，必然折成两行。
        .child(
            Element::label("合并跳过已存在的同名文件，替换则覆盖它们")
                .font_size(11.5)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(1),
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
                        .enabled_when(move || can_confirm.get())
                        .on_click(move |_| {
                            apply_import_written(&app_replace, replace.confirm_import("replace"))
                        }),
                )
                .child(
                    Element::button("合并导入")
                        .small()
                        .enabled_when(move || can_confirm.get())
                        .on_click(move |_| {
                            apply_import_written(&app_merge, merge.confirm_import("merge"))
                        }),
                ),
        );
    Element::dialog(state.import_confirm_visible, content)
}

/// 把 `config.applyPatch` 落盘的键值回灌配置镜像与设置页控件（复用外部变更的既有
/// 回填通路）。空列表 = 包不带片段或片段没应用成，直接返回。
fn apply_import_written(app: &Rc<RefCell<AppState>>, written: Vec<(String, Value)>) {
    if written.is_empty() {
        return;
    }
    let mut st = app.borrow_mut();
    for (key, value) in written {
        st.apply_external_config_change(&key, value);
    }
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
            Element::label_signal(state.confirm_msg)
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

/// 逐个方案提交队列，**成功一个移除一个**。
///
/// 中途失败时：已落盘的必须移出队列，否则下次保存会再写一遍；未落盘的必须留着，
/// 否则用户重试时那些改动已经不在了——而他并不知道自己丢了什么。
///
/// 提交动作由调用方传入（真实路径是两个 RPC），本函数只管队列的进出，
/// 好让重试语义能脱离 RPC 单测。
fn drain_pending(
    q: &Rc<RefCell<std::collections::HashMap<String, PendingSchemaEdit>>>,
    mut submit: impl FnMut(&str, &PendingSchemaEdit) -> Result<(), String>,
) -> Result<(), String> {
    // 先取 id 列表：submit 期间不持有 q 的借用，否则 RPC 回调链上任何一处碰到队列即 panic。
    let ids: Vec<String> = q.borrow().keys().cloned().collect();
    for id in ids {
        let Some(edit) = q.borrow().get(&id).cloned() else {
            continue;
        };
        submit(&id, &edit)?;
        q.borrow_mut().remove(&id);
    }
    Ok(())
}

/// 把一次编辑合并进待提交队列。
///
/// **合并而非覆盖**：用户可以先改词库、关窗、再进去改码表配置，两次都得留下；
/// 也可以连着配好几个方案再点「应用设置」，先配的那几个不能被后面的挤掉。
/// 只留最后一份工作态的话，丢失是静默的——用户以为都存了。
fn merge_pending(
    q: &mut std::collections::HashMap<String, PendingSchemaEdit>,
    id: &str,
    cfg: Option<Value>,
    dicts: Vec<(String, bool)>,
) {
    let slot = q.entry(id.to_string()).or_default();
    // cfg 是整份配置快照，后一次天然含前一次的内容，直接替换。
    if cfg.is_some() {
        slot.cfg = cfg;
    }
    // 词库开关是逐条的，同一条以后写为准，不同条累加。
    for (dict_id, want) in dicts {
        match slot.dicts.iter_mut().find(|(d, _)| *d == dict_id) {
            Some(e) => e.1 = want,
            None => slot.dicts.push((dict_id, want)),
        }
    }
}

/// 码表配置行的摘要文案。关着时说清「跟的是谁」——普通方案跟全局，特殊方案跟内置基线
/// （core 不让特殊方案继承全局，见 `codetable_baseline`）。纯函数便于单测。
fn ct_summary(enabled: bool, hidden_schema: bool) -> &'static str {
    match (enabled, hidden_schema) {
        (true, _) => "已自定义",
        (false, true) => "跟随内置基线",
        (false, false) => "跟随全局配置",
    }
}

fn build_section(kind: SettingsSection, state: &SchemaManagerState) -> Element {
    match kind {
        SettingsSection::Header => {
            let entry = state.settings_entry.borrow().clone().unwrap_or_default();
            build_settings_header(&entry)
        }
        SettingsSection::EntryKeys => {
            let entry = state.settings_entry.borrow().clone().unwrap_or_default();
            let disp = state.settings_hotkey_disp;
            let visible = state.settings_hotkey_visible;
            // 「清除」按钮的闭包要动这三个：raw 是真相源（commit 侧据它增删条目），
            // disp 是行内显示，enabled 是对话框内部的派生态。
            let (hk_raw, hk_disp, hk_enabled) = (
                state.settings_hotkey_raw,
                state.settings_hotkey_disp,
                state.settings_hotkey_enabled,
            );
            // 多数行不写说明：行名已经说清了，再挂一行灰色小字只是噪音。语义细节
            // （切换 vs 临时进入的区别）在文档站。「切换到本方案」是例外——它写的不是
            // 方案自己的配置，见下方那行说明。
            let mut col = Element::col()
                .width_match()
                .spacing(4)
                .padding_xy(0, 4)
                .child(crate::manifest::dialog_group_header("按键设置"))
                .child(
                    Element::row()
                        .width_match()
                        .cross(Align::Center)
                        .spacing(10)
                        .child(
                            Element::label("切换到本方案按键")
                                .font_size(13.0)
                                .fg_role(Role::Text)
                                .weight(1.0)
                                .max_lines(1),
                        )
                        .child(
                            // 固定宽度：长热键文本（如 Ctrl+Shift+\）不该撑宽右侧挤压左列。
                            Element::label_signal(disp)
                                .font_size(12.5)
                                .fg_role(Role::TextMuted)
                                .max_lines(1)
                                .width(114),
                        )
                        .child(
                            Element::button("设置")
                                .small()
                                .outline_soft()
                                .on_click(move |_| visible.set(true)),
                        )
                        // ⛔ 这里**不要**再加「启用」复选框（2026-08-21 删除）。
                        //
                        // 配置模型里没有「停用但保留键位」这回事：一条方案热键就是
                        // key_actions 里的一个条目，要么在、要么不在。而复选框天然让人
                        // 以为取消勾选只是暂时关掉——实际是把条目删了，关掉对话框再打开
                        // 就变成「未设置」，键位也没了。用户原话：「关闭后就清空了」。
                        //
                        // 真要做「停用保留」，得先在 core 给 key_actions 的值增加一种
                        // 「停用态」的表达（`none` 不行，那是**键**维度的「禁用这个键」，
                        // 语义不同还会占着这个键）。在那之前，UI 不该暗示一个不存在的能力。
                        .child(Element::button("清除").small().outline_soft().on_click(
                            move |_| {
                                // 置空 raw 即解绑：commit 侧按 `raw` 为空处理为移除条目。
                                // enabled 同步置 false——它是对话框内部的派生量
                                // （`enabled.set(v != "none")`），不同步会让下次打开
                                // 对话框时「已启用」与「未设置」自相矛盾。
                                hk_enabled.set(false);
                                hk_disp.set("未设置".to_string());
                                hk_raw.set(String::new());
                            },
                        )),
                )
                // 说明这条配置**存在哪儿**：它写的不是方案自己的 override，而是全局
                // `keys.key_actions` 里一条指向本方案的条目。不点破的话，用户在「全局
                // 自定义按键」里看到一条自己没加过的绑定，会以为是哪里冒出来的脏数据，
                // 甚至手动删掉——而删掉就等于把这里的「切换到本方案」也一并清了。
                .child(
                    Element::label("此配置同时出现在「按键 → 全局自定义按键」中")
                        .font_size(11.0)
                        .fg_role(Role::TextMuted)
                        .width_match()
                        .max_lines(2),
                );

            // 引导键只给 **overlay 方案**：按下引导键临时进入、打完一段自动退回原方案。
            // 普通方案没有这条通路（切过去就留在那儿），给它们显示这一行只会让人以为能配。
            //
            // ★ 判据从 `entry.hidden` 改成 `entry.overlay`。hidden 当年是 overlay 的**代理**
            // （那时快符与英文都是隐藏方案），但它一直不准：隐藏方案未必有 overlay 生命周期，
            // 给这类方案配了引导键按下去什么都不会发生。现在 core 给出了真判据。
            if entry.overlay {
                let tv = state.settings_trigger_visible;
                col = col.child(
                    Element::row()
                        .width_match()
                        .cross(Align::Center)
                        .spacing(10)
                        .child(
                            Element::label("引导键")
                                .font_size(13.0)
                                .fg_role(Role::Text)
                                .weight(1.0)
                                .max_lines(1),
                        )
                        .child(
                            // 摘要由 effect 与勾选同步刷新（见 build_schema_settings_dialog），
                            // 不等 commit 落表——否则用户勾完看不到变化。
                            Element::label_signal(state.settings_trigger_disp)
                                .font_size(12.5)
                                .fg_role(Role::TextMuted)
                                .max_lines(1)
                                .width(114),
                        )
                        .child(
                            Element::button("设置")
                                .small()
                                .outline_soft()
                                .on_click(move |_| tv.set(true)),
                        ),
                );
            }

            // 按键绑定（方案顶层 `key_actions`）：与上面两行同属「这个方案怎么进、
            // 键怎么用」，故并入本节而非另立组标题。所有方案都有。
            let kv = state.settings_ka_visible;
            col = col.child(
                Element::row()
                    .width_match()
                    .cross(Align::Center)
                    .spacing(10)
                    .child(
                        Element::label("本方案-按键自定义")
                            .font_size(13.0)
                            .fg_role(Role::Text)
                            .weight(1.0)
                            .max_lines(1),
                    )
                    .child(
                        Element::label_signal(state.settings_ka_disp)
                            .font_size(12.5)
                            .fg_role(Role::TextMuted)
                            .max_lines(1)
                            .width(114),
                    )
                    .child(
                        Element::button("设置")
                            .small()
                            .outline_soft()
                            .on_click(move |_| kv.set(true)),
                    ),
            );

            // 输入中按键（方案顶层 `session_actions`）：与上一行成对——那张管「没在输入时
            // 按下去干什么」（引导键），这张管「输入中按下去干什么」（翻页/选词/取消）。
            // 措辞按**用户处境**分而不是按内部表名分：用户不知道也不必知道有两张表。
            let sv = state.settings_sa_visible;
            col = col.child(
                Element::row()
                    .width_match()
                    .cross(Align::Center)
                    .spacing(10)
                    .child(
                        Element::label("本方案-输入中按键自定义")
                            .font_size(13.0)
                            .fg_role(Role::Text)
                            .weight(1.0)
                            .max_lines(1),
                    )
                    .child(
                        Element::label_signal(state.settings_sa_disp)
                            .font_size(12.5)
                            .fg_role(Role::TextMuted)
                            .max_lines(1)
                            .width(114),
                    )
                    .child(
                        Element::button("设置")
                            .small()
                            .outline_soft()
                            .on_click(move |_| sv.set(true)),
                    ),
            );

            // 按键总览：把上面两行加全局那几张表**合起来**回答「这个键现在到底干什么」。
            // 排在两个编辑入口之后 —— 它是查看用的，不是又一个编辑入口。
            let ov = state.settings_ko_visible;
            col = col.child(
                Element::row()
                    .width_match()
                    .cross(Align::Center)
                    .spacing(10)
                    .child(
                        Element::label("按键总览")
                            .font_size(13.0)
                            .fg_role(Role::Text)
                            .weight(1.0)
                            .max_lines(1),
                    )
                    .child(
                        Element::label_signal(state.settings_ko_disp)
                            .font_size(12.5)
                            .fg_role(Role::TextMuted)
                            .max_lines(1)
                            .width(114),
                    )
                    .child(
                        Element::button("查看")
                            .small()
                            .outline_soft()
                            .on_click(move |_| ov.set(true)),
                    ),
            );
            col
        }
        SettingsSection::Divider => Element::divider(),
        SettingsSection::ShuangpinLayout => {
            let labels: Vec<String> = state
                .layouts
                .borrow()
                .iter()
                .map(|(_, name)| name.clone())
                .collect();
            // 带分组标题，与「进入方式」「扩展词库」同构——少了它这一节就是节间唯一的
            // 裸行，视觉上像是漏排了。
            Element::col()
                .width_match()
                .spacing(4)
                .padding_xy(0, 4)
                .child(crate::manifest::dialog_group_header("方案参数"))
                .child(
                    Element::row()
                        .width_match()
                        .cross(Align::Center)
                        .spacing(12)
                        .child(
                            Element::label("双拼布局")
                                .font_size(13.0)
                                .fg_role(Role::Text)
                                .weight(1.0)
                                .max_lines(1),
                        )
                        .child(Element::dropdown(labels, state.settings_layout).width(170)),
                )
        }
        SettingsSection::AuxCode => {
            // 关闭态的说明随全局基线而变——「跟随全局」到底是开还是关，用户在这一屏看不到
            // 全局页，不写清就得来回切页确认。
            Element::col()
                .width_match()
                .spacing(4)
                .padding_xy(0, 4)
                .child(crate::manifest::dialog_group_header("辅助码"))
                .child(
                    Element::row()
                        .width_match()
                        .cross(Align::Center)
                        .spacing(12)
                        .child(
                            Element::label("本方案启用辅助码")
                                .font_size(13.0)
                                .fg_role(Role::Text)
                                .weight(1.0)
                                .max_lines(1),
                        )
                        .child(
                            Element::segmented(
                                AUX_CODE_MODES.iter().map(|s| s.to_string()).collect(),
                                state.settings_aux_idx,
                            )
                            .width(210),
                        ),
                )
                .child(
                    Element::label(
                        "打出候选后按触发键，用字形码进一步筛选。触发键在「基本配置 → 按键设置」里配。",
                    )
                    .font_size(11.5)
                    .fg_role(Role::TextMuted)
                    .width_match()
                    .max_lines(2),
                )
        }
        SettingsSection::CodeTable => {
            // 总开关 + **内联**的具体设置（勾掉即整片折叠），而不是「勾选 + 打开子对话框」。
            //
            // 分了 TAB 之后这一屏有的是纵向空间，再套一层浮层只是多一次点击；而折叠区把
            // 「跟随全局 / 自己一套」这个二选一和它的后果放在同一屏里，选完就能看到改什么。
            //
            // 组件是通用的（`collapsible_group`）：混输等方案类型将来加自己的覆盖设置时
            // 直接复用，不必再拼一遍开关与折叠的联动。
            let ct = state.settings_ct.clone();
            let manifest = state.manifest.clone();
            let body =
                Element::host_signal(state.settings_ct_plan, move |item| ct.row(&manifest, item));
            crate::dialogs::field_dialogs::collapsible_group(
                "方案自定义",
                "自定义本方案的行为",
                state.settings_ct_disp,
                state.settings_ct_enabled,
                Element::col().width_match().padding_xy(0, 4).child(body),
            )
        }
        SettingsSection::Overlay => {
            let show_all = state.settings_ov_show_all;
            let layout = state.settings_ov_layout;
            // 本节只在方案已声明 `[overlay]` 时出现，故无总开关、无置灰联动：
            // 两项恒可用。见 `SchemaManagerState` 里那组信号的说明。
            Element::col()
                .width_match()
                .spacing(4)
                .padding_xy(0, 4)
                .child(crate::manifest::dialog_group_header("特殊模式"))
                .child(
                    Element::label(
                        "本方案可用引导键临时进入，打完一段自动退回原方案。引导键在上方「按键设置」里配。",
                    )
                    .font_size(12.0)
                    .fg_role(Role::TextMuted)
                    .max_lines(2),
                )
                .child(
                    Element::row()
                        .width_match()
                        .cross(Align::Center)
                        .spacing(10)
                        .child(
                            Element::label("进入即展示候选")
                                .font_size(13.0)
                                .fg_role(Role::Text)
                                .weight(1.0)
                                .max_lines(1),
                        )
                        .child(Element::checkbox("开启", show_all).small().fg_role(Role::Text)),
                )
                .child(
                    Element::row()
                        .width_match()
                        .cross(Align::Center)
                        .spacing(10)
                        .child(
                            Element::label("候选排列")
                                .font_size(13.0)
                                .fg_role(Role::Text)
                                .weight(1.0)
                                .max_lines(1),
                        )
                        // 下拉而非分段控件：同一对话框里的「双拼布局」、码表配置里的各枚举项
                        // 用的都是 dropdown，同类选项换个控件形态会显得没来由。宽度也对齐
                        // 双拼布局那一行。
                        //
                        // ⚠️ 别加 `.small()`：它对非 Button/CheckBox/Switch 的 widget 会落到
                        // `config_button` 的 `debug_assert!(false, "small() 只能用于 button")`，
                        // debug 构建下当场 panic。这类链式 API 用**运行时 downcast** 做类型
                        // 检查，编译期一律放行——本节此前就是这么崩的。
                        .child(
                            Element::dropdown(vec!["跟随全局", "竖排", "横排"], layout).width(170),
                        ),
                )
        }
        SettingsSection::ExtensionDicts => {
            let mut col = Element::col()
                .width_match()
                .spacing(4)
                .padding_xy(0, 4)
                .child(crate::manifest::dialog_group_header("扩展词库"));
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
        SettingsSection::Empty => Element::label("该方案除进入方式外没有其它可配置项。")
            .font_size(11.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .padding_xy(0, 4),
    }
}

fn build_schema_settings_dialog(state: &SchemaManagerState) -> Element {
    let visible = state.settings_visible;

    // 配置区：按 settings_sections 响应式重建（每次打开对话框按方案类型刷新）；
    // Header 恒为首节，元信息与各配置节统一由此渲染。
    // TAB 条 + 三块内容。
    //
    // 三块**同时建好、靠 visible_when 切换**，不是按选中项重建：各节的工作态（勾选、
    // 下拉、行编辑器）都挂在 Element 上，重建等于用户切个 TAB 回来就发现刚改的没了。
    let tab = state.settings_tab;
    let tab_bar = Element::row().width_match().padding_xy(0, 8).child(
        Element::segmented(SETTINGS_TABS.iter().map(|s| s.to_string()).collect(), tab).weight(1.0),
    );

    let mut panes = Element::col().width_match();
    for i in 0..SETTINGS_TABS.len() {
        let st = state.clone();
        let sec = state.settings_sections[i];
        let body = Element::host_signal(sec, move |kind| build_section(kind, &st));
        // 空 TAB 给一句话而不是留白：一片空白看起来像没加载出来，而「方案自定义」对
        // 拼音/混输这类方案确实是空的（core 只对 codetable 给 effectiveCodetable）。
        let hint = Element::label("该方案在这一类下没有可配置的项。")
            .font_size(12.5)
            .fg_role(Role::TextMuted)
            .padding_xy(0, 10)
            .visible_when(move || sec.get().is_empty());
        panes = panes.child(
            Element::col()
                .width_match()
                .child(body)
                .child(hint)
                .visible_when(move || tab.get() == i),
        );
    }
    let sections = Element::col()
        .width_match()
        .child(tab_bar)
        .child(Element::divider())
        .child(panes);

    // 热键工作态 → 内存 map 的落盘点。**用 effect 而非在各个「关闭」按钮上调**：
    // 对话框有多条关闭路径（footer 按钮 / 标题栏 / 保存后自动关），逐个挂等于赌自己
    // 数全了；而工作态是所有方案复用的同一组信号，漏一条路径就意味着「配了 A 的热键、
    // 转头打开 B」时 A 的改动被 seed 静默冲掉。
    // raw 与 enabled 都要监听：关掉「启用」不改 raw，只看 raw 会漏掉「解绑」这个操作。
    let st_eff = state.clone();
    let raw_sig = state.settings_hotkey_raw;
    let en_sig = state.settings_hotkey_enabled;
    let hotkey_effect = crate::pages::dict::reactive::reactive_effect(
        move || raw_sig.version().wrapping_add(en_sig.version()),
        move || st_eff.commit_hotkey_edit(),
    );

    // 引导键同理：9 个复选各自独立，版本号求和即可覆盖「任一勾选变化」。
    // 顺带刷新行内摘要——它与勾选同源，放两个 effect 里只会多一次遍历。
    let st_tk = state.clone();
    let tk_sigs = state.settings_trigger_sigs.clone();
    let tk_disp = state.settings_trigger_disp;
    let ver_sigs = state.settings_trigger_sigs.clone();
    let trigger_effect = crate::pages::dict::reactive::reactive_effect(
        move || {
            ver_sigs
                .iter()
                .fold(0u64, |acc, (_, s)| acc.wrapping_add(s.version()))
        },
        move || {
            st_tk.commit_trigger_keys_edit();
            let n = tk_sigs.iter().filter(|(_, s)| s.get()).count();
            tk_disp.set(if n == 0 {
                "未设置".to_string()
            } else {
                format!("已选 {n} 个")
            });
        },
    );

    // 码表配置摘要随勾选刷新。与引导键摘要同理：不等落盘，否则勾完看不到变化。
    let st_ct = state.clone();
    let ct_en = state.settings_ct_enabled;
    let ct_disp = state.settings_ct_disp;
    let ct_effect = crate::pages::dict::reactive::reactive_effect(
        move || ct_en.version(),
        move || ct_disp.set(ct_summary(ct_en.get(), st_ct.settings_entry_hidden()).to_string()),
    );

    // 关窗即把工作态暂存进待提交队列，随「应用设置 / 保存并退出」落盘。
    //
    // 对话框自己不再有「保存」按钮：方案 override 与热键/引导键属于同一次编辑，
    // 分两个按钮提交必然出现「一半存了一半没存」。每多一个非 config.toml 的落点就多
    // 一个独立保存按钮的话，设置界面最后会是一堆各存各的按钮——所以统一登记进
    // `state::SideCommitter`，由底栏那个按钮提交。
    //
    // 挂 effect 而非挂在「关闭」按钮上：对话框有多条关闭路径（footer 按钮 / ESC /
    // 标题栏），逐个挂等于赌自己数全了。
    let st_stage = state.clone();
    let vis = visible;
    let stage_effect = crate::pages::dict::reactive::reactive_effect(
        move || vis.version(),
        move || {
            if !vis.get() {
                st_stage.stage_settings_edits();
            }
        },
    );

    // 走与「全局方案配置」相同的外壳（标题栏 + 固定高滚动区 + 按钮栏）。此前用的是
    // dialog_panel：无滚动、标题 18px、宽 440，同一个设置界面里两种弹窗长得不一样。
    let content = Element::col()
        .width_match()
        .padding_xy(16, 2)
        .child(sections)
        .child(hotkey_effect)
        .child(trigger_effect)
        .child(ct_effect)
        .child(stage_effect);

    // 用放大的外壳：分了 TAB 之后，默认 460×380 里 TAB 条要占掉一行，剩下的高度装不下
    // 「基本信息 + 扩展词库」那一屏。
    crate::dialogs::field_dialogs::build_group_dialog_shell_large(
        visible,
        "方案设置",
        content,
        vec![],
    )
}

#[cfg(test)]
mod ext_dict_tests {
    use super::*;

    /// 改绑必须**先摘再写**：取消勾选的键要真的消失。
    ///
    /// 只增不减的话，用户取消勾选毫无效果；且一个键只能有一个动词，旧键留着会
    /// 挡住别的模式配同一个键——现象是「那个键怎么配都归前一个模式」。
    #[test]
    fn verb_rebind_drops_stale_keys() {
        let mut m = std::collections::BTreeMap::new();
        apply_verb_rebind(&mut m, "special:kf", &["backslash".into(), "grave".into()]);
        assert_eq!(m.get("backslash").map(String::as_str), Some("special:kf"));
        assert_eq!(m.get("grave").map(String::as_str), Some("special:kf"));

        // 改成只留 grave：backslash 那条必须消失。
        apply_verb_rebind(&mut m, "special:kf", &["grave".into()]);
        assert!(
            !m.contains_key("backslash"),
            "取消勾选的键应移除，实际 {m:?}"
        );
        assert_eq!(m.get("grave").map(String::as_str), Some("special:kf"));

        // 全部取消：该动词一条不剩。
        apply_verb_rebind(&mut m, "special:kf", &[]);
        assert!(m.is_empty(), "清空后不该有残留，实际 {m:?}");
    }

    /// 别的动词的绑定不受影响——改 A 模式的引导键不能碰掉 B 模式的。
    #[test]
    fn verb_rebind_leaves_other_verbs_alone() {
        let mut m = std::collections::BTreeMap::new();
        m.insert("semicolon".to_string(), "mix:quick_mix".to_string());
        m.insert("backtick".to_string(), "temp_pinyin".to_string());

        apply_verb_rebind(&mut m, "special:kf", &["backslash".into()]);
        assert_eq!(
            m.get("semicolon").map(String::as_str),
            Some("mix:quick_mix")
        );
        assert_eq!(m.get("backtick").map(String::as_str), Some("temp_pinyin"));
        assert_eq!(m.get("backslash").map(String::as_str), Some("special:kf"));
    }

    /// 空串/纯空白的键名不进表——它们是历史占位（`trigger_keys = [""]`），
    /// 写进去会变成一条永远匹配不上的死绑定。
    #[test]
    fn verb_rebind_skips_blank_keys() {
        let mut m = std::collections::BTreeMap::new();
        apply_verb_rebind(&mut m, "special:kf", &["".into(), "  ".into()]);
        assert!(m.is_empty(), "空键名不该进表，实际 {m:?}");
    }

    /// 解绑必须**移除条目**，不能留空串——否则 config.toml 里会积下死条目，
    /// 用户以为自己设过热键。
    ///
    /// 表方向已随合并翻转：现在是 `key_actions` 的「键 → 动词」，故按热键串查。
    #[test]
    fn hotkey_edit_unbind_removes_entry() {
        let mut m = std::collections::BTreeMap::new();
        apply_hotkey_edit(&mut m, "english", true, "ctrl+shift+n");
        assert_eq!(
            m.get("ctrl+shift+n").map(String::as_str),
            Some("switch_schema:english")
        );

        // 关掉「启用」
        apply_hotkey_edit(&mut m, "english", false, "ctrl+shift+n");
        assert!(m.is_empty(), "解绑应移除条目，实际 {m:?}");

        // 启用着但主键选成「（未设置）」：raw 为空，同样按解绑处理
        apply_hotkey_edit(&mut m, "english", true, "ctrl+shift+n");
        apply_hotkey_edit(&mut m, "english", true, "");
        assert!(m.is_empty(), "空 raw 应移除条目，实际 {m:?}");

        // core 的未绑定 sentinel 也不该被写进来
        apply_hotkey_edit(&mut m, "english", true, "none");
        assert!(m.is_empty(), "\"none\" 不该入表，实际 {m:?}");
    }

    /// 改键必须**清掉旧键**：表方向翻转后，旧条目的主键是热键串而不是方案 id，
    /// 忘了清就会留下两个键都指向同一方案——用户改了键，旧键却照样生效。
    #[test]
    fn hotkey_edit_rebind_drops_the_old_key() {
        let mut m = std::collections::BTreeMap::new();
        apply_hotkey_edit(&mut m, "wubi86", true, "ctrl+shift+w");
        apply_hotkey_edit(&mut m, "wubi86", true, "ctrl+shift+r");
        assert_eq!(
            m.get("ctrl+shift+r").map(String::as_str),
            Some("switch_schema:wubi86")
        );
        assert!(
            !m.contains_key("ctrl+shift+w"),
            "改键后旧键仍在表里 ⇒ 两个键都能切，用户改了等于没改：{m:?}"
        );
    }

    /// ★ 合并后才有的风险：「进入方式」与「按键功能」共用同一张表，前者**只能碰**
    /// `switch_schema:` 的条目。按方案粗暴清理会把用户在另一节配的往返绑定、
    /// overlay 引导键连带抹掉——而他只是在这一节改了个键。
    #[test]
    fn hotkey_edit_does_not_touch_other_verbs_in_the_shared_table() {
        let mut m = std::collections::BTreeMap::new();
        m.insert(
            "ctrl+shift+n".to_string(),
            "toggle_schema:english".to_string(),
        );
        m.insert("backslash".to_string(), "special:quick_symbols".to_string());
        m.insert("rshift".to_string(), "toggle_schema:wubi86".to_string());

        apply_hotkey_edit(&mut m, "wubi86", true, "ctrl+shift+r");
        apply_hotkey_edit(&mut m, "wubi86", false, "");

        assert_eq!(
            m.get("ctrl+shift+n").map(String::as_str),
            Some("toggle_schema:english"),
            "别的方案的往返绑定被误删：{m:?}"
        );
        assert_eq!(
            m.get("backslash").map(String::as_str),
            Some("special:quick_symbols"),
            "overlay 引导键被误删：{m:?}"
        );
        assert_eq!(
            m.get("rshift").map(String::as_str),
            Some("toggle_schema:wubi86"),
            "**同一方案**的往返绑定归「按键功能」管，不该被「进入方式」的解绑带走：{m:?}"
        );
    }

    /// 待提交队列必须按方案累积。用户连着配好几个方案再点「应用设置」是正常用法，
    /// 只留最后一份的话前面几个会**静默**丢——没有任何提示。
    #[test]
    fn merge_pending_accumulates_across_schemas_and_edits() {
        let mut q = std::collections::HashMap::new();

        // 方案 A：先只改词库开关。
        merge_pending(&mut q, "wubi", None, vec![("emoji".into(), true)]);
        // 方案 B：改码表配置。
        merge_pending(
            &mut q,
            "qsym",
            Some(json!({"engine":{"codetable":{}}})),
            vec![],
        );
        assert_eq!(q.len(), 2, "两个方案各占一条，不该互相挤掉");

        // 回到方案 A 改码表配置：与上一次的词库改动**合并**。
        merge_pending(
            &mut q,
            "wubi",
            Some(json!({"engine":{"type":"codetable"}})),
            vec![],
        );
        let a = &q["wubi"];
        assert!(a.cfg.is_some(), "码表配置应记下");
        assert_eq!(
            a.dicts,
            vec![("emoji".to_string(), true)],
            "上一次的词库改动不该被这次覆盖掉"
        );

        // 同一个词库开关反复拨：以最后一次为准，不产生两条。
        merge_pending(&mut q, "wubi", None, vec![("emoji".into(), false)]);
        assert_eq!(q["wubi"].dicts, vec![("emoji".to_string(), false)]);
        // 另一个词库累加。
        merge_pending(&mut q, "wubi", None, vec![("district".into(), true)]);
        assert_eq!(q["wubi"].dicts.len(), 2);
        // cfg 未再给出时保留上一次的——不能因为这次只改了词库就把方案配置抹掉。
        assert!(q["wubi"].cfg.is_some(), "只改词库不该清掉已暂存的方案配置");
    }

    /// 中途失败时的队列状态决定了「重试」是否可用：已落盘的不能再写一遍，
    /// 未落盘的不能消失。两个方向都错过一次就是静默丢数据。
    #[test]
    fn drain_pending_keeps_unsubmitted_on_failure() {
        let q: Rc<RefCell<std::collections::HashMap<String, PendingSchemaEdit>>> =
            Rc::new(RefCell::new(std::collections::HashMap::new()));
        for id in ["a", "b", "c"] {
            merge_pending(&mut q.borrow_mut(), id, Some(json!({"id": id})), vec![]);
        }

        // 第二个方案提交失败（顺序不定，故按「第 2 次调用」判）。
        let seen = Rc::new(RefCell::new(Vec::<String>::new()));
        let s2 = seen.clone();
        let err = drain_pending(&q, move |id, _| {
            s2.borrow_mut().push(id.to_string());
            if s2.borrow().len() == 2 {
                Err("boom".to_string())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(err, "boom");

        let done = &seen.borrow()[0];
        let failed = &seen.borrow()[1];
        assert!(
            !q.borrow().contains_key(done),
            "已落盘的必须移出队列，否则下次保存重写一遍"
        );
        assert!(q.borrow().contains_key(failed), "失败的必须留下供重试");
        assert_eq!(q.borrow().len(), 2, "失败点之后的也该原样留着");

        // 重试：这次全成功 → 队列清空。
        assert!(drain_pending(&q, |_, _| Ok(())).is_ok());
        assert!(q.borrow().is_empty(), "全部成功后队列应清空");
    }

    /// 提交动作执行期间不得持有队列借用——真实路径上它会走 RPC，
    /// 回调链上任何一处碰到队列就是 BorrowMutError panic。
    #[test]
    fn drain_pending_does_not_hold_borrow_during_submit() {
        let q: Rc<RefCell<std::collections::HashMap<String, PendingSchemaEdit>>> =
            Rc::new(RefCell::new(std::collections::HashMap::new()));
        merge_pending(&mut q.borrow_mut(), "wubi", Some(json!({})), vec![]);
        let probe = q.clone();
        let r = drain_pending(&q, move |_, _| {
            // 若实现是「borrow 着遍历」，这一句直接 panic。
            assert!(probe.try_borrow_mut().is_ok(), "submit 期间队列不该被借着");
            Ok(())
        });
        assert!(r.is_ok());
    }

    /// 摘要必须区分「跟随全局」与「跟随内置基线」：特殊方案不继承全局码表配置
    /// （core `codetable_baseline`），对它说「跟随全局配置」是在骗人——用户会去改
    /// 全局，然后发现快符纹丝不动。
    #[test]
    fn ct_summary_distinguishes_baseline_by_schema_kind() {
        assert_eq!(ct_summary(false, false), "跟随全局配置");
        assert_eq!(ct_summary(false, true), "跟随内置基线");
        // 开启后跟谁都无关了，两种方案同一句。
        assert_eq!(ct_summary(true, false), "已自定义");
        assert_eq!(ct_summary(true, true), "已自定义");
    }

    /// 每个方案一条，互不干扰——对话框复用同一组工作态信号，最容易写成
    /// 「改了 B 把 A 也一起覆盖掉」。
    #[test]
    fn hotkey_edit_is_per_schema() {
        let mut m = std::collections::BTreeMap::new();
        apply_hotkey_edit(&mut m, "wubi86", true, "ctrl+shift+w");
        apply_hotkey_edit(&mut m, "english", true, "ctrl+shift+n");
        apply_hotkey_edit(&mut m, "english", false, "");
        assert_eq!(
            m.get("ctrl+shift+w").map(String::as_str),
            Some("switch_schema:wubi86"),
            "解绑 english 不应影响 wubi86：{m:?}"
        );
        // 空 id（settings_schema_id 尚未播种）不得污染表
        apply_hotkey_edit(&mut m, "", true, "ctrl+shift+x");
        assert_eq!(m.len(), 1, "空方案 id 不该产生条目：{m:?}");
    }

    #[test]
    fn scheme_import_preview_msg() {
        let v = json!({
            "package": {
                "package": { "created_at": "2026-07-13T00:00:00+08:00" },
                "schema": { "id": "demo", "version": "1.0" }
            },
            "willAdd": ["a", "b"], "conflicts": ["c"], "systemRefs": [], "missing": []
        });
        let msg = SchemaManagerState::format_import_preview(&v);
        assert!(msg.contains("方案 demo v1.0"), "{msg}");
        // 机器格式的时间戳要收成人读的样子，且**不含** T/时区尾巴
        assert!(msg.contains("2026-07-13 00:00"), "{msg}");
        assert!(!msg.contains('T') && !msg.contains("+08:00"), "{msg}");
        // 统计与策略说明已各自独立，不再挤在这一行里
        assert!(!msg.contains("新增文件"), "统计不该留在元信息行：{msg}");
        assert!(!msg.contains("合并"), "策略说明不该留在元信息行：{msg}");

        let counts = SchemaManagerState::format_import_counts(&v);
        assert_eq!(counts, "新增文件 2 · 已存在 1", "零值项不该占位：{counts}");

        // 手工包无 package.toml:回落"未知"不炸
        let bare = json!({ "willAdd": ["x"], "conflicts": [] });
        let msg2 = SchemaManagerState::format_import_preview(&bare);
        assert!(msg2.contains("未知"), "{msg2}");
        assert_eq!(
            SchemaManagerState::format_import_counts(&bare),
            "新增文件 1"
        );
        // 全零 = 包里没有方案文件，要说人话而不是摆一排 0
        assert_eq!(
            SchemaManagerState::format_import_counts(&json!({})),
            "包内没有方案文件"
        );
    }

    /// 导入报表的内容与顺序：**这是什么 → 装哪些文件 → 改哪些配置**。
    ///
    /// 断言走 `plain_text()`（富文本的全文投影）而不是数 span——后者把实现细节
    /// 焊进测试，改个样式名就红；前者盯的是「用户到底看到哪些字、按什么顺序」。
    #[test]
    fn import_report_covers_package_files_and_changes_in_reading_order() {
        let preview = json!({
            "title": "快符示例 · 引导键符号表",
            "description": "按引导键呼出的符号表。",
            "package": {
                "package": { "created_at": "2026-08-20T15:30:00+08:00" },
                "schema": { "id": "quick_symbols", "version": "1.0" }
            },
            "willAdd": ["a.schema.toml", "b.dict.yaml"], "conflicts": [], "systemRefs": [], "missing": []
        });
        let patch = ConfigPatchPreview {
            text: String::new(),
            ok: true,
            rows: vec![PreviewRow {
                key: "keys.key_actions[backslash]".to_string(),
                detail: "未设置 → special:quick_symbols".to_string(),
                is_error: false,
            }],
            info: PackageInfo {
                title: "接上引导键".to_string(),
                description: "把反斜杠键设为引导键。".to_string(),
            },
        };
        let t = SchemaManagerState::build_import_report(&preview, Some(&patch)).plain_text();

        for needle in [
            "快符示例 · 引导键符号表",
            "按引导键呼出的符号表。",
            "方案 quick_symbols v1.0",
            "2026-08-20 15:30",
            "新增文件 2",
            "将修改的配置",
            "接上引导键",
            "keys.key_actions[backslash]",
            "未设置 → special:quick_symbols",
        ] {
            assert!(t.contains(needle), "报表缺少「{needle}」：\n{t}");
        }
        // 顺序即阅读顺序：包名在最前，改动清单在文件统计之后
        let at = |s: &str| t.find(s).unwrap_or_else(|| panic!("缺 {s}"));
        assert!(at("快符示例 · 引导键符号表") < at("方案 quick_symbols"));
        assert!(at("新增文件 2") < at("将修改的配置"));
        assert!(at("将修改的配置") < at("keys.key_actions[backslash]"));
        // ok 的片段不该出现拒绝提示
        assert!(!t.contains("已拒绝导入"), "{t}");
    }

    /// 纯方案包（无 config_patch）不出现「将修改的配置」整节——没有的东西不占版面。
    /// 片段有错时才出拒绝提示，且提示必须在报表里（按钮灰着而无解释是最难受的状态）。
    #[test]
    fn import_report_omits_change_section_and_flags_rejection() {
        let preview = json!({ "willAdd": ["x"], "conflicts": [] });
        let plain = SchemaManagerState::build_import_report(&preview, None).plain_text();
        assert!(
            !plain.contains("将修改的配置"),
            "纯方案包不该有该节：{plain}"
        );
        assert!(!plain.contains("已拒绝导入"), "{plain}");

        let bad = ConfigPatchPreview {
            text: String::new(),
            ok: false,
            rows: vec![PreviewRow {
                key: "input.foo".to_string(),
                detail: "未知配置键".to_string(),
                is_error: true,
            }],
            info: PackageInfo::default(),
        };
        let t = SchemaManagerState::build_import_report(&preview, Some(&bad)).plain_text();
        assert!(t.contains("将修改的配置"), "{t}");
        assert!(t.contains("未知配置键"), "{t}");
        assert!(t.contains("已拒绝导入"), "整包被拒必须给出原因：{t}");
    }

    /// 时间戳只截取、不换算：显示的正是**打包者当地**的时间，换算到本地会让
    /// 作者写在说明里的时间对不上号。认不出的格式原样返回，不猜。
    #[test]
    fn created_at_is_trimmed_not_converted() {
        let f = SchemaManagerState::format_created_at;
        assert_eq!(f("2026-08-20T15:30:00+08:00"), "2026-08-20 15:30");
        assert_eq!(f("2026-08-20T15:30:00Z"), "2026-08-20 15:30");
        // 认不出的一律原样：宁可显示得丑，不可显示得错
        assert_eq!(f("2026-08-20"), "2026-08-20");
        assert_eq!(f("昨天"), "昨天");
        assert_eq!(f(""), "");
        assert_eq!(f("2026-08-20T1"), "2026-08-20T1");
    }

    /// 导入门控（整包拒绝策略，`package-format.md` §3.3）：纯方案包恒可导入；
    /// 带片段的包要求片段**整体**通过，有一条错就整包禁止——半装一个包比不装更糟。
    #[test]
    fn import_blocked_when_config_patch_has_error() {
        let patch = |ok| ConfigPatchPreview {
            text: "x = 1\n".to_string(),
            ok,
            rows: vec![],
            info: PackageInfo::default(),
        };
        assert!(import_allowed(None), "纯方案包无片段，恒可导入");
        assert!(import_allowed(Some(&patch(true))));
        assert!(!import_allowed(Some(&patch(false))), "片段有错即整包拒绝");
    }

    /// 打开确认对话框：zip 与文本信封走同一个对话框，且都要把 configPatch 的逐键
    /// diff 渲染进「将修改的配置」区块、按片段是否通过设置导入门控。
    ///
    /// 两条路各测一遍：只测 zip 的话，信封那支忘了接 patch 也照绿。
    #[test]
    fn open_import_wires_patch_rows_and_gate_for_both_sources() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mgr = SchemaManagerState::new();

        let preview = json!({
            "willAdd": ["kf.schema.toml"], "conflicts": [],
            "configPatch": { "text": "[keys.key_actions]\n", "ok": true, "entries": [
                { "key": "keys.key_actions", "mapEntry": "backtick", "next": "quick_input" }
            ]}
        });
        mgr.open_import_confirm(std::path::PathBuf::from("a.wpkg"), &preview);
        assert!(mgr.import_confirm_visible.get());
        assert_eq!(mgr.import_patch_rows.get().len(), 1);
        assert_eq!(
            mgr.import_patch_rows.get()[0].key,
            "keys.key_actions[backtick]",
            "Map 条目的显示键"
        );
        assert!(mgr.import_can_confirm.get());

        // 信封路径 + 片段有错 → 同一区块、按钮禁用。
        let bad = json!({
            "willAdd": ["kf.schema.toml"],
            "configPatch": { "text": "foo = 1\n", "ok": false, "entries": [
                { "key": "foo", "next": 1, "error": "未知配置键" }
            ]}
        });
        mgr.open_import_text_confirm("[package]\nkind = \"schema_text\"\n".into(), &bad);
        assert_eq!(mgr.import_patch_rows.get().len(), 1);
        assert!(mgr.import_patch_rows.get()[0].is_error);
        assert!(!mgr.import_can_confirm.get(), "片段有错即禁用导入");

        // 纯方案包（无 configPatch）：区块不显示、门控放行。
        mgr.open_import_confirm(
            std::path::PathBuf::from("b.zip"),
            &json!({ "willAdd": ["x"], "conflicts": [] }),
        );
        assert!(mgr.import_patch_rows.get().is_empty());
        assert!(mgr.import_can_confirm.get());

        mgr.cancel_import();
        assert!(!mgr.import_confirm_visible.get());
        assert!(mgr.import_patch_rows.get().is_empty(), "取消须清空区块");
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 两级说明在确认对话框上**各显各的**：包级取自响应顶层（package.toml / 信封
    /// `[package]`），片段自己的取自 `configPatch.info`。
    ///
    /// 四个信号刻意给四个不同取值：写成同源就分不清 UI 是各显各的、还是有一处串了源。
    /// zip 与信封两条路各测一遍——只测 zip 的话，信封那支忘了接说明也照绿。
    #[test]
    fn open_import_wires_both_levels_of_info_without_crossing() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mgr = SchemaManagerState::new();

        mgr.open_import_confirm(
            std::path::PathBuf::from("a.wpkg"),
            &json!({
                "title": "快符方案", "description": "分号引导的符号表。",
                "willAdd": ["kf.schema.toml"], "conflicts": [],
                "configPatch": { "text": "x = 1\n", "ok": true, "entries": [],
                                 "info": { "title": "快符引导键", "description": "把反引号设为引导键。" } }
            }),
        );
        assert_eq!(mgr.import_pkg_title.get(), "快符方案");
        assert_eq!(mgr.import_pkg_desc.get(), "分号引导的符号表。");
        assert_eq!(mgr.import_patch_title.get(), "快符引导键");
        assert_eq!(mgr.import_patch_desc.get(), "把反引号设为引导键。");

        // 信封路径：同样两级都要接上，且不与 zip 那次的取值重合。
        mgr.open_import_text_confirm(
            "[package]\nkind = \"schema_text\"\n".into(),
            &json!({
                "title": "拼音增强", "description": "补一批模糊音规则。",
                "willAdd": ["py.schema.toml"],
                "configPatch": { "text": "y = 2\n", "ok": true, "entries": [],
                                 "info": { "title": "模糊音开关", "description": "打开 zh/z 等价。" } }
            }),
        );
        assert_eq!(mgr.import_pkg_title.get(), "拼音增强");
        assert_eq!(mgr.import_pkg_desc.get(), "补一批模糊音规则。");
        assert_eq!(mgr.import_patch_title.get(), "模糊音开关");
        assert_eq!(mgr.import_patch_desc.get(), "打开 zh/z 等价。");

        // 只有包级说明的包：片段那份保持空，不拿包级的顶上（§2.3：两级说的不是
        // 同一件事，顶上会把「这个包是什么」说成「这段配置改什么」）。
        mgr.open_import_confirm(
            std::path::PathBuf::from("c.wpkg"),
            &json!({ "title": "只有包级说明", "willAdd": ["x"],
                     "configPatch": { "text": "z = 3\n", "ok": true, "entries": [] } }),
        );
        assert_eq!(mgr.import_pkg_title.get(), "只有包级说明");
        assert!(
            mgr.import_patch_title.get().is_empty() && mgr.import_patch_desc.get().is_empty(),
            "包级说明不得回落成片段说明"
        );

        // 纯方案包（无说明、无片段）：四个信号全清，两处区块都不占位。恒设值而不是
        // 只在有值时 set——否则上一个包的说明会留在这一个包的对话框上。
        mgr.open_import_confirm(
            std::path::PathBuf::from("d.zip"),
            &json!({ "willAdd": ["x"], "conflicts": [] }),
        );
        for s in [
            mgr.import_pkg_title,
            mgr.import_pkg_desc,
            mgr.import_patch_title,
            mgr.import_patch_desc,
        ] {
            assert!(s.get().is_empty(), "换包必须清掉上一个包的说明");
        }

        // 取消同样清场（与 import_patch_rows 同口径）。
        mgr.open_import_confirm(
            std::path::PathBuf::from("e.wpkg"),
            &json!({ "title": "待取消", "willAdd": ["x"] }),
        );
        mgr.cancel_import();
        assert!(mgr.import_pkg_title.get().is_empty(), "取消须清空说明");
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 确认导入的两步编排：第二步 `config.applyPatch` 的 `written` 必须原样回传给
    /// 调用方去回灌配置镜像——否则 core 的逐键 `config.changed` 回声会被当成外部改动，
    /// 用户刚导入完就被告知「已在别处更改」。
    #[test]
    fn confirm_import_returns_written_from_apply_patch() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mgr = SchemaManagerState::new();

        // 带片段的包：走完两步，拿到 written。
        mgr.open_import_confirm(
            std::path::PathBuf::from("a.wpkg"),
            &json!({ "willAdd": ["x"], "conflicts": [],
                     "configPatch": { "text": "ui.candidate.per_page = 9\n", "ok": true,
                                      "entries": [{ "key": "ui.candidate.per_page", "current": 5, "next": 9 }] }}),
        );
        let written = mgr.confirm_import("merge");
        assert!(
            written.iter().any(|(k, _)| k == "keys.key_actions"),
            "written 应原样来自 applyPatch 响应（含 Map 父键整表），实际: {written:?}"
        );

        // 纯方案包：不发第二步，回空。
        mgr.open_import_confirm(
            std::path::PathBuf::from("b.zip"),
            &json!({ "willAdd": ["x"], "conflicts": [] }),
        );
        assert!(mgr.confirm_import("merge").is_empty());

        // 没有待导入项（重复点击/已取消）：静默返回空，不发 RPC。
        assert!(mgr.confirm_import("merge").is_empty());
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 信封分支的端到端：**发出去的请求**长什么样。
    ///
    /// 与上面 zip 那条独立断言而不复用——两条路在 `confirm_import` 里是各自的
    /// match 臂，共用一份断言的话，信封臂把 text 漏成空串、或错发成 zip 那一法，
    /// 都能照绿。返回值对不对是一回事，请求里少个字段是另一回事，后者只有接上
    /// 真实 core 才会暴露。
    #[test]
    fn confirm_import_text_envelope_sends_text_and_strategy() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mgr = SchemaManagerState::new();

        let envelope = "[package]\nformat_version = 2\nkind = \"schema_text\"\n";
        let patch_text = "[keys.key_actions]\nbacktick = \"quick_input\"\n";
        mgr.open_import_text_confirm(
            envelope.to_string(),
            &json!({
                "willAdd": ["kf.schema.toml"], "conflicts": [],
                "configPatch": { "text": patch_text, "ok": true, "entries": [
                    { "key": "keys.key_actions", "mapEntry": "backtick", "next": "quick_input" }
                ]}
            }),
        );
        // 清场：丢掉 new()/refresh 期间的调用，只看确认之后发出的。
        let _ = crate::rpc::take_mock_calls();

        let written = mgr.confirm_import("replace");
        let calls = crate::rpc::take_mock_calls();
        let find = |name: &str| calls.iter().find(|(m, _)| m == name).map(|(_, p)| p);
        let pos = |name: &str| calls.iter().position(|(m, _)| m == name);

        // ① 第一步：信封走 importText，text 逐字原样、strategy 跟着按钮走。
        let p = find("scheme.importText").expect("信封必须走 scheme.importText");
        assert_eq!(p.get("text").and_then(|v| v.as_str()), Some(envelope));
        assert_eq!(p.get("strategy").and_then(|v| v.as_str()), Some("replace"));
        assert!(
            find("scheme.importPackage").is_none(),
            "信封不得错发成 zip 那一法（它要的是 path，信封没有）"
        );

        // ② 第二步：applyPatch 收到的是 configPatch.text 原文（客户端不重新拼装）。
        let ap = find("config.applyPatch").expect("带片段的包必须发第二步");
        assert_eq!(ap.get("text").and_then(|v| v.as_str()), Some(patch_text));
        assert!(
            pos("scheme.importText") < pos("config.applyPatch"),
            "先写方案文件再应用片段——次序反了，片段引用的方案还不存在"
        );

        // ③ written 原样回传给调用方去回灌镜像。
        assert!(
            written.iter().any(|(k, _)| k == "keys.key_actions"),
            "written 应来自 applyPatch 响应，实际: {written:?}"
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
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

    /// 空 order = 全部启用。这条语义在 rebuild_list/toggle 里各写了一遍，
    /// 抽成谓词后由本测试统一守住。
    #[test]
    fn empty_order_means_all_enabled() {
        assert!(schema_is_enabled(&[], "anything"));
        let order = vec!["a".to_string(), "b".to_string()];
        assert!(schema_is_enabled(&order, "a"));
        assert!(!schema_is_enabled(&order, "c"));
    }

    #[test]
    fn can_activate_requires_selected_enabled_and_not_current() {
        let order = vec!["a".to_string(), "b".to_string()];
        assert!(!can_activate(&order, "a", None), "无选中 → 不可用");
        assert!(!can_activate(&order, "a", Some("a")), "已是当前 → 不可用");
        assert!(!can_activate(&order, "a", Some("c")), "未启用 → 不可用");
        assert!(
            can_activate(&order, "a", Some("b")),
            "已启用的非当前项 → 可用"
        );
        // 全部启用（order 空）时，任何非当前方案都可设为当前。
        assert!(can_activate(&[], "a", Some("z")));
    }

    #[test]
    fn parse_missing_dictionaries_is_empty() {
        assert!(parse_extension_dicts(&json!({})).is_empty());
        assert!(parse_extension_dicts(&json!({ "dictionaries": [] })).is_empty());
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    /// 布局下标 ↔ core `LayoutIntent` 值域逐字对应；越界回落首项而不是 panic。
    ///
    /// 跨仓契约无编译期约束：拼错的表现是 core 解析回落默认值，用户改了没反应。
    #[test]
    fn layout_index_maps_to_core_value_domain() {
        for (idx, want) in [(0, "follow"), (1, "vertical"), (2, "horizontal")] {
            assert_eq!(overlay_layout_value(idx), want);
            assert_eq!(overlay_layout_idx(want), idx, "反向映射应自洽");
        }
        assert_eq!(overlay_layout_value(99), "follow", "越界应回落跟随全局");
        assert_eq!(overlay_layout_idx("不存在的值"), 0, "未知值回落跟随全局");
    }

    /// ★★ 保存**只改本 UI 管的两个字段**，`[overlay]` 段里其余项原样保留。
    ///
    /// 段里还可能有 `kind`、`comment_template_vertical/_horizontal`（模式级注释模板，
    /// 无图形界面、只能手写），以及将来新增的字段。用字面量重建整个对象会把它们静默
    /// 抹掉——用户改一次候选排列就丢掉手写的注释模板配置。
    ///
    /// 这与已删除的 `special_modes.rs` 当年那条教训是同一个形状，故单独钉住。
    #[test]
    fn saving_patches_in_place_and_keeps_unmanaged_fields() {
        let mut cfg = json!({
            "overlay": {
                "kind": "special",
                "show_all_on_enter": false,
                "candidate_layout": "follow",
                "comment_template_vertical": "${dict}",
            }
        });
        // 模拟入队那段的写法：在原对象上 insert，不重建。
        let ov = cfg.get_mut("overlay").unwrap().as_object_mut().unwrap();
        ov.insert("show_all_on_enter".into(), json!(true));
        ov.insert("candidate_layout".into(), json!(overlay_layout_value(1)));

        let ov = &cfg["overlay"];
        assert_eq!(ov["show_all_on_enter"], json!(true), "管的字段要写进去");
        assert_eq!(ov["candidate_layout"], json!("vertical"));
        assert_eq!(
            ov["comment_template_vertical"],
            json!("${dict}"),
            "不管的字段必须原样保留，否则用户手写的注释模板会被静默抹掉"
        );
        assert_eq!(ov["kind"], json!("special"), "kind 同样不该被动");
    }
}

#[cfg(test)]
mod section_build_tests {
    use super::*;

    /// ★★ 辅助码的「跟随全局」必须落成 **null**，不是 false。
    ///
    /// core 的 `json_to_toml` 跳过 null ⇒ 该键不进 override ⇒ 取值回落全局基线。写 false
    /// 的话就成了「显式关闭本方案」——用户明明选的是「跟随」，全局开了这个方案却不跟，
    /// 而且从配置文件上看不出是谁写的。
    #[test]
    fn aux_code_follow_global_writes_null_not_false() {
        assert_eq!(aux_code_value(0), Value::Null, "跟随全局必须写 null");
        assert_eq!(aux_code_value(1), json!(true));
        assert_eq!(aux_code_value(2), json!(false));
        // 越界回落**跟随**而不是钳到末项：钳会把「关闭」塞给一个没选过的方案。
        // 与 `overlay_layout_value` 的同类处置同口径。
        assert_eq!(aux_code_value(99), Value::Null);
    }

    /// ★ 建节与落盘必须**同判据**，`files = []` 不算有表。
    ///
    /// 两处各写一份时它们中间夹着一道缝：建节按「非空数组」、落盘按「这个键存在」，
    /// 于是 `files = []` 的方案不出这一节（信号没被重新播种），保存时却过得了守卫——
    /// 上一个方案留在信号里的档位被写进这个方案的 override，用户从没碰过这个开关。
    #[test]
    fn aux_code_table_criterion_is_shared_by_build_and_save() {
        assert!(has_aux_code_table(&json!({
            "engine": {"aux_code": {"files": ["aux_code/stroke.txt"]}}
        })));
        // 声明了段但没给表 = 没有可用的表。这一条正是那道缝。
        assert!(!has_aux_code_table(&json!({
            "engine": {"aux_code": {"files": []}}
        })));
        assert!(!has_aux_code_table(&json!({
            "engine": {"aux_code": {"enabled": true}}
        })));
        assert!(!has_aux_code_table(&json!({"engine": {}})));
        // 类型不对（写成字符串）同样不算——`is_some()` 会放过它。
        assert!(!has_aux_code_table(&json!({
            "engine": {"aux_code": {"files": "aux_code/stroke.txt"}}
        })));
    }

    /// tri-state 三档往返：读出来的档位写回去必须是同一个值。
    #[test]
    fn aux_code_tri_state_round_trips() {
        for (v, idx) in [(None, 0), (Some(true), 1), (Some(false), 2)] {
            assert_eq!(aux_code_idx(v), idx, "{v:?} 应落在第 {idx} 档");
            let back = aux_code_value(idx);
            let back = if back.is_null() { None } else { back.as_bool() };
            assert_eq!(back, v, "第 {idx} 档写回后应仍是 {v:?}");
        }
    }

    /// 每个配置节都要能**构建出来**而不 panic。
    ///
    /// ★ 这条测的不是布局对不对，而是链式 API 有没有用错对象：windui 的 `.small()`
    /// `.outline_soft()` 这类是 Button/CheckBox 专属，对别的 widget 会落到
    /// `config_button` 的 `debug_assert!(false, "…只能用于 Element::button(..)")`。
    /// 那是**运行时 downcast**，编译期一律放行——`Overlay` 节就因为给 `segmented`
    /// 加了 `.small()` 而在真机上一打开设置就崩，编译和既有测试全绿。
    /// TAB 拆分：每节落在约定的那一页，且分隔线**只在组内**重插。
    ///
    /// 原有的 `Divider` 表达的是「相邻两节之间」，分完 TAB 后相邻关系变了——照搬会出现
    /// 开头就是一条分隔线、或两节之间没有线。这条守的就是那个重插规则。
    #[test]
    fn sections_split_into_two_tabs_with_dividers_reinserted() {
        use SettingsSection::*;
        // 扁平表按真实构造顺序：Header, EntryKeys, |, CodeTable, |, Overlay, |, ExtensionDicts
        let flat = [
            Header,
            EntryKeys,
            Divider,
            CodeTable,
            Divider,
            Overlay,
            Divider,
            ExtensionDicts,
        ];
        let [basic, custom] = split_sections_by_tab(&flat);
        assert_eq!(
            basic,
            vec![Header, EntryKeys, Divider, ExtensionDicts],
            "按键配置已并回基本配置；Header 自带结尾分隔故其后不插"
        );
        assert_eq!(
            custom,
            vec![CodeTable, Divider, Overlay],
            "方案自定义页开头不该是一条分隔线"
        );
    }

    /// 每一节都必须有归属，且只归一页——漏一节的表现是那节在界面上整个消失。
    #[test]
    fn every_content_section_belongs_to_exactly_one_tab() {
        use SettingsSection::*;
        for s in [
            Header,
            EntryKeys,
            ShuangpinLayout,
            AuxCode,
            CodeTable,
            Overlay,
            ExtensionDicts,
        ] {
            let t = s.tab().unwrap_or_else(|| panic!("{s:?} 没有归属 TAB"));
            assert!(t < SETTINGS_TABS.len(), "{s:?} 的 TAB 下标越界");
        }
        // 结构性节不参与分组（分隔线要重插、空态节已退役）。
        assert_eq!(Divider.tab(), None);
        assert_eq!(Empty.tab(), None);
    }

    #[test]
    fn every_settings_section_builds_without_panicking() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let state = SchemaManagerState::new();
        for kind in [
            SettingsSection::Header,
            SettingsSection::EntryKeys,
            SettingsSection::ShuangpinLayout,
            SettingsSection::AuxCode,
            SettingsSection::CodeTable,
            SettingsSection::Overlay,
            SettingsSection::ExtensionDicts,
            SettingsSection::Divider,
            SettingsSection::Empty,
        ] {
            let _ = build_section(kind, &state);
        }
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }
}

#[cfg(test)]
mod row_menu_tests {
    use super::*;

    fn entry(id: &str, source: &str, hidden: bool) -> SchemaEntry {
        SchemaEntry {
            id: id.into(),
            name: id.into(),
            source: source.into(),
            hidden,
            ..Default::default()
        }
    }

    /// 三行已启用（a/b/c）+ 一个特殊方案 s；当前方案为 a，全为用户安装（除 b 为内置）。
    fn fixture() -> SchemaManagerState {
        let st = SchemaManagerState::new();
        let rows = vec![
            entry("a", "user", false),
            entry("b", "builtin", false),
            entry("c", "user", false),
        ];
        *st.schemas.borrow_mut() = {
            let mut v = rows.clone();
            v.push(entry("s", "user", true));
            v
        };
        *st.enabled_order.borrow_mut() = vec!["a".into(), "b".into(), "c".into()];
        st.enabled_rows.set(rows);
        *st.active_id.borrow_mut() = "a".into();
        st
    }

    fn labels(items: &[MenuItem]) -> Vec<String> {
        items
            .iter()
            .filter(|i| !i.separator)
            .map(|i| i.label.clone())
            .collect()
    }

    fn item<'a>(items: &'a [MenuItem], label: &str) -> &'a MenuItem {
        items
            .iter()
            .find(|i| i.label == label)
            .unwrap_or_else(|| panic!("菜单里应有「{label}」项，实际：{:?}", labels(items)))
    }

    /// 右击一行即把它设为选中——菜单里所有判定都按"选中项"算，不选中就会对着上一次
    /// 选的那个方案出菜单（点了「删除」删掉的是别的方案）。
    #[test]
    fn right_click_selects_the_row() {
        let st = fixture();
        st.select("a");
        let _ = row_menu_items(&st, &entry("c", "user", false));
        assert_eq!(st.selected_id.borrow().as_deref(), Some("c"));
    }

    /// 特殊方案（英文/快符）不进 available、不参与循环切换：菜单里不该出现
    /// 「设为当前 / 启用 / 排序」——列出来只会造出一个点了也不知道发生什么的状态。
    #[test]
    fn special_schema_menu_omits_activation_and_ordering() {
        let st = fixture();
        let items = row_menu_items(&st, &entry("s", "user", true));
        assert_eq!(labels(&items), ["方案设置…", "导出方案包…", "删除"]);
    }

    /// 当前方案：不能"再设为当前"，也不能被禁用（toggle 会拒），菜单里直接置灰。
    #[test]
    fn active_schema_can_neither_be_reactivated_nor_disabled() {
        let st = fixture();
        let items = row_menu_items(&st, &entry("a", "user", false));
        assert!(!item(&items, "设为当前").enabled, "已是当前方案");
        let toggle = item(&items, "启用");
        assert!(toggle.checked, "当前方案必然是启用的");
        assert!(
            !toggle.enabled,
            "当前方案不可禁用，应提前置灰而不是点了才报错"
        );
    }

    /// 非当前的已启用方案：可设为当前、可禁用。
    #[test]
    fn enabled_non_active_schema_can_be_activated_and_disabled() {
        let st = fixture();
        let items = row_menu_items(&st, &entry("c", "user", false));
        assert!(item(&items, "设为当前").enabled);
        let toggle = item(&items, "启用");
        assert!(toggle.checked && toggle.enabled);
    }

    /// 排序项按行在**已启用区**的位置置灰：首行不能上移/移到最前，末行反之。
    #[test]
    fn ordering_items_gray_out_at_the_ends() {
        let st = fixture();
        let first = row_menu_items(&st, &entry("a", "user", false));
        assert!(!item(&first, "上移").enabled && !item(&first, "移到最前").enabled);
        assert!(item(&first, "下移").enabled && item(&first, "移到最后").enabled);

        let last = row_menu_items(&st, &entry("c", "user", false));
        assert!(item(&last, "上移").enabled && item(&last, "移到最前").enabled);
        assert!(!item(&last, "下移").enabled && !item(&last, "移到最后").enabled);
    }

    /// 内置方案不可删除（与 `request_delete_at_id` 的守护同口径，菜单提前置灰）。
    #[test]
    fn delete_is_enabled_only_for_user_installed_schemas() {
        let st = fixture();
        assert!(item(&row_menu_items(&st, &entry("c", "user", false)), "删除").enabled);
        assert!(!item(&row_menu_items(&st, &entry("b", "builtin", false)), "删除").enabled);
    }

    /// 「移到最前/最后」走 `enabled_rows` 的下标（与拖拽重排同一真相源）。
    #[test]
    fn move_to_top_and_bottom_rewrite_the_order() {
        let st = fixture();
        st.select("c");
        st.move_selected_to(true);
        assert_eq!(*st.enabled_order.borrow(), ["c", "a", "b"]);

        let st = fixture();
        st.select("a");
        st.move_selected_to(false);
        assert_eq!(*st.enabled_order.borrow(), ["b", "c", "a"]);
    }

    /// 接线守护：右击行内**任意位置**都要弹出菜单——包括行首那一格手柄槽。
    ///
    /// 菜单挂在整行的 stack 上而非内层那个 clickable 的 row，正是为了这一格：手柄是
    /// stack 的同级覆盖层（见 `build_compound_row` 的文档），挂内层 row 的话右击手柄
    /// 冒泡不到菜单节点，行首一整列就成了死区。上面那些测试只验证菜单**内容**，
    /// 挂错节点它们照样全绿。
    #[test]
    fn right_clicking_anywhere_on_the_row_opens_the_menu() {
        use windui::core::Tree;
        use windui::event::{MouseButton, PointerEvent, PointerKind};
        use windui::geometry::{Point, Size};

        let st = fixture();
        // 带手柄 = 已启用区的可拖行，行首那一格被手柄覆盖层占着。
        let el = build_compound_row(
            entry("c", "user", false),
            true,
            &st,
            Some(Element::leaf().width(12).height(12)),
        );
        let mut tree = Tree::new();
        let root = el.build(&mut tree);
        tree.root = Some(root);
        tree.layout_root(Size::new(600, 200), &mut windui::text::NullTextEngine);

        // 手柄槽（x≈12）、名称列（x≈200）、右侧徽章区（x≈540）各来一次。
        for x in [12, 200, 540] {
            let (mut hover, mut capture) = (None, None);
            let res = tree.dispatch_pointer(
                PointerEvent::single(PointerKind::Down, Point::new(x, 20), MouseButton::Right),
                &mut hover,
                &mut capture,
            );
            let menu = res
                .menu
                .unwrap_or_else(|| panic!("x={x} 处右击应弹出行菜单"));
            assert!(
                menu.items.iter().any(|i| i.label == "方案设置…"),
                "x={x} 弹出的应是方案行菜单"
            );
        }
    }

    /// 列表级菜单：过滤项是粘滞的（点了菜单不关），且勾选态取自当前开关值。
    #[test]
    fn list_menu_filter_items_are_sticky_and_reflect_current_state() {
        let st = fixture();
        st.hide_disabled.set(true);
        let noop = || Rc::new(|_: bool| {}) as Rc<dyn Fn(bool)>;
        let items = list_menu_items(&st, noop(), noop());
        let hide = item(&items, "隐藏未启用");
        assert!(hide.checked && hide.stay_open, "粘滞项才能连点多个过滤开关");
        assert!(!item(&items, "显示特殊方案").checked);
    }

    /// 过滤项点击后既翻信号也调持久化回调——只翻信号的话重开设置页就打回原形。
    #[test]
    fn list_menu_filter_persists_through_callback() {
        let st = fixture();
        let saved = Rc::new(std::cell::Cell::new(None));
        let s = saved.clone();
        let items = list_menu_items(&st, Rc::new(move |v| s.set(Some(v))), Rc::new(|_: bool| {}));
        // 真的把这一项的动作跑一遍：ctx 由 `windui::testing` 借出（菜单动作收
        // `&mut EventCtx`，只有宿主造得出，故不能直接调）。这样测的是**接线本身**，
        // 而不是被抽出来的那半截逻辑——项挂错了动作照样能被这条断言抓住。
        let windui::event::MenuAction::Run(action) = &item(&items, "隐藏未启用").action else {
            panic!("过滤项应挂着可执行动作");
        };
        windui::testing::run_with_ctx(|ctx| action(ctx));
        assert!(st.hide_disabled.get(), "信号应翻转");
        assert_eq!(saved.get(), Some(true), "并应把新值交给持久化回调");
    }
}

#[cfg(test)]
mod tests {
    use super::reordered_enabled_order;

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn moves_row_to_target_slot() {
        let vis = strs(&["a", "b", "c"]);
        assert_eq!(
            reordered_enabled_order(&vis, &vis, 0, 2).unwrap(),
            strs(&["b", "c", "a"])
        );
        assert_eq!(
            reordered_enabled_order(&vis, &vis, 2, 0).unwrap(),
            strs(&["c", "a", "b"])
        );
    }

    #[test]
    fn expands_empty_order_from_visible_rows() {
        // enabled_order 为空 = "全部启用"，此时可见行才是真顺序，拖完要把它展开落地。
        let vis = strs(&["a", "b", "c"]);
        assert_eq!(
            reordered_enabled_order(&vis, &[], 0, 1).unwrap(),
            strs(&["b", "a", "c"])
        );
    }

    #[test]
    fn keeps_order_entries_that_have_no_visible_row() {
        // order 里的 x 在已启用区没有对应行（特殊方案 / 已卸载但仍在 available 里）。
        // 拖一次顺序不该把它从 available 里抹掉。
        let vis = strs(&["a", "b"]);
        let order = strs(&["a", "x", "b"]);
        assert_eq!(
            reordered_enabled_order(&vis, &order, 1, 0).unwrap(),
            strs(&["b", "a", "x"])
        );
    }

    #[test]
    fn out_of_range_source_is_rejected() {
        assert!(reordered_enabled_order(&strs(&["a"]), &[], 3, 0).is_none());
    }
}

/// 「进入方式」热键的**端到端保存链路**：编辑工作态 → 内存表 → writer → diff。
///
/// 这条链上的每一环单独看都对，合起来却出过「配了热键、点保存说『没有变更』」——
/// 单测各测一环验不出来，故在此从 `AppState` 一路走到 diff。
#[cfg(test)]
mod hotkey_save_pipeline_tests {
    use crate::config::Config;
    use crate::state::AppState;
    use serde_json::json;

    /// 配一个新热键之后，主保存的 diff 必须**包含** `keys.key_actions`。
    ///
    /// 空 diff 就是用户看到的「没有变更」——而他明明刚配完键。
    #[test]
    fn editing_entry_hotkey_produces_a_nonempty_diff() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        // 基线：用户原本已有一条别的绑定，用来验证写回不会把它冲掉。
        let base = json!({
            "keys": { "key_actions": { "ctrl+shift+n": "toggle_schema:english" } }
        });
        st.base_config = Config::from_value(base.clone());
        st.current_config = Config::from_value(base);
        *st.schema_mgr.key_actions.borrow_mut() = [(
            "ctrl+shift+n".to_string(),
            "toggle_schema:english".to_string(),
        )]
        .into_iter()
        .collect();

        // 用户在方案对话框里给 wubi86 配了 Ctrl+Shift+R。
        *st.schema_mgr.settings_schema_id.borrow_mut() = "wubi86".to_string();
        st.schema_mgr.settings_hotkey_enabled.set(true);
        st.schema_mgr.settings_hotkey_raw.set("ctrl+shift+r".into());
        st.schema_mgr.commit_hotkey_edit();

        let items = st.debug_diff_after_writers();
        let ka = items
            .iter()
            .find(|it| it.key == "keys.key_actions")
            .unwrap_or_else(|| {
                panic!(
                    "diff 里没有 keys.key_actions ⇒ 保存时会说「没有变更」；实际 diff: {items:?}"
                )
            });
        let obj = ka.value.as_object().expect("key_actions 应是对象");
        assert_eq!(
            obj.get("ctrl+shift+r").and_then(|v| v.as_str()),
            Some("switch_schema:wubi86"),
            "新配的热键没进写回值"
        );
        assert_eq!(
            obj.get("ctrl+shift+n").and_then(|v| v.as_str()),
            Some("toggle_schema:english"),
            "写回把用户原有的绑定冲掉了"
        );
    }

    /// ★ 回归守卫：`FieldRegistry::reset()` 之后，方案页那批登记必须仍然在。
    ///
    /// `shell::build` 开头会 reset 整个注册表（主窗可关掉再开），而这批登记原先只在
    /// `AppState::empty()` 里做一次、且 `empty()` 早于 `shell::build` —— 于是设置树一
    /// 建好它们就全没了：热键改动进不了 diff，保存时提示「没有变更」。manifest 那批
    /// writer 因为在 `shell::build` 里注册而照常重建，**所以症状是「别的设置能存、只有
    /// 方案页存不了」**，极易误判成方案页自身的问题（真机上误判了整整两天）。
    ///
    /// 这里直接模拟那个顺序：reset 之后重新登记，再验热键改动仍能产出 diff。
    #[test]
    fn builtin_registrations_survive_registry_reset() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        let base = json!({ "keys": { "key_actions": {} } });
        st.base_config = Config::from_value(base.clone());
        st.current_config = Config::from_value(base);

        // shell::build 的开头：清空整棵设置树的登记项……
        st.fields.reset();
        assert!(
            st.fields.writers.borrow().is_empty(),
            "前提：reset 确实清空了 writers，否则本用例证明不了什么"
        );
        // ……随后必须把方案页那批补回来（真实调用点在 shell::build 里）。
        crate::state::register_schema_registrations(&st.fields, &st.schema_mgr);

        *st.schema_mgr.settings_schema_id.borrow_mut() = "wubi86".to_string();
        st.schema_mgr.settings_hotkey_enabled.set(true);
        st.schema_mgr.settings_hotkey_raw.set("ctrl+shift+r".into());
        st.schema_mgr.commit_hotkey_edit();

        let items = st.debug_diff_after_writers();
        assert!(
            items.iter().any(|it| it.key == "keys.key_actions"),
            "reset 后 key_actions writer 缺席 ⇒ 方案热键永远存不下：{items:?}"
        );
        assert!(
            !st.fields.side.borrow().is_empty(),
            "SideCommitter 也要补回来，否则方案 override / 词库开关同样存不下"
        );
    }

    /// ★★ 与上一条配套：钉住 `shell::build` **确实调用**了那个补登记的函数。
    ///
    /// 上一条测试自己调 `register_schema_registrations`，所以它只证明「这个函数有效」，
    /// 证明不了「有人调它」——把 shell.rs 里的真实调用点删掉，它照样绿（本条守卫就是
    /// 在那次改坏验证里补出来的）。**行为测试与调用点守卫缺一不可**：前者防函数写错，
    /// 后者防调用点丢失，而本次事故恰恰是后一种。
    ///
    /// 用源码扫描是因为 `shell::build` 要真实 UI 运行时才跑得起来，测试里进不去。
    #[test]
    fn shell_build_reregisters_after_reset() {
        const SHELL_SRC: &str = include_str!("../shell.rs");
        let reset_at = SHELL_SRC
            .find("st.fields.reset();")
            .expect("shell::build 里找不到 fields.reset()（改名了？守卫需同步更新）");
        let reregister_at = SHELL_SRC
            .find("register_schema_registrations(")
            .expect(
                "shell::build 没有补回方案页的登记 ⇒ reset 之后热键/方案启用/设为当前/                 方案 override 全部保存不了，且症状是「别的设置能存、只有方案页存不了」",
            );
        assert!(
            reregister_at > reset_at,
            "补登记必须排在 reset **之后**，否则照样被清空"
        );
    }

    /// 反向对照：什么都没改时 diff 必须为空。
    ///
    /// 少了这条，上面那条在「diff 恒非空」（比如 writer 每次都产出不同的表）时也会绿，
    /// 而那种缺陷的表现是关窗永远被拦着问「要不要保存」。
    #[test]
    fn untouched_state_produces_an_empty_diff() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        let base = json!({
            "keys": { "key_actions": { "ctrl+shift+n": "toggle_schema:english" } }
        });
        st.base_config = Config::from_value(base.clone());
        st.current_config = Config::from_value(base);
        *st.schema_mgr.key_actions.borrow_mut() = [(
            "ctrl+shift+n".to_string(),
            "toggle_schema:english".to_string(),
        )]
        .into_iter()
        .collect();

        let items = st.debug_diff_after_writers();
        assert!(
            !items.iter().any(|it| it.key == "keys.key_actions"),
            "没动过就不该产出 key_actions 改动：{items:?}"
        );
    }
}
