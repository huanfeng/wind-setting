//! 「方案」页的码表 / 拼音 / 双拼 / 混输 / 英文五张卡各自绑定一个具体方案。
//!
//! core 已取消按类型共用的全局设置：`schema.codetable.*` 这类键现在是「某个方案的设置」，
//! 读写走 `schema.getSettings` / `schema.setSettings`（键名不变）。卡片仍按这些键名画控件：
//! 加载时把所绑方案的生效值盖进配置树，保存时把这些键的改动分流到所绑方案，其余键照旧走
//! `config.setItems`。设计见主仓 `docs/design/scheme-settings-flatten.md` §6。

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::config::{Config, ConfigItem};
use crate::state::SchemaInfo;

/// 卡片类别；`label` 即卡名。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Card {
    Codetable,
    Pinyin,
    Shuangpin,
    Mixed,
    English,
}

impl Card {
    pub const ALL: [Card; 5] = [
        Card::Codetable,
        Card::Pinyin,
        Card::Shuangpin,
        Card::Mixed,
        Card::English,
    ];

    /// 一个配置键归哪张卡；不是方案设置键返回 `None`。
    /// 双拼专有项（`schema.pinyin.shuangpin.*`）归双拼卡，其余拼音项归拼音卡。
    pub fn of_key(key: &str) -> Option<Card> {
        if key.starts_with("schema.codetable.") {
            Some(Card::Codetable)
        } else if key.starts_with("schema.pinyin.shuangpin.") {
            Some(Card::Shuangpin)
        } else if key.starts_with("schema.pinyin.") {
            Some(Card::Pinyin)
        } else if key.starts_with("schema.mix.") {
            Some(Card::Mixed)
        } else if key.starts_with("schema.english.") {
            Some(Card::English)
        } else {
            None
        }
    }
}

/// 出厂英文方案的 id（临时英文与混入的英文候选都用它）。
const ENGLISH_SCHEMA: &str = "english";

/// 各卡绑定的方案（id 与显示名）。某类一个方案都没有时该卡没有绑定。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CardSchemes(HashMap<Card, (String, String)>);

impl CardSchemes {
    /// 按配置与方案列表定各卡绑谁：
    /// - 码表：`schema.primary_codetable`，没设取启用顺序里第一个码表方案；
    /// - 拼音：启用顺序里第一个全拼方案；双拼：`schema.primary_pinyin` 是双拼就用它，否则
    ///   启用顺序里第一个双拼方案；
    /// - 混输：启用顺序里第一个混输方案；英文：出厂英文方案。
    ///
    /// 「启用顺序里没有」时退到已安装的同类方案里的第一个，免得装了却没启用的方案无处可设。
    pub fn resolve(cfg: &Config, schemas: &[SchemaInfo]) -> Self {
        let enabled: Vec<&str> = cfg
            .get_array("schema.available")
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let by_id = |id: &str| schemas.iter().find(|s| s.id == id);
        let first = |pred: &dyn Fn(&SchemaInfo) -> bool| -> Option<&SchemaInfo> {
            enabled
                .iter()
                .filter_map(|id| by_id(id))
                .find(|s| pred(s))
                .or_else(|| schemas.iter().find(|s| pred(s)))
        };
        let is_shuangpin = |s: &SchemaInfo| s.kind == "pinyin" && s.scheme == "shuangpin";
        let explicit = |path: &str, pred: &dyn Fn(&SchemaInfo) -> bool| -> Option<&SchemaInfo> {
            cfg.get_str(path)
                .filter(|id| !id.is_empty())
                .and_then(by_id)
                .filter(|s| pred(s))
        };
        let pick = [
            (
                Card::Codetable,
                explicit("schema.primary_codetable", &|s| s.kind == "codetable")
                    .or_else(|| first(&|s| s.kind == "codetable" && !s.hidden)),
            ),
            (
                Card::Pinyin,
                first(&|s| s.kind == "pinyin" && !is_shuangpin(s)),
            ),
            (
                Card::Shuangpin,
                explicit("schema.primary_pinyin", &is_shuangpin).or_else(|| first(&is_shuangpin)),
            ),
            (Card::Mixed, first(&|s| s.kind == "mixed")),
            (
                Card::English,
                by_id(ENGLISH_SCHEMA).or_else(|| first(&|s| s.kind == "english")),
            ),
        ];
        Self(
            pick.into_iter()
                .filter_map(|(c, s)| s.map(|s| (c, (s.id.clone(), s.name.clone()))))
                .collect(),
        )
    }

    pub fn id(&self, card: Card) -> Option<&str> {
        self.0.get(&card).map(|(id, _)| id.as_str())
    }

    pub fn name(&self, card: Card) -> Option<&str> {
        self.0.get(&card).map(|(_, name)| name.as_str())
    }

    /// 被绑定的方案 id，去重。
    pub fn ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self.0.values().map(|(id, _)| id.clone()).collect();
        v.sort();
        v.dedup();
        v
    }
}

/// 各卡所绑方案的设置（`schema.getSettings` 的 `effective`，键为配置键名）。
#[derive(Debug, Clone, Default)]
pub struct CardSettings {
    pub schemes: CardSchemes,
    pub effective: HashMap<String, serde_json::Map<String, Value>>,
}

impl CardSettings {
    /// 定各卡绑谁并取回它们的设置。取不到的方案（老 core、方案文件坏了）没有条目，
    /// 那张卡显示配置树里原有的值。
    pub fn fetch(cfg: &Config, schemas: &[SchemaInfo]) -> Self {
        let schemes = CardSchemes::resolve(cfg, schemas);
        let effective = schemes
            .ids()
            .into_iter()
            .filter_map(|id| {
                let v =
                    crate::rpc::call_core("schema.getSettings", &serde_json::json!({ "id": id }))
                        .ok()?;
                Some((id, v.get("effective")?.as_object()?.clone()))
            })
            .collect();
        Self { schemes, effective }
    }

    /// 把各卡所绑方案的值盖进配置树。只盖归这张卡的键：双拼方案的 `sentence_count`
    /// 不该出现在拼音卡上。
    pub fn overlay(&self, cfg: &mut Config) {
        for card in Card::ALL {
            let Some(values) = self.schemes.id(card).and_then(|id| self.effective.get(id)) else {
                continue;
            };
            for (key, value) in values {
                if Card::of_key(key) == Some(card) {
                    cfg.set_path(key, value.clone());
                }
            }
        }
    }
}

/// 保存时的分流结果。
#[derive(Debug, Default)]
pub struct SplitItems {
    /// 照旧走 `config.setItems` 的项。
    pub global: Vec<ConfigItem>,
    /// 方案 id → 该方案要写的项。
    pub by_scheme: BTreeMap<String, Vec<ConfigItem>>,
    /// 归某张卡、但那张卡没有绑定方案的项（按理不会出现：没有方案时卡片不画）。
    pub orphan: Vec<String>,
}

/// 把改动项按落点分开。
pub fn split_items(items: Vec<ConfigItem>, schemes: &CardSchemes) -> SplitItems {
    let mut out = SplitItems::default();
    for it in items {
        match Card::of_key(&it.key) {
            None => out.global.push(it),
            Some(card) => match schemes.id(card) {
                Some(id) => out.by_scheme.entry(id.to_string()).or_default().push(it),
                None => out.orphan.push(it.key),
            },
        }
    }
    out
}

#[cfg(test)]
#[path = "scheme_cards_tests.rs"]
mod tests;
