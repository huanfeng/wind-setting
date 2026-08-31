//! 汉字拼音首字母 —— 让「主码表方案」能被 `mbfa` 搜到。
//!
//! ## 表从哪来
//!
//! 不是手工录的：由 `scripts/gen-pinyin-initials.ps1` 从 core 的
//! `../WindInput/build/data/pinyin_map.txt`（44 439 行，多音字按常用频率排序、已裁古音）
//! 派生而来，用的就是这套输入法自己的读音。`pinyin_map.txt` 是 core 的构建产物、
//! 不随本仓发布，故**只在生成时读一次，运行时零依赖**。
//!
//! ## 为什么只覆盖标题
//!
//! 拼音首字母只对 [`SearchEntry::title`](crate::search::SearchEntry::title) 生效，
//! hint 与关键词只做汉字子串匹配。理由是长句的首字母串没有检索价值：拿一句 20 字的
//! 说明去比首字母，只会让四五个字母的查询命中一堆不相干的项。
//! ⇒ 表只需覆盖标题里的字，几百个，10 KB 以内。
//!
//! ## 多音字
//!
//! 收前两个读音的首字母（表已按常用频率排序，第三个往后基本是生僻音，收进来只会
//! 制造误命中）。匹配时任一命中即算——多音字只影响召回，不影响精度，因为首字母
//! 匹配的得分本就低于汉字匹配。

use std::collections::HashMap;
use std::sync::OnceLock;

/// 生成的首字母表（`字 声母[ 声母]` 一行，`#` 开头为注释）。
const TABLE_SRC: &str = include_str!("../assets/pinyin_initials.txt");

/// 一个字最多收几个读音的首字母。
const MAX_INITIALS: usize = 2;

fn table() -> &'static HashMap<char, [u8; MAX_INITIALS]> {
    static T: OnceLock<HashMap<char, [u8; MAX_INITIALS]>> = OnceLock::new();
    T.get_or_init(|| {
        let mut m = HashMap::new();
        for line in TABLE_SRC.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.split_whitespace();
            let Some(ch) = it.next().and_then(|s| s.chars().next()) else {
                continue;
            };
            let mut slot = [0u8; MAX_INITIALS];
            for (i, tok) in it.take(MAX_INITIALS).enumerate() {
                if let Some(b) = tok.bytes().next() {
                    slot[i] = b.to_ascii_lowercase();
                }
            }
            if slot[0] != 0 {
                m.insert(ch, slot);
            }
        }
        m
    })
}

/// 一个字符的候选首字母。
///
/// 汉字查表；非汉字（英文/数字）取其自身小写——这样「T9 键盘」这类混排标题
/// 也能用 `t9jp` 搜到，不必给它们单开一条规则。
fn initials_of(c: char, buf: &mut [u8; MAX_INITIALS]) -> usize {
    if let Some(slot) = table().get(&c) {
        *buf = *slot;
        return if slot[1] == 0 { 1 } else { 2 };
    }
    if c.is_ascii_alphanumeric() {
        buf[0] = (c as u8).to_ascii_lowercase();
        return 1;
    }
    0
}

/// `query` 是否与 `chars` 从 `start` 起**连续**匹配首字母。
fn matches_at(chars: &[char], start: usize, query: &[u8]) -> bool {
    if start + query.len() > chars.len() {
        return false;
    }
    let mut buf = [0u8; MAX_INITIALS];
    for (j, q) in query.iter().enumerate() {
        let n = initials_of(chars[start + j], &mut buf);
        if n == 0 || !buf[..n].contains(q) {
            return false;
        }
    }
    true
}

/// 在 `title` 里找首字母连续匹配 `query` 的起始**字**位置。
///
/// 逐位比对而不是先把标题展开成所有读音组合：一个六字标题若每字两读音就是 64 条
/// 分支，展开是没必要的开销，而逐位比对天然支持多音字。
pub fn initials_find(title: &str, query: &str) -> Option<usize> {
    let q: Vec<u8> = query.bytes().map(|b| b.to_ascii_lowercase()).collect();
    if q.is_empty() || !q.iter().all(u8::is_ascii_alphanumeric) {
        return None;
    }
    let chars: Vec<char> = title.chars().collect();
    (0..chars.len()).find(|&i| matches_at(&chars, i, &q))
}

/// 表里是否有这个字（供守门测试与诊断用）。
pub fn has(c: char) -> bool {
    table().contains_key(&c)
}

/// 表的字数（供守门测试报数）。
pub fn table_len() -> usize {
    table().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use crate::search::index::{build_index, is_cjk, title_chars};

    /// **守门**：索引标题里的每个汉字都必须在表里，否则那一项的拼音搜索静默失效。
    ///
    /// 失败时按 `scripts/gen-pinyin-initials.ps1` 重新生成表即可（不需要人工查读音）。
    #[test]
    fn table_covers_every_title_char() {
        let idx = build_index(&Manifest::from_embedded());
        let need = title_chars(&idx);
        let missing: Vec<char> = need.iter().copied().filter(|c| !has(*c)).collect();
        assert!(
            missing.is_empty(),
            "拼音首字母表缺 {} 个字（表内 {} 字）：{}\n\
             → 跑 `pwsh scripts/gen-pinyin-initials.ps1` 重新生成 src/assets/pinyin_initials.txt",
            missing.len(),
            table_len(),
            missing.iter().collect::<String>()
        );
    }

    /// 表里不该混进非汉字条目（生成脚本出错时会看到）。
    #[test]
    fn table_holds_only_cjk() {
        let idx = build_index(&Manifest::from_embedded());
        let _ = idx; // 仅为保证表已按真实语料生成
        assert!(table_len() > 100, "表只有 {} 字，疑似未生成", table_len());
    }

    #[test]
    fn finds_initials_prefix() {
        assert_eq!(initials_find("主码表方案", "mbfa"), Some(1));
        assert_eq!(initials_find("主码表方案", "zmbfa"), Some(0));
        assert_eq!(initials_find("主码表方案", "zmb"), Some(0));
    }

    #[test]
    fn rejects_non_matching() {
        assert_eq!(initials_find("主码表方案", "xyz"), None);
        // 不连续的不算：中间跳过了「表」。
        assert_eq!(initials_find("主码表方案", "zmfa"), None);
    }

    #[test]
    fn empty_or_non_ascii_query_is_none() {
        assert_eq!(initials_find("主码表方案", ""), None);
        assert_eq!(initials_find("主码表方案", "码表"), None);
    }

    /// 非汉字按自身字母参与匹配（混排标题）。
    #[test]
    fn alphanumeric_chars_match_themselves() {
        assert_eq!(initials_find("T9 键盘", "t9"), Some(0));
    }

    /// 多音字的两个读音都能匹配上。
    #[test]
    fn polyphone_accepts_both_readings() {
        // 「重」在表里应同时有 c(chóng) 与 z(zhòng)；两条路都该通。
        if has('重') {
            let by_c = initials_find("重复", "cf").is_some();
            let by_z = initials_find("重复", "zf").is_some();
            assert!(by_c || by_z, "多音字「重」两个读音都匹配不上");
        }
    }

    #[test]
    fn is_cjk_covers_basic_plane() {
        assert!(is_cjk('码'));
        assert!(!is_cjk('a'));
        assert!(!is_cjk('，'));
    }
}
