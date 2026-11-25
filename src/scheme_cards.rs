//! 「方案」页的码表 / 拼音 / 双拼 / 混输 / 英文五张卡各自绑定一个具体方案。
//!
//! core 已取消按类型共用的全局设置：`schema.codetable.*` 这类键现在是「某个方案的设置」，
//! 读写走 `schema.getSettings` / `schema.setSettings`（键名不变）。卡片仍按这些键名画控件：
//! 加载时把所绑方案的生效值盖进配置树，保存时把这些键的改动分流到所绑方案，其余键照旧走
//! `config.setItems`。设计见主仓 `docs/design/scheme-settings-flatten.md` §6。
//!
//! 全拼卡与双拼卡用的是同一批 `schema.pinyin.*` 键、绑的却是两个方案，配置树里一个键只能有
//! 一个值，所以双拼卡上的项换成 `schema.shuangpin.*`（清单里自动生成的镜像，见
//! `manifest::add_shuangpin_mirrors`），存取时用 [`ui_key`] / [`real_key`] 互换。双拼专有项
//! （`schema.pinyin.shuangpin.*`）只在双拼卡上，不必换。

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

    /// 一个（页面上的）配置键归哪张卡；不是方案设置键返回 `None`。
    /// 双拼专有项（`schema.pinyin.shuangpin.*`）与镜像（`schema.shuangpin.*`）归双拼卡，
    /// 其余拼音项归拼音卡。
    pub fn of_key(key: &str) -> Option<Card> {
        if key.starts_with("schema.codetable.") {
            Some(Card::Codetable)
        } else if key.starts_with("schema.pinyin.shuangpin.")
            || key.starts_with(crate::manifest::SHUANGPIN_MIRROR_PREFIX)
        {
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

/// 某张卡上的方案设置键在页面上叫什么：双拼卡把 `schema.pinyin.*` 换成镜像键
/// （双拼专有项除外）。不归这张卡的键返回 `None`。
pub fn ui_key(card: Card, key: &str) -> Option<String> {
    let ui = match (card, key.strip_prefix("schema.pinyin.")) {
        (Card::Shuangpin, Some(rest)) if !rest.starts_with("shuangpin.") => {
            format!("{}{rest}", crate::manifest::SHUANGPIN_MIRROR_PREFIX)
        }
        _ => key.to_string(),
    };
    (Card::of_key(&ui) == Some(card)).then_some(ui)
}

/// 页面上的键对应的真实设置键（[`ui_key`] 的逆）。
pub fn real_key(key: &str) -> String {
    match key.strip_prefix(crate::manifest::SHUANGPIN_MIRROR_PREFIX) {
        Some(rest) => format!("schema.pinyin.{rest}"),
        None => key.to_string(),
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

/// 各卡所绑方案的设置（`schema.getSettings` 的 `effective` 与 `default`，键为配置键名）。
#[derive(Debug, Clone, Default)]
pub struct CardSettings {
    pub schemes: CardSchemes,
    pub effective: HashMap<String, serde_json::Map<String, Value>>,
    /// 方案默认值：卡片「恢复默认」恢复到它，而不是 core 的出厂配置。
    pub default: HashMap<String, serde_json::Map<String, Value>>,
}

impl CardSettings {
    /// 定各卡绑谁并取回它们的设置。取不到的方案（老 core、方案文件坏了）没有条目，
    /// 那张卡显示配置树里原有的值。
    pub fn fetch(cfg: &Config, schemes: &[SchemaInfo]) -> Self {
        let schemes = CardSchemes::resolve(cfg, schemes);
        let mut out = Self {
            schemes,
            ..Self::default()
        };
        for id in out.schemes.ids() {
            let Ok(v) =
                crate::rpc::call_core("schema.getSettings", &serde_json::json!({ "id": id }))
            else {
                continue;
            };
            let map = |k: &str| v.get(k).and_then(|m| m.as_object()).cloned();
            if let Some(m) = map("effective") {
                out.effective.insert(id.clone(), m);
            }
            if let Some(m) = map("default") {
                out.default.insert(id, m);
            }
        }
        out
    }

    /// 各卡所绑方案在 `of` 里的值，键换成页面上的键（[`ui_key`]），只取归这张卡的：
    /// 双拼方案的 `sentence_count` 在双拼卡上是镜像键，不会盖到全拼卡上。
    fn card_values<'a>(
        &'a self,
        of: &'a HashMap<String, serde_json::Map<String, Value>>,
    ) -> impl Iterator<Item = (String, &'a Value)> + 'a {
        Card::ALL.into_iter().flat_map(move |card| {
            self.schemes
                .id(card)
                .and_then(|id| of.get(id))
                .into_iter()
                .flatten()
                .filter_map(move |(key, v)| ui_key(card, key).map(|k| (k, v)))
        })
    }

    /// 把各卡所绑方案的值盖进配置树。
    ///
    /// 先给双拼卡的镜像键铺一层底：拷一份配置树里的 `schema.pinyin`（双拼专有子段除外）。
    /// 没装双拼方案、或取不到它的设置时，镜像键也得在基线里有值——否则它们的写回器写出的
    /// 值与基线（缺键）对不上，界面就恒判「有未保存的更改」。
    pub fn overlay(&self, cfg: &mut Config) {
        if let Some(mut py) = cfg.get_object("schema.pinyin").cloned() {
            py.remove("shuangpin");
            let base = crate::manifest::SHUANGPIN_MIRROR_PREFIX.trim_end_matches('.');
            cfg.set_path(base, Value::Object(py));
        }
        for (key, value) in self.card_values(&self.effective) {
            cfg.set_path(&key, value.clone());
        }
    }

    /// 各卡键的方案默认值，供「恢复默认」叠在出厂配置上。
    pub fn default_patch(&self) -> Vec<(String, Value)> {
        self.card_values(&self.default)
            .map(|(k, v)| (k, v.clone()))
            .collect()
    }
}

/// 保存时的分流结果。
#[derive(Debug, Default)]
pub struct SplitItems {
    /// 照旧走 `config.setItems` 的项。
    pub global: Vec<ConfigItem>,
    /// 方案 id → 该方案要写的项。键仍是页面上的键（基线按它记），发给 core 前用
    /// [`real_key`] 换回真实键。
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
