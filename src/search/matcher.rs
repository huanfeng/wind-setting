//! 匹配与排序 —— 把一串查询变成一小列结果。
//!
//! 三条路各有得分档（见下 [`score`]），同分按索引原序稳定排序，取前 N 条。
//!
//! 不做模糊/编辑距离匹配：设置项标题都很短，错一个字宁可没有结果，也不要给一个
//! 看起来像、其实不是的项——用户会照着它改配置。

use super::index::SearchEntry;
use super::pinyin;

/// 一条命中。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchHit {
    /// 在传入的 entries 里的下标。
    pub index: usize,
    pub score: i32,
}

/// 结果条数上限（浮层最多显示这么多）。
pub const DEFAULT_LIMIT: usize = 12;

// 得分档。数值本身不重要，档与档之间拉开距离才重要——避免"关键词命中"意外
// 排到"标题前缀"前面。
const S_TITLE_EXACT: i32 = 1000;
const S_TITLE_PREFIX: i32 = 800;
const S_TITLE_SUBSTR: i32 = 600;
const S_INITIALS_PREFIX: i32 = 500;
const S_INITIALS_SUBSTR: i32 = 350;
const S_KEYWORD: i32 = 200;
const S_ID: i32 = 150;

/// 方案级条目的降权。
///
/// 它们**到不了**——「哪个方案」未定，点了只能给一条指示、由用户自己走过去。
/// 同样匹配程度下，能一步到位的那条更该排在前面。
///
/// 只降 30 分（档与档之间隔 150+），所以**不跨档**：方案级项若是标题前缀命中、
/// 全局项只是子串命中，前者仍然排在前面——那时它确实是更贴题的答案，
/// 结果列表会给它一个标记说明需要先选方案（P1）。
const P_SCHEMA_LEVEL: i32 = 30;

/// 导航型条目的加成 —— 「大的优先」。
///
/// 搜「统计」时，用户要找的是**统计这块功能**，不是「统计英文输入」这个开关。
/// 但按纯文本匹配，「统计英文输入」是标题前缀（800）、「使用统计」只是子串（600），
/// 开关反而排前面——反直觉。
///
/// 导航型条目（手写页的整块卡片、词库窗）代表「去某个地方」，通常正是搜索时的首要
/// 意图；具体设置项则是到了那里之后的事。故给它们跨一档的加成（250 > 前缀与子串
/// 之间的 200），让同样命中的情况下整块优先。
const B_NAVIGATION: i32 = 250;

/// 给一条 entry 打分；0 = 不命中。
fn score(entry: &SearchEntry, query: &str) -> i32 {
    let raw = score_match(entry, query);
    if raw == 0 {
        return 0;
    }
    match entry.target {
        super::JumpTarget::SchemaLevel { .. } => raw - P_SCHEMA_LEVEL,
        super::JumpTarget::Card { .. } | super::JumpTarget::DictWindow => raw + B_NAVIGATION,
        _ => raw,
    }
}

/// 纯匹配得分，不含可达性降权。
fn score_match(entry: &SearchEntry, query: &str) -> i32 {
    let title = entry.title.as_str();
    // ── 汉字/原文匹配（大小写不敏感）──
    let (lt, lq) = (title.to_lowercase(), query.to_lowercase());
    if lt == lq {
        return S_TITLE_EXACT;
    }
    if lt.starts_with(&lq) {
        return S_TITLE_PREFIX;
    }
    if lt.contains(&lq) {
        return S_TITLE_SUBSTR;
    }
    // ── 拼音首字母（只对标题）──
    if let Some(pos) = pinyin::initials_find(title, query) {
        return if pos == 0 {
            S_INITIALS_PREFIX
        } else {
            S_INITIALS_SUBSTR
        };
    }
    // ── 关键词（hint 首句 / 别名 / key 末段）──
    if entry
        .keywords
        .iter()
        .any(|k| k.to_lowercase().contains(&lq))
    {
        return S_KEYWORD;
    }
    // ── 配置 key 本身（用户按 `codetable` 这类英文标识找）──
    if entry.id.to_lowercase().contains(&lq) {
        return S_ID;
    }
    0
}

/// 检索。返回按得分降序、同分按索引原序的前 `limit` 条。
///
/// 查询为空或全是空白时返回空——空查询该收起浮层，而不是把整个索引倒出来。
pub fn search(entries: &[SearchEntry], query: &str, limit: usize) -> Vec<SearchHit> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<SearchHit> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, e)| {
            let score = score(e, q);
            (score > 0).then_some(SearchHit { index, score })
        })
        .collect();
    // sort_by_key 是稳定排序：同分保持索引原序（即 manifest 原序 → 页面顺序）。
    hits.sort_by_key(|h| -h.score);
    hits.truncate(limit);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use crate::search::index::build_index;

    fn idx() -> Vec<SearchEntry> {
        build_index(&Manifest::from_embedded())
    }

    fn titles(entries: &[SearchEntry], hits: &[SearchHit]) -> Vec<String> {
        hits.iter()
            .map(|h| entries[h.index].title.clone())
            .collect()
    }

    /// 人工核对检索质量：打印若干典型查询的结果。
    ///
    /// 打分档位是否合理、有没有明显该在前面却排到后面的项，只能靠眼睛看——
    /// 断言能守住「命中/不命中」，守不住「排得对不对」。改了打分就跑一次这个。
    #[test]
    #[ignore = "人工核对：cargo test demo_queries -- --ignored --nocapture"]
    fn demo_queries() {
        let e = idx();
        for q in [
            "码表",
            "mbfa",
            "hxzh",
            "统计",
            "横排",
            "词库",
            "备份",
            "codetable",
            "主题",
        ] {
            println!("\n── 「{q}」 ──");
            for h in search(&e, q, 8) {
                let it = &e[h.index];
                println!("  {:>4}  {}  ({})", h.score, it.title, it.path.join(" › "));
            }
        }
    }

    #[test]
    fn empty_query_returns_nothing() {
        let e = idx();
        assert!(search(&e, "", DEFAULT_LIMIT).is_empty());
        assert!(search(&e, "   ", DEFAULT_LIMIT).is_empty());
    }

    /// 汉字子串：搜「码表」应命中主码表方案。
    #[test]
    fn han_substring_hits() {
        let e = idx();
        let hits = search(&e, "码表", DEFAULT_LIMIT);
        let t = titles(&e, &hits);
        assert!(t.iter().any(|x| x == "主码表方案"), "实际结果：{t:?}");
    }

    /// 拼音首字母：`mbfa` 应命中主码表方案。
    #[test]
    fn initials_hit_primary_codetable() {
        let e = idx();
        let hits = search(&e, "mbfa", DEFAULT_LIMIT);
        let t = titles(&e, &hits);
        assert!(t.iter().any(|x| x == "主码表方案"), "实际结果：{t:?}");
    }

    /// 完全相等 > 前缀 > 子串：排序档位不能串。
    #[test]
    fn ranks_exact_before_prefix_before_substring() {
        let e = vec![
            SearchEntry {
                id: "a".into(),
                title: "候选窗口大小".into(),
                keywords: vec![],
                path: vec![],
                target: crate::search::JumpTarget::Card {
                    tab: 0,
                    anchor: "a".into(),
                },
                feature: "",
            },
            SearchEntry {
                id: "b".into(),
                title: "候选窗口".into(),
                keywords: vec![],
                path: vec![],
                target: crate::search::JumpTarget::Card {
                    tab: 0,
                    anchor: "b".into(),
                },
                feature: "",
            },
            SearchEntry {
                id: "c".into(),
                title: "调整候选窗口".into(),
                keywords: vec![],
                path: vec![],
                target: crate::search::JumpTarget::Card {
                    tab: 0,
                    anchor: "c".into(),
                },
                feature: "",
            },
        ];
        let hits = search(&e, "候选窗口", DEFAULT_LIMIT);
        assert_eq!(
            titles(&e, &hits),
            vec!["候选窗口", "候选窗口大小", "调整候选窗口"]
        );
    }

    /// 标题匹配必须排在关键词匹配之前。
    #[test]
    fn title_outranks_keyword() {
        let e = vec![
            SearchEntry {
                id: "kw".into(),
                title: "无关标题".into(),
                keywords: vec!["候选窗口".into()],
                path: vec![],
                target: crate::search::JumpTarget::Card {
                    tab: 0,
                    anchor: "kw".into(),
                },
                feature: "",
            },
            SearchEntry {
                id: "ti".into(),
                title: "候选窗口".into(),
                keywords: vec![],
                path: vec![],
                target: crate::search::JumpTarget::Card {
                    tab: 0,
                    anchor: "ti".into(),
                },
                feature: "",
            },
        ];
        let hits = search(&e, "候选窗口", DEFAULT_LIMIT);
        assert_eq!(titles(&e, &hits), vec!["候选窗口", "无关标题"]);
    }

    /// 英文 key 也能搜：`codetable` 应命中码表相关项。
    #[test]
    fn english_key_hits() {
        let e = idx();
        let hits = search(&e, "codetable", DEFAULT_LIMIT);
        assert!(!hits.is_empty(), "搜 codetable 应有结果");
    }

    /// 结果条数受 limit 限制。
    #[test]
    fn respects_limit() {
        let e = idx();
        let hits = search(&e, "设置", 3);
        assert!(hits.len() <= 3);
    }

    /// 同样匹配程度下，可直达的条目排在够不着的方案级条目之前。
    #[test]
    fn reachable_outranks_schema_level() {
        let e = vec![
            SearchEntry {
                id: "schema-level:码表配置".into(),
                title: "码表配置".into(),
                keywords: vec![],
                path: vec![],
                target: crate::search::JumpTarget::SchemaLevel {
                    hint_path: vec!["方案".into()],
                },
                feature: "",
            },
            SearchEntry {
                id: "reachable".into(),
                title: "码表配置".into(),
                keywords: vec![],
                path: vec![],
                // 用 Row 而不是 Card：Card 带导航加成，会把这条测试想验证的
                // 「同分时可直达优先」盖过去（见 B_NAVIGATION）。
                target: crate::search::JumpTarget::Row {
                    tab: 0,
                    anchor: "r".into(),
                },
                feature: "",
            },
        ];
        let hits = search(&e, "码表配置", DEFAULT_LIMIT);
        assert_eq!(
            e[hits[0].index].id, "reachable",
            "同样匹配时，能直达的那条该排前面"
        );
    }

    /// 降权不跨档：方案级的标题前缀命中仍应排在全局项的子串命中之前。
    #[test]
    fn schema_level_penalty_does_not_cross_tiers() {
        let e = vec![
            SearchEntry {
                id: "global".into(),
                title: "主码表方案".into(), // 子串命中
                keywords: vec![],
                path: vec![],
                // 同上：这里要的是中性替身。
                target: crate::search::JumpTarget::Row {
                    tab: 0,
                    anchor: "g".into(),
                },
                feature: "",
            },
            SearchEntry {
                id: "schema-level:码表配置".into(),
                title: "码表配置".into(), // 前缀命中，高一档
                keywords: vec![],
                path: vec![],
                target: crate::search::JumpTarget::SchemaLevel {
                    hint_path: vec!["方案".into()],
                },
                feature: "",
            },
        ];
        let hits = search(&e, "码表", DEFAULT_LIMIT);
        assert_eq!(e[hits[0].index].title, "码表配置", "降权不该跨档位");
    }

    /// **回归**：搜「统计」时，统计这块功能本身要排在「统计英文输入」那类开关前面。
    ///
    /// 用户反馈的原话是「大的优先」。按纯文本匹配，开关是标题前缀、功能块只是子串，
    /// 开关反而在前——这条钉住导航加成把次序摆正了。
    #[test]
    fn navigation_entries_outrank_settings() {
        let e = idx();
        let hits = search(&e, "统计", DEFAULT_LIMIT);
        let t = titles(&e, &hits);
        assert_eq!(
            t.first().map(String::as_str),
            Some("使用统计"),
            "「统计」的首条应是统计功能本身，实际：{t:?}"
        );
    }

    /// 词库同理：整块的「词库管理」压过具体设置项。
    #[test]
    fn dict_window_entry_ranks_first() {
        let e = idx();
        let hits = search(&e, "词库", DEFAULT_LIMIT);
        let t = titles(&e, &hits);
        assert_eq!(t.first().map(String::as_str), Some("词库管理"), "{t:?}");
    }

    /// 加成不能把不相干的整块顶上来：没命中就是没命中。
    #[test]
    fn navigation_bonus_needs_a_match() {
        let e = idx();
        let hits = search(&e, "统计", DEFAULT_LIMIT);
        for h in &hits {
            assert!(h.score > 0);
        }
        // 「备份与还原」与「统计」无关，不该出现。
        let t = titles(&e, &hits);
        assert!(!t.iter().any(|x| x == "备份与还原"), "{t:?}");
    }

    /// 同分保持索引原序（稳定排序），避免每次搜索结果顺序抖动。
    #[test]
    fn equal_scores_keep_index_order() {
        let e = idx();
        let hits = search(&e, "候选", DEFAULT_LIMIT);
        let same: Vec<usize> = hits
            .iter()
            .filter(|h| h.score == hits[0].score)
            .map(|h| h.index)
            .collect();
        let mut sorted = same.clone();
        sorted.sort_unstable();
        assert_eq!(same, sorted, "同分结果未按索引原序排列");
    }
}
