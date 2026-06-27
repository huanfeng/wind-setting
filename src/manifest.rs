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
use crate::widgets::setting_row;

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
    pub default: Value,
    pub options: Vec<MOption>,
    pub options_source: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub unit: Option<String>,
    pub enabled_when: Option<String>,
    /// dialog_button 专用：门控开关 key（该 toggle 关时按钮置灰不可点）。
    pub gate_key: Option<String>,
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
                    enabled_when: opt_str(it, "enabled_when"),
                    gate_key: opt_str(it, "gate_key"),
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

    /// 内嵌设置清单源文本（随二进制发布，core 退役 manifest.toml 后由 setting 维护）。
    const EMBEDDED: &'static str = include_str!("assets/settings_manifest.toml");

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

/// 按 key 构建一行设置项（label + 控件），并登记保存写回器与联动取值器。
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
    let control = match item.typ.as_str() {
        "toggle" => build_toggle(state, key, item, cfg),
        "select" => build_select(state, key, item, cfg),
        "segmented" => build_segmented(state, key, item, cfg),
        "checkbox_group" => build_checkbox_group(state, key, item, cfg),
        "number" | "slider" => build_number(state, key, item, cfg),
        "string" => build_string(state, key, item, cfg),
        "dialog_button" => build_dialog_button(state, key, item, cfg),
        other => Element::label(format!("[{other}?]")).fg_role(Role::TextMuted),
    };
    let mut row = setting_row(&item.label, item.hint.as_deref(), control);
    if let Some((ck, expect)) = item.enabled_when.as_deref().and_then(parse_cond) {
        let reg = state.fields.clone();
        row = row.visible_when(move || {
            let g = reg.getters.borrow();
            g.get(&ck)
                .map(|get| cond_matches(&get(), &expect))
                .unwrap_or(true)
        });
    }
    row
}

fn build_toggle(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    let cur = cfg
        .get_bool(key)
        .unwrap_or_else(|| item.default.as_bool().unwrap_or(false));
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
    let def = item.default.as_bool().unwrap_or(false);
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| sig.set(c.get_bool(&kl).unwrap_or(def))),
    );
    Element::switch(sig)
}

fn build_select(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
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
    let cur = cfg
        .get_str(key)
        .map(str::to_string)
        .unwrap_or_else(|| item.default.as_str().unwrap_or("").to_string());
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
    let def_str = item.default.as_str().unwrap_or("").to_string();
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
    Element::dropdown(labels, sig).width(CTRL_W)
}

fn build_number(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    let cur = cfg
        .get_f64(key)
        .unwrap_or_else(|| item.default.as_f64().unwrap_or(0.0));
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
    let def = item.default.as_f64().unwrap_or(0.0);
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| sig.set(c.get_f64(&kl).unwrap_or(def))),
    );
    Element::stepper(sig, min, max, step).width(CTRL_W)
}

fn build_string(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    let cur = cfg
        .get_str(key)
        .map(str::to_string)
        .unwrap_or_else(|| item.default.as_str().unwrap_or("").to_string());
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
    let def = item.default.as_str().unwrap_or("").to_string();
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

fn build_segmented(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    let (values, labels): (Vec<String>, Vec<String>) = item
        .options
        .iter()
        .map(|o| (o.value.clone(), o.label.clone()))
        .unzip();
    if values.is_empty() {
        return Element::label("（无可选项）").fg_role(Role::TextMuted);
    }
    let cur = cfg
        .get_str(key)
        .map(str::to_string)
        .unwrap_or_else(|| item.default.as_str().unwrap_or("").to_string());
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
    let def_str = item.default.as_str().unwrap_or("").to_string();
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

fn build_checkbox_group(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    let cur: Vec<String> = cfg
        .get_array(key)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let opts = item.options.clone();
    let sigs: Vec<(String, Signal<bool>)> = opts
        .iter()
        .map(|o| (o.value.clone(), signal(cur.iter().any(|c| *c == o.value))))
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

    // loader：从 Config 回填各 signal
    let sigs_l = sigs.clone();
    let kl = key.to_string();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let cur: Vec<String> = c
                .get_array(&kl)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            for (v, s) in &sigs_l {
                s.set(cur.iter().any(|c| c == v));
            }
        }),
    );

    // 控件：grid 复选（2 列）
    let cells: Vec<Element> = sigs
        .iter()
        .zip(opts.iter())
        .map(|((_, s), o)| Element::checkbox(&o.label, *s).fg_role(Role::Text))
        .collect();
    Element::grid(2, 10, cells)
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

/// dialog_button 控件：外层「设置…」按钮（受 gate_key 联动），点开空格分隔文本对话框。
fn build_dialog_button(state: &AppState, key: &str, item: &MItem, cfg: &Config) -> Element {
    use crate::dialogs::field_dialogs::{build_spaced_text_dialog, spaced_to_strlist, strlist_to_spaced};

    // 读当前值播种文本框 signal
    let cur: Vec<String> = cfg
        .get_array(key)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
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

    // loader：从 Config 回填文本框
    let kl = key.to_string();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let cur: Vec<String> = c
                .get_array(&kl)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
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

    // 对话框面板
    let hint = item.hint.clone().unwrap_or_default();
    let dlg = build_spaced_text_dialog(visible, &item.label, &hint, text, def_text);

    // 外层按钮 + 门控联动：windui 无 disabled_when，用 visible_when 双按钮实现
    // 启用态按钮（gate 开或无 gate 时可见）；禁用态占位（gate 关时可见）
    if let Some(gate) = &item.gate_key {
        let reg_on = state.fields.clone();
        let gk_on = gate.clone();
        let reg_off = state.fields.clone();
        let gk_off = gate.clone();
        let active_btn = Element::button("设置…")
            .small()
            .neutral()
            .on_click(move |_| visible.set(true))
            .visible_when(move || {
                let g = reg_on.getters.borrow();
                g.get(&gk_on)
                    .map(|get| get().as_bool() == Some(true))
                    .unwrap_or(true)
            });
        let inactive_btn = Element::button("设置…")
            .small()
            .neutral()
            .disabled(true)
            .visible_when(move || {
                let g = reg_off.getters.borrow();
                g.get(&gk_off)
                    .map(|get| get().as_bool() != Some(true))
                    .unwrap_or(false)
            });
        Element::stack().child(active_btn).child(inactive_btn).child(dlg)
    } else {
        let btn = Element::button("设置…")
            .small()
            .neutral()
            .on_click(move |_| visible.set(true));
        Element::stack().child(btn).child(dlg)
    }
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
