//! 方案设置「复制到其它方案」：按块挑出源方案的生效值，写进同类目标方案的用户层。
//!
//! 一次性复制，不建立跟随关系。写入走 core `schema.setSettings`，与目标方案默认值相同的项
//! 由 core 删掉，所以复制后目标方案在这些块上的生效值与源方案一致。

use serde_json::{Map, Value};

use crate::dialogs::schema_codetable::SchemeKind;

/// 一块：名称 + 归它的键（相对 `schema.<类型>.` 的前缀）。不匹配任何块的键归 [`REST`]。
type BlockDef = (&'static str, &'static [&'static str]);

/// 不归任何具名块的项。
pub const REST: &str = "其余设置";

const CODETABLE: &[BlockDef] = &[
    ("整句", &["sentence_"]),
    ("通配", &["wildcard"]),
    ("调频", &["frequency."]),
    ("自动造词", &["auto_phrase."]),
    ("英文混入", &["english_merge."]),
];

const PINYIN: &[BlockDef] = &[
    ("整句", &["use_smart_compose", "sentence_"]),
    ("补全", &["completion."]),
    ("英文混入", &["english_merge."]),
    ("调频", &["frequency."]),
    ("自动造词", &["auto_learn."]),
    ("语法模型", &["grammar."]),
    ("双拼", &["shuangpin."]),
];

const ENGLISH: &[BlockDef] = &[("调频", &["frequency."]), ("自动造词", &["auto_learn."])];

fn defs(kind: SchemeKind) -> &'static [BlockDef] {
    match kind {
        SchemeKind::Codetable => CODETABLE,
        SchemeKind::Pinyin { .. } => PINYIN,
        SchemeKind::Mixed => &[],
        SchemeKind::English => ENGLISH,
    }
}

/// 键所属的块。`key` 是完整旧键名（`schema.pinyin.frequency.enabled`）。
pub fn block_of(kind: SchemeKind, key: &str) -> &'static str {
    let rel = key.splitn(3, '.').nth(2).unwrap_or(key);
    defs(kind)
        .iter()
        .find(|(_, prefixes)| prefixes.iter().any(|p| rel.starts_with(p)))
        .map_or(REST, |(name, _)| name)
}

/// 两个方案能否互相复制：同一类即可，全拼与双拼算同类（双拼专有项由 [`items_for`] 滤掉）。
pub fn same_family(a: SchemeKind, b: SchemeKind) -> bool {
    std::mem::discriminant(&a) == std::mem::discriminant(&b)
}

/// 复制时跳过的键：辅助码有自己的页、按方案各配各的码表；模糊音是全局项。
fn skipped(key: &str) -> bool {
    key.starts_with("schema.pinyin.aux_code.") || key.starts_with("schema.pinyin.fuzzy.")
}

/// 源方案的块列表（按定义顺序，只列有键的块，[`REST`] 在最后）。
pub fn blocks_present(kind: SchemeKind, effective: &Map<String, Value>) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = defs(kind)
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| {
            effective
                .keys()
                .any(|k| !skipped(k) && block_of(kind, k) == *name)
        })
        .collect();
    if effective
        .keys()
        .any(|k| !skipped(k) && block_of(kind, k) == REST)
    {
        out.push(REST);
    }
    out
}

/// 写给某个目标方案的项：源方案生效值里属于所选块、且目标方案收的键。
pub fn items_for(
    src_kind: SchemeKind,
    effective: &Map<String, Value>,
    blocks: &[&str],
    target_kind: SchemeKind,
) -> Vec<(String, Value)> {
    let mut keys: Vec<&String> = effective
        .keys()
        .filter(|k| !skipped(k))
        .filter(|k| blocks.contains(&block_of(src_kind, k)))
        .filter(|k| target_kind.accepts(k))
        .collect();
    keys.sort();
    keys.into_iter()
        .map(|k| (k.clone(), effective[k].clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    const FULL: SchemeKind = SchemeKind::Pinyin { shuangpin: false };
    const SP: SchemeKind = SchemeKind::Pinyin { shuangpin: true };

    #[test]
    fn keys_fall_into_named_blocks_or_rest() {
        assert_eq!(block_of(FULL, "schema.pinyin.frequency.half_life"), "调频");
        assert_eq!(block_of(FULL, "schema.pinyin.sentence_count"), "整句");
        assert_eq!(block_of(FULL, "schema.pinyin.use_smart_compose"), "整句");
        assert_eq!(block_of(FULL, "schema.pinyin.separator"), REST);
        assert_eq!(
            block_of(SchemeKind::Codetable, "schema.codetable.wildcard_key"),
            "通配"
        );
        assert_eq!(
            block_of(SchemeKind::Mixed, "schema.mix.enable_english"),
            REST
        );
    }

    #[test]
    fn blocks_present_lists_only_blocks_with_keys_and_rest_last() {
        let eff = map(json!({
            "schema.pinyin.separator": "'",
            "schema.pinyin.grammar.enabled": true,
            "schema.pinyin.frequency.enabled": true,
            "schema.pinyin.aux_code.enabled": true,
        }));
        assert_eq!(blocks_present(FULL, &eff), ["调频", "语法模型", REST]);
    }

    #[test]
    fn items_cover_chosen_blocks_and_skip_aux_code() {
        let eff = map(json!({
            "schema.pinyin.separator": "'",
            "schema.pinyin.frequency.enabled": false,
            "schema.pinyin.frequency.half_life": 30,
            "schema.pinyin.aux_code.enabled": true,
        }));
        let items = items_for(FULL, &eff, &["调频"], FULL);
        assert_eq!(
            items,
            [
                ("schema.pinyin.frequency.enabled".to_string(), json!(false)),
                ("schema.pinyin.frequency.half_life".to_string(), json!(30)),
            ]
        );
        assert!(
            items_for(FULL, &eff, &[REST], FULL)
                .iter()
                .all(|(k, _)| k == "schema.pinyin.separator")
        );
    }

    #[test]
    fn full_and_shuangpin_are_one_family() {
        assert!(same_family(FULL, SP));
        assert!(!same_family(FULL, SchemeKind::Mixed));
    }

    /// 双拼专有项只写给双拼方案。
    #[test]
    fn shuangpin_only_items_skip_full_pinyin_targets() {
        let eff = map(json!({ "schema.pinyin.shuangpin.allow_full_pinyin": true }));
        assert!(items_for(SP, &eff, &["双拼"], FULL).is_empty());
        assert_eq!(items_for(SP, &eff, &["双拼"], SP).len(), 1);
    }
}
