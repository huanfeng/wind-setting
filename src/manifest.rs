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
}

/// select 的一个选项（value 写回配置，label 展示）。
#[derive(Clone)]
pub struct MOption {
    pub value: String,
    pub label: String,
}

/// 清单：key → 项。
#[derive(Default)]
pub struct Manifest {
    pub items: HashMap<String, MItem>,
}

impl Manifest {
    /// 从 system.manifest 返回的 JSON 解析。容错：缺字段回退默认。
    pub fn from_value(v: &Value) -> Self {
        let mut items = HashMap::new();
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
                };
                items.insert(key.to_string(), item);
            }
        }
        Self { items }
    }

    pub fn get(&self, key: &str) -> Option<&MItem> {
        self.items.get(key)
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
        "number" | "slider" => build_number(state, key, item, cfg),
        "string" => build_string(state, key, item, cfg),
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
    Element::text_input(sig, item.hint.clone().unwrap_or_default()).width(200)
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
