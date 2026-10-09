//! 向导的纯逻辑：当前配置 → 草稿 → 目标配置。不碰 RPC、不碰 UI，全部可单测。

use serde_json::Value;

use crate::config::Config;
use crate::manifest::{
    PUNCT_DEFAULTS, PUNCT_ENABLED_KEY, PUNCT_MAP_KEY, seed_cell, slash_dun_on, slash_row_index,
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
}
