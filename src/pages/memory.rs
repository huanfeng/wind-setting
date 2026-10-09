//! 「高级 → 性能与诊断」里的内存诊断（只读，core `system.memoryStats`）。
//!
//! 用途是让用户反馈「输入法占内存」时能直接贴数，回答「涨的是哪一档」：进程私有内存、
//! 已加载的方案、各懒建结构自报的大小（设计 `WindInput/docs/design/memory-footprint.md` §8）。
//!
//! 分区里只有一行摘要 +「诊断信息…」按钮；全部明细在对话框里，「复制」复制的就是对话框
//! 正显示的那份文本——以后要加诊断项，只改 [`diagnostic_text`] 一处。
//!
//! # 取数时机
//!
//! - 进「高级」页时取一次（同统计页的懒加载：切页才取、只取一次），喂摘要；
//! - 打开对话框时取一次、点「刷新」再取；勾了「每 2 秒自动刷新」则由窗口定时器
//!   ([`on_tick`]) 每 2 秒取一次，对话框关着时不取。
//!
//! 每次取数的结果同时更新摘要与对话框，两处永远是同一份数。
//!
//! # 不在 UI 线程上等
//!
//! core 卡住（管道在、不回话）时 `call_core` 要等满 5 秒超时。故取数在
//! 后台线程做，结果经 `App::channel`（[`MemoryDiag::set_tx`]，`main` 注册）回到 UI 线程
//! 写信号——与升级检查同一条路。同一时刻最多一个请求在途：上一个没回来时，刷新 /
//! 定时都直接跳过，core 卡住时也不会越积越多。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

use crate::pages::dict::reactive::reactive_effect;
use crate::rpc;
use crate::state::AppState;
use crate::widgets::setting_row_desc_signal;

/// 搜索锚点（`search::index::HANDWRITTEN` 的 `advanced:memory` 条目指向这里）。
/// 盖在「内存占用」那一行上：搜到后闪的就是「诊断信息…」按钮所在行。
pub const ANCHOR: &str = "advanced:memory";

/// 分区名（清单 `section` + 二级导航 + 卡片标题，三处同名）。
pub const SECTION: &str = "性能与诊断";

/// 自动刷新的周期。窗口定时器按它注册，对话框里的勾选框文案也写死这个数。
pub const TICK: std::time::Duration = std::time::Duration::from_secs(2);

/// 后台线程送回 UI 线程的一次取数结果。
pub struct MemoryReply(pub Result<Value, String>);

/// 一次定时节拍该做什么（纯判定，见 [`tick_action`]）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Tick {
    /// 对话框没开或没勾自动刷新：什么都不做。
    Idle,
    /// 上一个请求还没回来：跳过这一拍，不排队。
    Skip,
    /// 发一次请求。
    Fire,
}

pub(crate) fn tick_action(open: bool, auto: bool, in_flight: bool) -> Tick {
    if !(open && auto) {
        Tick::Idle
    } else if in_flight {
        Tick::Skip
    } else {
        Tick::Fire
    }
}

/// 内存诊断的状态（摘要行 + 对话框）。挂在 `AppState` 上，好让 channel 回调找得到它。
#[derive(Clone)]
pub struct MemoryDiag {
    /// 分区里那一行的副标题。
    pub summary: Signal<String>,
    /// 对话框正文，也是「复制」的内容。
    pub text: Signal<String>,
    pub visible: Signal<bool>,
    /// 「每 2 秒自动刷新」勾选框。
    pub auto: Signal<bool>,
    in_flight: Rc<Cell<bool>>,
    /// 连续「读取失败」（连得上 core 却拿不到数）的次数，见 [`AUTO_FAIL_LIMIT`]。
    fail_streak: Rc<Cell<u32>>,
    /// 进页取数是否已发起过——切页只取一次。
    loaded: Rc<Cell<bool>>,
    tx: Rc<RefCell<Option<Sender<MemoryReply>>>>,
}

impl Default for MemoryDiag {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryDiag {
    pub fn new() -> Self {
        Self {
            summary: signal("加载中…".to_string()),
            text: signal("加载中…".to_string()),
            visible: signal(false),
            auto: signal(false),
            in_flight: Rc::new(Cell::new(false)),
            fail_streak: Rc::new(Cell::new(0)),
            loaded: Rc::new(Cell::new(false)),
            tx: Rc::new(RefCell::new(None)),
        }
    }

    /// 注入后台 → UI 的发送端。`main()` 在 App 构建期调用一次（channel 只能那时注册）。
    pub fn set_tx(&self, tx: Sender<MemoryReply>) {
        *self.tx.borrow_mut() = Some(tx);
    }

    /// 占住「在途」位；已有请求在途时返回 false（调用方跳过本次）。
    fn begin(&self) -> bool {
        !self.in_flight.replace(true)
    }

    /// 后台取一次。已有请求在途则跳过。
    pub fn request(&self) {
        if !self.begin() {
            return;
        }
        let Some(tx) = self.tx.borrow().clone() else {
            self.on_reply(MemoryReply(Err("取数通道未初始化".to_string())));
            return;
        };
        std::thread::spawn(move || {
            let r = rpc::call_core("system.memoryStats", &json!({}));
            // 发送失败 = 窗口已关，结果没人要了。
            let _ = tx.send(MemoryReply(r));
        });
    }

    /// 结果回到 UI 线程（channel 回调）。
    pub fn on_reply(&self, reply: MemoryReply) {
        self.in_flight.set(false);
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        match reply.0 {
            Ok(v) => {
                self.fail_streak.set(0);
                self.summary.set(summary_text(&v));
                self.text.set(diagnostic_text(&v, &stamp));
            }
            Err(e) => {
                let offline = e.starts_with(rpc::CONNECT_FAILED);
                let head = if offline { OFFLINE } else { READ_FAILED };
                let mut text = failure_text(head, &failure_reason(&e), &stamp);
                // 连不上是快速失败、不占线程，自动刷新一直跟着等 core 回来；连得上却拿不到
                // （多半是 core 卡住、每次都等满超时）则每拍漏一条等待线程，连续几次就停。
                let streak = if offline {
                    0
                } else {
                    self.fail_streak.get() + 1
                };
                self.fail_streak.set(streak);
                if streak >= AUTO_FAIL_LIMIT && self.auto.get() {
                    self.auto.set(false);
                    self.fail_streak.set(0);
                    text.push_str(&format!(
                        "\n已停止自动刷新：连续 {AUTO_FAIL_LIMIT} 次读取失败"
                    ));
                }
                self.summary.set(head.to_string());
                self.text.set(text);
            }
        }
    }

    fn ensure_loaded(&self) {
        if self.loaded.replace(true) {
            return;
        }
        self.request();
    }

    /// 打开对话框：每次打开都从「不自动刷新」起，并取一次最新数。
    fn open(&self) {
        self.auto.set(false);
        self.visible.set(true);
        self.request();
    }

    /// 窗口定时器的一拍。
    pub fn on_tick(&self) {
        if tick_action(self.visible.get(), self.auto.get(), self.in_flight.get()) == Tick::Fire {
            self.request();
        }
    }
}

/// 窗口定时器回调（主窗与重建的主窗共用）。借用不到就跳过这一拍：此刻正处在某个改状态的
/// 回调里，下一拍再来。
pub fn on_tick(app: &Rc<RefCell<AppState>>) {
    if let Ok(st) = app.try_borrow() {
        st.memory.on_tick();
    }
}

const OFFLINE: &str = "服务未连接";
const READ_FAILED: &str = "读取失败";
/// 自动刷新连续失败几次后自动取消勾选。
pub(crate) const AUTO_FAIL_LIMIT: u32 = 3;

/// 勾选框文案（跟 [`TICK`] 走）。
pub(crate) fn auto_label() -> String {
    format!("每 {} 秒自动刷新", TICK.as_secs())
}

/// 失败原因的脱敏：去掉管道 / 套接字路径（`连接 core 失败 (<路径>): <系统错误>` 只留
/// 系统错误），再把用户名段打码——复制出去的诊断文本只该有方案名和数字。
pub(crate) fn redact(e: &str, user: Option<&str>) -> String {
    let mut s = match e.strip_prefix(rpc::CONNECT_FAILED) {
        Some(rest) => match rest.split_once("): ") {
            Some((_, os_err)) => format!("{}：{os_err}", rpc::CONNECT_FAILED),
            None => rpc::CONNECT_FAILED.to_string(),
        },
        None => e.to_string(),
    };
    // 太短的用户名（单字符）替换会误伤正文，跳过。
    if let Some(u) = user.filter(|u| u.chars().count() >= 2) {
        s = s.replace(u, "<用户名>");
    }
    s
}

fn failure_reason(e: &str) -> String {
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .ok();
    redact(e, user.as_deref())
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

fn str_at<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("?")
}

fn list<'a>(v: &'a Value, k: &str) -> impl Iterator<Item = &'a Value> {
    v.get(k).and_then(Value::as_array).into_iter().flatten()
}

fn sum_of(v: &Value, key: &str, field: &str) -> u64 {
    list(v, key).map(|x| u64_at(x, field)).sum()
}

fn schema_ids(v: &Value) -> Vec<&str> {
    list(v, "schemas")
        .filter_map(|s| s.get("id").and_then(Value::as_str))
        .collect()
}

/// 口径名：与 core `process_memory` 的 `kind` 对应。
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "private" => "私有内存",
        "rss_anon" => "匿名驻留",
        _ => "进程内存",
    }
}

fn process_bytes(v: &Value) -> Option<u64> {
    v["process"].get("bytes").and_then(Value::as_u64)
}

/// 分区那一行的摘要：「当前占用 32.1 MB · 已加载 2 个方案」。
pub(crate) fn summary_text(v: &Value) -> String {
    let total = match process_bytes(v) {
        Some(b) => format!("当前占用 {}", human_bytes(b)),
        None => "当前占用：本平台不提供".to_string(),
    };
    format!("{total} · 已加载 {} 个方案", schema_ids(v).len())
}

/// 各结构自报大小的合计行，只列非零项。
pub(crate) fn parts_text(v: &Value) -> String {
    let engines: u64 = list(v, "schemas")
        .flat_map(|s| list(s, "parts"))
        .map(|p| u64_at(p, "bytes"))
        .sum();
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

const HEADER: &str = "[清风输入法 内存诊断]";

/// 对话框正文 = 「复制」的全文。只含方案 id 与数字，不含用户内容。
///
/// `stamp` 是取数时刻：自动刷新时用户靠它确认数字确实在更新，贴到反馈里也知道是哪一刻的。
pub(crate) fn diagnostic_text(v: &Value, stamp: &str) -> String {
    let p = &v["process"];
    let process = match process_bytes(v) {
        Some(b) => format!(
            "进程内存：{}（{}）",
            human_bytes(b),
            kind_label(p.get("kind").and_then(Value::as_str).unwrap_or(""))
        ),
        None => "进程内存：本平台不提供".to_string(),
    };
    let ids = schema_ids(v);
    let mut o = vec![
        format!("{HEADER} {stamp}"),
        process,
        format!("已加载方案 {} 个：{}", ids.len(), ids.join("、")),
        format!("主要结构：{}", parts_text(v)),
    ];
    for s in list(v, "schemas") {
        let parts: Vec<String> = list(s, "parts")
            .map(|p| {
                format!(
                    "{}={}（{} 块）",
                    str_at(p, "name"),
                    human_bytes(u64_at(p, "bytes")),
                    u64_at(p, "allocs")
                )
            })
            .collect();
        let parts = if parts.is_empty() {
            "无自报结构".to_string()
        } else {
            parts.join(" ")
        };
        o.push(format!(
            "方案 {} [{}] {parts}",
            str_at(s, "id"),
            str_at(s, "type")
        ));
    }
    for r in list(v, "reverseIndex") {
        o.push(format!(
            "反查索引 {}：常驻 {} / 镜像 {}",
            str_at(r, "schema"),
            human_bytes(u64_at(r, "heapBytes")),
            human_bytes(u64_at(r, "imageBytes"))
        ));
    }
    for r in list(v, "userText") {
        let building = if r.get("building").and_then(Value::as_bool) == Some(true) {
            "（重建中）"
        } else {
            ""
        };
        o.push(format!(
            "用户词编码索引 {}：{}{building}",
            str_at(r, "schema"),
            human_bytes(u64_at(r, "bytes"))
        ));
    }
    for (label, key) in [("联想索引", "userAssoc"), ("单字全码表", "singleChar")] {
        if let Some(r) = v.get(key).filter(|r| r.is_object()) {
            o.push(format!(
                "{label} {}：{}",
                str_at(r, "schema"),
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

/// 取数失败时的对话框正文。原因照列：「连不上」与「连上了但超时」修法不同。
/// `reason` 须已脱敏（[`redact`]）。
pub(crate) fn failure_text(head: &str, reason: &str, stamp: &str) -> String {
    format!("{HEADER} {stamp}\n{head}\n原因：{reason}")
}

/// 「性能与诊断」分区里的诊断行，外加挂在卡片外的懒加载哨兵。对话框登记到根
/// （`fields.dialogs`，由 `shell::build` 挂到根窗口实现全窗模态）。
pub fn build_rows(state: &AppState) -> (Element, Element) {
    let diag = state.memory.clone();
    // 关窗重开是干净的开始：不带着上一个窗口里开着的对话框 / 自动刷新。
    diag.visible.set(false);
    diag.auto.set(false);
    // 摘要也重取一次：上一个窗口里那份可能已是很久以前的数。
    diag.loaded.set(false);
    state.fields.dialogs.borrow_mut().push(build_dialog(&diag));

    let tab = state.tab;
    let target = crate::cli::page_to_index("advanced");
    if target == Some(tab.get()) {
        diag.ensure_loaded();
    }
    let diag_eff = diag.clone();
    let loader = reactive_effect(
        move || tab.version(),
        move || {
            if target == Some(tab.get()) {
                diag_eff.ensure_loaded();
            }
        },
    );

    let diag_open = diag.clone();
    let row = setting_row_desc_signal(
        "内存占用",
        diag.summary,
        Element::button("诊断信息…")
            .small()
            .outline_soft()
            .on_click(move |_| diag_open.open()),
    );
    (crate::search::anchor::wrap(row, ANCHOR, state.jump), loader)
}

fn build_dialog(diag: &MemoryDiag) -> Element {
    let content = Element::col().width_match().padding_xy(18, 12).child(
        Element::label_signal(diag.text)
            .font_size(12.5)
            .fg_role(Role::Text)
            .width_match(),
    );
    let d_refresh = diag.clone();
    let text = diag.text;
    let leading = vec![
        Element::button("刷新")
            .small()
            .outline_soft()
            .on_click(move |_| d_refresh.request()),
        Element::button("复制")
            .small()
            .outline_soft()
            .on_click(move |ctx| {
                ctx.clipboard_set(&text.get());
                ctx.toast_ok("已复制诊断信息");
            }),
        Element::checkbox(auto_label(), diag.auto).fg_role(Role::Text),
    ];
    crate::dialogs::field_dialogs::build_group_dialog_shell_large(
        diag.visible,
        "诊断信息",
        content,
        leading,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        crate::rpc::mock_reply("system.memoryStats", &json!({}))
    }

    const STAMP: &str = "2026-10-09 12:00:00";

    #[test]
    fn human_bytes_switches_unit_at_one_mb() {
        assert_eq!(human_bytes(0), "0 KB");
        assert_eq!(human_bytes(1500), "2 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024 + 300 * 1024), "3.3 MB");
    }

    #[test]
    fn summary_is_total_and_schema_count() {
        assert_eq!(
            summary_text(&sample()),
            "当前占用 32.1 MB · 已加载 2 个方案"
        );
    }

    #[test]
    fn summary_without_process_number_says_so() {
        let mut v = sample();
        v["process"] = json!({ "bytes": null, "kind": null });
        assert_eq!(summary_text(&v), "当前占用：本平台不提供 · 已加载 2 个方案");
    }

    #[test]
    fn parts_skip_zero_items() {
        let p = parts_text(&sample());
        assert!(p.contains("反查索引 2.3 MB"), "{p}");
        assert!(!p.contains("拆字表"), "零项不列：{p}");
    }

    #[test]
    fn diagnostic_text_carries_every_section() {
        let t = diagnostic_text(&sample(), STAMP);
        assert!(t.starts_with(&format!("{HEADER} {STAMP}\n")), "{t}");
        for needle in [
            "进程内存：32.1 MB（私有内存）",
            "已加载方案 2 个：wubi86、pinyin",
            "主要结构：方案引擎内索引 1.0 MB",
            "方案 wubi86 [CodeTable] sentence_index=1.0 MB（3 块）",
            "方案 pinyin [Pinyin] 无自报结构",
            "反查索引 wubi86：常驻 2.3 MB / 镜像 2.3 MB",
            "用户词编码索引 wubi86：4.2 MB",
            "拼音表 1.2 MB · 拆字表 0 KB · 注释库 0 KB",
            "用户词库读缓存上限 256.0 MB · 已空闲回收 3 次",
        ] {
            assert!(t.contains(needle), "缺「{needle}」：\n{t}");
        }
        // null 的段不出行。
        assert!(!t.contains("联想索引 "), "{t}");
    }

    #[test]
    fn diagnostic_text_marks_rebuilding_user_index() {
        let mut v = sample();
        v["userText"][0]["building"] = json!(true);
        assert!(diagnostic_text(&v, STAMP).contains("用户词编码索引 wubi86：4.2 MB（重建中）"));
    }

    /// 复制的就是对话框显示的：两者是同一个信号，回包落下后它恰是 `diagnostic_text`。
    #[test]
    fn reply_fills_dialog_text_and_summary_from_same_snapshot() {
        let d = MemoryDiag::new();
        assert!(d.begin());
        d.on_reply(MemoryReply(Ok(sample())));
        let t = d.text.get();
        let stamp = t.lines().next().unwrap().trim_start_matches(HEADER).trim();
        assert_eq!(t, diagnostic_text(&sample(), stamp));
        assert_eq!(d.summary.get(), summary_text(&sample()));
    }

    #[test]
    fn failed_reply_shows_offline_and_releases_in_flight() {
        let d = MemoryDiag::new();
        assert!(d.begin());
        d.on_reply(MemoryReply(Err("连接 core 失败".into())));
        assert_eq!(d.summary.get(), "服务未连接");
        let t = d.text.get();
        assert!(t.contains("\n服务未连接\n原因：连接 core 失败"), "{t}");
        assert!(d.begin(), "失败也要放开在途位，否则自动刷新从此停摆");
    }

    #[test]
    fn second_request_is_skipped_while_first_in_flight() {
        let d = MemoryDiag::new();
        assert!(d.begin());
        assert!(!d.begin(), "在途时不得再发");
        d.on_reply(MemoryReply(Ok(sample())));
        assert!(d.begin());
    }

    /// 没注册通道（测试 / 异常路径）不能把在途位永久占住。
    #[test]
    fn request_without_channel_fails_fast_and_stays_retryable() {
        let d = MemoryDiag::new();
        d.request();
        assert_eq!(d.summary.get(), "读取失败");
        assert!(d.text.get().contains("取数通道未初始化"));
        assert!(!d.in_flight.get());
    }

    #[test]
    fn tick_fires_only_when_open_and_auto_and_idle() {
        assert_eq!(tick_action(false, false, false), Tick::Idle);
        assert_eq!(
            tick_action(false, true, false),
            Tick::Idle,
            "对话框关着不取"
        );
        assert_eq!(
            tick_action(true, false, false),
            Tick::Idle,
            "没勾自动刷新不取"
        );
        assert_eq!(
            tick_action(true, true, true),
            Tick::Skip,
            "上一拍没回来就跳过"
        );
        assert_eq!(tick_action(true, true, false), Tick::Fire);
    }

    /// 定时拍走完整状态：关着 → 不发；开 + 勾 → 发；在途 → 跳过；回包后下一拍再发。
    #[test]
    fn on_tick_respects_dialog_auto_and_in_flight() {
        let d = MemoryDiag::new();
        d.auto.set(true);
        d.on_tick();
        // 无通道时一发就同步回落失败、摘要立刻变——所以摘要没变 = 这一拍确实没发。
        // （查 `in_flight` 没用：无通道路径会当场把它放回 false，发没发都一样。）
        assert_eq!(d.summary.get(), "加载中…", "对话框关着不该发");
        d.visible.set(true);
        d.in_flight.set(true);
        d.on_tick();
        assert_eq!(d.summary.get(), "加载中…", "在途时这一拍应跳过");
        d.in_flight.set(false);
        d.on_tick(); // 无通道 → 立即回落失败，证明这一拍确实发了
        assert_eq!(d.summary.get(), "读取失败");
    }

    /// 诊断行是按分区名拼进清单那张卡片的（`advanced_page`），清单改了名而常量没跟，
    /// 诊断行就会悄无声息地从页面上消失。
    #[test]
    fn manifest_section_matches_constant() {
        let m = crate::manifest::Manifest::from_embedded();
        assert_eq!(
            m.get("schema.keep_all_loaded").map(|i| i.section.as_str()),
            Some(SECTION)
        );
        assert!(m.sections_of("advanced").iter().any(|(s, _)| s == SECTION));
    }

    /// 只有连不上 core 才叫「服务未连接」；超时 / 方法不认 / 解析失败是「读取失败」。
    #[test]
    fn only_connect_failure_reads_as_offline() {
        let d = MemoryDiag::new();
        d.on_reply(MemoryReply(Err(
            "连接 core 失败 (/run/user/1000/wind.sock): No such file or directory (os error 2)"
                .into(),
        )));
        assert_eq!(d.summary.get(), "服务未连接");
        d.on_reply(MemoryReply(Err("core 无响应(超过 5s)".into())));
        assert_eq!(d.summary.get(), "读取失败");
        assert!(
            d.text
                .get()
                .contains("\n读取失败\n原因：core 无响应(超过 5s)"),
            "{}",
            d.text.get()
        );
    }

    /// 管道 / 套接字路径不进对话框（复制内容只含方案名和数字）；用户名段打码。
    #[test]
    fn failure_reason_drops_endpoint_and_user_name() {
        assert_eq!(
            redact(
                r"连接 core 失败 (\\.\pipe\wind_input_ctrl_dev): 系统找不到指定的文件。 (os error 2)",
                None
            ),
            "连接 core 失败：系统找不到指定的文件。 (os error 2)"
        );
        assert_eq!(
            redact(r"读不到 C:\Users\alice\AppData\x", Some("alice")),
            r"读不到 C:\Users\<用户名>\AppData\x"
        );
        let d = MemoryDiag::new();
        d.on_reply(MemoryReply(Err(
            "连接 core 失败 (/tmp/wind.sock): refused".into()
        )));
        assert!(!d.text.get().contains("/tmp/wind.sock"), "{}", d.text.get());
        assert!(!d.summary.get().contains("wind.sock"));
    }

    /// core 卡住（超时等）时自动刷新连续失败 N 次即自行取消勾选，并在对话框里说明。
    #[test]
    fn auto_refresh_stops_after_repeated_read_failures() {
        let d = MemoryDiag::new();
        d.auto.set(true);
        for _ in 0..AUTO_FAIL_LIMIT - 1 {
            d.on_reply(MemoryReply(Err("core 无响应(超过 5s)".into())));
        }
        assert!(d.auto.get(), "未到上限不该停");
        d.on_reply(MemoryReply(Err("core 无响应(超过 5s)".into())));
        assert!(!d.auto.get(), "连续 {AUTO_FAIL_LIMIT} 次失败应自动取消勾选");
        assert!(d.text.get().contains("已停止自动刷新"), "{}", d.text.get());
    }

    /// 连不上 core（离线）不算：那是快速失败、不漏线程，自动刷新要一直跟着等它回来。
    #[test]
    fn auto_refresh_keeps_going_while_offline() {
        let d = MemoryDiag::new();
        d.auto.set(true);
        for _ in 0..AUTO_FAIL_LIMIT * 2 {
            d.on_reply(MemoryReply(Err("连接 core 失败 (x): refused".into())));
        }
        assert!(d.auto.get());
    }

    /// 中间成功一次就从零数。
    #[test]
    fn success_resets_failure_streak() {
        let d = MemoryDiag::new();
        d.auto.set(true);
        for _ in 0..AUTO_FAIL_LIMIT - 1 {
            d.on_reply(MemoryReply(Err("core 无响应".into())));
        }
        d.on_reply(MemoryReply(Ok(sample())));
        d.on_reply(MemoryReply(Err("core 无响应".into())));
        assert!(d.auto.get());
    }

    #[test]
    fn open_resets_auto_and_shows_dialog() {
        let d = MemoryDiag::new();
        d.auto.set(true);
        d.open();
        assert!(d.visible.get());
        assert!(!d.auto.get(), "每次打开都从不勾选开始");
    }

    /// 主窗重建时：不带着上一个窗口开着的对话框 / 自动刷新，摘要也要重取。
    #[test]
    fn build_rows_resets_state_for_rebuilt_window() {
        let st = crate::state::AppState::empty();
        st.tab.set(0); // 不在高级页：重建后不应立刻取数，只把「已取过」复位
        st.memory.visible.set(true);
        st.memory.auto.set(true);
        st.memory.loaded.set(true);
        let _ = build_rows(&st);
        assert!(!st.memory.visible.get());
        assert!(!st.memory.auto.get());
        assert!(!st.memory.loaded.get(), "重建后进页应重新取一次摘要");
    }

    #[test]
    fn auto_label_follows_tick() {
        assert_eq!(auto_label(), format!("每 {} 秒自动刷新", TICK.as_secs()));
        assert_eq!(auto_label(), "每 2 秒自动刷新");
    }

    /// 诊断行挂在清单这个分区的卡片里：分区要是在某个平台 / 变体上被整片门控掉，
    /// 诊断行就跟着没了。门控在解析期折进 `hidden`、只看得到本平台，故直接查清单原文。
    #[test]
    fn perf_section_is_not_gated_on_any_platform() {
        let raw: toml::Value =
            toml::from_str(include_str!("../assets/settings_manifest.toml")).unwrap();
        let items: Vec<_> = raw["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i.get("section").and_then(|s| s.as_str()) == Some(SECTION))
            .collect();
        assert!(!items.is_empty());
        assert!(
            items.iter().any(|i| ["platform", "variant", "hidden"]
                .iter()
                .all(|k| i.get(*k).is_none())),
            "「{SECTION}」至少要有一项三平台、两变体都显示"
        );
    }

    #[test]
    fn failure_text_keeps_header_and_reason() {
        assert_eq!(
            failure_text("读取失败", "core 无响应", STAMP),
            format!("{HEADER} {STAMP}\n读取失败\n原因：core 无响应")
        );
    }
}
