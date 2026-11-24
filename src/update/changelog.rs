//! 跨版本更新记录：把「当前版本 → 最新版本」之间所有版本的说明拼成一份。
//!
//! 只展示最新版时，修正版（0.133.1 之类）的说明往往只有一两行，用户跳过中间版本升级后
//! 看不到真正的改动。数据源是文档站的 `changelog.json`（各版本说明的合集），纯逻辑
//! 与网络分开：这里只做解析、区间筛选与渲染，取不到数据由调用方退回单版说明。

use semver::Version;
use serde::Deserialize;

use super::version;

/// 最多展示的版本数。跨度很大时全展开既长又没人读，超出部分提示去官网看。
const MAX_VERSIONS: usize = 10;

/// `changelog.json` 的一项（与文档站 `releases.json` 同构）。
#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub version: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub notes: Vec<String>,
}

/// 解析 `changelog.json`。非数组/语法错误一律当作没有（调用方会退回单版说明）。
pub fn parse(text: &str) -> Vec<Entry> {
    serde_json::from_str(text).unwrap_or_default()
}

/// 拼出 `(local, remote]` 区间内所有版本的说明，新版在前。
///
/// 返回 `None` 表示数据不足以代替单版说明，调用方应退回老路径：
/// 区间内没有任何条目，或最新版（`remote`）本身不在合集里（合集滞后于元数据）。
pub fn compose(entries: &[Entry], local: &Version, remote: &Version) -> Option<String> {
    let mut picked: Vec<(Version, &Entry)> = entries
        .iter()
        .filter_map(|e| Some((version::parse(&e.version)?, e)))
        .filter(|(v, _)| v > local && v <= remote)
        .collect();
    picked.sort_by(|a, b| b.0.cmp(&a.0));

    if picked.first().map(|(v, _)| v) != Some(remote) {
        return None;
    }

    // 只有一版时与老路径观感一致：不加版本头。
    if picked.len() == 1 {
        return Some(render_body(picked[0].1));
    }

    let omitted = picked.len().saturating_sub(MAX_VERSIONS);
    let mut blocks: Vec<String> = picked
        .iter()
        .take(MAX_VERSIONS)
        .map(|(_, e)| {
            let head = if e.date.is_empty() {
                format!("【{}】", e.version)
            } else {
                format!("【{} · {}】", e.version, e.date)
            };
            format!("{head}\n{}", render_body(e))
        })
        .collect();
    if omitted > 0 {
        blocks.push(format!("……另有 {omitted} 个更早的版本，详见官网更新日志。"));
    }
    Some(blocks.join("\n\n"))
}

/// 一版的正文：`#` 开头的是小节标题（去井号），其余是条目（加 `- `）。
fn render_body(e: &Entry) -> String {
    let md: Vec<String> = e
        .notes
        .iter()
        .map(|n| {
            if n.starts_with('#') {
                n.clone()
            } else {
                format!("- {n}")
            }
        })
        .collect();
    super::notes::to_plain_text(&md.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(v: &str, notes: &[&str]) -> Entry {
        Entry {
            version: v.to_string(),
            date: "2026-11-01".to_string(),
            notes: notes.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn v(s: &str) -> Version {
        version::parse(s).unwrap()
    }

    #[test]
    fn spans_all_versions_between_local_and_remote() {
        let es = vec![
            entry("0.133.1", &["修正版只有一行"]),
            entry("0.133.0", &["## ✨ 新功能", "大功能"]),
            entry("0.132.0", &["旧版内容"]),
        ];
        let out = compose(&es, &v("0.132.0"), &v("0.133.1")).unwrap();
        assert_eq!(
            out,
            "【0.133.1 · 2026-11-01】\n- 修正版只有一行\n\n【0.133.0 · 2026-11-01】\n✨ 新功能\n- 大功能"
        );
    }

    #[test]
    fn local_version_itself_is_excluded() {
        let es = vec![entry("0.133.0", &["a"]), entry("0.132.0", &["b"])];
        let out = compose(&es, &v("0.132.0"), &v("0.133.0")).unwrap();
        assert!(!out.contains('b'));
    }

    #[test]
    fn build_metadata_on_local_does_not_pull_in_its_own_tag() {
        let es = vec![entry("0.133.0", &["a"]), entry("0.132.0", &["b"])];
        let out = compose(&es, &v("0.132.0+abc1234"), &v("0.133.0")).unwrap();
        assert!(!out.contains("- b"));
    }

    #[test]
    fn single_version_has_no_header() {
        let es = vec![entry("0.133.0", &["只有这一版"])];
        assert_eq!(
            compose(&es, &v("0.132.0"), &v("0.133.0")).unwrap(),
            "- 只有这一版"
        );
    }

    #[test]
    fn prerelease_local_gets_the_stable_notes() {
        let es = vec![entry("0.127.0", &["正式版"])];
        assert!(compose(&es, &v("0.127.0-beta.1"), &v("0.127.0")).is_some());
    }

    #[test]
    fn missing_remote_entry_falls_back() {
        let es = vec![entry("0.132.0", &["b"])];
        assert!(compose(&es, &v("0.131.0"), &v("0.133.0")).is_none());
    }

    #[test]
    fn empty_range_falls_back() {
        let es = vec![entry("0.130.0", &["b"])];
        assert!(compose(&es, &v("0.131.0"), &v("0.133.0")).is_none());
    }

    /// 平台版本不同步：mac 只发到 0.132.0 时，不能把 0.133.0 的内容展示给它。
    #[test]
    fn versions_newer_than_remote_are_excluded() {
        let es = vec![
            entry("0.133.0", &["不该出现"]),
            entry("0.132.0", &["应出现"]),
        ];
        let out = compose(&es, &v("0.131.0"), &v("0.132.0")).unwrap();
        assert_eq!(out, "- 应出现");
    }

    #[test]
    fn long_span_is_capped_with_hint() {
        let es: Vec<Entry> = (1..=15)
            .map(|i| entry(&format!("0.{i}.0"), &["x"]))
            .collect();
        let out = compose(&es, &v("0.0.1"), &v("0.15.0")).unwrap();
        assert!(out.contains("【0.15.0"));
        assert!(out.contains("【0.6.0"));
        assert!(!out.contains("【0.5.0"));
        assert!(out.ends_with("……另有 5 个更早的版本，详见官网更新日志。"));
    }

    #[test]
    fn malformed_json_yields_no_entries() {
        assert!(parse("<html>首页回落</html>").is_empty());
        assert!(parse("{\"a\":1}").is_empty());
    }

    #[test]
    fn unparsable_versions_are_skipped() {
        let es = vec![entry("abc", &["x"]), entry("0.133.0", &["y"])];
        assert_eq!(compose(&es, &v("0.132.0"), &v("0.133.0")).unwrap(), "- y");
    }
}
