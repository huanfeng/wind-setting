//! 统计页数据层：RPC 拉取 + 本地聚合。
//!
//! 两次 RPC 就够：`stats.summary` 取已算好的概览指标，`stats.daily` 取区间每日明细。
//! 字符分类 / 码长分布 / 方案占比 / 时段柱状图**全部由 daily 本地聚合**，不再为每张图开 RPC。
//!
//! core 的 `stats.daily` 已补齐区间内每一天（缺失日填零值）并按日期升序返回
//! （见 `wind-coordinator/src/webdata.rs::web_stats_daily`），故本模块不做补空日与排序，
//! 直接顺序消费。JSON 字段名是 core 为 webview 传输压缩过的两三字母缩写，
//! 这里一次性 `rename` 成可读名——页面与图表代码不再接触缩写。

use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;

use crate::rpc;

/// 热力图默认回溯天数（半年）。实际列数由可用宽度决定，见 `charts::Heatmap`。
pub const DEFAULT_DAYS_BACK: i64 = 180;

/// `stats.summary` 返回（17 字段，形状对齐 core `web_stats_summary`）。
///
/// core 侧已处理「DB 未 flush 时用采集器内存值覆盖今天」，UI 直接用，不要重算。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Summary {
    pub today_chars: u32,
    pub today_chinese: u32,
    pub today_english: u32,
    pub total_chars: u64,
    pub active_days: u32,
    pub daily_avg: u32,
    pub streak_current: u32,
    pub streak_max: u32,
    pub week_chars: u32,
    pub month_chars: u32,
    pub max_day_chars: u32,
    pub max_day_date: String,
    pub avg_code_len: f64,
    pub first_select_rate: f64,
    pub today_speed: u32,
    pub overall_speed: u32,
    pub max_speed: u32,
}

/// 单方案的当日分项（`bs` 映射的值）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SchemaShare {
    #[serde(rename = "tc")]
    pub total_chars: u32,
    #[serde(rename = "cn")]
    pub commit_count: u32,
    #[serde(rename = "cls")]
    pub code_len_sum: u32,
    #[serde(rename = "clc")]
    pub code_len_count: u32,
    #[serde(rename = "cpd")]
    pub cand_pos_dist: [u32; 5],
}

/// 每日统计明细（`stats.daily` 数组元素）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DailyItem {
    /// `YYYY-MM-DD`。
    #[serde(rename = "d")]
    pub date: String,
    /// 总字数（= 中 + 英 + 符 + 数，core 侧已求和）。
    #[serde(rename = "tc")]
    pub total: u32,
    #[serde(rename = "cc")]
    pub chinese: u32,
    #[serde(rename = "ec")]
    pub english: u32,
    #[serde(rename = "pc")]
    pub punct: u32,
    #[serde(rename = "oc")]
    pub other: u32,
    /// 24 小时分布。
    #[serde(rename = "h")]
    pub hours: [u32; 24],
    #[serde(rename = "cn")]
    pub commit_count: u32,
    #[serde(rename = "cls")]
    pub code_len_sum: u32,
    #[serde(rename = "clc")]
    pub code_len_count: u32,
    /// 码长分布 [1码, 2码, 3码, 4码, 5码, 6码+]。
    #[serde(rename = "cld")]
    pub code_len_dist: [u32; 6],
    /// 候选位置分布（[0] 为首选命中数）。
    #[serde(rename = "cpd")]
    pub cand_pos_dist: [u32; 5],
    /// 活跃秒数（算速度用）。
    #[serde(rename = "as")]
    pub active_seconds: u32,
    /// 方案 id → 该方案当日分项。
    #[serde(rename = "bs")]
    pub by_schema: HashMap<String, SchemaShare>,
    #[serde(rename = "src")]
    pub by_source: Vec<u32>,
}

/// 统计页的一份完整数据快照。
#[derive(Debug, Clone, Default)]
pub struct StatsData {
    pub summary: Summary,
    /// 按日期升序、区间内连续（含零值日）。
    pub days: Vec<DailyItem>,
}

impl StatsData {
    /// 今日明细（取 `days` 末元素并校验日期，避免跨零点后错取昨天）。
    pub fn today(&self) -> Option<&DailyItem> {
        let key = today_key();
        self.days.last().filter(|d| d.date == key)
    }
}

// ─────────────────────────── 日期工具 ───────────────────────────

/// 今天的 `YYYY-MM-DD`（本地时区，与 core 的 `today_str()` 同口径）。
pub fn today_key() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 今天往前 `days` 天的 `YYYY-MM-DD`。
pub fn date_key_before(days: i64) -> String {
    (chrono::Local::now() - chrono::Duration::days(days))
        .format("%Y-%m-%d")
        .to_string()
}

/// 解析 `YYYY-MM-DD`，失败返回 None。
pub fn parse_key(key: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(key, "%Y-%m-%d").ok()
}

/// 周一=0 … 周日=6（热力图按周一起始排行）。
pub fn weekday_mon0(date: chrono::NaiveDate) -> usize {
    use chrono::Datelike;
    date.weekday().num_days_from_monday() as usize
}

/// `2026-07-29` → `7月29日`。
pub fn format_date_label(key: &str) -> String {
    use chrono::Datelike;
    match parse_key(key) {
        Some(d) => format!("{}月{}日", d.month(), d.day()),
        None => key.to_string(),
    }
}

/// `2026-07-29` → `星期三`。
pub fn format_weekday_label(key: &str) -> String {
    const NAMES: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];
    match parse_key(key) {
        Some(d) => format!("星期{}", NAMES[weekday_mon0(d)]),
        None => String::new(),
    }
}

// ─────────────────────────── 数字格式 ───────────────────────────

/// 大数缩写：≥1 万显示 `1.2万`，否则千分位。
pub fn format_num(n: u64) -> String {
    if n >= 10_000 {
        return format!("{:.1}万", n as f64 / 10_000.0);
    }
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

// ─────────────────────────── 聚合 ───────────────────────────

/// 水平条形图的一行。
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub label: String,
    pub count: u64,
    /// 0..=100 的整数百分比（仅用于条长与文案，不参与再计算）。
    pub pct: u32,
}

/// 把 (标签, 计数) 列表折算成百分比条，过滤零项；全零返回空表示「无数据」。
fn to_bars(items: Vec<(String, u64)>) -> Vec<Bar> {
    let total: u64 = items.iter().map(|(_, c)| *c).sum();
    if total == 0 {
        return Vec::new();
    }
    items
        .into_iter()
        .filter(|(_, c)| *c > 0)
        .map(|(label, count)| Bar {
            label,
            count,
            pct: ((count * 100 + total / 2) / total) as u32,
        })
        .collect()
}

/// 字符分类占比（统计窗口内累计）。
pub fn char_category_bars(days: &[DailyItem]) -> Vec<Bar> {
    let mut cc = 0u64;
    let mut ec = 0u64;
    let mut pc = 0u64;
    let mut oc = 0u64;
    for d in days {
        cc += d.chinese as u64;
        ec += d.english as u64;
        pc += d.punct as u64;
        oc += d.other as u64;
    }
    to_bars(vec![
        ("中文".into(), cc),
        ("英文".into(), ec),
        ("标点".into(), pc),
        ("数字".into(), oc),
    ])
}

/// 码长分布占比。
pub fn code_len_bars(days: &[DailyItem]) -> Vec<Bar> {
    const LABELS: [&str; 6] = ["1码", "2码", "3码", "4码", "5码", "6码+"];
    let mut dist = [0u64; 6];
    for d in days {
        for (i, v) in d.code_len_dist.iter().enumerate() {
            dist[i] += *v as u64;
        }
    }
    to_bars(
        LABELS
            .iter()
            .zip(dist)
            .map(|(l, c)| ((*l).to_string(), c))
            .collect(),
    )
}

/// 方案占比，按字数降序。
///
/// `display_name` 把方案 id 映射成用户可见名（取自已加载的方案列表；未知 id 回退原样），
/// 故本函数不硬编码方案名——新增方案无需改这里。
pub fn schema_bars(days: &[DailyItem], display_name: impl Fn(&str) -> String) -> Vec<Bar> {
    let mut map: HashMap<String, u64> = HashMap::new();
    for d in days {
        for (id, s) in &d.by_schema {
            *map.entry(id.clone()).or_default() += s.total_chars as u64;
        }
    }
    let mut items: Vec<(String, u64)> = map.into_iter().collect();
    // 先按字数降序，再按 id 稳定排序，保证同值时顺序确定（截图可复现）。
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    to_bars(
        items
            .into_iter()
            .map(|(id, c)| (display_name(&id), c))
            .collect(),
    )
}

/// 热力图色阶档位 0..=4（阈值对齐 Go 版：0 / <500 / <2000 / <5000 / 更多）。
pub fn heat_level(chars: u32) -> usize {
    match chars {
        0 => 0,
        1..=499 => 1,
        500..=1999 => 2,
        2000..=4999 => 3,
        _ => 4,
    }
}

// ─────────────────────────── RPC ───────────────────────────

/// 拉取统计快照（概览 + 近 `days_back` 天明细）。
///
/// 两个调用并行发起：本地 redb 查询很快，但管道往返各自独立，并行省一次 RTT。
/// 任一失败即整体失败——半份数据画出来的图会误导，不如明确报错。
pub fn fetch(days_back: i64) -> Result<StatsData, String> {
    let from = date_key_before(days_back);
    let to = today_key();

    let h_summary = std::thread::spawn(|| rpc::call_core("stats.summary", &json!({})));
    let daily_res = rpc::call_core("stats.daily", &json!({ "from": from, "to": to }));

    let summary_val = h_summary
        .join()
        .map_err(|_| "统计概览线程异常".to_string())??;
    let daily_val = daily_res?;

    let summary: Summary =
        serde_json::from_value(summary_val).map_err(|e| format!("统计概览解析失败: {e}"))?;
    let days: Vec<DailyItem> =
        serde_json::from_value(daily_val).map_err(|e| format!("每日统计解析失败: {e}"))?;

    Ok(StatsData { summary, days })
}

/// 清空全部统计数据。
pub fn clear() -> Result<(), String> {
    rpc::call_core("stats.clear", &json!({})).map(|_| ())
}

/// 清理 `days` 天前的数据，返回被删除的天数。
pub fn prune_before(days: i64) -> Result<u64, String> {
    let v = rpc::call_core("stats.pruneBefore", &json!({ "days": days }))?;
    Ok(v.get("count").and_then(|c| c.as_u64()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(date: &str, cc: u32, ec: u32) -> DailyItem {
        DailyItem {
            date: date.into(),
            total: cc + ec,
            chinese: cc,
            english: ec,
            ..Default::default()
        }
    }

    #[test]
    fn format_num_uses_wan_above_ten_thousand() {
        assert_eq!(format_num(0), "0");
        assert_eq!(format_num(999), "999");
        assert_eq!(format_num(1_234), "1,234");
        assert_eq!(format_num(9_999), "9,999");
        assert_eq!(format_num(12_345), "1.2万");
    }

    #[test]
    fn heat_level_thresholds_match_reference() {
        assert_eq!(heat_level(0), 0);
        assert_eq!(heat_level(1), 1);
        assert_eq!(heat_level(499), 1);
        assert_eq!(heat_level(500), 2);
        assert_eq!(heat_level(1_999), 2);
        assert_eq!(heat_level(2_000), 3);
        assert_eq!(heat_level(4_999), 3);
        assert_eq!(heat_level(5_000), 4);
    }

    #[test]
    fn bars_drop_zero_items_and_sum_to_full() {
        let days = vec![day("2026-07-28", 700, 300), day("2026-07-29", 300, 700)];
        let bars = char_category_bars(&days);
        // 标点/数字全零 → 不出现在结果里。
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].label, "中文");
        assert_eq!(bars[0].count, 1_000);
        assert_eq!(bars[0].pct, 50);
        assert_eq!(bars[1].pct, 50);
    }

    #[test]
    fn all_zero_yields_empty_bars() {
        assert!(char_category_bars(&[day("2026-07-29", 0, 0)]).is_empty());
        assert!(code_len_bars(&[]).is_empty());
    }

    #[test]
    fn schema_bars_sort_desc_and_map_names() {
        let mut d = day("2026-07-29", 0, 0);
        d.by_schema.insert(
            "wubi86".into(),
            SchemaShare {
                total_chars: 300,
                ..Default::default()
            },
        );
        d.by_schema.insert(
            "pinyin".into(),
            SchemaShare {
                total_chars: 700,
                ..Default::default()
            },
        );
        let bars = schema_bars(&[d], |id| match id {
            "wubi86" => "五笔86".into(),
            other => other.into(),
        });
        assert_eq!(bars[0].label, "pinyin");
        assert_eq!(bars[0].pct, 70);
        assert_eq!(bars[1].label, "五笔86");
    }

    #[test]
    fn weekday_mon0_starts_on_monday() {
        // 2026-07-27 是周一。
        assert_eq!(weekday_mon0(parse_key("2026-07-27").unwrap()), 0);
        assert_eq!(weekday_mon0(parse_key("2026-08-02").unwrap()), 6);
    }
}
