//! 「词库统计」对话框（core `dict.systemStats`）：一个方案的系统词库有多少词、怎么分布、
//! 重码多少。入口两处——中栏末行「词库统计」与左栏方案的右键菜单——打开的是同一个对话框。
//!
//! # 用词
//!
//! 界面一律说「系统词库」：中栏已经有「用户词库 › 主词库」（用户的主分库），论坛里说的
//! 「主词库」指的却是方案自带的那份，混用就对不上号。各库分行时主库标「主」。
//!
//! # 取数
//!
//! 拼音大词库要扫几十万条（百毫秒到一秒），所以在后台线程取，结果经 `App::channel`
//! （[`StatsDialog::set_tx`]，`main` 注册）回 UI 线程——同内存诊断那条路。请求带序号，
//! 用户在结果回来之前换了方案或点了「重新统计」，旧结果直接丢掉。通道没接上（单元测试）
//! 或 mock 模式时就地同步取。
//!
//! # 正文
//!
//! 对话框正文挂在 [`StatsDialog::epoch`] 上整体重建：数据、加载态、展开态一变就重画。
//! 文案与数字由本文件的纯函数产出（[`overview_items`]、[`layer_rows`] 等），测试直接对它们。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

use super::state::DictManagerState;
use crate::rpc;

/// 重码 Top 列表向 core 要几组。
pub const TOP_N: u64 = 50;

/// 对话框宽度。混输两个成员并排时各列表格改用窄档（见 [`Widths`]）。
const DIALOG_W: i32 = 720;
/// 正文滚动区的高度上限（内容短则自然收缩）。
const BODY_MAX_H: i32 = 460;

/// 静态口径说明（ⓘ 那段）：数字都按词库自身的排序算，用户实际看到的候选还会被这些开关改动。
pub const STATIC_NOTE: &str = "说明：所有数字按词库静态排序计算，未计出简让全、单字模式、\
字符集过滤和调频。单字、词组按字数分，含 emoji、符号或字母的条目计入「其他」；\
条目按（编码, 文字）计，一个字有简码和全码算多条。";

/// 后台线程送回 UI 线程的一次取数结果。
pub struct StatsReply {
    seq: u64,
    result: Result<Value, String>,
}

/// 对话框当前处在哪一步。
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Loading,
    Failed(String),
    Ready(Value),
}

/// 正文的全部可变状态（重建时整份读）。
#[derive(Clone, Debug)]
pub struct StatsView {
    pub schema_id: String,
    /// 标题里的方案名（取数前先用左栏的名字，回来后换成 core 给的）。
    pub title: String,
    pub phase: Phase,
    /// 展开了「高级」（字集覆盖 / 加权指标）。
    pub advanced: bool,
    /// 展开了哪一列的重码 Top（混输时 0/1 是两个成员）。
    pub top: Option<usize>,
    /// 展开了哪一列哪个字集的未收字：(列, 字集 key)。
    pub missing: Option<(usize, String)>,
}

impl Default for StatsView {
    fn default() -> Self {
        Self {
            schema_id: String::new(),
            title: String::new(),
            phase: Phase::Loading,
            advanced: false,
            top: None,
            missing: None,
        }
    }
}

#[derive(Clone)]
pub struct StatsDialog {
    pub visible: Signal<bool>,
    /// 标题「词库统计 · 方案名」。
    pub heading: Signal<String>,
    /// 正文版本（单元素，见模块文档）。
    pub epoch: Signal<Vec<u64>>,
    /// 重码 Top 的「只看单字」。
    pub chars_only: Signal<bool>,
    pub view: Rc<RefCell<StatsView>>,
    seq: Rc<Cell<u64>>,
    tx: Rc<RefCell<Option<Sender<StatsReply>>>>,
}

impl Default for StatsDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl StatsDialog {
    pub fn new() -> Self {
        Self {
            visible: signal(false),
            heading: signal(String::new()),
            epoch: signal(vec![0]),
            chars_only: signal(false),
            view: Rc::new(RefCell::new(StatsView::default())),
            seq: Rc::new(Cell::new(0)),
            tx: Rc::new(RefCell::new(None)),
        }
    }

    /// 注入后台 → UI 的发送端。`main()` 在 App 构建期调用一次。
    pub fn set_tx(&self, tx: Sender<StatsReply>) {
        *self.tx.borrow_mut() = Some(tx);
    }

    fn bump(&self) {
        let title = self.view.borrow().title.clone();
        if self.heading.get() != format!("词库统计 · {title}") {
            self.heading.set(format!("词库统计 · {title}"));
        }
        let e = self.epoch.get().first().copied().unwrap_or(0);
        self.epoch.set(vec![e + 1]);
    }

    /// 打开 `schema_id` 的统计（`title` 是取数前先显示的方案名）。
    pub fn open(&self, schema_id: &str, title: &str) {
        *self.view.borrow_mut() = StatsView {
            schema_id: schema_id.to_string(),
            title: title.to_string(),
            ..Default::default()
        };
        self.chars_only.set(false);
        self.visible.set(true);
        self.request(false);
    }

    /// 取数；`refresh` 让 core 绕过缓存重算。
    pub fn request(&self, refresh: bool) {
        let seq = self.seq.get() + 1;
        self.seq.set(seq);
        let schema_id = {
            let mut v = self.view.borrow_mut();
            v.phase = Phase::Loading;
            v.schema_id.clone()
        };
        self.bump();
        let params = json!({ "schemaId": schema_id, "topN": TOP_N, "refresh": refresh });
        // mock 没有延迟，就地取：离屏截图只渲染当前帧，等不到通道回包。
        let tx = self.tx.borrow().clone().filter(|_| !rpc::is_mock());
        match tx {
            Some(tx) => {
                std::thread::spawn(move || {
                    let result = rpc::call_core("dict.systemStats", &params);
                    // 发送失败 = 窗口已关，结果没人要了。
                    let _ = tx.send(StatsReply { seq, result });
                });
            }
            None => {
                let result = rpc::call_core("dict.systemStats", &params);
                self.on_reply(StatsReply { seq, result });
            }
        }
    }

    /// 结果回到 UI 线程。不是最近一次请求的（换了方案、又点了重新统计）直接丢掉。
    pub fn on_reply(&self, reply: StatsReply) {
        if reply.seq != self.seq.get() {
            return;
        }
        {
            let mut v = self.view.borrow_mut();
            v.phase = match reply.result {
                Ok(val) => {
                    if let Some(name) = val.get("schemaName").and_then(Value::as_str)
                        && !name.is_empty()
                    {
                        v.title = name.to_string();
                    }
                    Phase::Ready(val)
                }
                Err(e) => Phase::Failed(e),
            };
        }
        self.bump();
    }

    pub fn close(&self) {
        self.visible.set(false);
    }

    fn update(&self, f: impl FnOnce(&mut StatsView)) {
        f(&mut self.view.borrow_mut());
        self.bump();
    }

    pub fn toggle_advanced(&self) {
        self.update(|v| v.advanced = !v.advanced);
    }

    pub fn toggle_top(&self, col: usize) {
        self.update(|v| v.top = if v.top == Some(col) { None } else { Some(col) });
    }

    pub fn toggle_missing(&self, col: usize, key: &str) {
        let want = (col, key.to_string());
        self.update(|v| {
            v.missing = if v.missing.as_ref() == Some(&want) {
                None
            } else {
                Some(want)
            }
        });
    }
}

impl DictManagerState {
    /// 打开数据域 `domain` 的方案的词库统计。全局域没有系统词库，不响应。
    pub fn open_stats_at(&self, domain: usize) {
        let Some(id) = self.schema_id_at(domain) else {
            return;
        };
        let label = self.domain_label_at(domain);
        let name = label.split_once(" · ").map_or(label.as_str(), |(n, _)| n);
        self.stats.open(&id, name);
    }
}

// ── 文案（纯函数）────────────────────────────────────────────

fn u64_at(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

/// 千分位：88526 → 「88,526」。
pub fn fmt_n(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn num(v: &Value, k: &str) -> String {
    fmt_n(u64_at(v, k))
}

fn engine_type(v: &Value) -> &str {
    v.get("engineType").and_then(Value::as_str).unwrap_or("")
}

/// 回包里的各列：混输是 `members`，其余就是回包本身。
pub fn columns(v: &Value) -> Vec<&Value> {
    match v.get("members").and_then(Value::as_array) {
        Some(ms) if engine_type(v) == "mixed" => ms.iter().collect(),
        _ => vec![v],
    }
}

/// 概览：(名称, 数字) 序列。
pub fn overview_items(v: &Value) -> Vec<(String, String)> {
    let t = &v["total"];
    let by = &t["byLen"];
    let english = engine_type(v) == "english";
    let mut out: Vec<(String, String)> = vec![("系统词库词条".into(), num(t, "entries"))];
    if english {
        out.push(("单词（去重）".into(), num(t, "uniqueWords")));
        for (k, label) in [
            ("1", "1 字母"),
            ("2", "2 字母"),
            ("3", "3 字母"),
            ("4", "4 字母"),
            ("5+", "5 字母及以上"),
        ] {
            out.push((label.into(), num(by, k)));
        }
    } else {
        out.push(("单字（去重）".into(), num(t, "uniqueChars")));
        out.push(("词组（去重）".into(), num(t, "uniqueWords")));
        for (k, label) in [
            ("2", "二字"),
            ("3", "三字"),
            ("4", "四字"),
            ("5+", "五字及以上"),
        ] {
            out.push((label.into(), num(by, k)));
        }
        out.push(("其他".into(), num(t, "other")));
    }
    if let Some(u) = v.get("user").filter(|u| u.is_object()) {
        out.push(("用户词".into(), num(u, "userWords")));
        out.push(("临时词".into(), num(u, "tempWords")));
    }
    out
}

/// 「范围：系统词库（已启用 2 / 3）」。
pub fn scope_line(v: &Value) -> String {
    let layers = v["layers"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let on = layers
        .iter()
        .filter(|l| l["enabled"].as_bool() == Some(true))
        .count();
    format!("范围：系统词库（已启用 {on} / {}）", layers.len())
}

/// 各词库一行：[名称, 条目, 单字, 词组, 其他]；停用 / 失败的库只给状态。
pub fn layer_rows(v: &Value) -> Vec<[String; 5]> {
    let empty = Vec::new();
    v["layers"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .map(|l| {
            let label = l["label"].as_str().unwrap_or("");
            let name = match l["kind"].as_str() {
                Some("main") => format!("{label}（主）"),
                Some("userLib") => format!("用户分库：{label}"),
                _ => label.to_string(),
            };
            let dash = || "—".to_string();
            if l["enabled"].as_bool() != Some(true) {
                return [name, "未启用".into(), dash(), dash(), dash()];
            }
            if l["failed"].as_bool() == Some(true) || l.get("entries").is_none() {
                return [name, "加载失败".into(), dash(), dash(), dash()];
            }
            [
                name,
                num(l, "entries"),
                num(l, "chars"),
                num(l, "words"),
                num(l, "other"),
            ]
        })
        .collect()
}

/// 码长分布：[(行名, [1码..≥5码])]；不是码表给空。
pub fn code_len_rows(v: &Value) -> Vec<(String, [String; 5])> {
    let cl = &v["codeLen"];
    if !cl.is_object() {
        return Vec::new();
    }
    [("单字", "char"), ("词组", "word")]
        .into_iter()
        .map(|(name, k)| {
            let m = &cl[k];
            let cells = ["1", "2", "3", "4", "5+"].map(|b| num(m, b));
            (name.to_string(), cells)
        })
        .collect()
}

fn level_name(k: &str) -> String {
    const CN: [&str; 9] = ["一", "二", "三", "四", "五", "六", "七", "八", "九"];
    match k.parse::<usize>() {
        Ok(n @ 1..=9) => format!("{}级", CN[n - 1]),
        _ => format!("{k} 码"),
    }
}

/// 「简码：一级 50 · 二级 654 · 三级 3,748（有简码的字 4,124）」；不是码表给 None。
pub fn short_code_line(v: &Value) -> Option<String> {
    let sc = v.get("shortCodes").filter(|s| s.is_object())?;
    let mut levels: Vec<(usize, String)> = sc["levels"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, n)| {
                    (
                        k.parse().unwrap_or(usize::MAX),
                        format!("{} {}", level_name(k), fmt_n(n.as_u64().unwrap_or(0))),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    levels.sort();
    let parts: Vec<String> = levels.into_iter().map(|(_, s)| s).collect();
    let body = if parts.is_empty() {
        "无".to_string()
    } else {
        parts.join(" · ")
    };
    Some(format!(
        "简码：{body}（有简码的字 {}）",
        num(sc, "charsWithShort")
    ))
}

/// 重码摘要。码表叫「重码」，拼音同音是常态，换个说法。
pub fn dup_line(v: &Value) -> String {
    let d = &v["dup"];
    let max_code = d["maxCode"].as_str().unwrap_or("");
    if engine_type(v) == "pinyin" {
        return format!(
            "同码多词的编码 {} 个 · 最多一码 {} 个（{max_code}）",
            num(d, "groups"),
            num(d, "max")
        );
    }
    if u64_at(d, "groups") == 0 {
        return "没有重码".to_string();
    }
    format!(
        "重码组 {} · 涉及 {} 条 · 最大 {} 个（编码 {max_code}）",
        num(d, "groups"),
        num(d, "entries"),
        num(d, "max")
    )
}

/// 「单字全码：重码组 2,889 · 涉及 6,402 字 · 最大 12」；拼音给 None。
pub fn char_full_line(v: &Value) -> Option<String> {
    let c = v["dup"].get("charFull").filter(|c| c.is_object())?;
    Some(format!(
        "单字全码：重码组 {} · 涉及 {} 字 · 最大 {}",
        num(c, "groups"),
        num(c, "chars"),
        num(c, "max")
    ))
}

/// 码表多码字 / 拼音多音字 + 拼音的音节与零权重。
pub fn extra_line(v: &Value) -> Option<String> {
    let t = &v["total"];
    match engine_type(v) {
        "codetable" => Some(format!("多码字 {}", num(t, "multiCodeChars"))),
        "pinyin" => {
            let p = &v["pinyin"];
            Some(format!(
                "多音字 {} · 音节 {} · 零权重条目 {}",
                num(t, "multiCodeChars"),
                num(p, "syllables"),
                num(p, "zeroWeight")
            ))
        }
        _ => None,
    }
}

/// 单字：恰一个字符且是文字（汉字属 Alphabetic），emoji 与符号不算。
fn is_single_char(t: &str) -> bool {
    let mut it = t.chars();
    matches!((it.next(), it.next()), (Some(c), None) if c.is_alphabetic() && !c.is_ascii())
}

/// Top 列表的一行：(编码, 组大小, 成员)。`chars_only` 只留单字成员、且只留还剩 ≥2 个的组。
pub fn top_rows(v: &Value, chars_only: bool) -> Vec<(String, u64, Vec<String>)> {
    let empty = Vec::new();
    v["dup"]["top"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|g| {
            let code = g["code"].as_str()?.to_string();
            let texts: Vec<String> = g["texts"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .filter(|t| !chars_only || is_single_char(t))
                .map(str::to_string)
                .collect();
            if chars_only {
                (texts.len() >= 2).then_some((code, texts.len() as u64, texts))
            } else {
                Some((code, u64_at(g, "size"), texts))
            }
        })
        .collect()
}

/// 字集表：[字集, 字数, 未收, 静重, 动态选重率, 加权码长]；码表才有后三列。
pub fn charset_rows(v: &Value) -> Vec<Vec<String>> {
    let empty = Vec::new();
    let codetable = engine_type(v) == "codetable";
    v["charsets"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .map(|c| {
            let mut row = vec![
                c["label"].as_str().unwrap_or("").to_string(),
                num(c, "size"),
                num(c, "missing"),
            ];
            if codetable {
                let opt =
                    |k: &str, f: &dyn Fn(f64) -> String| c[k].as_f64().map_or("—".to_string(), f);
                row.push(c["staticDup"].as_u64().map_or("—".into(), fmt_n));
                row.push(opt("dynRate", &|r| format!("{:.3}%", r * 100.0)));
                row.push(opt("avgCodeLen", &|x| format!("{x:.2}")));
            }
            row
        })
        .collect()
}

/// 字集表下的脚注（只码表有）：字频来源，没有字频源时说明加权指标为何空着。
pub fn freq_line(v: &Value) -> Option<String> {
    if engine_type(v) != "codetable" {
        return None; // 字频只喂码表的加权指标
    }
    Some(match v.get("freqSource").and_then(Value::as_str) {
        Some(s) => format!("字频：{s}（同字各读音权重之和）"),
        None => "没有找到白霜拼音字表，动态选重率与加权码长不显示".to_string(),
    })
}

/// 「统计于 10:21 · 用时 0.12 s」，命中缓存时注明。
pub fn status_line(v: &Value) -> String {
    let first = columns(v).first().copied().cloned().unwrap_or(Value::Null);
    let at = first["computedAtMs"]
        .as_i64()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_default();
    let ms: u64 = columns(v).iter().map(|m| u64_at(m, "elapsedMs")).sum();
    let cached = columns(v)
        .iter()
        .all(|m| m["cached"].as_bool() == Some(true));
    format!(
        "统计于 {at} · 用时 {:.2} s{}",
        ms as f64 / 1000.0,
        if cached { "（缓存）" } else { "" }
    )
}

// ── 视图 ────────────────────────────────────────────────

fn section(title: &str) -> Element {
    Element::label(title.to_string())
        .font_size(12.5)
        .font_weight(600)
        .fg_role(Role::Text)
        .padding_xy(0, 2)
}

fn muted(text: impl Into<String>) -> Element {
    Element::label(text.into())
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .width_match()
}

fn cell(text: impl Into<String>, width: i32, head: bool) -> Element {
    Element::label(text.into())
        .font_size(12.0)
        .fg_role(if head { Role::TextMuted } else { Role::Text })
        .width(width)
        .max_lines(1)
}

/// 一行表格：首列吃剩余宽度，其余定宽右侧排开。
fn table_row(first: &str, rest: &[String], width: i32, head: bool) -> Element {
    let mut row = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(4)
        .child(
            Element::label(first.to_string())
                .font_size(12.0)
                .fg_role(if head { Role::TextMuted } else { Role::Text })
                .max_lines(1)
                .weight(1.0),
        );
    for t in rest {
        row = row.child(cell(t.clone(), width, head));
    }
    row
}

/// 概览：两列网格。
fn overview_grid(items: &[(String, String)]) -> Element {
    let mut col = Element::col().width_match().spacing(3);
    for pair in items.chunks(2) {
        let mut row = Element::row().width_match().spacing(12);
        for (k, n) in pair {
            row = row.child(
                Element::row()
                    .weight(1.0)
                    .spacing(6)
                    .child(
                        Element::label(k.clone())
                            .font_size(12.0)
                            .fg_role(Role::TextMuted)
                            .weight(1.0)
                            .max_lines(1),
                    )
                    .child(
                        Element::label(n.clone())
                            .font_size(12.0)
                            .font_weight(600)
                            .fg_role(Role::Text)
                            .max_lines(1),
                    ),
            );
        }
        if pair.len() == 1 {
            row = row.child(Element::leaf().weight(1.0));
        }
        col = col.child(row);
    }
    col
}

/// 表格各列的定宽：单方案一列时宽松，混输两列并排时收窄。
#[derive(Clone, Copy)]
struct Widths {
    layer: i32,
    code_len: i32,
    charset: i32,
}

const WIDE: Widths = Widths {
    layer: 60,
    code_len: 56,
    charset: 64,
};
const NARROW: Widths = Widths {
    layer: 52,
    code_len: 48,
    charset: 46,
};

/// 一列统计（单方案即全部；混输是一个成员）。
fn member_column(d: &StatsDialog, v: &Value, col_idx: usize, view: &StatsView) -> Element {
    let ws = if columns_is_mixed(view) { NARROW } else { WIDE };
    let mut c = Element::col().width_match().spacing(6);
    if columns_is_mixed(view) {
        c = c.child(
            Element::label(v["schemaName"].as_str().unwrap_or("").to_string())
                .font_size(13.0)
                .font_weight(600)
                .fg_role(Role::Accent),
        );
    }
    c = c
        .child(section("概览"))
        .child(overview_grid(&overview_items(v)))
        .child(section("各词库"))
        .child(muted(scope_line(v)));
    let w = ws.layer;
    c = c.child(table_row(
        "词库",
        &["条目", "单字", "词组", "其他"].map(String::from),
        w,
        true,
    ));
    for r in layer_rows(v) {
        c = c.child(table_row(&r[0], &r[1..], w, false));
    }

    let rows = code_len_rows(v);
    if !rows.is_empty() {
        c = c.child(section("码长分布")).child(table_row(
            "",
            &["1 码", "2 码", "3 码", "4 码", "≥5 码"].map(String::from),
            ws.code_len,
            true,
        ));
        for (name, cells) in rows {
            c = c.child(table_row(&name, &cells, ws.code_len, false));
        }
    }
    if let Some(s) = short_code_line(v) {
        c = c.child(muted(s));
    }
    if let Some(s) = extra_line(v) {
        c = c.child(muted(s));
    }

    if engine_type(v) != "english" {
        let title = if engine_type(v) == "pinyin" {
            "同码"
        } else {
            "重码"
        };
        let has_top = v["dup"]["top"].as_array().is_some_and(|a| !a.is_empty());
        let open = view.top == Some(col_idx);
        let mut head = Element::row()
            .width_match()
            .cross(Align::Center)
            .child(section(title).weight(1.0));
        if has_top {
            let dd = d.clone();
            head = head.child(
                Element::button(if open { "收起 Top" } else { "查看 Top" })
                    .small()
                    .outline_soft()
                    .on_click(move |_| dd.toggle_top(col_idx)),
            );
        }
        c = c.child(head).child(muted(dup_line(v)));
        if let Some(s) = char_full_line(v) {
            c = c.child(muted(s));
        }
        if open {
            c = c.child(top_panel(d, v));
        }
    }

    if !v["charsets"].as_array().is_some_and(|a| a.is_empty()) && view.advanced {
        c = c.child(charset_section(d, v, col_idx, view, ws.charset));
    }
    c
}

fn columns_is_mixed(view: &StatsView) -> bool {
    matches!(&view.phase, Phase::Ready(v) if engine_type(v) == "mixed")
}

/// 重码 Top：编码（点击复制）· 组大小 · 成员。
fn top_panel(d: &StatsDialog, v: &Value) -> Element {
    let chars_only = d.chars_only.get();
    let (sig, dd) = (d.chars_only, d.clone());
    let mut col = Element::col()
        .width_match()
        .spacing(2)
        .padding_xy(10, 8)
        .bg_role(Role::SurfaceAlt)
        .corner(8.0)
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .child(muted("按组大小排列，成员按序位；点编码复制").weight(1.0))
                .child(
                    Element::checkbox("只看单字", sig)
                        .small()
                        .on_toggle(move |_| {
                            sig.set(!sig.get());
                            dd.bump();
                        }),
                ),
        );
    let rows = top_rows(v, chars_only);
    if rows.is_empty() {
        return col.child(muted("没有符合条件的组"));
    }
    for (code, size, texts) in rows {
        let copy = code.clone();
        col = col.child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(6)
                .child(
                    Element::link(code)
                        .font_size(12.0)
                        .width(72)
                        .tooltip("复制编码")
                        .on_click(move |ctx| {
                            ctx.clipboard_set(&copy);
                            ctx.toast_ok(format!("已复制编码 {copy}"));
                        }),
                )
                .child(cell(fmt_n(size), 40, false))
                .child(
                    Element::label(texts.join("、"))
                        .font_size(12.0)
                        .fg_role(Role::Text)
                        .max_lines(2)
                        .weight(1.0),
                ),
        );
    }
    col
}

/// 高级：字集覆盖（码表另有静重与加权指标）。
fn charset_section(
    d: &StatsDialog,
    v: &Value,
    col_idx: usize,
    view: &StatsView,
    w: i32,
) -> Element {
    let codetable = engine_type(v) == "codetable";
    let mut c = Element::col()
        .width_match()
        .spacing(4)
        .child(section("字集覆盖"));
    let mut head: Vec<String> = vec!["字数".into(), "未收".into()];
    if codetable {
        head.extend(["静重".into(), "选重率".into(), "加权码长".into()]);
    }
    c = c.child(table_row("字集", &head, w, true));
    let keys: Vec<String> = v["charsets"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| x["key"].as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    for (row, key) in charset_rows(v).into_iter().zip(keys) {
        let missing = row[2].clone();
        let mut r = Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(4)
            .child(
                Element::label(row[0].clone())
                    .font_size(12.0)
                    .max_lines(1)
                    .weight(1.0),
            )
            .child(cell(row[1].clone(), w, false));
        r = if missing != "0" {
            let (dd, k) = (d.clone(), key.clone());
            r.child(
                Element::link(missing)
                    .font_size(12.0)
                    .width(w)
                    .tooltip("查看未收字")
                    .on_click(move |_| dd.toggle_missing(col_idx, &k)),
            )
        } else {
            r.child(cell(missing, w, false))
        };
        for t in &row[3..] {
            r = r.child(cell(t.clone(), w, false));
        }
        c = c.child(r);
        if view.missing.as_ref() == Some(&(col_idx, key.clone())) {
            let chars: Vec<&str> = v["charsets"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|x| x["key"].as_str() == Some(key.as_str()))
                .and_then(|x| x["missingChars"].as_array())
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            c = c.child(
                Element::label(chars.join(" "))
                    .font_size(13.0)
                    .width_match()
                    .padding_xy(8, 6)
                    .bg_role(Role::SurfaceAlt)
                    .corner(6.0),
            );
        }
    }
    if codetable {
        c = c.child(muted(
            "静重：字集内按单字全码分组的非首选字数；选重率：按字频加权、最优码不在首选的比例",
        ));
    }
    if let Some(s) = freq_line(v) {
        c = c.child(muted(s));
    }
    c
}

/// 正文（挂在 epoch 上重建）。
fn build_body(d: &StatsDialog) -> Element {
    let view = d.view.borrow().clone();
    let v = match &view.phase {
        Phase::Loading => {
            return Element::col()
                .width_match()
                .padding_xy(18, 24)
                .child(muted("统计中…（大词库需要一两秒）"));
        }
        Phase::Failed(e) => {
            let dd = d.clone();
            return Element::col()
                .width_match()
                .padding_xy(18, 18)
                .spacing(10)
                .child(
                    Element::label(format!("统计失败：{e}"))
                        .font_size(12.5)
                        .fg_role(Role::Danger)
                        .width_match(),
                )
                .child(
                    Element::button("重试")
                        .small()
                        .outline_soft()
                        .on_click(move |_| dd.request(false)),
                );
        }
        Phase::Ready(v) => v,
    };
    let cols = columns(v);
    let mut row = Element::row().width_match().spacing(16);
    for (i, m) in cols.iter().enumerate() {
        if i > 0 {
            row = row.child(
                Element::leaf()
                    .width(1)
                    .height_match()
                    .bg_role(Role::Divider),
            );
        }
        row = row.child(
            Element::col()
                .weight(1.0)
                .child(member_column(d, m, i, &view)),
        );
    }
    let any_charsets = cols
        .iter()
        .any(|m| m["charsets"].as_array().is_some_and(|a| !a.is_empty()));
    let mut col = Element::col()
        .width_match()
        .spacing(10)
        .padding_xy(18, 12)
        .child(muted(status_line(v)))
        .child(row);
    if any_charsets {
        let dd = d.clone();
        col = col.child(
            Element::link(if view.advanced {
                "收起高级统计"
            } else {
                "展开高级统计（字集覆盖 / 动态选重率 / 平均码长）"
            })
            .font_size(12.0)
            .on_click(move |_| dd.toggle_advanced()),
        );
    }
    col.child(
        Element::label(STATIC_NOTE)
            .font_size(11.5)
            .fg_role(Role::TextSubtle)
            .width_match(),
    )
}

/// 词库统计对话框（挂进 `build_dict_dialogs` 的 stack）。
///
/// 外框定宽、标题走信号，只有正文挂在 epoch 上重建：`host_signal` 的容器是 `fill` 的，
/// 拿它当对话框的直接子节点会撑满整个遮罩层、面板就不居中了。滚动放在这一层（同
/// `comment_dicts_dialog` 的说明），内层用非滚动的 `host_signal`。
pub fn build_stats_dialog(mgr: &DictManagerState) -> Element {
    let d = mgr.stats.clone();
    let (d_refresh, d_busy, d_close, d_body) = (d.clone(), d.clone(), d.clone(), d.clone());
    let header = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(18, 12)
        .child(
            Element::label(d.heading)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .max_lines(1)
                .weight(1.0),
        )
        .child(
            Element::button("重新统计")
                .small()
                .outline_soft()
                .enabled_when(move || d_busy.view.borrow().phase != Phase::Loading)
                .tooltip("不用缓存，重新扫一遍词库")
                .on_click(move |_| d_refresh.request(true)),
        )
        .child(
            Element::button("关闭")
                .small()
                .outline_soft()
                .on_click(move |_| d_close.close()),
        );
    let frame = Element::col()
        .width(DIALOG_W)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(header)
        .child(Element::divider())
        .child(
            Element::scroll()
                .width_match()
                .max_height(BODY_MAX_H)
                .child(Element::host_signal(d.epoch, move |_| build_body(&d_body))),
        );
    Element::dialog(mgr.stats.visible, frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_mock<R>(f: impl FnOnce() -> R) -> R {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        crate::rpc::take_mock_calls();
        let r = f();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        r
    }

    fn ready(d: &StatsDialog) -> Value {
        match &d.view.borrow().phase {
            Phase::Ready(v) => v.clone(),
            p => panic!("应已取到数：{p:?}"),
        }
    }

    #[test]
    fn thousands_separator() {
        assert_eq!(fmt_n(0), "0");
        assert_eq!(fmt_n(999), "999");
        assert_eq!(fmt_n(88_526), "88,526");
        assert_eq!(fmt_n(1_234_567), "1,234,567");
    }

    /// 码表：概览、各词库、码长、简码、重码与字集各项都按回包出文案。
    #[test]
    fn codetable_texts_follow_the_reply() {
        with_mock(|| {
            let d = StatsDialog::new();
            d.open("wubi86", "五笔 86");
            assert!(d.visible.get());
            let v = ready(&d);
            assert_eq!(d.view.borrow().title, "五笔 86");
            let calls = crate::rpc::take_mock_calls();
            assert!(calls.iter().any(|(m, p)| m == "dict.systemStats"
                && *p == json!({"schemaId": "wubi86", "topN": 50, "refresh": false})));

            let ov = overview_items(&v);
            assert_eq!(ov[0], ("系统词库词条".into(), "90,624".into()));
            assert!(ov.contains(&("五字及以上".into(), "359".into())));
            assert!(ov.contains(&("用户词".into(), "312".into())));
            assert_eq!(scope_line(&v), "范围：系统词库（已启用 3 / 4）");
            let rows = layer_rows(&v);
            assert_eq!(rows[0][0], "极点五笔主词库（主）");
            assert_eq!(rows[0][1], "88,526");
            assert_eq!(rows[3][1], "未启用");
            assert_eq!(code_len_rows(&v)[0].1[3], "19,845");
            assert_eq!(
                short_code_line(&v).unwrap(),
                "简码：一级 50 · 二级 654 · 三级 3,748（有简码的字 4,124）"
            );
            assert_eq!(
                dup_line(&v),
                "重码组 9,657 · 涉及 22,191 条 · 最大 71 个（编码 emoj）"
            );
            assert_eq!(
                char_full_line(&v).unwrap(),
                "单字全码：重码组 2,889 · 涉及 6,402 字 · 最大 12"
            );
            let cs = charset_rows(&v);
            assert_eq!(cs[0], ["通规一级", "3,500", "0", "66", "0.091%", "1.81"]);
            assert!(freq_line(&v).unwrap().starts_with("字频：白霜拼音字表"));
        });
    }

    #[test]
    fn top_rows_filter_to_single_chars() {
        with_mock(|| {
            let d = StatsDialog::new();
            d.open("wubi86", "五笔 86");
            let v = ready(&d);
            let all = top_rows(&v, false);
            assert_eq!(all[0].0, "emoj");
            assert_eq!(all[0].1, 71);
            let chars = top_rows(&v, true);
            assert!(
                chars
                    .iter()
                    .all(|(_, n, t)| *n >= 2 && t.len() as u64 == *n)
            );
            assert!(!chars.iter().any(|(c, ..)| c == "emoj"), "emoji 组没有单字");
        });
    }

    #[test]
    fn pinyin_and_mixed_columns() {
        with_mock(|| {
            let d = StatsDialog::new();
            d.open("pinyin_simp", "拼音");
            let v = ready(&d);
            assert_eq!(columns(&v).len(), 1);
            assert_eq!(
                extra_line(&v).unwrap(),
                "多音字 9,042 · 音节 424 · 零权重条目 45,138"
            );
            assert!(dup_line(&v).starts_with("同码多词的编码 41,330 个"));
            assert!(char_full_line(&v).is_none() && code_len_rows(&v).is_empty());
            assert_eq!(charset_rows(&v)[2], ["通规全表", "8,105", "0"]);

            d.open("wubi_mixed", "五笔拼音混输");
            let v = ready(&d);
            let cols = columns(&v);
            assert_eq!(cols.len(), 2, "混输并排两个成员");
            assert_eq!(cols[0]["engineType"], "codetable");
            assert_eq!(cols[1]["engineType"], "pinyin");
            assert!(columns_is_mixed(&d.view.borrow()));
        });
    }

    /// 失败态带原因；换方案后迟到的旧结果被丢掉。
    #[test]
    fn failure_and_stale_replies() {
        with_mock(|| {
            let d = StatsDialog::new();
            crate::rpc::force_mock_error("dict.systemStats", Some("方案不存在: x"));
            d.open("x", "x");
            crate::rpc::force_mock_error("dict.systemStats", None);
            assert_eq!(d.view.borrow().phase, Phase::Failed("方案不存在: x".into()));
            d.request(false);
            let before = d.view.borrow().phase.clone();
            d.on_reply(StatsReply {
                seq: d.seq.get() - 1,
                result: Ok(json!({"stale": true})),
            });
            assert_eq!(d.view.borrow().phase, before, "旧请求的结果不该覆盖");
        });
    }

    /// 展开态切换都会重建正文。
    #[test]
    fn toggles_bump_the_epoch() {
        with_mock(|| {
            let d = StatsDialog::new();
            d.open("wubi86", "五笔 86");
            let e0 = d.epoch.get()[0];
            d.toggle_advanced();
            d.toggle_top(0);
            d.toggle_missing(0, "common_han_12");
            assert_eq!(d.epoch.get()[0], e0 + 3);
            let v = d.view.borrow();
            assert!(v.advanced);
            assert_eq!(v.top, Some(0));
            assert_eq!(v.missing, Some((0, "common_han_12".into())));
            drop(v);
            d.toggle_top(0);
            assert_eq!(d.view.borrow().top, None);
        });
    }
}
