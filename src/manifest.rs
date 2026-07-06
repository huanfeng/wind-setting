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
use crate::state::AppState;
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
    /// dialog_button 专用：门控开关 key（该 toggle 关时按钮置灰不可点）。
    pub gate_key: Option<String>,
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
                let options = it
                    .get("options")
                    .and_then(|x| x.as_array())
                    .map(|opts| {
                        opts.iter()
                            .filter_map(|o| {
                                let value = o.get("value").and_then(|x| x.as_str())?.to_string();
                                let label = o
                                    .get("label")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or(&value)
                                    .to_string();
                                Some(MOption { value, label })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
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
            .and_then(parse_cond)
            .map(|(ck, _)| ck != *k && keys.iter().any(|x| x == &ck))
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
        "segmented" => build_segmented(state, key, item, cfg),
        "checkbox_group" => build_checkbox_group(state, key, item, cfg),
        "number" | "slider" => build_number(state, key, item, cfg),
        "number_select" => build_number_select(state, key, item, cfg),
        "string" => build_string(state, key, item, cfg),
        "hotkey" => build_hotkey(state, key, item, cfg),
        "dialog_button" => build_dialog_button(state, key, item, cfg),
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
    if let Some((ck, expect)) = item.enabled_when.as_deref().and_then(parse_cond) {
        let reg = state.fields.clone();
        // 联动用「置灰整行」而非隐藏：保留占位，避免分隔线残留与高度抖动。
        row = row.enabled_when(move || {
            let g = reg.getters.borrow();
            g.get(&ck)
                .map(|get| cond_matches(&get(), &expect))
                .unwrap_or(true)
        });
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
        .filter(|k| state.manifest.get(k).and_then(|i| i.dialog_group.as_deref()) == Some(group.as_str()))
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
    let row_control = Element::row().cross(Align::Center).spacing(10).child(control).child(btn);
    let mut row = setting_row(&item.label, item.hint.as_deref(), row_control);
    // 宿主 enabled_when 作用于整行（含扩展按钮）。
    if let Some((ck, expect)) = item.enabled_when.as_deref().and_then(parse_cond) {
        let reg = state.fields.clone();
        row = row.enabled_when(move || {
            let g = reg.getters.borrow();
            g.get(&ck).map(|get| cond_matches(&get(), &expect)).unwrap_or(true)
        });
    }
    row
}

fn build_toggle(state: &AppState, key: &str, _item: &MItem, cfg: &Config) -> Element {
    Element::switch(register_toggle(state, key, cfg))
}

/// 为某 bool key 建 Signal 并登记 writer/getter/loader，返回该 Signal。
/// 独立出来便于「开关 + 设置」合并行直接复用同一开关（见 [`build_gated_dialog_row`]）。
fn register_toggle(state: &AppState, key: &str, cfg: &Config) -> Signal<bool> {
    let def = state.capabilities.default_for(key).as_bool().unwrap_or(false);
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
    let def_str = state.capabilities.default_for(key).as_str().unwrap_or("").to_string();
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
        "themes"           => Some((state.theme_value_sig,          state.theme_label_sig)),
        "fonts"            => Some((state.font_sig,                  state.font_label_sig)),
        "codetableSchemas" => Some((state.codetable_schema_val_sig,  state.codetable_schema_lab_sig)),
        "pinyinSchemas"    => Some((state.pinyin_schema_val_sig,     state.pinyin_schema_lab_sig)),
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
    let def_str = state.capabilities.default_for(key).as_str().unwrap_or("").to_string();
    let cur = cfg.get_str(key).map(str::to_string).unwrap_or_else(|| def_str.clone());
    // 初始索引：在当前（可能为空）值列表中定位；空则 0，待 loader 在数据到达后重定位。
    let idx = val_sig.with(|v| v.iter().position(|x| *x == cur).unwrap_or(0));
    let sig = signal(idx);

    // writer：按当前索引取值；列表为空（未加载）时不写，避免清空配置。
    let kw = key.to_string();
    state.fields.writers.borrow_mut().push(Box::new(move |c: &mut Config| {
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
            let cur = c.get_str(&kl).map(str::to_string).unwrap_or_else(|| def_l.clone());
            let i = val_sig.with(|v| v.iter().position(|x| *x == cur).unwrap_or(0));
            sig.set(i);
        }),
    );
    // getter（一致性）：返回当前选中值；列表未就位/越界时返回 Null（与静态 select 路径一致，
    // 避免潜在 enabled_when 对空值的判定分歧）。仅供联动读取，不代表 writer 写回值。
    state.fields.getters.borrow_mut().insert(
        key.to_string(),
        Box::new(move || {
            val_sig.with(|vs| vs.get(sig.get()).cloned()).map(Value::String).unwrap_or(Value::Null)
        }),
    );

    let el = Element::dropdown_reactive(lab_sig, sig);
    match item.width {
        Some(w) => el.width(w as i32),
        None => el.min_width(CTRL_W),
    }
}

/// 离散数值下拉（int 键，如每页候选数）：options 的 value 为整数字符串，显示 label，写回整数。
fn build_number_select(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    let (values, labels): (Vec<String>, Vec<String>) =
        item.options.iter().map(|o| (o.value.clone(), o.label.clone())).unzip();
    if values.is_empty() {
        return Element::label("（无可选项）").fg_role(Role::TextMuted);
    }
    let def = state.capabilities.default_for(key).as_i64().unwrap_or(0);
    // 越界/缺失回落到「默认值对应索引」（而非首项），避免显示与保存偏离 capability 默认。
    let def_idx = values.iter().position(|v| *v == def.to_string()).unwrap_or(0);
    let cur = cfg.get_f64(key).map(|f| f as i64).unwrap_or(def);
    let idx = values.iter().position(|v| *v == cur.to_string()).unwrap_or(def_idx);
    let sig = signal(idx);

    // writer：取选中项的整数值写回。
    let kw = key.to_string();
    let vals_w = values.clone();
    state.fields.writers.borrow_mut().push(Box::new(move |c: &mut Config| {
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
            sig.set(vals_l.iter().position(|v| *v == cur.to_string()).unwrap_or(def_idx));
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
    let def = state.capabilities.default_for(key).as_str().unwrap_or("").to_string();
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
fn hotkey_keys() -> Vec<(String, String)> {
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
        ("space", "Space"), ("tab", "Tab"), ("enter", "Enter"),
        (".", "."), (",", ","), ("/", "/"), (";", ";"), ("'", "'"),
        ("[", "["), ("]", "]"), ("minus", "-"), ("equal", "="), ("`", "`"), ("\\", "\\"),
    ] {
        v.push((val.to_string(), lbl.to_string()));
    }
    v
}

/// 修饰键 (值, 显示名)，顺序即合成顺序（与 core 既有默认 "ctrl+shift+e" 一致）。
const HOTKEY_MODS: [(&str, &str); 4] =
    [("ctrl", "Ctrl"), ("shift", "Shift"), ("alt", "Alt"), ("win", "Win")];

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
    if enabled { raw.to_string() } else { "none".to_string() }
}

fn build_hotkey(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    use crate::dialogs::field_dialogs::build_hotkey_dialog;

    let def = state.capabilities.default_for(key).as_str().unwrap_or("none").to_string();
    let cur = cfg.get_str(key).map(str::to_string).unwrap_or_else(|| def.clone());
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
    state.fields.writers.borrow_mut().push(Box::new(move |c: &mut Config| {
        c.set_path(&kw, Value::String(effective_hotkey(enabled_sig.get(), &raw.get())))
    }));
    // loader：回填启用态 + raw + disp（禁用时以默认绑定填充，置灰展示）。
    let kl = key.to_string();
    let def_l = def.clone();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let v = c.get_str(&kl).map(str::to_string).unwrap_or_else(|| def_l.clone());
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
        visible, &item.label, raw, disp, def, mods, key_idx,
        key_vals.clone(), key_labels, global_sig, enabled_sig,
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
        .child(Element::checkbox("启用", enabled_sig).small().fg_role(Role::Text))
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
    state.fields.writers.borrow_mut().push(Box::new(move |c: &mut Config| {
        // 排序使输出确定（数组按值相等比对，避免顺序差异误判 dirty / 每次保存重排）。
        let mut names: Vec<String> =
            pairs_w.iter().filter(|(_, s)| s.get()).map(|(f, _)| f.clone()).collect();
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
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
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
    let cap_ty = state.capabilities.entries.get(key).map(|e| e.ty.as_str()).unwrap_or("");
    if cap_ty == "bool" {
        return build_segmented_bool(state, key, cfg, values, labels);
    }
    let def_str = state.capabilities.default_for(key).as_str().unwrap_or("").to_string();
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
    let def = state.capabilities.default_for(key).as_bool().unwrap_or(false);
    let cur = cfg.get_bool(key).unwrap_or(def);
    let pos_of = |on: bool, vals: &[String]| -> usize {
        let s = if on { "true" } else { "false" };
        vals.iter().position(|v| v == s).unwrap_or(usize::from(on))
    };
    let sig = signal(pos_of(cur, &values));

    let vals_w = values.clone();
    let kw = key.to_string();
    state.fields.writers.borrow_mut().push(Box::new(move |c: &mut Config| {
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
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
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
            let arr: Vec<Value> = collect_checked(&pairs).into_iter().map(Value::String).collect();
            c.set_path(&kw, Value::Array(arr));
        }));

    // getter：当前勾选值的 Value::Array（供其它项 enabled_when 联动读取）
    let sigs_g = sigs.clone();
    state.fields.getters.borrow_mut().insert(
        key.to_string(),
        Box::new(move || {
            let pairs: Vec<(&str, bool)> =
                sigs_g.iter().map(|(v, s)| (v.as_str(), s.get())).collect();
            let arr: Vec<Value> = collect_checked(&pairs).into_iter().map(Value::String).collect();
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
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
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
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// 读 capability 默认值并解析为键值对 map（map 类型回落用）。
fn default_map(state: &AppState, key: &str) -> serde_json::Map<String, Value> {
    state.capabilities.default_for(key).as_object().cloned().unwrap_or_default()
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
        "codetableSchemas" => schema_options(state, Some("codetable")),
        "pinyinSchemas" => schema_options(state, Some("pinyin")),
        "fonts" => {
            let v: Vec<String> = state.fonts.clone();
            (v.clone(), v)
        }
        // 主题：值=id/slug（写回 ui.theme.name），标签=显示名。
        "themes" => state.theme_names.iter().cloned().unzip(),
        _ => (vec![], vec![]),
    }
}

/// 方案选项：可按 kind 过滤；首项补一个"自动/未设置"空值便于回落。
fn schema_options(state: &AppState, kind: Option<&str>) -> (Vec<String>, Vec<String>) {
    let mut values = vec![String::new()];
    let mut labels = vec!["（自动）".to_string()];
    for s in &state.schemas {
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
    let grid = Element::grid(2, 10, cells).width_match().enabled_when(move || en.get());
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
fn dialog_group_header(name: &str) -> Element {
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
        let Some(item) = state.manifest.get(k) else { continue };
        if item.hidden { continue; }
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
    let cap_ty = state.capabilities.entries.get(key).map(|e| e.ty.as_str()).unwrap_or("");
    // 自定义标点映射：map 值为 [中半,英全,中全,英半] 四元数组（core 存储序），专用 5 列表格对话框。
    if key == "input.punct.custom_mappings" {
        build_dialog_button_punct(state, key, cfg)
    } else if key == "ui.candidate.index_labels" {
        // 自定义序号标签：字符串拆成 10 个序号槽位，专用 10 框对话框。
        build_dialog_button_index_labels(state, key, cfg)
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
    let btn = open_dialog_button(visible).enabled_when(move || gate_sig.get());

    let control = Element::row()
        .cross(Align::Center)
        .spacing(10)
        .child(Element::checkbox("启用", gate_sig).fg_role(Role::Text))
        .child(btn);
    setting_row(&gate_item.label, gate_item.hint.as_deref(), control)
}

/// dialog_button（strlist，无 options）：空格分隔文本编辑。挂对话框到根，返回 visible。
fn build_dialog_button_spaced(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Signal<bool> {
    use crate::dialogs::field_dialogs::{build_spaced_text_dialog, spaced_to_strlist, strlist_to_spaced};

    // 读当前值播种文本框 signal（缺省回落 capability 默认）
    let cur: Vec<String> = cfg
        .get_array(key)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_else(|| default_strlist(state, key));
    let text = signal(strlist_to_spaced(&cur));
    let visible = signal(false);

    // writer：把文本框内容（空格分隔）写回 strlist
    let kw = key.to_string();
    state.fields.writers.borrow_mut().push(Box::new(move |c: &mut Config| {
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
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_else(|| def_l.clone());
            text.set(strlist_to_spaced(&cur));
        }),
    );

    // 恢复默认文本：读 capability 默认值
    let def_text = {
        let d = state.capabilities.default_for(key);
        let v: Vec<String> = d
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
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
fn build_dialog_button_string(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Signal<bool> {
    use crate::dialogs::field_dialogs::build_text_dialog;

    let def = state.capabilities.default_for(key).as_str().unwrap_or("").to_string();
    let cur = cfg.get_str(key).map(str::to_string).unwrap_or_else(|| def.clone());
    let text = signal(cur);
    let visible = signal(false);

    let kw = key.to_string();
    state.fields.writers.borrow_mut().push(Box::new(move |c: &mut Config| {
        c.set_path(&kw, Value::String(text.get()))
    }));

    let kl = key.to_string();
    let def_l = def.clone();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            text.set(c.get_str(&kl).map(str::to_string).unwrap_or_else(|| def_l.clone()));
        }),
    );

    let hint = item.hint.clone().unwrap_or_default();
    let dlg = build_text_dialog(visible, &item.label, &hint, text, def, "输入字符…");
    register_dialog(state, dlg);
    visible
}

/// dialog_button（strlist with options）：勾选配对清单。挂对话框到根，返回 visible。
fn build_dialog_button_pairs(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Signal<bool> {
    use crate::dialogs::field_dialogs::build_pairs_dialog;

    let defaults = default_strlist(state, key);
    let cur: Vec<String> = cfg
        .get_array(key)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
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
            let pairs: Vec<(&str, bool)> = sigs_w.iter().map(|(v, s)| (v.as_str(), s.get())).collect();
            let arr: Vec<Value> = collect_checked(&pairs).into_iter().map(Value::String).collect();
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
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
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

/// dialog_button（map）：键值对 text_input 降级实现。挂对话框到根，返回 visible。
fn build_dialog_button_map(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Signal<bool> {
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

    let def = state.capabilities.default_for(key).as_str().unwrap_or("").to_string();
    let cur = cfg.get_str(key).map(str::to_string).unwrap_or_else(|| def.clone());
    let cur_chars: Vec<char> = cur.chars().collect();
    let inputs: Vec<Signal<String>> = (0..INDEX_LABEL_SLOTS)
        .map(|i| signal(cur_chars.get(i).map(|c| c.to_string()).unwrap_or_default()))
        .collect();
    let visible = signal(false);

    // writer：各框首字符拼接（遇空即停）→ 前缀串。
    let inputs_w = inputs.clone();
    let kw = key.to_string();
    state.fields.writers.borrow_mut().push(Box::new(move |c: &mut Config| {
        c.set_path(&kw, Value::String(collect_labels(&inputs_w)));
    }));
    // loader：字符串按字符回填各框（多余截断、不足留空）。
    let inputs_l = inputs.clone();
    let kl = key.to_string();
    let def_l = def.clone();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let cur = c.get_str(&kl).map(str::to_string).unwrap_or_else(|| def_l.clone());
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

    let cur_map = cfg.get_path(key).and_then(|v| v.as_object()).cloned().unwrap_or_default();

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
    state.fields.writers.borrow_mut().push(Box::new(move |c: &mut Config| {
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
            let m = c.get_path(&kl).and_then(|v| v.as_object()).cloned().unwrap_or_default();
            for (row, (_, tok, defs)) in cells_l.iter().zip(PUNCT_DEFAULTS.iter()) {
                let ov = m.get(*tok).and_then(|v| v.as_array());
                for i in 0..4 {
                    row[i].set(seed_cell(ov, i, defs[i]));
                }
            }
        }),
    );

    let src_labels: Vec<String> = PUNCT_DEFAULTS.iter().map(|(s, _, _)| s.to_string()).collect();
    let defaults: Vec<[String; 4]> = PUNCT_DEFAULTS
        .iter()
        .map(|(_, _, d)| std::array::from_fn(|i| d[i].to_string()))
        .collect();
    let dlg = build_punct_table_dialog(visible, "自定义标点映射", src_labels, cells, defaults);
    register_dialog(state, dlg);
    visible
}

/// 解析 `enabled_when`，仅支持 `key == value`（manifest 现有用法均为 `== true`）。
fn parse_cond(cond: &str) -> Option<(String, String)> {
    let (k, v) = cond.split_once("==")?;
    Some((k.trim().to_string(), v.trim().to_string()))
}

/// 判断当前值是否满足条件（支持 true/false 与字符串相等）。
fn cond_matches(cur: &Value, expect: &str) -> bool {
    match expect {
        "true" => cur.as_bool() == Some(true),
        "false" => cur.as_bool() == Some(false),
        s => cur.as_str() == Some(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkbox_group_collects_checked_values() {
        // 纯逻辑：勾选集合 → strlist
        let checked = [("lshift", true), ("rshift", false), ("capslock", true)];
        let arr = collect_checked(&checked);
        assert_eq!(arr, vec!["lshift".to_string(), "capslock".to_string()]);
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
            assert_eq!(hosts, 1, "dialog_group `{g}`（{cnt} 成员）应恰好 1 个宿主，实际 {hosts}");
        }
        for g in openers.keys() {
            assert!(members.contains_key(g), "opens_dialog `{g}` 无任何成员(dialog_group)");
        }
    }

    #[test]
    fn hotkey_parse_compose_roundtrip() {
        // core 实际默认值需原样往返（注意 = 用命名 equal、标点 . \ 用字面）。
        for s in ["ctrl+shift+e", "shift+space", "ctrl+.", "ctrl+shift+\\", "ctrl+equal", "ctrl+shift+["] {
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

    #[test]
    fn from_embedded_parses_groups_and_items() {
        let m = Manifest::from_embedded();
        // 6 个分组：schema/input/keys/ui/general/advanced
        assert_eq!(m.groups.len(), 6, "应解析出 6 个分组");
        // 代表性 key 存在且类型/标签正确
        let it = m.get("input.auto_pair.chinese").expect("应含 input.auto_pair.chinese");
        assert_eq!(it.typ, "toggle");
        assert_eq!(it.group, "input");
        assert!(!it.label.is_empty(), "label 不应为空");
        // 至少 70 个配置项
        assert!(m.order.len() >= 70, "items 数量应 >= 70，实际 {}", m.order.len());
    }
}
