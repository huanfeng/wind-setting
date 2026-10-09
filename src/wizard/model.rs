//! 向导的纯逻辑：当前配置 → 草稿 → 目标配置。不碰 RPC、不碰 UI，全部可单测。

use std::collections::HashMap;

use serde_json::Value;

use crate::config::{Config, ConfigItem, diff_config};
use crate::manifest::{
    PUNCT_DEFAULTS, PUNCT_ENABLED_KEY, PUNCT_MAP_KEY, apply_slash_dun, punct_row_override,
    seed_cell, slash_dun_on, slash_row_index,
};
use crate::state::SchemaInfo;

pub const VERB_QUICK: &str = "mix:quick_mix";
pub const VERB_TEMP_PINYIN: &str = "temp_pinyin";
pub(crate) const KEY_ACTIONS: &str = "keys.key_actions";

/// 快捷输入可选键（键名, 标签）。只列常用的几个；其余绑定落在 `Trigger::Keep`。
pub const QUICK_KEYS: &[(&str, &str)] = &[
    ("semicolon", "分号 ( ; )"),
    ("backtick", "反引号 ( ` )"),
    ("quote", "单引号 ( ' )"),
];
/// 临时拼音可选键。
pub const TEMP_KEYS: &[(&str, &str)] = &[
    ("backtick", "反引号 ( ` )"),
    ("semicolon", "分号 ( ; )"),
    ("quote", "单引号 ( ' )"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Codetable,
    Pinyin,
    Shuangpin,
}

impl Kind {
    /// 主类型对应的默认方案 id。
    pub fn schema_id(self) -> &'static str {
        match self {
            Kind::Codetable => "wubi86",
            Kind::Pinyin => "pinyin",
            Kind::Shuangpin => "shuangpin",
        }
    }
    fn from_active(active: &str) -> Kind {
        match active {
            "pinyin" => Kind::Pinyin,
            "shuangpin" => Kind::Shuangpin,
            _ => Kind::Codetable,
        }
    }
}

/// 一个引导动词的绑定选择。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// 保持现状：现有绑定不是「恰好一个、且在可选键里」。携带排序后的现有键，供界面显示。
    Keep(Vec<String>),
    /// 不启用：清除该动词的全部绑定。
    Off,
    /// 恰好绑这一个键。
    Key(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub kind: Kind,
    /// 默认方案。主类型是五笔时，保留用户原来的码表默认方案（例如自定义码表）。
    pub active: String,
    pub layout: String,
    /// 启用方案，保持原有顺序。
    pub enabled: Vec<String>,
    // 第 2 步：码表
    pub auto_commit_at_full: bool,
    pub clear_on_empty_max: bool,
    pub code_hint: bool,
    // 第 2 步：拼音
    pub emoji: bool,
    pub fuzzy: bool,
    pub fuzzy_pairs: [bool; 4],
    pub allow_full_pinyin: bool,
    // 第 3 步
    pub quick: Trigger,
    pub slash_dun: bool,
    pub temp_pinyin: bool,
    pub temp_key: Trigger,
}

/// 模糊音常用 4 对的键名（`schema.pinyin.fuzzy.<名>`）与标签。
pub const FUZZY_PAIRS: [(&str, &str); 4] = [
    ("zh_z", "zh = z"),
    ("ch_c", "ch = c"),
    ("sh_s", "sh = s"),
    ("n_l", "n = l"),
];

fn trigger_of(cfg: &Config, verb: &str, options: &[(&str, &str)]) -> Trigger {
    let mut keys: Vec<String> = cfg
        .get_path(KEY_ACTIONS)
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter(|(_, v)| v.as_str() == Some(verb))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();
    keys.sort();
    match keys.as_slice() {
        [] => Trigger::Off,
        [k] if options.iter().any(|(o, _)| o == k) => Trigger::Key(k.clone()),
        _ => Trigger::Keep(keys),
    }
}

fn slash_dun_of(cfg: &Config) -> bool {
    let idx = slash_row_index();
    let (_, tok, defs) = PUNCT_DEFAULTS[idx];
    let ov = cfg
        .get_path(PUNCT_MAP_KEY)
        .and_then(|v| v.get(tok))
        .and_then(|v| v.as_array());
    slash_dun_on(
        &seed_cell(ov, 0, defs[0]),
        cfg.get_bool(PUNCT_ENABLED_KEY).unwrap_or(false),
    )
}

fn strs(v: Option<&Vec<Value>>) -> Vec<String> {
    v.map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect()
    })
    .unwrap_or_default()
}

impl Draft {
    /// 按当前实际配置预填。`layout` 是 `shuangpin` 方案当前布局 id（拿不到时传出厂 `xiaohe`）。
    pub fn from_current(cfg: &Config, _schemas: &[SchemaInfo], layout: &str) -> Draft {
        let active = cfg.get_str("schema.active").unwrap_or("wubi86").to_string();
        let b = |k: &str| cfg.get_bool(k).unwrap_or(false);
        Draft {
            kind: Kind::from_active(&active),
            enabled: strs(cfg.get_array("schema.available")),
            active,
            layout: layout.to_string(),
            auto_commit_at_full: b("schema.codetable.auto_commit_at_full"),
            clear_on_empty_max: b("schema.codetable.clear_on_empty_max"),
            code_hint: cfg
                .get_str("schema.pinyin.code_hint_source")
                .unwrap_or("off")
                != "off",
            emoji: b("input.emoji.enabled"),
            fuzzy: b("schema.pinyin.fuzzy.enabled"),
            fuzzy_pairs: FUZZY_PAIRS.map(|(k, _)| b(&format!("schema.pinyin.fuzzy.{k}"))),
            allow_full_pinyin: b("schema.pinyin.shuangpin.allow_full_pinyin"),
            quick: trigger_of(cfg, VERB_QUICK, QUICK_KEYS),
            slash_dun: slash_dun_of(cfg),
            temp_pinyin: cfg.get_bool("input.temp_pinyin.enabled").unwrap_or(true),
            temp_key: trigger_of(cfg, VERB_TEMP_PINYIN, TEMP_KEYS),
        }
    }
}

impl Draft {
    /// 切换主类型：第 2、3 步套推荐预设，方案集按规则调整。
    /// 自定义方案（不在出厂 6 个里）一律保留，不受预设影响。
    pub fn with_preset(&self, kind: Kind, schemas: &[SchemaInfo]) -> Draft {
        const BUILTIN: [&str; 6] = [
            "wubi86",
            "wubi86_pinyin",
            "pinyin",
            "shuangpin",
            "stroke",
            "english",
        ];
        let main = kind.schema_id();
        let preset: &[&str] = match kind {
            Kind::Codetable => &["wubi86_pinyin", "pinyin"],
            Kind::Pinyin | Kind::Shuangpin => &[],
        };
        let mut enabled = vec![main.to_string()];
        for id in self.enabled.iter() {
            let custom = !BUILTIN.contains(&id.as_str());
            if custom && !enabled.contains(id) {
                enabled.push(id.clone());
            }
        }
        for s in schemas {
            if preset.contains(&s.id.as_str()) && !enabled.contains(&s.id) {
                enabled.push(s.id.clone());
            }
        }
        let mut d = self.clone();
        d.kind = kind;
        d.active = main.to_string();
        d.enabled = enabled;
        match kind {
            Kind::Codetable => {
                d.auto_commit_at_full = true;
                d.clear_on_empty_max = true;
                d.code_hint = true;
                d.temp_pinyin = true;
                if !matches!(d.temp_key, Trigger::Keep(_)) {
                    d.temp_key = Trigger::Key("backtick".into());
                }
            }
            Kind::Pinyin | Kind::Shuangpin => {
                d.emoji = true;
                d.fuzzy = false;
                d.fuzzy_pairs = [false; 4];
                d.allow_full_pinyin = false;
            }
        }
        if !matches!(d.quick, Trigger::Keep(_)) {
            d.quick = Trigger::Key("semicolon".into());
        }
        d
    }

    /// 基线 + 草稿 → 目标配置。只写本主类型可见、且语义有变化的项。
    pub fn to_target(&self, base: &Config) -> Config {
        let cur = Draft::from_current(base, &[], &self.layout);
        let mut t = base.clone();
        let mut set = |k: &str, v: Value| t.set_path(k, v);

        if self.active != cur.active {
            set("schema.active", Value::String(self.active.clone()));
        }
        if self.enabled != cur.enabled {
            set("schema.available", Value::from(self.enabled.clone()));
        }
        match self.kind {
            Kind::Codetable => {
                if self.auto_commit_at_full != cur.auto_commit_at_full {
                    set(
                        "schema.codetable.auto_commit_at_full",
                        self.auto_commit_at_full.into(),
                    );
                }
                if self.clear_on_empty_max != cur.clear_on_empty_max {
                    set(
                        "schema.codetable.clear_on_empty_max",
                        self.clear_on_empty_max.into(),
                    );
                }
                if self.code_hint != cur.code_hint {
                    let v = if self.code_hint { "codetable" } else { "off" };
                    set("schema.pinyin.code_hint_source", v.into());
                }
                if self.temp_pinyin != cur.temp_pinyin {
                    set("input.temp_pinyin.enabled", self.temp_pinyin.into());
                }
            }
            Kind::Pinyin | Kind::Shuangpin => {
                if self.emoji != cur.emoji {
                    set("input.emoji.enabled", self.emoji.into());
                }
                if self.fuzzy != cur.fuzzy {
                    set("schema.pinyin.fuzzy.enabled", self.fuzzy.into());
                }
                for (i, (k, _)) in FUZZY_PAIRS.iter().enumerate() {
                    if self.fuzzy_pairs[i] != cur.fuzzy_pairs[i] {
                        set(
                            &format!("schema.pinyin.fuzzy.{k}"),
                            self.fuzzy_pairs[i].into(),
                        );
                    }
                }
                if self.kind == Kind::Shuangpin && self.allow_full_pinyin != cur.allow_full_pinyin {
                    set(
                        "schema.pinyin.shuangpin.allow_full_pinyin",
                        self.allow_full_pinyin.into(),
                    );
                }
            }
        }
        drop(set);
        rebind(&mut t, VERB_QUICK, &self.quick, &cur.quick);
        if self.kind == Kind::Codetable {
            rebind(&mut t, VERB_TEMP_PINYIN, &self.temp_key, &cur.temp_key);
        }
        if self.slash_dun != cur.slash_dun {
            write_slash_dun(&mut t, self.slash_dun);
        }
        t
    }

    /// `config.setItems` 的条目：与设置页保存同一个 diff。
    pub fn items(&self, base: &Config, atomic_keys: &[String]) -> Vec<ConfigItem> {
        diff_config(base, &self.to_target(base), atomic_keys)
    }
}

fn rebind(t: &mut Config, verb: &str, want: &Trigger, cur: &Trigger) {
    if want == cur || matches!(want, Trigger::Keep(_)) {
        return;
    }
    let mut map = t
        .get_path(KEY_ACTIONS)
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    map.retain(|_, v| v.as_str() != Some(verb));
    if let Trigger::Key(k) = want {
        map.insert(k.clone(), Value::String(verb.to_string()));
    }
    t.set_path(KEY_ACTIONS, Value::Object(map));
}

fn write_slash_dun(t: &mut Config, on: bool) {
    let idx = slash_row_index();
    let (_, tok, defs) = PUNCT_DEFAULTS[idx];
    let mut map = t
        .get_path(PUNCT_MAP_KEY)
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let ov = map.get(tok).and_then(|v| v.as_array()).cloned();
    let mut row: [String; 4] = std::array::from_fn(|i| seed_cell(ov.as_ref(), i, defs[i]));
    apply_slash_dun(&mut row, &defs, on);
    match punct_row_override(&row, &defs) {
        Some(v) => {
            map.insert(tok.to_string(), v);
        }
        None => {
            map.remove(tok);
        }
    }
    t.set_path(PUNCT_MAP_KEY, Value::Object(map));
    if on {
        t.set_path(PUNCT_ENABLED_KEY, Value::Bool(true));
    }
}

/// 第 4 步的一行。
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub group: &'static str,
    pub label: String,
    pub old: String,
    pub new: String,
}

fn on_off(b: bool) -> String {
    if b { "开".into() } else { "关".into() }
}

fn trigger_text(t: &Trigger, opts: &[(&str, &str)]) -> String {
    match t {
        Trigger::Off => "不启用".into(),
        Trigger::Key(k) => opts
            .iter()
            .find(|(o, _)| o == k)
            .map(|(_, l)| l.to_string())
            .unwrap_or_else(|| k.clone()),
        Trigger::Keep(ks) => format!("保持现状（{}）", ks.join("、")),
    }
}

/// 第 4 步摘要：与 `to_target` 判定同一组「可见且变化」的项。
pub fn summary(
    cur: &Draft,
    new: &Draft,
    names: &dyn Fn(&str) -> String,
    layout_label: &dyn Fn(&str) -> String,
) -> Vec<Change> {
    let mut out = Vec::new();
    let mut push = |g: &'static str, l: &str, o: String, n: String| {
        if o != n {
            out.push(Change {
                group: g,
                label: l.to_string(),
                old: o,
                new: n,
            });
        }
    };
    let list = |v: &[String]| v.iter().map(|s| names(s)).collect::<Vec<_>>().join("、");
    push("方案", "默认方案", names(&cur.active), names(&new.active));
    push("方案", "启用的方案", list(&cur.enabled), list(&new.enabled));
    if new.kind == Kind::Shuangpin {
        push(
            "方案",
            "双拼布局",
            layout_label(&cur.layout),
            layout_label(&new.layout),
        );
    }
    match new.kind {
        Kind::Codetable => {
            push(
                "输入习惯",
                "全码唯一自动上屏",
                on_off(cur.auto_commit_at_full),
                on_off(new.auto_commit_at_full),
            );
            push(
                "输入习惯",
                "满码空码清空",
                on_off(cur.clear_on_empty_max),
                on_off(new.clear_on_empty_max),
            );
            push(
                "输入习惯",
                "拼音反查（显示五笔编码）",
                on_off(cur.code_hint),
                on_off(new.code_hint),
            );
        }
        Kind::Pinyin | Kind::Shuangpin => {
            push(
                "输入习惯",
                "emoji 候选",
                on_off(cur.emoji),
                on_off(new.emoji),
            );
            push("输入习惯", "模糊音", on_off(cur.fuzzy), on_off(new.fuzzy));
            for (i, (_, l)) in FUZZY_PAIRS.iter().enumerate() {
                push(
                    "输入习惯",
                    &format!("模糊音 {l}"),
                    on_off(cur.fuzzy_pairs[i]),
                    on_off(new.fuzzy_pairs[i]),
                );
            }
            if new.kind == Kind::Shuangpin {
                push(
                    "输入习惯",
                    "允许按全拼输入",
                    on_off(cur.allow_full_pinyin),
                    on_off(new.allow_full_pinyin),
                );
            }
        }
    }
    if !matches!(new.quick, Trigger::Keep(_)) {
        push(
            "通用",
            "快捷输入启用键",
            trigger_text(&cur.quick, QUICK_KEYS),
            trigger_text(&new.quick, QUICK_KEYS),
        );
    }
    push(
        "通用",
        "/ 键输出顿号",
        on_off(cur.slash_dun),
        on_off(new.slash_dun),
    );
    if new.kind == Kind::Codetable {
        push(
            "通用",
            "临时拼音",
            on_off(cur.temp_pinyin),
            on_off(new.temp_pinyin),
        );
        if !matches!(new.temp_key, Trigger::Keep(_)) {
            push(
                "通用",
                "临时拼音触发键",
                trigger_text(&cur.temp_key, TEMP_KEYS),
                trigger_text(&new.temp_key, TEMP_KEYS),
            );
        }
    }
    out
}

/// 只有选了双拼、并且布局确实变了，才写方案级布局。
pub fn layout_changed(cur: &Draft, new: &Draft) -> bool {
    new.kind == Kind::Shuangpin && new.layout != cur.layout && !new.layout.is_empty()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    pub key: String,
    pub msg: String,
}

/// 向导要占用的键：彼此重复，或在基线里被别的动词占着、且那个动词本次不会让出，即为冲突。
///
/// ★ 判据是「占用方会不会让出」，不是「占用方是不是向导管的动词」：拼音用户把快捷输入改到
/// 反引号时，临时拼音不在拼音用户的向导里，不会让出，覆盖它就是悄悄删掉临拼绑定。
pub fn key_conflicts(d: &Draft, base: &Config) -> Vec<Conflict> {
    let cur = Draft::from_current(base, &[], &d.layout);
    let moving = |verb: &str| match verb {
        VERB_QUICK => d.quick != cur.quick && !matches!(d.quick, Trigger::Keep(_)),
        VERB_TEMP_PINYIN => {
            d.kind == Kind::Codetable
                && d.temp_key != cur.temp_key
                && !matches!(d.temp_key, Trigger::Keep(_))
        }
        _ => false,
    };
    let label = |verb: &str| match verb {
        VERB_QUICK => "快捷输入".to_string(),
        VERB_TEMP_PINYIN => "临时拼音".to_string(),
        v => v.to_string(),
    };
    // (键, 认领的动词, 标签)；顿号不是动词绑定，动词记空串，任何占用都算冲突。
    let mut claims: Vec<(String, &str, &str)> = Vec::new();
    if let Trigger::Key(k) = &d.quick {
        claims.push((k.clone(), VERB_QUICK, "快捷输入"));
    }
    if d.kind == Kind::Codetable && d.temp_pinyin {
        if let Trigger::Key(k) = &d.temp_key {
            claims.push((k.clone(), VERB_TEMP_PINYIN, "临时拼音"));
        }
    }
    if d.slash_dun {
        claims.push(("slash".into(), "", "/ 键输出顿号"));
    }
    let ka = base.get_path(KEY_ACTIONS).and_then(|v| v.as_object());
    let mut out = Vec::new();
    for (i, (k, verb, who)) in claims.iter().enumerate() {
        if let Some((_, _, other)) = claims[..i].iter().find(|c| &c.0 == k) {
            out.push(Conflict {
                key: k.clone(),
                msg: format!("「{who}」与「{other}」用了同一个键"),
            });
            continue;
        }
        let held = ka
            .and_then(|m| m.get(k))
            .and_then(|v| v.as_str())
            .filter(|v| *v != "none");
        if let Some(v) = held {
            if v != *verb && !moving(v) {
                out.push(Conflict {
                    key: k.clone(),
                    msg: format!("「{who}」的键已被「{}」占用", label(v)),
                });
            }
        }
    }
    out
}

/// 目标默认方案对哪些「将写入的全局键」有独立设置（override）。
pub fn override_hints(
    items: &[ConfigItem],
    active: &str,
    ov: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    items
        .iter()
        .filter(|it| {
            ov.get(&it.key)
                .is_some_and(|ids| ids.iter().any(|i| i == active))
        })
        .map(|it| it.key.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn si(id: &str, kind: &str, scheme: &str) -> SchemaInfo {
        SchemaInfo {
            id: id.into(),
            name: id.into(),
            kind: kind.into(),
            scheme: scheme.into(),
            hidden: false,
        }
    }
    fn schemas() -> Vec<SchemaInfo> {
        vec![
            si("wubi86", "codetable", ""),
            si("wubi86_pinyin", "mixed", ""),
            si("pinyin", "pinyin", "full"),
            si("shuangpin", "pinyin", "shuangpin"),
            si("stroke", "codetable", ""),
            si("english", "english", ""),
        ]
    }
    fn cfg(v: serde_json::Value) -> Config {
        Config::from_value(v)
    }

    #[test]
    fn kind_is_inferred_from_active_schema() {
        let s = schemas();
        let d = |a: &str| {
            Draft::from_current(
                &cfg(json!({"schema": {"active": a, "available": [a]}})),
                &s,
                "xiaohe",
            )
            .kind
        };
        assert_eq!(d("pinyin"), Kind::Pinyin);
        assert_eq!(d("shuangpin"), Kind::Shuangpin);
        assert_eq!(d("wubi86"), Kind::Codetable);
        assert_eq!(d("my_custom_table"), Kind::Codetable);
    }

    #[test]
    fn code_hint_auto_counts_as_on() {
        let c =
            cfg(json!({"schema": {"active": "wubi86", "pinyin": {"code_hint_source": "auto"}}}));
        assert!(Draft::from_current(&c, &schemas(), "xiaohe").code_hint);
        let c = cfg(json!({"schema": {"active": "wubi86", "pinyin": {"code_hint_source": "off"}}}));
        assert!(!Draft::from_current(&c, &schemas(), "xiaohe").code_hint);
    }

    #[test]
    fn trigger_single_known_binding_is_key() {
        let c = cfg(
            json!({"keys": {"key_actions": {"semicolon": "mix:quick_mix", "backtick": "temp_pinyin"}}}),
        );
        let d = Draft::from_current(&c, &schemas(), "xiaohe");
        assert_eq!(d.quick, Trigger::Key("semicolon".into()));
        assert_eq!(d.temp_key, Trigger::Key("backtick".into()));
    }

    #[test]
    fn trigger_none_bound_is_off_and_multi_is_keep() {
        let c = cfg(
            json!({"keys": {"key_actions": {"semicolon": "mix:quick_mix", "quote": "mix:quick_mix"}}}),
        );
        let d = Draft::from_current(&c, &schemas(), "xiaohe");
        assert_eq!(
            d.quick,
            Trigger::Keep(vec!["quote".into(), "semicolon".into()])
        );
        assert_eq!(d.temp_key, Trigger::Off);
    }

    #[test]
    fn trigger_unlisted_key_is_keep() {
        let c = cfg(json!({"keys": {"key_actions": {"lbracket": "mix:quick_mix"}}}));
        assert_eq!(
            Draft::from_current(&c, &schemas(), "x").quick,
            Trigger::Keep(vec!["lbracket".into()])
        );
    }

    #[test]
    fn slash_dun_requires_master_switch() {
        let on = cfg(
            json!({"input": {"punct": {"custom_enabled": true, "custom_mappings": {"/": ["、","／","、","/"]}}}}),
        );
        assert!(Draft::from_current(&on, &schemas(), "x").slash_dun);
        let gated = cfg(
            json!({"input": {"punct": {"custom_enabled": false, "custom_mappings": {"/": ["、","／","、","/"]}}}}),
        );
        assert!(!Draft::from_current(&gated, &schemas(), "x").slash_dun);
    }

    #[test]
    fn enabled_follows_available_and_keeps_custom_schemas() {
        let c = cfg(json!({"schema": {"active": "wubi86", "available": ["wubi86", "my_table"]}}));
        let d = Draft::from_current(&c, &schemas(), "x");
        assert_eq!(
            d.enabled,
            vec!["wubi86".to_string(), "my_table".to_string()]
        );
    }

    fn base() -> Config {
        cfg(json!({
            "schema": {"active": "wubi86", "available": ["wubi86", "wubi86_pinyin", "my_table"],
                       "codetable": {"auto_commit_at_full": false, "clear_on_empty_max": false},
                       "pinyin": {"code_hint_source": "auto", "fuzzy": {"enabled": false}}},
            "input": {"emoji": {"enabled": false}, "temp_pinyin": {"enabled": true},
                      "punct": {"custom_enabled": false, "custom_mappings": {}}},
            "keys": {"key_actions": {"semicolon": "mix:quick_mix", "backtick": "temp_pinyin",
                                     "backslash": "special:kf"}}
        }))
    }
    const ATOMIC: &[String] = &[];

    #[test]
    fn untouched_draft_produces_no_items() {
        let b = base();
        let d = Draft::from_current(&b, &schemas(), "xiaohe");
        assert!(d.items(&b, ATOMIC).is_empty());
    }

    #[test]
    fn untouched_pinyin_draft_produces_no_items() {
        // 键缺省时 from_current 读成 false；没动过就不能把这个 false 写回去
        let b = cfg(json!({"schema": {"active": "pinyin", "available": ["pinyin"]}}));
        let d = Draft::from_current(&b, &schemas(), "xiaohe");
        assert!(d.items(&b, ATOMIC).is_empty());
    }

    #[test]
    fn code_hint_auto_untouched_stays_auto() {
        let b = base();
        let mut d = Draft::from_current(&b, &schemas(), "xiaohe");
        d.auto_commit_at_full = true; // 改别的
        let t = d.to_target(&b);
        assert_eq!(t.get_str("schema.pinyin.code_hint_source"), Some("auto"));
    }

    #[test]
    fn code_hint_on_from_off_writes_codetable() {
        let mut b = base();
        b.set_path("schema.pinyin.code_hint_source", json!("off"));
        let mut d = Draft::from_current(&b, &schemas(), "x");
        d.code_hint = true;
        assert_eq!(
            d.to_target(&b).get_str("schema.pinyin.code_hint_source"),
            Some("codetable")
        );
    }

    #[test]
    fn codetable_preset() {
        let d =
            Draft::from_current(&base(), &schemas(), "x").with_preset(Kind::Codetable, &schemas());
        assert!(d.auto_commit_at_full && d.clear_on_empty_max && d.code_hint && d.temp_pinyin);
        assert_eq!(d.active, "wubi86");
        for id in ["wubi86", "wubi86_pinyin", "pinyin", "my_table"] {
            assert!(d.enabled.contains(&id.to_string()), "{id}");
        }
        assert_eq!(d.quick, Trigger::Key("semicolon".into()));
    }

    #[test]
    fn pinyin_preset_keeps_custom_schemas_and_forces_main() {
        let d = Draft::from_current(&base(), &schemas(), "x").with_preset(Kind::Pinyin, &schemas());
        assert_eq!(d.active, "pinyin");
        assert_eq!(d.enabled.first().map(String::as_str), Some("pinyin"));
        assert!(d.enabled.contains(&"my_table".to_string()));
        assert!(!d.enabled.contains(&"wubi86".to_string()));
        assert!(d.emoji && !d.fuzzy);
    }

    #[test]
    fn pinyin_kind_does_not_write_codetable_or_temp_pinyin_keys() {
        let b = base();
        let mut d = Draft::from_current(&b, &schemas(), "x").with_preset(Kind::Pinyin, &schemas());
        d.temp_pinyin = false; // 拼音用户界面不显示，改了也不该写
        d.auto_commit_at_full = true; // 同上
        let keys: Vec<String> = d.items(&b, ATOMIC).into_iter().map(|i| i.key).collect();
        assert!(
            !keys
                .iter()
                .any(|k| k.starts_with("input.temp_pinyin") || k.starts_with("schema.codetable")),
            "{keys:?}"
        );
    }

    #[test]
    fn quick_off_removes_binding_and_key_moves() {
        let b = base();
        let mut d = Draft::from_current(&b, &schemas(), "x");
        d.quick = Trigger::Off;
        let t = d.to_target(&b);
        assert!(t.get_path("keys.key_actions.semicolon").is_none());
        d.quick = Trigger::Key("quote".into());
        let t = d.to_target(&b);
        assert_eq!(t.get_str("keys.key_actions.quote"), Some("mix:quick_mix"));
        assert!(t.get_path("keys.key_actions.semicolon").is_none());
        assert_eq!(t.get_str("keys.key_actions.backslash"), Some("special:kf"));
    }

    #[test]
    fn keep_trigger_writes_nothing() {
        let b = base();
        let mut d = Draft::from_current(&b, &schemas(), "x");
        d.quick = Trigger::Keep(vec!["semicolon".into()]);
        assert_eq!(
            d.to_target(&b).get_path("keys.key_actions"),
            b.get_path("keys.key_actions")
        );
    }

    #[test]
    fn slash_dun_on_touches_only_slash_row_and_enables_master() {
        let b = base();
        let mut d = Draft::from_current(&b, &schemas(), "x");
        d.slash_dun = true;
        let t = d.to_target(&b);
        assert_eq!(t.get_bool("input.punct.custom_enabled"), Some(true));
        // 路径含 `/`，不能走点分路径：取整张表再按键取
        let m = t
            .get_path("input.punct.custom_mappings")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m["/"][0], "、");
        assert_eq!(m["/"][2], "、");
    }

    #[test]
    fn conflicts_between_wizard_keys_and_foreign_bindings() {
        let b = base();
        let mut d = Draft::from_current(&b, &schemas(), "x");
        d.temp_key = Trigger::Key("semicolon".into()); // 与快捷输入同键
        assert!(!key_conflicts(&d, &b).is_empty());
        let mut d = Draft::from_current(&b, &schemas(), "x");
        d.quick = Trigger::Key("quote".into());
        assert!(key_conflicts(&d, &b).is_empty());
        let mut b2 = base();
        b2.set_path("keys.key_actions.quote", json!("special:kf"));
        assert!(
            !key_conflicts(&d, &b2).is_empty(),
            "占用其它动词的键要报冲突"
        );
    }

    #[test]
    fn slash_dun_conflicts_with_slash_trigger() {
        let mut b = base();
        b.set_path("keys.key_actions.slash", json!("mix:quick_mix"));
        b.set_path("keys.key_actions.semicolon", json!(null));
        let mut d = Draft::from_current(&b, &schemas(), "x");
        d.slash_dun = true;
        assert!(!key_conflicts(&d, &b).is_empty());
    }

    #[test]
    fn temp_conflict_ignored_for_pinyin_kind() {
        let b = base();
        let mut d = Draft::from_current(&b, &schemas(), "x").with_preset(Kind::Pinyin, &schemas());
        d.temp_key = Trigger::Key("semicolon".into());
        assert!(
            key_conflicts(&d, &b).is_empty(),
            "拼音用户不显示临拼，不参与冲突"
        );
    }

    #[test]
    fn pinyin_quick_on_temp_pinyin_key_conflicts() {
        // 拼音用户向导里看不到临拼，它不会让出 backtick；覆盖即悄悄删掉临拼绑定
        let b = base();
        let mut d = Draft::from_current(&b, &schemas(), "x").with_preset(Kind::Pinyin, &schemas());
        d.quick = Trigger::Key("backtick".into());
        assert!(!key_conflicts(&d, &b).is_empty());
    }

    #[test]
    fn codetable_swapping_quick_and_temp_keys_is_fine() {
        let b = base();
        let mut d = Draft::from_current(&b, &schemas(), "x");
        d.quick = Trigger::Key("backtick".into());
        d.temp_key = Trigger::Key("semicolon".into());
        assert!(
            key_conflicts(&d, &b).is_empty(),
            "两者都在让出原键，互换合法"
        );
    }

    #[test]
    fn summary_matches_items() {
        let b = base();
        let cur = Draft::from_current(&b, &schemas(), "xiaohe");
        let new = cur.with_preset(Kind::Shuangpin, &schemas());
        let id = |s: &str| s.to_string();
        let ch = summary(&cur, &new, &id, &id);
        assert!(!ch.is_empty());
        assert!(summary(&cur, &cur, &id, &id).is_empty());
    }

    #[test]
    fn layout_change_detected_only_for_shuangpin() {
        let cur = Draft::from_current(&base(), &schemas(), "xiaohe");
        let mut new = cur.with_preset(Kind::Shuangpin, &schemas());
        new.layout = "ziranma".into();
        assert!(layout_changed(&cur, &new));
        let mut wubi = cur.clone();
        wubi.layout = "ziranma".into();
        assert!(!layout_changed(&cur, &wubi));
    }

    #[test]
    fn override_hint_names_active_schema_keys() {
        let items = vec![crate::config::ConfigItem {
            key: "schema.codetable.auto_commit_at_full".into(),
            value: json!(true),
        }];
        let mut ov = std::collections::HashMap::new();
        ov.insert(
            "schema.codetable.auto_commit_at_full".to_string(),
            vec!["wubi86".to_string()],
        );
        assert_eq!(
            override_hints(&items, "wubi86", &ov),
            vec!["schema.codetable.auto_commit_at_full".to_string()]
        );
        assert!(override_hints(&items, "pinyin", &ov).is_empty());
    }
}
