//! 统计页 — 输入习惯与效率指标。
//!
//! 与其他页不同，本页主体不是 manifest 驱动的配置表单，而是数据可视化：
//! 一次 [`data::fetch`] 取回快照，页面各块都从这份快照本地聚合，不再各自发 RPC。
//! 唯二的配置项（`stats.enabled` / `stats.track_english`）仍走 manifest 行渲染，
//! 保持与其他页一致的外观与写回路径。
//!
//! **数据区整体重建**：数字、条形图行数都随数据变化，逐个绑 `Signal<String>` 既繁琐又易漏，
//! 故用 [`Element::host_signal`] 把整个数据区挂在一个信号上——数据一到就重建子树。
//! 设置区不依赖统计数据，留在宿主外，避免无谓重建。

pub mod charts;
pub mod data;

use std::cell::Cell;
use std::rc::Rc;

use windui::core::EventCtx;
use windui::prelude::*;

use crate::manifest::section_rows;
use crate::pages::dict::reactive::{Feedback, reactive_effect, toast_sink};
use crate::state::AppState;
use crate::widgets::{page, section, setting_row};
use data::{Bar, StatsData};

/// 数据清理的可选范围。`None` = 清空全部。
const PRUNE_OPTIONS: &[(&str, Option<i64>)] = &[
    ("30 天前", Some(30)),
    ("90 天前", Some(90)),
    ("180 天前", Some(180)),
    ("1 年前", Some(365)),
    ("2 年前", Some(730)),
    ("全部数据", None),
];

/// 页尾为悬浮刷新按钮预留的空白高度。
const FAB_CLEARANCE: i32 = 40;

/// 数据区三态。
#[derive(Clone)]
enum View {
    Loading,
    Failed(String),
    /// `Rc` 承载：`host_signal` 每次重建都会 clone 载荷，180 天明细深拷贝不值当。
    Ready(Rc<StatsData>),
}

/// 待确认的破坏性操作。
#[derive(Debug, Clone, Copy, PartialEq)]
enum Pending {
    Idle,
    ClearAll,
    Prune(i64),
}

/// 统计页状态。
#[derive(Clone)]
pub struct StatsState {
    /// 数据区三态（恒 1 个元素，`host_signal` 的载体）。
    view: Signal<Vec<View>>,
    /// 是否已发起过加载——切页只拉一次，来回切换不重复请求。
    loaded: Rc<Cell<bool>>,
    /// 清理范围下拉选中项（索引对应 [`PRUNE_OPTIONS`]）。
    prune_idx: Signal<usize>,
    confirm_visible: Signal<bool>,
    confirm_msg: Signal<String>,
    pending: Rc<Cell<Pending>>,
    feedback: Feedback,
    /// 热力图悬停详情（空 = 未悬停）。
    hover: Signal<String>,
    /// 时段柱状图悬停详情（与热力图各用一行，互不覆盖）。
    hour_hover: Signal<String>,
    /// 方案 `(id, 显示名)`，由 [`AppState::apply_loaded`] 填充。
    ///
    /// 用信号而非构建期快照：页面树在冷启动 RPC 回来之前就建好了，那时 `state.schemas`
    /// 还是空的——直接快照会让「方案占比」永远显示裸 id。
    pub schema_names: Signal<Vec<(String, String)>>,
}

impl Default for StatsState {
    fn default() -> Self {
        Self::new()
    }
}

impl StatsState {
    pub fn new() -> Self {
        Self {
            view: signal(vec![View::Loading]),
            loaded: Rc::new(Cell::new(false)),
            prune_idx: signal(0),
            confirm_visible: signal(false),
            confirm_msg: signal(String::new()),
            pending: Rc::new(Cell::new(Pending::Idle)),
            feedback: signal(None),
            hover: signal(String::new()),
            hour_hover: signal(String::new()),
            schema_names: signal(Vec::new()),
        }
    }

    fn notify(&self, kind: ToastKind, msg: impl Into<String>) {
        self.feedback.set(Some((kind, msg.into())));
    }

    /// 首次进入统计页时拉一次数据。
    fn ensure_loaded(&self) {
        if self.loaded.replace(true) {
            return;
        }
        self.reload();
    }

    /// 重新拉取数据。
    ///
    /// 同步调用：本地 redb 范围扫描 + 一次管道往返，180 天量级在毫秒档，
    /// 且只在进页/手动刷新时发生，尚不值得为它引入后台线程 + `App::channel` 回投。
    /// 若日后统计窗口拉长到卡顿可感，再改走 channel。
    fn reload(&self) {
        self.loaded.set(true);
        match data::fetch(data::DEFAULT_DAYS_BACK) {
            Ok(d) => self.view.set(vec![View::Ready(Rc::new(d))]),
            Err(e) => self.view.set(vec![View::Failed(e)]),
        }
    }

    /// 按下拉选中的范围发起清理，先弹确认（不可恢复）。
    fn request_prune(&self) {
        let (label, days) = PRUNE_OPTIONS[self.prune_idx.get().min(PRUNE_OPTIONS.len() - 1)];
        let (pending, msg) = match days {
            None => (
                Pending::ClearAll,
                "确定要清空全部统计数据吗？此操作不可恢复。".to_string(),
            ),
            Some(d) => (
                Pending::Prune(d),
                format!("确定要清理{label}的统计数据吗？此操作不可恢复。"),
            ),
        };
        self.pending.set(pending);
        self.confirm_msg.set(msg);
        self.confirm_visible.set(true);
    }

    /// 执行已确认的操作，完成后刷新数据。
    fn run_pending(&self) {
        self.confirm_visible.set(false);
        let result = match self.pending.replace(Pending::Idle) {
            Pending::Idle => return,
            Pending::ClearAll => data::clear().map(|()| "统计数据已清空".to_string()),
            Pending::Prune(days) => {
                data::prune_before(days).map(|n| format!("已清理 {n} 天统计数据"))
            }
        };
        match result {
            Ok(msg) => {
                self.notify(ToastKind::Success, msg);
                self.reload();
            }
            Err(e) => self.notify(ToastKind::Error, format!("清理失败: {e}")),
        }
    }

    fn cancel_pending(&self) {
        self.pending.set(Pending::Idle);
        self.confirm_visible.set(false);
    }
}

// ───────────────────────── 页面组装 ─────────────────────────

/// 统计页。
pub fn build_page(state: &AppState) -> Element {
    let mgr = state.stats_mgr.clone();

    let schema_names = mgr.schema_names;
    let (hover, hour_hover) = (mgr.hover, mgr.hour_hover);
    let data_area = Element::host_signal(mgr.view, move |v| match v {
        View::Loading => hint_row("加载中…"),
        View::Failed(e) => hint_row(&format!("统计数据加载失败：{e}")),
        View::Ready(d) => ready_body(&d, &schema_names.get(), hover, hour_hover),
    });

    let body = Element::col()
        .width_match()
        .spacing(18)
        .child(data_area)
        .child(settings_section(state))
        .child(load_effect(state))
        // 页尾让位：悬浮按钮盖在内容之上，没有这块留白，滚到底时它会压住最后一行
        //（正好是「数据清理」的红色按钮，挨着更容易误触）。
        .child(Element::leaf().height(FAB_CLEARANCE));

    // 刷新做成右下角悬浮按钮而非设置区里的一行：那一行在页尾，"想刷新得先滚到底、
    // 刷完再滚回去看"，恰好和它的用途相反。悬浮层在页面任意滚动位置都可点。
    let mgr_refresh = state.stats_mgr.clone();
    let fab = Element::button("刷新")
        .small()
        .on_click(move |_| mgr_refresh.reload())
        .align(Align::End)
        // 右侧留够 14px 避开滚动条轨道，底部抬高避开设置卡片的按钮列。
        .margin_xy(14, 22);

    Element::stack().fill().child(page(body)).child(fab)
}

/// 切到统计页时懒加载。
///
/// `reactive_effect` 以构建时版本为基线、构建帧不触发，故这里额外判一次当前 tab——
/// 覆盖「`--page stats` 直接进本页」的冷启动路径（那时 tab 已是终值，不会再有版本变化）。
fn load_effect(state: &AppState) -> Element {
    let tab = state.tab;
    let target = crate::cli::page_to_index("stats");
    let mgr = state.stats_mgr.clone();
    if target == Some(tab.get()) {
        mgr.ensure_loaded();
    }
    let mgr_eff = mgr;
    reactive_effect(
        move || tab.version(),
        move || {
            if target == Some(tab.get()) {
                mgr_eff.ensure_loaded();
            }
        },
    )
}

/// 数据区（加载完成后）：数字卡片 · 输入日历 · 今日时段 · 输入详情。
fn ready_body(
    d: &StatsData,
    schema_names: &[(String, String)],
    hover: Signal<String>,
    hour_hover: Signal<String>,
) -> Element {
    let mut col = Element::col()
        .width_match()
        .spacing(18)
        .child(summary_cards(d));
    if let Some(cal) = calendar_section(d, hover) {
        col = col.child(cal);
    }
    col.child(hours_section(d, hour_hover))
        .child(detail_section(d, schema_names))
}

/// 今日时段分布卡片。今日无输入时给空态提示而非一排空柱。
fn hours_section(d: &StatsData, hover: Signal<String>) -> Element {
    let today = d.today();
    let total = today.map_or(0, |t| t.total);
    let body = if total == 0 {
        hint_row("今日暂无输入")
    } else {
        Element::col()
            .width_match()
            .spacing(6)
            .child(charts::hour_bars(
                today.map_or([0; 24], |t| t.hours),
                total,
                hover,
            ))
            .child(
                Element::label_rc(hover)
                    .font_size(12.0)
                    .fg_role(Role::TextMuted)
                    .width_match()
                    .max_lines(1)
                    .truncate(Truncate::End)
                    .height(18),
            )
    };
    section("今日时段分布", vec![body.padding_xy(0, 12)])
}

/// 输入日历卡片：热力图 + 悬停详情行。无数据时整块不渲染。
fn calendar_section(d: &StatsData, hover: Signal<String>) -> Option<Element> {
    let first = d.days.first()?;
    let first_weekday = data::weekday_mon0(data::parse_key(&first.date)?);
    let days: Vec<(String, u32)> = d.days.iter().map(|x| (x.date.clone(), x.total)).collect();

    // 详情行高度固定：未悬停时留白而非塌陷，鼠标划过日历时布局才不会上下跳。
    let detail = Element::label_rc(hover)
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(1)
        .truncate(Truncate::End)
        .height(18);

    Some(section(
        "输入日历",
        vec![
            Element::col()
                .width_match()
                .spacing(6)
                .padding_xy(0, 12)
                .child(charts::heatmap(days, first_weekday, hover))
                .child(detail),
        ],
    ))
}

/// 加载中 / 失败的居中提示。
fn hint_row(text: &str) -> Element {
    Element::label(text)
        .font_size(13.0)
        .fg_role(Role::TextMuted)
        .width_match()
        .padding_xy(0, 40)
        .text_align(Align::Center)
}

// ───────────────────────── 数字卡片 ─────────────────────────

/// 一张数字卡片：大字数值 + 标签 + 可选补充行。
///
/// 内边距与字号偏紧：四张卡片等分一行，在 560px 最小窗口下每列只剩 ~75px，
/// 再宽松一点「59.6万」这类值就会被省略号吃掉。
fn stat_card(value: String, label: &str, detail: Vec<String>) -> Element {
    let mut col = Element::col()
        .width_match()
        .cross(Align::Center)
        .spacing(3)
        .padding_xy(4, 14)
        .corner(13.0)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .child(
            Element::label(value)
                .font_size(20.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(1)
                .truncate(Truncate::End)
                .text_align(Align::Center)
                .height(28),
        )
        .child(
            Element::label(label)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(1)
                .text_align(Align::Center)
                .height(16),
        );
    for t in detail {
        col = col.child(
            Element::label(t)
                .font_size(11.0)
                .fg_role(Role::Placeholder)
                .width_match()
                .max_lines(1)
                .truncate(Truncate::End)
                .text_align(Align::Center)
                .height(15),
        );
    }
    col
}

/// 四张概览卡片：今日 / 累计 / 日均 / 连续。
fn summary_cards(d: &StatsData) -> Element {
    let s = &d.summary;
    // 今日分类拆两行（中英 / 符数）：四张卡片等分一行，一行塞四项在窄窗口下必被省略。
    // 今日明细缺失时退回 summary 的中英两项（core 的 summary 只带这两个分类）。
    let today_detail = match d.today() {
        Some(t) if t.total > 0 => vec![
            format!(
                "中{} 英{}",
                data::format_num(t.chinese as u64),
                data::format_num(t.english as u64)
            ),
            format!(
                "符{} 数{}",
                data::format_num(t.punct as u64),
                data::format_num(t.other as u64)
            ),
        ],
        _ if s.today_chinese + s.today_english > 0 => vec![format!(
            "中{} 英{}",
            data::format_num(s.today_chinese as u64),
            data::format_num(s.today_english as u64)
        )],
        _ => Vec::new(),
    };
    Element::grid(
        4,
        12,
        vec![
            stat_card(
                data::format_num(s.today_chars as u64),
                "今日输入",
                today_detail,
            ),
            stat_card(
                data::format_num(s.total_chars),
                "累计输入",
                vec![format!("{} 天", s.active_days)],
            ),
            stat_card(data::format_num(s.daily_avg as u64), "日均输入", Vec::new()),
            stat_card(
                s.streak_current.to_string(),
                "连续天数",
                vec![format!("最长 {} 天", s.streak_max)],
            ),
        ],
    )
}

// ───────────────────────── 输入详情 ─────────────────────────

/// 详情格：左标签 + 右数值，淡底圆角块。
///
/// 弹性给右侧数值而非左侧标签：窄窗口下放不下时该截断的是数值尾巴（多为日期后缀），
/// 标签被吃成「..」则整行失去意义。
fn detail_item(label: &str, value: String) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(10, 7)
        .corner(7.0)
        .bg_role(Role::SurfaceAlt)
        .child(
            Element::label(label)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .max_lines(1),
        )
        .child(
            Element::label(value)
                .font_size(12.5)
                .font_weight(600)
                .fg_role(Role::Text)
                .weight(1.0)
                .max_lines(1)
                .truncate(Truncate::End)
                .text_align(Align::End),
        )
}

/// 小标题（详情卡片内的分组标题，比 section_header 轻一档）。
fn sub_title(text: &str) -> Element {
    Element::label(text)
        .font_size(12.5)
        .font_weight(600)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(1)
        .height(18)
        .margin_xy(0, 4)
}

/// 一组占比条（标题 + 若干行）。
///
/// `min_items` 之下整组不渲染：方案占比传 2——只有一个方案时永远是 100%，
/// 一根满格的条没有任何信息量（`data::schema_bars` 已滤掉零输入的方案）。
fn bar_group(
    title: &str,
    bars: &[Bar],
    color: charts::ChartColor,
    min_items: usize,
) -> Option<Element> {
    if bars.len() < min_items.max(1) {
        return None;
    }
    let mut col = Element::col()
        .width_match()
        .spacing(4)
        .child(sub_title(title));
    for b in bars {
        col = col.child(charts::bar_row(&b.label, b.pct, color));
    }
    Some(col)
}

/// 输入详情卡片：指标格 + 字符分类 / 码长分布 / 方案占比三组占比条。
fn detail_section(d: &StatsData, schema_names: &[(String, String)]) -> Element {
    let s = &d.summary;
    let mut items = vec![
        detail_item(
            "本周",
            format!("{} 字", data::format_num(s.week_chars as u64)),
        ),
        detail_item(
            "本月",
            format!("{} 字", data::format_num(s.month_chars as u64)),
        ),
    ];
    if s.max_day_chars > 0 {
        let date = s.max_day_date.get(5..).unwrap_or(&s.max_day_date);
        items.push(detail_item(
            "最高日",
            format!("{} 字 {date}", data::format_num(s.max_day_chars as u64)),
        ));
    }
    if s.avg_code_len > 0.0 {
        items.push(detail_item("平均码长", format!("{:.2}", s.avg_code_len)));
    }
    if s.first_select_rate > 0.0 {
        items.push(detail_item(
            "首选率",
            format!("{:.1}%", s.first_select_rate * 100.0),
        ));
    }
    for (label, v) in [
        ("今日速度", s.today_speed),
        ("平均速度", s.overall_speed),
        ("历史最快", s.max_speed),
    ] {
        if v > 0 {
            items.push(detail_item(label, format!("{v} 字/分钟")));
        }
    }

    let mut col = Element::col()
        .width_match()
        .spacing(8)
        .child(Element::grid(2, 8, items));

    let name_of = |id: &str| {
        schema_names
            .iter()
            .find(|(sid, _)| sid == id)
            .map_or_else(|| id.to_string(), |(_, n)| n.clone())
    };
    for g in [
        bar_group(
            "字符分类",
            &data::char_category_bars(&d.days),
            charts::AMBER,
            1,
        ),
        bar_group("码长分布", &data::code_len_bars(&d.days), charts::GREEN, 1),
        bar_group(
            "方案占比",
            &data::schema_bars(&d.days, name_of),
            charts::INDIGO,
            2,
        ),
    ]
    .into_iter()
    .flatten()
    {
        col = col.child(g);
    }

    section("输入详情", vec![col.padding_xy(0, 10)])
}

// ───────────────────────── 统计设置 ─────────────────────────

/// 统计设置卡片：manifest 的两个开关 + 刷新 + 数据清理。
fn settings_section(state: &AppState) -> Element {
    let keys: Vec<String> = state
        .manifest
        .sections_of("stats")
        .into_iter()
        .flat_map(|(_, k)| k)
        .collect();
    let mut rows = section_rows(state, &keys);

    let mgr_prune = state.stats_mgr.clone();
    let labels: Vec<&str> = PRUNE_OPTIONS.iter().map(|(l, _)| *l).collect();
    let prune_ctl = Element::row()
        .cross(Align::Center)
        .spacing(8)
        .child(Element::dropdown(labels, state.stats_mgr.prune_idx).min_width(120))
        .child(
            Element::button("清理")
                .small()
                .danger()
                .on_click(move |_| mgr_prune.request_prune()),
        );
    rows.push(setting_row(
        "数据清理",
        Some("删除指定范围前的历史数据，不可恢复"),
        prune_ctl,
    ));
    rows.push(toast_sink(state.stats_mgr.feedback));

    section("统计设置", rows)
}

/// 清理确认对话框（挂在 shell 根层，覆盖整窗）。
pub fn build_stats_confirm_dialog(state: &StatsState) -> Element {
    let visible = state.confirm_visible;
    let s_cancel = state.clone();
    let s_close = state.clone();
    let s_ok = state.clone();

    let body = Element::col().width_match().spacing(8).child(
        Element::label_rc(state.confirm_msg)
            .font_size(13.0)
            .fg_role(Role::Text)
            .width_match()
            .max_lines(4),
    );
    let footer = Element::row()
        .width_match()
        .spacing(8)
        .child(Element::flex_spacer())
        .child(
            Element::button("取消")
                .small()
                .outline_soft()
                .on_click(move |_| s_cancel.cancel_pending()),
        )
        .child(
            Element::button("确定清理")
                .small()
                .danger()
                .on_click(move |_| s_ok.run_pending()),
        );

    Element::dialog_panel(
        visible,
        "确认清理",
        380,
        move |_: &mut EventCtx| s_close.cancel_pending(),
        body,
        footer,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_options_cover_clear_all() {
        // 「全部数据」必须是唯一的 None，否则 request_prune 会把清空误当成按天清理。
        let nones = PRUNE_OPTIONS.iter().filter(|(_, d)| d.is_none()).count();
        assert_eq!(nones, 1);
        assert_eq!(PRUNE_OPTIONS.last().unwrap().0, "全部数据");
    }

    #[test]
    fn request_prune_maps_selection_to_pending() {
        let s = StatsState::new();
        s.prune_idx.set(1); // 90 天前
        s.request_prune();
        assert_eq!(s.pending.get(), Pending::Prune(90));
        assert!(s.confirm_visible.get());
        assert!(s.confirm_msg.get().contains("90 天前"));

        s.prune_idx.set(PRUNE_OPTIONS.len() - 1); // 全部数据
        s.request_prune();
        assert_eq!(s.pending.get(), Pending::ClearAll);
        assert!(s.confirm_msg.get().contains("全部"));
    }

    fn bar(label: &str, pct: u32) -> Bar {
        Bar {
            label: label.into(),
            count: pct as u64,
            pct,
        }
    }

    /// 只剩一个方案时占比图恒为 100%，没有信息量——整组不渲染。
    /// 字符分类等单项仍要显示（它表达的是绝对构成，不是相互比较）。
    #[test]
    fn schema_group_needs_at_least_two_entries() {
        let one = vec![bar("五笔 86", 100)];
        let two = vec![bar("五笔 86", 62), bar("全拼·简", 38)];
        assert!(bar_group("方案占比", &one, charts::INDIGO, 2).is_none());
        assert!(bar_group("方案占比", &two, charts::INDIGO, 2).is_some());
        assert!(bar_group("字符分类", &one, charts::AMBER, 1).is_some());
        assert!(bar_group("字符分类", &[], charts::AMBER, 1).is_none());
    }

    #[test]
    fn cancel_clears_pending_action() {
        let s = StatsState::new();
        s.prune_idx.set(0);
        s.request_prune();
        s.cancel_pending();
        assert_eq!(s.pending.get(), Pending::Idle);
        assert!(!s.confirm_visible.get());
    }

    /// 守护：确认框取消后再点「确定」不应重放上一次的操作。
    #[test]
    fn run_pending_is_idempotent_after_cancel() {
        let s = StatsState::new();
        s.request_prune();
        s.cancel_pending();
        s.run_pending(); // pending 已清空 → 直接返回，不触碰 RPC
        assert_eq!(s.pending.get(), Pending::Idle);
        assert!(s.feedback.get().is_none(), "不应产生任何反馈");
    }
}
