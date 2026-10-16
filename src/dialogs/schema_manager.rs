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
    /// 协议链接下载来的临时包：随待导入项释放（确认、取消或被下一个包顶替）即删文件。
    Downloaded(Rc<crate::schema_url_import::DownloadedPackage>),
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

/// 拉双拼布局清单 (id, 显示名)；拿不到时为空。方案管理与设置向导共用。
pub(crate) fn fetch_shuangpin_layouts() -> Vec<(String, String)> {
    rpc::call_core("schema.layouts", &json!({}))
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
        .unwrap_or_default()
}

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
    /// 本方案行为：切到该方案时的标点态 / 候选排列 / 要不要加载短语
    /// （方案文件 `[punct]` / `[candidate]` / `[phrases]` 三段）。**所有方案都有**。
    ///
    /// 与 [`Self::Overlay`] 的「段存在才建节」不同：那三段是通用能力，默认值就是
    /// 「跟随全局」，方案文件里不写才是常态——要求先手写方案文件才能配，等于没有。
    Behavior,
    ExtensionDicts,
    /// 节间分隔线（Header 自带结尾分隔，故只在其后的各节之间插入）。
    Divider,
    Empty,
}

/// 方案设置对话框的三个 TAB。
///
/// ⚠️ **数量反转过两次**，两次的理由都成立、只是当时的页面不同：
/// 1. 最初三个 → 合成两个：「按键配置」那时只有四行，单开一页显得空。
/// 2. 两个 → 又拆回三个（2026-08-27，用户提出）：基本配置页此后长到了
///    「元信息 + 五行按键 + 扩展词库 + 双拼参数」，按键那几行占掉大半页，
///    而它们是**配一次就不再看**的东西，天天要看的元信息与词库反被挤下去。
///
/// ⇒ 判据不是「这一页够不够满」，而是「**常看的东西有没有被不常看的挤走**」。
pub const SETTINGS_TABS: [&str; 3] = ["基本配置", "按键设置", "方案自定义"];

/// 「方案自定义」在 [`SETTINGS_TABS`] 里的下标（TAB 星号打在这一页）。
pub const CUSTOM_TAB: usize = 2;

impl SettingsSection {
    /// 全部分节，供 `every_settings_section_builds_without_panicking` 遍历。
    ///
    /// ⚠️ **新增变体后必须加进这里**——这份列表是手写的，编译器不会提醒。
    /// 漏加的后果不是「少测一节」，而是那一节的 `.small()` / `.weight()` 之类误用
    /// 完全没有防护网：windui 的链式 API 是**运行时 downcast**，`.small()` 落到
    /// 非 Button/CheckBox/Switch 的 widget 上会在 debug 构建下当场 panic，
    /// 而编译与其余测试全绿（本仓已因此崩过一次）。
    ///
    /// 放在 enum 定义旁边而不是测试里，就是为了让新增变体的人看得见它。
    pub const ALL: &'static [Self] = &[
        Self::Header,
        Self::EntryKeys,
        Self::ShuangpinLayout,
        Self::AuxCode,
        Self::CodeTable,
        Self::Overlay,
        Self::Behavior,
        Self::ExtensionDicts,
        Self::Divider,
        Self::Empty,
    ];

    /// 本节归到哪个 TAB。
    ///
    /// 分组判据是**用户想做什么**，不是数据落在哪个文件：
    /// - 基本配置：这个方案是什么、带哪些词库、双拼用哪套布局——日常会看的都在这一页
    /// - 按键设置：怎么切进来、键怎么按——**配一次就不再看**，故单独一页
    /// - 方案自定义：这个方案的引擎行为要不要偏离全局（码表覆盖 / 特殊模式 / 本方案行为）
    ///
    /// ★ 双拼布局归**基本配置**而不是方案自定义：它是「这个方案是什么」的一部分
    /// （换一套布局等于换一种打法），不是「要不要偏离全局」——方案自定义那一页的每一项
    /// 都有「跟随全局」这个档，双拼布局没有。
    ///
    /// `Divider` 不参与分组：原先那份扁平列表里的分隔线是按「相邻两节之间」插的，分完 TAB
    /// 后相邻关系全变了，故丢弃重插（见 `split_sections_by_tab`）。
    fn tab(self) -> Option<usize> {
        match self {
            Self::Header | Self::ShuangpinLayout | Self::ExtensionDicts => Some(0),
            Self::EntryKeys => Some(1),
            Self::AuxCode | Self::CodeTable | Self::Overlay | Self::Behavior => Some(2),
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
fn split_sections_by_tab(flat: &[SettingsSection]) -> [Vec<SettingsSection>; SETTINGS_TABS.len()] {
    let mut out: [Vec<SettingsSection>; SETTINGS_TABS.len()] = Default::default();
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

/// 「本方案行为」节三个下拉的统一宽度。要装得下最长的跟随档「跟随全局（沿用当前状态）」，
/// 170 会把它折成两行（最终审查目视发现）；三个下拉同宽，右侧才对得齐。
const BH_DD_W: i32 = 220;

/// 辅助码卡片里步进器（及「候选文字字体」输入框）**自身**的宽度。控件位容器宽是
/// [`AUX_SRC_W`]，控件在容器里靠右。
const AUX_CTRL_W: i32 = 150;
/// 辅助码卡片三行的**控件位**宽（`override_control` 的容器）。由「辅助码来源」下拉定：
/// 要放得下最长的「五笔 86」+ 徽章 + 下拉箭头。三行同用它，来源列（跟随 / 已自定义）
/// 才在同一竖线上；控件在容器里靠右，开关、数字框仍按 `AUX_CTRL_W` 自身宽度画。
const AUX_SRC_W: i32 = 220;

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

/// `[engine.aux_code]` 「启用」与「最长词长」两行**各自独立**的落盘值。
///
/// ★ 两个勾选框而不是一个三档分段：`enabled` 与 `max_phrase_len` 是方案文件里两个
/// 各自独立的键，「跟不跟」也该各自表达——旧版用一个三档分段把两件事拧在一起，
/// 装不下后来加的 `max_phrase_len` 覆盖需求，才有了这次改法。
///
/// 未勾的一位写 `Value::Null`：core 的 `json_to_toml` 跳过 null，该键不进 override，
/// 取值回落全局——这正是「跟随全局」的落地方式，不是丢失。
fn aux_write(on_over: bool, on: bool, len_over: bool, len: i32) -> (Value, Value) {
    let enabled = if on_over { json!(on) } else { Value::Null };
    let max_len = if len_over { json!(len) } else { Value::Null };
    (enabled, max_len)
}

/// 「本方案直接辅助码」（`[engine.aux_code].direct`）的落盘值，口径同 [`aux_write`]：
/// 未勾写 `Value::Null`（回落全局）。与那两行分开算、分开写，理由见 [`seed_aux_direct`]。
fn aux_direct_write(over: bool, on: bool) -> Value {
    if over { json!(on) } else { Value::Null }
}

/// 「本方案直接辅助码」一行打开时的 `(勾选, 控件值)`。控件值取生效值（覆盖有则用它，
/// 否则跟随值 `followedBehavior.auxDirect`），同「本方案启用辅助码」。
///
/// ★ 非双拼方案恒为「不勾」：这一行只在双拼方案显示（core 只认双拼，全拼写了也不生效）。
/// 若全拼方案的覆盖里手写了 `direct`，勾选态报 `true` 的话，看不见的一行会计进「已自定义
/// N 项」，「全部恢复跟随」还会把它清成 null——替用户改了一个他在界面上看不到的值。
/// 恒为不勾则打开 / 保存前后落盘结果都是 null、不写，手写的那个键原样留着。
fn seed_aux_direct(cfg: &Value, is_sp: bool) -> (bool, bool) {
    let followed = cfg
        .pointer("/followedBehavior/auxDirect")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let raw = cfg
        .pointer("/engine/aux_code/direct")
        .and_then(Value::as_bool)
        .filter(|_| is_sp);
    (raw.is_some(), raw.unwrap_or(followed))
}

/// 布尔跟随值的展示文案：「开」/「关」。数字项直接 `to_string()`，用不上这一层。
fn follow_text_bool(v: bool) -> String {
    if v {
        "开".to_string()
    } else {
        "关".to_string()
    }
}

/// 候选字体跟随值的展示文案：空串（外观页没配字体，即系统默认）显示「系统字体」，
/// 否则原样显示字族名。与 [`follow_text_bool`] 同类的小型展示层，不算三态本身。
fn follow_text_font(v: &str) -> String {
    if v.is_empty() {
        "系统字体".to_string()
    } else {
        v.to_string()
    }
}

/// 「跟随」档下拉首项的文案：`跟随{scope}（{value_label}）`（R8.2，全角括号）。
///
/// 值必须带出来——「跟随全局」不带值时用户不知道跟的是什么，得切到全局页才能确认；
/// 跟随值变了这一档的文案要跟着变，故调用方每次打开对话框都要重算，不能缓存首次种下
/// 的文案（见 `follow_labels_reseeded_on_each_open`）。
fn follow_option_label(scope: &str, value_label: &str) -> String {
    format!("跟随{scope}（{value_label}）")
}

/// `followedBehavior.layout`（`"horizontal"` / `"vertical"`）→ 中文标签。
///
/// 与 [`overlay_layout_idx`] 是两回事：那个映射的是**本方案覆盖值**的三档下标
/// （含「跟随全局」），这个映射的是**全局当前生效值**（只有横/竖两种，没有「跟随」
/// 这一档——全局页本身就是跟随的终点）。未知值回落「横排」，同各处「不猜方向」的口径，
/// 但这里的输入来自 core 只读旁路字段，不是用户手写，出现未知值基本不会发生。
fn layout_label(v: &str) -> &'static str {
    match v {
        "vertical" => "竖排",
        _ => "横排",
    }
}

/// 候选排列跟随档下拉的完整标签表：首项按 `followed_layout` 现算，其余固定。
///
/// 「本方案行为」的候选排列与 overlay 的候选排列是**同一个全局跟随值**，共用这一份
/// 计算——两处各写一遍的话，其中一处忘了随 `followedBehavior` 重算，用户就会看到
/// 两个「跟随全局」档显示不同的值。
fn layout_dropdown_labels(followed_layout: &str) -> Vec<String> {
    vec![
        follow_option_label("全局", layout_label(followed_layout)),
        "竖排".to_string(),
        "横排".to_string(),
    ]
}

/// 「最长词长」的合法范围钳制，与全局 manifest 该项的 `min`/`max`（0..=20，见
/// `settings_manifest.toml` 的 `schema.pinyin.aux_code.max_phrase_len`）同口径。
///
/// ⚠️ 手改 `config.toml` 能写出这个范围（跟随值、方案覆盖值都可能来自手写文件），
/// 两处读取（种子取控件初值、UI 取跟随值展示）都要钳，否则界外的数字会被当作
/// 「生效值」直接喂给 stepper（它的 min/max 只管用户在控件里拖，挡不住程序塞进去的
/// 初值），或者原样显示在跟随值文案里。
fn clamp_aux_len(v: i64) -> i32 {
    v.clamp(0, 20) as i32
}

/// 辅助码卡片「全部恢复跟随」按钮做的事：各行勾选一并清空（含来源一行的只读态），
/// 控件值拨回跟随值，摘要归零。
///
/// ★ 抽出来供测：按钮的 `on_click` 闭包本身测不到（只能在真机上点），闭包只转调这里，
/// 测试直接拿 `SchemaManagerState` 的真信号驱动它。
///
/// 控件值也拨回跟随值——同 `schema_codetable::CtFields::revert_all`：只清勾选不清值的话，
/// 用户下次再勾上会看到上一次改的旧值，而不是真正的跟随值。
fn aux_reset_all(
    state: &SchemaManagerState,
    followed_on: bool,
    followed_len: i32,
    followed_direct: bool,
) {
    state.settings_aux_on_over.set(false);
    state.settings_aux_len_over.set(false);
    state.settings_aux_direct_over.set(false);
    state.settings_aux_on.set(followed_on);
    state.settings_aux_len.set(followed_len as f64);
    state.settings_aux_direct.set(followed_direct);
    // 来源这一行也清：只读态一并清掉，保存时才会走正常路径落 null（回落方案文件）。
    state.settings_aux_src_over.set(false);
    state.settings_aux_src_manual.set(false);
    state
        .settings_aux_src_idx
        .set(state.settings_aux_src_follow_idx.get());
    state.settings_aux_disp.set(ct_summary(0));
}

/// 「辅助码来源」下拉的一项。`entry` 是写进 `[engine.aux_code].files` 的原文：
/// 码表方案写 `schema:<id>`，码表文件写相对数据目录的路径——与 core `AuxCodeSpec`
/// 解析 `files` 的口径一致，下拉选中哪项就原样落哪串，中间不做翻译。
#[derive(Debug, Clone, Default)]
struct AuxSourceItem {
    entry: String,
    label: String,
    /// 下拉按它给每项挂「输入方案 / 辅助码表」徽章。
    is_schema: bool,
}

/// `schema.auxCodeSources` 的结果：方案在前、码表在后（下拉即按此顺序）。
///
/// RPC 失败时是 `Default`（空表）——下游据此区分「判不了装没装」与「确实没装」，
/// 见 [`aux_src_follow_text`]。
#[derive(Debug, Clone, Default)]
struct AuxSourceOpts {
    items: Vec<AuxSourceItem>,
}

impl AuxSourceOpts {
    fn label_of(&self, entry: &str) -> Option<&str> {
        self.items
            .iter()
            .find(|i| i.entry == entry)
            .map(|i| i.label.as_str())
    }
    fn index_of(&self, entry: &str) -> Option<usize> {
        self.items.iter().position(|i| i.entry == entry)
    }
}

/// 解析 `schema.auxCodeSources` 的返回。缺 id / path 的条目丢掉——它们写不出一条
/// 有效的 `files` 原文，放进下拉只会让用户选中一个落盘后 core 认不出的来源。
fn parse_aux_source_opts(v: &Value) -> AuxSourceOpts {
    let arr = |k: &str| {
        v.get(k)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let s = |x: &Value, k: &str| x.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let mut items: Vec<AuxSourceItem> = arr("schemas")
        .iter()
        .filter(|x| !s(x, "id").is_empty())
        .map(|x| AuxSourceItem {
            entry: format!("schema:{}", s(x, "id")),
            label: s(x, "name"),
            is_schema: true,
        })
        .collect();
    items.extend(
        arr("files")
            .iter()
            .filter(|x| !s(x, "path").is_empty())
            .map(|x| AuxSourceItem {
                entry: s(x, "path"),
                label: s(x, "label"),
                is_schema: false,
            }),
    );
    AuxSourceOpts { items }
}

/// 跟随档括号里的文字：方案文件声明的来源名，多个用「+」连接；可选项里没有的标「（未安装）」。
///
/// 可选项整个拿不到（RPC 失败 / 空）时退回原文——那时判不了装没装，不能乱报缺失，
/// 否则老核心（没有 `schema.auxCodeSources`）下每个方案都会显示「未安装」。
fn aux_src_follow_text(base: &[String], opts: &AuxSourceOpts) -> String {
    if base.is_empty() {
        return "无".to_string();
    }
    base.iter()
        .map(|e| match opts.label_of(e) {
            Some(l) if !l.is_empty() => l.to_string(),
            Some(_) => e.clone(),
            None if opts.items.is_empty() => e.clone(),
            None => format!("{e}（未安装）"),
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// 只读态控件位的文字：报**现状**——这份值是手写进方案覆盖（`schema_overrides/<id>.toml`）
/// 的，不是方案文件声明的，所以不能指路「去方案文件里改」（改了也不生效）。撤掉它的路
/// 在界面上：取消勾选即恢复跟随（见 [`aux_src_locked`]），由行上的悬停提示说明。
///
/// 名字取 [`aux_src_follow_text`] 同一口径，未安装的已带「（未安装）」。单条（下拉里
/// 没有的那种）不说「多个」。
fn aux_src_manual_text(merged: &[String], opts: &AuxSourceOpts) -> String {
    let head = if merged.len() > 1 {
        "手写的多个来源"
    } else {
        "手写的来源"
    };
    format!("{head}：{}", aux_src_follow_text(merged, opts))
}

/// 打开时「辅助码来源」这一行的状态。
///
/// ★ 判据只看「合并值是否等于方案文件自己的值」（core 旁路字段 `auxCodeBaseFiles`）——
/// 相等就是跟随。即便 override 里手写了一份相同的，core 的 `json_diff` 保存时也会把它
/// 当无差异删掉，所以这里判成跟随与落盘结果同口径，不会出现「显示覆盖、存完变跟随」。
#[derive(Debug)]
enum AuxSrcSeed {
    Follow,
    /// 单条且在下拉里：选中第几项。
    Pick(usize),
    /// 多条或下拉里没有的条目：下拉表达不了，只读展示，保存原样不动。
    /// 携带合并值原文，只读态据此报出各条名字。
    Manual(Vec<String>),
}

fn seed_aux_src(merged: &[String], base: &[String], opts: &AuxSourceOpts) -> AuxSrcSeed {
    if merged == base {
        return AuxSrcSeed::Follow;
    }
    match merged {
        [one] => opts
            .index_of(one)
            .map_or_else(|| AuxSrcSeed::Manual(merged.to_vec()), AuxSrcSeed::Pick),
        _ => AuxSrcSeed::Manual(merged.to_vec()),
    }
}

/// 「辅助码来源」此刻是否处在只读态：打开时 seed 出只读（`manual`）**且**仍勾着。
///
/// ★ 只读态下取消勾选 = 离开只读、回到跟随：界面随即显示「跟随方案」，保存落 null。
/// 不这样做的话，勾选框能被点掉、摘要也跟着少计一项，保存却因只读跳过这一行——
/// 用户看到的与落盘的对不上。没选「把勾选框置灰」：只读的原值是用户（或早先版本）
/// 手写进 override 的，设置端至少要给一条不打开方案文件就能撤掉它的路；而重新勾上
/// 又回到只读态（`manual` 没被清），不会凭空把下拉当前项写进去。
fn aux_src_locked(over: bool, manual: bool) -> bool {
    over && manual
}

/// 「辅助码来源」的落盘值：不勾写 `null`（core 跳过 null ⇒ 该键不进 override ⇒ 回落
/// 方案文件），勾上写单元素数组。勾上却没有可选项（`pick` 为 `None`）也写 `null`——
/// 写一个空数组等于替用户关掉辅助码表，那是另一件事。
fn aux_src_write(over: bool, pick: Option<&str>) -> Value {
    match (over, pick) {
        (true, Some(e)) => json!([e]),
        _ => Value::Null,
    }
}

/// 「本方案行为」节「全部恢复跟随」的核心逻辑（R8.5）：把有「跟随」档的三项一并
/// 拨回跟随（标点 / 候选排列下标回 0，候选字体取消勾选）。
///
/// ⚠️ 「横排时文字排列」不在这里：它没有跟随档（出厂就是「默认」，全局页压根没有
/// 这一项），按下这个按钮不该动它——动了等于给一个不存在的状态强行赋值。
/// 注释模板、自定义标点表也不在这里：它们是各自独立的子对话框，各有自己的清空入口，
/// 这个按钮只清「本方案行为」节直接铺开显示的这三项（R8.5：只清本区块界面上可见的）。
///
/// 抽出来的理由同 [`aux_reset_all`]：闭包测不到，测试拿真信号驱动这里。
fn behavior_reset_all(state: &SchemaManagerState) {
    state.settings_bh_punct.set(0);
    state.settings_bh_layout.set(0);
    state.settings_bh_font_over.set(false);
}

/// 候选字体（`[candidate] font_family`）的落盘值：未勾选，或勾选但留空，都写
/// `Value::Null`（core 的 `json_to_toml` 跳过 null，该键不进 override，取值回落
/// 外观页的字体设置）。只有勾选且非空才落成具体字族。
///
/// 「勾了但留空」不落成空串——那会被 core 读成「本方案覆盖成空字体」，与用户实际想要
/// 的「跟随外观页」正好相反，且界面上看不出区别（勾上的文本框本来就可能是空的）。
fn font_write(over: bool, family: &str) -> Value {
    if over && !family.is_empty() {
        json!(family)
    } else {
        Value::Null
    }
}

/// `[overlay] candidate_layout` 的字符串值 ↔ 分段控件下标。
///
/// 值域与 core 的 `LayoutIntent` 逐字一致（`follow` / `vertical` / `horizontal` /
/// `rotated`）——跨仓契约无编译期约束，拼错的表现是 core 解析回落默认值、用户改了没反应。
///
/// ⚠️ **core 加取值时这里必须跟着加**，否则不只是「选不到新值」：读取走
/// [`overlay_layout_idx`]（未知值 → 0），保存走 [`overlay_layout_value`]（0 → `follow`）
/// ⇒ 用户手写的新值会在**打开设置页再保存**时被静默改写成「跟随全局」。
/// `rotated`（顺时针 90°，蒙古文等纵向书写脚本）就是这么补进来的。
const OVERLAY_LAYOUTS: [&str; 3] = ["follow", "vertical", "horizontal"];

/// 与 [`OVERLAY_LAYOUTS`] 逐项对应的中文标签。R8.2 改勾选首项带值之后，两处下拉
/// 改用 [`layout_dropdown_labels`] 现算，不再直接用这份静态表——只留给
/// `layout_value_domain_matches_core_capability` 校验标签数与值域数对得上，故标 `cfg(test)`。
#[cfg(test)]
const OVERLAY_LAYOUT_LABELS: [&str; 3] = ["跟随全局", "竖排", "横排"];

/// 方案级「横排时文字排列」（`[candidate] text_orientation`）的取值与标签。
///
/// ⚠️ 它**不是** [`OVERLAY_LAYOUTS`] 的延长——两者是正交的两根轴。旋转 90° 与文字竖排
/// 曾是候选排列的第三、第四个取值，作废理由见 core 的 `TextOrientation` 文档；
/// 这里若把它们混回同一个下拉，那次拆分就白做了。
///
/// 无「跟随全局」档：外观页压根没有这一项（它面向所有用户，而这是极少数方案才用的），
/// 首档就是出厂行为「默认」。
const TEXT_ORIENTS: [&str; 3] = ["normal", "rotated", "upright"];
const TEXT_ORIENT_LABELS: [&str; 3] = ["默认（水平）", "旋转 90°", "文字竖排（对联式）"];

fn text_orient_idx(v: &str) -> usize {
    TEXT_ORIENTS.iter().position(|x| *x == v).unwrap_or(0)
}

/// 下标 → 字符串值。越界回落首项（默认），与 [`overlay_layout_value`] 同口径。
fn text_orient_value(idx: usize) -> &'static str {
    TEXT_ORIENTS.get(idx).copied().unwrap_or(TEXT_ORIENTS[0])
}

fn overlay_layout_idx(v: &str) -> usize {
    OVERLAY_LAYOUTS.iter().position(|x| *x == v).unwrap_or(0)
}

/// 分段控件下标 → `[overlay] candidate_layout` 的字符串值。
///
/// ★ 越界回落**首项**（跟随全局）而不是钳到末项：钳会把用户没选过的「横排」塞给他。
/// 与 [`overlay_layout_idx`] 对未知值的处理、以及 core `layout.rs`「下标越界回落
/// Follow——跟随全局是安全的默认，不猜方向」同口径。
/// 「本方案行为」节的工作态快照：既是播种产物，也是脏判定的比较对象。
///
/// 从元组换成结构体，是因为注释模板进来之后它有五项、且其中两项是三态的
/// `Option<String>`——元组位置越多，脏判定那处 `now != at_open` 的可读性越差。
#[derive(Clone, PartialEq, Eq, Debug)]
struct BehaviorSeed {
    punct: usize,
    layout: usize,
    phrases: bool,
    /// 竖排注释模板三态：`None` = 键缺失（跟随全局），`Some("")` = 本方案不显示注释。
    comment_v: Option<String>,
    comment_h: Option<String>,
    /// 方案级自定义标点表三态，与 core `PunctSpec::custom_mappings` 同构：
    /// `None` = 跟随全局（键缺失）；`Some(表)` = **整份替换**全局表；`Some({})` = 一条都不要。
    ///
    /// ⚠️ 与上面两个注释模板一样，三态**只能由「键在不在」区分**，不能拿空表代表跟随——
    /// 那正是 core 侧刻意不加方案级 `custom_enabled` 的理由（空表已经是第三态）。
    punct_map: Option<serde_json::Map<String, Value>>,
    /// 方案级候选**文字**字族（`[candidate] font_family`）三态：`None` = 未勾选覆盖
    /// （写回 `null`）；`Some(s)` = 本方案覆盖为 `s`。
    ///
    /// R8.1 改勾选行之前它是朴素 `String`（空串 = 不覆盖）；改勾选行后「没有勾」与
    /// 「勾了但没填」都写 `null`（见 [`font_write`]），这里用 `Option` 与注释模板同构，
    /// 免得又造一个只能靠“空串”表达跟随的哨兵。读取时旧文件里的 `""` 仍视为未设置
    /// （见 [`seed_behavior`]），兼容改字段前存下的方案文件。
    font_family: Option<String>,
    /// 横排时文字排列（`[candidate] text_orientation`）：0=默认 / 1=旋转 90° / 2=文字竖排。
    ///
    /// 与 `layout` 是**两根正交的轴**，不是同一个下拉的延长——改这里不影响横竖，
    /// 改横竖也不影响这里。
    text_orient: usize,
}

impl Default for BehaviorSeed {
    /// ★ 与 core 各段的 `Default` 同源。**别 derive**：`phrases` 的出厂值是 `true`，
    /// derive 出来的 `false` 会让绝大多数方案在界面上显示成「不加载短语」。
    fn default() -> Self {
        Self {
            punct: 0,
            layout: 0,
            phrases: true,
            comment_v: None,
            comment_h: None,
            punct_map: None,
            font_family: None,
            text_orient: 0,
        }
    }
}

/// 从方案配置里读出「本方案行为」各项的初值（纯函数，供测试）。
///
/// ★ **短语默认 `true`**：与 core `PhrasesSpec::default()` 同源。取 `false` 作默认会让
/// 所有没写这段的方案（也就是绝大多数）在界面上显示成「不加载短语」，而实际是加载的
/// ——用户照着界面去开一下，反而在 override 里写死了一个本来就生效的值。
///
/// 两个下标项的未知值一律回落 0（跟随全局），理由见 [`punct_mode_idx`]。
///
/// ★ 注释模板读的是**有没有这个键**，不是「值空不空」：`Some("")` 是一个有意义的取值
/// （本方案不显示注释），把它和键缺失混为一谈，三态就塌成了两态。
fn seed_behavior(cfg: &Value) -> BehaviorSeed {
    let punct = cfg
        .get("punct")
        .and_then(|v| v.get("mode"))
        .and_then(|v| v.as_str())
        .map(punct_mode_idx)
        .unwrap_or(0);
    let candidate = cfg.get("candidate");
    let layout = candidate
        .and_then(|v| v.get("layout"))
        .and_then(|v| v.as_str())
        .map(overlay_layout_idx)
        .unwrap_or(0);
    let phrases = cfg
        .get("phrases")
        .and_then(|v| v.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    // `as_str()` 顺带滤掉 null：core 侧「取消覆盖」写的就是 null，读回来必须还原成
    // 「未设置」而不是 `Some("null")` 之类。
    let tpl = |k: &str| -> Option<String> {
        candidate
            .and_then(|v| v.get(k))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    // 未勾选 → `null`；旧文件（改字段前存下的）里的 `""` 同样视为未设置——
    // 两者读回来都该是「跟随外观页」，不是「覆盖成空字体」。见 [`font_write`]。
    let font_family = candidate
        .and_then(|v| v.get("font_family"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let text_orient = candidate
        .and_then(|v| v.get("text_orientation"))
        .and_then(|v| v.as_str())
        .map(text_orient_idx)
        .unwrap_or(0);
    BehaviorSeed {
        punct,
        layout,
        font_family,
        text_orient,
        phrases,
        comment_v: tpl("comment_template_vertical"),
        comment_h: tpl("comment_template_horizontal"),
        // `as_object()` 顺带滤掉 null（core「取消覆盖」写的就是 null），与上面模板同理。
        punct_map: cfg
            .get("punct")
            .and_then(|v| v.get("custom_mappings"))
            .and_then(|v| v.as_object())
            .cloned(),
    }
}

/// `[punct] mode` 的字符串值 ↔ 下拉下标。
///
/// 与 [`overlay_layout_idx`] 同构：未知值一律回落 0（跟随全局）。**不猜方向**——
/// 方案文件是可手写的，写错一个词不该被解释成某个具体档位。
fn punct_mode_idx(v: &str) -> usize {
    match v {
        "chinese" => 1,
        "english" => 2,
        _ => 0,
    }
}

/// 下拉下标 → `[punct] mode` 的字符串值。
fn punct_mode_value(idx: usize) -> &'static str {
    match idx {
        1 => "chinese",
        2 => "english",
        _ => "follow",
    }
}

/// 候选注释子对话框里的一行：`标签 + 说明 ······ [☑ 自定义] [模板输入框]`。
///
/// 三态由**两个控件**表达：勾选框回答「本方案要不要覆盖这一项」，输入框只装内容
/// （勾上而留空 = 空串 = 本方案不显示注释）。做成三档下拉的话，「自定义 + 留空」与
/// 「不显示」会是同一个状态的两种走法——`config-design-rules` §R3 反对的那种重复态。
///
/// ⚠️ 输入框用 `enabled_signal` 随勾选置灰而非隐藏：隐藏会让这一行的高度跟着跳，
/// 且用户看不出「这里还能填东西」。`.small()` 只能给勾选框（Button/CheckBox/Switch 专属，
/// 落到 text_input 上会在 debug 构建下当场 panic）。
fn comment_tpl_row(label: &str, hint: &str, on: Signal<bool>, text: Signal<String>) -> Element {
    crate::widgets::setting_row(
        label,
        Some(hint),
        Element::row()
            .cross(Align::Center)
            .spacing(8)
            .child(Element::checkbox("自定义", on).small().fg_role(Role::Text))
            .child(
                Element::text_input(text, "留空则不显示注释")
                    .width(190)
                    .enabled_signal(on),
            ),
    )
}

/// 「候选注释」那一行的摘要（纯函数，供测试）。
///
/// 摘要只看**勾选态**：内容改了但没勾上等于没生效，跟着内容走会提前说「已自定义」。
fn comment_summary(v_on: bool, h_on: bool) -> String {
    match (v_on, h_on) {
        (false, false) => "跟随全局".to_string(),
        (true, false) => "竖排已自定义".to_string(),
        (false, true) => "横排已自定义".to_string(),
        (true, true) => "横竖均已自定义".to_string(),
    }
}

/// 「自定义标点」那一行的摘要（纯函数，供测试）。
///
/// ★ 与 [`comment_summary`] 同构：**只看勾选态**，不看表里有几行。
/// 报行数就得让摘要 effect 依赖全部 4×N 个单元格信号，那是每帧要遍历的开销，
/// 而这一屏已经因为「每帧轮询」付过一次代价。行数打开子对话框一眼就能看见。
///
/// ⚠️ 于是「勾上但一行都没改」（core 侧的 `Some({})`＝本方案一条自定义都不要）
/// 与「勾上且配了几行」在这一行文案上不作区分——两者都是「本方案自己说了算」，
/// 与「跟随全局」的区别才是这行要回答的。
fn punct_map_summary(on: bool) -> String {
    if on {
        "本方案自定义".to_string()
    } else {
        "跟随全局".to_string()
    }
}

/// 方案级自定义标点子对话框：一个「本方案使用自己的表」开关 + 复用全局那张 5 列表格。
///
/// # ★ 为什么复用 `build_punct_table_dialog` 而不是另画一张
///
/// 列序契约（core 存储序 [中半, 英全, 中全, 英半] ↔ 展示序）**只许在
/// `field_dialogs::PUNCT_DISPLAY_COLS` 一处换**。另写一张表就是第二处写死下标，
/// 而两处错位的杀伤力是「中文半角状态永远输出英文字符」，且不报错——本仓已栽过一次。
///
/// # ★ 表格里的基线仍是**内置默认**，不是全局表
///
/// 与全局那处同构：只有「与内置默认不同」的行会被写进方案表。这不是偷懒——整表替换下
/// 方案表里没写的行本来就走 core 内建转换，写进去反而是一堆恒等的冗余行。
///
/// ⚠️ 于是有一条用户可见的后果，hint 里必须说：**开启后，全局表里配的那些行不再生效**。
fn build_schema_punct_dialog(state: &SchemaManagerState) -> Element {
    use crate::dialogs::field_dialogs::{PunctTableHeader, build_punct_table_dialog};

    let on = state.settings_bh_punct_map_on;
    let cells: Vec<[Signal<String>; 4]> = state.settings_bh_punct_cells.as_ref().clone();
    // 摘要不在这里推：它随勾选态走，由「码表覆盖计数 + 候选注释摘要」那个统一 effect
    // 一并刷新（另开一个 effect 只会多一处要同步的口径）。
    let header = crate::widgets::setting_row(
        "使用本方案的标点表",
        Some("开启后全局的自定义标点整份不再生效；关闭则完全跟随全局"),
        Element::checkbox("开启", on).small().fg_role(Role::Text),
    );
    let src_labels: Vec<String> = crate::manifest::PUNCT_DEFAULTS
        .iter()
        .map(|(s, _, _)| s.to_string())
        .collect();
    let defaults: Vec<[String; 4]> = crate::manifest::PUNCT_DEFAULTS
        .iter()
        .map(|(_, _, d)| std::array::from_fn(|i| d[i].to_string()))
        .collect();
    build_punct_table_dialog(
        state.settings_bh_punct_map_visible,
        "本方案的自定义标点",
        src_labels,
        cells,
        defaults,
        // ★ 开关必须管住表格：未勾选时 writer 写的是 null（`punct_map_value`），表里改什么
        // 都不落盘。不置灰的话用户改完一整页、保存、毫无变化且零提示——真机反馈即此。
        Some(PunctTableHeader {
            element: header,
            gate: on,
        }),
        "点单元格编辑替换字符（留空恢复该格默认）；仅与默认不同的行会写进本方案。\n未开启时表格不可编辑，本方案完全跟随全局的自定义标点设置。",
    )
}

/// 表格工作态 → 方案表（只收「与内置默认不同」的行，判据与全局那处**同一个函数**）。
///
/// `extra` 是方案表里 `PUNCT_DEFAULTS` **管不到的源字符**，原样带回去。
/// ⚠️ 不带的话它们每次保存都会消失：方案表是**整表替换**，表格没覆盖到的行不是「保持
/// 原样」而是「不存在」。全局那处能容忍丢弃（那边的归一只作用于 diff 基线），这里不能。
fn collect_punct_map(
    cells: &[[Signal<String>; 4]],
    extra: &serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    let mut m = extra.clone();
    for (row, (_, tok, defs)) in cells.iter().zip(crate::manifest::PUNCT_DEFAULTS.iter()) {
        let vals: [String; 4] = std::array::from_fn(|i| row[i].get());
        if let Some(arr) = crate::manifest::punct_row_override(&vals, defs) {
            m.insert((*tok).to_string(), arr);
        }
    }
    m
}

/// 方案表里 `PUNCT_DEFAULTS` 管不到的源字符（表格没有对应行，故只能原样透传）。
fn punct_extra_rows(raw: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    raw.iter()
        .filter(|(k, _)| {
            !crate::manifest::PUNCT_DEFAULTS
                .iter()
                .any(|(_, tok, _)| tok == k)
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// ★★ 把方案表归一成「**表格反算出来的形状**」——脏判定基线必须用它，不能用配置原文。
///
/// 两处不对称会让「什么都没改」恒判为脏（[`crate::manifest::punct_row_override`] 的文档
/// 里写着同一条）：
///
/// 1. 方案作者手写 `"." = ["。"]` 这种**缺列**的行，播种时缺格回落默认，反算回来四格全
///    等默认 ⇒ 被裁掉，而原文里有；
/// 2. 四格恰好全等内置默认的行，同样被裁掉。
///
/// 表现是**打开方案设置看一眼再关掉就提示有未保存改动、每次保存都写盘**，而用户什么
/// 都没动。⇒ 基线走这里，与 [`collect_punct_map`] 同源。
fn normalize_schema_punct_map(
    raw: &serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    let mut out = punct_extra_rows(raw);
    for (_, tok, defs) in crate::manifest::PUNCT_DEFAULTS {
        let ov = raw.get(*tok).and_then(|v| v.as_array());
        let vals: [String; 4] = std::array::from_fn(|i| crate::manifest::seed_cell(ov, i, defs[i]));
        if let Some(row) = crate::manifest::punct_row_override(&vals, defs) {
            out.insert((*tok).to_string(), row);
        }
    }
    out
}

/// 候选注释子对话框：横竖两行 + 恢复默认。
///
/// ★ **「恢复默认」＝回到「跟随全局」**，也就是把两个勾选都取消。不是"清空输入框"——
/// 勾着而内容为空是三态里的第三态（本方案不显示注释），那恰恰不是默认值。
/// 输入框里的文字一并清掉，免得下次勾上时冒出一段用户以为已经不在了的旧模板。
fn build_comment_tpl_dialog(state: &SchemaManagerState) -> Element {
    let visible = state.settings_bh_comment_visible;
    let (v_on, v_txt) = (state.settings_bh_comment_v_on, state.settings_bh_comment_v);
    let (h_on, h_txt) = (state.settings_bh_comment_h_on, state.settings_bh_comment_h);

    let frame = Element::col()
        .width(560)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("候选注释")
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(concat!(
                "候选词右侧的小字。不勾 = 跟随全局；勾上填模板 = 本方案改用它；",
                "勾上留空 = 本方案不显示注释。
",
                "模板语法与可用变量见「外观 → 候选标注 → 候选注释」，两处写法完全一样。",
            ))
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(4)
            .padding_xy(18, 10),
        )
        .child(
            Element::col()
                .width_match()
                .padding_xy(16, 2)
                .child(crate::widgets::card(vec![
                    comment_tpl_row("竖排注释", "竖排每行独占，放得下注音或字根", v_on, v_txt),
                    comment_tpl_row(
                        "横排注释",
                        "横排候选共享一行宽度，注释长了会把窗口撑宽",
                        h_on,
                        h_txt,
                    ),
                ])),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    // 柔和描边而不是实心：它是兜底动作，与「完成」并排时实心色块会比
                    // 主操作还抢眼。同「全部恢复跟随」那个按钮的取舍。
                    Element::button("恢复默认")
                        .small()
                        .outline_soft()
                        .on_click(move |_| {
                            v_on.set(false);
                            h_on.set(false);
                            v_txt.set(String::new());
                            h_txt.set(String::new());
                        }),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("完成")
                        .small()
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// 注释模板三态 → 回传给 core 的 JSON 值（纯函数，供测试）。
///
/// `None`（未勾选）→ `null` = **取消覆盖**，回落下一层；`Some(s)` → 字符串，
/// 其中空串是一个正常取值（本方案不显示注释），**不得**被顺手折成 null。
fn comment_tpl_value(tpl: Option<&str>) -> Value {
    match tpl {
        Some(s) => json!(s),
        None => Value::Null,
    }
}

/// 方案级自定义标点表三态 → 回传 JSON（与 [`comment_tpl_value`] 同构）。
///
/// ★ **未勾选写 `null` 而不是省略这个键**：core 的 `json_to_toml` 跳过 null ⇒ override 里
/// 不含该键 ⇒ 回落方案文件（作者写了就用作者的、没写就跟随全局）。省略则 `json_diff`
/// 压根看不到这一项，方案作者写的表想取消也取消不掉。
///
/// ⚠️ `Some({})` 要如实写成空表——那是「本方案一条自定义标点都不要」，不是「跟随」。
fn punct_map_value(map: Option<&serde_json::Map<String, Value>>) -> Value {
    match map {
        Some(m) => Value::Object(m.clone()),
        None => Value::Null,
    }
}

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
    pub settings_sections: [Signal<Vec<SettingsSection>>; SETTINGS_TABS.len()],
    /// 当前选中的 TAB 下标。
    pub settings_tab: Signal<usize>,
    /// TAB 标签（外层 `Vec` 恒长度 1，理由见 `tab_bar` 的构建处）。「方案自定义」那一项
    /// 会带上已改项数，由码表配置的 effect 推。
    pub settings_tab_labels: Signal<Vec<Vec<String>>>,
    /// 扩展词库行（每行含独立受控开关信号）；ExtensionDicts 分节据此渲染。
    pub settings_dicts: Rc<RefCell<Vec<DictRow>>>,
    /// 方案声明的扩展词库发现目录（`[[dictionary_dirs]]` 的 path，相对 `schemas/`）。
    /// 有它时即使一个扩展库都还没有，也要显示「扩展词库」一节——用户正是从这里找到该把
    /// 词库文件放哪的（「打开扩展词库目录」按钮）。
    pub settings_dict_dirs: Rc<RefCell<Vec<String>>>,

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
    /// [`Self::key_actions`] 是否**确实从 core 读到过**（而非兜底的空表）。
    ///
    /// ★ 这张表的 writer 是**整表覆盖**语义，所以「空」有两种含义必须分开：
    /// 「RPC 没读到」与「用户真的删光了」。混同的代价是前者被当成后者写回去，
    /// 用户全部按键绑定被清空。旁边 `schema.active` 的 writer 早就防了同一件事
    /// （注释原文：「离线/未连接时不该把配置清空」），这张表当时漏了。
    ///
    /// 置位点有二：构造期同步 RPC 成功，或 `apply_loaded` → [`Self::refresh_key_actions`]。
    pub key_actions_loaded: Rc<std::cell::Cell<bool>>,
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
    // ── 特殊模式（方案文件 `[overlay]` 段）──
    //
    // 本节**只在方案已声明 `[overlay]` 时出现**，故没有「是不是特殊方案」的总开关：
    // 那是方案的固有属性，由方案文件决定。这里只调段内的行为参数。
    /// 进入模式即展示候选（`[overlay] show_all_on_enter`）。
    /// 「本方案启用辅助码」是否覆盖全局（勾选框）。
    pub settings_aux_on_over: Signal<bool>,
    /// 勾选时的取值。未勾选时仍保留一份初值（生效值，见 `seed_sections` 里的说明），
    /// 供用户勾上时接着编辑，而不是从零值开始。
    pub settings_aux_on: Signal<bool>,
    /// 「辅助码最长词长」是否覆盖全局。
    pub settings_aux_len_over: Signal<bool>,
    /// 勾选时的取值。stepper 只收 `Signal<f64>`（windui 数字控件的通用形态，见
    /// `manifest::build_number` / `schema_codetable::build_editor`），落盘前按
    /// `aux_write` 的 `i32` 形参转一次，两处口径不冲突。
    pub settings_aux_len: Signal<f64>,
    /// 辅助码卡片标题右侧的摘要（「全部跟随」/「已自定义 N 项」）。
    pub settings_aux_disp: Signal<String>,
    /// 打开时四项的快照 `(on_over, on, len_over, len)`，供脏判定：把它与当前值各自
    /// 代入 `aux_write`，比较两次的落盘结果（而不是逐项比对信号）是否变化。
    settings_aux_at_open: Rc<std::cell::Cell<(bool, bool, bool, i32)>>,
    /// 「本方案直接辅助码」是否覆盖全局（勾选框）。只在双拼方案显示，见 [`seed_aux_direct`]。
    pub settings_aux_direct_over: Signal<bool>,
    /// 勾选时的取值；未勾时保留生效值，同 `settings_aux_on`。
    pub settings_aux_direct: Signal<bool>,
    /// 打开时 `(direct_over, direct)` 的快照，脏判定同 `settings_aux_at_open`。
    settings_aux_direct_at_open: Rc<std::cell::Cell<(bool, bool)>>,
    /// 「辅助码来源」是否覆盖方案文件（勾选框）。判据见 [`seed_aux_src`]：合并值等于
    /// 方案文件自己的值即为跟随（不勾）。
    pub settings_aux_src_over: Signal<bool>,
    /// 勾选时选中的下拉项下标（指向 `settings_aux_src_opts.items`）。
    pub settings_aux_src_idx: Signal<usize>,
    /// 只读态：打开时 `files` 是多条或下拉里没有的条目，下拉表达不了。此时保存原样不动，
    /// 直到被「全部恢复跟随」清掉（那时落 null）。只读态同时 `over` 为真——它确实是覆盖。
    pub settings_aux_src_manual: Signal<bool>,
    /// 下拉可选项（`schema.auxCodeSources`），每次打开重取；RPC 失败为空表。
    settings_aux_src_opts: Rc<RefCell<AuxSourceOpts>>,
    /// 跟随档括号里的文字（[`aux_src_follow_text`]），不含「跟随方案」前缀——前缀由行给。
    pub settings_aux_src_follow: Signal<String>,
    /// 打开时 `(over, idx, manual)` 的快照，供保存时的脏判定（同 `settings_aux_at_open`）。
    settings_aux_src_at_open: Rc<std::cell::Cell<(bool, usize, bool)>>,
    /// 只读态控件位的文字（[`aux_src_manual_text`]）；非只读时为空。
    settings_aux_src_manual_text: Rc<RefCell<String>>,
    /// 跟随态该停的下拉下标（方案声明的首个来源在可选项里的位置）。打开时 seed、
    /// 「全部恢复跟随」拨回——与另两行「控件值拨回跟随值」同理。
    settings_aux_src_follow_idx: Rc<std::cell::Cell<usize>>,
    // ── 本方案行为（`[punct]` / `[candidate]` / `[phrases]` 三段）──
    //
    // 所有方案都有，无总开关：三项的「跟随全局」档本身就是关闭状态，再套一个开关
    // 等于两处表达同一件事。
    /// 标点态：0=跟随全局 / 1=中文标点 / 2=英文标点（`[punct] mode`）。
    pub settings_bh_punct: Signal<usize>,
    /// 候选排列：0=跟随全局 / 1=竖排 / 2=横排（`[candidate] layout`）。
    pub settings_bh_layout: Signal<usize>,
    /// 候选排列下拉的标签表：首项按 `followedBehavior.layout` 现算（R8.2），每次
    /// `open_settings_at_id` 都要重设，不能只在构造时算一次。与「本方案行为」节和
    /// overlay 节的候选排列下拉共用同一份（同一个全局跟随值）。
    pub settings_layout_follow_labels: Signal<Vec<String>>,
    /// 候选字体是否由本方案覆盖（R8.1 改勾选行：`[candidate] font_family` 这个键存不存在）。
    pub settings_bh_font_over: Signal<bool>,
    /// 勾选时的取值；未勾选时文本框仍保留上次编辑内容，供用户勾上时接着编辑
    /// （同 `settings_aux_on`/`settings_aux_len` 的做法）。落盘走 [`font_write`]。
    pub settings_bh_font: Signal<String>,
    /// 横排时文字排列：0=默认 / 1=旋转 90° / 2=文字竖排（`[candidate] text_orientation`）。
    pub settings_bh_text_orient: Signal<usize>,
    /// 是否加载短语（`[phrases] enabled`）。
    pub settings_bh_phrases: Signal<bool>,
    /// 竖排候选注释模板是否由本方案覆盖（`[candidate] comment_template_vertical` 存不存在）。
    ///
    /// 与旁边三项**形态不同**：那三项各有一个显式的「跟随全局」档位（`punct.mode = "follow"`
    /// 是个真值），注释模板的「跟随」是**键缺失**，只能用勾选框表达。勾上才写这一项。
    pub settings_bh_comment_v_on: Signal<bool>,
    /// 竖排注释模板内容。**留空即空串 = 本方案不显示注释**（三态的第三态），
    /// 它与「未勾选 = 跟随全局」是两回事。
    pub settings_bh_comment_v: Signal<String>,
    /// 横排候选注释模板是否由本方案覆盖。横竖各自独立三态。
    pub settings_bh_comment_h_on: Signal<bool>,
    /// 横排注释模板内容。
    pub settings_bh_comment_h: Signal<String>,
    /// 候选注释子对话框的可见性。四个控件（横竖各「勾选 + 输入框」）不内联在节里：
    /// 与旁边三行「一行一个控件」的节奏差太多，铺开就乱。
    pub settings_bh_comment_visible: Signal<bool>,
    /// 节内那一行的摘要（「跟随全局」/「竖排已自定义」…），随勾选态刷新。
    pub settings_bh_comment_disp: Signal<String>,
    /// 本方案是否使用自己的自定义标点表（`[punct] custom_mappings` 这个键存不存在）。
    ///
    /// 形态与注释模板那两个勾选同类：「跟随全局」是**键缺失**，没有显式档位可选。
    /// ⚠️ 勾上而表格全空 = `Some({})` = **本方案一条自定义标点都不要**，不是「跟随全局」。
    pub settings_bh_punct_map_on: Signal<bool>,
    /// 自定义标点子对话框的可见性（表格铺不进节里，同注释模板那处的理由）。
    pub settings_bh_punct_map_visible: Signal<bool>,
    /// 节内那一行的摘要（「跟随全局」/「本方案 3 项」…）。
    pub settings_bh_punct_map_disp: Signal<String>,
    /// 表格单元格工作态：每行 4 列，行序与列序都跟 `manifest::PUNCT_DEFAULTS` 走
    /// （列序是 **core 存储序** [中半, 英全, 中全, 英半]，展示换序只在 `field_dialogs`
    /// 的 `PUNCT_DISPLAY_COLS` 一处发生——★ 禁止在别处再写死下标）。
    settings_bh_punct_cells: Rc<Vec<[Signal<String>; 4]>>,
    /// 方案表里 `PUNCT_DEFAULTS` 管不到的源字符，播种时拆出、回写时并回。
    /// 表格没有对应行 ⇒ 不透传就等于每次保存都把方案作者写的这些行删掉。
    settings_bh_punct_extra: Rc<RefCell<serde_json::Map<String, Value>>>,
    /// 打开时的各项初值，供脏判定——没有它，打开看一眼再关掉也会入队写盘。
    ///
    /// `RefCell` 而非 `Cell`：注释模板带 `String`，`BehaviorSeed` 不再是 `Copy`。
    settings_bh_at_open: Rc<RefCell<BehaviorSeed>>,
    pub settings_ov_show_all: Signal<bool>,
    /// 候选排列（0=跟随全局 / 1=竖排 / 2=横排），对应 `[overlay] candidate_layout`。
    pub settings_ov_layout: Signal<usize>,
    /// 打开对话框时的两项初值，用于脏判定——没有它，打开看一眼再关掉也会入队写盘。
    settings_ov_at_open: Rc<std::cell::Cell<(bool, usize)>>,
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
    pub settings_ct_plan: Signal<Vec<crate::dialogs::schema_codetable::CtGroup>>,
    /// 用户按过「全部恢复跟随」。落盘时据此额外调 [`CtFields::reset`]——逐项 `apply`
    /// 够不着**隐藏项**（没控件就没信号），而 0.118 的「开总开关整段写实」会把
    /// `auto_commit_min_len` 写进 override。不带这个标志的话，那个键清不掉，用户按了
    /// 「全部恢复跟随」仍有一项在偷偷生效。`seed` 时清零。
    settings_ct_reset_all: Rc<std::cell::Cell<bool>>,

    /// 打开对话框那一刻的双拼布局索引。脏判定用——没有它就分不清「用户拨过」和
    /// 「本来就是这个值」，于是打开看一眼再关掉也会写一次盘。
    settings_layout_at_open: Rc<std::cell::Cell<usize>>,
    /// 待提交的方案 override 改动（方案 id → 变更）。关窗时暂存，随主保存提交。
    ///
    /// **按方案 id 存**而不是只留最后一次的工作态：用户可以连着配好几个方案再点
    /// 「应用设置」，只留一份的话先配的那几个会静默丢。
    pending_edits: Rc<RefCell<std::collections::HashMap<String, PendingSchemaEdit>>>,
}

impl SchemaManagerState {
    pub fn new() -> Self {
        let available = Self::read_available();
        // 读不到与「读到空表」必须分开，见 `key_actions_loaded`。
        let key_actions = Self::read_key_actions();
        let key_actions_loaded = Rc::new(std::cell::Cell::new(key_actions.is_some()));
        let state = Self {
            schemas: Rc::new(RefCell::new(Vec::new())),
            enabled_order: Rc::new(RefCell::new(available)),
            key_actions: Rc::new(RefCell::new(key_actions.unwrap_or_default())),
            key_actions_loaded,
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
            settings_sections: [signal(vec![]), signal(vec![]), signal(vec![])],
            settings_tab: signal(0),
            settings_tab_labels: signal(vec![
                SETTINGS_TABS.iter().map(|s| s.to_string()).collect(),
            ]),
            settings_dicts: Rc::new(RefCell::new(Vec::new())),
            settings_dict_dirs: Rc::new(RefCell::new(Vec::new())),
            manifest: Rc::new(crate::manifest::Manifest::from_embedded()),
            settings_aux_on_over: signal(false),
            settings_aux_on: signal(false),
            settings_aux_len_over: signal(false),
            settings_aux_len: signal(0.0),
            settings_aux_disp: signal(ct_summary(0)),
            settings_aux_at_open: Rc::new(std::cell::Cell::new((false, false, false, 0))),
            settings_aux_direct_over: signal(false),
            settings_aux_direct: signal(false),
            settings_aux_direct_at_open: Rc::new(std::cell::Cell::new((false, false))),
            settings_aux_src_over: signal(false),
            settings_aux_src_idx: signal(0),
            settings_aux_src_manual: signal(false),
            settings_aux_src_opts: Rc::new(RefCell::new(AuxSourceOpts::default())),
            settings_aux_src_follow: signal(String::new()),
            settings_aux_src_at_open: Rc::new(std::cell::Cell::new((false, 0, false))),
            settings_aux_src_manual_text: Rc::new(RefCell::new(String::new())),
            settings_aux_src_follow_idx: Rc::new(std::cell::Cell::new(0)),
            settings_ov_show_all: signal(false),
            settings_ov_layout: signal(0),
            settings_bh_punct: signal(0),
            settings_bh_layout: signal(0),
            settings_layout_follow_labels: signal(layout_dropdown_labels("horizontal")),
            settings_bh_text_orient: signal(0),
            settings_bh_font_over: signal(false),
            settings_bh_font: signal(String::new()),
            settings_bh_phrases: signal(true),
            settings_bh_comment_v_on: signal(false),
            settings_bh_comment_v: signal(String::new()),
            settings_bh_comment_h_on: signal(false),
            settings_bh_comment_h: signal(String::new()),
            settings_bh_comment_visible: signal(false),
            settings_bh_comment_disp: signal(comment_summary(false, false)),
            settings_bh_punct_map_on: signal(false),
            settings_bh_punct_map_visible: signal(false),
            settings_bh_punct_map_disp: signal(punct_map_summary(false)),
            settings_bh_punct_cells: Rc::new(
                crate::manifest::PUNCT_DEFAULTS
                    .iter()
                    .map(|(_, _, defs)| std::array::from_fn(|i| signal(defs[i].to_string())))
                    .collect(),
            ),
            settings_bh_punct_extra: Rc::new(RefCell::new(serde_json::Map::new())),
            settings_bh_at_open: Rc::new(RefCell::new(BehaviorSeed::default())),
            settings_ov_at_open: Rc::new(std::cell::Cell::new((false, 0))),
            settings_ct_disp: signal(String::new()),
            settings_ct: Default::default(),
            settings_ka: crate::dialogs::schema_key_actions::KeyActionFields::schema_scoped(),
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
            settings_ct_reset_all: Rc::new(std::cell::Cell::new(false)),
            settings_layout_at_open: Rc::new(std::cell::Cell::new(0)),
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
    /// ⚠️ **返回 `None` = 没读到**，与「读到一张空表」必须分开——本函数在
    /// `AppState::empty()` 里执行，那时 RPC 未必可用（该处注释原文是「不阻塞任何 RPC」，
    /// 本调用其实违反了它）。两者混同的后果见 [`Self::key_actions_loaded`]：writer 会拿
    /// 空表整表覆盖，把用户**全部**按键绑定清空。
    fn read_key_actions() -> Option<std::collections::BTreeMap<String, String>> {
        let v = match rpc::call_core("config.get", &json!({})) {
            Ok(v) => v,
            Err(e) => {
                // 静默失败是本 bug 的一半——没有这条日志，事后无从判断用户当时是
                // 「没读到」还是「真的没配过」。
                tracing::warn!("读取 keys.key_actions 失败（将等待 apply_loaded 补齐）: {e}");
                return None;
            }
        };
        let map = v
            .get("keys")
            .and_then(|k| k.get("key_actions"))
            .and_then(|m| m.as_object())?;
        Some(
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect(),
        )
    }

    /// 用完整配置刷新 `key_actions` 内存表（由 [`Self::apply_config`] 在 `apply_loaded`
    /// 时调用）。
    ///
    /// # 为什么必须有这一步
    ///
    /// 构造期的 [`Self::read_key_actions`] 是**同步 RPC**，离线/服务未就绪时拿不到值。
    /// 而这张表此前**没有任何后续刷新**——对比之下 `enabled_order` 走
    /// [`Self::apply_config`] 能自愈，`key_actions` 漏了，于是一次失败就永久是空表。
    /// 叠加 writer 的整表覆盖语义，用户的全部按键绑定会在下一次保存时被清空
    /// （`keys.key_actions` 出厂默认是 `{}`，core 的 `set_user_value` 还会判「等于默认」
    /// 直接删掉整个键，连痕迹都不留）。
    fn refresh_key_actions(&self, cfg: &Config) {
        let Some(map) = cfg.get_object("keys.key_actions") else {
            return;
        };
        *self.key_actions.borrow_mut() = map
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();
        self.key_actions_loaded.set(true);
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

    /// 全局 `keys.key_actions` 的动词表。
    ///
    /// ★ 2026-08-30 起两层的**切方案动词**完全一致；全局仍多一项 `softkeyboard`（见函数体）。
    /// 此前全局多一段单向
    /// `switch_schema:<id>`，因为 core 禁止方案级单向（`bound_key_decision` 让位并 warn）；
    /// 那条禁令已撤销——它的理由「单向切走就回不来了」说的是**这把键**按不动，而回程
    /// 完全可以由别的键负责，禁令挡掉了合法配法。core 侧改为「送达键在目标方案里吞掉」，
    /// 见 core 仓 `BoundAction::SwitchSchema` 的文档。
    ///
    /// 保留本函数而不让调用方直接用方案级那个：两层的动词表**在概念上仍是两件事**
    /// （将来若再出现只在某一层合法的动词，改这里即可，不必回头去找调用方）。
    pub(crate) fn build_global_key_action_verbs(
        &self,
    ) -> Vec<crate::dialogs::schema_key_actions::Verb> {
        let mut out = self.build_key_action_verbs();
        // 软键盘：**只加在全局这一份**。core 出厂就绑着 `ctrl+shift+k`
        // （`data/config.toml` 的 `key_actions`），不列出来的话那条出厂绑定在界面上显示成
        // 「softkeyboard（当前值，目标可能已不存在）」——功能好好的，措辞却在说它坏了。
        //
        // ⚠️ 不下放到方案级：`hotkey_action_entry`（组合键白名单）是编译**全局**
        // `keys.key_actions` 用的，方案级那条通路认不认这个动词没有验证过，列出来就是
        // 「配得上、按下去没反应」。
        // ⚠️ 面 id 直通车（`softkeyboard:<面id>`）也不列：面的枚举在
        // `system.softkeyboard.toml`，是另一条数据通路，手写配置仍然有效。
        out.push(crate::dialogs::schema_key_actions::Verb {
            value: "softkeyboard".into(),
            label: "软键盘".into(),
        });
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
    /// ⚠️ 这里曾写着「`toggle_schema:<id>` 刻意不列」——**与代码早已不符**（下方就在列），
    /// 且方案级单向也已于 2026-08-30 放开。两族切方案动词现在方案级/全局都列。
    ///
    /// ⚠️ 本函数是**两层共用**的那份，只放两层都合法的动词。只在全局合法的（当前是
    /// `softkeyboard`）加在 [`Self::build_global_key_action_verbs`] 里。
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
            // 生僻字：用当前方案的编码输入，候选只留生僻字（core 的 `BoundAction::RareChar`）。
            // 与临拼/临英同属「两层都合法」——方案级 `[key_actions]` 与全局共用
            // `BoundAction::parse`，故放在本函数而非 `build_global_key_action_verbs`。
            Verb {
                value: "rare_char".into(),
                label: "生僻字".into(),
            },
            // 反查模式：core 的 `BoundAction::Reverse`，全局与方案级共用 parse。
            Verb {
                value: "reverse".into(),
                label: "反查模式".into(),
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
        // 切方案两族：往返（C 类）与单向。**都只在修饰键上可用**——它们必须在英文模式下
        // 也按得动（否则切到英文方案就回不来），而有字符的键走的 keydown 链在英文态根本
        // 到不了。这里照常列出，由 `KeyActionFields` 在行内按所选键给提示（core 侧同样有裁决）。
        //
        // ★ 单向此前**只列在全局表**，因为 core 禁止方案级单向。2026-08-30 那条禁令已撤销：
        // 它的理由「单向切走后目标方案没这条绑定、键就按不动了」说的是**这把键**按不动，
        // 而回程完全可以由别的键负责（真实配法：右 Shift 单向去英文方案、左 Shift 管中英文
        // 态）。core 改为「送达键在目标方案里被吞掉」，不再落回全局链去误切中英文。
        //
        // ⚠️ 两族的区别要让用户看得见，故标签分别是「切换方案（可返回）」与「切到方案
        // （单向）」——只写方案名的话，下拉里会出现两条同名项，选错了行为却完全不同。
        for e in self.schemas.borrow().iter() {
            if e.id.is_empty() || e.hidden {
                continue;
            }
            let name = if e.name.is_empty() { &e.id } else { &e.name };
            out.push(Verb {
                value: format!("toggle_schema:{}", e.id),
                label: format!("切换方案（可返回） - {name}"),
            });
        }
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
            ("toggle_t2s", "繁体/简体切换"),
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
        let enabled = self.settings_hotkey_enabled.get();
        // 诊断：本函数只由 `hotkey_effect` 驱动（对话框有多条关闭路径，故用 effect 而非
        // 挂按钮）。「清除后保存说没有变更」时据此分辨：没有这条日志 = effect 没触发；
        // 有但 before==after = 内存表里本就没有这条（表的来源出了问题）。
        let before = self.key_actions.borrow().len();
        apply_hotkey_edit(&mut self.key_actions.borrow_mut(), &id, enabled, &raw);
        let after = self.key_actions.borrow().len();
        tracing::debug!(
            "commit_hotkey_edit: schema={id} raw={raw:?} enabled={enabled} \
             entries {before}->{after}"
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
        // 与 enabled_order 同批刷新：构造期那次同步 RPC 可能失败，而这张表此前没有任何
        // 后续刷新点 —— 一次失败就永久是空表，再被 writer 整表覆盖回 core。
        self.refresh_key_actions(cfg);
        self.rebuild_list();
    }

    /// 刷新方案列表（调用 schema.list RPC）。
    pub fn refresh(&self) {
        // 恒取全集（含特殊方案），显示与否由 `show_hidden` 在渲染层过滤——
        // 开关切换无需重新走 RPC，与 `hide_disabled` 同一套做法。
        let list = rpc::call_core("schema.list", &json!({ "includeHidden": true }))
            .unwrap_or(Value::Array(vec![]));

        // 双拼布局清单（动态，取代硬编码）：供下拉选项/索引映射/行名后缀。
        *self.layouts.borrow_mut() = fetch_shuangpin_layouts();

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
        // 本方案行为：所有方案都有。三项默认「跟随全局 / 加载」，方案文件不写这三段是常态，
        // 故与 overlay 那节不同——不以「段存在」为建节条件。
        {
            let seed = seed_behavior(&self.settings_cfg.borrow());
            self.settings_bh_punct.set(seed.punct);
            self.settings_bh_layout.set(seed.layout);
            // 下拉首项文案按**本次打开**的跟随值现算（Review Focus 3）：跟随值是
            // core 只读旁路字段，随全局页配置变化，不能用构造时缓存的默认值——
            // 否则用户在全局页改了候选排列，方案设置页打开两次都还显示旧值。
            let followed_layout = self
                .settings_cfg
                .borrow()
                .pointer("/followedBehavior/layout")
                .and_then(Value::as_str)
                .unwrap_or("horizontal")
                .to_string();
            self.settings_layout_follow_labels
                .set(layout_dropdown_labels(&followed_layout));
            self.settings_bh_font_over.set(seed.font_family.is_some());
            self.settings_bh_font
                .set(seed.font_family.clone().unwrap_or_default());
            self.settings_bh_text_orient.set(seed.text_orient);
            self.settings_bh_phrases.set(seed.phrases);
            self.seed_comment_tpl(&seed);
            self.settings_bh_comment_disp.set(comment_summary(
                seed.comment_v.is_some(),
                seed.comment_h.is_some(),
            ));
            // 自定义标点表：勾选态问「键在不在」，格子按「方案表覆盖值 ⊕ 内置默认」播种。
            // ⚠️ 换方案必须逐格重播——信号是整个对话框复用的一组，不重播就会把上一个方案
            // 的表显示成这个方案的（同本函数开头那句「对话框复用同一组信号」）。
            self.settings_bh_punct_map_on.set(seed.punct_map.is_some());
            let pm = seed.punct_map.clone().unwrap_or_default();
            *self.settings_bh_punct_extra.borrow_mut() = punct_extra_rows(&pm);
            for (row, (_, tok, defs)) in self
                .settings_bh_punct_cells
                .iter()
                .zip(crate::manifest::PUNCT_DEFAULTS.iter())
            {
                let ov = pm.get(*tok).and_then(|v| v.as_array());
                for i in 0..4 {
                    row[i].set(crate::manifest::seed_cell(ov, i, defs[i]));
                }
            }
            self.settings_bh_punct_map_disp
                .set(punct_map_summary(seed.punct_map.is_some()));
            // ★ 脏判定基线要用**归一后**的表，不能用配置原文——理由见
            // `normalize_schema_punct_map`（否则打开看一眼关掉就恒判为脏）。
            let seed = BehaviorSeed {
                punct_map: seed.punct_map.as_ref().map(normalize_schema_punct_map),
                ..seed
            };
            *self.settings_bh_at_open.borrow_mut() = seed;
            sections.push(SettingsSection::Divider);
            sections.push(SettingsSection::Behavior);
        }
        if is_sp {
            sections.push(SettingsSection::Divider);
            sections.push(SettingsSection::ShuangpinLayout);
        }
        // 辅助码：**只在方案声明了辅助码表时**建节。判据见 `has_aux_code_table`——
        // 落盘那侧用的是同一个函数，两处必须同判据。
        {
            let cfg = self.settings_cfg.borrow();
            if has_aux_code_table(&cfg) {
                // 控件初值取**生效值**（override 有则用它，否则用跟随值）——理由同
                // `schema_codetable::CtFields::seed` 里 `val` 取 `effectiveCodetable`
                // 那段注释：不然用户第一次勾选时控件会显示一个陌生的零值，而不是
                // 「这个方案现在实际生效的那个值」。
                let followed_on = cfg
                    .pointer("/followedBehavior/auxEnabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let followed_len = cfg
                    .pointer("/followedBehavior/auxMaxPhraseLen")
                    .and_then(Value::as_i64)
                    .map(clamp_aux_len)
                    .unwrap_or(0);
                let raw_on = cfg
                    .pointer("/engine/aux_code/enabled")
                    .and_then(Value::as_bool);
                let raw_len = cfg
                    .pointer("/engine/aux_code/max_phrase_len")
                    .and_then(Value::as_i64)
                    .map(clamp_aux_len);
                let on_over = raw_on.is_some();
                let on = raw_on.unwrap_or(followed_on);
                let len_over = raw_len.is_some();
                let len = raw_len.unwrap_or(followed_len);
                // 辅助码来源：合并值 vs 方案文件自己的值（core 旁路字段 `auxCodeBaseFiles`，
                // 不含 override）。老核心没有这个字段 ⇒ 按合并值处理 ⇒ 呈现为跟随，
                // 保存时也就不会动 `files`——宁可少显示一个覆盖，也不替用户改写。
                let str_list = |v: Option<&Value>| -> Vec<String> {
                    v.and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let merged = str_list(cfg.pointer("/engine/aux_code/files"));
                let base = match cfg.get("auxCodeBaseFiles") {
                    Some(v) => str_list(Some(v)),
                    None => merged.clone(),
                };
                let (direct_over, direct) = seed_aux_direct(&cfg, is_sp);
                drop(cfg);
                self.settings_aux_on_over.set(on_over);
                self.settings_aux_on.set(on);
                self.settings_aux_len_over.set(len_over);
                self.settings_aux_len.set(len as f64);
                self.settings_aux_at_open.set((on_over, on, len_over, len));
                self.settings_aux_direct_over.set(direct_over);
                self.settings_aux_direct.set(direct);
                self.settings_aux_direct_at_open.set((direct_over, direct));

                // 可选项每次打开重取：用户可能在两次打开之间往 aux_code/ 里放了新表。
                // 失败给空表——跟随文字退回原文、已有覆盖进只读态，都不会误写。
                let opts = rpc::call_core("schema.auxCodeSources", &json!({ "id": schema_id }))
                    .map(|v| parse_aux_source_opts(&v))
                    .unwrap_or_default();
                // 跟随态的下标也要 seed 成当前生效的那一项：勾上时下拉停在它上面，
                // 否则一勾就悄悄换成了第一项。方案声明了多条时取第一条。
                //
                // ⚠️ 两个在 diff 模型下**预期**的后果（不是 bug）：
                // - 勾上不改直接保存：写下的 `[该项]` 等于方案文件的值，core `json_diff`
                //   当无差异删掉，重开回到跟随——效果与跟随完全相同，只是勾选没留下。
                // - 方案声明了多条时勾上即**收窄成首条**：下拉只能表达单条，保存落的就是
                //   `[首条]`，与方案文件不等，会作为覆盖留下。
                let follow_idx = base.first().and_then(|e| opts.index_of(e)).unwrap_or(0);
                let mut manual_text = String::new();
                let (src_over, src_idx, src_manual) = match seed_aux_src(&merged, &base, &opts) {
                    AuxSrcSeed::Follow => (false, follow_idx, false),
                    AuxSrcSeed::Pick(i) => (true, i, false),
                    AuxSrcSeed::Manual(v) => {
                        manual_text = aux_src_manual_text(&v, &opts);
                        (true, follow_idx, true)
                    }
                };
                *self.settings_aux_src_manual_text.borrow_mut() = manual_text;
                self.settings_aux_src_follow_idx.set(follow_idx);
                self.settings_aux_src_follow
                    .set(aux_src_follow_text(&base, &opts));
                *self.settings_aux_src_opts.borrow_mut() = opts;
                self.settings_aux_src_over.set(src_over);
                self.settings_aux_src_idx.set(src_idx);
                self.settings_aux_src_manual.set(src_manual);
                self.settings_aux_src_at_open
                    .set((src_over, src_idx, src_manual));
                self.settings_aux_disp.set(ct_summary(
                    on_over as usize + len_over as usize + src_over as usize + direct_over as usize,
                ));
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
                // 四份数据各答一个问题，缺一项就有一类状态显示不出来：
                // 生效值＝勾选时控件的初值；跟随值＝取消覆盖后变成什么（未勾选时显示它）；
                // override 层＝这一项是不是用户改的；合并值＝配合 override 层判「方案自带」。
                let eff = cfg.get("effectiveCodetable").cloned().unwrap_or(json!({}));
                let followed = cfg.get("followedCodetable").cloned().unwrap_or(json!({}));
                let ov = cfg.get("codetableOverride").cloned().unwrap_or(json!({}));
                let merged = cfg
                    .pointer("/engine/codetable")
                    .cloned()
                    .unwrap_or(json!({}));
                // 特殊方案不继承全局（core `codetable_baseline`），文案跟着换，否则会写着
                // 「跟随全局」而实际跟的是内置基线。
                //
                // ★ 判据是 **`[overlay]` 段存在**，不是 `[schema].hidden` ——两者正交：
                // hidden 管「列不列进方案切换列表」，与折叠哪份基线无关。core 侧
                // `codetable_baseline` 早已换成 overlay 判据，这里跟上（本行内摘要此前
                // 一直用 hidden，对「hidden 但非 overlay 的小码表」会说反）。
                let follow_label = if cfg.get("overlay").is_some_and(|v| v.is_object()) {
                    "跟随基线"
                } else {
                    "跟随全局"
                };
                self.settings_ct.seed(
                    &self.manifest,
                    &crate::dialogs::schema_codetable::CtSeed {
                        effective: &eff,
                        followed: &followed,
                        overridden: &ov,
                        merged: &merged,
                        follow_label,
                    },
                );
                self.settings_ct_plan.set(self.settings_ct.plan());
                // 换方案要清：上一个方案按过「全部恢复跟随」，标志留着会把这个方案的
                // 隐藏项一起清掉，而用户在这个方案上什么都没按。
                self.settings_ct_reset_all.set(false);
                drop(cfg);
                self.settings_ct_disp
                    .set(ct_summary(self.settings_ct.custom_count()));
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
        let dirs = parse_dict_dirs(&self.settings_cfg.borrow());
        if !rows.is_empty() || !dirs.is_empty() {
            sections.push(SettingsSection::Divider);
            sections.push(SettingsSection::ExtensionDicts);
        }
        *self.settings_dicts.borrow_mut() = rows;
        *self.settings_dict_dirs.borrow_mut() = dirs;
        // 注：曾有 `has_config` 与 `SettingsSection::Empty`——「没有任何可配置项」的空状态。
        // 「按键功能」对所有方案都有之后，那个分支恒不可达，故一并退役（枚举项保留，
        // 将来若又出现真的空方案可直接复用）。
        for (slot, list) in self
            .settings_sections
            .iter()
            .zip(split_sections_by_tab(&sections))
        {
            slot.set(list);
        }
        // 每次打开都回到首个 TAB：对话框服务于任意方案，停在上一个方案看过的 TAB 上
        // 会让人以为「这个方案也有那些项」，而它可能整个 TAB 都是空的。
        self.settings_tab.set(0);

        self.settings_visible.set(true);
    }

    /// 注释模板两行按 `seed` 播种。
    ///
    /// 勾选态与文本分开落：勾选问「有没有这个键」，文本框只装内容。
    /// 未设置时文本框留空——用户一勾上就是「本方案不显示注释」，这正是三态的第三态，
    /// 而不是一个需要先填点什么才生效的空状态。
    ///
    /// ★ 文本框装的是**转义后**的显示形态（真换行显示成 `\n`，同全局注释模板与悬停提示的段
    /// 内容），[`Self::behavior_now`] 还原。`seed` 本身保持真值，脏判定比的是真值。
    fn seed_comment_tpl(&self, seed: &BehaviorSeed) {
        use crate::dialogs::tooltip_sections_dialog::escape_template;
        let shown = |t: &Option<String>| t.as_deref().map(escape_template).unwrap_or_default();
        self.settings_bh_comment_v_on.set(seed.comment_v.is_some());
        self.settings_bh_comment_v.set(shown(&seed.comment_v));
        self.settings_bh_comment_h_on.set(seed.comment_h.is_some());
        self.settings_bh_comment_h.set(shown(&seed.comment_h));
    }

    /// 「本方案行为」节的当前工作态。与 [`seed_behavior`] 的产物同型，故可直接与
    /// `settings_bh_at_open` 比较判脏。
    ///
    /// ★ 勾选态与文本合成三态**只在这一处**发生：勾上才 `Some`，勾上留空即 `Some("")`。
    /// 若在渲染处也各写一遍，两处口径迟早漂移，而漂移的表现是「取消勾选却仍写了空串」
    /// ——那会把「跟随全局」静默变成「不显示注释」。
    fn behavior_now(&self) -> BehaviorSeed {
        use crate::dialogs::tooltip_sections_dialog::unescape_template;
        let pick = |on: Signal<bool>, text: Signal<String>| -> Option<String> {
            on.get().then(|| text.get())
        };
        BehaviorSeed {
            punct: self.settings_bh_punct.get(),
            layout: self.settings_bh_layout.get(),
            // 与注释模板同一个 `pick`：未勾选恒读 `None`，不管文本框里还留着什么——
            // 否则用户勾上改了字体又取消勾选，脏判定会把这行误判成「改过」（落盘结果其实
            // 与打开时一样都是 `null`，见 `font_write`）。
            font_family: pick(self.settings_bh_font_over, self.settings_bh_font),
            text_orient: self.settings_bh_text_orient.get(),
            phrases: self.settings_bh_phrases.get(),
            // 注释模板框里是转义后的显示形态（见 [`Self::seed_comment_tpl`]），还原成真值。
            comment_v: pick(self.settings_bh_comment_v_on, self.settings_bh_comment_v)
                .map(|t| unescape_template(&t)),
            comment_h: pick(self.settings_bh_comment_h_on, self.settings_bh_comment_h)
                .map(|t| unescape_template(&t)),
            // 同上：勾上才 `Some`，勾上而一行都没改即 `Some({})`＝本方案一条自定义都不要。
            // 三态合成只此一处。
            punct_map: self.settings_bh_punct_map_on.get().then(|| {
                collect_punct_map(
                    &self.settings_bh_punct_cells,
                    &self.settings_bh_punct_extra.borrow(),
                )
            }),
        }
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
            // 脏判定必须在 apply 之前：apply 会把每一项都写一遍（勾选的写值、没勾的写
            // null），之后再比就永远是「有改动」。
            //
            // 三态之后脏判定只剩内容比对这一条——没有总开关，「勾/取消勾」本身就在
            // `is_dirty` 里（勾选框的 version 计入 version_sum）。
            let reset_all = self.settings_ct_reset_all.get();
            if self.settings_ct.is_dirty() || reset_all {
                // 先 reset 后 apply，顺序不能反：reset 清的是**全集**（含隐藏项），
                // apply 再把有控件的那些按勾选状态写一遍。反过来的话 reset 会把刚写好的
                // 用户值又抹成 null——而用户可能是「全部恢复跟随后又勾了一两项」。
                if reset_all {
                    self.settings_ct.reset(&mut cfg);
                }
                // 没勾的项写入 null——core 的 json_to_toml 跳过 null，于是 override 里不含
                // 这些键；而 saveConfig 是拿**方案文件基线**与 cfg 做 diff，故取消覆盖后
                // 该项回到方案作者的值，作者没写就回到全局。这是「取消覆盖」的落地方式。
                self.settings_ct.apply(&mut cfg);
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
        // 只在本方案有表（`has_aux_code_table`）且两项落盘结果与打开时不同时才写——
        // 否则给没有辅助码表的方案也塞一个 `engine.aux_code` 段，那会让 `json_diff`
        // 认定方案文件缺这个键，把整份快照写进 override。
        if has_aux_code_table(&cfg) {
            let (on_over, on, len_over, len) = (
                self.settings_aux_on_over.get(),
                self.settings_aux_on.get(),
                self.settings_aux_len_over.get(),
                self.settings_aux_len.get() as i32,
            );
            let now = aux_write(on_over, on, len_over, len);
            let (at_on_over, at_on, at_len_over, at_len) = self.settings_aux_at_open.get();
            let before = aux_write(at_on_over, at_on, at_len_over, at_len);
            if now != before {
                set_json_path(&mut cfg, &["engine", "aux_code", "enabled"], now.0);
                set_json_path(&mut cfg, &["engine", "aux_code", "max_phrase_len"], now.1);
                cfg_changed = true;
            }

            // 直接辅助码：单独比、单独写。非双拼方案打开与现在都是「不勾」（见
            // [`seed_aux_direct`]），两次都是 null ⇒ 不写，覆盖里手写的值原样留着。
            let now = aux_direct_write(
                self.settings_aux_direct_over.get(),
                self.settings_aux_direct.get(),
            );
            let (at_over, at_on) = self.settings_aux_direct_at_open.get();
            if now != aux_direct_write(at_over, at_on) {
                set_json_path(&mut cfg, &["engine", "aux_code", "direct"], now);
                cfg_changed = true;
            }

            // 辅助码来源。只读态（多条 / 下拉里没有的条目）原样不动：下拉表达不了它，
            // 写什么都是替用户改。★ 只看**当前**是否仍为只读——被「全部恢复跟随」清掉、
            // 或被用户取消勾选（见 [`aux_src_locked`]）后，就该走下面的正常路径落 null，
            // 否则那个按钮 / 那个勾选框对这一行点了等于没点。
            if !aux_src_locked(
                self.settings_aux_src_over.get(),
                self.settings_aux_src_manual.get(),
            ) {
                let opts = self.settings_aux_src_opts.borrow();
                let pick = |i: usize| opts.items.get(i).map(|x| x.entry.as_str());
                let now = aux_src_write(
                    self.settings_aux_src_over.get(),
                    pick(self.settings_aux_src_idx.get()),
                );
                let (at_over, at_idx, at_manual) = self.settings_aux_src_at_open.get();
                // 打开时是只读态：它原来的值 `aux_src_write` 表达不了，离开只读态本身就是改动。
                if at_manual || now != aux_src_write(at_over, pick(at_idx)) {
                    set_json_path(&mut cfg, &["engine", "aux_code", "files"], now);
                    cfg_changed = true;
                }
            }
        }

        // 本方案行为三段：与上面写进**同一份 cfg**，理由同上（saveConfig 是整份 diff
        // 后全量重写 override，分两次提交后者会把前者整个覆盖掉）。
        {
            let now = self.behavior_now();
            if now != *self.settings_bh_at_open.borrow() {
                // 用 `set_json_path` 而不是取段再重建：它只 insert 叶子键，段里本 UI 不管的
                // 项（`[phrases]` 的 categories / exclude_categories，目前只能手写）原样保留。
                // 同 `[overlay]` 那节的教训。
                // ⚠️ 整张表作一个叶子写下去。core 侧 `json_diff` 对 `custom_mappings`
                // 是**原子叶子**、`merge_toml` 对它是**整体替换**——这里若按行拆着写，
                // override 里就只剩改过的那几行，读侧整体替换后方案作者写的其余行全丢。
                set_json_path(
                    &mut cfg,
                    &["punct", "custom_mappings"],
                    punct_map_value(now.punct_map.as_ref()),
                );
                set_json_path(
                    &mut cfg,
                    &["punct", "mode"],
                    json!(punct_mode_value(now.punct)),
                );
                set_json_path(
                    &mut cfg,
                    &["candidate", "layout"],
                    json!(overlay_layout_value(now.layout)),
                );
                set_json_path(
                    &mut cfg,
                    &["candidate", "font_family"],
                    font_write(
                        now.font_family.is_some(),
                        now.font_family.as_deref().unwrap_or(""),
                    ),
                );
                set_json_path(
                    &mut cfg,
                    &["candidate", "text_orientation"],
                    json!(text_orient_value(now.text_orient)),
                );
                set_json_path(&mut cfg, &["phrases", "enabled"], json!(now.phrases));
                // ★ 注释模板的「取消覆盖」写 **null**，不是空串——空串是「本方案不显示注释」。
                // core 侧 `json_to_toml` 跳过 null，而 `saveConfig` 拿方案文件基线做 diff，
                // 两条既有性质合起来正好是「回传 null = 这一项不写进 override」。
                // 与辅助码「跟随全局必须写 null」同一形态。
                set_json_path(
                    &mut cfg,
                    &["candidate", "comment_template_vertical"],
                    comment_tpl_value(now.comment_v.as_deref()),
                );
                set_json_path(
                    &mut cfg,
                    &["candidate", "comment_template_horizontal"],
                    comment_tpl_value(now.comment_h.as_deref()),
                );
                cfg_changed = true;
            }
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

        // 诊断：本函数是方案设置对话框**唯一**的落地入口（关窗时由 stage_effect 触发），
        // 而它有多条静默 return —— 一旦没走到 merge_pending，用户的改动就无声消失，
        // 保存时表现为「没有变更」。报障时先看这条日志：没有它 = 压根没 stage；
        // 有它但 ka=false = 脏判定认为没变；都对则问题在 commit 或更后面。
        tracing::debug!(
            "stage_settings_edits: schema={id} cfg_changed={cfg_changed} dicts={} \
             ka_dirty={} sa_dirty={}",
            dicts.len(),
            self.settings_ka.is_dirty(),
            self.settings_sa.is_dirty(),
        );
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
                .filter("方案包", &["wpkg", "zip"])
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

    /// 同上，但包是协议链接下载来的临时文件：对话框关掉（确认 / 取消 / 被顶替）后删掉它。
    pub fn open_downloaded_import_confirm(
        &self,
        pkg: crate::schema_url_import::DownloadedPackage,
        preview: &Value,
    ) {
        self.open_import(PendingImport::Downloaded(Rc::new(pkg)), preview);
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
            PendingImport::Downloaded(pkg) => (
                "scheme.importPackage",
                json!({ "path": pkg.path().to_string_lossy(), "strategy": strategy }),
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
pub(crate) fn set_json_path(v: &mut Value, path: &[&str], leaf: Value) {
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
/// 生效值：enabled.unwrap_or(default_enabled.unwrap_or(false))——不写 `default_enabled` 视为未启用，
/// 与引擎 `DictionarySpec::should_load` 同口径（此前这里按启用显示，界面勾着、实际没加载）。
///
/// 用户分库（id 以 `user:` 开头）也登记在这里，但它们在词库窗的「用户词库 → 管理…」里管，
/// 不在此列出。
fn parse_extension_dicts(cfg: &Value) -> Vec<DictInfo> {
    let Some(arr) = cfg.get("dictionaries").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter(|d| !d.get("default").and_then(|v| v.as_bool()).unwrap_or(false))
        .filter(|d| {
            !d.get("id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id.starts_with("user:"))
        })
        .map(|d| {
            let default_enabled = d
                .get("default_enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
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
                // 方案手写的库可能没写 label，行标题空着就认不出是哪一项，退回 id。
                label: d
                    .get("label")
                    .and_then(|v| v.as_str())
                    .filter(|l| !l.is_empty())
                    .or_else(|| d.get("id").and_then(|v| v.as_str()))
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

/// 方案声明的扩展词库发现目录（`[[dictionary_dirs]]` 的 path），去掉首尾斜杠、跳过空值。
fn parse_dict_dirs(cfg: &Value) -> Vec<String> {
    cfg.get("dictionary_dirs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|d| d.get("path")?.as_str())
                .map(|p| p.trim_matches('/').to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// 打开用户目录下的发现目录（`<用户目录>/schemas/<dir>`），不存在就先建。
///
/// 只开用户目录这一层：安装目录多半只读，且用户放进去的东西升级时会被覆盖。core 扫描时
/// 用户层遮蔽安装层的同名文件，放这里就对。
fn open_user_dict_dir(dir: &str) -> Result<(), String> {
    let root = crate::user_dir::user_config_dir_for_mode(crate::mode::get())
        .ok_or_else(|| "找不到用户配置目录".to_string())?;
    let path = root.join("schemas").join(dir);
    std::fs::create_dir_all(&path).map_err(|e| format!("创建目录失败 {}: {e}", path.display()))?;
    crate::open_external::open_with_default_app(&path)
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
            crate::widgets::card_pad(build_compound_row(entry, is_enabled, &mgr, None))
        }
        SchemaListItem::SectionHeader {
            title,
            hidden_with_disabled,
        } => {
            // 与卡片里的二级分类同一套（跟随「界面细节」`subheading`），前后分割线也由它管。
            let el = crate::widgets::list_heading(&title);
            if hidden_with_disabled {
                el.visible_when(move || !hide_sig.get())
            } else {
                el
            }
        }
        SchemaListItem::Divider {
            hidden_with_disabled,
        } => {
            let d = crate::widgets::row_divider();
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
        .bg_role_alpha(Role::Accent, 0.08)
        .visible_when(sel_when(mgr, &entry.id));
    let sel_border = Element::leaf()
        .align(Align::Stretch)
        .corner(8.0)
        .border_role(Role::Accent, 1)
        .visible_when(sel_when(mgr, &entry.id));
    // 左缘指示条只在「界面细节」`nav_indicator` = 竖条+淡底时出现，与各处导航 / 列表选中一致。
    let li = crate::widgets::look().0[crate::look::NAV_INDICATOR];
    let sel_ind = sel_when(mgr, &entry.id);
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
        .visible_when(move || sel_ind() && li.get() == crate::look::nav::BAR);

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
        .child(crate::widgets::row_divider())
        .child(crate::widgets::card_pad(build_compound_row(
            entry,
            true,
            mgr,
            Some(handle),
        )))
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
    let cmt_dlg = build_comment_tpl_dialog(state);
    let punct_dlg = build_schema_punct_dialog(state);
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
        .child(cmt_dlg)
        // 自定义标点自带一层「单元格编辑」子对话框（在 build_punct_table_dialog 内部
        // 已叠成 stack），故整体排在这里即可，不必再拆一级。
        .child(punct_dlg)
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
///
/// 只挂在**主窗**（本对话框是 `build_schema_manager_dialog` 的一部分）。词库窗的右键
/// 导入也能选到 `.zip`/`.wpkg`，但那条路先把主窗拉到前台再弹选择器（见
/// `config_import::import_menu_items_via_main_window`），确认框照样出现在主窗里——
/// 词库窗不再挂一份：`import_confirm_visible` 是同一个 `Signal`，两棵树各画一遍就是
/// 两个窗口同时弹同一个确认框。
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

    // 上边距：头部紧贴在 TAB 条下的分隔线后面，0 边距时方案名几乎顶着那条线
    // （维护者：上 padding 太小）。12 与下方各节小标题到上一张卡片的距离相当。
    let mut col = Element::col()
        .width_match()
        .spacing(6)
        .padding_edges(Insets::new(0, 12, 0, 0))
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

/// 码表配置行的摘要文案：**报数**，不报「开/关」。
///
/// 逐项三态之后没有「整段自定义」这个状态了——用户可能只覆盖了一项。报数是这里唯一
/// 说得准的话：`0` 就是全跟随，非 0 就直接告诉他改过几项，不必点进去数。
///
/// 「跟的是谁」不放在这句里：那是**逐项**的（同一个方案里，A 项跟全局、B 项跟方案作者），
/// 一句总括的摘要说不清，硬说就会说错。来源写在每一行自己的那一列。纯函数便于单测。
fn ct_summary(custom: usize) -> String {
    if custom == 0 {
        "全部跟随".to_string()
    } else {
        format!("已自定义 {custom} 项")
    }
}

/// 「摘要 + 按钮」行——本对话框里最常见的一种（进入方式、按键自定义、按键总览…）。
///
/// 用 [`crate::widgets::setting_row`] 而不是手拼 row：那是主设置页每一条设置项的标准骨架
/// （标题 13 / 说明 12 muted / 右控件，间距取自 `FormTheme`）。本对话框此前每行各拼各的，
/// 于是同一屏里出现了三四种略有出入的排版。⇒ 新增行一律走这里。
///
/// 摘要固定 114px：长热键文本（`Ctrl+Shift+\`）不该撑宽右侧挤压左列。
fn summary_action_row(
    label: &str,
    desc: Option<&str>,
    disp: Signal<String>,
    btn_text: &str,
    open: Signal<bool>,
) -> Element {
    crate::widgets::setting_row(
        label,
        desc,
        Element::row()
            .cross(Align::Center)
            .spacing(8)
            .child(
                Element::label_signal(disp)
                    .font_size(12.5)
                    .fg_role(Role::TextMuted)
                    .max_lines(1)
                    .width(114),
            )
            .child(
                Element::button(btn_text)
                    .small()
                    .outline_soft()
                    .on_click(move |_| open.set(true)),
            ),
    )
}

fn build_section(kind: SettingsSection, state: &SchemaManagerState) -> Element {
    match kind {
        SettingsSection::Header => {
            let entry = state.settings_entry.borrow().clone().unwrap_or_default();
            build_settings_header(&entry)
        }
        SettingsSection::EntryKeys => {
            let entry = state.settings_entry.borrow().clone().unwrap_or_default();
            let visible = state.settings_hotkey_visible;
            // 「清除」按钮的闭包要动这三个：raw 是真相源（commit 侧据它增删条目），
            // disp 是行内显示，enabled 是对话框内部的派生态。
            let (hk_raw, hk_disp, hk_enabled) = (
                state.settings_hotkey_raw,
                state.settings_hotkey_disp,
                state.settings_hotkey_enabled,
            );
            let mut rows: Vec<Element> = Vec::new();

            // 说明这条配置**存在哪儿**：它写的不是方案自己的 override，而是全局
            // `keys.key_actions` 里一条指向本方案的条目。不点破的话，用户在「全局
            // 自定义按键」里看到一条自己没加过的绑定，会以为是哪里冒出来的脏数据，
            // 甚至手动删掉——而删掉就等于把这里的「切换到本方案」也一并清了。
            rows.push(crate::widgets::setting_row(
                "切换到本方案按键",
                Some("此配置同时出现在「按键 → 全局自定义按键」中"),
                Element::row()
                    .cross(Align::Center)
                    .spacing(8)
                    .child(
                        // 固定宽度：长热键文本（如 Ctrl+Shift+\）不该撑宽右侧挤压左列。
                        Element::label_signal(state.settings_hotkey_disp)
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
                    .child(
                        Element::button("清除")
                            .small()
                            .outline_soft()
                            .on_click(move |_| {
                                // 置空 raw 即解绑：commit 侧按 `raw` 为空处理为移除条目。
                                // enabled 同步置 false——它是对话框内部的派生量
                                // （`enabled.set(v != "none")`），不同步会让下次打开
                                // 对话框时「已启用」与「未设置」自相矛盾。
                                hk_enabled.set(false);
                                hk_disp.set("未设置".to_string());
                                hk_raw.set(String::new());
                            }),
                    ),
            ));

            // 引导键只给 **overlay 方案**：按下引导键临时进入、打完一段自动退回原方案。
            // 普通方案没有这条通路（切过去就留在那儿），给它们显示这一行只会让人以为能配。
            //
            // ★ 判据从 `entry.hidden` 改成 `entry.overlay`。hidden 当年是 overlay 的**代理**
            // （那时快符与英文都是隐藏方案），但它一直不准：隐藏方案未必有 overlay 生命周期，
            // 给这类方案配了引导键按下去什么都不会发生。现在 core 给出了真判据。
            if entry.overlay {
                rows.push(summary_action_row(
                    "引导键",
                    Some("按住它临时进入本方案，松开回到原方案"),
                    // 摘要由 effect 与勾选同步刷新（见 build_schema_settings_dialog），
                    // 不等 commit 落表——否则用户勾完看不到变化。
                    state.settings_trigger_disp,
                    "设置",
                    state.settings_trigger_visible,
                ));
            }

            // 按键绑定（方案顶层 `key_actions` / `session_actions`）：与上面两行同属
            // 「这个方案怎么进、键怎么用」，故并入本节而非另立组标题。所有方案都有。
            //
            // ⛔ **说明文案照抄全局那两项**（`keys.key_actions` / `keys.session_actions`
            // 的 hint），一字不改。两处是同一件事的全局版与方案版，各写各的必然漂移，
            // 而漂移出来的措辞往往是错的——本行此前写过「没在输入时按下某个键做什么」，
            // 而 `key_actions` **不是「空闲时专用」**：它在输入中同样生效
            // （用户 2026-08-22 已纠正过一次，2026-08-27 又踩）。
            // 见 [[project_key_resolver_unification]]「别再用『空闲时 / 未输入时』描述」。
            rows.push(summary_action_row(
                "本方案-按键自定义",
                Some("为按键指定功能，如临时拼音、切换方案"),
                state.settings_ka_disp,
                "设置",
                state.settings_ka_visible,
            ));
            rows.push(summary_action_row(
                "本方案-输入中按键自定义",
                Some("指定按键在输入中的功能，如翻页、选择候选"),
                state.settings_sa_disp,
                "设置",
                state.settings_sa_visible,
            ));

            // 按键总览：把上面两行加全局那几张表**合起来**回答「这个键现在到底干什么」。
            // 排在两个编辑入口之后 —— 它是查看用的，不是又一个编辑入口。
            rows.push(summary_action_row(
                "按键总览",
                Some("本方案与全局各表合起来，这个键现在到底干什么"),
                state.settings_ko_disp,
                "查看",
                state.settings_ko_visible,
            ));

            // ★ 与 TAB 同名是**有意的**：本节独占「按键设置」TAB，标题看着是重复了一遍，
            // 但别的 TAB 里每一节都有小标题，唯独这里没有会让结构看着缺一块
            // （2026-08-27 用户拍板：结构统一 > 避免这一处重复）。
            // ⇒ 想去掉重复就给这一节换个更具体的名字，别把标题拿掉。
            crate::widgets::dialog_section("按键设置", rows)
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
            crate::widgets::dialog_section(
                "方案参数",
                vec![crate::widgets::setting_row(
                    "双拼布局",
                    Some("决定声母韵母怎么落到键位上"),
                    Element::dropdown(labels, state.settings_layout).width(170),
                )],
            )
        }
        SettingsSection::AuxCode => {
            // 跟随值随全局基线而变——「跟随全局」到底是开还是关、最长词长是多少，来自
            // `followedBehavior`（core 的旁路只读字段）。用户在这一屏看不到全局页，
            // 不把值带过来就得来回切页确认。
            let cfg = state.settings_cfg.borrow();
            let followed_on = cfg
                .pointer("/followedBehavior/auxEnabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let followed_len = cfg
                .pointer("/followedBehavior/auxMaxPhraseLen")
                .and_then(Value::as_i64)
                .map(clamp_aux_len)
                .unwrap_or(0);
            let followed_direct = cfg
                .pointer("/followedBehavior/auxDirect")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            drop(cfg);

            let on_over = state.settings_aux_on_over;
            let on = state.settings_aux_on;
            let len_over = state.settings_aux_len_over;
            let len = state.settings_aux_len;

            let row_on = crate::widgets::setting_row_override(
                on_over,
                "本方案启用辅助码",
                Some("打出候选后按触发键，用字形码进一步筛选。触发键在「基本配置 → 按键设置」里配"),
                "跟随全局",
                Role::TextMuted,
                crate::widgets::override_control(
                    on_over,
                    Rc::new(|| true),
                    Element::switch(on),
                    &follow_text_bool(followed_on),
                    "",
                    AUX_SRC_W,
                ),
            );

            // 第二行的可用性看**生效值**：勾选了「启用辅助码」就用它当前的控件值,
            // 没勾就用跟随值——与 `schema_codetable::CtItemState::effective_value`
            // 同一个道理，「上游那项现在到底是开是关」不是它控件里存着什么。
            let gate: Rc<dyn Fn() -> bool> =
                Rc::new(move || if on_over.get() { on.get() } else { followed_on });

            // 直接辅助码：只在双拼方案显示（core 只认双拼）。随「启用辅助码」变淡，同
            // 「最长词长」——`enabled` 是总闸，关着时 `direct` 不起作用。
            let direct_over = state.settings_aux_direct_over;
            let gate_direct = gate.clone();
            let row_direct = state.settings_is_shuangpin.get().then(|| {
                crate::widgets::setting_row_override(
                    direct_over,
                    "本方案直接辅助码",
                    Some("音节后直接敲字形码筛选，不按引导键"),
                    "跟随全局",
                    Role::TextMuted,
                    crate::widgets::override_control(
                        direct_over,
                        gate.clone(),
                        Element::switch(state.settings_aux_direct),
                        &follow_text_bool(followed_direct),
                        "未启用辅助码",
                        AUX_SRC_W,
                    ),
                )
                .enabled_when(move || gate_direct())
            });

            let row_len = crate::widgets::setting_row_override(
                len_over,
                "辅助码最长词长",
                Some("超过这个字数的词组不参与筛选；0 = 不限。单字始终参与"),
                "跟随全局",
                Role::TextMuted,
                crate::widgets::override_control(
                    len_over,
                    gate.clone(),
                    Element::stepper(len, 0.0, 20.0, 1.0).width(AUX_CTRL_W),
                    &followed_len.to_string(),
                    "未启用辅助码",
                    AUX_SRC_W,
                ),
            )
            .enabled_when(move || gate());

            // 辅助码来源：R8.1 勾选行。不勾显示方案文件声明的来源（跟随方案），勾上选一个。
            // 下拉每项带徽章区分「输入方案 / 辅助码表」——同名的（如笔画方案与笔画码表）
            // 不看徽章分不出来。
            //
            // 只读态（多条 / 下拉里没有的条目）：`gate` 为假，控件位报出现状
            // （[`aux_src_manual_text`]），悬停提示给出撤销的路：取消勾选即离开只读回到跟随，
            // 见 [`aux_src_locked`]。
            //
            // ★ 这一行**有意不**像「最长词长」那样随「启用辅助码」变淡：来源与开关互不依赖，
            // 辅助码关着时先把来源选好无害，打开即按它生效。
            let src_over = state.settings_aux_src_over;
            let src_manual = state.settings_aux_src_manual;
            let items: Vec<windui::ui::select::DropdownItem> = state
                .settings_aux_src_opts
                .borrow()
                .items
                .iter()
                .map(|i| {
                    windui::ui::select::DropdownItem::new(i.label.clone()).badge(
                        if i.is_schema {
                            "输入方案"
                        } else {
                            "辅助码表"
                        },
                        Intent::Neutral,
                    )
                })
                .collect();
            let src_editor =
                Element::dropdown_items(items, state.settings_aux_src_idx).width(AUX_SRC_W);
            // 控件位只有 `AUX_SRC_W` 宽，折成两行会被行高裁掉半行（目视验证所见），
            // 撤销说明因此放悬停提示而不是挤进这一行。
            let manual_reason = state.settings_aux_src_manual_text.borrow().clone();
            let mut src_ctl = crate::widgets::override_control_signal(
                src_over,
                Rc::new(move || !aux_src_locked(src_over.get(), src_manual.get())),
                src_editor,
                state.settings_aux_src_follow,
                &manual_reason,
                AUX_SRC_W,
            );
            // 静态提示挂在控件位容器上只在**建树时即为只读**时做：那时这一格里永远只有
            // 两个文字标签（重新勾上仍回只读，下拉不会出现），容器吞命中不会挡掉任何交互。
            // 每次打开都重建本节，所以「建树时」就是「打开时」。
            if src_manual.get() {
                src_ctl = src_ctl.tooltip(
                    "这些来源手写在本方案的覆盖设置里，下拉表达不了。取消勾选即恢复跟随方案（保存后生效）",
                );
            }
            let row_src = crate::widgets::setting_row_override(
                src_over,
                "辅助码来源",
                // 一行放得下为准（左栏被 `AUX_SRC_W` 控件位挤窄，两行会被行高裁掉半行）。
                Some("用哪个方案或码表的字形码筛选"),
                "跟随方案",
                Role::TextMuted,
                src_ctl,
            );

            // 「全部恢复跟随」：样式/交互照 `SettingsSection::CodeTable` 那个同名按钮
            // （柔和描边而不是实心主按钮——它是兜底动作，不该比同行的卡片标题抢眼）。
            // 与码表那边不同的是它挂在**卡片自己的**标题行里，不是外层节头：这一节只有
            // 一张卡片，两者是同一件事，不用再分层。
            let st = state.clone();
            let reset_btn = move || {
                let st = st.clone();
                Element::button("全部恢复跟随")
                    .small()
                    .outline_soft()
                    .on_click(move |_| {
                        aux_reset_all(&st, followed_on, followed_len, followed_direct)
                    })
            };

            let mut rows = vec![row_on];
            rows.extend(row_direct);
            rows.extend([row_src, row_len]);
            let disp = state.settings_aux_disp;
            crate::widgets::dialog_section_full(
                "辅助码",
                None,
                Some(&|| {
                    Element::row()
                        .cross(Align::Center)
                        .spacing(10)
                        .child(crate::widgets::summary_label(disp))
                        .child(reset_btn())
                }),
                rows,
            )
        }
        SettingsSection::CodeTable => {
            // **内联**的具体设置，而不是「勾选 + 打开子对话框」：分了 TAB 之后这一屏有的是
            // 纵向空间，再套一层浮层只是多一次点击。
            //
            // 0.119 起没有总开关（原先是 `collapsible_group`，勾掉即整片折叠）：覆盖是
            // **逐项**的，每行左侧一个勾选框。总开关与逐项勾选并存会语义重叠——「总开关
            // 开着但一项都没勾」说不清是什么状态。整段的入口只剩「全部恢复跟随」。
            let ct = state.settings_ct.clone();
            let manifest = state.manifest.clone();
            let body = Element::host_signal(state.settings_ct_plan, move |g| ct.card(&manifest, g));
            let ct_reset = state.settings_ct.clone();
            let disp = state.settings_ct_disp;
            let reset_flag = state.settings_ct_reset_all.clone();
            // 节名是「码表配置」而不是「方案自定义」——后者是这个 TAB 的名字，
            // 同名的小标题读起来像套娃，且说不清它与同 TAB 里「本方案行为」那节的区别。
            //
            // 节头**不重复报摘要**：每个分类的小标题右侧已各报本组的数，节头再写一遍
            // 总数，两处都写「全部跟随」时就是纯噪音。总数仍在方案列表那一行上
            // （`settings_ct_disp` 的另一个消费点）。
            //
            // 各分类同在一张卡片里、以卡内小标题分隔（与方案配置卡同一套写法），而不是
            // 每类一张小卡——那是本对话框里唯一一处自成一格的卡片样式。
            crate::widgets::dialog_section_items(
                "码表配置",
                None,
                Some(&move || {
                    let (ct_reset, reset_flag) = (ct_reset.clone(), reset_flag.clone());
                    // 柔和描边（灰边 + 意图色文字、透明底）而不是实心主按钮：
                    // 它是一个**兜底动作**，与它同行的是这一节的小标题，实心色块
                    // 在那儿比标题还抢眼，看着像这一屏的主操作。
                    Element::button("全部恢复跟随")
                        .small()
                        .outline_soft()
                        .on_click(move |_| {
                            // 只动 UI（勾选清空 + 控件值拨回跟随值）。落盘统一走底栏的
                            // 保存——本对话框不该有自己的写盘入口，见 core 仓那条
                            // 「非 config.toml 落点一律登记 SideCommitter」。
                            ct_reset.revert_all();
                            // 标志给落盘那侧看：隐藏项没有控件，只能靠它去清。
                            reset_flag.set(true);
                            disp.set(ct_summary(ct_reset.custom_count()));
                        })
                }),
                vec![crate::widgets::CardItem::Bare(body)],
            )
        }
        SettingsSection::Overlay => {
            // 本节只在方案已声明 `[overlay]` 时出现，故无总开关、无置灰联动：
            // 两项恒可用。见 `SchemaManagerState` 里那组信号的说明。
            crate::widgets::dialog_section_hint(
                "特殊模式",
                "本方案可用引导键临时进入，打完一段自动退回原方案。引导键在上方「按键设置」里配。",
                vec![
                    crate::widgets::setting_row(
                        "进入即展示候选",
                        Some("刚进入、还没敲码时就列出本方案的首页候选"),
                        Element::checkbox("开启", state.settings_ov_show_all)
                            .small()
                            .fg_role(Role::Text),
                    ),
                    // 下拉而非分段控件：同一对话框里的「双拼布局」、码表配置里的各枚举项
                    // 用的都是 dropdown，同类选项换个控件形态会显得没来由。宽度也对齐
                    // 双拼布局那一行。
                    //
                    // ⚠️ 别加 `.small()`：它对非 Button/CheckBox/Switch 的 widget 会落到
                    // `config_button` 的 `debug_assert!(false, "small() 只能用于 button")`，
                    // debug 构建下当场 panic。这类链式 API 用**运行时 downcast** 做类型
                    // 检查，编译期一律放行——本节此前就是这么崩的。
                    crate::widgets::setting_row(
                        "候选排列",
                        Some("仅本模式期间生效，退出自动恢复"),
                        // 首项文案带跟随值（R8.2），随 `open_settings_at_id` 每次打开重算，
                        // 故绑 `Signal<Vec<String>>` 而不是静态 `OVERLAY_LAYOUT_LABELS`。
                        Element::dropdown_signal(
                            state.settings_layout_follow_labels,
                            state.settings_ov_layout,
                        )
                        .width(170),
                    ),
                ],
            )
        }
        SettingsSection::Behavior => {
            // ⚠️ dropdown / label 上别加 `.small()`：那是 Button/CheckBox/Switch 专属，
            // 落到别的 widget 会在 debug 构建下当场 panic（运行时 downcast，编译期放行）。
            //
            // ★ 注释模板**不内联在本节**：它是「勾选 + 输入框」两个控件一组、横竖各一组，
            // 四个控件铺在这一屏里，与旁边三行「一行一个控件」的节奏完全不同
            // （2026-08-26 用户原话：「候选注释又是比较杂乱的」）。收进子对话框后，
            // 这里只留一行摘要 + [设置]，与「按键设置」那几行同构。
            //
            // 候选字体跟随值从 `followedBehavior.fontFamily` 取（同 `AuxCode` 节读
            // `followedBehavior.auxEnabled` 的做法一样，是 core 只读旁路字段）；
            // 候选排列的跟随值已经在 `open_settings_at_id` 里算进
            // `settings_layout_follow_labels`，这里不用再读一遍。
            let followed_font = state
                .settings_cfg
                .borrow()
                .pointer("/followedBehavior/fontFamily")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let follow_font_disp = follow_text_font(&followed_font);

            let font_over = state.settings_bh_font_over;
            let font = state.settings_bh_font;
            let font_row = crate::widgets::setting_row_override(
                font_over,
                "候选文字字体",
                Some("只改候选词本身；序号与编码栏不跟随"),
                // 来源列只写来源；跟随值（字体名）由下面 override_control 的 follow_text 显示，
                // 两处都写字体名会读成「微软雅黑 … 微软雅黑」。
                "跟随全局",
                Role::TextMuted,
                crate::widgets::override_control(
                    font_over,
                    Rc::new(|| true),
                    Element::text_input(font, "留空同未勾选").width(AUX_CTRL_W),
                    &follow_font_disp,
                    "",
                    AUX_CTRL_W,
                ),
            );

            // 「全部恢复跟随」（R8.5）：只清标点 / 候选排列 / 候选字体这三个有跟随档的
            // 字段，见 [`behavior_reset_all`] 上的说明。样式照 `AuxCode` 卡片同名按钮。
            let st = state.clone();
            let reset_btn = move || {
                let st = st.clone();
                Element::button("全部恢复跟随")
                    .small()
                    .outline_soft()
                    .on_click(move |_| behavior_reset_all(&st))
            };

            crate::widgets::dialog_section_full(
                "本方案行为",
                Some("切到本方案时的表现。选「跟随全局」的项不受本方案影响。"),
                Some(&reset_btn),
                vec![
                    crate::widgets::setting_row(
                        "标点",
                        Some("中文标点 / 英文标点，或不干预"),
                        Element::dropdown(
                            // 标点跟随值是运行时状态（当前中英文态），没有静态值可带，
                            // 故这一档的文案固定，不像候选排列那样按 `followedBehavior`
                            // 现算（R8.2 例外，控制者裁定）。
                            vec!["跟随全局（沿用当前状态）", "中文标点", "英文标点"],
                            state.settings_bh_punct,
                        )
                        .width(BH_DD_W),
                    ),
                    crate::widgets::setting_row(
                        "候选排列",
                        Some("本方案期间的候选窗方向"),
                        // 首项文案带跟随值（R8.2），绑 `Signal<Vec<String>>`——
                        // 与 overlay 节那个下拉同一份标签表，同一个全局跟随值。
                        Element::dropdown_signal(
                            state.settings_layout_follow_labels,
                            state.settings_bh_layout,
                        )
                        .width(BH_DD_W),
                    ),
                    crate::widgets::setting_row(
                        "横排时文字排列",
                        // ⚠️ 一行装得下才算数（说明区超宽静默截断、不加省略号）。
                        // 「竖排时不生效」放在前面：选哪一项从标签就看得出来，
                        // 而「配了没反应」只有这句话能救。
                        Some("竖排时不生效。蒙古文选旋转 90°，汉字对联式选文字竖排"),
                        Element::dropdown(
                            TEXT_ORIENT_LABELS.to_vec(),
                            state.settings_bh_text_orient,
                        )
                        .width(BH_DD_W),
                    ),
                    font_row,
                    crate::widgets::setting_row(
                        "加载快捷短语",
                        Some(
                            "关闭后本方案不出快捷短语候选（与词库里的词组无关）。英文方案建议关闭：短语码常与英文词撞车",
                        ),
                        Element::checkbox("开启", state.settings_bh_phrases)
                            .small()
                            .fg_role(Role::Text),
                    ),
                    summary_action_row(
                        "候选注释",
                        Some("候选词右侧小字显示的内容，横排与竖排可分别设置"),
                        state.settings_bh_comment_disp,
                        "设置",
                        state.settings_bh_comment_visible,
                    ),
                    summary_action_row(
                        "自定义标点",
                        Some("本方案专用的标点映射表。开启后全局那份整份不再生效"),
                        state.settings_bh_punct_map_disp,
                        "设置",
                        state.settings_bh_punct_map_visible,
                    ),
                ],
            )
        }
        SettingsSection::ExtensionDicts => {
            // 行用与别处同一套 `setting_row`：勾选框当控件放右侧，词库名与说明落左列。
            // 原先是「勾选框带标签 + 下面缩进一行说明」，那是这一屏独一份的排版，
            // 与相邻各节对不齐。
            let rows: Vec<Element> = state
                .settings_dicts
                .borrow()
                .iter()
                .map(|row| {
                    let desc = (!row.description.is_empty()).then(|| row.description.clone());
                    crate::widgets::setting_row(
                        &row.label,
                        desc.as_deref(),
                        // 默认点击即翻转绑定的 on 信号（暂存）；实际提交在「保存」按钮统一进行。
                        Element::checkbox("启用", row.on)
                            .small()
                            .fg_role(Role::Text),
                    )
                })
                .collect();
            let dirs = state.settings_dict_dirs.borrow().clone();
            if dirs.is_empty() {
                return crate::widgets::dialog_section("扩展词库", rows);
            }
            let mut rows = rows;
            if rows.is_empty() {
                rows.push(
                    Element::label("还没有扩展词库。把自己的词库放进用户扩展词库目录即可。")
                        .font_size(11.0)
                        .fg_role(Role::TextMuted)
                        .width_match()
                        .padding_xy(0, 4),
                );
            }
            // 「打开」挂在节标题行：词库一多，挂在列表末尾的那一行就被挤到很下面。
            // 列表里大多是程序目录里的系统词库，按钮打开的却是用户目录——不说清楚，用户会以为
            // 「列了这么多、打开却是空的」。这句话放悬停提示而不是节说明：两行灰字压在
            // 标题下太占地方（维护者：影响布局整洁）。
            let feedback = state.feedback;
            let open_btn = |dir: String, label: String| {
                let tip = format!(
                    "打开 schemas/{dir}（用户目录）。自己的词库（*.dict.yaml）放进去，\
                     重开本窗口后出现在列表里；系统自带的词库不在这里"
                );
                Element::button(label)
                    .small()
                    .outline_soft()
                    .tooltip(tip)
                    .on_click(move |_| {
                        if let Err(e) = open_user_dict_dir(&dir) {
                            feedback.set(Some((ToastKind::Error, e)));
                        }
                    })
            };
            let action = || {
                if dirs.len() == 1 {
                    open_btn(dirs[0].clone(), "打开用户扩展词库目录".to_string())
                } else {
                    // 方案声明了多个目录（出厂没有）：各给一个按钮，按目录末段区分
                    dirs.iter().fold(Element::row().spacing(6), |r, d| {
                        let name = d.rsplit('/').next().unwrap_or(d).to_string();
                        r.child(open_btn(d.clone(), format!("打开 {name}")))
                    })
                }
            };
            crate::widgets::dialog_section_full("扩展词库", None, Some(&action), rows)
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
    // TAB 标签带自定义计数（「方案自定义 · 2」）：切在「基本配置」那边时，也得能看出
    // 另一边有没有改动——否则用户改完切走，回头完全不记得自己动过哪个方案。
    //
    // ⚠️ 外面套一层 `Vec` 是 `host_signal` 的签名所迫：它对 `Signal<Vec<T>>` 的**每个元素**
    // 调一次闭包，直接喂 `Signal<Vec<String>>` 会按标签数生成同样多个 segmented。
    // 装进单元素 Vec，闭包收到的才是整份标签表。
    //
    // 重建 segmented 不丢状态：选中项在 `tab` 信号里，不在 Element 上。
    let labels = state.settings_tab_labels;
    let tab_bar = Element::host_signal(labels, move |ls: Vec<String>| {
        Element::row()
            .width_match()
            .padding_xy(0, 8)
            .child(Element::segmented(ls, tab).weight(1.0))
    });

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
    //
    // 监听的是**整组信号的 version 之和**（含值信号），而摘要只用到勾选数——多刷几次
    // 无害，漏刷才是问题：只盯勾选框的话，`revert_all` 之后若哪天改成不动勾选框，
    // 摘要就会停在旧数字上。
    let ct_watch = state.settings_ct.clone();
    let ct_count = state.settings_ct.clone();
    let ct_disp = state.settings_ct_disp;
    let tab_labels = state.settings_tab_labels;
    // TAB 星号要覆盖**整个 TAB**，不只是码表配置那一节：同一屏上面还有「本方案行为」的
    // 标点与候选排列，它们各自的第 0 档就是「跟随全局」。只算码表的话，用户把标点改成
    // 「中文标点」，TAB 上却一点痕迹都没有——那个星号就是在说谎。
    //
    // 「加载短语」不计入：它是个没有「跟随」档的布尔，恒有值，算不出「有没有自定义」。
    let bh_punct = state.settings_bh_punct;
    let bh_layout = state.settings_bh_layout;
    // 注释模板只看**勾选态**：内容改了但没勾上等于没生效，星号跟着内容走会提前亮起来。
    // 与「加载短语」不计入同一个道理——星号回答的是「有没有项脱离了跟随」。
    let bh_cmt_v = state.settings_bh_comment_v_on;
    let bh_cmt_h = state.settings_bh_comment_h_on;
    let cmt_disp = state.settings_bh_comment_disp;
    // 自定义标点同理：只看勾选态。★ 不把 4×N 个单元格信号并进依赖——那是每帧遍历，
    // 而这一屏已经为「每帧轮询」付过一次代价（见 punct_map_summary 的说明）。
    let bh_punct_map = state.settings_bh_punct_map_on;
    let punct_disp = state.settings_bh_punct_map_disp;
    // 辅助码卡片摘要同理：只看各行的勾选态。`AuxCode` 也归「方案自定义」页
    // （见 `SettingsSection::tab`），星号要一并算上它，否则用户勾了辅助码却看不到 TAB 有变化。
    let aux_on_over = state.settings_aux_on_over;
    let aux_len_over = state.settings_aux_len_over;
    let aux_src_over = state.settings_aux_src_over;
    let aux_direct_over = state.settings_aux_direct_over;
    let aux_disp = state.settings_aux_disp;
    let ct_effect = crate::pages::dict::reactive::reactive_effect(
        move || {
            ct_watch
                .version_sum()
                .wrapping_add(bh_punct.version())
                .wrapping_add(bh_layout.version())
                .wrapping_add(bh_cmt_v.version())
                .wrapping_add(bh_cmt_h.version())
                .wrapping_add(bh_punct_map.version())
                .wrapping_add(aux_on_over.version())
                .wrapping_add(aux_len_over.version())
                .wrapping_add(aux_src_over.version())
                .wrapping_add(aux_direct_over.version())
        },
        move || {
            let n = ct_count.custom_count();
            let bh_custom = (bh_punct.get() != 0) as usize
                + (bh_layout.get() != 0) as usize
                + bh_cmt_v.get() as usize
                + bh_cmt_h.get() as usize
                + bh_punct_map.get() as usize;
            let aux_custom = aux_on_over.get() as usize
                + aux_len_over.get() as usize
                + aux_src_over.get() as usize
                + aux_direct_over.get() as usize;
            ct_disp.set(ct_summary(n));
            // 「候选注释」那一行的摘要与 TAB 星号同源（都只看勾选态），故顺带在这里推。
            // 另开一个 effect 只会多一处要同步的口径。
            cmt_disp.set(comment_summary(bh_cmt_v.get(), bh_cmt_h.get()));
            // 自定义标点那一行同源（也只看勾选态），一并在这里推。
            punct_disp.set(punct_map_summary(bh_punct_map.get()));
            // 辅助码卡片标题右侧那句同源，一并在这里推。
            aux_disp.set(ct_summary(aux_custom));
            // 各分类卡片标题右侧那句也在这里推——它与整段摘要同源（勾选状态），
            // 分两个 effect 只会多一次遍历，还得担心两处口径漂移。
            ct_count.refresh_group_summaries();
            // TAB 标签只打一个星号，不报数：数已经在节头和每张卡片上了，TAB 要回答的
            // 只是「这边有没有动过」——切在另一个 TAB 时也一眼看得见。
            // 星号打在「方案自定义」那一页：本 effect 统计的三类（码表覆盖、本方案行为、
            // 辅助码）都在那一页。⚠️ 用 `CUSTOM_TAB` 而不是字面下标——TAB 数量已经反转过
            // 两次，写死的 `1` 在上一次反转里正好还指对，下一次就未必。
            let mut ls: Vec<String> = SETTINGS_TABS.iter().map(|s| s.to_string()).collect();
            if n + bh_custom + aux_custom > 0 {
                ls[CUSTOM_TAB] = format!("{} *", SETTINGS_TABS[CUSTOM_TAB]);
            }
            tab_labels.set(vec![ls]);
        },
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

    /// 摘要报的是**项数**。
    ///
    /// 逐项三态之后没有「整段自定义」这个状态了，摘要唯一说得准的话就是「改过几项」。
    ///
    /// ⚠️ 「跟的是谁」**不能**放进这句总括：三态下同一个方案里 A 项可以跟全局、B 项跟
    /// 方案作者，一句话说不清。此前那版摘要（「跟随全局配置」/「跟随内置基线」）正是
    /// 因此退役——它还顺带用错了判据（`[schema].hidden` 而非 `[overlay]` 段）。
    /// 来源现在写在每一行自己那一列，由 `CtSeed::follow_label` + 「方案自带」给出。
    #[test]
    fn ct_summary_reports_the_number_of_overridden_items() {
        assert_eq!(ct_summary(0), "全部跟随");
        assert_eq!(ct_summary(1), "已自定义 1 项");
        assert_eq!(ct_summary(7), "已自定义 7 项");
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
                warning: String::new(),
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
                warning: String::new(),
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

    /// 协议下载来的临时包跟着待导入项走：取消、被下一个包顶替、确认导入后都删掉文件；
    /// 用户自己的本地包（`open_import_confirm` 那条）永不删。
    #[test]
    fn downloaded_package_is_deleted_when_dialog_done() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let dir = std::env::temp_dir().join(format!("wind_setting_dl_mgr_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let save = || crate::schema_url_import::save_package(&dir, b"PK").unwrap();
        let preview = json!({ "willAdd": ["x"], "conflicts": [] });
        let mgr = SchemaManagerState::new();

        let a = save();
        let pa = a.path().to_path_buf();
        mgr.open_downloaded_import_confirm(a, &preview);
        assert!(pa.exists(), "对话框开着时文件要在");
        mgr.cancel_import();
        assert!(!pa.exists(), "取消即删");

        let b = save();
        let pb = b.path().to_path_buf();
        mgr.open_downloaded_import_confirm(b, &preview);
        let c = save();
        let pc = c.path().to_path_buf();
        mgr.open_downloaded_import_confirm(c, &preview);
        assert!(!pb.exists(), "被下一个包顶替即删");
        mgr.confirm_import("merge");
        assert!(!pc.exists(), "确认导入后删");

        let local = dir.join("mine.wpkg");
        std::fs::write(&local, b"PK").unwrap();
        mgr.open_import_confirm(local.clone(), &preview);
        mgr.cancel_import();
        assert!(local.exists(), "用户自己的包不删");

        let _ = std::fs::remove_dir_all(&dir);
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

    /// 「本方案行为」三项的播种：缺段时的默认值必须与 core 一致。
    ///
    /// ★ 短语那项默认 `true` 是关键：绝大多数方案文件不写 `[phrases]` 段，取 `false`
    /// 作默认会让它们在界面上全部显示成「不加载短语」——而实际是加载的。用户照着界面
    /// 去开一下，反而在 override 里写死了一个本来就生效的值。
    #[test]
    fn seed_behavior_defaults_match_core() {
        assert_eq!(
            seed_behavior(&json!({})),
            BehaviorSeed::default(),
            "三段全缺：两个下标跟随全局，短语加载，注释模板未设置"
        );
        assert_eq!(
            seed_behavior(&json!({
                "punct": { "mode": "english" },
                "candidate": { "layout": "vertical" },
                "phrases": { "enabled": false }
            })),
            BehaviorSeed {
                punct: 2,
                layout: 1,
                phrases: false,
                ..Default::default()
            },
            "英文方案那套写法"
        );
        assert_eq!(
            seed_behavior(&json!({ "punct": { "mode": "chinese" } })),
            BehaviorSeed {
                punct: 1,
                ..Default::default()
            },
            "只写一段时另两段仍取默认"
        );
    }

    /// 「候选注释」行的摘要只看**勾选态**，且四格各不相同。
    ///
    /// 摘要跟着输入框内容走的话，用户打字打到一半、还没勾上「自定义」，行上就已经写着
    /// 「已自定义」——而那时保存下去什么也不会变。
    #[test]
    fn comment_summary_reads_checkboxes_only() {
        assert_eq!(comment_summary(false, false), "跟随全局");
        assert_eq!(comment_summary(true, false), "竖排已自定义");
        assert_eq!(comment_summary(false, true), "横排已自定义");
        assert_eq!(comment_summary(true, true), "横竖均已自定义");
    }

    /// ★★ 注释模板是**三态**：键缺失 / 有内容 / 空串，三者互不相同。
    ///
    /// 最容易写错的是把 `Some("")` 当成「未设置」——那会让「本方案不显示注释」
    /// 在重开设置页后变回「跟随全局」，用户配的东西静默消失。
    #[test]
    fn seed_behavior_reads_comment_tri_state() {
        let seed = seed_behavior(&json!({}));
        assert_eq!(seed.comment_v, None, "键缺失 = 未设置");
        assert_eq!(seed.comment_h, None);

        let seed = seed_behavior(&json!({
            "candidate": { "comment_template_vertical": "${code_hint}" }
        }));
        assert_eq!(seed.comment_v.as_deref(), Some("${code_hint}"));
        assert_eq!(seed.comment_h, None, "横竖各自独立，只配一个是常见用法");

        let seed = seed_behavior(&json!({
            "candidate": { "comment_template_horizontal": "" }
        }));
        assert_eq!(
            seed.comment_h.as_deref(),
            Some(""),
            "空串是「本方案不显示注释」，不是「未设置」"
        );

        // core 侧「取消覆盖」回传的就是 null，读回来必须还原成未设置。
        let seed = seed_behavior(&json!({
            "candidate": { "comment_template_vertical": null }
        }));
        assert_eq!(seed.comment_v, None, "null 读回来是未设置");
    }

    /// ★★ 「取消覆盖」写 `null` 而不是空串。
    ///
    /// 写空串的话，用户取消勾选后得到的是「本方案不显示注释」——与他要的「跟随全局」
    /// 恰好相反，且界面上看不出来（两者都是空输入框）。
    #[test]
    fn comment_tpl_value_uses_null_to_clear() {
        assert_eq!(comment_tpl_value(None), Value::Null, "未勾选必须写 null");
        assert_eq!(comment_tpl_value(Some("")), json!(""), "勾上留空写空串");
        assert_eq!(comment_tpl_value(Some("${dict}")), json!("${dict}"));
    }

    /// 注释模板的 seed → 回写 → 再 seed 必须闭合，否则「保存后重开设置页，配的东西没了」。
    #[test]
    fn comment_tpl_round_trip() {
        for tpl in [None, Some(""), Some("${code_hint}")] {
            let mut cfg = json!({});
            set_json_path(
                &mut cfg,
                &["candidate", "comment_template_vertical"],
                comment_tpl_value(tpl),
            );
            assert_eq!(
                seed_behavior(&cfg).comment_v.as_deref(),
                tpl,
                "{tpl:?} 往返不闭合"
            );
        }
    }

    /// F2：下拉「跟随」档必须带上当前跟随值，不能只写「跟随全局」四个字。
    #[test]
    fn follow_label_carries_value() {
        assert_eq!(
            follow_option_label("全局", layout_label("horizontal")),
            "跟随全局（横排）"
        );
        assert_eq!(
            follow_option_label("全局", layout_label("vertical")),
            "跟随全局（竖排）"
        );
    }

    /// F3：候选字体改勾选行——未勾选、或勾选但留空，都写 `null`，绝不写空串。
    ///
    /// 空串曾是「不覆盖」的哨兵；改勾选行之后空串没有专属含义了，写出去只会被 core
    /// 读成「本方案把候选字体覆盖成空字体」，与用户实际想要的「跟随外观页」正相反。
    #[test]
    fn font_unchecked_writes_null_not_empty_string() {
        assert_eq!(font_write(false, "楷体"), Value::Null);
        assert_eq!(font_write(true, ""), Value::Null);
        assert_eq!(font_write(true, "楷体"), json!("楷体"));
    }

    /// 读取时兼容旧文件：改字段前存下的空串 `font_family = ""` 也要读成「未设置」，
    /// 不能读成「本方案覆盖成空字体」（那样界面会显示成已勾选、内容为空，与实际想表达的
    /// 「跟随外观页」不符）。
    #[test]
    fn seed_behavior_treats_legacy_empty_font_as_unset() {
        assert_eq!(
            seed_behavior(&json!({ "candidate": { "font_family": "" } })).font_family,
            None
        );
        assert_eq!(
            seed_behavior(&json!({ "candidate": { "font_family": "楷体" } })).font_family,
            Some("楷体".to_string())
        );
        assert_eq!(seed_behavior(&json!({})).font_family, None);
    }

    /// R8.5：「本方案行为」节的「全部恢复跟随」把三项拨回跟随（下标 0 / 取消勾选），
    /// 「横排时文字排列」没有跟随档（设计如此），不该被这个按钮动到。
    ///
    /// 驱动的是按钮闭包实际调用的 [`behavior_reset_all`] 与 `SchemaManagerState` 的真信号。
    #[test]
    fn behavior_reset_all_clears_follow_fields() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let state = SchemaManagerState::new();
        state.settings_bh_punct.set(2);
        state.settings_bh_layout.set(1);
        state.settings_bh_font_over.set(true);
        state.settings_bh_font.set("楷体".to_string());
        state.settings_bh_text_orient.set(1);

        behavior_reset_all(&state);

        assert_eq!(state.settings_bh_punct.get(), 0, "标点回跟随档");
        assert_eq!(state.settings_bh_layout.get(), 0, "候选排列回跟随档");
        assert!(!state.settings_bh_font_over.get(), "候选字体取消勾选");
        assert_eq!(
            font_write(
                state.settings_bh_font_over.get(),
                &state.settings_bh_font.get()
            ),
            Value::Null,
            "取消勾选后字体落盘为 null（跟随外观页），不论文本框里残留什么"
        );
        assert_eq!(
            state.settings_bh_text_orient.get(),
            1,
            "无跟随档的项不被动到"
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 未知取值一律回落「跟随全局」，不猜方向。
    ///
    /// 方案文件是可手写的，写错一个词（`englsh`）不该被解释成某个具体档位——那会让用户
    /// 看到一个他没选过的设置，并且一保存就把这个错误值固化成正确值。
    #[test]
    fn unknown_values_fall_back_to_follow() {
        assert_eq!(
            seed_behavior(&json!({ "punct": { "mode": "englsh" } })).punct,
            0
        );
        assert_eq!(
            seed_behavior(&json!({ "candidate": { "layout": "斜排" } })).layout,
            0
        );
    }

    /// 三项的下标 ↔ 字符串往返必须闭合，否则「保存后重开设置页，选项变了」。
    #[test]
    fn behavior_value_round_trip() {
        for i in 0..3 {
            assert_eq!(punct_mode_idx(punct_mode_value(i)), i, "punct 第 {i} 档");
            assert_eq!(
                overlay_layout_idx(overlay_layout_value(i)),
                i,
                "layout 第 {i} 档"
            );
        }
    }

    /// ★★ 保存只改本 UI 管的字段——`[phrases]` 里手写的 `categories` 不得被抹掉。
    ///
    /// 分类目前没有图形界面，只能手写方案文件。若保存时用字面量重建整个段，用户改一次
    /// 「加载短语」就把自己配的分类清单丢了，且完全静默。这正是已删除的 `special_modes.rs`
    /// 当年那条教训的同形（见 `[overlay]` 那节的保存代码）。
    #[test]
    fn behavior_save_preserves_handwritten_fields() {
        let mut cfg = json!({
            "phrases": { "enabled": true, "categories": ["工作"], "exclude_categories": ["日期"] },
            "candidate": { "layout": "vertical" },
            "engine": { "type": "codetable" }
        });
        // 模拟保存路径：三项都写，用的是 set_json_path（只 insert 叶子）。
        set_json_path(&mut cfg, &["punct", "mode"], json!(punct_mode_value(2)));
        set_json_path(
            &mut cfg,
            &["candidate", "layout"],
            json!(overlay_layout_value(2)),
        );
        set_json_path(&mut cfg, &["phrases", "enabled"], json!(false));

        assert_eq!(
            cfg["phrases"]["enabled"],
            json!(false),
            "本 UI 管的字段要改到"
        );
        assert_eq!(
            cfg["phrases"]["categories"],
            json!(["工作"]),
            "手写的分类白名单必须原样保留"
        );
        assert_eq!(
            cfg["phrases"]["exclude_categories"],
            json!(["日期"]),
            "黑名单同理"
        );
        assert_eq!(cfg["candidate"]["layout"], json!("horizontal"));
        assert_eq!(cfg["punct"]["mode"], json!("english"), "缺段时应能创建");
        assert_eq!(
            cfg["engine"]["type"],
            json!("codetable"),
            "同级的其它段不受影响"
        );
    }

    // ── 方案级自定义标点表 ─────────────────────────────────────────────

    /// ★★ 三态**只能由「键在不在」区分**，空表不是「跟随全局」。
    ///
    /// 最容易写错的是把 `Some({})` 当成未设置——那会让「本方案一条自定义标点都不要」
    /// 在重开设置页后变回「跟随全局」，于是全局那张表又冒出来，用户配的东西静默失效。
    /// 与注释模板的 `Some("")` 是同一个形状。
    #[test]
    fn seed_behavior_reads_punct_map_tri_state() {
        assert_eq!(
            seed_behavior(&json!({})).punct_map,
            None,
            "键缺失 = 跟随全局"
        );
        assert_eq!(
            seed_behavior(&json!({ "punct": { "custom_mappings": {} } })).punct_map,
            Some(serde_json::Map::new()),
            "空表 = 本方案一条都不要，必须与「跟随」可区分"
        );
        // core「取消覆盖」写的是 null，读回来要还原成「未设置」而不是 Some。
        assert_eq!(
            seed_behavior(&json!({ "punct": { "custom_mappings": null } })).punct_map,
            None,
            "null 等同键缺失"
        );
        let seed = seed_behavior(&json!({
            "punct": { "custom_mappings": { ".": ["·", "", "", ""] } }
        }));
        assert_eq!(
            seed.punct_map.as_ref().and_then(|m| m.get(".")),
            Some(&json!(["·", "", "", ""])),
            "列序原样带过，不在设置端重排（换序只许在 PUNCT_DISPLAY_COLS 一处发生）"
        );
    }

    /// 未勾选必须写 `null`——省略这个键的话 `json_diff` 看不到这一项，
    /// 方案作者写死的表就再也取消不掉了。
    #[test]
    fn punct_map_value_uses_null_to_clear() {
        assert_eq!(punct_map_value(None), Value::Null, "未勾选必须写 null");
        assert_eq!(
            punct_map_value(Some(&serde_json::Map::new())),
            json!({}),
            "勾上而一行没改 = 空表，如实写"
        );
        let mut m = serde_json::Map::new();
        m.insert(".".into(), json!(["·"]));
        assert_eq!(punct_map_value(Some(&m)), json!({ ".": ["·"] }));
    }

    /// seed → 回写 → 再 seed 必须闭合，否则「保存后重开设置页，配的东西没了」。
    #[test]
    fn punct_map_round_trip() {
        let mut table = serde_json::Map::new();
        table.insert(".".into(), json!(["·", "", "", ""]));
        for map in [None, Some(serde_json::Map::new()), Some(table)] {
            let mut cfg = json!({});
            set_json_path(
                &mut cfg,
                &["punct", "custom_mappings"],
                punct_map_value(map.as_ref()),
            );
            // null 落进 cfg 后 seed 要读回 None——这一步正是三态闭合的关键格。
            assert_eq!(seed_behavior(&cfg).punct_map, map, "三态回环必须闭合");
        }
    }

    /// ★★ 脏判定基线 = **表格反算出来的形状**，不是配置原文。
    ///
    /// 这条防的是「打开方案设置看一眼再关掉，就提示有未保存改动、每次保存都写盘」——
    /// 用户什么都没动。两种触发都来自方案作者手写的方案文件：缺列的行、以及四格恰好
    /// 全等内置默认的行，两者播种后反算回来都会被 `punct_row_override` 裁掉。
    ///
    /// ⚠️ 断言必须是 `normalize(normalize(x)) == normalize(x)`（幂等）**加上**
    /// 「原文 != 归一后」——只测幂等的话，`normalize` 写成恒等函数也照样通过。
    #[test]
    fn normalize_schema_punct_map_matches_table_roundtrip() {
        let raw: serde_json::Map<String, Value> = json!({
            // 缺列：播种时后三格回落默认 ⇒ 四格全等默认 ⇒ 该行应被裁掉
            "!": ["！"],
            // 四格全等内置默认 ⇒ 同样应被裁掉
            "@": ["@", "＠", "＠", "@"],
            // 真正的自定义 ⇒ 保留
            "%": ["％", "％", "％", "%"],
        })
        .as_object()
        .unwrap()
        .clone();

        let norm = normalize_schema_punct_map(&raw);
        assert!(
            !norm.contains_key("!"),
            "缺列但等价于默认的行必须被裁掉，否则与表格反算的结果对不上 ⇒ 恒判脏"
        );
        assert!(!norm.contains_key("@"), "四格全等默认的行同样要裁掉");
        assert_eq!(
            norm.get("%"),
            Some(&json!(["％", "％", "％", "%"])),
            "真正的自定义要留下"
        );
        assert_ne!(
            norm, raw,
            "前置条件：这份夹具必须真的会被归一改变，否则本测试测不到东西"
        );
        assert_eq!(
            normalize_schema_punct_map(&norm),
            norm,
            "幂等：归一后的表再归一不变"
        );
    }

    /// ★ `PUNCT_DEFAULTS` 之外的源字符必须原样透传。
    ///
    /// 表格没有对应行 ⇒ 丢掉它就等于「每次保存都把方案作者写的这一行删掉」。
    /// 方案表是**整表替换**，被裁掉的行不是「保持原样」而是「不存在」——
    /// 全局那处能容忍丢弃（那边归一只作用于 diff 基线），这里不能。
    #[test]
    fn punct_extra_rows_survive_normalize() {
        let raw: serde_json::Map<String, Value> = json!({
            "%": ["％", "％", "％", "%"],
            "§": ["§", "§", "§", "S"],
        })
        .as_object()
        .unwrap()
        .clone();
        assert!(
            !crate::manifest::PUNCT_DEFAULTS
                .iter()
                .any(|(_, tok, _)| *tok == "§"),
            "前置条件：被测源字符必须真的不在表里"
        );
        let norm = normalize_schema_punct_map(&raw);
        assert_eq!(
            norm.get("§"),
            Some(&json!(["§", "§", "§", "S"])),
            "★ 表外源字符必须原样带回，否则每次保存都会静默删掉它"
        );
    }

    /// 摘要只看勾选态（与候选注释同构）；行数不进摘要，理由见 `punct_map_summary`。
    #[test]
    fn punct_map_summary_reads_checkbox_only() {
        assert_eq!(punct_map_summary(false), "跟随全局");
        assert_eq!(punct_map_summary(true), "本方案自定义");
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
        assert!(
            !rows[3].enabled,
            "d: 无字段 → 未启用（与引擎 should_load 同口径）"
        );
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

    /// 没写 label 的库，行标题退回 id——空标题的一行认不出是哪个库。
    #[test]
    fn extension_dict_without_label_falls_back_to_id() {
        let rows = parse_extension_dicts(&json!({ "dictionaries": [
            { "id": "main", "default": true },
            { "id": "ext_a", "label": "" },
            { "id": "ext_b", "label": "成语" },
        ]}));
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["ext_a", "成语"]);
    }

    /// 用户分库（`user:` 前缀）在词库窗里管，方案管理的扩展词库列表不列它们。
    #[test]
    fn extension_dicts_skip_user_libraries() {
        let rows = parse_extension_dicts(&json!({ "dictionaries": [
            { "id": "main", "default": true },
            { "id": "dir:ext/成语.dict.yaml", "label": "成语" },
            { "id": "user:行业词", "label": "行业词", "enabled": true },
        ]}));
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["dir:ext/成语.dict.yaml"]);
    }

    /// 发现目录：取 path、去首尾斜杠、跳过空值；没声明就是空（这一节随之不显示）。
    #[test]
    fn parse_dict_dirs_trims_and_skips_empty() {
        assert!(parse_dict_dirs(&json!({})).is_empty());
        let dirs = parse_dict_dirs(&json!({ "dictionary_dirs": [
            { "path": "pinyin/cn_dicts_cell/", "type": "rime_pinyin" },
            { "path": "" },
            { "type": "rime_codetable" },
        ]}));
        assert_eq!(dirs, ["pinyin/cn_dicts_cell"]);
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
        // ★ 旋转/文字竖排**不在这条轴上**。曾把它们当成候选排列的第 3、4 个取值，
        // 拆分后若有人「顺手补回来」，这里立刻红——两根轴合并回去就白拆了。
        assert_eq!(
            overlay_layout_idx("rotated"),
            0,
            "文字排列不该出现在候选排列的值域里"
        );
    }

    /// ★★ 「横排时文字排列」的下标 ↔ core `TextOrientation` 值域，**逐项**比对。
    ///
    /// 这一条比隔壁的布局那条更硬：它直接拿 core 的类型当真相源（wind-config 在本仓是
    /// 测试期可用的），不经 capability 快照——因为 `text_orientation` 是**方案键**，
    /// 压根不在 `config.toml` 的注册表里，capability 里查不到它。
    ///
    /// 没有这条的话，跨仓拼错的表现是：用户在设置页选了「文字竖排」，core 解析不出来
    /// 回落 `normal`，界面显示得好好的而候选一点没变。
    #[test]
    fn text_orient_index_maps_to_core_value_domain() {
        use wind_config::TextOrientation;
        let core: Vec<&str> = TextOrientation::ALL.iter().map(|t| t.as_str()).collect();
        assert_eq!(
            TEXT_ORIENTS.to_vec(),
            core,
            "取值与 core 漂移了（含顺序——下拉按这个顺序排）"
        );
        assert_eq!(
            TEXT_ORIENT_LABELS.len(),
            TEXT_ORIENTS.len(),
            "标签与值域必须逐项对应"
        );
        for (idx, v) in TEXT_ORIENTS.iter().enumerate() {
            assert_eq!(text_orient_value(idx), *v);
            assert_eq!(text_orient_idx(v), idx, "反向映射应自洽");
            // core 那侧确实认得这个字符串（拼错的话它会回落 Normal）。
            assert_eq!(
                TextOrientation::from_str_or_normal(v).as_str(),
                *v,
                "core 解析不出 {v:?}"
            );
        }
        assert_eq!(text_orient_value(99), "normal", "越界应回落默认");
        assert_eq!(text_orient_idx("不存在的值"), 0, "未知值回落默认");
    }

    /// 播种与回读：方案文件里写的 `text_orientation` 必须落到界面下标上。
    ///
    /// ★ 与候选排列**互不干扰**是重点：只断言自己那一项的话，一个把两项写进同一个字段的
    /// 实现照样通过，而那正是这次要拆掉的东西。
    #[test]
    fn seed_reads_text_orientation_independently_of_layout() {
        let seed = seed_behavior(&json!({
            "candidate": { "layout": "vertical", "text_orientation": "upright" }
        }));
        assert_eq!(seed.layout, overlay_layout_idx("vertical"));
        assert_eq!(seed.text_orient, text_orient_idx("upright"));
        // 只写一项时另一项保持默认，不被牵连。
        let only_text = seed_behavior(&json!({
            "candidate": { "text_orientation": "rotated" }
        }));
        assert_eq!(only_text.layout, 0, "没写 layout 就该是跟随全局");
        assert_eq!(only_text.text_orient, text_orient_idx("rotated"));
        let only_layout = seed_behavior(&json!({ "candidate": { "layout": "horizontal" } }));
        assert_eq!(
            only_layout.text_orient, 0,
            "没写 text_orientation 就该是默认"
        );
        assert_eq!(only_layout.layout, overlay_layout_idx("horizontal"));
    }

    /// ★★ 值域必须与 core 的 `LayoutIntent` **等长**。
    ///
    /// core 加了取值而这里没跟，后果不是「选不到新值」而是**静默改写用户配置**：
    /// 读取时未知值回落 0，保存时 0 写回 `follow` ⇒ 方案里手写的新值，
    /// 用户打开一次设置页再保存就没了。`rotated` 补进来之前正是这个状态。
    ///
    /// 跨仓无编译期约束，故拿 core 的 capability 值域当真相源逐项比对——
    /// 只断言「条数相等」的话，两边各写了不同的四个值也照样通过。
    #[test]
    fn layout_value_domain_matches_core_capability() {
        let caps = crate::capabilities::Capabilities::from_embedded();
        let core_values = &caps
            .entries
            .get("input.temp_english.candidate_layout")
            .expect("core 应登记 candidate_layout 的枚举值域")
            .values;
        assert_eq!(
            OVERLAY_LAYOUTS.len(),
            core_values.len(),
            "值域条数与 core 不符：core={core_values:?} 本仓={OVERLAY_LAYOUTS:?}
             —— 少一条会让用户手写的新值在「打开设置页再保存」时被静默改写"
        );
        for v in core_values {
            assert!(
                OVERLAY_LAYOUTS.contains(&v.as_str()),
                "core 有取值 {v:?} 而本仓没有：{OVERLAY_LAYOUTS:?}"
            );
        }
        assert_eq!(
            OVERLAY_LAYOUT_LABELS.len(),
            OVERLAY_LAYOUTS.len(),
            "标签与值域必须逐项对应"
        );
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

    /// ★★ 未勾选的一项必须落成 **null**，不是 false/0。
    ///
    /// core 的 `json_to_toml` 跳过 null ⇒ 该键不进 override ⇒ 取值回落全局基线。写具体值
    /// 的话就成了「显式定值」——用户明明没勾，写回去却把这个方案钉死成一个值，
    /// 而且从配置文件上看不出是谁写的。两行（启用 / 最长词长）各自独立，互不联动。
    #[test]
    fn aux_unchecked_rows_write_null() {
        assert_eq!(aux_write(false, true, false, 4), (Value::Null, Value::Null));
    }

    /// 勾选的一项落成勾选框当前值——与「跟随」互斥的另一态。
    #[test]
    fn aux_checked_rows_write_values() {
        assert_eq!(aux_write(true, false, true, 3), (json!(false), json!(3)));
    }

    /// 「全部恢复跟随」：起点是两行都已覆盖、控件值与跟随值不同，复位要把两个勾选框
    /// 一并清空、控件值拨回跟随值、摘要归零；落盘按 `aux_write` 的既有规则回落成 `null`。
    ///
    /// 驱动的是按钮闭包实际调用的 [`aux_reset_all`] 与 `SchemaManagerState` 的真信号。
    #[test]
    fn aux_reset_all_clears_both_rows_writes_null() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let state = SchemaManagerState::new();
        state.settings_aux_on_over.set(true);
        state.settings_aux_on.set(false);
        state.settings_aux_len_over.set(true);
        state.settings_aux_len.set(9.0);
        state.settings_aux_disp.set(ct_summary(2));

        aux_reset_all(&state, true, 4, false);

        assert!(!state.settings_aux_on_over.get());
        assert!(!state.settings_aux_len_over.get());
        assert!(state.settings_aux_on.get(), "启用开关拨回跟随值");
        assert_eq!(state.settings_aux_len.get(), 4.0, "最长词长拨回跟随值");
        assert_eq!(state.settings_aux_disp.get(), ct_summary(0));
        assert_eq!(
            aux_write(
                state.settings_aux_on_over.get(),
                state.settings_aux_on.get(),
                state.settings_aux_len_over.get(),
                state.settings_aux_len.get() as i32,
            ),
            (Value::Null, Value::Null)
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 「本方案直接辅助码」落盘同「启用辅助码」：未勾 null（回落全局），勾上写布尔。
    #[test]
    fn aux_direct_unchecked_writes_null_checked_writes_value() {
        assert_eq!(aux_direct_write(false, true), Value::Null);
        assert_eq!(aux_direct_write(true, false), json!(false));
        assert_eq!(aux_direct_write(true, true), json!(true));
    }

    /// 打开时的种子：双拼方案按覆盖值判勾选、控件取生效值；非双拼恒不勾（那一行不显示，
    /// 手写的 `direct` 不能计进摘要、也不能被复位清掉）。
    #[test]
    fn aux_direct_seed_is_shuangpin_only() {
        let followed_on = json!({ "followedBehavior": { "auxDirect": true } });
        assert_eq!(
            seed_aux_direct(&followed_on, true),
            (false, true),
            "跟随：控件取跟随值"
        );
        let over_off = json!({
            "followedBehavior": { "auxDirect": true },
            "engine": { "aux_code": { "direct": false } }
        });
        assert_eq!(
            seed_aux_direct(&over_off, true),
            (true, false),
            "双拼覆盖：勾上、取覆盖值"
        );
        assert_eq!(
            seed_aux_direct(&over_off, false),
            (false, true),
            "非双拼：手写的覆盖不认，恒不勾"
        );
        assert_eq!(
            seed_aux_direct(&json!({}), true),
            (false, false),
            "老核心无旁路字段 ⇒ 关"
        );
    }

    /// 「全部恢复跟随」也清直接辅助码一行：勾选清空、控件拨回跟随值、落盘回落 null。
    #[test]
    fn aux_reset_all_also_clears_direct_row() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let state = SchemaManagerState::new();
        state.settings_aux_direct_over.set(true);
        state.settings_aux_direct.set(false);

        aux_reset_all(&state, true, 4, true);

        assert!(!state.settings_aux_direct_over.get());
        assert!(state.settings_aux_direct.get(), "直接辅助码拨回跟随值");
        assert_eq!(
            aux_direct_write(
                state.settings_aux_direct_over.get(),
                state.settings_aux_direct.get()
            ),
            Value::Null
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 全局 manifest 该项 min/max 是 0..=20；手改 `config.toml` 能写出这个范围，
    /// 两处读取都要钳，不能把界外的数字当成生效值喂给 stepper 或原样显示在文案里。
    #[test]
    fn aux_len_out_of_manifest_range_is_clamped() {
        assert_eq!(clamp_aux_len(-3), 0, "负数钳到下界");
        assert_eq!(clamp_aux_len(99), 20, "越界钳到上界");
        assert_eq!(clamp_aux_len(7), 7, "范围内原样通过");
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
    fn sections_split_into_tabs_with_dividers_reinserted() {
        use SettingsSection::*;
        // 扁平表按真实构造顺序（双拼方案：Header, EntryKeys, |, ShuangpinLayout,
        // |, CodeTable, |, Overlay, |, ExtensionDicts）
        let flat = [
            Header,
            EntryKeys,
            Divider,
            ShuangpinLayout,
            Divider,
            CodeTable,
            Divider,
            Overlay,
            Divider,
            ExtensionDicts,
        ];
        let [basic, keys, custom] = split_sections_by_tab(&flat);
        assert_eq!(
            basic,
            vec![Header, ShuangpinLayout, Divider, ExtensionDicts],
            "双拼布局归基本配置；Header 自带结尾分隔故其后不插"
        );
        assert_eq!(keys, vec![EntryKeys], "按键设置独立成页，只此一节");
        assert_eq!(
            custom,
            vec![CodeTable, Divider, Overlay],
            "方案自定义页开头不该是一条分隔线"
        );
    }

    /// ★ 星号打在**「方案自定义」**那一页上。
    ///
    /// TAB 数量已经反转过两次（三 → 二 → 三），而写死的下标在其中一次反转里**正好还指对**
    /// ——那种错法不会被任何断言抓到，直到下一次反转才暴露成「星号打在按键设置上」。
    #[test]
    fn custom_tab_index_points_at_the_custom_tab() {
        assert_eq!(SETTINGS_TABS[CUSTOM_TAB], "方案自定义");
        assert_eq!(SettingsSection::Behavior.tab(), Some(CUSTOM_TAB));
        assert_eq!(SettingsSection::CodeTable.tab(), Some(CUSTOM_TAB));
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
        // 遍历 `SettingsSection::ALL`：列表挂在 enum 定义旁边，新增变体时看得见。
        // ⚠️ 它仍是手写的——之前这份列表就写在这里，`Behavior` 节加进来时漏掉了整整一轮。
        for &kind in SettingsSection::ALL {
            let _ = build_section(kind, &state);
        }
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// Review Focus 3：下拉首项的跟随文案必须来自**本次打开**对话框时的
    /// `schema.getConfig`，不能停在上一次打开时种下的标签上——否则用户去全局页把
    /// 候选排列改了，方案设置页开两次都还显示旧值。
    ///
    /// mock 的 `followedBehavior.layout` 默认恒为 `"horizontal"`（离线截图不需要它变），
    /// 故用 [`crate::rpc::set_mock_followed_layout`] 在两次 `open_settings_at_id` 之间
    /// 显式换一个值，模拟「全局页配置在两次打开之间变了」。
    #[test]
    fn follow_labels_reseeded_on_each_open() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        crate::rpc::set_mock_followed_layout(Some("horizontal"));

        let state = SchemaManagerState::new();
        *state.schemas.borrow_mut() = vec![SchemaEntry {
            id: "wubi86".into(),
            name: "五笔 86".into(),
            engine_type: "codetable".into(),
            ..Default::default()
        }];

        state.open_settings_at_id("wubi86");
        assert_eq!(
            state.settings_layout_follow_labels.get()[0],
            follow_option_label("全局", "横排"),
            "第一次打开：跟随值是横排"
        );

        crate::rpc::set_mock_followed_layout(Some("vertical"));
        state.open_settings_at_id("wubi86");
        assert_eq!(
            state.settings_layout_follow_labels.get()[0],
            follow_option_label("全局", "竖排"),
            "第二次打开要按新的跟随值重算，不能停在第一次种下的标签上"
        );

        crate::rpc::set_mock_followed_layout(None);
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    // ── 辅助码来源（`[engine.aux_code].files`）──

    /// 测试用可选项：一个码表方案、两张码表。形状对齐 core `schema.auxCodeSources`。
    fn aux_opts() -> AuxSourceOpts {
        parse_aux_source_opts(&json!({
            "schemas": [{ "id": "wubi86", "name": "五笔 86" }],
            "files": [{ "path": "aux_code/stroke.txt", "label": "笔画" },
                      { "path": "aux_code/flypy_full.txt", "label": "小鹤" }]
        }))
    }
    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// 下拉顺序是「方案在前、码表在后」，`entry` 即写进 `files` 的原文（方案带 `schema:` 前缀）。
    #[test]
    fn aux_source_items_put_schemas_first() {
        let o = aux_opts();
        let e: Vec<&str> = o.items.iter().map(|i| i.entry.as_str()).collect();
        assert_eq!(
            e,
            vec![
                "schema:wubi86",
                "aux_code/stroke.txt",
                "aux_code/flypy_full.txt"
            ]
        );
        assert!(o.items[0].is_schema && !o.items[1].is_schema);
        assert_eq!(o.label_of("schema:wubi86"), Some("五笔 86"));
    }

    /// 跟随档括号里的文字：按方案文件声明的来源取名，多个用「+」连，没装的标出来。
    #[test]
    fn follow_text_names_base_sources() {
        let o = aux_opts();
        assert_eq!(
            aux_src_follow_text(&strs(&["aux_code/stroke.txt"]), &o),
            "笔画"
        );
        assert_eq!(
            aux_src_follow_text(&strs(&["schema:wubi86", "aux_code/flypy_full.txt"]), &o),
            "五笔 86+小鹤"
        );
        assert_eq!(
            aux_src_follow_text(&strs(&["aux_code/gone.txt"]), &o),
            "aux_code/gone.txt（未安装）"
        );
        assert_eq!(aux_src_follow_text(&[], &o), "无");
        // 可选项拿不到（RPC 失败）时退回原文，不崩、不报未安装。
        assert_eq!(
            aux_src_follow_text(&strs(&["aux_code/stroke.txt"]), &AuxSourceOpts::default()),
            "aux_code/stroke.txt"
        );
    }

    /// 打开时三种状态的判定：合并值等于方案文件自己的值 ⇒ 跟随；单条且下拉里有 ⇒ 选中；
    /// 多条或下拉里没有 ⇒ 只读（UI 表达不了，保存原样不动）。
    #[test]
    fn seed_distinguishes_follow_pick_manual() {
        let o = aux_opts();
        let base = strs(&["aux_code/stroke.txt"]);
        assert!(
            matches!(seed_aux_src(&base, &base, &o), AuxSrcSeed::Follow),
            "合并值 = 基线 ⇒ 跟随"
        );
        assert!(matches!(
            seed_aux_src(&strs(&["schema:wubi86"]), &base, &o),
            AuxSrcSeed::Pick(0)
        ));
        assert!(
            matches!(
                seed_aux_src(&strs(&["aux_code/flypy_full.txt"]), &[], &o),
                AuxSrcSeed::Pick(2)
            ),
            "方案没写、用户配了"
        );
        assert!(
            matches!(
                seed_aux_src(&strs(&["schema:wubi86", "aux_code/stroke.txt"]), &base, &o),
                AuxSrcSeed::Manual(_)
            ),
            "多条 ⇒ 只读"
        );
        assert!(
            matches!(
                seed_aux_src(&strs(&["schema:nope"]), &base, &o),
                AuxSrcSeed::Manual(_)
            ),
            "下拉里没有 ⇒ 只读"
        );
    }

    /// ★ 不勾写 `null`（core 跳过 null ⇒ 删键 ⇒ 回落方案文件），不是写一份基线的拷贝——
    /// 后者会把「跟随」钉死成当下的值，方案作者以后改了来源，这个用户也跟不上。
    #[test]
    fn aux_src_write_null_when_following() {
        assert_eq!(aux_src_write(false, Some("schema:wubi86")), Value::Null);
        assert_eq!(
            aux_src_write(true, Some("schema:wubi86")),
            json!(["schema:wubi86"])
        );
    }

    /// 「全部恢复跟随」连来源这一行一起清，落盘回到 null；多条的只读态也被清掉。
    #[test]
    fn aux_reset_all_also_clears_source_row() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let state = SchemaManagerState::new();
        state.settings_aux_src_over.set(true);
        state.settings_aux_src_manual.set(true);
        state.settings_aux_src_idx.set(1);

        aux_reset_all(&state, true, 4, false);

        assert!(!state.settings_aux_src_over.get());
        assert!(!state.settings_aux_src_manual.get());
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 测试用：mock 下打开拼音族方案（`auxCodeBaseFiles` 与合并后的 `files` 相同 ⇒ 跟随）。
    fn open_mock_pinyin() -> SchemaManagerState {
        let state = SchemaManagerState::new();
        *state.schemas.borrow_mut() = vec![SchemaEntry {
            id: "pinyin_simp".into(),
            name: "全拼·简".into(),
            engine_type: "pinyin".into(),
            scheme: "full".into(),
            ..Default::default()
        }];
        state.open_settings_at_id("pinyin_simp");
        state
    }

    /// 测试用：mock 下打开双拼方案（mock 的 getConfig 对拼音族一律给带辅助码表的配置）。
    fn open_mock_shuangpin() -> SchemaManagerState {
        let state = SchemaManagerState::new();
        *state.schemas.borrow_mut() = vec![SchemaEntry {
            id: "double_pinyin".into(),
            name: "双拼".into(),
            engine_type: "pinyin".into(),
            scheme: "shuangpin".into(),
            ..Default::default()
        }];
        state.open_settings_at_id("double_pinyin");
        state
    }

    /// 方案级注释模板与全局同一套转义：输入框显示 `\n` 两个字符，方案文件里是真换行。
    /// 打开不改不进待提交队列（含 `\\`、`\t`、不成对 `\x`），三态（未勾 / 空串）不塌。
    #[test]
    fn comment_tpl_escaped_in_box_and_raw_in_cfg() {
        use crate::dialogs::tooltip_sections_dialog::escape_template;
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        // 打开时方案里已有带真换行 / 反斜杠的模板：显示转义、原样不改就不脏。
        let raw_v = "${pinyin}\n${chaizi}";
        let raw_h = "a\\b\tc\\x";
        let state = open_mock_pinyin();
        let seed = seed_behavior(&json!({ "candidate": {
            "comment_template_vertical": raw_v,
            "comment_template_horizontal": raw_h,
        } }));
        state.seed_comment_tpl(&seed);
        {
            let mut at = state.settings_bh_at_open.borrow_mut();
            at.comment_v = seed.comment_v.clone();
            at.comment_h = seed.comment_h.clone();
        }
        assert_eq!(state.settings_bh_comment_v.get(), "${pinyin}\\n${chaizi}");
        assert_eq!(state.settings_bh_comment_h.get(), escape_template(raw_h));
        assert_eq!(state.behavior_now(), *state.settings_bh_at_open.borrow());
        state.stage_settings_edits();
        assert!(
            !state.has_pending_edits(),
            "什么都没改，不该有待提交的方案改动"
        );

        // 手打 `\n` ⇒ 方案 cfg 里写真换行；勾上留空仍是空串（不被转义逻辑折成别的）。
        let state = open_mock_pinyin();
        state.settings_bh_comment_v_on.set(true);
        state
            .settings_bh_comment_v
            .set("${pinyin}\\n${chaizi}".into());
        state.settings_bh_comment_h_on.set(true);
        state.stage_settings_edits();
        let cfg = state.pending_edits.borrow()["pinyin_simp"]
            .cfg
            .clone()
            .unwrap();
        assert_eq!(cfg["candidate"]["comment_template_vertical"], json!(raw_v));
        assert_eq!(cfg["candidate"]["comment_template_horizontal"], json!(""));

        // 未设置的方案：播种后两框为空、未勾。
        let state = open_mock_pinyin();
        state.seed_comment_tpl(&BehaviorSeed::default());
        assert!(!state.settings_bh_comment_v_on.get());
        assert_eq!(state.settings_bh_comment_v.get(), "");
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 双拼方案：直接辅助码一行显示（`settings_is_shuangpin` 即渲染判据）、跟随全局值；
    /// 打开即关不脏。勾上改值 ⇒ 写布尔、计入「已自定义 N 项」；再取消勾选 ⇒ 写 null。
    #[test]
    fn aux_direct_row_on_shuangpin_follows_then_writes_tristate() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let state = open_mock_shuangpin();
        assert!(
            state.settings_is_shuangpin.get(),
            "双拼方案显示直接辅助码一行"
        );
        assert!(
            !state.settings_aux_direct_over.get(),
            "方案未写 direct ⇒ 跟随"
        );
        assert!(
            state.settings_aux_direct.get(),
            "控件取跟随值（mock auxDirect = 开）"
        );
        assert_eq!(state.settings_aux_disp.get(), ct_summary(0));
        state.stage_settings_edits();
        assert!(
            !state.has_pending_edits(),
            "什么都没改，不该有待提交的方案改动"
        );

        let state = open_mock_shuangpin();
        state.settings_aux_direct_over.set(true);
        state.settings_aux_direct.set(false);
        state.stage_settings_edits();
        let cfg = state.pending_edits.borrow()["double_pinyin"]
            .cfg
            .clone()
            .unwrap();
        assert_eq!(cfg["engine"]["aux_code"]["direct"], json!(false));

        // 模拟「打开时已覆盖」：基线换成勾选态，再取消勾选 ⇒ 这一次落 null。
        let state = open_mock_shuangpin();
        state.settings_aux_direct_at_open.set((true, false));
        state.stage_settings_edits();
        let cfg = state.pending_edits.borrow()["double_pinyin"]
            .cfg
            .clone()
            .unwrap();
        assert_eq!(cfg["engine"]["aux_code"]["direct"], Value::Null);
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 全拼方案：直接辅助码一行不显示、不勾；保存不写 `direct`（连 null 都不写）。
    #[test]
    fn aux_direct_row_hidden_on_full_pinyin_and_never_written() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let state = open_mock_pinyin();
        assert!(
            !state.settings_is_shuangpin.get(),
            "全拼方案不显示直接辅助码一行"
        );
        assert!(!state.settings_aux_direct_over.get());
        // 改动别的行让 cfg 进待提交队列，看 `direct` 有没有被顺手写进去。
        state.settings_aux_on_over.set(true);
        state.stage_settings_edits();
        let cfg = state.pending_edits.borrow()["pinyin_simp"]
            .cfg
            .clone()
            .unwrap();
        assert!(
            cfg.pointer("/engine/aux_code/direct").is_none(),
            "全拼方案不该出现 direct 键：{cfg}"
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// ★ 打开即关不脏：默认跟随时来源这一行不往 cfg 里写任何东西（尤其不写 null——
    /// 那虽然落盘结果一样，但会让整份 cfg 进待提交队列，主保存按钮平白亮起）。
    #[test]
    fn aux_src_follow_seeded_from_mock_and_untouched_save_is_clean() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let state = open_mock_pinyin();
        assert!(!state.settings_aux_src_over.get(), "合并值 = 方案值 ⇒ 跟随");
        assert!(!state.settings_aux_src_manual.get());
        assert_eq!(state.settings_aux_src_follow.get(), "笔画");
        assert_eq!(
            state.settings_aux_src_opts.borrow().items.len(),
            5,
            "可选项取自 schema.auxCodeSources"
        );
        assert_eq!(state.settings_aux_disp.get(), ct_summary(0));

        state.stage_settings_edits();
        assert!(
            !state.has_pending_edits(),
            "什么都没改，不该有待提交的方案改动"
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 勾上并选一项 ⇒ `files` 写成单元素数组；再取消勾选 ⇒ 写 null（回落方案文件）。
    #[test]
    fn aux_src_pick_writes_single_entry_and_unpick_writes_null() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let state = open_mock_pinyin();
        let flypy = state
            .settings_aux_src_opts
            .borrow()
            .index_of("aux_code/flypy_full.txt")
            .unwrap();
        state.settings_aux_src_over.set(true);
        state.settings_aux_src_idx.set(flypy);
        state.stage_settings_edits();
        let cfg = state.pending_edits.borrow()["pinyin_simp"]
            .cfg
            .clone()
            .unwrap();
        assert_eq!(
            cfg["engine"]["aux_code"]["files"],
            json!(["aux_code/flypy_full.txt"])
        );

        // 模拟「打开时已覆盖」：基线换成勾选态，再取消勾选 ⇒ 这一次落 null。
        let state = open_mock_pinyin();
        state.settings_aux_src_at_open.set((true, flypy, false));
        state.stage_settings_edits();
        let cfg = state.pending_edits.borrow()["pinyin_simp"]
            .cfg
            .clone()
            .unwrap();
        assert_eq!(cfg["engine"]["aux_code"]["files"], Value::Null);
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 跟随态的下拉下标 seed 成**当前生效**的那一项：用户勾上时下拉停在它上面，
    /// 而不是悄悄换成第一项（mock 里第一项是「五笔 86」，生效的是「笔画」码表）。
    #[test]
    fn aux_src_follow_idx_points_at_effective_source() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let state = open_mock_pinyin();
        let bihua = state
            .settings_aux_src_opts
            .borrow()
            .index_of("aux_code/bihua.txt")
            .unwrap();
        assert_ne!(bihua, 0, "夹具要让生效项不在首位，否则测不出差别");
        assert!(!state.settings_aux_src_over.get());
        assert_eq!(state.settings_aux_src_idx.get(), bihua);

        // 直接勾上就存：落的是原来生效的那一张，不是首项。
        state.settings_aux_src_over.set(true);
        state.stage_settings_edits();
        let cfg = state.pending_edits.borrow()["pinyin_simp"]
            .cfg
            .clone()
            .unwrap();
        assert_eq!(
            cfg["engine"]["aux_code"]["files"],
            json!(["aux_code/bihua.txt"])
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 「全部恢复跟随」把下拉下标也拨回生效项——同 `aux_reset_all` 对另两行「控件值拨回
    /// 跟随值」的理由：不拨的话再勾上，下拉停在上一次选的旧项上。
    #[test]
    fn aux_reset_all_restores_source_idx_to_effective() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let state = open_mock_pinyin();
        let bihua = state.settings_aux_src_idx.get();
        state.settings_aux_src_over.set(true);
        state.settings_aux_src_idx.set(0);
        assert_ne!(bihua, 0);

        aux_reset_all(&state, true, 4, false);
        assert_eq!(state.settings_aux_src_idx.get(), bihua);
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 只读态文案：说的是**现状**（手写在方案覆盖里的来源），不指路去方案文件——
    /// 那份值在 override 里，改方案文件无效。单条（未安装）不说「多个」。
    #[test]
    fn manual_text_describes_handwritten_sources() {
        let o = aux_opts();
        assert_eq!(
            aux_src_manual_text(&strs(&["schema:wubi86", "aux_code/stroke.txt"]), &o),
            "手写的多个来源：五笔 86+笔画"
        );
        assert_eq!(
            aux_src_manual_text(&strs(&["aux_code/gone.txt"]), &o),
            "手写的来源：aux_code/gone.txt（未安装）"
        );
    }

    /// 只读态只由「勾选 + 只读」一起构成：取消勾选即离开只读，重新勾上又回到只读。
    #[test]
    fn aux_src_locked_needs_both_over_and_manual() {
        assert!(aux_src_locked(true, true));
        assert!(!aux_src_locked(false, true), "取消勾选 ⇒ 不再只读");
        assert!(!aux_src_locked(true, false));
        assert!(!aux_src_locked(false, false));
    }

    /// ★ 只读态下取消勾选不能「看着生效、存了没用」：保存要落 null（回落方案文件），
    /// 与界面上已经显示的「跟随方案」一致。
    #[test]
    fn aux_src_manual_untick_writes_null() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let state = open_mock_pinyin();
        state.settings_aux_src_over.set(true);
        state.settings_aux_src_manual.set(true);
        state.settings_aux_src_at_open.set((true, 0, true));

        state.settings_aux_src_over.set(false);
        state.stage_settings_edits();
        let cfg = state.pending_edits.borrow()["pinyin_simp"]
            .cfg
            .clone()
            .unwrap();
        assert_eq!(cfg["engine"]["aux_code"]["files"], Value::Null);
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 只读态（多条 / 下拉里没有）保存原样不动；被「全部恢复跟随」清掉后才写 null。
    #[test]
    fn aux_src_manual_is_left_alone_until_reset() {
        let _guard = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        // 模拟打开时 seed 出只读态（mock 的 files 是单条已知项，造不出来，直接设信号）。
        let state = open_mock_pinyin();
        state.settings_aux_src_over.set(true);
        state.settings_aux_src_manual.set(true);
        state.settings_aux_src_at_open.set((true, 0, true));
        state.stage_settings_edits();
        assert!(!state.has_pending_edits(), "只读态不动 files");

        aux_reset_all(&state, true, 4, false);
        state.stage_settings_edits();
        let cfg = state.pending_edits.borrow()["pinyin_simp"]
            .cfg
            .clone()
            .unwrap();
        assert_eq!(cfg["engine"]["aux_code"]["files"], Value::Null);
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

    /// ★★ 构造期 RPC 失败（内存表是兜底空表）时，writer 必须**跳过**这个键。
    ///
    /// 不跳的话：用户点一次「应用设置」→ `keys.key_actions` 被整表写成 `{}` ⇒ 方案直达
    /// 热键 / 往返热键 / special / 五c 收编后的全部引导键一次清空；又因 `{}` 恰好等于出厂
    /// 默认，core 的 `set_user_value` 会判「等于默认」把整个键删掉，连痕迹都不留。
    #[test]
    fn key_actions_writer_skips_when_never_loaded() {
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

        // 复现构造期那次同步 RPC 失败：表是空的，且从未被标记为「读到过」。
        st.schema_mgr.key_actions.borrow_mut().clear();
        st.schema_mgr.key_actions_loaded.set(false);

        let items = st.debug_diff_after_writers();
        assert!(
            !items.iter().any(|it| it.key == "keys.key_actions"),
            "未加载就写回 ⇒ 用户全部按键绑定被清空：{items:?}"
        );
    }

    /// ★★ 反向守卫：**已加载**时的空表必须照写——那是「用户真的删光了」。
    ///
    /// 缺这一条，上面那道闸就会被顺手做成「空表一律不写」，于是用户永远删不掉最后一条
    /// 绑定——正是 `key_actions` 这一族反复栽的「清空意图被静默丢弃」。两个方向都要钉。
    #[test]
    fn key_actions_writer_writes_empty_table_once_loaded() {
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

        // 用户把最后一条绑定也删了，且这张表确实是从 core 读来的。
        st.schema_mgr.key_actions.borrow_mut().clear();
        st.schema_mgr.key_actions_loaded.set(true);

        let items = st.debug_diff_after_writers();
        let ka = items
            .iter()
            .find(|it| it.key == "keys.key_actions")
            .unwrap_or_else(|| panic!("已加载时的空表必须写回，否则删不掉：{items:?}"));
        assert_eq!(
            ka.value.as_object().map(|o| o.len()),
            Some(0),
            "写回的应当是空表本身"
        );
    }

    /// ★★★ 端到端：点「清除」后，改动必须经 **effect** 落到 `key_actions` 内存表。
    ///
    /// # 这条链此前是测试盲区
    ///
    /// `commit_hotkey_edit` 的**唯一**驱动者是 `build_schema_settings_dialog` 里的
    /// `hotkey_effect`（对话框有多条关闭路径，故用 effect 而非挂按钮）。而既有测试
    /// 全部**直接调** `commit_hotkey_edit`，绕过了 effect —— 于是「effect 到底有没有
    /// 驱动它」从来没有被任何东西守住，真机表现就是「清除后保存说没有变更，重开对话框
    /// 那个热键还在」。
    ///
    /// 本用例用 `NullTextEngine` 跑真实的 layout 循环（`layout_root` 里调
    /// `dispatch_reactive_updates`，那是 effect 唯一的驱动点），把整条链纳入测试。
    #[test]
    fn clearing_hotkey_reaches_key_actions_through_effect() {
        use super::SchemaManagerState;
        use windui::core::Tree;
        use windui::geometry::Size;

        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let st = SchemaManagerState::new();
        st.key_actions.borrow_mut().insert(
            "ctrl+shift+q".to_string(),
            "switch_schema:wubi86".to_string(),
        );
        *st.settings_schema_id.borrow_mut() = "wubi86".to_string();
        // 打开对话框时的播种。
        st.seed_hotkey_editor("ctrl+shift+q");

        // 建树 + 跑一帧：effect 以当前版本为基线（构建帧不触发，见 reactive_effect）。
        let mut tree = Tree::new();
        let id = super::build_schema_settings_dialog(&st).build(&mut tree);
        tree.root = Some(id);
        let mut te = windui::text::NullTextEngine;
        tree.layout_root(Size::new(800, 600), &mut te);

        // 模拟点「清除」：与 EntryKeys 节里那个按钮的闭包做同样的事。
        st.settings_hotkey_enabled.set(false);
        st.settings_hotkey_disp.set("未设置".to_string());
        st.settings_hotkey_raw.set(String::new());

        // 下一帧：dispatch_reactive_updates 应当驱动 hotkey_effect → commit_hotkey_edit。
        tree.layout_root(Size::new(800, 600), &mut te);

        assert!(
            !st.key_actions
                .borrow()
                .values()
                .any(|v| v == "switch_schema:wubi86"),
            "清除后 effect 没把删除落到内存表 ⇒ writer 写回的仍是旧值 ⇒ 保存报「没有变更」。\
             当前表: {:?}",
            st.key_actions.borrow()
        );
    }

    /// `apply_loaded` 必须能把构造期没读到的表补齐（此前它只刷新 enabled_order）。
    #[test]
    fn apply_config_refreshes_key_actions() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let st = AppState::empty();
        st.schema_mgr.key_actions.borrow_mut().clear();
        st.schema_mgr.key_actions_loaded.set(false);

        let cfg = Config::from_value(json!({
            "keys": { "key_actions": { "ctrl+shift+r": "switch_schema:wubi86" } }
        }));
        st.schema_mgr.apply_config(&cfg);

        assert!(
            st.schema_mgr.key_actions_loaded.get(),
            "补齐后必须标记为已加载，否则 writer 会一直跳过、用户改什么都存不下"
        );
        assert_eq!(
            st.schema_mgr
                .key_actions
                .borrow()
                .get("ctrl+shift+r")
                .map(String::as_str),
            Some("switch_schema:wubi86")
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
