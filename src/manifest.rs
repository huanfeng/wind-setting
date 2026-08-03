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
use crate::state::{AppState, SchemaInfo};
use crate::widgets::{setting_row, setting_row_checks};

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
    /// hotkey 专用：该功能是否支持「全局热键」（编辑对话框显示「全局」勾选）。
    pub global: bool,
    /// 子对话框分组：成员项标记同一 id，收进对话框渲染、不在分区内独立成行。
    pub dialog_group: Option<String>,
    /// 子对话框开启项：宿主控件旁挂「扩展设置」按钮，打开 dialog_group==此 id 的成员对话框。
    pub opens_dialog: Option<String>,
    /// 对话框内子分区（如"词频调整"/"自动造词"），同值的项聚合在同一子标题下。
    pub subsection: Option<String>,
    /// 隐藏此项（未完全实现或不对最终用户开放），不渲染也不注册 writer/loader。
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
                    global: it.get("global").and_then(|x| x.as_bool()).unwrap_or(false),
                    dialog_group: opt_str(it, "dialog_group"),
                    opens_dialog: opt_str(it, "opens_dialog"),
                    subsection: opt_str(it, "subsection"),
                    hidden: it.get("hidden").and_then(|x| x.as_bool()).unwrap_or(false),
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
        let row = if item.opens_dialog.is_some() {
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
        "dialog_button_mix_trigger" | "dialog_button_mix_members" => {
            build_dialog_button(state, key, item, cfg)
        }
        other => Element::label(format!("[{other}?]")).fg_role(Role::TextMuted),
    }
}

/// 清单缺失该 key 时返回占位行（便于发现 key 漂移）。
pub fn field(state: &AppState, key: &str) -> Element {
    let Some(item) = state.manifest.get(key) else {
        return setting_row(
            key,
            Some("（清单缺失此项）"),
            Element::label("—").fg_role(Role::TextMuted),
        );
    };
    let cfg = &state.current_config;
    let control = build_control(state, key, item, cfg);
    // 多选键组用专用行（固定宽标签 + 占满网格），避免宽网格把组名挤掉。
    let mut row = if item.typ == "checkbox_group" {
        setting_row_checks(&item.label, item.hint.as_deref(), control)
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

fn build_select(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    // 响应式动态源（异步加载的主题/字体）：选项绑定 Signal，数据到达即填充下拉，
    // 避免静态构建在数据到达前拿到空列表显示「（无可选项）」。schemas 仍走下方静态路径。
    if let Some(src) = item.options_source.as_deref()
        && let Some((val_sig, lab_sig)) = reactive_option_signals(state, src)
    {
        return build_select_reactive(state, key, item, cfg, val_sig, lab_sig);
    }
    let (values, labels): (Vec<String>, Vec<String>) = if let Some(src) = &item.options_source {
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
        Element::dropdown_items_reactive(state.theme_items_sig, sig)
    } else {
        Element::dropdown_reactive(lab_sig, sig)
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
    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            let v = sig.get();
            let val = if v.fract() == 0.0 {
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
        .enabled_when(move || enabled_sig.get())
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
            Element::label_rc(disp)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .max_lines(1)
                .width(114)
                .enabled_when(move || enabled_sig.get()),
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
        values.push(s.id.clone());
        labels.push(s.name.clone());
    }
    (values, labels)
}

/// 把对话框主体挂到根（登记到注册表，`shell::build` 取出挂到根窗口 stack 实现全窗模态）。
fn register_dialog(state: &AppState, dlg: Element) {
    state.fields.dialogs.borrow_mut().push(dlg);
}

/// 模糊音紧凑组：启用开关 + 11 对小号 checkbox（2 列），随启用置灰。
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
    let cells: Vec<Element> = pairs
        .iter()
        .map(|(k, lbl)| {
            let s = register_toggle(state, k, cfg);
            Element::checkbox(*lbl, s).small().fg_role(Role::Text)
        })
        .collect();
    let grid = Element::grid(2, 10, cells)
        .width_match()
        .enabled_when(move || en.get());
    Element::col()
        .width_match()
        .padding_xy(0, 11)
        .spacing(10)
        .child(setting_row(
            "启用模糊音",
            Some("勾选下方拼音对，使其互不区分"),
            Element::switch(en),
        ))
        .child(grid)
}

/// 对话框内分组标题（section/subsection 同一层级，主题色强调，不额外缩进）。
pub(crate) fn dialog_group_header(name: &str) -> Element {
    Element::label(name)
        .font_size(11.5)
        .font_weight(600)
        .fg_role(Role::Accent)
        .width_match()
        .padding_xy(0, 5)
}

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
            tmp.pop().unwrap_or_else(|| field(state, k))
        } else if item.typ == "dialog_button"
            && let Some(g) = &item.gate_key
            && state.manifest.get(g).is_some()
        {
            continue;
        } else {
            field(state, k)
        };
        rows.push(row);
    }
    if with_fuzzy {
        rows.push(dialog_group_header("模糊音"));
        rows.push(build_fuzzy_group(state, &state.current_config));
    }

    // 恢复默认：调用 config.getDefaults RPC 取 core 权威默认值，灌回各字段 loader。
    // 若 core 未连接或接口返回错误，则回落空对象（等效 capability fallback）。
    let fields = state.fields.clone();
    let mut reset_keys = keys.clone();
    if with_fuzzy {
        for k in state.manifest.order.iter() {
            if k.starts_with("schema.pinyin.fuzzy.") {
                reset_keys.push(k.clone());
            }
        }
    }
    let reset_fn: Box<dyn Fn() + 'static> = Box::new(move || {
        let defaults = crate::rpc::call_core("config.getDefaults", &serde_json::json!({}))
            .unwrap_or(Value::Object(serde_json::Map::new()));
        let def_cfg = Config::from_value(defaults);
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
    if item.typ == "dialog_button_mix_trigger" {
        build_dialog_button_mix_trigger(state, item, cfg)
    } else if item.typ == "dialog_button_mix_members" {
        build_dialog_button_mix_members(state, key, item, cfg)
    } else if key == "input.punct.custom_mappings" {
        // 自定义标点映射：map 值为 [中半,英全,中全,英半] 四元数组（core 存储序），专用 5 列表格对话框。
        build_dialog_button_punct(state, key, cfg)
    } else if key == "ui.candidate.index_labels" {
        // 自定义序号标签：字符串拆成 10 个序号槽位，专用 10 框对话框。
        build_dialog_button_index_labels(state, key, cfg)
    } else if key == COMMENT_HOST_KEY {
        // 候选注释：横/纵两份模板 + 最大字数 + 可复制的模板参考，共用一个对话框。
        build_dialog_button_comment(state, key, cfg)
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
        open_dialog_button(visible).enabled_when(move || gate_sig.get())
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
    let members: Vec<String> = state
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
        .collect();
    if members.is_empty() {
        return None;
    }
    let mut col = Element::col().width_match().spacing(6);
    for k in &members {
        col = col.child(field(state, k));
    }
    Some(col)
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

    // 恢复默认：与 build_gated_dialog_row 同法，取 core 权威默认值灌回三个字段的 loader。
    // 走 loader 而非直接 set signal，寄居项才能一并复位（它们的 signal 本函数拿不到）。
    let fields = state.fields.clone();
    let reset_keys: Vec<String> = std::iter::once(key.to_string())
        .chain(
            state
                .manifest
                .order
                .iter()
                .filter(|k| {
                    state
                        .manifest
                        .get(k)
                        .and_then(|i| i.dialog_extra_for.as_deref())
                        == Some(key)
                })
                .cloned(),
        )
        .collect();
    let reset_fn: Box<dyn Fn() + 'static> = Box::new(move || {
        let defaults = crate::rpc::call_core("config.getDefaults", &serde_json::json!({}))
            .unwrap_or(Value::Object(serde_json::Map::new()));
        let def_cfg = Config::from_value(defaults);
        let ld = fields.loaders.borrow();
        for k in &reset_keys {
            if let Some(loader) = ld.get(k) {
                loader(&def_cfg);
            }
        }
    });

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

/// 快捷输入触发键：`schema.mix_modes` 是整体读写的 structlist，实际要改的是数组里
/// `id=="quick_mix"` 那一项的 `trigger_keys` 字段，走非声明式读写（区别于通用 `build_dialog_button_pairs`
/// 假设 key 直接指向顶层 strlist/map 的情形）。
const QUICK_MIX_ID: &str = "quick_mix";
/// `quick_mix` 找不到该项时的回落触发键，对齐主仓 wind-config `default_mix_modes()` 给
/// `quick_mix` 定的默认值。注意仅用于「整个 quick_mix 项都找不到」（`None`）的场景，
/// 不用于「trigger_keys 显式为空数组」（`Some(vec![])`，用户故意清空）的场景。
const QUICK_MIX_DEFAULT_TRIGGER: &str = "semicolon";

fn build_dialog_button_mix_trigger(state: &AppState, item: &MItem, cfg: &Config) -> Signal<bool> {
    use crate::dialogs::field_dialogs::build_pairs_dialog;
    use crate::mix_trigger::{mix_mode_trigger_keys, patch_mix_mode_trigger_keys};

    let mix_modes_path = "schema.mix_modes";
    // 注意：这里的 cfg 可能是 AppState::empty() 的占位配置（main.rs 里 shell::build 先于
    // 后台异步 LoadedState::fetch 完成就同步执行，UI 立即可显示、真实数据稍后经 apply_loaded
    // 回填），此时 quick_mix 大概率还找不到。writer/loader 必须**无条件注册**（不能像早期实现
    // 那样 `if found` 才注册）——否则等真实配置到达、loader 本该被 apply_loaded 统一调用去刷新
    // Signal 时，这个字段的 loader 根本没登记过，永远显示/保存不出真实值。
    let cur_mix_modes: Vec<Value> = cfg.get_array(mix_modes_path).cloned().unwrap_or_default();
    let cur: Vec<String> = mix_mode_trigger_keys(&cur_mix_modes, QUICK_MIX_ID)
        .unwrap_or_else(|| vec![QUICK_MIX_DEFAULT_TRIGGER.to_string()]);

    let opts = item.options.clone();
    let sigs: Vec<(String, Signal<bool>)> = opts
        .iter()
        .map(|o| (o.value.clone(), signal(cur.contains(&o.value))))
        .collect();
    let visible = signal(false);

    // writer：整体读出 schema.mix_modes，按 id 替换 trigger_keys，整体写回；找不到 quick_mix
    // 项时 patch_mix_mode_trigger_keys 返回 None，本次保存对这个字段静默无操作（不新建实例）。
    let sigs_w = sigs.clone();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            let selected: Vec<String> = sigs_w
                .iter()
                .filter(|(_, s)| s.get())
                .map(|(v, _)| v.clone())
                .collect();
            let arr = c.get_array(mix_modes_path).cloned().unwrap_or_default();
            if let Some(patched) = patch_mix_mode_trigger_keys(&arr, QUICK_MIX_ID, &selected) {
                c.set_path(mix_modes_path, Value::Array(patched));
            }
        }));

    // loader：apply_loaded/重新加载/重置时把 Signal 状态从当前 Config 灌回；找不到 quick_mix
    // 项时回落到 QUICK_MIX_DEFAULT_TRIGGER（对齐主仓 default_mix_modes 的默认触发键）。
    // 注册在**清单项 key** 下（触发键项就是 mix_modes_path 本身；候选来源项另用视图 key），
    // 两者互不覆盖。
    let sigs_l = sigs.clone();
    state.fields.loaders.borrow_mut().insert(
        mix_modes_path.to_string(),
        Box::new(move |c: &Config| {
            let arr = c.get_array(mix_modes_path).cloned().unwrap_or_default();
            let cur = mix_mode_trigger_keys(&arr, QUICK_MIX_ID)
                .unwrap_or_else(|| vec![QUICK_MIX_DEFAULT_TRIGGER.to_string()]);
            for (v, s) in &sigs_l {
                s.set(cur.iter().any(|k| k == v));
            }
        }),
    );

    let labels: Vec<String> = opts.iter().map(|o| o.label.clone()).collect();
    let hint = item.hint.clone().unwrap_or_default();
    // 对话框「恢复默认」目标：固定用 QUICK_MIX_DEFAULT_TRIGGER，不用 cur.clone()——cur 在占位
    // 配置阶段可能是空/临时值，不能当作「默认值」语义。
    let dlg = build_pairs_dialog(
        visible,
        &item.label,
        &hint,
        sigs,
        labels,
        vec![QUICK_MIX_DEFAULT_TRIGGER.to_string()],
    );
    register_dialog(state, dlg);
    visible
}

/// dialog_button_mix_members：条目开关式编辑内置「快捷」融合的候选来源（core 侧 `members`）。
///
/// core 把 members 当作候选来源的单一真相源——**有无即开关、顺序即优先级**。界面只管
/// 开关这一维，顺序由 `patch_mix_mode_members` 保序写回（不打乱手写配置，也不丢掉界面
/// 不认识的成员）；调整优先级留给直接编辑配置文件的高级用户。
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
    use crate::mix_members::{mix_mode_members, patch_mix_mode_members};

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
    let visible = signal(false);

    let sigs_w = sigs.clone();
    let managed_w = managed.clone();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            let selected: Vec<String> = sigs_w
                .iter()
                .filter(|(_, s)| s.get())
                .map(|(v, _)| v.clone())
                .collect();
            let arr = c.get_array(mix_modes_path).cloned().unwrap_or_default();
            if let Some(patched) = patch_mix_mode_members(&arr, QUICK_MIX_ID, &managed_w, &selected)
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
        }),
    );

    let hint = item.hint.clone().unwrap_or_default();
    let dlg = build_toggle_rows_dialog(visible, &item.label, &hint, rows, managed);
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
const PUNCT_DEFAULTS: &[(&str, &str, [&str; 4])] = &[
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
fn seed_cell(ov: Option<&Vec<Value>>, i: usize, def: &str) -> String {
    ov.and_then(|a| a.get(i))
        .and_then(|x| x.as_str())
        .map(String::from)
        .unwrap_or_else(|| def.to_string())
}

/// 自定义序号标签槽位数（序号 1-10）。
const INDEX_LABEL_SLOTS: usize = 10;

/// 序号标签拼接：从序号 1 起取各框首字符，**遇到第一个空框即停止**（前缀语义，
/// 避免「中间留空、后面有值」时把后续标签向前位移）。writer 与 getter 共用，保持一致。
fn collect_labels(inputs: &[Signal<String>]) -> String {
    inputs
        .iter()
        .map(|s| s.get())
        .take_while(|s| !s.is_empty())
        .filter_map(|s| s.chars().next())
        .collect()
}

/// dialog_button（ui.candidate.index_labels）：10 框对话框，每框一个字符，连续拼成标签串。
/// 字符串语义为前缀（char[i] = 序号 i+1 的标签），整体留空 = 默认 1-9。
fn build_dialog_button_index_labels(state: &AppState, key: &str, cfg: &Config) -> Signal<bool> {
    use crate::dialogs::field_dialogs::build_index_labels_dialog;

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
    let cur_chars: Vec<char> = cur.chars().collect();
    let inputs: Vec<Signal<String>> = (0..INDEX_LABEL_SLOTS)
        .map(|i| signal(cur_chars.get(i).map(|c| c.to_string()).unwrap_or_default()))
        .collect();
    let visible = signal(false);

    // writer：各框首字符拼接（遇空即停）→ 前缀串。
    let inputs_w = inputs.clone();
    let kw = key.to_string();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            c.set_path(&kw, Value::String(collect_labels(&inputs_w)));
        }));
    // loader：字符串按字符回填各框（多余截断、不足留空）。
    let inputs_l = inputs.clone();
    let kl = key.to_string();
    let def_l = def.clone();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let cur = c
                .get_str(&kl)
                .map(str::to_string)
                .unwrap_or_else(|| def_l.clone());
            let chars: Vec<char> = cur.chars().collect();
            for (i, sig) in inputs_l.iter().enumerate() {
                sig.set(chars.get(i).map(|c| c.to_string()).unwrap_or_default());
            }
        }),
    );
    // getter（一致性，与 writer 同拼接逻辑）。
    let inputs_g = inputs.clone();
    state.fields.getters.borrow_mut().insert(
        key.to_string(),
        Box::new(move || Value::String(collect_labels(&inputs_g))),
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
                if (0..4).any(|i| vals[i].as_str() != defs[i]) {
                    let arr: Vec<Value> = vals.iter().cloned().map(Value::String).collect();
                    m.insert((*tok).to_string(), Value::Array(arr));
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
    let dlg = build_punct_table_dialog(visible, "自定义标点映射", src_labels, cells, defaults);
    register_dialog(state, dlg);
    visible
}

/// `enabled_when` 里单个条件的比较算子。
///
/// 原先是 `contains: bool`。加 `!=` 时没有改成第二个布尔——两个布尔能表达四种组合，
/// 其中两种（既 contains 又 ne）没有意义，枚举才是恰好三态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CondOp {
    /// `key == value`
    Eq,
    /// `key != value`
    Ne,
    /// `key contains value`（key 为字符串数组时判是否含该元素）
    Contains,
}

/// `enabled_when` 里的单个条件。
#[derive(Debug, Clone, PartialEq)]
struct Cond {
    key: String,
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
fn parse_conds(expr: &str) -> Option<Vec<Cond>> {
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
fn cond_matches(cur: &Value, c: &Cond) -> bool {
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

    fn si(id: &str, kind: &str) -> SchemaInfo {
        SchemaInfo {
            id: id.to_string(),
            name: format!("{id}名"),
            kind: kind.to_string(),
            scheme: String::new(),
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
            assert_eq!(
                host.typ, "dialog_button",
                "`{k}` 寄居的宿主 `{host_key}` 必须是 dialog_button，实际 `{}`",
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
        assert_eq!(trigger.typ, "dialog_button_mix_trigger");
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

    /// 守护：`build_dialog_button_mix_trigger` loader 里 `mix_mode_trigger_keys(...).unwrap_or_else(...)`
    /// 那一行的回落语义——「恢复本页默认」用空 Config 跑 loader 时，quick_mix 整项找不到（`None`）应
    /// 回落到 semicolon，而不是清空；但若 quick_mix 项存在且 trigger_keys 显式为空数组（`Some(vec![])`，
    /// 用户故意清空），不应被强行改回 semicolon。
    #[test]
    fn mix_trigger_loader_fallback_semantics() {
        use crate::mix_trigger::mix_mode_trigger_keys;
        use serde_json::json;

        // 找不到 quick_mix 项（如「恢复本页默认」传入的空 Config）：回落 semicolon。
        let empty_arr: Vec<Value> = vec![];
        let cur = mix_mode_trigger_keys(&empty_arr, QUICK_MIX_ID)
            .unwrap_or_else(|| vec![QUICK_MIX_DEFAULT_TRIGGER.to_string()]);
        assert_eq!(cur, vec!["semicolon".to_string()]);

        // quick_mix 项存在但 trigger_keys 显式为空数组：尊重用户，不强行改回 semicolon。
        let arr_with_empty_trigger = vec![json!({
            "id": "quick_mix",
            "trigger_keys": []
        })];
        let cur = mix_mode_trigger_keys(&arr_with_empty_trigger, QUICK_MIX_ID)
            .unwrap_or_else(|| vec![QUICK_MIX_DEFAULT_TRIGGER.to_string()]);
        assert_eq!(cur, Vec::<String>::new());
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
}
