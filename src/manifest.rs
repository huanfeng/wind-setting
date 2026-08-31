//! 清单引擎 — 消费 `system.manifest`（声明式设置真相源）按 key 构建控件。
//!
//! 设计：页面只声明"哪些 key、放哪个分区"，控件类型 / label / 选项 / 默认 / 范围
//! 全部从 manifest 取——key 永远与 core 一致，core 加字段自动带上。
//!
//! [`field`] 按 key 取清单项 → 用当前配置值播种一个 `Signal` → 返回设置行，
//! 同时把"写回器"和"取值器"登记进 [`crate::state::FieldRegistry`]：
//! 保存时跑所有写回器把 Signal 值写回 Config 再 diff；`enabled_when` 用取值器联动。

use std::collections::HashMap;

use serde_json::Value;
use windui::prelude::*;

use crate::config::Config;
use crate::dialogs::key_binding::ForeignBinding;
use crate::state::{AppState, SchemaInfo};
use crate::widgets::{setting_row, setting_row_checks, setting_row_schema_hint};

/// 单个清单项（来自 manifest.toml 经 system.manifest 暴露的 JSON）。
/// 部分字段（key/group/section/unit）暂未消费，保留以备分组/单位展示。
#[allow(dead_code)]
#[derive(Clone, Default)]
pub struct MItem {
    pub key: String,
    pub group: String,
    pub section: String,
    pub typ: String, // toggle / select / slider / number / string
    pub label: String,
    pub hint: Option<String>,
    /// 默认值已归 capability（core 权威，见 [`crate::capabilities::Capabilities::default_for`]）；
    /// 清单不再声明 default，此字段仅为 JSON 解析兼容保留。
    #[allow(dead_code)]
    pub default: Value,
    pub options: Vec<MOption>,
    pub options_source: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub unit: Option<String>,
    /// 控件显式宽度（select/dropdown 选项文本较长时加宽，覆盖默认 CTRL_W）。
    pub width: Option<f64>,
    pub enabled_when: Option<String>,
    /// dialog_button 专用：门控开关 key（该 toggle 关时按钮置灰不可点，除非 `gate_soft`）。
    pub gate_key: Option<String>,
    /// dialog_button 专用：只与 `gate_key` 合并排版、**按钮不随开关置灰**。
    /// 用于对话框内容不专属于该开关的情形——如「英文符号字符集」同时服务于英文标点智能符号，
    /// 「中文符号字符集」的对话框里还寄居着别的开关；置灰会让另一条路的用户够不着。
    pub gate_soft: bool,
    /// 寄居在某个 dialog_button 的对话框里（值为宿主 dialog_button 的 key），不独立成行。
    /// 与 `dialog_group`/`opens_dialog` 那套「扩展设置」不同：这是把一两个从属项塞进**已有的**
    /// 值编辑对话框，不额外挂按钮，避免为一个开关多开一层套娃。
    pub dialog_extra_for: Option<String>,
    /// 分组对话框宿主：本项**自身也是一个普通值控件**（number/toggle/…），但卡片上不显示
    /// 自己的控件行，而是与寄居它的 `dialog_extra_for` 从属项一起收进一个对话框，卡片上
    /// 只留一行「<dialog_title> …… [设置]」。此字段即该行的标签兼对话框标题。
    ///
    /// 与 `dialog_button` 宿主的区别：那个宿主自身是个按钮、值编辑器要专门写；这里宿主
    /// 在对话框里就按自己的 `type` 用 [`field`] 渲染成普通一行，读写零手工。用于一组
    /// 同类字段（如候选窗四个尺寸下限 + 最小行数）挤爆卡片、又都不常改的情形。
    pub dialog_title: Option<String>,
    /// 分组对话框宿主行的副标题（`dialog_title` 存在时才有意义）。宿主自己的 `hint`
    /// 是对话框里那一行的说明，讲的是单个字段；卡片行需要另一句概括整组。
    pub dialog_hint: Option<String>,
    /// `dialog_button_key_action` 专用：这一项对应全局 `keys.key_actions` 里的哪个**动词**
    /// （`temp_pinyin` / `mix:quick_mix` …）。
    ///
    /// 五c 收编后，引导键的唯一落点是 `keys.key_actions`；`item.key`（如
    /// `input.temp_pinyin.trigger_keys`）退化为**标识符**，只用于 capability 校验与
    /// 清单定位，不再是写入路径。`dialog_button_mix_trigger` 早就是这个形态
    /// （key 指 `schema.mix_modes`，实际改的是数组里某项的字段）。
    pub verb: Option<String>,
    /// hotkey 专用：该功能是否支持「全局热键」（编辑对话框显示「全局」勾选）。
    pub global: bool,
    /// 子对话框分组：成员项标记同一 id，收进对话框渲染、不在分区内独立成行。
    pub dialog_group: Option<String>,
    /// 子对话框开启项：宿主控件旁挂「扩展设置」按钮，打开 dialog_group==此 id 的成员对话框。
    pub opens_dialog: Option<String>,
    /// 对话框内子分区（如"词频调整"/"自动造词"），同值的项聚合在同一子标题下。
    pub subsection: Option<String>,
    /// 隐藏此项（未完全实现 / 不对最终用户开放 / 当前平台无落点），不渲染也不注册
    /// writer/loader。清单里的 `platform` 字段在解析时折进本字段（见 [`platform_matches`]）。
    pub hidden: bool,
}

/// select 的一个选项（value 写回配置，label 展示）。
#[derive(Clone)]
pub struct MOption {
    pub value: String,
    pub label: String,
    /// 该项的补充说明。条目开关式对话框（`dialog_button_mix_members`）渲染成行副标题；
    /// 网格式控件忽略它。
    pub hint: String,
}

/// 解析 `[{value, label, hint}]` 选项数组；非数组/缺 value 的项跳过，label 缺省取 value。
fn parse_options(v: Option<&Value>) -> Vec<MOption> {
    v.and_then(|x| x.as_array())
        .map(|opts| {
            opts.iter()
                .filter_map(|o| {
                    let value = o.get("value").and_then(|x| x.as_str())?.to_string();
                    let label = o
                        .get("label")
                        .and_then(|x| x.as_str())
                        .unwrap_or(&value)
                        .to_string();
                    let hint = o
                        .get("hint")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string();
                    Some(MOption { value, label, hint })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 一个分组（→ Tab）。来自 manifest 的 `groups`，按 order 排序。
/// 当前 Tab 集为「manifest 分组 + 自定义页」的精选混排（标签亦本地化，如 hotkeys→按键），
/// 故暂不直接消费 groups 派生 Tab；保留完整解析以备后续完全清单驱动。
#[allow(dead_code)]
#[derive(Clone, Default)]
pub struct MGroup {
    pub key: String,
    pub label: String,
    pub order: i64,
}

/// 清单：key → 项（含 manifest 原始顺序与分组，便于 (group, section) 聚合渲染）。
#[derive(Default)]
pub struct Manifest {
    pub items: HashMap<String, MItem>,
    /// 项 key 的 manifest 出现顺序（HashMap 无序，渲染需按此还原）。
    pub order: Vec<String>,
    /// 分组列表（已按 order 升序）。保留备用，见 [`MGroup`] 说明。
    #[allow(dead_code)]
    pub groups: Vec<MGroup>,
}

impl Manifest {
    /// 内嵌设置清单源文本（随二进制发布，core 退役 manifest.toml 后由 setting 维护）。
    const EMBEDDED: &'static str = include_str!("assets/settings_manifest.toml");

    /// 从 system.manifest 返回的 JSON 解析。容错：缺字段回退默认。
    pub fn from_value(v: &Value) -> Self {
        let mut items = HashMap::new();
        let mut order = Vec::new();
        let mut groups: Vec<MGroup> = v
            .get("groups")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|g| {
                        let key = g.get("key").and_then(|x| x.as_str())?.to_string();
                        Some(MGroup {
                            label: g
                                .get("label")
                                .and_then(|x| x.as_str())
                                .unwrap_or(&key)
                                .to_string(),
                            order: g.get("order").and_then(|x| x.as_i64()).unwrap_or(999),
                            key,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        groups.sort_by_key(|g| g.order);
        if let Some(arr) = v.get("items").and_then(|x| x.as_array()) {
            for it in arr {
                let key = it.get("key").and_then(|x| x.as_str()).unwrap_or("");
                if key.is_empty() {
                    continue;
                }
                let options = parse_options(it.get("options"));
                let item = MItem {
                    key: key.to_string(),
                    group: str_field(it, "group"),
                    section: str_field(it, "section"),
                    typ: str_field(it, "type"),
                    label: str_field(it, "label"),
                    hint: opt_str(it, "hint"),
                    default: it.get("default").cloned().unwrap_or(Value::Null),
                    options,
                    options_source: opt_str(it, "options_source"),
                    min: it.get("min").and_then(|x| x.as_f64()),
                    max: it.get("max").and_then(|x| x.as_f64()),
                    step: it.get("step").and_then(|x| x.as_f64()),
                    unit: opt_str(it, "unit"),
                    width: it.get("width").and_then(|x| x.as_f64()),
                    enabled_when: opt_str(it, "enabled_when"),
                    gate_key: opt_str(it, "gate_key"),
                    gate_soft: it
                        .get("gate_soft")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false),
                    dialog_extra_for: opt_str(it, "dialog_extra_for"),
                    dialog_title: opt_str(it, "dialog_title"),
                    dialog_hint: opt_str(it, "dialog_hint"),
                    verb: opt_str(it, "verb"),
                    global: it.get("global").and_then(|x| x.as_bool()).unwrap_or(false),
                    dialog_group: opt_str(it, "dialog_group"),
                    opens_dialog: opt_str(it, "opens_dialog"),
                    subsection: opt_str(it, "subsection"),
                    // platform 不匹配当前平台 = 隐藏。折进 hidden 而不是新开一条渲染判据：
                    // 「该项在本平台不存在」与「该项不对用户开放」的后果完全一致——不渲染、
                    // 不注册 writer/loader，配置文件里的原值原样留着不动。
                    hidden: it.get("hidden").and_then(|x| x.as_bool()).unwrap_or(false)
                        || !platform_matches(opt_str(it, "platform").as_deref()),
                };
                items.insert(key.to_string(), item);
                order.push(key.to_string());
            }
        }
        Self {
            items,
            order,
            groups,
        }
    }

    pub fn get(&self, key: &str) -> Option<&MItem> {
        self.items.get(key)
    }

    /// 返回以 `gate_key` 为门控开关的 dialog_button 项（用于「开关 + 设置」合并成一行）。
    /// 按 manifest 顺序取第一个匹配。
    pub fn dialog_for_gate(&self, gate_key: &str) -> Option<&MItem> {
        self.order
            .iter()
            .filter_map(|k| self.items.get(k))
            .find(|it| it.typ == "dialog_button" && it.gate_key.as_deref() == Some(gate_key))
    }

    /// 从内嵌 TOML 清单构建。TOML 先反序列化为 serde_json::Value（与 system.manifest
    /// 旧 JSON 同形：groups + 扁平 items），复用 [`Manifest::from_value`]。
    pub fn from_embedded() -> Self {
        match toml::from_str::<Value>(Self::EMBEDDED) {
            Ok(v) => Self::from_value(&v),
            Err(e) => {
                tracing::error!("内嵌 settings_manifest.toml 解析失败: {e}");
                Self::default()
            }
        }
    }

    /// 取某分组下、按 manifest 顺序聚合的 (分区名, 该分区项 key 列表)。
    /// 同一 section 的项归到一张卡片；section 首次出现的次序即卡片次序。
    pub fn sections_of(&self, group: &str) -> Vec<(String, Vec<String>)> {
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        for key in &self.order {
            let Some(it) = self.items.get(key) else {
                continue;
            };
            if it.group != group {
                continue;
            }
            match out.iter_mut().find(|(s, _)| *s == it.section) {
                Some((_, keys)) => keys.push(key.clone()),
                None => out.push((it.section.clone(), vec![key.clone()])),
            }
        }
        out
    }
}

fn str_field(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn opt_str(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 清单项的 `platform` 字段是否匹配当前平台。
///
/// 取值为逗号分隔的平台名（`windows` / `macos`），命中其一即显示；缺省（`None`）= 全平台。
/// 用于「该配置项在本平台没有落点」的情形——如工具栏自动隐藏（macOS 用菜单栏指示器，
/// 没有浮动工具栏可隐藏）。**不要**用它藏「本平台还没做」的功能：那属于待办，藏起来只会
/// 让人以为不支持。
///
/// 未知平台名一律不匹配：写错名字宁可整项消失（一眼看得见），也不要静默当成全平台显示。
fn platform_matches(platform: Option<&str>) -> bool {
    let Some(list) = platform else {
        return true;
    };
    list.split(',').map(str::trim).any(|p| match p {
        "windows" => cfg!(windows),
        "macos" => cfg!(target_os = "macos"),
        _ => false,
    })
}

/// 控件标准宽度（右侧对齐区）。
const CTRL_W: i32 = 150;

/// 构建一个 section 的全部设置行，处理「开关 + 设置」合并：
/// - 某 toggle 被某 dialog_button 用作 `gate_key` → 渲染为合并行（开关 + 设置…），
///   并跳过该 dialog_button 的独立行（其内容已并入合并行）。
/// - 其余项各自 [`field`] 独立成行。
///
/// 合并行渲染在 **gate toggle 的原始位置**（保持特性原有排序）。
pub fn section_rows(state: &AppState, keys: &[String]) -> Vec<Element> {
    let cfg = &state.current_config;
    let mut rows = Vec::with_capacity(keys.len());
    for k in keys {
        let Some(item) = state.manifest.get(k) else {
            rows.push(field(state, k));
            continue;
        };
        // 隐藏项：不渲染也不注册 writer/loader（功能未完成或不对用户开放）。
        if item.hidden {
            continue;
        }
        // 子对话框成员：不在分区内独立成行（由 opens_dialog 宿主的「扩展设置」对话框承载）。
        if item.dialog_group.is_some() {
            continue;
        }
        // 寄居项：渲染在宿主 dialog_button 的对话框里（见 `dialog_extra_for`），不独立成行。
        if item.dialog_extra_for.is_some() {
            continue;
        }
        // 被 gate 合并的 dialog_button：跳过独立行（gate 存在才跳，避免误吞）。
        if item.typ == "dialog_button"
            && item.opens_dialog.is_none()
            && let Some(g) = &item.gate_key
            && state.manifest.get(g).is_some()
        {
            continue;
        }
        let row = if item.dialog_title.is_some() {
            // 分组对话框宿主：卡片上收成一行「<dialog_title> …… [设置]」，宿主自身与寄居项
            // 都在对话框里（见 `dialog_title` 的说明）。
            build_grouped_dialog_row(state, k, item)
        } else if item.opens_dialog.is_some() {
            // 子对话框宿主：渲染 [宿主控件][扩展设置] 行，按钮打开成员对话框。
            build_dialog_group_row(state, k, item, cfg)
        } else if item.typ == "toggle"
            && let Some(dlg_item) = state.manifest.dialog_for_gate(k)
        {
            // 作为某 dialog_button 门控开关的 toggle：渲染合并行。
            build_gated_dialog_row(state, k, item, dlg_item, cfg)
        } else {
            field(state, k)
        };
        // enabled_when 引用同卡片内其它 key → 视觉从属行（仅缩进，不画连接线：
        // 多个子项连续时一排 └ 显得杂乱；缩进本身已传达从属关系，对齐 Win11/macOS 设置惯例）。
        let dependent = item
            .enabled_when
            .as_deref()
            .and_then(parse_conds)
            .map(|cs| {
                cs.iter()
                    .any(|c| c.key != *k && keys.iter().any(|x| x == &c.key))
            })
            .unwrap_or(false);
        rows.push(if dependent {
            Element::row()
                .width_match()
                .cross(Align::Stretch)
                .child(Element::leaf().width(24))
                .child(row.weight(1.0))
        } else {
            row
        });
    }
    rows
}

/// 按 key 构建一行设置项（label + 控件），并登记保存写回器与联动取值器。
/// 按 item.typ 构建控件本体（不含行包裹/标签/enabled_when）。field() 与子对话框宿主行共用。
/// `build_control` 能渲染出真控件的全部 `type` 取值。
///
/// 存在的理由是**清单声明的类型与渲染器认识的类型是两份知识**，而不同步的后果没有任何
/// 编译期或测试期信号：capability 校验只问「控件与配置类型兼不兼容」，答得上来照样全绿，
/// 界面上却是一行 `[类型名?]` 灰字占位符。2026-08-25 加 `dialog_button_ordered_pairs`
/// 时实测踩到，靠 `--screenshot` 才看见。
///
/// 两道断言把缺口封死（见 `renderable_control_types_*` 两条测试）：
/// 清单里的每个 type 都在本名单里；本名单里的每一项都在 `build_control` 的 match 里。
pub(crate) const RENDERABLE_CONTROL_TYPES: &[&str] = &[
    "toggle",
    "select",
    "select_mix_layout",
    "select_mix_free_input",
    "checkbox_mix_free_select_keys",
    "segmented",
    "checkbox_group",
    "number",
    "slider",
    "number_select",
    "string",
    "hotkey",
    "dialog_button",
    "dialog_button_key_action",
    "dialog_button_mix_members",
    "dialog_button_ordered_pairs",
    "dialog_button_comment_dicts",
];

fn build_control(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    match item.typ.as_str() {
        "toggle" => build_toggle(state, key, item, cfg),
        "select" => build_select(state, key, item, cfg),
        "select_mix_layout" => build_select_mix_layout(state, key, item, cfg),
        "select_mix_free_input" => build_select_mix_free_input(state, key, item, cfg),
        "checkbox_mix_free_select_keys" => build_checkbox_mix_free_select_keys(state, key, cfg),
        "segmented" => build_segmented(state, key, item, cfg),
        "checkbox_group" => build_checkbox_group(state, key, item, cfg),
        "number" | "slider" => build_number(state, key, item, cfg),
        "number_select" => build_number_select(state, key, item, cfg),
        "string" => build_string(state, key, item, cfg),
        "hotkey" => build_hotkey(state, key, item, cfg),
        "dialog_button" => build_dialog_button(state, key, item, cfg),
        // 三者都渲染成「设置…」按钮，差别只在 `build_dialog_body` 里按 typ 分派出的
        // 对话框主体。⚠️ **加新的 dialog_button_* 类型必须同时改这两处**：只加
        // build_dialog_body 的分支，capability 校验照样过、测试照样全绿，界面上却是
        // 下面那行 `[类型名?]` 占位符（2026-08-25 实测踩到，靠 --screenshot 才发现）。
        "dialog_button_key_action"
        | "dialog_button_mix_members"
        | "dialog_button_ordered_pairs"
        | "dialog_button_comment_dicts" => build_dialog_button(state, key, item, cfg),
        other => {
            // 名单说能渲染、match 却没有分支 ⇒ 名单与实现脱节，debug 构建当场炸。
            // release 下仍回落占位符（灰字比崩溃好），故不用 unreachable!。
            debug_assert!(
                !RENDERABLE_CONTROL_TYPES.contains(&other),
                "控件类型 {other:?} 在 RENDERABLE_CONTROL_TYPES 里，却没有 build_control 分支"
            );
            Element::label(format!("[{other}?]")).fg_role(Role::TextMuted)
        }
    }
}

/// 「可被方案覆盖」这条事实在一个容器里怎么呈现。见 [`schema_hint_mode`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaHintMode {
    /// 逐行挂 ⓘ 角标（默认）。可覆盖的是少数时，角标标的是**例外**，有信息量。
    PerRow,
    /// 容器顶部统一说明一次，行内不再挂角标；反过来给**不可**覆盖的那几项标「仅全局」。
    Header,
}

/// 判定一个容器该用哪种呈现。
///
/// ★ 判据是**这条事实在这个容器里有没有区分度**：可覆盖项占绝大多数时，逐行角标每行都
/// 有、等于每行都没有——用户报的「一个对话框十几个提示」就是这个。占少数时才反过来，
/// 角标标出的是例外，那才值得逐行挂。
///
/// 实测四个「全局方案配置」对话框的比例天差地别（码表 18/20、混输 0/9、拼音 2/14、
/// 英文 0/8），所以**不能按容器硬编码**，也不能一刀切——现算。
///
/// 阈值 2/3 且至少 5 项：只有 3 项的容器里标 2 个角标算不上噪音，切模式反而多一行说明。
/// 守门测试 `scheme_config_dialogs_pick_expected_hint_mode` 锁住当前四个对话框各落在哪一
/// 档——core 增删可覆盖项跨过阈值时它会红，那正是需要人看一眼的时候。
pub fn schema_hint_mode(state: &AppState, keys: &[String]) -> SchemaHintMode {
    let (mut total, mut yes) = (0usize, 0usize);
    for k in keys {
        let Some(item) = state.manifest.get(k) else {
            continue;
        };
        if item.hidden {
            continue;
        }
        total += 1;
        if schema_override_of(state, k).is_some() {
            yes += 1;
        }
    }
    if yes >= 5 && yes * 3 >= total * 2 {
        SchemaHintMode::Header
    } else {
        SchemaHintMode::PerRow
    }
}

/// 某项的「可被方案覆盖」声明（core `SCHEMA_OVERRIDES` 经 capability 快照下发）。
/// 静态事实 ⇒ 只查内存里的快照，不发 RPC、不读盘。
fn schema_override_of<'a>(
    state: &'a AppState,
    key: &str,
) -> Option<&'a crate::capabilities::SchemaOverrideHint> {
    state
        .capabilities
        .entries
        .get(key)
        .and_then(|e| e.schema_override.as_ref())
}

/// 清单缺失该 key 时返回占位行（便于发现 key 漂移）。
pub fn field(state: &AppState, key: &str) -> Element {
    field_with(state, key, SchemaHintMode::PerRow)
}

/// [`field`] 的带模式版本。`Header` 模式下行内不挂 ⓘ，改为给**不可**覆盖的项标「仅全局」。
pub fn field_with(state: &AppState, key: &str, mode: SchemaHintMode) -> Element {
    let Some(item) = state.manifest.get(key) else {
        return setting_row(
            key,
            Some("（清单缺失此项）"),
            Element::label("—").fg_role(Role::TextMuted),
        );
    };
    let cfg = &state.current_config;
    let control = build_control(state, key, item, cfg);
    // 「此项可被方案级配置覆盖」的角标。`Header` 模式下容器顶部已统一说过一次，行内不再挂。
    let overridable = schema_override_of(state, key);
    let schema_hint = match mode {
        SchemaHintMode::PerRow => overridable,
        SchemaHintMode::Header => None,
    };
    // ⚠️ checkbox_group 分支**没有**角标形态。当前没有可覆盖的键是这个类型（实测：
    // toggle 11 / number 7 / select 3 / dialog_button 2 / number_select 1 / segmented 1
    // / string 1），故不是缺口；真出现了要给 setting_row_checks 也补上，否则那一行
    // 静默失去提示——与本仓「控件类型两处分派漏一处」是同一个形状。
    debug_assert!(
        !(item.typ == "checkbox_group" && schema_hint.is_some()),
        "{key}: checkbox_group 行还没有「可被方案覆盖」角标形态"
    );
    // 多选键组用专用行（固定宽标签 + 占满网格），避免宽网格把组名挤掉。
    let mut row = if item.typ == "checkbox_group" {
        setting_row_checks(&item.label, item.hint.as_deref(), control)
    } else if let Some(h) = schema_hint {
        setting_row_schema_hint(&item.label, item.hint.as_deref(), &h.note, control)
    } else if mode == SchemaHintMode::Header && overridable.is_none() {
        // 顶部那句「本页可被方案覆盖」对这一项不成立 ⇒ 必须标出来，否则统一说明就是
        // 在传播错误信息。反向标记只在 Header 模式下出现，见 `setting_row_global_only`。
        crate::widgets::setting_row_global_only(&item.label, item.hint.as_deref(), control)
    } else {
        setting_row(&item.label, item.hint.as_deref(), control)
    };
    if let Some(f) = item
        .enabled_when
        .as_deref()
        .and_then(|e| enabled_when_closure(state, e))
    {
        // 联动用「置灰整行」而非隐藏：保留占位，避免分隔线残留与高度抖动。
        row = row.enabled_when(f);
    }
    row
}

/// 子对话框宿主行：[宿主控件][扩展设置] —— 按钮打开承载 dialog_group 成员的分组对话框。
/// 成员经各自 field() 登记 writer/loader/getter（与独立成行时一致），随主保存提交。
/// 宿主自身的 enabled_when 仍作用于整行（如 position_mode 随状态提示开关置灰）。
fn build_dialog_group_row(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    use crate::dialogs::field_dialogs::build_group_dialog;
    let group = item.opens_dialog.clone().unwrap_or_default();
    // 宿主控件本体（与 field() 共用 build_control，避免分派漂移）。
    let control = build_control(state, key, item, cfg);
    // 成员项（按 manifest 顺序；group id 约定全局唯一以免跨 section 误吸）→ field() 行 → 分组对话框。
    let member_keys: Vec<String> = state
        .manifest
        .order
        .iter()
        .filter(|k| {
            state
                .manifest
                .get(k)
                .and_then(|i| i.dialog_group.as_deref())
                == Some(group.as_str())
        })
        .cloned()
        .collect();
    let rows: Vec<Element> = member_keys.iter().map(|k| field(state, k)).collect();
    let visible = signal(false);
    let dlg = build_group_dialog(visible, &format!("{} · 扩展设置", item.label), rows, None);
    register_dialog(state, dlg);

    let btn = Element::button("扩展设置")
        .small()
        .outline_soft()
        .on_click(move |_| visible.set(true));
    let row_control = Element::row()
        .cross(Align::Center)
        .spacing(10)
        .child(control)
        .child(btn);
    let mut row = setting_row(&item.label, item.hint.as_deref(), row_control);
    // 宿主 enabled_when 作用于整行（含扩展按钮）。
    if let Some(f) = item
        .enabled_when
        .as_deref()
        .and_then(|e| enabled_when_closure(state, e))
    {
        row = row.enabled_when(f);
    }
    row
}

fn build_toggle(state: &AppState, key: &str, _item: &MItem, cfg: &Config) -> Element {
    Element::switch(register_toggle(state, key, cfg))
}

/// 为某 bool key 建 Signal 并登记 writer/getter/loader，返回该 Signal。
/// 独立出来便于「开关 + 设置」合并行直接复用同一开关（见 [`build_gated_dialog_row`]）。
fn register_toggle(state: &AppState, key: &str, cfg: &Config) -> Signal<bool> {
    let def = state
        .capabilities
        .default_for(key)
        .as_bool()
        .unwrap_or(false);
    let cur = cfg.get_bool(key).unwrap_or(def);
    let sig = signal(cur);
    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            c.set_path(&kw, Value::Bool(sig.get()))
        }));
    state
        .fields
        .getters
        .borrow_mut()
        .insert(key.to_string(), Box::new(move || Value::Bool(sig.get())));
    let kl = key.to_string();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| sig.set(c.get_bool(&kl).unwrap_or(def))),
    );
    sig
}

/// 求 `cur` 在 `values` 中的索引；不在其中时补一个承载它的选项并返回该索引。
///
/// 清单只能列举**已知**取值，而 config.toml 是用户可手写的——例如
/// `schema.codetable.z_key_action` 可写 `special:<id>`，实例 id 随已安装方案增删，
/// 静态清单无从列举。不补这一项的话 `position` 会回落到 0，而 writer 保存的是
/// **选中项的值**：用户手写的取值就在「打开设置页、点一次保存」之间被静默换掉，
/// 页面上还看不出任何异常。
fn select_index_for(values: &mut Vec<String>, labels: &mut Vec<String>, cur: &str) -> usize {
    if let Some(i) = values.iter().position(|v| v == cur) {
        return i;
    }
    // 空值本就表示「未设置」，清单没列空选项时回落首项即可（补一项空标签反而更糟）。
    if cur.is_empty() {
        return 0;
    }
    values.push(cur.to_string());
    labels.push(format!("{cur}（当前配置）"));
    values.len() - 1
}

fn build_select(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    // 响应式动态源（异步加载的主题/字体）：选项绑定 Signal，数据到达即填充下拉，
    // 避免静态构建在数据到达前拿到空列表显示「（无可选项）」。schemas 仍走下方静态路径。
    if let Some(src) = item.options_source.as_deref()
        && let Some((val_sig, lab_sig)) = reactive_option_signals(state, src)
    {
        return build_select_reactive(state, key, item, cfg, val_sig, lab_sig);
    }
    let (mut values, mut labels): (Vec<String>, Vec<String>) =
        if let Some(src) = &item.options_source {
            dynamic_options(state, src)
        } else {
            item.options
                .iter()
                .map(|o| (o.value.clone(), o.label.clone()))
                .unzip()
        };
    if values.is_empty() {
        return Element::label("（无可选项）").fg_role(Role::TextMuted);
    }
    let def_str = state
        .capabilities
        .default_for(key)
        .as_str()
        .unwrap_or("")
        .to_string();
    let cur = cfg
        .get_str(key)
        .map(str::to_string)
        .unwrap_or_else(|| def_str.clone());
    let idx = select_index_for(&mut values, &mut labels, &cur);
    let sig = signal(idx);
    let kw = key.to_string();
    let vals_w = values.clone();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            if let Some(v) = vals_w.get(sig.get()) {
                c.set_path(&kw, Value::String(v.clone()));
            }
        }));
    let vals_l = values.clone();
    let kl = key.to_string();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let cur = c
                .get_str(&kl)
                .map(str::to_string)
                .unwrap_or_else(|| def_str.clone());
            let idx = vals_l.iter().position(|v| *v == cur).unwrap_or(0);
            sig.set(idx);
        }),
    );
    let vals_g = values;
    state.fields.getters.borrow_mut().insert(
        key.to_string(),
        Box::new(move || {
            vals_g
                .get(sig.get())
                .map(|v| Value::String(v.clone()))
                .unwrap_or(Value::Null)
        }),
    );
    // 无显式 width：用 min_width 下界——短选项对齐 CTRL_W 基线，长选项按内容自适应加宽
    // 避免文本换行；显式 width 仍走固定宽（覆盖下界）。
    let dd = Element::dropdown(labels, sig);
    match item.width {
        Some(w) => dd.width(w as i32),
        None => dd.min_width(CTRL_W),
    }
}

/// 响应式动态选项信号对：(值列表信号, 标签列表信号)。
type OptionSignals = (Signal<Vec<String>>, Signal<Vec<String>>);

/// 响应式动态源 → (值列表信号, 标签列表信号)。仅主题/字体异步加载需要响应式；
/// 字体 value 为字体族名（空字符串=默认），label 首项为"默认"其余同值。
fn reactive_option_signals(state: &AppState, src: &str) -> Option<OptionSignals> {
    match src {
        "themes" => Some((state.theme_value_sig, state.theme_label_sig)),
        "fonts" => Some((state.font_sig, state.font_label_sig)),
        "codetableSchemas" => Some((
            state.codetable_schema_val_sig,
            state.codetable_schema_lab_sig,
        )),
        "pinyinSchemas" => Some((state.pinyin_schema_val_sig, state.pinyin_schema_lab_sig)),
        _ => None,
    }
}

/// 响应式 select：选项随 `lab_sig` 变更刷新；index↔value 映射读 `val_sig`（异步数据到达后就位）。
fn build_select_reactive(
    state: &AppState,
    key: &str,
    item: &MItem,
    cfg: &Config,
    val_sig: Signal<Vec<String>>,
    lab_sig: Signal<Vec<String>>,
) -> Element {
    let def_str = state
        .capabilities
        .default_for(key)
        .as_str()
        .unwrap_or("")
        .to_string();
    let cur = cfg
        .get_str(key)
        .map(str::to_string)
        .unwrap_or_else(|| def_str.clone());
    // 初始索引：在当前（可能为空）值列表中定位；空则 0，待 loader 在数据到达后重定位。
    let idx = val_sig.with(|v| v.iter().position(|x| *x == cur).unwrap_or(0));
    let sig = signal(idx);

    // writer：按当前索引取值；列表为空（未加载）时不写，避免清空配置。
    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            if let Some(v) = val_sig.with(|vs| vs.get(sig.get()).cloned()) {
                c.set_path(&kw, Value::String(v));
            }
        }));
    // loader：按配置值在（已就位的）值列表中重定位索引。
    let kl = key.to_string();
    let def_l = def_str.clone();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let cur = c
                .get_str(&kl)
                .map(str::to_string)
                .unwrap_or_else(|| def_l.clone());
            let i = val_sig.with(|v| v.iter().position(|x| *x == cur).unwrap_or(0));
            sig.set(i);
        }),
    );
    // getter（一致性）：返回当前选中值；列表未就位/越界时返回 Null（与静态 select 路径一致，
    // 避免潜在 enabled_when 对空值的判定分歧）。仅供联动读取，不代表 writer 写回值。
    state.fields.getters.borrow_mut().insert(
        key.to_string(),
        Box::new(move || {
            val_sig
                .with(|vs| vs.get(sig.get()).cloned())
                .map(Value::String)
                .unwrap_or(Value::Null)
        }),
    );

    // 主题下拉用富内容（版本徽章 + 外部主题删除图标）；其余动态源仍用纯文本响应式下拉。
    let el = if item.options_source.as_deref() == Some("themes") {
        Element::dropdown_items_signal(state.theme_items_sig, sig)
    } else {
        Element::dropdown_signal(lab_sig, sig)
    };
    match item.width {
        Some(w) => el.width(w as i32),
        None => el.min_width(CTRL_W),
    }
}

/// 离散数值下拉（int 键，如每页候选数）：options 的 value 为整数字符串，显示 label，写回整数。
fn build_number_select(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    let (values, labels): (Vec<String>, Vec<String>) = item
        .options
        .iter()
        .map(|o| (o.value.clone(), o.label.clone()))
        .unzip();
    if values.is_empty() {
        return Element::label("（无可选项）").fg_role(Role::TextMuted);
    }
    let def = state.capabilities.default_for(key).as_i64().unwrap_or(0);
    // 越界/缺失回落到「默认值对应索引」（而非首项），避免显示与保存偏离 capability 默认。
    let def_idx = values
        .iter()
        .position(|v| *v == def.to_string())
        .unwrap_or(0);
    let cur = cfg.get_f64(key).map(|f| f as i64).unwrap_or(def);
    let idx = values
        .iter()
        .position(|v| *v == cur.to_string())
        .unwrap_or(def_idx);
    let sig = signal(idx);

    // writer：取选中项的整数值写回。
    let kw = key.to_string();
    let vals_w = values.clone();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            if let Some(n) = vals_w.get(sig.get()).and_then(|v| v.parse::<i64>().ok()) {
                c.set_path(&kw, Value::from(n));
            }
        }));
    // loader：配置整数 → 在 values 中定位索引（越界回落默认值索引）。
    let kl = key.to_string();
    let vals_l = values.clone();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let cur = c.get_f64(&kl).map(|f| f as i64).unwrap_or(def);
            sig.set(
                vals_l
                    .iter()
                    .position(|v| *v == cur.to_string())
                    .unwrap_or(def_idx),
            );
        }),
    );
    // getter：当前选中整数值。
    let vals_g = values;
    state.fields.getters.borrow_mut().insert(
        key.to_string(),
        Box::new(move || {
            vals_g
                .get(sig.get())
                .and_then(|v| v.parse::<i64>().ok())
                .map(Value::from)
                .unwrap_or(Value::Null)
        }),
    );
    // 无显式 width：用 min_width 下界——短选项对齐 CTRL_W 基线，长选项按内容自适应加宽
    // 避免文本换行；显式 width 仍走固定宽（覆盖下界）。
    let dd = Element::dropdown(labels, sig);
    match item.width {
        Some(w) => dd.width(w as i32),
        None => dd.min_width(CTRL_W),
    }
}

fn build_number(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    let def = state.capabilities.default_for(key).as_f64().unwrap_or(0.0);
    let cur = cfg.get_f64(key).unwrap_or(def);
    let sig = signal(cur);
    let (min, max, step) = (
        item.min.unwrap_or(0.0),
        item.max.unwrap_or(100.0),
        item.step.unwrap_or(1.0),
    );
    // 写回该编码成 serde_json 的 Int 变体还是 Float 变体，得看 capability 登记的字段类型，
    // 不能猜——core 的 int 型字段严格校验，必须收整数；但 float 型字段（如 half_life、
    // font_size）取整数值时若也按整数写回，会跟 config.get 返回的 Float 变体在 Value
    // 层面判不相等，diff_config 就会把它当成一条虚假变更。
    let is_int = state
        .capabilities
        .entries
        .get(key)
        .is_some_and(|e| e.ty == "int");
    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            let v = sig.get();
            let val = if is_int {
                Value::from(v as i64)
            } else {
                Value::from(v)
            };
            c.set_path(&kw, val);
        }));
    state
        .fields
        .getters
        .borrow_mut()
        .insert(key.to_string(), Box::new(move || Value::from(sig.get())));
    let kl = key.to_string();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| sig.set(c.get_f64(&kl).unwrap_or(def))),
    );
    Element::stepper(sig, min, max, step).width(CTRL_W)
}

fn build_string(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    let def = state
        .capabilities
        .default_for(key)
        .as_str()
        .unwrap_or("")
        .to_string();
    let cur = cfg
        .get_str(key)
        .map(str::to_string)
        .unwrap_or_else(|| def.clone());
    let sig = signal(cur);
    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            c.set_path(&kw, Value::String(sig.get()))
        }));
    state
        .fields
        .getters
        .borrow_mut()
        .insert(key.to_string(), Box::new(move || Value::String(sig.get())));
    let kl = key.to_string();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            sig.set(
                c.get_str(&kl)
                    .map(str::to_string)
                    .unwrap_or_else(|| def.clone()),
            )
        }),
    );
    Element::text_input(sig, item.hint.clone().unwrap_or_default()).width(200)
}

/// hotkey 主键候选 (value, 显示名)。idx 0 = 未设置（值为空 → 合成 "none"）。
pub(crate) fn hotkey_keys() -> Vec<(String, String)> {
    let mut v = vec![(String::new(), "（未设置）".to_string())];
    for c in 'a'..='z' {
        v.push((c.to_string(), c.to_ascii_uppercase().to_string()));
    }
    for d in '0'..='9' {
        v.push((d.to_string(), d.to_string()));
    }
    for n in 1..=12 {
        v.push((format!("f{n}"), format!("F{n}")));
    }
    // 注：core 对 = / - 用命名键 equal / minus（见快照默认 "ctrl+equal"），其余标点用字面符号。
    for (val, lbl) in [
        ("space", "Space"),
        ("tab", "Tab"),
        ("enter", "Enter"),
        (".", "."),
        (",", ","),
        ("/", "/"),
        (";", ";"),
        ("'", "'"),
        ("[", "["),
        ("]", "]"),
        ("minus", "-"),
        ("equal", "="),
        ("`", "`"),
        ("\\", "\\"),
    ] {
        v.push((val.to_string(), lbl.to_string()));
    }
    v
}

/// 修饰键 (值, 显示名)，顺序即合成顺序（与 core 既有默认 "ctrl+shift+e" 一致）。
const HOTKEY_MODS: [(&str, &str); 4] = [
    ("ctrl", "Ctrl"),
    ("shift", "Shift"),
    ("alt", "Alt"),
    ("win", "Win"),
];

/// 解析 "ctrl+shift+e" → (修饰位 [ctrl,shift,alt,win], 主键值)。"none"/空 → 全 false + 空键。
pub(crate) fn parse_hotkey(s: &str) -> ([bool; 4], String) {
    let mut mods = [false; 4];
    let mut key = String::new();
    if s.is_empty() || s == "none" {
        return (mods, key);
    }
    for part in s.split('+') {
        match part.trim() {
            "ctrl" => mods[0] = true,
            "shift" => mods[1] = true,
            "alt" => mods[2] = true,
            "win" | "super" | "meta" | "cmd" => mods[3] = true,
            "" => {}
            other => key = other.to_string(),
        }
    }
    (mods, key)
}

/// 合成 "ctrl+shift+e"；无主键 → "none"。
pub(crate) fn compose_hotkey(mods: [bool; 4], key: &str) -> String {
    if key.is_empty() {
        return "none".to_string();
    }
    let mut parts: Vec<&str> = Vec::new();
    for (i, (name, _)) in HOTKEY_MODS.iter().enumerate() {
        if mods[i] {
            parts.push(name);
        }
    }
    parts.push(key);
    parts.join("+")
}

/// 显示用格式化 "Ctrl+Shift+E"；none/空 → "未设置"。
pub(crate) fn format_hotkey(s: &str) -> String {
    if s.is_empty() || s == "none" {
        return "未设置".to_string();
    }
    s.split('+')
        .map(|p| match p.trim() {
            "ctrl" => "Ctrl".to_string(),
            "shift" => "Shift".to_string(),
            "alt" => "Alt".to_string(),
            "win" | "super" | "meta" | "cmd" => "Win".to_string(),
            "equal" => "=".to_string(),
            "minus" => "-".to_string(),
            "space" => "Space".to_string(),
            "tab" => "Tab".to_string(),
            "enter" => "Enter".to_string(),
            "number" => "数字".to_string(),
            other => {
                let mut c = other.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// hotkey 控件：行右侧 = 当前绑定文本（响应式）+「设置」按钮（打开编辑对话框）。
/// 值为 "ctrl+shift+e" 形式或 "none"；支持全局者登记进 global_flags 收集器。
/// 拆分热键绑定值：返回 (是否启用, 用于展示/编辑的绑定)。
/// 禁用（"none"/空）时以默认绑定回填，便于置灰展示与重新启用。
fn split_binding(v: &str, def: &str) -> (bool, String) {
    let on = !v.is_empty() && v != "none";
    (on, if on { v.to_string() } else { def.to_string() })
}

/// 热键有效写回值：启用→绑定，禁用→"none"（core 未绑定 sentinel）。
fn effective_hotkey(enabled: bool, raw: &str) -> String {
    if enabled {
        raw.to_string()
    } else {
        "none".to_string()
    }
}

fn build_hotkey(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    use crate::dialogs::field_dialogs::build_hotkey_dialog;

    let def = state
        .capabilities
        .default_for(key)
        .as_str()
        .unwrap_or("none")
        .to_string();
    let cur = cfg
        .get_str(key)
        .map(str::to_string)
        .unwrap_or_else(|| def.clone());
    // 启用 = 有实际绑定；禁用（"none"/空）时以默认绑定填充 raw（置灰展示，重新启用即用默认）。
    let (on, binding) = split_binding(&cur, &def);
    let enabled_sig = signal(on);

    // 候选键表：确保当前绑定的主键一定在列表内（否则打开对话框 idx 回退 0，确认即丢绑定）。
    let mut keys = hotkey_keys();
    let (_, cur_key) = parse_hotkey(&binding);
    if !cur_key.is_empty() && !keys.iter().any(|(v, _)| *v == cur_key) {
        keys.push((cur_key.clone(), format_hotkey(&cur_key)));
    }
    let key_vals: Vec<String> = keys.iter().map(|(v, _)| v.clone()).collect();
    let key_labels: Vec<String> = keys.iter().map(|(_, l)| l.clone()).collect();
    let disp = signal(format_hotkey(&binding));
    let raw = signal(binding);
    let visible = signal(false);

    // 对话框工作态：修饰位 + 主键 idx（打开时从 raw 播种、确认时合成回 raw/disp）。
    let mods = [signal(false), signal(false), signal(false), signal(false)];
    let key_idx = signal(0usize);

    // writer：启用时写 raw 绑定，禁用时写 "none"（core 未绑定 sentinel）。
    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            c.set_path(
                &kw,
                Value::String(effective_hotkey(enabled_sig.get(), &raw.get())),
            )
        }));
    // loader：回填启用态 + raw + disp（禁用时以默认绑定填充，置灰展示）。
    let kl = key.to_string();
    let def_l = def.clone();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let v = c
                .get_str(&kl)
                .map(str::to_string)
                .unwrap_or_else(|| def_l.clone());
            let (on, binding) = split_binding(&v, &def_l);
            enabled_sig.set(on);
            disp.set(format_hotkey(&binding));
            raw.set(binding);
        }),
    );
    // getter（一致性：供潜在 enabled_when 读取）——返回实际写回的有效值。
    state.fields.getters.borrow_mut().insert(
        key.to_string(),
        Box::new(move || Value::String(effective_hotkey(enabled_sig.get(), &raw.get()))),
    );

    // 全局勾选（可选）：登记进收集器，由 register_global_hotkeys_collector 汇总写回
    let global_sig = if item.global {
        let field = key.strip_prefix("keys.").unwrap_or(key).to_string();
        let in_global = cfg
            .get_array("keys.global_hotkeys")
            .map(|a| a.iter().any(|v| v.as_str() == Some(field.as_str())))
            .unwrap_or(false);
        let g = signal(in_global);
        state.fields.global_flags.borrow_mut().push((field, g));
        Some(g)
    } else {
        None
    };

    let dlg = build_hotkey_dialog(
        visible,
        &item.label,
        raw,
        disp,
        def,
        mods,
        key_idx,
        key_vals.clone(),
        key_labels,
        global_sig,
        enabled_sig,
    );
    register_dialog(state, dlg);

    // 行控件：[启用] 复选 + 当前绑定（响应式）+ 设置按钮（打开前从 raw 播种工作态）。
    // 禁用时绑定文本与设置按钮一并置灰（enabled_when），写回 "none"。
    let kv = key_vals;
    let open = Element::button("设置")
        .small()
        .outline_soft()
        .enabled_signal(enabled_sig)
        .on_click(move |_| {
            let (m, k) = parse_hotkey(&raw.get());
            for (i, s) in mods.iter().enumerate() {
                s.set(m[i]);
            }
            key_idx.set(kv.iter().position(|v| *v == k).unwrap_or(0));
            visible.set(true);
        });
    Element::row()
        .cross(Align::Center)
        .spacing(10)
        .child(
            Element::checkbox("启用", enabled_sig)
                .small()
                .fg_role(Role::Text),
        )
        .child(
            // 固定宽度：防止长热键文本（如 "Ctrl+Shift+\"）撑宽右侧，挤压左侧功能名标签。
            Element::label_signal(disp)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .max_lines(1)
                .width(114)
                .enabled_signal(enabled_sig),
        )
        .child(open)
}

/// 汇总全局热键收集器为 `keys.global_hotkeys` strlist 的 writer + loader。
/// 须在 `pages::all_tabs` 之后调用（此时各支持全局的功能键已登记进 global_flags）。
pub fn register_global_hotkeys_collector(state: &AppState) {
    let flags = state.fields.global_flags.borrow();
    if flags.is_empty() {
        return;
    }
    let pairs: Vec<(String, Signal<bool>)> = flags.clone();

    let pairs_w = pairs.clone();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            // 排序使输出确定（数组按值相等比对，避免顺序差异误判 dirty / 每次保存重排）。
            let mut names: Vec<String> = pairs_w
                .iter()
                .filter(|(_, s)| s.get())
                .map(|(f, _)| f.clone())
                .collect();
            names.sort();
            let arr: Vec<Value> = names.into_iter().map(Value::String).collect();
            c.set_path("keys.global_hotkeys", Value::Array(arr));
        }));

    let pairs_l = pairs;
    state.fields.loaders.borrow_mut().insert(
        "keys.global_hotkeys".to_string(),
        Box::new(move |c: &Config| {
            let set: Vec<String> = c
                .get_array("keys.global_hotkeys")
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            for (f, s) in &pairs_l {
                s.set(set.iter().any(|x| x == f));
            }
        }),
    );
}

fn build_segmented(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    let (values, labels): (Vec<String>, Vec<String>) = item
        .options
        .iter()
        .map(|o| (o.value.clone(), o.label.clone()))
        .unzip();
    if values.is_empty() {
        return Element::label("（无可选项）").fg_role(Role::TextMuted);
    }
    // bool 键的分段控制器（两段映射 false/true，值 "false"/"true"）：用于「初始语言模式」等二态项。
    let cap_ty = state
        .capabilities
        .entries
        .get(key)
        .map(|e| e.ty.as_str())
        .unwrap_or("");
    if cap_ty == "bool" {
        return build_segmented_bool(state, key, cfg, values, labels);
    }
    let def_str = state
        .capabilities
        .default_for(key)
        .as_str()
        .unwrap_or("")
        .to_string();
    let cur = cfg
        .get_str(key)
        .map(str::to_string)
        .unwrap_or_else(|| def_str.clone());
    let idx = values.iter().position(|v| *v == cur).unwrap_or(0);
    let sig = signal(idx);
    let kw = key.to_string();
    let vals_w = values.clone();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            if let Some(v) = vals_w.get(sig.get()) {
                c.set_path(&kw, Value::String(v.clone()));
            }
        }));
    let vals_l = values.clone();
    let kl = key.to_string();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let cur = c
                .get_str(&kl)
                .map(str::to_string)
                .unwrap_or_else(|| def_str.clone());
            let idx = vals_l.iter().position(|v| *v == cur).unwrap_or(0);
            sig.set(idx);
        }),
    );
    let vals_g = values;
    state.fields.getters.borrow_mut().insert(
        key.to_string(),
        Box::new(move || {
            vals_g
                .get(sig.get())
                .map(|v| Value::String(v.clone()))
                .unwrap_or(Value::Null)
        }),
    );
    Element::segmented(labels, sig)
}

/// bool 键的分段控制器：两段，值 "false"/"true" 映射 bool。idx→对应 value=="true"。
fn build_segmented_bool(
    state: &AppState,
    key: &str,
    cfg: &Config,
    values: Vec<String>,
    labels: Vec<String>,
) -> Element {
    let def = state
        .capabilities
        .default_for(key)
        .as_bool()
        .unwrap_or(false);
    let cur = cfg.get_bool(key).unwrap_or(def);
    let pos_of = |on: bool, vals: &[String]| -> usize {
        let s = if on { "true" } else { "false" };
        vals.iter().position(|v| v == s).unwrap_or(usize::from(on))
    };
    let sig = signal(pos_of(cur, &values));

    let vals_w = values.clone();
    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            let on = vals_w.get(sig.get()).map(|v| v == "true").unwrap_or(false);
            c.set_path(&kw, Value::Bool(on));
        }));

    let vals_g = values.clone();
    state.fields.getters.borrow_mut().insert(
        key.to_string(),
        Box::new(move || Value::Bool(vals_g.get(sig.get()).map(|v| v == "true").unwrap_or(false))),
    );

    let vals_l = values;
    let kl = key.to_string();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| sig.set(pos_of(c.get_bool(&kl).unwrap_or(def), &vals_l))),
    );
    Element::segmented(labels, sig)
}

fn build_checkbox_group(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    let cur: Vec<String> = cfg
        .get_array(key)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| default_strlist(state, key));
    let opts = item.options.clone();
    let sigs: Vec<(String, Signal<bool>)> = opts
        .iter()
        .map(|o| (o.value.clone(), signal(cur.contains(&o.value))))
        .collect();

    // writer：收集勾选 → strlist（经 collect_checked 单一真相源）
    let sigs_w = sigs.clone();
    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            let pairs: Vec<(&str, bool)> =
                sigs_w.iter().map(|(v, s)| (v.as_str(), s.get())).collect();
            let arr: Vec<Value> = collect_checked(&pairs)
                .into_iter()
                .map(Value::String)
                .collect();
            c.set_path(&kw, Value::Array(arr));
        }));

    // getter：当前勾选值的 Value::Array（供其它项 enabled_when 联动读取）
    let sigs_g = sigs.clone();
    state.fields.getters.borrow_mut().insert(
        key.to_string(),
        Box::new(move || {
            let pairs: Vec<(&str, bool)> =
                sigs_g.iter().map(|(v, s)| (v.as_str(), s.get())).collect();
            let arr: Vec<Value> = collect_checked(&pairs)
                .into_iter()
                .map(Value::String)
                .collect();
            Value::Array(arr)
        }),
    );

    // loader：从 Config 回填各 signal（缺省回落 capability 默认）
    let sigs_l = sigs.clone();
    let kl = key.to_string();
    let def_l = default_strlist(state, key);
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let cur: Vec<String> = c
                .get_array(&kl)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_else(|| def_l.clone());
            for (v, s) in &sigs_l {
                s.set(cur.iter().any(|c| c == v));
            }
        }),
    );

    // 控件：grid 复选（2 列，小号 checkbox 省空间）。网格占满右侧由 setting_row_checks 控制。
    let cells: Vec<Element> = sigs
        .iter()
        .zip(opts.iter())
        .map(|((_, s), o)| Element::checkbox(&o.label, *s).small().fg_role(Role::Text))
        .collect();
    Element::grid(2, 10, cells)
}

/// 读 capability 默认值并解析为字符串数组（strlist 类型回落用）。
fn default_strlist(state: &AppState, key: &str) -> Vec<String> {
    state
        .capabilities
        .default_for(key)
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 读 capability 默认值并解析为键值对 map（map 类型回落用）。
fn default_map(state: &AppState, key: &str) -> serde_json::Map<String, Value> {
    state
        .capabilities
        .default_for(key)
        .as_object()
        .cloned()
        .unwrap_or_default()
}

/// 按声明顺序收集被勾选的选项值（写回 strlist 用）。
pub(crate) fn collect_checked(pairs: &[(&str, bool)]) -> Vec<String> {
    pairs
        .iter()
        .filter(|(_, on)| *on)
        .map(|(v, _)| v.to_string())
        .collect()
}

/// 动态选项来源（方案/字体）：从已加载的 schema/font 列表填充。
fn dynamic_options(state: &AppState, src: &str) -> (Vec<String>, Vec<String>) {
    match src {
        "codetableSchemas" => schema_options(&state.schemas, Some("codetable")),
        "pinyinSchemas" => schema_options(&state.schemas, Some("pinyin")),
        "fonts" => {
            let v: Vec<String> = state.fonts.clone();
            (v.clone(), v)
        }
        // 主题：值=id/slug（写回 ui.theme.name），标签=显示名。
        "themes" => state
            .theme_entries
            .iter()
            .map(|e| (e.id.clone(), e.display.clone()))
            .unzip(),
        _ => (vec![], vec![]),
    }
}

/// 方案选项：可按 kind 过滤；首项补一个"自动/未设置"空值便于回落。
///
/// **主方案下拉的唯一选项来源**——`dynamic_options()`（清单声明的 options_source）与
/// `AppState::apply_loaded` 的响应式 signal 都走这里。两者曾各写一份，语义悄悄漂成相反：
/// 一份保留 kind 为空的方案、另一份排除，而生效的恰是排除那份，导致同一个方案在两条路径
/// 下命运不同。故本函数接收 `&[SchemaInfo]` 而非 `&AppState`，让两个调用点都能复用。
pub(crate) fn schema_options(
    schemas: &[SchemaInfo],
    kind: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    // 空值（首项）语义随类型不同，标签须分别表达：
    //   码表 primary_codetable 空 = 自动挑 available 首个码表方案；
    //   拼音 primary_pinyin   空 = 固定全拼（core 不扫描 available，见 temp_pinyin_target）。
    // 拼音项不直接叫「全拼」——列表里若有 id=pinyin 的方案，其显示名多半也是「全拼」，会撞名。
    let empty_label = match kind {
        Some("pinyin") => "（默认：全拼）",
        _ => "（自动）",
    };
    let mut values = vec![String::new()];
    let mut labels = vec![empty_label.to_string()];
    for s in schemas {
        // kind 为空 = core 没读出该方案的 .schema.toml（TOML 语法错，或字段类型与 Schema
        // 结构体不匹配——第三方方案从别的输入法移植时常见）。此时 core 的 schema_name 会
        // 回退成 id，方案**照常出现在方案列表里**，只有类型是未知的。
        // 这种方案不能排除：排除等于「列表里看得见、下拉里选不到」，用户无从判断出了什么事，
        // 而失败只 warn 进日志。列出来至少让人能选、能试，也能从「名字显示成 id」看出端倪。
        if kind.is_some_and(|k| !s.kind.is_empty() && s.kind != k) {
            continue;
        }
        // 特殊方案（英文、快符）不进主方案下拉：它们不参与方案轮换，选中了也不会被
        // 「主拼音方案」「主码表方案」这类设置消费。方案列表恒含它们（词库页要用），
        // 过滤只在这一个呈现点做。
        if s.hidden {
            continue;
        }
        values.push(s.id.clone());
        labels.push(s.name.clone());
    }
    (values, labels)
}

/// 把对话框主体挂到根（登记到注册表，`shell::build` 取出挂到根窗口 stack 实现全窗模态）。
fn register_dialog(state: &AppState, dlg: Element) {
    state.fields.dialogs.borrow_mut().push(dlg);
}

/// 模糊音紧凑组：启用开关 + 全选 checkbox + 11 对小号 checkbox（2 列），随启用置灰。
/// 各项经 register_toggle 登记 writer/loader（key 在清单 schema/模糊音，受「恢复本页」覆盖）。
fn build_fuzzy_group(state: &AppState, cfg: &Config) -> Element {
    let en = register_toggle(state, "schema.pinyin.fuzzy.enabled", cfg);
    let pairs = [
        ("schema.pinyin.fuzzy.zh_z", "zh ↔ z"),
        ("schema.pinyin.fuzzy.ch_c", "ch ↔ c"),
        ("schema.pinyin.fuzzy.sh_s", "sh ↔ s"),
        ("schema.pinyin.fuzzy.n_l", "n ↔ l"),
        ("schema.pinyin.fuzzy.r_l", "r ↔ l"),
        ("schema.pinyin.fuzzy.f_h", "f ↔ h"),
        ("schema.pinyin.fuzzy.an_ang", "an ↔ ang"),
        ("schema.pinyin.fuzzy.en_eng", "en ↔ eng"),
        ("schema.pinyin.fuzzy.in_ing", "in ↔ ing"),
        ("schema.pinyin.fuzzy.ian_iang", "ian ↔ iang"),
        ("schema.pinyin.fuzzy.uan_uang", "uan ↔ uang"),
    ];
    let sigs: Vec<Signal<bool>> = pairs
        .iter()
        .map(|(k, _)| register_toggle(state, k, cfg))
        .collect();
    let cells: Vec<Element> = pairs
        .iter()
        .zip(sigs.iter())
        .map(|((_, lbl), s)| Element::checkbox(*lbl, *s).small().fg_role(Role::Text))
        .collect();
    let grid = Element::grid(2, 10, cells).width_match().enabled_signal(en);

    // 全选 checkbox：受控（on_toggle 接管点击），点击即把 11 项整体设为「取反当前是否
    // 全选」——已全选时点击变全不选，故同一个 checkbox 兼顾全选/取消全选，不需要
    // 额外按钮。checkbox 本身不接收 on-change，故其勾选状态需要另一条腿维持：下方
    // reactive_effect 监听 11 项版本，任一项被单独勾/取消时都重算「是否全选」回写
    // 本信号，让全选框始终如实反映当前状态（而非只在点它自己时才更新）。
    let select_all_sig = signal(sigs.iter().all(|s| s.get()));
    let click_sigs = sigs.clone();
    let select_all_box = Element::checkbox("全选", select_all_sig)
        .small()
        .fg_role(Role::Text)
        .enabled_signal(en)
        .on_toggle(move |_ctx| {
            let target = !click_sigs.iter().all(|s| s.get());
            for s in &click_sigs {
                s.set(target);
            }
        });
    let version_sigs = sigs.clone();
    let sync_sigs = sigs.clone();
    let sync_effect = crate::pages::dict::reactive::reactive_effect(
        move || {
            version_sigs
                .iter()
                .fold(0u64, |acc, s| acc.wrapping_add(s.version()))
        },
        move || {
            let all = sync_sigs.iter().all(|s| s.get());
            if select_all_sig.get() != all {
                select_all_sig.set(all);
            }
        },
    );

    Element::col()
        .width_match()
        .padding_xy(0, 11)
        .spacing(10)
        .child(setting_row(
            "启用模糊音",
            Some("勾选下方拼音对，使其互不区分"),
            Element::row()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::switch(en))
                .child(select_all_box),
        ))
        .child(grid)
        .child(sync_effect)
}

/// 对话框内分组标题（section/subsection 同一层级，主题色强调，不额外缩进）。
pub(crate) fn dialog_group_header(name: &str) -> Element {
    Element::label(name)
        .font_size(DIALOG_HEADER_SIZE)
        .font_weight(600)
        .fg_role(Role::Accent)
        .width_match()
        .padding_xy(0, 5)
}

/// 容器顶部那句「本页可被方案覆盖」的说明行（[`SchemaHintMode::Header`] 用）。
///
/// 图标 + 常驻文字，**不是悬停角标**：它说的是整个对话框的性质，用户不会想到去悬停一个
/// 标题；而它替掉的是十几个角标，一行常驻文字换回来的空间绰绰有余。
pub(crate) fn dialog_notice_row(note: &str) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Start)
        .spacing(6)
        .padding_xy(0, 4)
        .child(
            // 与行内角标同一枚细图标：同一件事在同一个对话框里换个画法只会让人以为是两件事。
            crate::icons::tinted(crate::icons::INFO_THIN, 14, Color::hex(0x8A94A6))
                .padding_xy(0, 2),
        )
        .child(
            Element::label(note)
                .font_size(12.0)
                .fg_role(Role::TextSubtle)
                .weight(1.0)
                .max_lines(2),
        )
}

/// [`SchemaHintMode::Header`] 那句统一说明的文案：取容器内出现最多的那条 `note`。
///
/// core 的 `SCHEMA_OVERRIDES` 按配置段给 note（`[codetable]` / `[codetable.frequency]` /
/// `[aux_code]` 各一句），措辞不同但语义一样。取众数而不是自己新写一句：**文案的权威在
/// core**，这边新写一句就成了第二真相源，core 改了措辞这里不会跟着变。
fn schema_hint_header_note(state: &AppState, keys: &[String]) -> Option<String> {
    let mut tally: HashMap<&str, usize> = HashMap::new();
    for k in keys {
        if let Some(h) = schema_override_of(state, k) {
            *tally.entry(h.note.as_str()).or_default() += 1;
        }
    }
    tally
        .into_iter()
        .max_by_key(|(note, n)| (*n, note.len()))
        .map(|(note, _)| note.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// 对话框内小标题的字号。
///
/// ★ **必须 ≥ 条目标题**（`FormTheme::label_size`，13.0）。原先是 11.5，比它管辖的条目
/// 标题还小 ⇒ 层级倒置，一屏看下来分不出哪句是分组名、哪句是设置项名
/// （2026-08-26 用户原话：「title 字体比较小，和其它条目的相对，条目的分割不是很明显」）。
///
/// 复刻这套样式的地方（塞进带按钮的行、装不下整宽 label 时）要一并引用本常量，
/// 别再抄字面量——`widgets::override_group_card` 与「码表配置」节头就是两处复刻。
pub(crate) const DIALOG_HEADER_SIZE: f32 = 13.5;

/// 「全局方案配置」某引擎的 [设置] 按钮 + 对话框（含该 schema-group section 的设置行）。
/// 对话框内顶部插入 section 名称作为一级标题；subsection 作为二级标题。
/// `with_fuzzy`=true 在末尾追加"模糊音"子组（拼音引擎用）。对话框挂根。
pub fn build_scheme_config_button(
    state: &AppState,
    label: &str,
    section: &str,
    with_fuzzy: bool,
) -> Element {
    use crate::dialogs::field_dialogs::build_group_dialog;
    let visible = signal(false);
    let keys: Vec<String> = state
        .manifest
        .sections_of("schema")
        .into_iter()
        .find(|(s, _)| s == section)
        .map(|(_, k)| k)
        .unwrap_or_default();

    let mut rows: Vec<Element> = Vec::new();

    // 「可被方案覆盖」怎么呈现：几乎全可覆盖时统一说一次（见 `schema_hint_mode`）。
    let hint_mode = schema_hint_mode(state, &keys);
    if hint_mode == SchemaHintMode::Header
        && let Some(note) = schema_hint_header_note(state, &keys)
    {
        rows.push(dialog_notice_row(&note));
    }

    // 首个标题：section 本身（如"上屏行为"），与 subsection 同级同样式。
    rows.push(dialog_group_header(section));

    // 按 subsection 分组，跳过 hidden 项；首次出现 subsection 时插入同级标题行。
    let mut cur_subsec: Option<String> = None;
    for k in &keys {
        let Some(item) = state.manifest.get(k) else {
            continue;
        };
        if item.hidden {
            continue;
        }
        let subsec = item.subsection.clone();
        if subsec != cur_subsec {
            if let Some(ref name) = subsec {
                rows.push(dialog_group_header(name));
            }
            cur_subsec = subsec;
        }
        let row = if item.typ == "toggle"
            && let Some(_dlg_item) = state.manifest.dialog_for_gate(k)
        {
            let mut tmp = section_rows(state, &[k.clone()]);
            tmp.pop().unwrap_or_else(|| field_with(state, k, hint_mode))
        } else if item.typ == "dialog_button"
            && let Some(g) = &item.gate_key
            && state.manifest.get(g).is_some()
        {
            continue;
        } else {
            field_with(state, k, hint_mode)
        };
        rows.push(row);
    }
    if with_fuzzy {
        rows.push(dialog_group_header("模糊音"));
        rows.push(build_fuzzy_group(state, &state.current_config));
    }

    // 恢复默认：取 core 权威出厂配置（含折算），灌回各字段 loader。见 [`core_defaults_config`]。
    let fields = state.fields.clone();
    let mut reset_keys = keys.clone();
    if with_fuzzy {
        for k in state.manifest.order.iter() {
            if k.starts_with("schema.pinyin.fuzzy.") {
                reset_keys.push(k.clone());
            }
        }
    }
    let seed = key_action_default_bindings(state);
    let reset_fn: Box<dyn Fn() + 'static> = Box::new(move || {
        let def_cfg = core_defaults_config(&seed);
        let ld = fields.loaders.borrow();
        for k in &reset_keys {
            if let Some(loader) = ld.get(k) {
                loader(&def_cfg);
            }
        }
    });

    let dlg = build_group_dialog(visible, label, rows, Some(reset_fn));
    register_dialog(state, dlg);
    Element::button("设置")
        .small()
        .outline_soft()
        .on_click(move |_| visible.set(true))
}

/// 普通「设置」按钮（无门控）：点击打开 `visible` 绑定的对话框。outline 风格。
fn open_dialog_button(visible: Signal<bool>) -> Element {
    Element::button("设置")
        .small()
        .outline_soft()
        .on_click(move |_| visible.set(true))
}

/// dialog_button 控件（无 gate_key）：单个「设置…」按钮；对话框已挂根。
/// 有 gate_key 者不走此路，由 [`build_gated_dialog_row`] 合并成「开关 + 设置」整行。
fn build_dialog_button(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    let visible = build_dialog_body(state, key, item, cfg);
    open_dialog_button(visible)
}

/// 按 capability 类型分派构建对话框主体，挂到根，返回控制其显隐的 `visible` Signal。
fn build_dialog_body(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Signal<bool> {
    let cap_ty = state
        .capabilities
        .entries
        .get(crate::capabilities::config_path_of(key))
        .map(|e| e.ty.as_str())
        .unwrap_or("");
    // 快捷输入的两项都读写 schema.mix_modes 数组整体（按 id=="quick_mix" 定位其中一项的
    // trigger_keys / members 字段），不是通用 strlist/map 形态，走非声明式整体读写。
    // 按**控件类型**分派而非 key：两者的配置路径相同，只有 type 能区分。
    if item.typ == "dialog_button_key_action" {
        build_dialog_button_key_action(state, item, cfg)
    } else if item.typ == "dialog_button_mix_members" {
        build_dialog_button_mix_members(state, key, item, cfg)
    } else if item.typ == "dialog_button_comment_dicts" {
        // 注释词库：可增删的条目列表 + 每条五个字段，manifest 的「一个 key 一个控件」
        // 套不上，走专用对话框（销掉 capabilities 里 `ui.comment_dicts` 那条 UNCOVERED）。
        build_dialog_button_comment_dicts(state, key, cfg)
    } else if item.typ == "dialog_button_ordered_pairs" {
        // 顺序有语义的 strlist（如 ui.toolbar.items）。与下面 `cap_ty == "strlist"` 那条
        // 通用分支的分野是**顺序是否被 core 消费**，而 capability 里只有类型没有这个信息，
        // 故只能由清单的 type 声明——同 mix_members 那条「按控件类型分派而非 key」。
        build_dialog_button_ordered_pairs(state, key, item, cfg)
    } else if key == "input.punct.custom_mappings" {
        // 自定义标点映射：map 值为 [中半,英全,中全,英半] 四元数组（core 存储序），专用 5 列表格对话框。
        build_dialog_button_punct(state, key, cfg)
    } else if key == "keys.session_actions" {
        // 会话态按键表：map 的键与值都是 core 的内部词表，通用 map 对话框的文本行编辑
        // 会让用户把内部词表当 API 手打（拼错零反馈），故走键/动词双下拉的专用对话框。
        build_dialog_button_session_actions(state, key, cfg)
    } else if key == "keys.key_actions" {
        // 全局引导键表：与上一项同族（同样的行编辑器），分野是触发态——本表管空闲时。
        build_dialog_button_key_actions(state, key, cfg)
    } else if key == "ui.candidate.index_labels" {
        // 自定义序号标签：字符串拆成 10 个序号槽位，专用 10 框对话框。
        build_dialog_button_index_labels(state, key, cfg)
    } else if key == COMMENT_HOST_KEY {
        // 候选注释：横/纵两份模板 + 最大字数 + 可复制的模板参考，共用一个对话框。
        build_dialog_button_comment(state, key, cfg)
    } else if key == "ui.font.fallback" || key == "ui.font.scripts" {
        // ⚠️ 字体名**含空格**（`Microsoft YaHei UI`），两条通用兜底都会毁数据：
        // strlist 的空格分隔编辑把一个字体拆成三个；map 的通用控件按标量读写
        // （`v.as_str()` / `Value::String`），而 `scripts` 的值是**字体链数组**——
        // 读出来是空串、存回去类型都变了，core 解析不了 ⇒ 打开设置页保存一次整项就没了。
        // 故走按**行**切分的专用对话框。
        build_dialog_button_font(state, key, item, cfg)
    } else if cap_ty == "map" {
        build_dialog_button_map(state, key, item, cfg)
    } else if cap_ty == "strlist" && !item.options.is_empty() {
        build_dialog_button_pairs(state, key, item, cfg)
    } else if cap_ty == "str" {
        build_dialog_button_string(state, key, item, cfg)
    } else {
        build_dialog_button_spaced(state, key, item, cfg)
    }
}

/// 「开关 + 设置」合并行：`[gate 标签 / 提示] ······ [启用开关] [设置…]`。
///
/// 复刻旧 webview：一个特性既有「启用」开关、又有需要细调的设置，合并为一行更紧凑。
/// 行标签/提示取自 **gate 开关项**（特性名），「设置…」打开 dialog_button 项的对话框，
/// 仅在开关开启时可点。dialog_button 项自身的标签/提示用作对话框标题/说明。
fn build_gated_dialog_row(
    state: &AppState,
    gate_key: &str,
    gate_item: &MItem,
    dlg_item: &MItem,
    cfg: &Config,
) -> Element {
    let gate_sig = register_toggle(state, gate_key, cfg);
    let visible = build_dialog_body(state, &dlg_item.key, dlg_item, cfg);

    // 开关开启才可点「设置」：用响应式 enabled_when 置灰（windui 0.4.x 起支持闭包启用条件）。
    // `gate_soft` 例外：对话框内容不专属于这个开关（共用字符集 / 里面还寄居着别的开关），
    // 置灰会让另一条路的用户够不着，故只合并排版、按钮常开。
    let btn = if dlg_item.gate_soft {
        open_dialog_button(visible)
    } else {
        open_dialog_button(visible).enabled_signal(gate_sig)
    };

    let control = Element::row()
        .cross(Align::Center)
        .spacing(10)
        .child(Element::checkbox("启用", gate_sig).fg_role(Role::Text))
        .child(btn);
    setting_row(&gate_item.label, gate_item.hint.as_deref(), control)
}

/// dialog_button（strlist，无 options）：空格分隔文本编辑。挂对话框到根，返回 visible。
fn build_dialog_button_spaced(
    state: &AppState,
    key: &str,
    item: &MItem,
    cfg: &Config,
) -> Signal<bool> {
    use crate::dialogs::field_dialogs::{
        build_spaced_text_dialog, spaced_to_strlist, strlist_to_spaced,
    };

    // 读当前值播种文本框 signal（缺省回落 capability 默认）
    let cur: Vec<String> = cfg
        .get_array(key)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| default_strlist(state, key));
    let text = signal(strlist_to_spaced(&cur));
    let visible = signal(false);

    // writer：把文本框内容（空格分隔）写回 strlist
    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            let arr: Vec<Value> = spaced_to_strlist(&text.get())
                .into_iter()
                .map(Value::String)
                .collect();
            c.set_path(&kw, Value::Array(arr));
        }));

    // loader：从 Config 回填文本框（缺省回落 capability 默认）
    let kl = key.to_string();
    let def_l = default_strlist(state, key);
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let cur: Vec<String> = c
                .get_array(&kl)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_else(|| def_l.clone());
            text.set(strlist_to_spaced(&cur));
        }),
    );

    // 恢复默认文本：读 capability 默认值
    let def_text = {
        let d = state.capabilities.default_for(key);
        let v: Vec<String> = d
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        strlist_to_spaced(&v)
    };

    let hint = item.hint.clone().unwrap_or_default();
    let dlg = build_spaced_text_dialog(visible, &item.label, &hint, text, def_text);
    register_dialog(state, dlg);
    visible
}

/// dialog_button（str）：单行字符串编辑对话框（如智能标点字符集/数字后保留英文标点）。
/// 走对话框是为了带「恢复默认」操作。挂对话框到根，返回 visible。
fn build_dialog_button_string(
    state: &AppState,
    key: &str,
    item: &MItem,
    cfg: &Config,
) -> Signal<bool> {
    use crate::dialogs::field_dialogs::build_text_dialog;

    let def = state
        .capabilities
        .default_for(key)
        .as_str()
        .unwrap_or("")
        .to_string();
    let cur = cfg
        .get_str(key)
        .map(str::to_string)
        .unwrap_or_else(|| def.clone());
    let text = signal(cur);
    let visible = signal(false);

    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            c.set_path(&kw, Value::String(text.get()))
        }));

    let kl = key.to_string();
    let def_l = def.clone();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            text.set(
                c.get_str(&kl)
                    .map(str::to_string)
                    .unwrap_or_else(|| def_l.clone()),
            );
        }),
    );

    let hint = item.hint.clone().unwrap_or_default();
    let extra = dialog_extra_rows(state, key);
    let dlg = build_text_dialog(visible, &item.label, &hint, text, def, "输入字符…", extra);
    register_dialog(state, dlg);
    visible
}

/// 收集寄居在 `host_key` 对话框里的从属项（`dialog_extra_for == host_key`），按 manifest 顺序
/// 渲染成行。无成员返回 None（版式与改造前一致）。
///
/// 成员经各自 [`field`] 登记 writer/loader，与独立成行时完全一致 —— 保存/加载/重置都自动覆盖，
/// 不需要为「寄居」这件事另写一套读写。
fn dialog_extra_rows(state: &AppState, host_key: &str) -> Option<Element> {
    let members = dialog_extra_keys(state, host_key);
    if members.is_empty() {
        return None;
    }
    let mut col = Element::col().width_match().spacing(6);
    for k in &members {
        col = col.child(field(state, k));
    }
    Some(col)
}

/// 寄居在 `host_key` 对话框里的从属项的 key，按 manifest 顺序。
fn dialog_extra_keys(state: &AppState, host_key: &str) -> Vec<String> {
    state
        .manifest
        .order
        .iter()
        .filter(|k| {
            state
                .manifest
                .get(k)
                .and_then(|i| i.dialog_extra_for.as_deref())
                == Some(host_key)
        })
        .cloned()
        .collect()
}

/// 「恢复默认」回调：向 core 取权威默认值，灌回 `keys` 各自的 loader。
///
/// 走 loader 而非直接 set signal，是因为调用点通常拿不到从属项的 signal（它们由 [`field`]
/// 在别处建立）；loader 是这些 signal 的唯一公共入口。core 未连接时回落空对象，等效
/// capability fallback——宁可什么都不改，也不要把用户的值清成 0。
/// 「恢复默认」要灌回各 loader 的那份**出厂配置**。三条恢复路径共用（本函数、分组对话框、
/// 底部栏「恢复本页」），**两道处理一道都不能省**：
///
/// - ⛔ **不能用 `Config::from_value({})` 的纯 L1**。顶码上屏（L1 `false` / 出厂 `true`）、
///   拼音自动学习等键的出厂值经 `data/config.toml`（L2）置开，纯 L1 会把它们**误关**。
///   core 的 `config.getDefaults` 正是为此从纯 L1 改成 `system_preset_value` 的
///   （见 wind-rpc `dispatch.rs` 那段注释），而「恢复本页」当时漏改。
/// - ⛔ **必须把引导键的出厂绑定补进 `keys.key_actions`**。五处 `trigger_keys` 的出厂值
///   住在**被折算的那一侧**（`schema.mix_modes[].trigger_keys` /
///   `input.temp_pinyin.trigger_keys`），core 的 `normalize()` 会把它们折算进
///   `keys.key_actions`，而 `getDefaults` 返回的是**未折算**的系统预置 ⇒ 那张表恒为 `{}`。
///   引导键类字段的 loader 读的正是它，不补的话恢复后快捷输入的 `;`、临拼的 `` ` ``
///   一并消失，而**清空即禁用**。见 [`seed_key_action_defaults`]。
/// ⚠️ `seed` 由 [`key_action_default_bindings`] **提前算好**再传进来：本函数的调用方多为
/// `'static` 闭包，握不住 `&AppState`；而这份绑定只依赖清单与 capability，二者在进程内
/// 不变，提前算与延后算等价。
pub(crate) fn core_defaults_config(seed: &[(String, String)]) -> Config {
    let v = crate::rpc::call_core("config.getDefaults", &serde_json::json!({}))
        .unwrap_or(Value::Object(serde_json::Map::new()));
    let mut cfg = Config::from_value(v);
    let mut map: serde_json::Map<String, Value> = cfg
        .get_path(KEY_ACTIONS_PATH)
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    for (k, verb) in seed {
        map.insert(k.clone(), Value::String(verb.clone()));
    }
    cfg.set_path(KEY_ACTIONS_PATH, Value::Object(map));
    cfg
}

/// 引导键类项的**出厂绑定**（键名 → 动词），供 [`core_defaults_config`] 补进 `keys.key_actions`。
///
/// ⛔ **这不是在重写 core 的折算规则**。设置端刻意不把 `wind-config` 作生产依赖
/// （Cargo.toml 里它在 `[dev-dependencies]`，注释写着「仅测试用…不引入新的路径前提」），
/// 所以跑不了 core 的 `normalize()`；而照抄一份折算规则是「同一条规则两处实现」，必然漂移。
///
/// 这里用的是**清单自己的声明**——每个 `dialog_button_key_action` 项的 `key`（出厂值在
/// capability 里的位置）与 `verb`（绑到哪个动词）。那个映射本来就由设置端定义，不是从
/// core 抄来的第二份真相。
pub(crate) fn key_action_default_bindings(state: &AppState) -> Vec<(String, String)> {
    key_action_default_bindings_from(&state.manifest, &state.capabilities)
}

/// 同上，但直接吃清单与 capability——`AppState` 造不出来的地方（测试）走这个。
pub(crate) fn key_action_default_bindings_from(
    m: &Manifest,
    caps: &crate::capabilities::Capabilities,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for key in m.order.iter() {
        let Some(it) = m.get(key) else {
            continue;
        };
        if it.typ != "dialog_button_key_action" {
            continue;
        }
        let Some(verb) = it.verb.as_deref() else {
            continue;
        };
        for k in key_action_defaults_from(&caps.default_for(&it.key), verb) {
            out.push((k, verb.to_string()));
        }
    }
    out
}

fn reset_to_core_defaults(state: &AppState, keys: Vec<String>) -> Box<dyn Fn() + 'static> {
    let fields = state.fields.clone();
    let seed = key_action_default_bindings(state);
    Box::new(move || {
        let def_cfg = core_defaults_config(&seed);
        let ld = fields.loaders.borrow();
        for k in &keys {
            if let Some(loader) = ld.get(k) {
                loader(&def_cfg);
            }
        }
    })
}

/// 分组对话框宿主行：卡片上只有「<dialog_title> …… [设置]」，宿主自身与寄居项都在对话框里。
///
/// 宿主与成员一律经 [`field`] 渲染并登记 writer/loader，与它们独立成行时完全一致——
/// 「搬进对话框」只是版式变化，不改读写语义（保存、加载、恢复本页都照旧覆盖）。
/// 每项之间由 [`build_group_dialog`] 自动加分隔线，故这里逐行 push 而不用
/// [`dialog_extra_rows`] 那个合并成一列的形态。
fn build_grouped_dialog_row(state: &AppState, key: &str, item: &MItem) -> Element {
    use crate::dialogs::field_dialogs::build_group_dialog;
    let visible = signal(false);
    let title = item
        .dialog_title
        .clone()
        .unwrap_or_else(|| item.label.clone());

    let members = dialog_extra_keys(state, key);
    let mut rows = vec![field(state, key)];
    for k in &members {
        rows.push(field(state, k));
    }

    let reset_keys: Vec<String> = std::iter::once(key.to_string()).chain(members).collect();
    let reset_fn = reset_to_core_defaults(state, reset_keys);

    let dlg = build_group_dialog(visible, &title, rows, Some(reset_fn));
    register_dialog(state, dlg);
    setting_row(
        &title,
        item.dialog_hint.as_deref(),
        open_dialog_button(visible),
    )
}

/// 候选注释对话框的宿主 key。宿主取**横排**模板：它是对话框里第一个出现的字段，而横排的
/// 可用宽度由全部候选共享、最需要斟酌，该让用户先看到它。
const COMMENT_HOST_KEY: &str = "ui.candidate.comment_template_horizontal";

/// 候选注释：宿主（横排模板）自建输入行，纵排模板与最大字数经 `dialog_extra_for` 寄居，
/// 末尾追加可复制的模板参考。
///
/// **为什么不直接用 `build_dialog_button_string`**（宿主是 str，本会落到那条通用分支）：
/// 那个对话框把 `item.hint` 渲染成 `max_lines(3)` 的**标签**。而这里的说明是变量表 + 示例
/// 模板，既超过三行，又是用户要照抄进输入框的东西——标签选不中、复制不了。参考区因此改用
/// `text_input` 承载，见 [`crate::dialogs::field_dialogs::build_comment_dialog`]。
///
/// 宿主的读写按 `build_dialog_button_string` 的写法手工登记；寄居项各自经 [`field`] 登记，
/// 与独立成行时完全一致（保存/加载/恢复默认都自动覆盖）。
fn build_dialog_button_comment(state: &AppState, key: &str, cfg: &Config) -> Signal<bool> {
    use crate::dialogs::field_dialogs::build_comment_dialog;

    let def = state
        .capabilities
        .default_for(key)
        .as_str()
        .unwrap_or("")
        .to_string();
    let cur = cfg
        .get_str(key)
        .map(str::to_string)
        .unwrap_or_else(|| def.clone());
    let text = signal(cur);
    let visible = signal(false);

    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            c.set_path(&kw, Value::String(text.get()))
        }));
    let kl = key.to_string();
    let def_l = def.clone();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            text.set(
                c.get_str(&kl)
                    .map(str::to_string)
                    .unwrap_or_else(|| def_l.clone()),
            );
        }),
    );
    state
        .fields
        .getters
        .borrow_mut()
        .insert(key.to_string(), Box::new(move || Value::String(text.get())));

    // 对话框内这一行的标签**不用 `item.label`**：那是卡片行的文案（「候选注释」，说的是整件
    // 事），而对话框里它只是三个字段之一，得叫「横排注释」才与旁边的「竖排注释」成对。
    // 同 `build_dialog_button_index_labels` 硬编码对话框标题的先例——专用 builder 里的文案
    // 属于该对话框的版式，不是清单项的属性。
    let mut rows = vec![setting_row(
        "横排注释",
        Some("横排候选窗的宽度由全部候选共享，放注音或字根容易把窗口撑宽"),
        Element::text_input(text, "留空则不显示注释").width(240),
    )];
    if let Some(extra) = dialog_extra_rows(state, key) {
        rows.push(extra);
    }

    // 恢复默认：宿主与寄居项一并复位（见 [`reset_to_core_defaults`]）。
    let reset_keys: Vec<String> = std::iter::once(key.to_string())
        .chain(dialog_extra_keys(state, key))
        .collect();
    let reset_fn = reset_to_core_defaults(state, reset_keys);

    let dlg = build_comment_dialog(visible, "候选注释", rows, Some(reset_fn));
    register_dialog(state, dlg);
    visible
}

/// dialog_button（strlist with options）：勾选配对清单。挂对话框到根，返回 visible。
fn build_dialog_button_pairs(
    state: &AppState,
    key: &str,
    item: &MItem,
    cfg: &Config,
) -> Signal<bool> {
    use crate::dialogs::field_dialogs::build_pairs_dialog;

    let defaults = default_strlist(state, key);
    let cur: Vec<String> = cfg
        .get_array(key)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| defaults.clone());
    let opts = item.options.clone();
    let sigs: Vec<(String, Signal<bool>)> = opts
        .iter()
        .map(|o| (o.value.clone(), signal(cur.contains(&o.value))))
        .collect();
    let visible = signal(false);

    // writer
    let sigs_w = sigs.clone();
    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            let pairs: Vec<(&str, bool)> =
                sigs_w.iter().map(|(v, s)| (v.as_str(), s.get())).collect();
            let arr: Vec<Value> = collect_checked(&pairs)
                .into_iter()
                .map(Value::String)
                .collect();
            c.set_path(&kw, Value::Array(arr));
        }));

    // loader（缺省回落 capability 默认）
    let sigs_l = sigs.clone();
    let kl = key.to_string();
    let def_l = defaults.clone();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let cur: Vec<String> = c
                .get_array(&kl)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_else(|| def_l.clone());
            for (v, s) in &sigs_l {
                s.set(cur.iter().any(|c| c == v));
            }
        }),
    );
    let labels: Vec<String> = opts.iter().map(|o| o.label.clone()).collect();
    let hint = item.hint.clone().unwrap_or_default();
    let dlg = build_pairs_dialog(visible, &item.label, &hint, sigs, labels, defaults);
    register_dialog(state, dlg);
    visible
}

/// 剥掉「关着」的 `-` 前缀，得到条目本名。无前缀则原样返回。
///
/// 与 core 的 `parse_toolbar_items` 是**同一套编码**的两端：那边负责「不渲染带前缀的项」，
/// 这边负责「把它读回成一个关着的行」。两处都只剥一层——`--x` 是手滑，不是双重否定。
pub(crate) fn strip_off(s: &str) -> &str {
    s.strip_prefix('-').unwrap_or(s)
}

/// 条目在当前列表里是否**开着**：以本名原样出现过（即那一次没带 `-` 前缀）。
///
/// 没出现过 = 关。这条让**旧配置**（只列启用项、无前缀）不必迁移即可正确读回：
/// 没列出的项判为关，位置由 `managed_order` 补在声明序位——与本编码引入前逐项相同。
pub(crate) fn enabled_of(cur: &[String], value: &str) -> bool {
    cur.iter().any(|s| s == value)
}

/// 把「界面行序 + 启用集」编码成配置数组：开着的写本名，关着的加 `-` 前缀。
///
/// **写全序**是这个编码存在的理由：只写启用项的话，关掉一项就丢掉了它的位置，
/// 重新打开只能补在声明序位，用户体感是「排好的顺序，关一下再开就乱了」。
pub(crate) fn encode_ordered_items(order: &[String], enabled: &[String]) -> Vec<String> {
    order
        .iter()
        .map(|v| {
            if enabled.iter().any(|e| e == v) {
                v.clone()
            } else {
                format!("-{v}")
            }
        })
        .collect()
}

/// ordered_pairs 写回配置的**完整取值逻辑**：行序 + 开关 + 当前配置 → 最终数组。
///
/// 抽成纯函数是因为 writer 挂在 `AppState` 上、而 `AppState` 需要 windui 运行时立不起来
/// ⇒ 那条链端到端测不了。把组合逻辑收在这里，至少保证「怎么组合」只有一份实现、
/// 且这一份被测到；writer 退化成一行转发。
///
/// ⚠️ 仍**测不到**「writer 真的调了本函数」——那一步只能靠 review 与真机。改 writer 时
/// 别把这几步重新内联回去。
pub(crate) fn ordered_pairs_write_value(
    order: &[String],
    enabled: &[String],
    managed: &[String],
    current: &[String],
) -> Vec<String> {
    let encoded = encode_ordered_items(order, enabled);
    merge_preserving_foreign(current, managed, &encoded)
}

/// 把界面结果并回当前配置，**保留界面不认识的条目**并按锚点还原它们的落点。
///
/// # 为什么必须有这一步
///
/// `managed_order` 只把界面管得着的条目排进行序（界面显示不了别的），于是 writer 若
/// 直接整表覆盖，用户**手写**的条目就没了。2026-08-26 真机踩到：用户手写了一个
/// `custom:sym` 自定义按钮，随后在设置页动了一下「显示内容」，那一条**当场消失**。
///
/// `mix_members` 早就有同一道保护（`apply_member_order`，其模块注释写着「写回时仍必须
/// 保留界面不认识的成员，且不能把它们冲到列表末尾」）。我复用了它的 `managed_order`
/// 却没搬这一半——**一个算法拆两半用，只搬走了显示的那一半。**
///
/// # 锚点规则
///
/// 记下外来条目原本前面有几个「界面管得着的条目」，重排后仍插在新序列的同一个序号处。
/// 这让「用户没动过顺序」成为恒等变换，也让手写的混排在拖动后不失真。
/// 反例是「外来条目一律追加末尾」：那会让用户每保存一次就把手写的编排推走一截。
///
/// 判定「界面认不认识」时先剥 `-` 前缀：一个关着的 `-custom:sym` 与开着的
/// `custom:sym` 是同一个条目，不能因为带了前缀就被当成外来项复制一份。
pub(crate) fn merge_preserving_foreign(
    current: &[String],
    managed: &[String],
    encoded: &[String],
) -> Vec<String> {
    let is_managed = |s: &str| managed.iter().any(|m| m == strip_off(s));
    // (锚点 = 它前面的受管条目个数, 原样保留的条目)
    let mut foreign: Vec<(usize, String)> = Vec::new();
    let mut seen = 0usize;
    for s in current {
        if is_managed(s) {
            seen += 1;
        } else {
            foreign.push((seen, s.clone()));
        }
    }
    if foreign.is_empty() {
        return encoded.to_vec();
    }
    let mut out: Vec<String> = Vec::with_capacity(encoded.len() + foreign.len());
    let mut fi = 0;
    for (i, s) in encoded.iter().enumerate() {
        while fi < foreign.len() && foreign[fi].0 <= i {
            out.push(foreign[fi].1.clone());
            fi += 1;
        }
        out.push(s.clone());
    }
    // 锚点落在新序列末尾之后的（原本排在所有受管条目之后）。
    for (_, s) in foreign.into_iter().skip(fi) {
        out.push(s);
    }
    out
}

/// `dialog_button_ordered_pairs`：**顺序有语义**的 strlist 编辑——一列开关决定有无，
/// 拖拽手柄决定顺序。写回路径就是 `item.key` 本身（不同于 mix_members 要 patch 进数组
/// 里的某一项），故可被任何「顺序即语义」的 strlist 键复用。
///
/// # 与 [`build_dialog_button_pairs`] 的分野只有一条
///
/// 那个控件写回的数组**顺序无语义**（core 侧按固定顺序消费，如 `ui.status.items`），
/// 所以恒按声明序写回即可；本控件写回的顺序**就是** core 的消费顺序。这条差别决定了
/// 两件事，缺一不可：
///
/// 1. 必须让用户排（否则用户手排的顺序会被声明序静默改写——`config-design-rules.md` §R3
///    点名的「两不管」）；
/// 2. 载入时顺序也要跟着配置回位。只复位开关会把上次拖出来的顺序留在界面上，与刚载入的
///    配置不符，保存时又把这个错顺序写回去——同 `build_dialog_button_mix_members` 里
///    那条注释记的坑。
fn build_dialog_button_ordered_pairs(
    state: &AppState,
    key: &str,
    item: &MItem,
    cfg: &Config,
) -> Signal<bool> {
    use crate::dialogs::toolbar_items_dialog::{
        BuiltinRow, ToolbarItemsUi, build_toolbar_items_dialog, custom_value,
    };
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    /// 读一个 strlist 键；缺省/非数组回落 `def`。
    fn read_list(c: &Config, key: &str, def: &[String]) -> Vec<String> {
        c.get_array(key)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_else(|| def.to_vec())
    }

    let builtins: Vec<BuiltinRow> = item
        .options
        .iter()
        .map(|o| BuiltinRow {
            value: o.value.clone(),
            label: o.label.clone(),
            desc: o.hint.clone(),
        })
        .collect();
    let declared: Vec<String> = builtins.iter().map(|b| b.value.clone()).collect();
    let defaults = default_strlist(state, key);
    let cur = read_list(cfg, key, &defaults);
    let cur_buttons = read_buttons(cfg);

    // 受管条目 = 内置格 + **当前已定义的**自定义按钮。后者随新建/删除变化，故写回时
    // 要按那一刻的按钮列表重算，不能用这份构建期快照（见 writer 里的注释）。
    let ui = ToolbarItemsUi {
        visible: signal(false),
        switches: Rc::new(RefCell::new(HashMap::new())),
        buttons: signal(cur_buttons.clone()),
        order: signal(Vec::new()),
        form: crate::dialogs::toolbar_button_edit::ButtonForm::new(),
    };
    // 开关与行序的初值：与 loader 同一套算法，避免"首次打开"与"重新加载后"不一致。
    apply_items_to_ui(&ui, &cur, &declared, &cur_buttons);

    let ui_w = ui.clone();
    let kw = key.to_string();
    let declared_w = declared.clone();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            // **写全序**：关掉的项也写进去，加 `-` 前缀表示「在这个位置，但不显示」。
            // 早先是 `.filter(开着的)`，关掉的项直接从数组里消失——位置信息也没了，
            // 重新打开只能补在声明序位。顺序与启用态留在同一个数组里，就不存在两张表
            // 要同步的问题。
            let on = ui_w.enabled_values();
            // 受管集合按**此刻**的按钮列表算：新建的按钮要算受管（否则它刚被写进
            // `order`，却被当成外来项，位置会错乱）；删掉的不再受管。
            let mut managed = declared_w.clone();
            managed.extend(ui_w.buttons.get().iter().map(|b| custom_value(&b.id)));
            // ⚠️ **必须以写回那一刻的配置为准**读外来条目，不能用界面构建时的快照：
            // 中间可能发生过「恢复本页」「重新加载」，甚至用户在外部编辑器改了配置。
            let cur_now = read_list(c, &kw, &[]);
            let merged = ordered_pairs_write_value(&ui_w.order.get(), &on, &managed, &cur_now);
            let arr: Vec<Value> = merged.into_iter().map(Value::String).collect();
            c.set_path(&kw, Value::Array(arr));
            // 按钮定义与引用是**同一次保存的两半**，必须一起落盘：只写 items 会留下
            // 指不到按钮的引用，只写 buttons 会有一个谁也没引用的按钮。
            c.set_path(
                BUTTONS_KEY,
                Value::Array(buttons_to_json(&ui_w.buttons.get())),
            );
        }));

    let ui_l = ui.clone();
    let kl = key.to_string();
    let def_l = defaults.clone();
    let declared_l = declared.clone();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let cur = read_list(c, &kl, &def_l);
            let btns = read_buttons(c);
            ui_l.buttons.set(btns.clone());
            apply_items_to_ui(&ui_l, &cur, &declared_l, &btns);
        }),
    );

    let hint = item.hint.clone().unwrap_or_default();
    let visible = ui.visible;
    // 至少留一项开着：core 对「全关」只能回落成全部显示（`items` 全带 `-` 前缀时），
    // 那与用户「我把它们都关了」的意图正好相反。越界时弹 toast 说明，不置灰。
    let dlg = build_toolbar_items_dialog(&ui, &item.label, &hint, builtins, declared, 1);
    register_dialog(state, dlg);
    visible
}

/// 注释词库（`ui.comment_dicts`）：可增删的条目列表，走专用对话框。
///
/// ★ 与 `ordered_pairs` 那条一样按**控件类型**分派而非 key——但这一项的 key 本就唯一，
/// 之所以仍走 type，是为了让「加一个同形态的列表」不必再改分派链。
fn build_dialog_button_comment_dicts(state: &AppState, key: &str, cfg: &Config) -> Signal<bool> {
    use crate::dialogs::comment_dicts_dialog::{CommentDictsUi, build_comment_dicts_dialog};

    let ui = CommentDictsUi::new();
    ui.seed(cfg.get_array(key).map(Vec::as_slice).unwrap_or_default());

    let ui_w = ui.clone();
    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            c.set_path(&kw, ui_w.to_value());
        }));

    let ui_l = ui.clone();
    let kl = key.to_string();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            ui_l.seed(c.get_array(&kl).map(Vec::as_slice).unwrap_or_default());
        }),
    );

    let ui_g = ui.clone();
    state
        .fields
        .getters
        .borrow_mut()
        .insert(key.to_string(), Box::new(move || ui_g.to_value()));

    let visible = ui.visible;
    register_dialog(state, build_comment_dicts_dialog(&ui));
    visible
}

/// 自定义按钮的配置键。与 `ui.toolbar.items` 是**同一个功能的两半**，故在同一处读写。
const BUTTONS_KEY: &str = "ui.toolbar.buttons";

/// 读 `ui.toolbar.buttons`（缺省/非数组 → 空表）。
fn read_buttons(c: &Config) -> Vec<crate::dialogs::toolbar_items_dialog::ButtonRow> {
    use crate::dialogs::toolbar_items_dialog::ButtonRow;
    let s = |v: &Value, k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    c.get_array(BUTTONS_KEY)
        .map(|a| {
            a.iter()
                // 无 id 的条目**跳过**：它无法被 `custom:<id>` 引用，界面上是一行点不到
                // 任何东西的死行。写回时它会作为外来条目被 merge 保住吗？不会——buttons
                // 是整表覆盖。故此处跳过 = 该条目会在下次保存时消失，这是对的：
                // 一个引用不到的按钮定义本就没有意义。
                .filter(|v| !s(v, "id").is_empty())
                .map(|v| ButtonRow {
                    id: s(v, "id"),
                    label: s(v, "label"),
                    action: s(v, "action"),
                    // 缺省 true，与 core 的 `default_true` 同源。
                    enabled: v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 界面的按钮列表 → 配置数组。
///
/// 四个字段**全写**，包括 `enabled`。
///
/// ⚠️ 这里曾**刻意不写** `enabled`，理由是「界面用『在不在 items 里』表达显示与否，
/// 再写一个就是两个真相源」。那条理由只说明**本屏不该去改它**，不等于可以把它丢掉：
/// 写回是重建整个对象，而 core 的 `ToolbarButtonSpec::enabled` 没有
/// `skip_serializing_if`、配置里恒有这一位 ⇒ 少写一个字段，写回值就与 base 不等，
/// `has_unsaved_changes` 恒为真（用户只要有一个自定义按钮，打开设置什么都不动、
/// 关窗也被拦一次；2026-08-27 真机现场）。值原样从 `ButtonRow` 带过来，本屏不解释它。
fn buttons_to_json(rows: &[crate::dialogs::toolbar_items_dialog::ButtonRow]) -> Vec<Value> {
    rows.iter()
        .map(|b| {
            let mut m = serde_json::Map::new();
            m.insert("id".into(), Value::String(b.id.clone()));
            m.insert("label".into(), Value::String(b.label.clone()));
            m.insert("action".into(), Value::String(b.action.clone()));
            m.insert("enabled".into(), Value::Bool(b.enabled));
            Value::Object(m)
        })
        .collect()
}

#[cfg(test)]
mod toolbar_buttons_round_trip_tests {
    use super::*;

    /// ★★★ 自定义按钮 **读进来再写回去必须逐字段等价**。
    ///
    /// 写回是「按 `ButtonRow` 重建整个对象」，于是 core 结构体里任何一个本屏没带上的
    /// 字段都会被抹掉。后果不只是丢数据——`has_unsaved_changes` 把 writer 的产物与 base
    /// 比 diff，少一个字段就恒判脏：**用户只要有一个自定义按钮，打开设置什么都不动、
    /// 关窗也被拦一次**（2026-08-27 真机现场，`enabled` 就是这么丢的）。
    ///
    /// ⚠️ mock 配置里 `ui.toolbar.buttons` 是**空数组**（与出厂一致），所以
    /// `untouched_settings_produce_no_diff` 那条整体守门**测不到这一段**——必须显式造数据。
    #[test]
    fn reading_and_writing_buttons_is_lossless() {
        let cfg = crate::config::Config::from_value(serde_json::json!({
            "ui": { "toolbar": { "buttons": [
                // 反斜杠原样保留：action 是 cmdbar 表达式，路径里的转义由那一层负责。
                { "id": "sym", "label": "符", "action": "proc.run(\"D:\\a\\b.exe\")", "enabled": true },
                { "id": "off", "label": "关", "action": "open(\"https://example.com\")", "enabled": false },
            ] } }
        }));
        let src = cfg.get_array(BUTTONS_KEY).cloned().unwrap_or_default();
        let back = buttons_to_json(&read_buttons(&cfg));
        assert_eq!(
            Value::Array(back),
            Value::Array(src),
            "按钮读写不等价 ⇒ 保存会吃掉字段，且打开设置就恒判「有未保存的更改」"
        );
    }

    /// 缺 `enabled` 的旧配置读成 true（与 core 的 `default_true` 同源），不是 false。
    #[test]
    fn missing_enabled_reads_as_true() {
        let cfg = crate::config::Config::from_value(serde_json::json!({
            "ui": { "toolbar": { "buttons": [{ "id": "a", "label": "A", "action": "x" }] } }
        }));
        assert!(read_buttons(&cfg)[0].enabled);
    }
}

/// 把配置里的 `items` 摊进界面状态（开关 + 行序）。载入与首次构建共用，避免两套算法。
fn apply_items_to_ui(
    ui: &crate::dialogs::toolbar_items_dialog::ToolbarItemsUi,
    cur: &[String],
    declared: &[String],
    buttons: &[crate::dialogs::toolbar_items_dialog::ButtonRow],
) {
    use crate::dialogs::toolbar_items_dialog::custom_value;
    use crate::mix_members::managed_order;

    let mut managed = declared.to_vec();
    managed.extend(buttons.iter().map(|b| custom_value(&b.id)));
    // 开关看**有没有前缀**：`enabled_of` 只认原样出现的本名。旧配置（无前缀、只列启用项）
    // 照常工作——没列出的判为关，位置由 `managed_order` 补在声明序位。
    ui.switches.borrow_mut().clear();
    for v in &managed {
        ui.switch_of(v, enabled_of(cur, v));
    }
    let seq: Vec<String> = cur.iter().map(|s| strip_off(s).to_string()).collect();
    ui.order.set(managed_order(&seq, &managed));
}

/// 引导键控件（`dialog_button_key_action`）：勾选若干符号键，绑到 `item.verb` 指定的动词上。
///
/// 五c 收编后，五处 `trigger_keys` 的唯一落点是全局 `keys.key_actions`（core 的
/// `Config::normalize` 会折算并清空原字段）。故本控件读写的是那张 Map，而 `item.key`
/// （如 `input.temp_pinyin.trigger_keys`）退化为**标识符**——只用于 capability 校验与
/// loader 注册，不再是写入路径。`dialog_button_mix_trigger` 早就是这个形态
/// （key 指 `schema.mix_modes`，实际改的是数组里某项的字段），此处只是把它泛化。
///
/// 替代了原先按 `schema.mix_modes[quick_mix].trigger_keys` 读写的专用实现：那个字段
/// 现在恒为空，继续读写它就是「勾了不生效」。
const KEY_ACTIONS_PATH: &str = "keys.key_actions";

/// 内置「快捷」融合的实例 id。引导键之外的字段（members / layout / 显示名等）仍按
/// id 定位 `schema.mix_modes` 数组里的那一项——收编只动引导键，不动实例定义。
const QUICK_MIX_ID: &str = "quick_mix";

/// 从 `keys.key_actions` 里取出绑到 `verb` 的全部键名。
fn key_action_keys_of(cfg: &Config, verb: &str) -> Vec<String> {
    cfg.get_path(KEY_ACTIONS_PATH)
        .and_then(|v| v.as_object())
        .map(|o| {
            o.iter()
                .filter(|(_, v)| v.as_str() == Some(verb))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// 本控件「恢复默认」该勾上哪些键 = **core 的出厂值**，从 capability 默认里取。
///
/// ⚠️ **不能问 `keys.key_actions` 的 capability 默认**，那里恒是 `{}`。五处 `trigger_keys`
/// 是在 core 的 `normalize()` 里折算进 `key_actions` 的，而 capability 快照取自
/// `Config::system_preset_value`（`Config::default()` ⊕ `data/config.toml`）——**它不跑
/// `normalize()`** ⇒ 出厂值只在**被折算的那一侧**看得见。
///
/// 出厂值刻意留在被折算侧还有第二个理由（见 core `default_mix_modes` 的注释）：直接写进
/// `key_actions` 的话，「用户清空」与「从没配过」就同形了，清空的意图会被静默丢掉。
///
/// 两种形态，按元素类型分：
/// - `strlist`（`input.temp_pinyin.trigger_keys`）：默认值本身就是键名数组。
/// - `structlist`（`schema.mix_modes`）：按**动词冒号后半段**（实例 id，`mix:quick_mix`
///   → `quick_mix`）在数组里定位那一项，取它的 `trigger_keys`。
fn key_action_defaults(state: &AppState, item_key: &str, verb: &str) -> Vec<String> {
    key_action_defaults_from(&state.capabilities.default_for(item_key), verb)
}

/// [`key_action_defaults`] 的纯逻辑部分（入参是 capability 默认值本身）。
/// 拆开只为可测：取 capability 要 `AppState`，而这段判据才是会出错的地方。
fn key_action_defaults_from(def: &Value, verb: &str) -> Vec<String> {
    let Some(list) = def.as_array() else {
        return Vec::new();
    };
    let strs = |a: &[Value]| -> Vec<String> {
        a.iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect()
    };
    // 空数组走这一支（`all` 对空恒真），结果为空——临英出厂就是不绑，那是正确答案。
    if list.iter().all(|x| x.is_string()) {
        return strs(list);
    }
    let id = verb.rsplit(':').next().unwrap_or_default();
    list.iter()
        .find(|e| e.get("id").and_then(|v| v.as_str()) == Some(id))
        .and_then(|e| e.get("trigger_keys"))
        .and_then(|v| v.as_array())
        .map(|a| strs(a))
        .unwrap_or_default()
}

/// 勾选状态的版本号，供 `reactive_effect` 判变。
///
/// 位序即 options 序，故只在选项数 ≤ 64 时成立——引导键控件出厂 9 个符号键，清单里再加
/// 也到不了那个量级。真超了会退化成「高位不参与判变」，表现是最后几个键勾了不生效，
/// 故用 `debug_assert` 把它挡在开发期而不是静默截断。
fn checked_mask(sigs: &[(String, Signal<bool>)]) -> u64 {
    debug_assert!(
        sigs.len() <= 64,
        "引导键选项超过 64 个，位掩码判变会漏掉高位"
    );
    sigs.iter()
        .take(64)
        .enumerate()
        .fold(0u64, |acc, (i, (_, s))| acc | (u64::from(s.get()) << i))
}

fn build_dialog_button_key_action(state: &AppState, item: &MItem, cfg: &Config) -> Signal<bool> {
    use crate::dialogs::field_dialogs::build_pairs_dialog;

    // verb 缺失是清单写错（新控件必须声明动词）。回落成空串会把所有键绑到 ""，
    // 那是一条 core 侧解析成 None 的死绑定——宁可什么都不绑。
    let Some(verb) = item.verb.clone() else {
        tracing::warn!(
            "清单项 {} 用了 dialog_button_key_action 却没声明 verb，跳过",
            item.key
        );
        return signal(false);
    };

    // 播种源 = **唯一真相源** `SchemaManagerState::key_actions`，不是 cfg。
    //
    // 本控件与全局「自定义按键」编辑器改的是同一张表（五c 收编后引导键的唯一落点就是
    // `keys.key_actions`），此前两边各持一份副本、各注册一个 writer：全局那个整表覆盖，
    // 本控件按动词切片覆盖（`retain(v != verb)` + 插回勾选项）。注册顺序上本控件在后
    // （manifest 这批 writer 建在 `shell::build` 里，晚于 `register_schema_registrations`），
    // 于是在全局表里删掉 `` ` `` → 临时拼音后保存，本控件那份「启动时播种、之后再没更新过」
    // 的旧快照原样把它写回去 ⇒ diff 恒为 0 ⇒ 提示「没有变更」，而绑定确实还在。
    //
    // ⚠️ 守门测试 `at_most_one_writer_overwrites_key_actions_wholesale` 曾放行这三个控件：
    // 它的哨兵是一个**不属于任何动词**的键，切片覆盖者不碰它。判据因此升级为「至多一个
    // **覆盖式**写入者」——判据是它会不会删掉不是自己写进去的条目，与覆盖范围无关。
    // 见 `no_slice_writer_overwrites_key_actions`。
    //
    // 注意：cfg 可能是 AppState::empty() 的占位配置（shell::build 先于异步 fetch 完成），
    // 此时表还是空的。loader/effect 必须**无条件注册**——否则等真实配置到达、loader 本该
    // 被 apply_loaded 调用去刷新 Signal 时，这个字段根本没登记过，永远显示/保存不出真实值。
    let cur = if state.schema_mgr.key_actions_loaded.get() {
        state.schema_mgr.keys_bound_to(&verb)
    } else {
        // 构造期那次同步 RPC 失败（离线 / 服务未就绪）时回落 cfg。它同样可能是空的，
        // 由 loader 在真实配置到达时补齐。
        key_action_keys_of(cfg, &verb)
    };
    let opts = item.options.clone();
    let sigs: Vec<(String, Signal<bool>)> = opts
        .iter()
        .map(|o| (o.value.clone(), signal(cur.contains(&o.value))))
        .collect();
    let visible = signal(false);

    // ⛔ **本控件不再注册 writer**，理由同全局编辑器（见 `build_dialog_button_key_actions`）：
    // 它是视图，不是存储。编辑经下面的 effect 同步进 `schema_mgr.key_actions`，由那一个
    // writer 统一落盘。

    // loader：注册在**清单项 key** 下（每项一个 verb，key 各不相同，互不覆盖）。
    let sigs_l = sigs.clone();
    let verb_l = verb.clone();
    state.fields.loaders.borrow_mut().insert(
        item.key.clone(),
        Box::new(move |c: &Config| {
            let cur = key_action_keys_of(c, &verb_l);
            for (v, s) in &sigs_l {
                s.set(cur.iter().any(|k| k == v));
            }
        }),
    );

    // ── 视图 ↔ 真相源的双向同步 ──
    //
    // 两个 effect 而不是一个：本控件与全局编辑器是同一份数据的两个视图，任一边改了另一边
    // 的副本都会过期。回灌管「本控件改了要落到真相源」，播种管「别处改了本控件要看得见」。
    // 少了后者，在全局表里删掉 `` ` `` 之后再来这里勾一个别的键，本控件那份仍勾着 `` ` ``
    // 的旧快照就会把它带回去——与修复前同形，只是触发路径长了一步。
    {
        // 回灌：勾选变化 → 真相源。
        //
        // ★ 只摘掉**本控件列得出来的那些键**，而不是所有指向本动词的键。全局编辑器的键名
        // 值域宽得多，用户完全可以在那边把本动词绑到 options 之外的键上（`f9` 之类）；
        // 一律 `retain(v != verb)` 会把它静默删掉——本控件管不着的绑定不该由它裁决。
        let owned: Vec<String> = opts.iter().map(|o| o.value.clone()).collect();
        let sigs_e = sigs.clone();
        let verb_e = verb.clone();
        let mgr = state.schema_mgr.clone();
        let dep = sigs.clone();
        let sync = crate::pages::dict::reactive::reactive_effect(
            move || checked_mask(&dep),
            move || {
                // 真相源还没成功读到过就不动它：那时它是兜底空表，回灌等于拿控件初值
                // （全 false）去覆盖，与 `state.rs` 那个 writer 的 `loaded` 闸同一个理由。
                if !mgr.key_actions_loaded.get() {
                    return;
                }
                let mut map = mgr.key_actions.borrow_mut();
                map.retain(|k, v| v.as_str() != verb_e.as_str() || !owned.contains(k));
                for (k, s) in &sigs_e {
                    if s.get() {
                        map.insert(k.clone(), verb_e.clone());
                    }
                }
            },
        );
        register_dialog(state, sync);
    }
    {
        // 播种：对话框每次打开时从真相源重取。
        //
        // 挂在 `visible` 上而不是「关闭时回写」：`build_pairs_dialog` 有多条关闭路径
        // （底栏 / ESC / 标题栏），逐个挂等于赌自己数全了——同全局编辑器那几个 effect。
        let sigs_s = sigs.clone();
        let verb_s = verb.clone();
        let mgr = state.schema_mgr.clone();
        let seed = crate::pages::dict::reactive::reactive_effect(
            move || u64::from(visible.get()),
            move || {
                if !visible.get() || !mgr.key_actions_loaded.get() {
                    return;
                }
                let cur = mgr.keys_bound_to(&verb_s);
                for (k, s) in &sigs_s {
                    s.set(cur.contains(k));
                }
            },
        );
        register_dialog(state, seed);
    }

    let labels: Vec<String> = opts.iter().map(|o| o.label.clone()).collect();
    let hint = item.hint.clone().unwrap_or_default();
    // 「恢复默认」= 勾回 core 的出厂绑定，见 [`key_action_defaults`]。
    //
    // 原先传的是 `Vec::new()`（＝全部取消勾选），理由写的是「出厂默认值由 core 的
    // data/config.toml 决定，设置端不再各存一份」。**前半句对、后半句的结论走反了**：
    // 「不各存一份」的解法是去读 core 那一份，不是一份都不读。而且这三项的出厂值恰恰
    // **不在** data/config.toml 里（`schema.mix_modes` 刻意不写进预置文件）。
    //
    // 实际后果：快捷输入点一次「恢复默认」，出厂的 `;` 被清空 ⇒ 功能整个关掉，而
    // 「清空 trigger_keys」正是 core 认定的「禁用快捷输入」。临拼的 `` ` `` 同理。
    let defaults = key_action_defaults(state, &item.key, &verb);
    let dlg = build_pairs_dialog(visible, &item.label, &hint, sigs, labels, defaults);
    register_dialog(state, dlg);
    visible
}

/// dialog_button_mix_members：条目开关式编辑内置「快捷」融合的候选来源（core 侧 `members`）。
///
/// core 把 members 当作候选来源的单一真相源——**有无即开关、顺序即优先级**。界面两维都管：
/// 一列开关决定有无，拖拽手柄决定顺序（`order` 信号是行序的真相源）。写回经
/// `patch_mix_mode_members`，界面不认识的成员按锚点留在原处，不被冲到末尾。
///
/// 清单项用**视图 key**（`schema.mix_modes#members`）与触发键项区分——`Manifest.items`
/// 按 key 索引，同 key 两项后者会覆盖前者。写回路径仍是真实的 `schema.mix_modes`。
fn build_dialog_button_mix_members(
    state: &AppState,
    view_key: &str,
    item: &MItem,
    cfg: &Config,
) -> Signal<bool> {
    use crate::dialogs::field_dialogs::{ToggleRow, build_toggle_rows_dialog};
    use crate::mix_members::{managed_order, mix_mode_members, patch_mix_mode_members};

    let mix_modes_path = "schema.mix_modes";
    let managed: Vec<String> = item.options.iter().map(|o| o.value.clone()).collect();
    // 与触发键控件同理：cfg 可能是 AppState::empty() 的占位配置，writer/loader 须无条件注册。
    let cur_arr: Vec<Value> = cfg.get_array(mix_modes_path).cloned().unwrap_or_default();
    let cur = mix_mode_members(&cur_arr, QUICK_MIX_ID).unwrap_or_else(|| managed.clone());

    let rows: Vec<ToggleRow> = item
        .options
        .iter()
        .map(|o| ToggleRow {
            value: o.value.clone(),
            label: o.label.clone(),
            desc: o.hint.clone(),
            sig: signal(cur.contains(&o.value)),
        })
        .collect();
    let sigs: Vec<(String, Signal<bool>)> = rows.iter().map(|r| (r.value.clone(), r.sig)).collect();
    // 行序按**当前配置**排（未启用的成员补在声明序位），而不是恒按声明序——
    // 否则用户手写的优先级在他还没动任何东西时就"看起来"被改过了。
    let order = signal(managed_order(&cur, &managed));
    let visible = signal(false);

    let sigs_w = sigs.clone();
    let managed_w = managed.clone();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            // 顺序取自 order（界面行序），有无取自各行开关：两者相乘才是 members。
            let ordered: Vec<String> = order
                .get()
                .into_iter()
                .filter(|v| {
                    sigs_w
                        .iter()
                        .find(|(x, _)| x == v)
                        .is_some_and(|(_, s)| s.get())
                })
                .collect();
            let arr = c.get_array(mix_modes_path).cloned().unwrap_or_default();
            if let Some(patched) = patch_mix_mode_members(&arr, QUICK_MIX_ID, &managed_w, &ordered)
            {
                c.set_path(mix_modes_path, Value::Array(patched));
            }
        }));

    // loader 注册在视图 key 下：`reset_current_page` 按清单 key 反查，用真实路径会与
    // 触发键项互相覆盖。
    let sigs_l = sigs.clone();
    let managed_l = managed.clone();
    state.fields.loaders.borrow_mut().insert(
        view_key.to_string(),
        Box::new(move |c: &Config| {
            let arr = c.get_array(mix_modes_path).cloned().unwrap_or_default();
            // 找不到 quick_mix（空配置/恢复默认）时回落「全开」，与 core 默认成员一致。
            let cur = mix_mode_members(&arr, QUICK_MIX_ID).unwrap_or_else(|| managed_l.clone());
            for (v, s) in &sigs_l {
                s.set(cur.iter().any(|m| m == v));
            }
            // 顺序同样要跟着配置回位——只复位开关会把上次拖出来的优先级留在界面上，
            // 与刚载入的配置不符，保存时又会把这个错顺序写回去。
            order.set(managed_order(&cur, &managed_l));
        }),
    );

    let hint = item.hint.clone().unwrap_or_default();
    // 不设下限（0）：**刻意保持既有行为**，不顺手改这个控件。快捷输入把候选来源全关
    // 是否该拦，是那个功能自己的判断，与工具栏「全关会回落成全显示」的处境不同。
    let dlg = build_toggle_rows_dialog(visible, &item.label, &hint, rows, managed, order, None);
    register_dialog(state, dlg);
    visible
}

/// 内置 quick_mix 的候选布局下拉。
///
/// `schema.mix_modes` 是整体读写的 structlist，实际要改的是数组里 `id == quick_mix`
/// 那项的 `candidate_layout`（core 侧是每实例的三态字段）。与触发键、候选来源两项同构，
/// 清单项用**视图 key** `schema.mix_modes#candidate_layout` 区分——`Manifest.items`
/// 按 key 索引，同 key 多项后者会覆盖前者。写回路径仍是真实的 `schema.mix_modes`。
///
/// 不能走通用 [`build_select`]：那个直接 `cfg.get_str(key)` / `set_path(key, ...)`，
/// 用在这里会把整个 mix_modes 数组换成一个字符串。
fn build_select_mix_layout(
    state: &AppState,
    view_key: &str,
    item: &MItem,
    cfg: &Config,
) -> Element {
    use crate::mix_fields::{LAYOUT_FALLBACK, mix_mode_layout, patch_mix_mode_layout};

    let mix_modes_path = "schema.mix_modes";
    let (values, labels): (Vec<String>, Vec<String>) = item
        .options
        .iter()
        .map(|o| (o.value.clone(), o.label.clone()))
        .unzip();
    if values.is_empty() {
        return Element::label("（无可选项）").fg_role(Role::TextMuted);
    }
    // 与触发键/候选来源控件同理：cfg 可能是 AppState::empty() 的占位配置，
    // writer/loader 须无条件注册（否则空配置构建的这一页永远写不回）。
    let cur_arr: Vec<Value> = cfg.get_array(mix_modes_path).cloned().unwrap_or_default();
    let cur =
        mix_mode_layout(&cur_arr, QUICK_MIX_ID).unwrap_or_else(|| LAYOUT_FALLBACK.to_string());
    let sig = signal(values.iter().position(|v| *v == cur).unwrap_or(0));

    let vals_w = values.clone();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            let Some(v) = vals_w.get(sig.get()) else {
                return;
            };
            let arr = c.get_array(mix_modes_path).cloned().unwrap_or_default();
            if let Some(patched) = patch_mix_mode_layout(&arr, QUICK_MIX_ID, v) {
                c.set_path(mix_modes_path, Value::Array(patched));
            }
        }));

    // loader 注册在视图 key 下：`reset_current_page` 按清单 key 反查，
    // 用真实路径会与触发键/候选来源两项互相覆盖。
    let vals_l = values.clone();
    state.fields.loaders.borrow_mut().insert(
        view_key.to_string(),
        Box::new(move |c: &Config| {
            let arr = c.get_array(mix_modes_path).cloned().unwrap_or_default();
            let cur =
                mix_mode_layout(&arr, QUICK_MIX_ID).unwrap_or_else(|| LAYOUT_FALLBACK.to_string());
            sig.set(vals_l.iter().position(|v| *v == cur).unwrap_or(0));
        }),
    );

    let dd = Element::dropdown(labels, sig);
    match item.width {
        Some(w) => dd.width(w as i32),
        None => dd.min_width(CTRL_W),
    }
}

/// 内置 quick_mix 的自由（字面）输入下拉（`free_input` 三态）。
///
/// 与 [`build_select_mix_layout`] 同构——整体读写 structlist + 按 id 打补丁，清单项走
/// 视图 key `schema.mix_modes#free_input`。
///
/// ★ **必须注册 getter**：下面那个 `free_input_takes_select_keys` 项用 `enabled_when`
/// 引用本项（按配置字段名指代，不按界面标签——标签会改，字段名不会），而
/// `enabled_when_closure` 是从 `fields.getters` 取值的，取不到就 `unwrap_or(true)`
/// ——条件恒满足、联动**静默失效**（既不报错也不锁死，只是看着像没接）。
/// 布局那项没有 getter，因为没人引用它；本项有人引用，就必须有。
fn build_select_mix_free_input(
    state: &AppState,
    view_key: &str,
    item: &MItem,
    cfg: &Config,
) -> Element {
    use crate::mix_fields::{FIELD_FREE_INPUT, FREE_INPUT_FALLBACK};
    build_mix_field_select(
        state,
        view_key,
        item,
        cfg,
        FIELD_FREE_INPUT,
        FREE_INPUT_FALLBACK,
    )
}

/// mix 实例某个**字符串字段**的下拉控件（按 id 打补丁），供 `free_input` 等复用。
fn build_mix_field_select(
    state: &AppState,
    view_key: &str,
    item: &MItem,
    cfg: &Config,
    field: &'static str,
    fallback: &'static str,
) -> Element {
    use crate::mix_fields::{mix_mode_field_str, patch_mix_mode_field};

    let mix_modes_path = "schema.mix_modes";
    let (values, labels): (Vec<String>, Vec<String>) = item
        .options
        .iter()
        .map(|o| (o.value.clone(), o.label.clone()))
        .unzip();
    if values.is_empty() {
        return Element::label("（无可选项）").fg_role(Role::TextMuted);
    }
    // 与布局控件同理：cfg 可能是 AppState::empty() 的占位配置，writer/loader 须无条件注册。
    let cur_arr: Vec<Value> = cfg.get_array(mix_modes_path).cloned().unwrap_or_default();
    let cur = mix_mode_field_str(&cur_arr, QUICK_MIX_ID, field, fallback)
        .unwrap_or_else(|| fallback.to_string());
    let sig = signal(values.iter().position(|v| *v == cur).unwrap_or(0));

    let vals_w = values.clone();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            let Some(v) = vals_w.get(sig.get()) else {
                return;
            };
            let arr = c.get_array(mix_modes_path).cloned().unwrap_or_default();
            if let Some(patched) =
                patch_mix_mode_field(&arr, QUICK_MIX_ID, field, Value::String(v.clone()))
            {
                c.set_path(mix_modes_path, Value::Array(patched));
            }
        }));

    // loader 注册在视图 key 下（同布局项：`reset_current_page` 按清单 key 反查，
    // 用真实路径会与同键的其它视图项互相覆盖）。
    let vals_l = values.clone();
    state.fields.loaders.borrow_mut().insert(
        view_key.to_string(),
        Box::new(move |c: &Config| {
            let arr = c.get_array(mix_modes_path).cloned().unwrap_or_default();
            let cur = mix_mode_field_str(&arr, QUICK_MIX_ID, field, fallback)
                .unwrap_or_else(|| fallback.to_string());
            sig.set(vals_l.iter().position(|v| *v == cur).unwrap_or(0));
        }),
    );

    // getter 也注册在视图 key 下——enabled_when 表达式里写的正是视图 key。
    let vals_g = values;
    state.fields.getters.borrow_mut().insert(
        view_key.to_string(),
        Box::new(move || {
            vals_g
                .get(sig.get())
                .map(|v| Value::String(v.clone()))
                .unwrap_or(Value::Null)
        }),
    );

    let dd = Element::dropdown(labels, sig);
    match item.width {
        Some(w) => dd.width(w as i32),
        None => dd.min_width(CTRL_W),
    }
}

/// 内置 quick_mix 的 `free_input_takes_select_keys` 开关：自由输入时把二三候选键
/// （默认 `;` `'`）当作字面字符。
///
/// 不能走通用 [`build_toggle`]：那个直接 `cfg.get_bool(key)` / `set_path(key, …)`，
/// 用在这里会把整个 mix_modes 数组换成一个布尔。
fn build_checkbox_mix_free_select_keys(state: &AppState, view_key: &str, cfg: &Config) -> Element {
    use crate::mix_fields::{
        FIELD_FREE_INPUT_TAKES_SELECT_KEYS, FREE_INPUT_TAKES_SELECT_KEYS_FALLBACK,
        mix_mode_field_bool, patch_mix_mode_field,
    };

    let mix_modes_path = "schema.mix_modes";
    let field = FIELD_FREE_INPUT_TAKES_SELECT_KEYS;
    let fallback = FREE_INPUT_TAKES_SELECT_KEYS_FALLBACK;

    let cur_arr: Vec<Value> = cfg.get_array(mix_modes_path).cloned().unwrap_or_default();
    let sig =
        signal(mix_mode_field_bool(&cur_arr, QUICK_MIX_ID, field, fallback).unwrap_or(fallback));

    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            let arr = c.get_array(mix_modes_path).cloned().unwrap_or_default();
            if let Some(patched) =
                patch_mix_mode_field(&arr, QUICK_MIX_ID, field, Value::Bool(sig.get()))
            {
                c.set_path(mix_modes_path, Value::Array(patched));
            }
        }));

    state.fields.loaders.borrow_mut().insert(
        view_key.to_string(),
        Box::new(move |c: &Config| {
            let arr = c.get_array(mix_modes_path).cloned().unwrap_or_default();
            sig.set(mix_mode_field_bool(&arr, QUICK_MIX_ID, field, fallback).unwrap_or(fallback));
        }),
    );
    state.fields.getters.borrow_mut().insert(
        view_key.to_string(),
        Box::new(move || Value::Bool(sig.get())),
    );

    Element::switch(sig)
}

/// dialog_button（map）：键值对 text_input 降级实现。挂对话框到根，返回 visible。
fn build_dialog_button_map(
    state: &AppState,
    key: &str,
    item: &MItem,
    cfg: &Config,
) -> Signal<bool> {
    use crate::dialogs::field_dialogs::{build_map_dialog, map_to_rows, rows_to_map};

    let def_map = default_map(state, key);
    let cur_map = cfg
        .get_path(key)
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_else(|| def_map.clone());
    let text = signal(map_to_rows(&cur_map));
    let visible = signal(false);

    // writer
    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            let m = rows_to_map(&text.get());
            c.set_path(&kw, Value::Object(m));
        }));

    // loader（缺省回落 capability 默认）
    let kl = key.to_string();
    let def_l = def_map.clone();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let m = c
                .get_path(&kl)
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_else(|| def_l.clone());
            text.set(map_to_rows(&m));
        }),
    );

    // 恢复默认
    let def_text = map_to_rows(&def_map);

    let hint = item.hint.clone().unwrap_or_default();
    let dlg = build_map_dialog(visible, &item.label, &hint, text, def_text);
    register_dialog(state, dlg);
    visible
}

/// 字体相关的两项（`ui.font.fallback` / `ui.font.scripts`）：按**行**切分的编辑对话框。
///
/// 一个函数管两个键，是因为它们的差别只有编解码与提示语；分成两个函数会让
/// writer/loader/恢复默认这三处结构重复一遍，而那三处正是漏一处就静默失效的地方。
///
/// ⚠️ 两者都**不能**用通用兜底，理由见 `build_dialog_body` 的分派注释与
/// `field_dialogs` 里那段字体编解码的说明。
fn build_dialog_button_font(
    state: &AppState,
    key: &str,
    item: &MItem,
    cfg: &Config,
) -> Signal<bool> {
    use crate::dialogs::field_dialogs::{
        build_text_rows_dialog_with, font_list_to_rows, font_scripts_to_rows, rows_to_font_list,
        rows_to_font_scripts,
    };
    let is_scripts = key == "ui.font.scripts";
    // 类别名的值域取自 core 的 capability，**不在这里手抄一份**：抄本会滞后，
    // core 加了新类别而设置页永远不列出来（而且没有任何测试拦得住）。
    // 空 Vec（快照缺该字段）时 `font_scripts_to_rows` 自动退回「只列已配的」。
    let known: Vec<String> = state
        .capabilities
        .entries
        .get("ui.font.scripts")
        .map(|e| e.values.clone())
        .unwrap_or_default();
    let encode = move |v: Option<&Value>| -> String {
        if is_scripts {
            let empty = serde_json::Map::new();
            font_scripts_to_rows(v.and_then(|v| v.as_object()).unwrap_or(&empty), &known)
        } else {
            font_list_to_rows(v)
        }
    };
    let text = signal(encode(cfg.get_path(key)));
    let visible = signal(false);

    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            let v = if is_scripts {
                Value::Object(rows_to_font_scripts(&text.get()))
            } else {
                Value::Array(rows_to_font_list(&text.get()))
            };
            c.set_path(&kw, v);
        }));

    let kl = key.to_string();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            text.set(encode(c.get_path(&kl)));
        }),
    );

    // 出厂默认两者都是空（不覆盖），故「恢复默认」就是清空。
    let hint = item.hint.clone().unwrap_or_default();
    // ⚠️ 说明区是 `max_lines(4)`，超出部分**静默截断**且不加省略号——本轮截图自查里
    // 这句原本写全了七个类别名，结果显示成「类别可填 latin / greek /」就没了。
    // 面向用户的文案要按「四行能装下」来写，不能按「说全了才对」来写。
    // 类别名不再列在说明里：七个类别已经**预填成行**，框里那份就是权威且不会过期的清单，
    // 说明区再抄一遍既占掉两行、又多一处会漂的真相源。
    let format_hint = if is_scripts {
        "每行一条：类别 = 字体1, 字体2；留空的类别 = 不单独指定"
    } else {
        "每行一个字体名（字体名含空格，故按行分而不是按空格分）"
    };
    // ⚠️ 占位符必须各给各的：复用外壳会静默继承标点表的「，=，」，而这两项的行格式
    // 完全不同——用户照着占位符写会得到一份空配置。它不进任何数据流，测试断言不到，
    // 只有 `--screenshot` 看得见（本轮正是这么发现的）。
    let placeholder = if is_scripts {
        "latin = Segoe UI"
    } else {
        "Noto Sans Mongolian"
    };
    let dlg = build_text_rows_dialog_with(
        visible,
        &item.label,
        &hint,
        format_hint,
        placeholder,
        text,
        String::new(),
    );
    register_dialog(state, dlg);
    visible
}

/// 自定义标点默认表：(原字符展示, map 键 token, [中半, 英全, 中全, 英半])。
/// 键 token 取自参考 webview（空格=" "、双引号首/次=\"1/\"2、单引号首/次='1/'2）；
/// 数组序 = **core 存储序**（wind-config config.rs `custom_mappings` 文档 +
/// wind-punct/wind-transform lookup 列语义）：[中文半角, 英文全角, 中文全角, 英文半角]。
/// UI 表格按展示序（英半/英全/中半/中全）呈现，由 field_dialogs 的 DISPLAY_COLS 单点映射。
/// 全角空格用 \u{3000}。
///
/// Directive：本表是「展示基线」，必须逐格等同 core 内建标点默认 —— 因 writer 只保存与默认
/// 不同的行，未改动行不落盘、运行时由 core 内建默认决定行为；若 core 默认变更须同步此表，
/// 否则 UI 展示与实际行为不符且用户无从察觉。
#[rustfmt::skip]
pub(crate) const PUNCT_DEFAULTS: &[(&str, &str, [&str; 4])] = &[
    ("空格",      " ",  ["",   "\u{3000}", "\u{3000}", ""]),
    ("!",         "!",  ["！", "！", "！", "!"]),
    ("@",         "@",  ["@",  "＠", "＠", "@"]),
    ("#",         "#",  ["#",  "＃", "＃", "#"]),
    ("$",         "$",  ["￥", "＄", "￥", "$"]),
    ("%",         "%",  ["%",  "％", "％", "%"]),
    ("^",         "^",  ["……", "＾", "……", "^"]),
    ("&",         "&",  ["&",  "＆", "＆", "&"]),
    ("*",         "*",  ["*",  "＊", "＊", "*"]),
    ("(",         "(",  ["（", "（", "（", "("]),
    (")",         ")",  ["）", "）", "）", ")"]),
    ("_",         "_",  ["——", "＿", "——", "_"]),
    ("-",         "-",  ["-",  "－", "－", "-"]),
    ("+",         "+",  ["+",  "＋", "＋", "+"]),
    ("=",         "=",  ["=",  "＝", "＝", "="]),
    ("[",         "[",  ["【", "［", "【", "["]),
    ("]",         "]",  ["】", "］", "】", "]"]),
    ("{",         "{",  ["｛", "｛", "｛", "{"]),
    ("}",         "}",  ["｝", "｝", "｝", "}"]),
    ("\\",        "\\", ["、", "＼", "、", "\\"]),
    ("|",         "|",  ["|",  "｜", "｜", "|"]),
    (";",         ";",  ["；", "；", "；", ";"]),
    (":",         ":",  ["：", "：", "：", ":"]),
    ("\" 第一次", "\"1", ["“", "＂", "“", "\""]),
    ("\" 第二次", "\"2", ["”", "＂", "”", "\""]),
    ("' 第一次",  "'1", ["‘",  "＇", "‘", "'"]),
    ("' 第二次",  "'2", ["’",  "＇", "’", "'"]),
    (",",         ",",  ["，", "，", "，", ","]),
    (".",         ".",  ["。", "．", "。", "."]),
    ("<",         "<",  ["《", "＜", "《", "<"]),
    (">",         ">",  ["》", "＞", "》", ">"]),
    ("/",         "/",  ["/",  "／", "／", "/"]),
    ("?",         "?",  ["？", "？", "？", "?"]),
    ("~",         "~",  ["～", "～", "～", "~"]),
    ("`",         "`",  ["·",  "｀", "·", "`"]),
];

/// 取某行第 i 格的展示值：有覆盖值用覆盖、否则回落默认（seed 与 loader 共用，避免逻辑漂移）。
pub(crate) fn seed_cell(ov: Option<&Vec<Value>>, i: usize, def: &str) -> String {
    ov.and_then(|a| a.get(i))
        .and_then(|x| x.as_str())
        .map(String::from)
        .unwrap_or_else(|| def.to_string())
}

/// `input.punct.custom_mappings` 的配置路径。归一化与 diff 基线共用，防两处写死漂移。
pub(crate) const PUNCT_MAP_KEY: &str = "input.punct.custom_mappings";

/// 一行四格 → 该行在 `custom_mappings` 里的落点：四格全等默认即 `None`（不落盘）。
///
/// 这是「什么算自定义」的**唯一判据**，writer 与 [`punct_canonical_map`] 都问它。
/// 两处一旦分头写，就会造出「基线里有、写回时被剪」的固定差异——关窗守卫从此永远
/// 提示未保存，而用户根本没动过任何东西。
pub(crate) fn punct_row_override(vals: &[String; 4], defs: &[&str; 4]) -> Option<Value> {
    if (0..4).all(|i| vals[i].as_str() == defs[i]) {
        return None;
    }
    Some(Value::Array(
        vals.iter().cloned().map(Value::String).collect(),
    ))
}

/// 把 `custom_mappings` 归一成「未编辑时 writer 会产出的形状」：逐行按 [`seed_cell`]
/// 播种（缺格回落默认），再套 [`punct_row_override`] 的裁剪。
///
/// 存在理由是两处不对称，它们都让「什么都没改」的配置与 writer 的输出恒不相等：
///
/// 1. **显式写着默认值的条目**。core 的「等出厂默认即删」够不着 Map 内部条目——
///    `input.punct.custom_mappings.<` 不是注册表登记键，`prune_redundant` 用
///    `is_known_key` 把整个 Map 子路径排除在外（那道保险是对的：一刀切会把用户的
///    自定义标点删光）。于是手工编辑 config.toml、或 `config.applyPatch` 按合并语义
///    带进来的冗余行会永久留存，而 writer 每次都把它剪掉。
///    真实案例：`"<" = ["《","＜","《","<"]` 四格全是内置默认，写进 config.toml 后
///    设置页每次开关都提示「有未保存改动」；改成 `〈` 反而正常，因为那才算自定义。
/// 2. **`PUNCT_DEFAULTS` 之外的源字符**。writer 只遍历这张表，表外 token 一律丢弃。
///
/// 归一只作用于 diff 基线，不回写 core：冗余行留在 config.toml 里对取值没有影响
/// （查到的就是默认值），静默删掉用户手写的行反而更糟。
fn punct_canonical_map(m: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for (_, tok, defs) in PUNCT_DEFAULTS {
        let ov = m.get(*tok).and_then(|v| v.as_array());
        let vals: [String; 4] = std::array::from_fn(|i| seed_cell(ov, i, defs[i]));
        if let Some(row) = punct_row_override(&vals, defs) {
            out.insert((*tok).to_string(), row);
        }
    }
    out
}

/// 就地归一配置里的 `custom_mappings`（见 [`punct_canonical_map`]）。
///
/// 键缺失时也写入空表——writer 是**无条件** `set_path` 的，基线缺键会让 diff 拿
/// `Null` 跟 `{}` 比而恒判有变更。
pub(crate) fn canonicalize_punct_baseline(cfg: &mut Config) {
    let cur = cfg
        .get_path(PUNCT_MAP_KEY)
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    cfg.set_path(PUNCT_MAP_KEY, Value::Object(punct_canonical_map(&cur)));
}

/// 自定义序号标签槽位数（序号 1-10）。
const INDEX_LABEL_SLOTS: usize = 10;

/// 序号标签数组的第 `i` 槽 → 输入框文本（越界/非数组 → 空串 = 该槽让位主题）。
/// 播种与 loader 共用，保证「打开对话框」与「外部改动后重载」看到的是同一套值。
fn label_slot(v: &Value, i: usize) -> String {
    v.as_array()
        .and_then(|a| a.get(i))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// 序号标签收集：一框一项，**整框内容为一个标签**（不再只取首字符）——
/// `(1)`、罗马数字、带 ZWJ 的组合 emoji 都是多 char 的合法标签，按 char 切会拆散它们。
///
/// 空框**原地保留**为空串（core 侧 `user_index_label` 把空串判为「该槽让位主题」），
/// 只裁掉**尾部**连续空框：全空即得 `[]`，等于出厂默认，写回时可被 `prune_redundant` 清掉。
/// 曾经的「遇空即停」是字符串前缀形态的遗留，它让「第 1、4 槽自定义，中间走主题」根本
/// 表达不出来。全空白的框（只敲了空格）按空槽处理，其余原样保留内部空格。
///
/// writer 与 getter 共用，保持一致。
fn collect_labels(inputs: &[Signal<String>]) -> Vec<Value> {
    let mut out: Vec<String> = inputs
        .iter()
        .map(|s| {
            let v = s.get();
            if v.trim().is_empty() {
                String::new()
            } else {
                v
            }
        })
        .collect();
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out.into_iter().map(Value::String).collect()
}

/// dialog_button（keys.session_actions）：`[按键] [功能] [修改][删除]` 的行编辑器。
///
/// 与按键功能表（`dialogs::schema_key_actions`）是同族编辑器，共用 `dialogs::key_binding`
/// 里的行渲染与按键选择对话框；两者的分野见那两个模块的文档注释。
fn build_dialog_button_session_actions(state: &AppState, key: &str, cfg: &Config) -> Signal<bool> {
    use crate::dialogs::session_actions::SessionActionFields;

    let fields = SessionActionFields::default();
    fields.seed(
        &cfg.get_path(key)
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default(),
    );
    let visible = signal(false);

    let (fw, kw) = (fields.clone(), key.to_string());
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            c.set_path(&kw, Value::Object(fw.to_map()));
        }));

    // 只读组：「候选操作」那四组键组——core 把它们折算进本表，且**本表显式项优先**。
    // 不列出来的话，用户在这里配了 `tab`，外面「高亮移动按键」仍勾着 Tab，两处各说各话。
    let foreign_src = foreign_sources(state);
    fields.set_foreign(session_foreign_bindings(&foreign_src, cfg));

    // loader：外部改动（恢复默认 / 重新拉取配置）后重建行。
    let (fl, kl) = (fields.clone(), key.to_string());
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            fl.seed(
                &c.get_path(&kl)
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default(),
            );
            // 同全局那份：构造期拿到的多半是占位配置，四组键组全是空的。
            fl.set_foreign(session_foreign_bindings(&foreign_src, c));
        }),
    );

    // getter（与 writer 同一取值逻辑，供一致性校验）。
    let fg = fields.clone();
    state.fields.getters.borrow_mut().insert(
        key.to_string(),
        Box::new(move || Value::Object(fg.to_map())),
    );

    let dlg = crate::dialogs::key_binding::build_binding_dialog(
        visible,
        signal("全局自定义输入中按键".to_string()),
        fields.build_body(),
    );
    register_dialog(state, dlg);
    // 按键选择浮层排在本对话框**之后**登记 = 叠在它上面。register_dialog 是往 Vec 里推，
    // 挂到根窗口时按 Vec 序成为 stack 的子节点，先推的在下。顺序反了的表现是点「添加」
    // 之后什么都看不见——浮层建出来了，被压在编辑器下面。
    register_dialog(state, fields.build_picker());
    visible
}

/// 「去哪儿改」的位置文案：配置路径 → `分组 › 小节`。
///
/// 从清单现算而不是另写一份位置表——设置项挪了分组，只读组的指路跟着变。清单里没有的
/// 字段（如 `input.temp_pinyin.hotkey` 的直达热键项）留空：它本来也没有可去的地方。
fn foreign_sources(state: &AppState) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (path, it) in &state.manifest.items {
        let g = state
            .manifest
            .groups
            .iter()
            .find(|g| g.key == it.group)
            .map(|g| g.label.as_str())
            .unwrap_or_default();
        if !g.is_empty() && !it.section.is_empty() {
            out.insert(path.clone(), format!("{g} › {}", it.section));
        }
    }
    out
}

/// 收集「住在专用字段里、与 `keys.key_actions` 同域」的绑定，供全局编辑器的只读组。
///
/// 域的边界见 `key_conflict` 那三张表：**与本表编译进同一张热键表**的才进来。
/// `page_keys` / `select_key_groups` 那批折算进的是 `session_actions`（另一张表），
/// 它们该出现在会话态编辑器的只读组里，不往这个对话框塞。
fn foreign_key_bindings(sources: &HashMap<String, String>, cfg: &Config) -> Vec<ForeignBinding> {
    use crate::dialogs::key_binding::key_display;
    use crate::key_conflict::{
        HOTKEY_FIELDS, MODE_HOTKEY_FIELDS, TOGGLE_MODE_FIELD, canonical_hotkey, expand_hotkey,
    };

    let where_to = |path: &str| sources.get(path).cloned().unwrap_or_default();
    let mut out = Vec::new();

    // ── 修饰键那一组：值是**键名数组**，逐个翻再拼 ──
    let (tm_path, tm_label) = TOGGLE_MODE_FIELD;
    let tm: Vec<String> = cfg
        .get_array(tm_path)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(key_display)
                .collect()
        })
        .unwrap_or_default();
    if !tm.is_empty() {
        out.push(ForeignBinding {
            keys: tm.join("、"),
            label: tm_label.to_string(),
            source: where_to(tm_path),
            // 比对 token 走 expand_hotkey（与上面那张表同函数），不是显示串。
            tokens: cfg
                .get_array(tm_path)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .flat_map(expand_hotkey)
                        .collect()
                })
                .unwrap_or_default(),
            conflict: false,
        });
    }

    // ── 组合键：`canonical_hotkey` 既是比较形也是显示形（`ctrl+number` → `Ctrl+数字`）──
    //
    // ★ 模板**不展开成 10 行**。展开是冲突检测要的（`Ctrl+1` 会与它撞），但列在这里
    // 会让 14 行变成 32 行，把「控制列表长度」这条要求直接推翻。
    for (path, label) in HOTKEY_FIELDS.iter().chain(MODE_HOTKEY_FIELDS) {
        let raw = cfg.get_str(path).unwrap_or("").trim().to_string();
        if raw.is_empty() || raw.eq_ignore_ascii_case("none") {
            continue;
        }
        out.push(ForeignBinding {
            keys: canonical_hotkey(&raw),
            label: label.to_string(),
            source: where_to(path),
            // 模板在这里**展开**（`Ctrl+数字` → 十个键）：显示不展开是为了列表长度，
            // 比对不展开则会漏掉「把置顶配成 Ctrl+1」这种撞车。两者要的东西不同。
            tokens: expand_hotkey(&raw),
            conflict: false,
        });
    }
    out
}

/// 会话态编辑器（`keys.session_actions`）的只读组：「候选操作」那四组键组字段。
///
/// ★ 范围判据与全局那份同源——**与本表编译进同一张表**。这四组由 core 的
/// `effective_session_actions` 折算进 `session_actions`，且**显式项优先**：所以撞车时
/// 的语义不是「一方失效」而是「上面那条盖住了这一组里的那个键」，文案与颜色都按
/// 「需注意」而非「错误」来（见 `foreign_row`）。
///
/// ⚠️ `[keys]` 段那批组合键热键**不进这里**：它们走 key_down 表，与本表不同域。两个
/// 编辑器各自对称，谁也别往对方塞——判据一松，两个只读组就都会长成同一张大杂烩。
fn session_foreign_bindings(
    sources: &HashMap<String, String>,
    cfg: &Config,
) -> Vec<ForeignBinding> {
    use crate::key_conflict::{CANDIDATE_FIELDS, candidate_group_keys, key_symbol};

    let mut out = Vec::new();
    for (path, label) in CANDIDATE_FIELDS {
        let groups: Vec<String> = cfg
            .get_array(path)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if groups.is_empty() {
            continue;
        }
        // 显示用组符号（`minus_equal` → `- =`）：这四项在设置页本来就是按组勾选的，
        // 逐键铺开反而对不上用户在「候选操作」那一节看到的东西。
        let keys = groups
            .iter()
            .map(|g| key_symbol(g).to_string())
            .collect::<Vec<_>>()
            .join("、");
        // 比对用具体键：显式表里配的是 `minus`，与组名 `minus_equal` 字面不相等。
        let tokens: Vec<String> = groups
            .iter()
            .flat_map(|g| candidate_group_keys(g))
            .map(|k| k.to_string())
            .collect();
        out.push(ForeignBinding {
            keys,
            label: label.to_string(),
            source: sources.get(*path).cloned().unwrap_or_default(),
            tokens,
            conflict: false,
        });
    }
    out
}

/// dialog_button（keys.key_actions）：全局按键功能表，与会话态那张同族。
///
/// 复用方案级的 [`crate::dialogs::schema_key_actions::KeyActionFields`]——两处的键名值域
/// 与行为完全一致，差别只在**动词值域**（全局多一个单向 `switch_schema:<id>`）与数据来源
/// （裸 map vs 方案 cfg 的一个段）。再写一个编辑器就是两个长得不一样的键绑定编辑器，
/// 正是 `capabilities.rs` 当初把本键登记进豁免清单时想避免的事。
/// ⚠️ `cfg` **不用于播种绑定表**：那份的唯一真相源是 `SchemaManagerState::key_actions`
/// （见函数体）。它只用来读只读组那批专用字段——那些不在 `key_actions` 里，真相源
/// 就是配置本身。
fn build_dialog_button_key_actions(state: &AppState, key: &str, cfg: &Config) -> Signal<bool> {
    use crate::dialogs::schema_key_actions::KeyActionFields;

    let fields = KeyActionFields::default();
    // ⚠️ 动词表要枚举方案（`special:` / `toggle_schema:` / `switch_schema:`），而方案列表
    // 是懒加载的——用户可能从没进过方案页。空表时先拉一次，否则下拉里只剩固定动词，
    // 看起来像「这个版本不支持切方案」。
    if state.schema_mgr.schemas.borrow().is_empty() {
        state.schema_mgr.refresh();
    }
    fields.set_verbs(state.schema_mgr.build_global_key_action_verbs());
    // ★ 从**唯一真相源**播种，而不是从 cfg。
    //
    // `SchemaManagerState::key_actions` 与本控件此前是同一个配置键的两份独立副本：方案
    // 对话框（切换热键 / 引导键）改前者，本控件改后者，各自注册一个**整表覆盖**的 writer。
    // 二者按注册顺序执行，本控件排在后面 ⇒ 它那份「设置页启动时播种、之后再没更新过」的
    // 旧快照，每次保存都把方案对话框的改动原样盖回去。表现是「清除/重设方案切换热键后
    // 保存说没有变更，重开对话框那条还在」，且 diff 恒为 0（写回 current 的正是 base 的
    // 原值）。见 docs 与提交记录里的诊断链。
    fields.seed_map(
        &state
            .schema_mgr
            .key_actions
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect(),
    );
    let visible = signal(false);

    // ⛔ **本控件不再注册 writer**。它是编辑器，不是存储：编辑经下面的 effect 同步进
    // `schema_mgr.key_actions`，由那一个 writer 统一落盘。一个配置键只能有一个能整表
    // 写它的地方——否则「谁后跑谁赢」是迟早的事，而注册顺序是隐式的、没有任何东西在守。
    // 守门测试见 `no_duplicate_writer_for_key_actions`。

    // 只读组：住在专用字段里、与本表同域的绑定（左右 Shift、8 个组合键功能、置顶/删词
    // 模板…）。它们在本表里看不见，用户于是会在这里再配一个左 Shift——见 `ForeignBinding`。
    let foreign_src = foreign_sources(state);
    fields.set_foreign(foreign_key_bindings(&foreign_src, cfg));

    let (fl, kl) = (fields.clone(), key.to_string());
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            fl.seed_map(
                &c.get_path(&kl)
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default(),
            );
            // 只读组同样要跟着刷新：构造期那次拿到的多半是 `AppState::empty()` 的占位
            // 配置，热键全是空的 ⇒ 一条都列不出来。
            fl.set_foreign(foreign_key_bindings(&foreign_src, c));
        }),
    );

    // 编辑 → 唯一真相源。挂 effect 而非在对话框「关闭」上调：`build_binding_dialog` 有多条
    // 关闭路径（底栏按钮 / ESC / 标题栏），逐个挂等于赌自己数全了——与方案对话框那几个
    // effect 同一个理由、同一套写法。
    //
    // 播种也会让 rows 版本变化，从而回灌一次；因为播种源就是这张表，那次回灌是幂等的。
    // loader 触发的播种同理：它跑在 `apply_loaded` / `reload` 里，而
    // `SchemaManagerState::apply_config` 在同一批里刷新 `key_actions`，两者取值一致。
    {
        let fe = fields.clone();
        let fw = fields.clone();
        let mgr = state.schema_mgr.clone();
        let sync = crate::pages::dict::reactive::reactive_effect(
            move || fe.rows_version(),
            move || {
                *mgr.key_actions.borrow_mut() = fw
                    .to_map()
                    .into_iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                    .collect();
                mgr.key_actions_loaded.set(true);
            },
        );
        register_dialog(state, sync);
    }

    let fg = fields.clone();
    state.fields.getters.borrow_mut().insert(
        key.to_string(),
        Box::new(move || Value::Object(fg.to_map())),
    );

    let dlg = crate::dialogs::key_binding::build_binding_dialog(
        visible,
        signal("全局自定义按键".to_string()),
        fields.build_body(),
    );
    register_dialog(state, dlg);
    // 同上：选择浮层必须排在编辑器之后。
    register_dialog(state, fields.build_picker());
    visible
}

fn build_dialog_button_index_labels(state: &AppState, key: &str, cfg: &Config) -> Signal<bool> {
    use crate::dialogs::field_dialogs::build_index_labels_dialog;

    let def = state.capabilities.default_for(key);
    let cur = cfg.get_path(key).cloned().unwrap_or_else(|| def.clone());
    let inputs: Vec<Signal<String>> = (0..INDEX_LABEL_SLOTS)
        .map(|i| signal(label_slot(&cur, i)))
        .collect();
    let visible = signal(false);

    // writer：一框一项，尾部空框裁掉（见 `collect_labels`）。
    let inputs_w = inputs.clone();
    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            c.set_path(&kw, Value::Array(collect_labels(&inputs_w)));
        }));
    // loader：数组按下标回填各框（超出 10 槽的项截断、不足留空）。
    let inputs_l = inputs.clone();
    let kl = key.to_string();
    let def_l = def.clone();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let cur = c.get_path(&kl).cloned().unwrap_or_else(|| def_l.clone());
            for (i, sig) in inputs_l.iter().enumerate() {
                sig.set(label_slot(&cur, i));
            }
        }),
    );
    // getter（一致性，与 writer 同收集逻辑）。
    let inputs_g = inputs.clone();
    state.fields.getters.borrow_mut().insert(
        key.to_string(),
        Box::new(move || Value::Array(collect_labels(&inputs_g))),
    );

    let dlg = build_index_labels_dialog(visible, "自定义序号标签", inputs);
    register_dialog(state, dlg);
    visible
}

/// dialog_button（input.punct.custom_mappings）：5 列可编辑标点表格。
/// 仅把「与默认不同」的行写回 custom_mappings（map：键=原字符 token，
/// 值=[中半,英全,中全,英半]，与 core `PunctuationConverter::lookup_custom` 列序一致）。
fn build_dialog_button_punct(state: &AppState, key: &str, cfg: &Config) -> Signal<bool> {
    use crate::dialogs::field_dialogs::build_punct_table_dialog;

    let cur_map = cfg
        .get_path(key)
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    // 每行 4 个可编辑信号，从「覆盖值或默认值」播种。
    let cells: Vec<[Signal<String>; 4]> = PUNCT_DEFAULTS
        .iter()
        .map(|(_, tok, defs)| {
            let ov = cur_map.get(*tok).and_then(|v| v.as_array());
            std::array::from_fn(|i| signal(seed_cell(ov, i, defs[i])))
        })
        .collect();
    let visible = signal(false);

    // writer：仅存与默认不同的行 → custom_mappings 对象。
    let cells_w = cells.clone();
    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            let mut m = serde_json::Map::new();
            for (row, (_, tok, defs)) in cells_w.iter().zip(PUNCT_DEFAULTS.iter()) {
                let vals: [String; 4] = std::array::from_fn(|i| row[i].get());
                // 裁剪判据只此一处（见 punct_row_override）：diff 基线归一化问的是同一个
                // 函数，两边同构，「什么都没改」才可能 diff 为空。
                if let Some(arr) = punct_row_override(&vals, defs) {
                    m.insert((*tok).to_string(), arr);
                }
            }
            c.set_path(&kw, Value::Object(m));
        }));

    // loader：config 覆盖叠加到默认，回填各信号。
    let cells_l = cells.clone();
    let kl = key.to_string();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let m = c
                .get_path(&kl)
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            for (row, (_, tok, defs)) in cells_l.iter().zip(PUNCT_DEFAULTS.iter()) {
                let ov = m.get(*tok).and_then(|v| v.as_array());
                for i in 0..4 {
                    row[i].set(seed_cell(ov, i, defs[i]));
                }
            }
        }),
    );

    let src_labels: Vec<String> = PUNCT_DEFAULTS
        .iter()
        .map(|(s, _, _)| s.to_string())
        .collect();
    let defaults: Vec<[String; 4]> = PUNCT_DEFAULTS
        .iter()
        .map(|(_, _, d)| std::array::from_fn(|i| d[i].to_string()))
        .collect();
    let dlg = build_punct_table_dialog(
        visible,
        "自定义标点映射",
        src_labels,
        cells,
        defaults,
        None,
        "点单元格编辑替换字符（留空恢复该格默认）；仅与默认不同的行会被保存。",
    );
    register_dialog(state, dlg);
    visible
}

/// `enabled_when` 里单个条件的比较算子。
///
/// 原先是 `contains: bool`。加 `!=` 时没有改成第二个布尔——两个布尔能表达四种组合，
/// 其中两种（既 contains 又 ne）没有意义，枚举才是恰好三态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CondOp {
    /// `key == value`
    Eq,
    /// `key != value`
    Ne,
    /// `key contains value`（key 为字符串数组时判是否含该元素）
    Contains,
}

/// `enabled_when` 里的单个条件。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cond {
    pub(crate) key: String,
    op: CondOp,
    value: String,
}

/// 解析 `enabled_when` 表达式：以 `&&` 分隔的若干条件，**全部满足**才启用。
///
/// 支持三种比较：
/// - `key == value`：相等；`true`/`false` 按布尔解释，其余按字符串。
/// - `key != value`：不等，取 `==` 的反。
/// - `key contains value`：`key` 是字符串数组（checkbox_group 写回的 strlist）时判是否含该元素。
///
/// 任一子句语法不合法即整体返回 None（调用方按"无条件"处理，不会把控件锁死）。
///
/// ⚠️ **`!=` 必须先于 `==` 匹配**：`"a != b"` 里并不含 `==`，若先试 `==` 会落空、再落到
/// ` contains ` 也落空 → 整体返回 None → 调用方按「无条件」处理，控件**永远可用**。
/// 注意失败方向是「永远可用」而非「永远禁用」——不会有任何可见异常，从属控件只是悄悄
/// 失去联动。这条顺序由 `parse_conds_ne_is_not_swallowed_by_eq` 守着。
pub(crate) fn parse_conds(expr: &str) -> Option<Vec<Cond>> {
    let mut out = Vec::new();
    for part in expr.split("&&") {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // `!=` 在前（见函数文档的 ⚠️）；`contains` 的键名里不会出现 `==`/`!=`，不会互相误判。
        let cond = if let Some((k, v)) = part.split_once("!=") {
            Cond {
                key: k.trim().to_string(),
                op: CondOp::Ne,
                value: v.trim().to_string(),
            }
        } else if let Some((k, v)) = part.split_once("==") {
            Cond {
                key: k.trim().to_string(),
                op: CondOp::Eq,
                value: v.trim().to_string(),
            }
        } else if let Some((k, v)) = part.split_once(" contains ") {
            Cond {
                key: k.trim().to_string(),
                op: CondOp::Contains,
                value: v.trim().to_string(),
            }
        } else {
            return None;
        };
        if cond.key.is_empty() || cond.value.is_empty() {
            return None;
        }
        out.push(cond);
    }
    if out.is_empty() { None } else { Some(out) }
}

/// 判断当前值是否满足单个条件。
pub(crate) fn cond_matches(cur: &Value, c: &Cond) -> bool {
    if c.op == CondOp::Contains {
        return cur
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(c.value.as_str())));
    }
    let eq = match c.value.as_str() {
        "true" => cur.as_bool() == Some(true),
        "false" => cur.as_bool() == Some(false),
        s => cur.as_str() == Some(s),
    };
    // `!=` 就是 `==` 的反。注意取值类型不匹配时（如布尔键写了 `!= off`）`eq` 恒 false ⇒
    // `!=` 恒 true ⇒ 控件恒可用，与「未登记 key 视为满足」的宽松方向一致：拼写错误只该
    // 让联动失效，不该把控件永久锁死。
    if c.op == CondOp::Ne { !eq } else { eq }
}

/// 条件不满足时给用户看的一句原因（「需先开启「精确匹配模式」」）。
///
/// 住在本模块是因为 [`Cond`] 的 `op`/`value` 是私有字段——把它们开放出去，等于让每个
/// 调用方各写一份「怎么把条件翻译成人话」，那种文案最容易各说各的。
///
/// ★ 用途是**替代置灰**：一行控件灰着而不说为什么，用户只能挨个试上游开关。把原因直接
/// 写在控件位上，那一行就自己解释了自己。
///
/// 文案取 manifest 的 `label`（和下拉的选项 label），不给原始 key/value——用户在界面上
/// 从没见过 `single_code_input` 这种东西。
pub(crate) fn cond_reason(manifest: &Manifest, c: &Cond) -> String {
    let label = manifest
        .get(&c.key)
        .map(|i| i.label.clone())
        .unwrap_or_else(|| c.key.clone());
    // 取值的显示名：下拉项报 label，其余原样。
    let value_label = || {
        manifest
            .get(&c.key)
            .and_then(|i| i.options.iter().find(|o| o.value == c.value))
            .map(|o| o.label.clone())
            .unwrap_or_else(|| c.value.clone())
    };
    match (c.op, c.value.as_str()) {
        // 布尔四种组合合并成两句：`!= true` 与 `== false` 对用户是同一件事。
        (CondOp::Eq, "true") | (CondOp::Ne, "false") => format!("需先开启「{label}」"),
        (CondOp::Eq, "false") | (CondOp::Ne, "true") => format!("需先关闭「{label}」"),
        (CondOp::Eq, _) => format!("需「{label}」为「{}」", value_label()),
        (CondOp::Ne, _) => format!("「{label}」为「{}」时不可用", value_label()),
        (CondOp::Contains, _) => format!("需「{label}」含「{}」", value_label()),
    }
}

/// 由 `enabled_when` 表达式构造联动闭包：全部条件满足才启用。
/// 引用了未登记的 key 时该子句视为满足——拼写错误只该让联动失效，不该把控件永久锁死。
fn enabled_when_closure(state: &AppState, expr: &str) -> Option<Box<dyn Fn() -> bool>> {
    let conds = parse_conds(expr)?;
    let reg = state.fields.clone();
    Some(Box::new(move || {
        let g = reg.getters.borrow();
        conds.iter().all(|c| {
            g.get(&c.key)
                .map(|get| cond_matches(&get(), c))
                .unwrap_or(true)
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ordered_pairs 的 `-` 前缀编码 ───────────────────────────

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// ★ 编码存在的**全部理由**：关掉一项，它的位置还在。
    ///
    /// 模拟设置页的一个来回——用户把 full_width 拖到第 2 位、关掉、保存、重新载入。
    /// 若只写启用项（本编码引入前的形态），full_width 会从数组里消失，重新打开时
    /// 只能补在声明序位（第 3 位）。
    #[test]
    fn disabled_item_keeps_its_position_across_a_round_trip() {
        let order = s(&["mode", "full_width", "punct"]);
        let enabled = s(&["mode", "punct"]); // full_width 被关掉
        let stored = encode_ordered_items(&order, &enabled);
        assert_eq!(stored, s(&["mode", "-full_width", "punct"]));

        // 读回：顺序仍是三项、full_width 仍在第 2 位，只是开关为关。
        let seq: Vec<String> = stored.iter().map(|x| strip_off(x).to_string()).collect();
        assert_eq!(seq, order, "剥掉前缀后必须还原成原来的行序");
        assert!(enabled_of(&stored, "mode"));
        assert!(!enabled_of(&stored, "full_width"), "带前缀的项判为关");
        assert!(enabled_of(&stored, "punct"));
    }

    /// 全开时不产生任何前缀——出厂配置读起来仍是干净的一行。
    #[test]
    fn all_enabled_encodes_without_prefixes() {
        let order = s(&["mode", "punct"]);
        assert_eq!(encode_ordered_items(&order, &order), order);
    }

    /// **旧配置**（只列启用项、无前缀）不迁移即可正确读回：
    /// 列出的判为开，没列出的判为关。这条保证升级后用户的工具栏不变样。
    #[test]
    fn legacy_list_without_prefixes_still_reads_correctly() {
        let legacy = s(&["mode", "punct"]);
        assert!(enabled_of(&legacy, "mode"));
        assert!(enabled_of(&legacy, "punct"));
        assert!(!enabled_of(&legacy, "settings"), "没列出的就是关掉的");
        let seq: Vec<String> = legacy.iter().map(|x| strip_off(x).to_string()).collect();
        assert_eq!(seq, legacy, "无前缀时剥离是恒等的");
    }

    /// ★★ 回归：**用户手写的、界面不认识的条目，保存后必须还在**。
    ///
    /// 2026-08-26 真机踩到：用户手写了一个 `custom:sym` 自定义按钮，随后在设置页动了
    /// 一下「显示内容」，那一条当场消失。根因是 writer 整表覆盖，而 `managed_order`
    /// 只把界面管得着的条目排进行序（界面显示不了别的）。
    #[test]
    fn foreign_items_survive_a_save() {
        let managed = s(&["mode", "punct", "settings"]);
        // 用户手写的配置：custom:sym 夹在 punct 与 settings 之间。
        let current = s(&["mode", "punct", "custom:sym", "settings"]);
        // 界面这一侧只知道三个内置项（用户什么都没改）。
        let encoded = encode_ordered_items(&s(&["mode", "punct", "settings"]), &managed);

        let merged = merge_preserving_foreign(&current, &managed, &encoded);
        assert_eq!(
            merged, current,
            "用户没动过顺序时，合并必须是恒等变换（外来条目留在原位）"
        );
    }

    /// ★★ 写回的**完整取值**：用户手写的按钮 + 界面关掉一格，两件事都要正确。
    ///
    /// 这条走 `ordered_pairs_write_value`（writer 实际调用的那个），而不是分别测
    /// 两个零件——「组合对不对」是独立于「零件对不对」的一层。
    #[test]
    fn write_value_preserves_foreign_and_encodes_disabled() {
        let managed = s(&["mode", "punct", "settings"]);
        let current = s(&["mode", "punct", "custom:sym", "settings"]);
        // 界面：顺序没动，但用户关掉了 punct。
        let order = s(&["mode", "punct", "settings"]);
        let enabled = s(&["mode", "settings"]);

        let got = ordered_pairs_write_value(&order, &enabled, &managed, &current);
        assert_eq!(
            got,
            s(&["mode", "-punct", "custom:sym", "settings"]),
            "关掉的格加前缀留在原位，手写的 custom:sym 也留在原位"
        );
    }

    /// 外来条目按**锚点**落位，不被冲到末尾——它原本的相对位置也是用户的编排意图。
    #[test]
    fn foreign_item_keeps_its_anchor_after_reorder() {
        let managed = s(&["mode", "punct", "settings"]);
        let current = s(&["mode", "custom:sym", "punct", "settings"]);
        // 用户把 settings 拖到最前面。
        let encoded = s(&["settings", "mode", "punct"]);

        let merged = merge_preserving_foreign(&current, &managed, &encoded);
        // custom:sym 的锚点是「前面有 1 个受管条目」⇒ 仍插在新序列的第 1 个之后。
        assert_eq!(merged, s(&["settings", "custom:sym", "mode", "punct"]));
    }

    /// 排在所有受管条目之后的外来条目，落到末尾（锚点超出新序列长度）。
    #[test]
    fn trailing_foreign_item_stays_trailing() {
        let managed = s(&["mode", "punct"]);
        let current = s(&["mode", "punct", "custom:tail"]);
        let encoded = s(&["punct", "mode"]);
        assert_eq!(
            merge_preserving_foreign(&current, &managed, &encoded),
            s(&["punct", "mode", "custom:tail"])
        );
    }

    /// 关着的外来条目（带 `-` 前缀）同样保留，且**不被当成另一个条目**。
    ///
    /// 判定「界面认不认识」时要先剥前缀，否则 `-custom:sym` 会被视为外来项、而
    /// `custom:sym` 又在别处出现，同一个按钮会被写出两份。
    #[test]
    fn disabled_foreign_item_is_preserved_once() {
        let managed = s(&["mode", "punct"]);
        let current = s(&["mode", "-custom:sym", "punct"]);
        let encoded = s(&["mode", "punct"]);
        let merged = merge_preserving_foreign(&current, &managed, &encoded);
        assert_eq!(merged, current);
        assert_eq!(
            merged
                .iter()
                .filter(|x| strip_off(x) == "custom:sym")
                .count(),
            1,
            "同一个条目不得被写出两份"
        );
    }

    /// 没有外来条目时，合并等于原样返回（快速路径，也保证不引入意外重排）。
    #[test]
    fn no_foreign_items_is_identity() {
        let managed = s(&["mode", "punct"]);
        let encoded = s(&["punct", "-mode"]);
        assert_eq!(
            merge_preserving_foreign(&s(&["mode", "punct"]), &managed, &encoded),
            encoded
        );
    }

    /// `-` 只剥一层，与 core 的 `parse_toolbar_items` 同口径。
    #[test]
    fn strip_off_peels_exactly_one_dash() {
        assert_eq!(strip_off("mode"), "mode");
        assert_eq!(strip_off("-mode"), "mode");
        assert_eq!(strip_off("--mode"), "-mode");
    }

    /// 自定义按钮的条目名带冒号，前缀编码不得破坏它。
    #[test]
    fn custom_item_names_survive_the_encoding() {
        let order = s(&["custom:sym"]);
        assert_eq!(
            encode_ordered_items(&order, &[]),
            s(&["-custom:sym"]),
            "关掉的自定义按钮加前缀，冒号原样保留"
        );
        assert_eq!(strip_off("-custom:sym"), "custom:sym");
    }

    /// 守护：清单里出现的每个 `type` 都必须能渲染出真控件。
    ///
    /// 缺这条的后果不是报错而是**静默降级**：capability 校验只问「控件与配置类型兼不兼容」，
    /// 一个渲染器根本不认识的 type 照样能答"兼容"，于是全套测试绿着，界面上那一行却是
    /// `[类型名?]` 灰字。2026-08-25 加 `dialog_button_ordered_pairs` 时实测如此——
    /// 741 条测试无一变红，是截图才看出来的。
    #[test]
    fn manifest_control_types_are_all_renderable() {
        let m = Manifest::from_embedded();
        let mut unknown: Vec<&str> = m
            .items
            .values()
            .map(|i| i.typ.as_str())
            .filter(|t| !RENDERABLE_CONTROL_TYPES.contains(t))
            .collect();
        unknown.sort_unstable();
        unknown.dedup();
        assert!(
            unknown.is_empty(),
            "清单声明了渲染器不认识的控件类型 {unknown:?}\n  \
             —— 界面上会显示成 [类型名?] 占位符。要么在 build_control 加分支并登记进 \
             RENDERABLE_CONTROL_TYPES，要么改用已有类型"
        );
    }

    /// 反向：名单里的每一项都必须真有 `build_control` 分支。
    ///
    /// 与上一条**方向相反、缺一不可**：上一条防"清单有、名单无"，这条防"名单有、实现无"
    /// ——后者会让上一条变成永真测试（名单里塞个不存在的类型，清单校验照样过）。
    ///
    /// 判据取自 `build_control` 的 `other` 分支那句 `debug_assert!`：测试构建即 debug，
    /// 真走到占位符分支会当场炸。这里只能静态核对名单非空且无重复——真正的对拍由那句
    /// 断言在运行时完成。
    #[test]
    fn renderable_control_types_has_no_duplicates() {
        let mut sorted = RENDERABLE_CONTROL_TYPES.to_vec();
        sorted.sort_unstable();
        let n = sorted.len();
        sorted.dedup();
        assert_eq!(n, sorted.len(), "RENDERABLE_CONTROL_TYPES 有重复项");
        assert!(!RENDERABLE_CONTROL_TYPES.is_empty());
    }

    /// 回归：config.toml 里**显式写着内置默认值**的标点映射行，不许把设置页判成脏。
    ///
    /// 用户实景：手工把 `"<"` 从 `〈` 改成 `《`（后者正是内置默认），此后每次打开设置页
    /// 什么都不动、关窗都提示「有未保存改动」。根因是 writer 会剪掉全等默认的行、而
    /// core 保留了它（Map 内部条目不在注册表，剪枝够不着），两侧恒不相等。
    ///
    /// 断言方式是「归一后的基线 == 未编辑时 writer 的输出」——writer 未编辑时的输出就是
    /// 对基线逐行 `seed_cell` 播种再裁剪，即 `punct_canonical_map` 本身，故直接比对它
    /// 的幂等性与裁剪结果即可，不必立起需要 windui 环境的 `AppState`。
    #[test]
    fn punct_baseline_absorbs_explicit_default_row() {
        let mut cfg = Config::from_value(serde_json::json!({
            "input": { "punct": { "custom_mappings": {
                // 四格全是内置默认 → 不算自定义，基线里必须被吸收掉
                "<": ["《", "＜", "《", "<"],
                ">": ["》", "＞", "》", ">"],
                // 真正的自定义 → 必须原样留下
                "\\": ["、", "＼", "、", "|"],
            } } }
        }));
        canonicalize_punct_baseline(&mut cfg);
        let m = cfg
            .get_path(PUNCT_MAP_KEY)
            .and_then(|v| v.as_object())
            .expect("归一后该键必须存在");
        assert!(!m.contains_key("<"), "全等默认的行应被剪掉: {m:?}");
        assert!(!m.contains_key(">"), "全等默认的行应被剪掉: {m:?}");
        assert_eq!(
            m.get("\\"),
            Some(&serde_json::json!(["、", "＼", "、", "|"])),
            "真正的自定义行必须原样保留"
        );

        // 幂等：再归一一次不再变化 —— writer 的输出喂回基线也必须落在同一个不动点上，
        // 否则「保存后立刻又判脏」。
        let once = m.clone();
        canonicalize_punct_baseline(&mut cfg);
        assert_eq!(
            cfg.get_path(PUNCT_MAP_KEY).and_then(|v| v.as_object()),
            Some(&once),
            "归一必须幂等"
        );
    }

    /// 键缺失时基线也要补出空表：writer 是无条件 `set_path` 的，
    /// 基线缺键会让 diff 拿 `Null` 跟 `{}` 比而恒判有变更。
    #[test]
    fn punct_baseline_fills_missing_key_with_empty_map() {
        let mut cfg = Config::from_value(serde_json::json!({ "input": {} }));
        canonicalize_punct_baseline(&mut cfg);
        assert_eq!(
            cfg.get_path(PUNCT_MAP_KEY),
            Some(&serde_json::json!({})),
            "缺键必须补成空表"
        );
    }

    /// 缺格（数组不足 4 项）按 `seed_cell` 回落默认后再判裁剪：
    /// 只写了默认值前缀的行同样不算自定义。
    #[test]
    fn punct_baseline_treats_short_default_row_as_clean() {
        let mut cfg = Config::from_value(serde_json::json!({
            "input": { "punct": { "custom_mappings": { "!": ["！"] } } }
        }));
        canonicalize_punct_baseline(&mut cfg);
        let m = cfg
            .get_path(PUNCT_MAP_KEY)
            .and_then(|v| v.as_object())
            .expect("归一后该键必须存在");
        assert!(m.is_empty(), "缺格补默认后全等默认，应被剪掉: {m:?}");
    }

    /// 端到端闭环：**真实** punct writer + 真实基线归一 + 真实 diff。
    ///
    /// 上面那条回归测的是归一函数本身，这条测的是「writer 的输出确实落在归一后的基线
    /// 上」——两者一旦分头演化（哪怕都各自自洽），关窗守卫就又会永远拦人。故这里不复刻
    /// writer 的逻辑，直接把 `build_dialog_button_punct` 注册的那个 writer 跑起来。
    #[test]
    fn punct_writer_lands_on_canonical_baseline() {
        // core 返回的配置：前两行四格全是内置默认（手工编辑 config.toml、或
        // config.applyPatch 的合并语义都会留下这种冗余行），第三行是真自定义。
        let cfg = Config::from_value(serde_json::json!({
            "input": { "punct": { "custom_mappings": {
                "<": ["《", "＜", "《", "<"],
                ">": ["》", "＞", "》", ">"],
                "\\": ["、", "＼", "、", "|"],
            } } }
        }));

        let st = AppState::empty();
        let _ = build_dialog_button_punct(&st, PUNCT_MAP_KEY, &cfg);

        // apply_loaded 的做法：基线归一后再作为 diff 基准。
        let mut base = cfg.clone();
        canonicalize_punct_baseline(&mut base);

        // has_unsaved_changes 的做法：跑全部 writer 到副本上。用户一格都没动过。
        let mut probe = base.clone();
        for w in st.fields.writers.borrow().iter() {
            w(&mut probe);
        }

        assert_eq!(
            probe.get_path(PUNCT_MAP_KEY),
            base.get_path(PUNCT_MAP_KEY),
            "什么都没改，writer 的输出必须与归一后的基线相等，否则关窗永远提示未保存"
        );
        // 顺带钉住：真自定义那行不许在这条路上蒸发。
        assert_eq!(
            probe
                .get_path(PUNCT_MAP_KEY)
                .and_then(|v| v.as_object())
                .and_then(|m| m.get("\\")),
            Some(&serde_json::json!(["、", "＼", "、", "|"])),
        );
    }

    /// `PUNCT_DEFAULTS` 的四列必须与 core 的标点转换流水线**逐格相等**。
    ///
    /// 这张表是 core 内置转换表在设置端的第二个知情者，且被当成「什么算自定义」的判据
    /// （见 [`punct_row_override`]）。漂移一格的后果不是显示错一个字符，而是那一行永远
    /// 判不干净——用户什么都没改，关窗也提示未保存。没有堆栈、没有报错，只有这条测试
    /// 看得见，所以对着 core 的真实流水线算，而不是抄一份快照自己跟自己比。
    ///
    /// 列语义（core 存储序）：0 中半 / 1 英全 / 2 中全 / 3 英半，
    /// 与 `wind_punct::punct_col_idx` 的 `(chinese_punct, full_width)` 组合一一对应。
    #[test]
    fn punct_defaults_match_core_pipeline() {
        use wind_config::config::InputConfig;
        use wind_transform::punctuation::PunctuationConverter;

        /// core 在「无自定义映射」时该格的产物。`skip` 为引号交替态的预热次数
        /// （`"2`/`'2` 那两行是**右形**，须先消耗掉一次左形）。
        fn core_col(ch: char, skip: usize, col: usize) -> String {
            let cfg = InputConfig::default();
            debug_assert!(!cfg.punct.custom_enabled, "默认应无自定义，否则本测试自欺");
            let (chinese, full) = match col {
                0 => (true, false),
                1 => (false, true),
                2 => (true, true),
                3 => (false, false),
                _ => unreachable!(),
            };
            let mut conv = PunctuationConverter::new();
            // 预热必须走中文分支：英文列不调 to_chinese，也就不推进交替态。
            for _ in 0..skip {
                wind_punct::convert_punct(&mut conv, &cfg.punct, true, false, ch, 0);
            }
            wind_punct::convert_punct(&mut conv, &cfg.punct, chinese, full, ch, 0)
        }

        for (label, tok, defs) in PUNCT_DEFAULTS {
            let (ch, skip) = match *tok {
                "\"1" => ('"', 0),
                "\"2" => ('"', 1),
                "'1" => ('\'', 0),
                "'2" => ('\'', 1),
                _ => {
                    let mut it = tok.chars();
                    let c = it.next().expect("token 不可为空");
                    assert!(it.next().is_none(), "非引号 token 必须是单字符: {tok:?}");
                    (c, 0)
                }
            };
            for (col, got) in defs.iter().enumerate() {
                let want = core_col(ch, skip, col);
                if got.is_empty() {
                    // 空串在 core 侧的语义是「该格回落默认」（lookup_custom 会 filter 掉
                    // 空值），所以空格行用它表示「不改」是对的——文本框里也没法把一个
                    // 空格显示成可辨认的内容。只有空格行允许这样。
                    assert_eq!(
                        *tok, " ",
                        "只有空格行允许留空格：{label} 第 {col} 列空着，core 却给出 {want:?}"
                    );
                    continue;
                }
                assert_eq!(
                    *got, want,
                    "PUNCT_DEFAULTS 与 core 转换表漂移：{label}(token={tok:?}) 第 {col} 列"
                );
            }
        }
    }

    #[test]
    fn platform_gate() {
        // 缺省 = 全平台。
        assert!(platform_matches(None));
        // 当前平台恒有一个匹配，另一个不匹配。
        assert_eq!(platform_matches(Some("windows")), cfg!(windows));
        assert_eq!(platform_matches(Some("macos")), cfg!(target_os = "macos"));
        // 逗号列表：命中其一即可，容忍空白。
        assert!(platform_matches(Some("windows, macos")));
        // 写错名字宁可整项消失（一眼看得见），也不要静默当成全平台显示。
        assert!(!platform_matches(Some("darwin")));
    }

    #[test]
    fn toolbar_items_hidden_on_macos() {
        // 这些配置在 macOS 无落点，逐项理由：
        //   toolbar.*         菜单栏指示器不是浮动窗口，无从隐藏/排列/自动淡出；
        //   capslock.cancel_* 取消大写靠合成 CapsLock 敲击，而 macOS 的锁定态由 HID 层
        //                     维护，CGEvent 改不动（要走 IOHIDSetModifierLockState）；
        //   stats.track_english  英文输入由 Windows 侧 TSF DLL 经 CMD_INPUT_STATS 上报，
        //                     macOS 的 `.app` 没有对应采集点。
        // 而 ui.toolbar.visible 必须留着——它在 macOS 上控制指示器的显示与否。
        let m = Manifest::from_embedded();
        for k in [
            "ui.toolbar.hide_in_fullscreen",
            "ui.toolbar.auto_hide",
            "ui.toolbar.auto_hide_delay",
            "ui.toolbar.vertical",
            "input.capslock.cancel_on_mode_switch",
            "stats.track_english",
        ] {
            let item = m.items.get(k).unwrap_or_else(|| panic!("清单缺 {k}"));
            assert_eq!(
                item.hidden,
                cfg!(target_os = "macos"),
                "{k} 的平台门控与当前平台不符"
            );
        }
        assert!(
            !m.items
                .get("ui.toolbar.visible")
                .expect("清单缺该项")
                .hidden,
            "ui.toolbar.visible 两平台都该显示"
        );
    }

    /// 「混输行为」对话框的分组结构必须是**可渲染的形状**，且四个拼音项对用户可见。
    ///
    /// ★ 守三个静默失效方向，`capabilities` 那几道守门测试一个都查不出来（它们只问
    /// 「key 有没有登记」）：
    ///
    /// 1. **`subsection = None` 不终止前一个分组**（[`build_scheme_config_button`] 只在
    ///    `subsec != cur_subsec` 且非 None 时插标题）⇒ 无 subsection 的通用项若排在某个
    ///    分组之后，会紧跟其后渲染、**视觉上成为那个分组的成员**。`schema.pinyin.separator`
    ///    曾因此长期挂在「自动学习」下，且只在对话框里看得出来（主体页压根不读 subsection）。
    /// 2. **同名 subsection 被别的分组打断**会渲染出两个同名标题。
    /// 3. **`hidden`（或写错的 `platform`）让项连行都不渲染**，而 key 仍在清单里登记着，
    ///    覆盖率检查照样绿——用户视角就是「设置页没有这个开关」。
    ///
    /// 前两条是对整个 section 的结构断言，比钉死具体位置更耐改：日后往这张卡片加项、调序
    /// 都不会误报，只有排出「渲染不出预期形状」的顺序才红。
    #[test]
    fn mix_section_subsections_render_as_declared() {
        let m = Manifest::from_embedded();
        let keys = m
            .sections_of("schema")
            .into_iter()
            .find(|(s, _)| s == "混输行为")
            .map(|(_, k)| k)
            .expect("清单缺「混输行为」卡片");

        // ① 无 subsection 的项必须全部排在第一个有 subsection 的项之前。
        let first_grouped = keys
            .iter()
            .position(|k| m.items[k].subsection.is_some())
            .expect("「混输行为」应当有 subsection 分组");
        for k in &keys[first_grouped..] {
            assert!(
                m.items[k].subsection.is_some(),
                "{k} 没有 subsection 却排在分组之后，会被静默吸附进上一个分组"
            );
        }

        // ② 同一 subsection 的项必须连续，否则标题会重复渲染。
        let mut seen: Vec<&str> = Vec::new();
        let mut cur: Option<&str> = None;
        for k in &keys {
            let sub = m.items[k].subsection.as_deref();
            if sub != cur {
                if let Some(name) = sub {
                    assert!(!seen.contains(&name), "分组「{name}」被打断后再次出现");
                    seen.push(name);
                }
                cur = sub;
            }
        }

        // ③ 四个拼音项的归属与可见性（本轮新增两项即在其中）。
        for k in [
            "schema.mix.enable_pinyin_abbrev",
            "schema.mix.min_pinyin_length",
            "schema.mix.pinyin_partial_candidates",
            "schema.mix.pinyin_partial_candidates_overflow",
        ] {
            let item = m.items.get(k).unwrap_or_else(|| panic!("清单缺 {k}"));
            assert!(!item.hidden, "{k} 必须对用户可见");
            assert_eq!(item.subsection.as_deref(), Some("拼音相关"), "{k} 的分组");
        }
        for k in [
            "schema.mix.auto_commit_block_on_pinyin",
            "schema.mix.block_commit_on_pinyin_word",
            "schema.mix.auto_commit_block_on_english",
        ] {
            let item = m.items.get(k).unwrap_or_else(|| panic!("清单缺 {k}"));
            assert!(!item.hidden, "{k} 必须对用户可见");
            assert_eq!(item.subsection.as_deref(), Some("上屏设置"), "{k} 的分组");
        }
    }

    /// 清单未列举的当前取值必须被保留成一个选项。
    ///
    /// ★ 守的是一个**静默数据丢失**方向：没有这一步，手写了 `z_key_action =
    /// "special:rare"` 的用户一打开设置页，下拉会显示首项「不启用」，点保存就把配置
    /// 改成了空——全程无报错、无提示，页面看着一切正常。
    #[test]
    fn select_keeps_unlisted_current_value() {
        let mut v = vec![String::new(), "temp_pinyin".to_string()];
        let mut l = vec!["不启用".to_string(), "临时拼音".to_string()];

        // 已列举的取值：正常命中，不该补项。
        assert_eq!(select_index_for(&mut v, &mut l, "temp_pinyin"), 1);
        assert_eq!(v.len(), 2, "命中时不该补项");

        // 未列举的取值：补项并选中；写回的必须是原值本身，不能是标签。
        let idx = select_index_for(&mut v, &mut l, "special:rare");
        assert_eq!(idx, 2);
        assert_eq!(v[2], "special:rare");
        assert!(l[2].contains("special:rare"), "标签应能看出这是当前配置");

        // 空值 = 未设置：回落首项，不该补出一个空标签项。
        let mut v2 = vec!["a".to_string()];
        let mut l2 = vec!["A".to_string()];
        assert_eq!(select_index_for(&mut v2, &mut l2, ""), 0);
        assert_eq!(v2.len(), 1);
    }

    /// ★ 联想类型的清单**刻意不列 `smart`**（桌面上它与 `word` 给的东西相同），而 core
    /// 的值域里仍有它、移动端出厂就是它，且已经有用户配着它。
    ///
    /// 这一条钉的正是「从 options 里删掉一个仍然合法的取值」这个动作的安全性：
    /// 已配 `smart` 的人打开设置页必须仍看到 `smart` 被选中，一旦回落到首项（关闭），
    /// 他只要点一次保存，联想就被静默关掉了——而页面上看不出任何异常。
    ///
    /// ⚠️ 将来若把 `smart` 放回 options（history/bigram 落地、两档真正分开后），
    /// 本测试的前两个断言会失效，届时删掉它即可，不必强行保留。
    #[test]
    fn association_kind_keeps_smart_when_configured() {
        let manifest = Manifest::from_embedded();
        let item = manifest
            .get("input.association.kind")
            .expect("清单应有联想类型");
        let mut values: Vec<String> = item.options.iter().map(|o| o.value.clone()).collect();
        let mut labels: Vec<String> = item.options.iter().map(|o| o.label.clone()).collect();

        assert!(!values.contains(&"smart".to_string()), "清单不该列 smart");
        assert!(values.contains(&"word".to_string()), "清单该列 word");

        let idx = select_index_for(&mut values, &mut labels, "smart");
        assert_eq!(values[idx], "smart", "已配 smart 的用户必须仍选中 smart");
        assert_ne!(idx, 0, "绝不能回落到首项（关闭）——保存一次就把联想关掉了");
    }

    /// 数值控件写回时该编码成 Value 的 Int 变体还是 Float 变体，得看 capability 登记的
    /// 字段类型，不能按当前值有没有小数部分猜——float 型字段恰好取整数值时（如
    /// `font_size` 默认 18）若被猜成 Int，会跟 `config.get` 返回的 Float 变体在
    /// `serde_json::Value` 层面判不相等，diff_config 就会把「什么都没改」误判成一条变更。
    #[test]
    fn number_writer_encodes_by_capability_type_not_by_value_shape() {
        let state = AppState::empty();
        let m = Manifest::from_embedded();
        let cfg = Config::from_value(serde_json::json!({}));

        // float 型字段：即使当前是整数值，也必须写回 Float 变体。
        let font_item = m.items.get("ui.candidate.font_size").expect("清单缺该项");
        build_number(&state, "ui.candidate.font_size", font_item, &cfg);

        // int 型字段：必须写回 Int 变体，否则服务端严格类型校验会直接拒绝该项。
        let max_chars_item = m.items.get("ui.candidate.max_chars").expect("清单缺该项");
        build_number(&state, "ui.candidate.max_chars", max_chars_item, &cfg);

        let mut out = Config::from_value(serde_json::json!({}));
        for w in state.fields.writers.borrow().iter() {
            w(&mut out);
        }

        let font_v = out.get_path("ui.candidate.font_size").unwrap();
        assert!(
            font_v.is_f64() && !font_v.is_i64(),
            "float 型字段应写回浮点 Value，实际: {font_v:?}"
        );
        let max_chars_v = out.get_path("ui.candidate.max_chars").unwrap();
        assert!(
            max_chars_v.is_i64(),
            "int 型字段应写回整数 Value，实际: {max_chars_v:?}"
        );
    }

    fn si(id: &str, kind: &str) -> SchemaInfo {
        SchemaInfo {
            id: id.to_string(),
            name: format!("{id}名"),
            kind: kind.to_string(),
            scheme: String::new(),
            hidden: false,
        }
    }

    /// 空值首项标签按 kind 分流：码表「（自动）」、拼音「（默认：全拼）」。
    ///
    /// core 侧两者语义确实不同——`primary_codetable` 空 = 扫描 available 挑首个码表方案，
    /// `primary_pinyin` 空 = 固定全拼、不扫描。标签必须跟着分流，否则拼音那份是在骗人。
    #[test]
    fn schema_options_empty_label_differs_by_kind() {
        let schemas = vec![si("wubi86", "codetable"), si("pinyin", "pinyin")];
        let (_, ct_labs) = schema_options(&schemas, Some("codetable"));
        assert_eq!(ct_labs[0], "（自动）");
        let (_, py_labs) = schema_options(&schemas, Some("pinyin"));
        assert_eq!(py_labs[0], "（默认：全拼）", "拼音空值 = 固定全拼，非自动");
    }

    /// kind 为空（core 没读出 .schema.toml）的方案**不得**被过滤掉。
    ///
    /// 该方案在方案列表里是看得见的（core 的 schema_name 回退成 id），排除它等于
    /// 「列表里有、下拉里没有」，而解析失败只 warn 进日志，用户完全无从判断。
    #[test]
    fn schema_options_keeps_schemas_with_unknown_kind() {
        let schemas = vec![si("pinyin", "pinyin"), si("thirdparty", "")];
        let (py_vals, _) = schema_options(&schemas, Some("pinyin"));
        assert!(
            py_vals.contains(&"thirdparty".to_string()),
            "kind 未知的方案应保留，实际 {py_vals:?}"
        );
        // 反向对照：kind 已知且不匹配的照常排除，否则上面那条会因为"根本没过滤"而假绿。
        let (ct_vals, _) = schema_options(&schemas, Some("codetable"));
        assert!(
            !ct_vals.contains(&"pinyin".to_string()),
            "kind 明确为 pinyin 的方案不该出现在码表下拉里，实际 {ct_vals:?}"
        );
    }

    /// 特殊方案（英文、快符）不进主方案下拉。
    ///
    /// 方案列表自打开 `includeHidden` 起就恒含它们（词库页、统计页要用），过滤只在这一个
    /// 呈现点做。少了这一条，「主拼音方案」下拉里会冒出英文和快符——选中也没有任何效果，
    /// 因为 core 的 `primary_pinyin` 只认拼音方案。
    #[test]
    fn schema_options_excludes_hidden_schemas() {
        let hidden = |id: &str, kind: &str| SchemaInfo {
            hidden: true,
            ..si(id, kind)
        };
        let schemas = vec![
            si("pinyin", "pinyin"),
            hidden("english", "pinyin"),
            si("wubi86", "codetable"),
            hidden("qsym", "codetable"),
        ];
        let (py_vals, _) = schema_options(&schemas, Some("pinyin"));
        assert!(
            !py_vals.contains(&"english".to_string()),
            "特殊方案不该进主拼音下拉，实际 {py_vals:?}"
        );
        let (ct_vals, _) = schema_options(&schemas, Some("codetable"));
        assert!(
            !ct_vals.contains(&"qsym".to_string()),
            "快符不该进主码表下拉，实际 {ct_vals:?}"
        );
        // 反向对照：同 kind 的普通方案必须还在——否则上面两条在「整个下拉空了」时也通过。
        assert!(
            py_vals.contains(&"pinyin".to_string()),
            "普通拼音方案应保留"
        );
        assert!(
            ct_vals.contains(&"wubi86".to_string()),
            "普通码表方案应保留"
        );
    }

    /// `==` 单条件：保持原有语义（布尔与字符串）。
    #[test]
    fn parse_conds_single_equality() {
        let cs = parse_conds("ui.status.enabled == true").unwrap();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].key, "ui.status.enabled");
        assert_eq!(cs[0].op, CondOp::Eq);
        assert!(cond_matches(&Value::Bool(true), &cs[0]));
        assert!(!cond_matches(&Value::Bool(false), &cs[0]));

        let cs = parse_conds("ui.status.position_mode == fixed").unwrap();
        assert!(cond_matches(&Value::String("fixed".into()), &cs[0]));
        assert!(!cond_matches(&Value::String("follow_caret".into()), &cs[0]));
    }

    /// `&&`：全部满足才算满足。
    #[test]
    fn parse_conds_multiple_are_anded() {
        let cs =
            parse_conds("ui.status.enabled == true && ui.status.display_mode == temp").unwrap();
        assert_eq!(cs.len(), 2);
        assert!(cond_matches(&Value::Bool(true), &cs[0]));
        assert!(cond_matches(&Value::String("temp".into()), &cs[1]));
        // 第二条不满足 → 整体不满足（由调用方的 all() 保证，此处校验单条判定正确）
        assert!(!cond_matches(&Value::String("always".into()), &cs[1]));
    }

    /// `contains`：strlist 里是否含指定元素（checkbox_group 的联动依据）。
    #[test]
    fn parse_conds_contains_matches_strlist_element() {
        let cs = parse_conds("ui.status.items contains schema").unwrap();
        assert_eq!(cs[0].op, CondOp::Contains);
        assert_eq!(cs[0].key, "ui.status.items");
        let arr = Value::Array(vec![
            Value::String("schema".into()),
            Value::String("punct".into()),
        ]);
        assert!(cond_matches(&arr, &cs[0]));
        let arr_without = Value::Array(vec![Value::String("punct".into())]);
        assert!(!cond_matches(&arr_without, &cs[0]));
        // 非数组值不应误判为命中
        assert!(!cond_matches(&Value::String("schema".into()), &cs[0]));
    }

    /// `!=` 必须先于 `==` 匹配。
    ///
    /// ★ 这条测试守的是一个**静默失败方向**：`"a != b"` 不含 `==`，顺序写反会让整条表达式
    /// 解析失败返回 None，而调用方对 None 的处理是「无条件启用」——控件不会锁死、不会报错，
    /// 只是悄悄失去联动。没有这条断言，写反了跑起来看着一切正常。
    #[test]
    fn parse_conds_ne_is_not_swallowed_by_eq() {
        let cs = parse_conds("schema.mix_modes#free_input != off").expect("!= 应能解析");
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].op, CondOp::Ne);
        assert_eq!(cs[0].key, "schema.mix_modes#free_input");
        assert_eq!(cs[0].value, "off");
        // 语义：等于该值时不满足，其余取值都满足。
        assert!(!cond_matches(&Value::String("off".into()), &cs[0]));
        assert!(cond_matches(&Value::String("auto".into()), &cs[0]));
        assert!(cond_matches(&Value::String("always".into()), &cs[0]));
        // 布尔形态的 != 同样成立（取 == 的反）。
        let cb = parse_conds("k != true").unwrap();
        assert!(!cond_matches(&Value::Bool(true), &cb[0]));
        assert!(cond_matches(&Value::Bool(false), &cb[0]));
    }

    /// `!=` 与 `==` 混用、以 `&&` 串联时仍是「全部满足」。
    #[test]
    fn parse_conds_mixes_ne_with_eq() {
        let cs = parse_conds("a == true && b != off").unwrap();
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].op, CondOp::Eq);
        assert_eq!(cs[1].op, CondOp::Ne);
    }

    /// 语法不合法 → None，调用方按「无条件」处理，绝不能把控件锁死。
    #[test]
    fn parse_conds_rejects_malformed() {
        assert!(parse_conds("ui.status.enabled").is_none());
        assert!(parse_conds("== true").is_none());
        assert!(parse_conds("ui.status.enabled ==").is_none());
        assert!(parse_conds("").is_none());
    }

    #[test]
    fn checkbox_group_collects_checked_values() {
        // 纯逻辑：勾选集合 → strlist
        let checked = [("lshift", true), ("rshift", false), ("capslock", true)];
        let arr = collect_checked(&checked);
        assert_eq!(arr, vec!["lshift".to_string(), "capslock".to_string()]);
    }

    /// 守护：`dialog_extra_for` 必须指向一个**真实存在且能承载附加行**的 dialog_button。
    ///
    /// 失配的后果是静默的：寄居项在 `section_rows` 里被跳过（不独立成行），宿主又不存在或
    /// 不走 `build_dialog_button_string`，于是这个配置项在整个设置页里**彻底消失**——
    /// 用户改不到，覆盖守护也查不出（key 仍在 manifest 里登记着）。
    #[test]
    fn dialog_extra_for_targets_are_valid() {
        let m = Manifest::from_embedded();
        for k in &m.order {
            let it = m.get(k).unwrap();
            let Some(host_key) = &it.dialog_extra_for else {
                continue;
            };
            let host = m
                .get(host_key)
                .unwrap_or_else(|| panic!("`{k}` 的 dialog_extra_for 指向不存在的项 `{host_key}`"));
            // 宿主有两种合法形态：dialog_button（自身即按钮），或声明了 dialog_title 的
            // 普通值控件（分组对话框宿主，见 build_grouped_dialog_row）。别的类型都会让
            // 寄居项无处可去——宿主照常独立成行，寄居项则被 section_rows 跳过、彻底消失。
            assert!(
                host.typ == "dialog_button" || host.dialog_title.is_some(),
                "`{k}` 寄居的宿主 `{host_key}` 必须是 dialog_button 或带 dialog_title，实际 `{}`",
                host.typ
            );
            assert_eq!(
                host.section, it.section,
                "`{k}` 与宿主 `{host_key}` 必须同 section，否则跨卡片寄居难以理解"
            );
            // 宿主若被 gate 且会置灰，寄居项就可能永远够不着（见 gate_soft 的存在理由）。
            if host.gate_key.is_some() {
                assert!(
                    host.gate_soft,
                    "`{host_key}` 承载了寄居项却仍随开关置灰，`{k}` 在开关关闭时将无法访问；\
                     请给宿主加 gate_soft = true"
                );
            }
        }
    }

    /// 守护：`dialog_title` 宿主必须真的有寄居项，且自身不被别的收纳机制吞掉。
    ///
    /// 没有寄居项的宿主 = 把一个普通设置行藏进只有它一项的对话框，纯粹多一次点击；
    /// 而宿主若同时带 `dialog_extra_for` / `dialog_group` / `hidden`，`section_rows`
    /// 会先在那些分支上把它跳过，整组设置（宿主 + 所有寄居项）一并从页面上消失。
    #[test]
    fn dialog_title_hosts_are_well_formed() {
        let m = Manifest::from_embedded();
        for k in &m.order {
            let it = m.get(k).unwrap();
            if it.dialog_title.is_none() {
                continue;
            }
            assert!(
                it.dialog_extra_for.is_none() && it.dialog_group.is_none() && !it.hidden,
                "`{k}` 是 dialog_title 宿主，不能同时被 dialog_extra_for / dialog_group / hidden 收走"
            );
            let members = m
                .order
                .iter()
                .filter(|x| {
                    m.get(x).and_then(|i| i.dialog_extra_for.as_deref()) == Some(k.as_str())
                })
                .count();
            assert!(
                members > 0,
                "`{k}` 声明了 dialog_title 却没有任何 dialog_extra_for 成员，                 等于把一行设置藏进对话框；要么补成员，要么去掉 dialog_title"
            );
        }
    }

    /// 守护：`dialog_hint` 只在 `dialog_title` 宿主上有意义（别处写了不会渲染）。
    #[test]
    fn dialog_hint_requires_dialog_title() {
        let m = Manifest::from_embedded();
        for k in &m.order {
            let it = m.get(k).unwrap();
            assert!(
                it.dialog_hint.is_none() || it.dialog_title.is_some(),
                "`{k}` 写了 dialog_hint 却没有 dialog_title，该文案不会出现在任何地方"
            );
        }
    }

    /// 守护：`enabled_when` 引用的 key 必须真实存在于 manifest。
    ///
    /// **失配的后果是静默的、且方向最坏**：求值时 `getters.get(key)` 取不到就
    /// `unwrap_or(true)` —— 条件被当作**恒满足**，控件永远可用。写错一个字母，联动就
    /// 悄无声息地整个失效，UI 上完全看不出异常。
    ///
    /// 顺带校验 `== value` 的取值落在该项声明的 options 内（仅对 select 类宿主）：
    /// 值写错同样是恒不满足或恒满足，肉眼极难发现。
    #[test]
    fn enabled_when_keys_and_values_are_valid() {
        let m = Manifest::from_embedded();
        for k in &m.order {
            let it = m.get(k).unwrap();
            let Some(expr) = &it.enabled_when else {
                continue;
            };
            let conds = parse_conds(expr)
                .unwrap_or_else(|| panic!("`{k}` 的 enabled_when 无法解析：`{expr}`"));
            for c in &conds {
                let target = m.get(&c.key).unwrap_or_else(|| {
                    panic!(
                        "`{k}` 的 enabled_when 引用了不存在的 key `{}`——求值会 unwrap_or(true)，                         条件恒满足、联动静默失效",
                        c.key
                    )
                });
                // 布尔字面量与 contains 不参与 options 校验
                if c.op == CondOp::Contains || c.value == "true" || c.value == "false" {
                    continue;
                }
                if target.typ == "select" && !target.options.is_empty() {
                    let vals: Vec<&str> = target.options.iter().map(|o| o.value.as_str()).collect();
                    assert!(
                        vals.contains(&c.value.as_str()),
                        "`{k}` 的 enabled_when 比较 `{} == {}`，但该项的 options 只有 {:?}",
                        c.key,
                        c.value,
                        vals
                    );
                }
            }
        }
    }

    /// 守护：子对话框分组配对一致 —— 每个 dialog_group 必有恰好一个 opens_dialog 宿主，
    /// 每个宿主至少一个成员。防止失配导致成员 key 静默丢失三件套注册（覆盖守护查不出）。
    #[test]
    fn dialog_group_pairing_is_consistent() {
        use std::collections::HashMap;
        let m = Manifest::from_embedded();
        let mut openers: HashMap<String, usize> = HashMap::new();
        let mut members: HashMap<String, usize> = HashMap::new();
        for k in &m.order {
            let it = m.get(k).unwrap();
            if let Some(g) = &it.opens_dialog {
                *openers.entry(g.clone()).or_default() += 1;
            }
            if let Some(g) = &it.dialog_group {
                *members.entry(g.clone()).or_default() += 1;
            }
        }
        for (g, cnt) in &members {
            let hosts = openers.get(g).copied().unwrap_or(0);
            assert_eq!(
                hosts, 1,
                "dialog_group `{g}`（{cnt} 成员）应恰好 1 个宿主，实际 {hosts}"
            );
        }
        for g in openers.keys() {
            assert!(
                members.contains_key(g),
                "opens_dialog `{g}` 无任何成员(dialog_group)"
            );
        }
    }

    #[test]
    fn hotkey_parse_compose_roundtrip() {
        // core 实际默认值需原样往返（注意 = 用命名 equal、标点 . \ 用字面）。
        for s in [
            "ctrl+shift+e",
            "shift+space",
            "ctrl+.",
            "ctrl+shift+\\",
            "ctrl+equal",
            "ctrl+shift+[",
        ] {
            let (m, k) = parse_hotkey(s);
            assert_eq!(compose_hotkey(m, &k), s, "round-trip 失败: {s}");
        }
        // 无主键 → none；none/空 → 全 false 空键
        assert_eq!(compose_hotkey([true, true, false, false], ""), "none");
        let (m, k) = parse_hotkey("none");
        assert_eq!((m, k.as_str()), ([false; 4], ""));
        // 显示格式：命名键映射回符号
        assert_eq!(format_hotkey("ctrl+equal"), "Ctrl+=");
        assert_eq!(format_hotkey("none"), "未设置");
    }

    #[test]
    fn scheme_sections_present_and_nonempty() {
        // 守护：方案页内联/对话框依赖这些硬编码 section 名（pages::scheme_page）。
        // 改名 TOML section 会让 find() 落空→静默空对话框，此测试把失败前移到 CI。
        let m = Manifest::from_embedded();
        let secs = m.sections_of("schema");
        for name in ["主方案设置", "上屏行为", "候选行为", "模糊音"] {
            let found = secs.iter().find(|(s, _)| s == name);
            assert!(found.is_some(), "schema 组缺少 section「{name}」");
            assert!(!found.unwrap().1.is_empty(), "section「{name}」为空");
        }
    }

    /// 守护：候选来源选项必须与 core 的默认 members **逐字同序**。
    ///
    /// 声明序在两处生效：新勾选成员的插入位置、以及对话框「恢复默认」的目标集合。
    /// 与 core 漂移（core 加了来源 / 改了默认序）时界面会静默少一个开关或把成员插错位置，
    /// 故在此对账而不是靠人工同步。
    #[test]
    fn mix_members_options_match_core_default_order() {
        let m = Manifest::from_embedded();
        let item = m.get("schema.mix_modes#members").expect("清单缺候选来源项");
        let declared: Vec<&str> = item.options.iter().map(|o| o.value.as_str()).collect();
        let core = wind_config::config::Config::default();
        let core_members: Vec<&str> = core.schema.mix_modes[0]
            .members
            .iter()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(
            declared, core_members,
            "候选来源选项与 core default_mix_modes() 的成员序漂移"
        );
    }

    /// 守护：触发键与候选来源是**两个独立清单项**，且读写同一个配置路径。
    /// 合成一项（或两项同 key）会让 `Manifest.items` 后者覆盖前者、loaders 互相挤掉。
    #[test]
    fn quick_input_has_two_separate_items_on_same_path() {
        use crate::capabilities::config_path_of;
        let m = Manifest::from_embedded();
        let trigger = m.get("schema.mix_modes").expect("缺触发键项");
        let members = m.get("schema.mix_modes#members").expect("缺候选来源项");
        // 触发键项五c 后改走 `keys.key_actions`（key 退化为标识符），候选来源仍写
        // `schema.mix_modes` 本身——两项落点不同，更要各自独立成行、各注册各的 loader。
        assert_eq!(trigger.typ, "dialog_button_key_action");
        assert_eq!(trigger.verb.as_deref(), Some("mix:quick_mix"));
        assert_eq!(members.typ, "dialog_button_mix_members");
        assert_eq!(
            config_path_of("schema.mix_modes#members"),
            "schema.mix_modes",
            "视图 key 必须解析回真实配置路径"
        );
        // 两项都在「快捷输入」分区，且各自独立成行（不是同一个对话框的两段）
        assert_eq!(trigger.section, "快捷输入");
        assert_eq!(members.section, "快捷输入");
    }

    /// 守护：每个候选来源都带说明文字——条目开关式对话框的意义就在于放得下说明，
    /// 缺说明的项会渲染成一行光秃秃的开关。
    #[test]
    fn mix_members_options_all_have_hints() {
        let m = Manifest::from_embedded();
        let item = m.get("schema.mix_modes#members").expect("清单缺候选来源项");
        for o in &item.options {
            assert!(!o.hint.is_empty(), "候选来源 `{}` 缺说明文字", o.value);
        }
    }

    /// 守护：三个引导键控件都声明了 `verb`。
    ///
    /// `dialog_button_key_action` 的写入路径由 `verb` 决定，缺了它控件会直接跳过——
    /// 表现是「那个对话框点开能勾，保存后什么也没发生」。清单里漏写不会有编译错误，
    /// 故在此拦一道。
    #[test]
    fn key_action_items_declare_verb() {
        let m = Manifest::from_embedded();
        let mut found = 0;
        for key in m.order.iter() {
            let it = m.get(key).expect("order 里的 key 必在 items 中");
            if it.typ == "dialog_button_key_action" {
                found += 1;
                let verb = it.verb.as_deref().unwrap_or("");
                assert!(!verb.is_empty(), "清单项 {key} 缺 verb，控件会静默跳过");
            }
        }
        assert!(
            found >= 3,
            "应至少有临拼/临英/快捷输入三个引导键控件，实际 {found}"
        );
    }

    /// ★★ 「恢复默认」灌回 loader 的那份配置必须**折算**，否则引导键类字段一恢复就全空。
    ///
    /// 现场（2026-08-22 用户报）：设置「输入」页点底部「恢复本页」，快捷输入触发键 `;`
    /// 消失——而**清空 trigger_keys 正是 core 认定的「禁用快捷输入」**，一个写着「恢复」
    /// 的按钮把功能关掉了。
    ///
    /// 本测试把 bug 与修复摆在一起看。
    #[test]
    fn reset_defaults_seed_binds_factory_trigger_keys() {
        // bug 的成因：出厂值住在 `schema.mix_modes[].trigger_keys` 这类**被折算侧**，
        // 而 `getDefaults` 返回未折算的系统预置 ⇒ loader 读的 `keys.key_actions` 是空的。
        let raw = Config::from_value(serde_json::json!({}));
        assert!(
            key_action_keys_of(&raw, "mix:quick_mix").is_empty(),
            "未补绑定时 key_actions 为空——这正是「一恢复就没了」的成因"
        );

        // 修复：按清单声明把出厂绑定补回去。
        let seed = key_action_default_bindings_from(
            &Manifest::from_embedded(),
            &crate::capabilities::Capabilities::from_embedded(),
        );
        assert!(
            seed.contains(&("semicolon".to_string(), "mix:quick_mix".to_string())),
            "快捷输入应绑回分号，实际 {seed:?}"
        );
        assert!(
            seed.contains(&("backtick".to_string(), "temp_pinyin".to_string())),
            "临时拼音应绑回反引号，实际 {seed:?}"
        );

        // ⚠️ 出厂绑定之间不得撞键：补进 map 时是后写覆盖先写，而 core 的折算是**先到先得**，
        // 撞了两边胜者相反。出厂值不撞，这条只是把前提钉住。
        let mut ks: Vec<&String> = seed.iter().map(|(k, _)| k).collect();
        let n = ks.len();
        ks.sort();
        ks.dedup();
        assert_eq!(ks.len(), n, "出厂绑定不应撞键，实际 {seed:?}");
    }

    /// ★ 「恢复默认」也不能用**纯 L1**：出厂经 `data/config.toml`（L2）置开的项会被误关。
    ///
    /// core 的 `config.getDefaults` 早就为此从纯 L1 改成 `system_preset_value`
    /// （wind-rpc `dispatch.rs` 那段注释点名了顶码上屏与拼音自动学习），但设置端的
    /// 「恢复本页」当时漏改，一直拿 `Config::from_value({})` 当默认。
    #[test]
    fn reset_defaults_must_come_from_system_preset_not_bare_l1() {
        let caps = crate::capabilities::Capabilities::from_embedded();
        // capability 默认 ＝ core 的 system_preset（L1 ⊕ data/config.toml）：出厂是开的。
        assert_eq!(
            caps.default_for("schema.codetable.top_code_commit"),
            serde_json::json!(true),
            "顶码上屏出厂应为开"
        );
        // 纯 L1 却是关的 —— 拿它当「默认」会把顶码上屏静默关掉。
        let l1 = wind_config::config::Config::default();
        assert!(
            !l1.schema.codetable.top_code_commit,
            "纯 L1 与出厂值分叉，正是不能拿它当默认的理由"
        );
    }

    /// ★★ 「恢复默认」必须勾回 core 的出厂绑定，不能一律清空。
    ///
    /// 现场（2026-08-22 用户报）：快捷输入点「恢复默认」后触发键为空。而**清空
    /// trigger_keys 正是 core 认定的「禁用快捷输入」** ⇒ 一个本意是「回到出厂」的按钮，
    /// 把功能整个关掉了。原因是控件把默认集写死成 `Vec::new()`。
    ///
    /// ⚠️ 判据取自 capability 快照（它由 core 的 `system_preset_value` 重生成），所以
    /// core 改了出厂值这条会红——那正是要的：出厂值变了就该有人来确认这里。
    #[test]
    fn key_action_reset_restores_core_factory_bindings() {
        let caps = crate::capabilities::Capabilities::from_embedded();
        let d = |k: &str, verb: &str| key_action_defaults_from(&caps.default_for(k), verb);

        // structlist：出厂 `;`。这一项的默认值**不在 data/config.toml 里**（预置文件刻意
        // 不写 mix_modes），只在 `Config::default()` 的 default_mix_modes() 里。
        assert_eq!(
            d("schema.mix_modes", "mix:quick_mix"),
            vec!["semicolon".to_string()],
            "快捷输入恢复默认应勾回分号"
        );
        // strlist：出厂反引号。
        assert_eq!(
            d("input.temp_pinyin.trigger_keys", "temp_pinyin"),
            vec!["backtick".to_string()],
            "临时拼音恢复默认应勾回反引号"
        );
        // 出厂就不绑 ⇒ 空是**正确答案**，不是漏读。留着这条免得日后有人看见空就去"修"。
        assert!(
            d("input.temp_english.trigger_keys", "temp_english").is_empty(),
            "临时英文出厂不绑引导键"
        );
        // 实例 id 对不上时不得瞎猜，宁可空。
        assert!(
            d("schema.mix_modes", "mix:not_installed").is_empty(),
            "找不到对应实例应返回空，不能回落到数组首项"
        );
    }

    /// 恢复默认要勾的键，必须都在该项的 options 里——否则那个复选框根本不存在，
    /// 点了「恢复默认」看起来仍是全空，与这次的 bug 完全同形却更难查。
    #[test]
    fn key_action_defaults_are_all_offered_as_options() {
        let caps = crate::capabilities::Capabilities::from_embedded();
        let m = Manifest::from_embedded();
        let mut checked = 0;
        for key in m.order.iter() {
            let it = m.get(key).expect("order 里的 key 必在 items 中");
            if it.typ != "dialog_button_key_action" {
                continue;
            }
            let verb = it.verb.as_deref().unwrap_or_default();
            let defs = key_action_defaults_from(&caps.default_for(&it.key), verb);
            for d in &defs {
                assert!(
                    it.options.iter().any(|o| &o.value == d),
                    "清单项 {key} 的出厂键 {d:?} 不在 options 里，恢复默认勾不上"
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "至少应校验到一个出厂绑定，否则本测试恒真");
    }

    #[test]
    fn from_embedded_parses_groups_and_items() {
        let m = Manifest::from_embedded();
        // 6 个分组：schema/input/keys/ui/general/advanced
        assert_eq!(m.groups.len(), 6, "应解析出 6 个分组");
        // 代表性 key 存在且类型/标签正确
        let it = m
            .get("input.auto_pair.chinese")
            .expect("应含 input.auto_pair.chinese");
        assert_eq!(it.typ, "toggle");
        assert_eq!(it.group, "input");
        assert!(!it.label.is_empty(), "label 不应为空");
        // 至少 70 个配置项
        assert!(
            m.order.len() >= 70,
            "items 数量应 >= 70，实际 {}",
            m.order.len()
        );
    }

    /// ★★★ 不变量：**整表覆盖 `keys.key_actions` 的 writer 最多一个**。
    ///
    /// # 这条守的是什么
    ///
    /// 该键一度有两个各自持有副本、且都整表覆盖的 writer：
    /// - `state::register_schema_registrations` 那个（源：`SchemaManagerState::key_actions`，
    ///   方案对话框改热键/引导键时实时更新）；
    /// - 本文件 `build_dialog_button_key_actions`（源：控件自己的 `KeyActionFields`，
    ///   **设置页启动时播种一次、之后再没更新过**）。
    ///
    /// 二者按注册顺序执行，后者排在后面 ⇒ 它那份旧快照每次保存都把方案对话框的改动原样
    /// 盖回去。真机表现：清除/重设方案切换热键后保存提示「没有变更」，重开对话框那条还在，
    /// 且 diff 恒为 0（写回 current 的正是 base 的原值）。已改为单一真相源。
    ///
    /// # 判据为什么从「整表覆盖」升级成「覆盖式写入」
    ///
    /// 上一版的原话是「多个读-改-写共存是设计意图，危险的只有整表覆盖」，并据此把三个
    /// 引导键控件（`build_dialog_button_key_action`，单数）判为无害。**那个判定是错的**，
    /// 2026-08-31 由用户报障推翻：在全局表里删掉 `` ` `` → 临时拼音，保存提示「没有变更」，
    /// 绑定还在。那三个控件确实读了旧值，但接着做的是 `retain(v != verb)` ——
    /// **按动词整段覆盖**，一样会删掉不是自己写进去的条目，只是覆盖范围小一圈。
    ///
    /// 而本测试的哨兵是一个**不属于任何动词**的键，切片覆盖者不碰它 ⇒ 恰好被放行。
    ///
    /// ⇒ 判据改为：**至多一个覆盖式写入者**。判据不是「覆盖多大范围」，而是
    /// **「它会不会删掉不是自己写进去的条目」**。两个哨兵一起下探区分得出来：
    ///
    /// | | 无主哨兵（`none`） | 带动词哨兵（`temp_pinyin`，键在 options 内） |
    /// |---|---|---|
    /// | 整表覆盖者 | 抹掉 | 抹掉 |
    /// | 切片覆盖者 | 留着 | **抹掉** ← 差集即它 |
    /// | 真·读-改-写 | 留着 | 留着 |
    ///
    /// 故断言是「两个集合相等且 ≤ 1」，不是各查各的。
    #[test]
    fn at_most_one_writer_overwrites_key_actions() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let app = Rc::new(RefCell::new(crate::state::AppState::empty()));
        let st = app.borrow();
        // 复现 shell::build 的注册顺序：先清空、再补方案页那批、最后建设置树。
        st.fields.reset();
        crate::state::register_schema_registrations(&st.fields, &st.schema_mgr);
        let _tabs = crate::pages::all_tabs(&app, &st);

        const SENTINEL: &str = "__probe_sentinel__";
        // ⚠️ 带动词哨兵的**键必须在引导键控件的 options 里**（`slash` 是那 9 个符号键之一）。
        // 改造后的切片 writer 只摘自己列得出来的键，用 options 之外的键当哨兵抓不到它。
        const SENTINEL_KEY: &str = "slash";
        const SENTINEL_VERB: &str = "temp_pinyin";
        let probe = Config::from_value(serde_json::json!({
            "keys": { "key_actions": { SENTINEL: "none", SENTINEL_KEY: SENTINEL_VERB } }
        }));

        // 跑一个 writer，返回 (无主哨兵还在吗, 带动词哨兵还在吗)。
        let survives = |w: &crate::state::FieldWriter| {
            let mut c = probe.clone();
            w(&mut c);
            let o = c.get_object("keys.key_actions");
            (
                o.as_ref().is_some_and(|o| o.contains_key(SENTINEL)),
                o.as_ref()
                    .and_then(|o| o.get(SENTINEL_KEY))
                    .and_then(|v| v.as_str())
                    == Some(SENTINEL_VERB),
            )
        };
        let scan = |ws: &[crate::state::FieldWriter]| -> (Vec<usize>, Vec<usize>) {
            let (mut whole, mut any) = (vec![], vec![]);
            for (i, w) in ws.iter().enumerate() {
                let (free_alive, verb_alive) = survives(w);
                if !free_alive {
                    whole.push(i);
                }
                if !free_alive || !verb_alive {
                    any.push(i);
                }
            }
            (whole, any)
        };

        let (wholesale, overwriting) = scan(&st.fields.writers.borrow());
        assert!(
            wholesale.len() <= 1,
            "有 {} 个 writer 整表覆盖 keys.key_actions（下标 {:?}）——它们按注册顺序互相踩，\
             后跑的那个用自己那份副本把先跑的改动盖掉。该键必须只有一个整表写入者。",
            wholesale.len(),
            wholesale
        );
        assert_eq!(
            overwriting,
            wholesale,
            "下标 {:?} 的 writer 抹掉了带动词的哨兵却留下无主哨兵 = **切片覆盖者**\
             （多半是 `retain(v != verb)` 那种「先摘自己的动词再插回勾选项」的写法）。\
             它与整表写入者按注册顺序互相踩，后跑的赢，表现是「在另一个入口删掉的绑定\
             保存时被写回、提示没有变更」。改为同步到 `SchemaManagerState::key_actions`\
             这一个真相源，由那一个 writer 落盘。",
            overwriting
                .iter()
                .filter(|i| !wholesale.contains(i))
                .collect::<Vec<_>>()
        );

        // 反事实自检：本测试若恒真就毫无价值（上一版正是在这里漏掉了三个 writer）。
        // 造一个切片覆盖者推进去，断言这次**检得出来**。
        {
            let mut ws: Vec<crate::state::FieldWriter> = Vec::new();
            ws.push(Box::new(|c: &mut Config| {
                let mut map: serde_json::Map<String, Value> = c
                    .get_path(KEY_ACTIONS_PATH)
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();
                map.retain(|_, v| v.as_str() != Some(SENTINEL_VERB));
                c.set_path(KEY_ACTIONS_PATH, Value::Object(map));
            }));
            let (whole, over) = scan(&ws);
            assert!(whole.is_empty(), "切片覆盖者不该被无主哨兵检出");
            assert_eq!(
                over,
                vec![0],
                "带动词哨兵必须检出切片覆盖者，否则本测试恒真"
            );
        }
    }

    /// ★★ 端到端回归：方案对话框删掉的热键，跑完**全部** writer 后不得复活。
    ///
    /// 精确复刻真机时序（用户报障现场）：
    /// 1. 设置页启动，全局「自定义按键」控件按当时的表播种；
    /// 2. 用户在方案对话框里清除某方案的切换热键 ⇒ 只有 `SchemaManagerState::key_actions`
    ///    变了，全局控件那份副本**不会**跟着变；
    /// 3. 点保存 ⇒ 跑全部 writer。
    ///
    /// 修复前第 3 步会被全局控件的整表覆盖盖回旧值，diff 因此恒为 0、提示「没有变更」。
    #[test]
    fn schema_hotkey_removal_survives_all_writers() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let app = Rc::new(RefCell::new(crate::state::AppState::empty()));
        let st = app.borrow();

        // ① 启动态：表里有一条五笔的切换热键。
        {
            let mut ka = st.schema_mgr.key_actions.borrow_mut();
            ka.clear();
            ka.insert("backtick".into(), "temp_pinyin".into());
            ka.insert("ctrl+shift+q".into(), "switch_schema:wubi86".into());
        }
        st.schema_mgr.key_actions_loaded.set(true);

        // 建设置树：全局控件在此按①播种（含 ctrl+shift+q）。
        st.fields.reset();
        crate::state::register_schema_registrations(&st.fields, &st.schema_mgr);
        let _tabs = crate::pages::all_tabs(&app, &st);

        // ② 用户在方案对话框里清除该热键——只动唯一真相源。
        st.schema_mgr
            .key_actions
            .borrow_mut()
            .retain(|_, v| v != "switch_schema:wubi86");

        // ③ 保存：跑全部 writer。
        let mut probe = Config::from_value(serde_json::json!({
            "keys": { "key_actions": {
                "backtick": "temp_pinyin",
                "ctrl+shift+q": "switch_schema:wubi86",
            } }
        }));
        for w in st.fields.writers.borrow().iter() {
            w(&mut probe);
        }

        let survived = probe
            .get_object("keys.key_actions")
            .is_some_and(|o| o.contains_key("ctrl+shift+q"));
        assert!(
            !survived,
            "被删的方案热键在跑完 writer 后复活了 ⇒ 又有 writer 拿旧副本整表覆盖。\
             最终表: {:?}",
            probe.get_object("keys.key_actions")
        );
    }

    /// ★★ 端到端回归（同族第二例）：在**全局按键表**里删掉的引导键，跑完全部 writer 不得复活。
    ///
    /// 与上一条同形，换成用户 2026-08-31 报的那个入口：
    /// 1. 设置页启动，临拼触发键控件按当时的表播种（勾着 `` ` ``）；
    /// 2. 用户在全局「自定义按键」里删掉 `` ` `` → 临时拼音，effect 同步进真相源；
    /// 3. 点保存 ⇒ 跑全部 writer。
    ///
    /// 修复前第 3 步会被临拼控件那份 `retain(v != "temp_pinyin")` + 插回勾选项的**切片
    /// 覆盖** writer 把 `` ` `` 写回去，diff 恒为 0 ⇒「没有变更」，而绑定确实还在。
    ///
    /// ⚠️ 这条与 [`Self::at_most_one_writer_overwrites_key_actions`] 不重复：那条守的是
    /// 「有没有第二个覆盖式写入者」的结构不变量，这条守的是**用户那条路径的实际结果**。
    /// 结构测试再严，也答不出「按这个顺序走一遍，最后表里还剩什么」。
    #[test]
    fn global_table_deletion_of_trigger_key_survives_all_writers() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let app = Rc::new(RefCell::new(crate::state::AppState::empty()));
        let st = app.borrow();

        // ① 启动态：临拼绑在反引号上（物化后的出厂形态），快捷输入绑在分号上。
        {
            let mut ka = st.schema_mgr.key_actions.borrow_mut();
            ka.clear();
            ka.insert("backtick".into(), "temp_pinyin".into());
            ka.insert("semicolon".into(), "mix:quick_mix".into());
        }
        st.schema_mgr.key_actions_loaded.set(true);

        // 建设置树：三个引导键控件在此按①播种（临拼那个勾着 backtick）。
        st.fields.reset();
        crate::state::register_schema_registrations(&st.fields, &st.schema_mgr);
        let _tabs = crate::pages::all_tabs(&app, &st);

        // ② 用户在全局表里删掉那条——全局编辑器的 effect 已把结果落到唯一真相源。
        st.schema_mgr
            .key_actions
            .borrow_mut()
            .retain(|k, _| k != "backtick");

        // ③ 保存：跑全部 writer。
        let mut probe = Config::from_value(serde_json::json!({
            "keys": { "key_actions": {
                "backtick": "temp_pinyin",
                "semicolon": "mix:quick_mix",
            } }
        }));
        for w in st.fields.writers.borrow().iter() {
            w(&mut probe);
        }

        let final_map = probe.get_object("keys.key_actions");
        assert!(
            !final_map
                .as_ref()
                .is_some_and(|o| o.contains_key("backtick")),
            "在全局表里删掉的临拼触发键复活了 ⇒ 仍有控件拿自己那份勾选快照做切片覆盖。\
             最终表: {final_map:?}"
        );
        // 反向对照：没被删的那条必须还在。少了它，把 writer 全删光也能让上面那条绿。
        assert_eq!(
            final_map
                .as_ref()
                .and_then(|o| o.get("semicolon"))
                .and_then(|v| v.as_str()),
            Some("mix:quick_mix"),
            "未受影响的绑定被一起抹掉了"
        );
    }

    /// 只读组：同域的专用字段都要列出来，未设置的不列，**模板不展开**。
    #[test]
    fn foreign_bindings_list_same_domain_fields_only() {
        let cfg = Config::from_value(serde_json::json!({
            "keys": {
                "toggle_mode_keys": ["lshift", "rshift"],
                "switch_engine": "ctrl+shift+e",
                "pin_candidate": "ctrl+number",
                // 下面两条是「未设置」的两种写法，都不该占一行——列出来等于告诉用户
                // 这个键被占了，而它根本没绑。
                "toggle_punct": "none",
                "open_settings": "",
                // 会话态那张表的来源：**不属于本域**，混进来会让人以为能在这里改。
                "page_keys": ["pageupdown", "minus_equal"],
            }
        }));
        let rows = foreign_key_bindings(&HashMap::new(), &cfg);
        let keys: Vec<&str> = rows.iter().map(|r| r.keys.as_str()).collect();

        assert!(
            keys.contains(&"左 Shift、右 Shift"),
            "中英切换那组没列出来——它正是用户「在全局表里看不见、于是又配一个左 Shift」\
             的那一条。实际: {keys:?}"
        );
        assert!(
            keys.contains(&"Ctrl+Shift+E"),
            "组合键字段漏了。实际: {keys:?}"
        );
        assert!(
            keys.contains(&"Ctrl+数字"),
            "置顶模板应按字段占一行。实际: {keys:?}"
        );
        assert!(
            !keys.iter().any(|k| k.starts_with("Ctrl+1")),
            "模板被展开成 10 行了——14 行会变成 32 行，「控制列表长度」这条要求就废了"
        );
        assert_eq!(
            rows.len(),
            3,
            "未设置的字段（none / 空串）或会话态键组混进来了。实际: {keys:?}"
        );
    }

    /// 会话态只读组：候选操作四组，显示按组、比对按具体键。
    #[test]
    fn session_foreign_bindings_show_groups_but_compare_keys() {
        let cfg = Config::from_value(serde_json::json!({
            "keys": {
                "select_key_groups": ["semicolon_quote"],
                "page_keys": ["pageupdown", "minus_equal"],
                "highlight_keys": ["arrows", "tab"],
                // 出厂为空 ⇒ 不占一行（列出来等于说这个功能占了键，而它没有）。
                "select_char_keys": [],
                // 组合键那批属于**另一张表**，不该混进来。
                "switch_engine": "ctrl+shift+e",
                "toggle_mode_keys": ["lshift"],
            }
        }));
        let rows = session_foreign_bindings(&HashMap::new(), &cfg);
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["次选/三选键", "翻页键", "高亮移动键"],
            "范围错了：只该收「候选操作」那四组里已设置的，组合键/中英切换属另一张表"
        );

        let page = &rows[1];
        assert_eq!(page.keys, "PageUp/PageDown、- =", "显示应按组，不逐键铺开");
        // 比对要的是具体键——显式表里配的是 `minus`，与组名 `minus_equal` 字面不相等。
        for k in ["pageup", "pagedown", "minus", "equal"] {
            assert!(
                page.tokens.iter().any(|t| t == k),
                "翻页组的比对 token 缺 {k:?}，用户在上面配了它也标不出橙色。实际: {:?}",
                page.tokens
            );
        }
        assert!(
            !page.tokens.iter().any(|t| t == "minus_equal"),
            "组名混进了比对 token —— 它永远匹配不上任何显式键，等于这一组恒不标橙"
        );
        // 高亮那组的 tab 展开成 tab + shift+tab（core 的 highlight_key_group_binds）。
        assert!(
            rows[2].tokens.iter().any(|t| t == "shift+tab"),
            "高亮组漏了 shift+tab。实际: {:?}",
            rows[2].tokens
        );
    }

    /// ★★ 锁住四个「全局方案配置」对话框各落在哪种角标模式。
    ///
    /// 判据是现算的（`schema_hint_mode`），好处是 core 增删可覆盖项时界面自动适配，
    /// 代价是**行为会随数据翻转**——某天跨过阈值，一个对话框从「十几个角标」变成
    /// 「一句说明」，没人会注意到。本测试就是那个通知：它红的时候正是该人看一眼的时候。
    ///
    /// 实测比例（2026-08-31）：码表 18/20、混输 0/9、拼音 2/14、英文 0/8。差别这么大，
    /// 所以既不能一刀切、也不能按对话框硬编码。
    #[test]
    fn scheme_config_dialogs_pick_expected_hint_mode() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let st = crate::state::AppState::empty();
        let keys_of = |sec: &str| -> Vec<String> {
            st.manifest
                .sections_of("schema")
                .into_iter()
                .find(|(s, _)| s == sec)
                .map(|(_, k)| k)
                .unwrap_or_default()
        };
        for (sec, want, why) in [
            (
                "上屏行为",
                SchemaHintMode::Header,
                "码表 18/20 可覆盖——逐行角标每行都有，等于每行都没有",
            ),
            ("混输行为", SchemaHintMode::PerRow, "0 项可覆盖，无标可加"),
            (
                "候选行为",
                SchemaHintMode::PerRow,
                "拼音 2/14——这时角标标的是例外，有信息量",
            ),
            ("英文行为", SchemaHintMode::PerRow, "0 项可覆盖"),
        ] {
            let keys = keys_of(sec);
            assert!(!keys.is_empty(), "section {sec:?} 在清单里没了");
            assert_eq!(
                schema_hint_mode(&st, &keys),
                want,
                "{sec} 的角标模式变了（{why}）。core 增删了可覆盖项跨过阈值 ⇒ 去看一眼                 那个对话框现在长什么样，确认新模式合适再改本断言"
            );
        }
        // Header 模式必须取得到统一说明文案，否则那一组既没角标也没说明 = 信息凭空消失。
        let note = schema_hint_header_note(&st, &keys_of("上屏行为"));
        assert!(
            note.as_deref().is_some_and(|n| n.contains("覆盖")),
            "Header 模式取不到 note：{note:?}"
        );
    }

    /// 只读组的「去哪儿改」从清单现算，不另写一份位置表。
    ///
    /// 守的是「设置项挪了分组，指路还指向老地方」——那种错不会报任何异常，用户按提示
    /// 翻过去找不到，只会以为自己看漏了。
    #[test]
    fn foreign_binding_source_comes_from_manifest() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let st = crate::state::AppState::empty();
        let src = foreign_sources(&st);
        let cfg = Config::from_value(serde_json::json!({
            "keys": { "toggle_mode_keys": ["lshift"] }
        }));
        let rows = foreign_key_bindings(&src, &cfg);
        let row = rows.first().expect("中英切换那行应存在");
        assert!(
            row.source.contains('›'),
            "指路文案应是「分组 › 小节」，实际 {:?}——查不到位置说明 \
             `keys.toggle_mode_keys` 不在清单里了，那时整条指路会静默变成空白",
            row.source
        );
    }
}
