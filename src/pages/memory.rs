//! 「高级 → 性能」里的「内存占用」卡片（只读诊断，core `system.memoryStats`）。
//!
//! 用途是让用户反馈「输入法占内存」时能直接贴数，回答「涨的是哪一档」：进程私有内存、
//! 已加载的方案、各懒建结构自报的大小（设计 `WindInput/docs/design/memory-footprint.md` §8）。
//!
//! 进「高级」页时拉一次（同统计页的懒加载：切页才拉、只拉一次），「刷新」重拉；
//! 「复制诊断信息」现拉一份全量文本进剪贴板——不复用卡片上那份，卡片可能是几分钟前的。
//! 同步 RPC：core 侧各结构自报，大词库约十毫秒级，不值得为它起后台线程。

use std::cell::Cell;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::core::EventCtx;
use windui::prelude::*;

use crate::pages::dict::reactive::reactive_effect;
use crate::rpc;
use crate::state::AppState;
use crate::widgets::{section, setting_row, setting_row_desc_signal};

/// 搜索锚点（`search::index::HANDWRITTEN` 的 `advanced:memory` 条目指向这里）。
pub const ANCHOR: &str = "advanced:memory";

#[derive(Clone)]
struct MemoryCard {
    /// 「当前占用」行的副标题：总量 + 已加载方案。
    summary: Signal<String>,
    /// 「主要结构」行的副标题：各结构自报大小。
    parts: Signal<String>,
    loaded: Rc<Cell<bool>>,
}

impl MemoryCard {
    fn new() -> Self {
        Self {
            summary: signal("加载中…".to_string()),
            parts: signal(String::new()),
            loaded: Rc::new(Cell::new(false)),
        }
    }

    fn ensure_loaded(&self) {
        if self.loaded.replace(true) {
            return;
        }
        self.reload();
    }

    fn reload(&self) {
        self.loaded.set(true);
        match fetch() {
            Ok(v) => {
                self.summary.set(summary_text(&v));
                self.parts.set(parts_text(&v));
            }
            Err(e) => {
                self.summary.set(format!("读取失败：{e}"));
                self.parts.set(String::new());
            }
        }
    }
}

fn fetch() -> Result<Value, String> {
    rpc::call_core("system.memoryStats", &json!({}))
}

/// 字节数 → 「12.3 MB」/「456 KB」。
pub(crate) fn human_bytes(b: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if b >= 1024 * 1024 {
        format!("{:.1} MB", b as f64 / MB)
    } else {
        format!("{} KB", b.div_ceil(1024))
    }
}

fn u64_at(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

fn sum_of(v: &Value, list: &str, field: &str) -> u64 {
    v.get(list)
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|x| u64_at(x, field)).sum())
        .unwrap_or(0)
}

fn schema_ids(v: &Value) -> Vec<String> {
    v.get("schemas")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| s.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// 口径名：与 core `process_memory` 的 `kind` 对应。
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "private" => "私有内存",
        "rss_anon" => "匿名驻留",
        _ => "进程内存",
    }
}

/// 「当前占用 32.1 MB（私有内存）· 已加载方案 3 个：a、b、c」。
pub(crate) fn summary_text(v: &Value) -> String {
    let p = &v["process"];
    let total = match p.get("bytes").and_then(Value::as_u64) {
        Some(b) => format!(
            "当前占用 {}（{}）",
            human_bytes(b),
            kind_label(p.get("kind").and_then(Value::as_str).unwrap_or(""))
        ),
        None => "当前占用：本平台不提供".to_string(),
    };
    let ids = schema_ids(v);
    format!("{total} · 已加载方案 {} 个：{}", ids.len(), ids.join("、"))
}

/// 各结构自报大小，只列非零项。
pub(crate) fn parts_text(v: &Value) -> String {
    let engines: u64 = v
        .get("schemas")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .flat_map(|s| {
                    s.get("parts")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .map(|p| u64_at(p, "bytes"))
                .sum()
        })
        .unwrap_or(0);
    let rl = &v["reverseLookup"];
    let items = [
        ("方案引擎内索引", engines),
        ("反查索引", sum_of(v, "reverseIndex", "heapBytes")),
        ("用户词编码索引", sum_of(v, "userText", "bytes")),
        ("联想索引", u64_at(&v["userAssoc"], "bytes")),
        ("单字全码表", u64_at(&v["singleChar"], "bytes")),
        ("拼音表", u64_at(rl, "pinyinBytes")),
        ("拆字表", u64_at(rl, "chaiziBytes")),
        ("注释库", u64_at(rl, "commentBytes")),
    ];
    let shown: Vec<String> = items
        .iter()
        .filter(|(_, b)| *b > 0)
        .map(|(name, b)| format!("{name} {}", human_bytes(*b)))
        .collect();
    if shown.is_empty() {
        "暂无已建的索引".to_string()
    } else {
        shown.join(" · ")
    }
}

/// 「复制诊断信息」的全文：摘要 + 逐方案 / 逐结构明细。只含方案 id 与数字，不含用户内容。
pub(crate) fn diagnostic_text(v: &Value) -> String {
    let mut o = vec!["[清风输入法 内存诊断]".to_string(), summary_text(v)];
    if let Some(a) = v.get("schemas").and_then(Value::as_array) {
        for s in a {
            let parts: Vec<String> = s
                .get("parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|p| {
                    format!(
                        "{}={}",
                        p.get("name").and_then(Value::as_str).unwrap_or("?"),
                        human_bytes(u64_at(p, "bytes"))
                    )
                })
                .collect();
            o.push(format!(
                "方案 {} [{}] {}",
                s.get("id").and_then(Value::as_str).unwrap_or("?"),
                s.get("type").and_then(Value::as_str).unwrap_or("?"),
                parts.join(" ")
            ));
        }
    }
    for r in v
        .get("reverseIndex")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        o.push(format!(
            "反查索引 {}：常驻 {} / 镜像 {}",
            r.get("schema").and_then(Value::as_str).unwrap_or("?"),
            human_bytes(u64_at(r, "heapBytes")),
            human_bytes(u64_at(r, "imageBytes"))
        ));
    }
    for r in v
        .get("userText")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        o.push(format!(
            "用户词编码索引 {}：{}",
            r.get("schema").and_then(Value::as_str).unwrap_or("?"),
            human_bytes(u64_at(r, "bytes"))
        ));
    }
    for (label, key) in [("联想索引", "userAssoc"), ("单字全码表", "singleChar")] {
        if let Some(r) = v.get(key).filter(|r| r.is_object()) {
            o.push(format!(
                "{label} {}：{}",
                r.get("schema").and_then(Value::as_str).unwrap_or("?"),
                human_bytes(u64_at(r, "bytes"))
            ));
        }
    }
    let rl = &v["reverseLookup"];
    o.push(format!(
        "拼音表 {} · 拆字表 {} · 注释库 {}",
        human_bytes(u64_at(rl, "pinyinBytes")),
        human_bytes(u64_at(rl, "chaiziBytes")),
        human_bytes(u64_at(rl, "commentBytes"))
    ));
    if let Some(r) = v.get("redb").filter(|r| r.is_object()) {
        o.push(format!(
            "用户词库读缓存上限 {} · 已空闲回收 {} 次",
            human_bytes(u64_at(r, "cacheLimitBytes")),
            u64_at(r, "pageCacheDrops")
        ));
    }
    o.join("\n")
}

/// 「内存占用」卡片。进「高级」页时懒加载一次（`--page advanced` 冷启动同样覆盖）。
pub fn build_memory_section(state: &AppState) -> Element {
    let card = MemoryCard::new();

    let tab = state.tab;
    let target = crate::cli::page_to_index("advanced");
    if target == Some(tab.get()) {
        card.ensure_loaded();
    }
    let card_eff = card.clone();
    let loader = reactive_effect(
        move || tab.version(),
        move || {
            if target == Some(tab.get()) {
                card_eff.ensure_loaded();
            }
        },
    );

    let card_refresh = card.clone();
    let copy = move |ctx: &mut EventCtx| match fetch() {
        Ok(v) => {
            ctx.clipboard_set(&diagnostic_text(&v));
            ctx.toast_ok("已复制内存诊断信息");
        }
        Err(e) => ctx.toast_err(format!("读取失败：{e}")),
    };

    let rows = vec![
        setting_row_desc_signal(
            "当前占用",
            card.summary,
            Element::button("刷新")
                .small()
                .outline_soft()
                .on_click(move |_| card_refresh.reload()),
        ),
        setting_row_desc_signal("主要结构", card.parts, Element::leaf()),
        setting_row(
            "诊断信息",
            Some("反馈内存问题时贴上它，不含输入内容"),
            Element::button("复制诊断信息")
                .small()
                .outline_soft()
                .on_click(copy),
        ),
    ];
    // 懒加载哨兵挂在卡片外：混进 rows 会多出一条分隔线（见 widgets::card_with_attachment）。
    Element::col()
        .width_match()
        .child(section("内存占用", rows))
        .child(loader)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        crate::rpc::mock_reply("system.memoryStats", &json!({}))
    }

    #[test]
    fn human_bytes_switches_unit_at_one_mb() {
        assert_eq!(human_bytes(0), "0 KB");
        assert_eq!(human_bytes(1500), "2 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024 + 300 * 1024), "3.3 MB");
    }

    #[test]
    fn summary_lists_total_and_loaded_schemas() {
        let s = summary_text(&sample());
        assert!(s.starts_with("当前占用 32.1 MB（私有内存）"), "{s}");
        assert!(s.contains("已加载方案 2 个：wubi86、pinyin"), "{s}");
    }

    #[test]
    fn summary_without_process_number_says_so() {
        let mut v = sample();
        v["process"] = json!({ "bytes": null, "kind": null });
        assert!(summary_text(&v).starts_with("当前占用：本平台不提供"));
    }

    #[test]
    fn parts_skip_zero_items() {
        let p = parts_text(&sample());
        assert!(p.contains("反查索引 2.3 MB"), "{p}");
        assert!(!p.contains("拆字表"), "零项不列：{p}");
    }

    #[test]
    fn diagnostic_text_carries_every_section() {
        let t = diagnostic_text(&sample());
        for needle in [
            "方案 wubi86 [CodeTable]",
            "反查索引 wubi86",
            "用户词编码索引 wubi86",
            "拼音表",
            "用户词库读缓存上限 256.0 MB",
        ] {
            assert!(t.contains(needle), "缺「{needle}」：\n{t}");
        }
    }
}
