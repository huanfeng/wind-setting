//! 设置向导对话框。框架照 `dialogs/update_dialog.rs`：一个面板，各段正文与按钮按
//! [`WizardPhase`] `visible_when` 切换。外框定宽定高（[`DIALOG_H`]），标题行与按钮栏
//! 固定，只有中间的正文区随内容滚动。
//!
//! 草稿在 `AppState::wizard` 会话里，控件绑的是本模块自建的信号。两者靠两个隐藏的
//! `reactive_effect` 双向同步：
//! - 控件 → 草稿（[`write_back`]）：任一控件信号变了就把控件值收进草稿，草稿真变了才 `wizard_rev += 1`；
//!   主类型变了只套预设、不收其它控件（那些控件此刻还是旧类型的值）。
//! - 草稿 → 控件（[`sync`]）：`wizard_rev` 变了就把草稿值推回控件（只推不同的，免得来回打转），
//!   并重算冲突、摘要等派生文案。打开向导、切主类型套预设，都靠这一路把新值送上界面。

use std::cell::RefCell;
use std::rc::Rc;

use windui::prelude::*;

use super::model::{self, Draft, FUZZY_PAIRS, Kind, QUICK_KEYS, TEMP_KEYS, Trigger};
use super::{
    close, complete, discard_and_continue, finish, layout_label, save_and_continue, step3_blockers,
    summary_of,
};
use crate::pages::dict::reactive::reactive_effect;
use crate::state::{AppState, WizardPhase, WizardSession};
use crate::widgets::{card, dialog_section, dialog_section_hint, setting_row};

const DIALOG_W: i32 = 600;
/// 对话框固定高度。按内容自适应时，开关双拼、临时拼音、模糊音会让外框忽大忽小、按钮栏
/// 跟着上下跳（靶机试用反馈）；改为定高后只有中间正文区伸缩，超出即滚动。
///
/// 取值：最高的常见形态——第 2 步「双拼 + 模糊音开」（页头 + 拼音卡 7 行，3 行带说明）——
/// 不滚动放得下，并留 24px 余量；测试文字引擎下正文视口 467、该页内容 436。再高的话主窗
/// 默认高 640 就显得局促，欢迎页、Dirty 页也只剩大片留白。数值以测试里的
/// `LineAwareTextEngine` 量得为准（见 `tests::tallest_steps_fit_without_scrolling`），
/// 靶机 DPI 下再目验。
const DIALOG_H: i32 = 560;
const KINDS: [Kind; 2] = [Kind::Codetable, Kind::Pinyin];
/// 第 1 步方案清单的列数与行高（行高与正常尺寸勾选框 + 「默认」徽标同档）。
const SCHEME_COLS: usize = 2;
const SCHEME_ROW_H: i32 = 30;

/// 第 1 步的一行方案勾选。
#[derive(Clone, PartialEq)]
struct SchemeRow {
    id: String,
    name: String,
    checked: bool,
    /// 默认方案：强制启用，不能取消。
    locked: bool,
}

/// 第 4 步摘要的一行（分组标题或「标签 旧 → 新」）。
#[derive(Clone, PartialEq)]
enum SumLine {
    Group(String),
    Row {
        label: String,
        old: String,
        new: String,
    },
}

/// 控件绑定的信号（全部 Copy）。
#[derive(Clone, Copy)]
struct Controls {
    kind: Signal<usize>,
    /// 各类别有没有可选的主方案（没有则卡片置灰），次序同 [`KINDS`]。
    kind_avail: [Signal<bool>; 2],
    /// 主方案下拉：当前类别的候选方案名。
    main_opts: Signal<Vec<String>>,
    main_idx: Signal<usize>,
    /// 主方案是否双拼（双拼布局行、「允许按全拼输入」随它显隐）。
    is_sp: Signal<bool>,
    layout_opts: Signal<Vec<String>>,
    layout_idx: Signal<usize>,
    /// 布局清单拿不到时只显示当前值的名字。
    layout_text: Signal<String>,
    /// 方案清单，按 [`SCHEME_COLS`] 个一行分好。
    schemes: Signal<Vec<Vec<SchemeRow>>>,
    auto_commit: Signal<bool>,
    clear_empty: Signal<bool>,
    code_hint: Signal<bool>,
    emoji: Signal<bool>,
    fuzzy: Signal<bool>,
    pairs: [Signal<bool>; 4],
    allow_full: Signal<bool>,
    quick_opts: Signal<Vec<String>>,
    quick_idx: Signal<usize>,
    slash: Signal<bool>,
    temp: Signal<bool>,
    temp_opts: Signal<Vec<String>>,
    temp_idx: Signal<usize>,
    // ── 派生文案 ──
    blockers: Signal<String>,
    summary: Signal<Vec<SumLine>>,
    /// 第 4 步导语（有无改动、几项）。
    sum_lead: Signal<String>,
    hints: Signal<String>,
    confirm_text: Signal<String>,
    result_text: Signal<String>,
}

impl Controls {
    fn new() -> Self {
        Controls {
            kind: signal(0),
            kind_avail: [signal(true), signal(true)],
            main_opts: signal(vec![]),
            main_idx: signal(0),
            is_sp: signal(false),
            layout_opts: signal(vec![]),
            layout_idx: signal(0),
            layout_text: signal(String::new()),
            schemes: signal(vec![]),
            auto_commit: signal(false),
            clear_empty: signal(false),
            code_hint: signal(false),
            emoji: signal(false),
            fuzzy: signal(false),
            pairs: [signal(false), signal(false), signal(false), signal(false)],
            allow_full: signal(false),
            quick_opts: signal(vec![]),
            quick_idx: signal(0),
            slash: signal(false),
            temp: signal(false),
            temp_opts: signal(vec![]),
            temp_idx: signal(0),
            blockers: signal(String::new()),
            summary: signal(vec![]),
            sum_lead: signal(String::new()),
            hints: signal(String::new()),
            confirm_text: signal(String::new()),
            result_text: signal(String::new()),
        }
    }

    /// 全部「用户可改」控件的组合版本号：任一变了即变。
    fn input_version(&self) -> u64 {
        let mut v = self.kind.version();
        for x in [
            self.main_idx.version(),
            self.layout_idx.version(),
            self.auto_commit.version(),
            self.clear_empty.version(),
            self.code_hint.version(),
            self.emoji.version(),
            self.fuzzy.version(),
            self.allow_full.version(),
            self.quick_idx.version(),
            self.slash.version(),
            self.temp.version(),
            self.temp_idx.version(),
        ] {
            v = v.wrapping_add(x);
        }
        for p in self.pairs {
            v = v.wrapping_add(p.version());
        }
        v
    }
}

fn set_if<T: PartialEq + Clone + 'static>(sig: Signal<T>, v: T) {
    if sig.get() != v {
        sig.set(v);
    }
}

fn kind_index(k: Kind) -> usize {
    KINDS.iter().position(|x| *x == k).unwrap_or(0)
}

/// 触发键下拉的选项：当前绑定不在常用键里时首项为「保持现状」；`allow_off` 或当前就是
/// 未绑定时末项为「不启用」。选项依据打开时的当前值（`cur`），一次会话内不变。
fn trigger_options(
    cur: &Trigger,
    keys: &[(&str, &str)],
    allow_off: bool,
) -> Vec<(Trigger, String)> {
    let mut out = Vec::new();
    if let Trigger::Keep(ks) = cur {
        let shown: Vec<&str> = ks
            .iter()
            .map(|k| crate::key_conflict::key_symbol(k))
            .collect();
        out.push((
            cur.clone(),
            format!("保持现状（当前：{}）", shown.join(" ")),
        ));
    }
    for (k, label) in keys {
        out.push((Trigger::Key((*k).to_string()), (*label).to_string()));
    }
    if allow_off || *cur == Trigger::Off {
        out.push((Trigger::Off, "不启用".to_string()));
    }
    out
}

fn quick_options(s: &WizardSession) -> Vec<(Trigger, String)> {
    trigger_options(&s.cur.quick, QUICK_KEYS, true)
}

fn temp_options(s: &WizardSession) -> Vec<(Trigger, String)> {
    trigger_options(&s.cur.temp_key, TEMP_KEYS, false)
}

fn index_of(opts: &[(Trigger, String)], t: &Trigger) -> usize {
    opts.iter().position(|(o, _)| o == t).unwrap_or(0)
}

/// 布局下拉的选项：`schema.layouts` 清单；当前布局不在清单里时补在末尾（显示 id），
/// 否则下拉选不中它，任何一次回写都会把布局悄悄改成清单里的某一项。清单为空时也为空
/// （界面改显示当前值的名字）。
fn layout_choices(s: &WizardSession) -> Vec<(String, String)> {
    let mut v = s.layouts.clone();
    if v.is_empty() {
        return v;
    }
    // 草稿当前值与主方案打开时的值都要选得中：后者是「改回原样」的那一项。
    let d = &s.draft;
    for l in [d.layout.clone(), d.catalog.layout_of(&d.active)] {
        if !l.is_empty() && !v.iter().any(|(id, _)| *id == l) {
            v.push((l.clone(), l));
        }
    }
    v
}

/// 主方案下拉的选项 (id, 名称)：当前类别的候选；草稿主方案不在其中（如打开时的默认方案
/// 是特殊方案或不在清单里）时补在末尾，否则下拉选不中它，一次回写就悄悄换掉了。
fn main_choices(st: &AppState, d: &Draft) -> Vec<(String, String)> {
    let name = |id: &str| {
        st.schemas
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| id.to_string())
    };
    let mut ids = d.catalog.candidates(d.kind);
    if !ids.contains(&d.active) {
        ids.push(d.active.clone());
    }
    ids.into_iter()
        .map(|id| {
            let n = name(&id);
            (id, n)
        })
        .collect()
}

/// 方案勾选清单：非隐藏方案 + `english`/`stroke`（规格要求列出、不预勾）+ 已启用但不在
/// `schema.list` 里的自定义 id。
fn scheme_rows(st: &AppState, d: &Draft) -> Vec<SchemeRow> {
    let mut rows: Vec<SchemeRow> = st
        .schemas
        .iter()
        .filter(|s| !s.hidden || s.id == "english" || s.id == "stroke")
        .map(|s| SchemeRow {
            id: s.id.clone(),
            name: s.name.clone(),
            checked: false,
            locked: false,
        })
        .collect();
    for id in &d.enabled {
        if !st.schemas.iter().any(|s| &s.id == id) {
            rows.push(SchemeRow {
                id: id.clone(),
                name: id.clone(),
                checked: false,
                locked: false,
            });
        }
    }
    for r in rows.iter_mut() {
        r.locked = r.id == d.active;
        r.checked = r.locked || d.enabled.contains(&r.id);
    }
    rows
}

/// 草稿 → 控件 + 派生文案。
fn sync(app: &Rc<RefCell<AppState>>, c: &Controls) {
    let st = app.borrow();
    let s = st.wizard.borrow();
    let Some(s) = s.as_ref() else { return };
    let d = &s.draft;

    set_if(c.kind, kind_index(d.kind));
    for (i, k) in KINDS.iter().enumerate() {
        set_if(c.kind_avail[i], !d.catalog.candidates(*k).is_empty());
    }
    let mains = main_choices(&st, d);
    set_if(c.main_opts, mains.iter().map(|(_, n)| n.clone()).collect());
    if let Some(i) = mains.iter().position(|(id, _)| *id == d.active) {
        set_if(c.main_idx, i);
    }
    set_if(c.is_sp, d.is_shuangpin());
    let layouts = layout_choices(s);
    set_if(
        c.layout_opts,
        layouts.iter().map(|(_, n)| n.clone()).collect(),
    );
    if let Some(i) = layouts.iter().position(|(id, _)| *id == d.layout) {
        set_if(c.layout_idx, i);
    }
    set_if(c.layout_text, layout_label(&s.layouts, &d.layout));
    set_if(
        c.schemes,
        scheme_rows(&st, d)
            .chunks(SCHEME_COLS)
            .map(<[SchemeRow]>::to_vec)
            .collect(),
    );
    set_if(c.auto_commit, d.auto_commit_at_full);
    set_if(c.clear_empty, d.clear_on_empty_max);
    set_if(c.code_hint, d.code_hint);
    set_if(c.emoji, d.emoji);
    set_if(c.fuzzy, d.fuzzy);
    for (i, p) in c.pairs.iter().enumerate() {
        set_if(*p, d.fuzzy_pairs[i]);
    }
    set_if(c.allow_full, d.allow_full_pinyin);
    let q = quick_options(s);
    set_if(c.quick_opts, q.iter().map(|(_, l)| l.clone()).collect());
    set_if(c.quick_idx, index_of(&q, &d.quick));
    set_if(c.slash, d.slash_dun);
    set_if(c.temp, d.temp_pinyin);
    let t = temp_options(s);
    set_if(c.temp_opts, t.iter().map(|(_, l)| l.clone()).collect());
    set_if(c.temp_idx, index_of(&t, &d.temp_key));

    set_if(c.blockers, step3_blockers(&st, d).join("\n"));

    let changes = summary_of(&st, s);
    let mut lines = Vec::new();
    let mut group = "";
    for ch in &changes {
        if ch.group != group {
            group = ch.group;
            lines.push(SumLine::Group(group.to_string()));
        }
        lines.push(SumLine::Row {
            label: ch.label.clone(),
            old: ch.old.clone(),
            new: ch.new.clone(),
        });
    }
    set_if(c.summary, lines);
    set_if(c.hints, hints_text(&st, d));
    set_if(
        c.sum_lead,
        if changes.is_empty() {
            "当前设置已符合你的选择，无需修改。".to_string()
        } else {
            format!("以下 {} 项设置将被修改，确认后立即生效。", changes.len())
        },
    );
    set_if(
        c.confirm_text,
        format!("将覆盖 {} 项现有设置，确定应用吗？", changes.len()),
    );
    set_if(c.result_text, result_text(s));
}

/// 目标默认方案有 override 的那些「将写入的键」，汇总成一句提示。
fn hints_text(st: &AppState, d: &Draft) -> String {
    let items = d.items(&st.base_config, &st.capabilities.map_keys());
    let keys = model::override_hints(&items, &d.active, &st.override_summary.get());
    if keys.is_empty() {
        return String::new();
    }
    let labels: Vec<String> = keys
        .iter()
        .map(|k| {
            st.manifest
                .get(k)
                .map(|e| e.label.clone())
                .unwrap_or_else(|| k.clone())
        })
        .collect();
    let name = st
        .schemas
        .iter()
        .find(|s| s.id == d.active)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| d.active.clone());
    format!(
        "方案「{name}」有独立设置，以下修改对它不生效，可在方案设置里调整：{}",
        labels.join("、")
    )
}

fn result_text(s: &WizardSession) -> String {
    match &s.outcome {
        Some(super::apply::ApplyOutcome::Failed { step, error, done }) => {
            let done = if done.is_empty() {
                "无（没有改动任何配置）".to_string()
            } else {
                done.join("、")
            };
            format!("「{step}」失败：{error}\n已完成的步骤：{done}")
        }
        _ => String::new(),
    }
}

/// 控件 → 草稿。
fn write_back(app: &Rc<RefCell<AppState>>, c: &Controls) {
    let st = app.borrow();
    let changed = {
        let mut s = st.wizard.borrow_mut();
        let Some(s) = s.as_mut() else { return };
        let kind = KINDS[c.kind.get().min(KINDS.len() - 1)];
        // 控制方裁定 #1：只在主类型真正改变时套预设——点了同一个主类型不能重套，
        // 否则把自定义码表默认方案的用户改成 wubi86。
        let main = main_choices(&st, &s.draft)
            .get(c.main_idx.get())
            .map(|(id, _)| id.clone());
        let next = if kind != s.draft.kind {
            s.draft.switch_kind(&s.cur, kind)
        } else if let Some(id) = main.filter(|id| *id != s.draft.active) {
            // 换主方案同样只做这一件事：布局控件此刻还是旧主方案的值，不能收。
            s.draft.set_main(&s.cur, &id)
        } else {
            let mut d = s.draft.clone();
            if let Some((id, _)) = layout_choices(s).get(c.layout_idx.get()) {
                d.layout = id.clone();
            }
            d.auto_commit_at_full = c.auto_commit.get();
            d.clear_on_empty_max = c.clear_empty.get();
            d.code_hint = c.code_hint.get();
            d.emoji = c.emoji.get();
            d.fuzzy = c.fuzzy.get();
            for (i, p) in c.pairs.iter().enumerate() {
                d.fuzzy_pairs[i] = p.get();
            }
            d.allow_full_pinyin = c.allow_full.get();
            if let Some((t, _)) = quick_options(s).get(c.quick_idx.get()) {
                d.quick = t.clone();
            }
            d.slash_dun = c.slash.get();
            d.temp_pinyin = c.temp.get();
            if let Some((t, _)) = temp_options(s).get(c.temp_idx.get()) {
                d.temp_key = t.clone();
            }
            d
        };
        let changed = next != s.draft;
        s.draft = next;
        changed
    };
    if changed {
        st.wizard_rev.set(st.wizard_rev.get().wrapping_add(1));
    }
}

/// 勾/取消一个方案（默认方案锁定，不响应）。
fn toggle_scheme(app: &Rc<RefCell<AppState>>, id: &str) {
    let st = app.borrow();
    {
        let mut s = st.wizard.borrow_mut();
        let Some(s) = s.as_mut() else { return };
        if s.draft.active == id {
            return;
        }
        if let Some(i) = s.draft.enabled.iter().position(|x| x == id) {
            s.draft.enabled.remove(i);
        } else {
            s.draft.enabled.push(id.to_string());
        }
    }
    st.wizard_rev.set(st.wizard_rev.get().wrapping_add(1));
}

pub fn build_wizard_dialog(app: &Rc<RefCell<AppState>>) -> Element {
    let (visible, phase) = {
        let st = app.borrow();
        (st.wizard_visible, st.wizard_phase)
    };
    let c = Controls::new();
    // 重建外壳（主窗关掉再开）时会话可能还在：先同步一次。
    sync(app, &c);

    let app_w = app.clone();
    let writer = reactive_effect(move || c.input_version(), move || write_back(&app_w, &c));
    let app_s = app.clone();
    let rev = app.borrow().wizard_rev;
    let syncer = reactive_effect(move || rev.version(), move || sync(&app_s, &c));
    // ESC 关对话框只置 `wizard_visible = false`，不经 `close`：补走一次关闭语义
    // （按勾选落 `wizard_dismissed`、清会话）。
    let app_e = app.clone();
    let esc_watch = reactive_effect(
        move || visible.version(),
        move || {
            let open = app_e.borrow().wizard.borrow().is_some();
            if !visible.get() && open {
                close(&app_e);
            }
        },
    );

    let frame = frame(app, phase, c)
        // 次序即同帧执行序：先收控件、再推草稿，套预设的那一帧就能把新值送上界面。
        .child(writer)
        .child(syncer)
        .child(esc_watch);

    let app_x = app.clone();
    Element::dialog_closable(visible, frame, move |_| close(&app_x))
}

/// 对话框面板：标题行 → 正文区（`weight(1.0)`，吃掉定高减去两头后的全部高度）→ 按钮栏。
///
/// 正文区是一个 stack，每段正文各包一个滚动容器、按阶段显隐（同 `shell.rs` 的
/// `pages_stack`）。不共用一个滚动容器：windui 的滚动位置没有程序化复位，共用的话
/// 在第 2 步滚到底再点「下一步」，第 3 步会从半截开始显示。
fn frame(app: &Rc<RefCell<AppState>>, phase: Signal<WizardPhase>, c: Controls) -> Element {
    let is = move |p: WizardPhase| move || phase.get() == p;
    let page = move |body: Element, p: WizardPhase| {
        Element::scroll().fill().child(body).visible_when(is(p))
    };
    let pages = Element::stack()
        .fill()
        .weight(1.0)
        .child(page(welcome_body(), WizardPhase::Welcome))
        .child(page(dirty_body(), WizardPhase::Dirty))
        .child(page(step1_body(app, c), WizardPhase::Step(1)))
        .child(page(step2_body(c), WizardPhase::Step(2)))
        .child(page(step3_body(c), WizardPhase::Step(3)))
        .child(page(step4_body(c), WizardPhase::Step(4)))
        .child(page(confirm_body(c.confirm_text), WizardPhase::Confirm))
        .child(page(result_body(c.result_text), WizardPhase::Result));
    Element::col()
        .width(DIALOG_W)
        .height(DIALOG_H)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(title_row(phase))
        .child(Element::divider())
        .child(pages)
        .child(Element::divider())
        .child(footer(app, phase, c))
}

/// 「上一步」/「返回」的去处：第 1 步回欢迎页，第 2–4 步回上一步，确认段回第 4 步。
fn back_of(p: WizardPhase) -> Option<WizardPhase> {
    match p {
        WizardPhase::Step(1) => Some(WizardPhase::Welcome),
        WizardPhase::Step(n) if (2..=4).contains(&n) => Some(WizardPhase::Step(n - 1)),
        WizardPhase::Confirm => Some(WizardPhase::Step(4)),
        _ => None,
    }
}

/// 「开始」/「下一步」的去处：欢迎页进第 1 步，第 1–3 步进下一步。
fn forward_of(p: WizardPhase) -> Option<WizardPhase> {
    match p {
        WizardPhase::Welcome => Some(WizardPhase::Step(1)),
        WizardPhase::Step(n) if (1..=3).contains(&n) => Some(WizardPhase::Step(n + 1)),
        _ => None,
    }
}

/// 卡片里「跟在 `base` 后面、条件成立才出现」的行：分隔线与行一起挂 `when`，再和 `base`
/// 并成卡片 `rows` 里的**一个**元素。
///
/// 直接把条件行塞进 `rows` 的话，[`crate::widgets::card`] 按「不是最后一个就加分隔线」
/// 给它前面插的那条线是常显的：条件行一隐藏，可见的最后一行下面就多出一条线（条件行
/// 连着两个时还会叠成两条）。条件可嵌套：`base` 自己也可以是 `with_cond` 的结果。
fn with_cond(base: Element, rows: Vec<Element>, when: impl Fn() -> bool + 'static) -> Element {
    let mut tail = Element::col().width_match();
    for r in rows {
        tail = tail.child(Element::divider()).child(r);
    }
    Element::col()
        .width_match()
        .child(base)
        .child(tail.visible_when(when))
}

fn title_row(phase: Signal<WizardPhase>) -> Element {
    let step = phase.map(|p| match p {
        WizardPhase::Step(n) => format!("{n} / 4"),
        WizardPhase::Confirm => "确认".to_string(),
        WizardPhase::Welcome | WizardPhase::Dirty | WizardPhase::Result => String::new(),
    });
    Element::row()
        .width_match()
        .cross(Align::Center)
        // 右侧让出对话框自带的关闭按钮（24px + 外边距），步骤指示才不会压在 × 上。
        .padding_edges(Insets::new(18, 14, 52, 14))
        .child(
            Element::label("设置向导")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .weight(1.0)
                .max_lines(1),
        )
        .child(
            Element::label_signal(step)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .max_lines(1),
        )
}

fn body() -> Element {
    Element::col().width_match().spacing(10).padding_xy(18, 14)
}

/// 页头：大标题 + 一行灰色导语，说清这一页要你做什么。各页正文都以它打头。
fn page_head(title: &str, lead: impl Into<TextContent>) -> Element {
    Element::col()
        .width_match()
        .spacing(4)
        .padding_edges(Insets::new(0, 0, 0, 4))
        .child(
            Element::label(title)
                .font_size(17.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(1),
        )
        .child(
            Element::label(lead)
                .font_size(13.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(2),
        )
}

/// 设置行：标签 + 说明 + 右侧控件。说明允许两行，其余照 [`setting_row`]。
///
/// 不直接用 `setting_row`：它的说明按本仓 `FormTheme` 限一行，向导的说明是写给新用户的
/// 白话（「做什么 / 什么时候有用」），右侧再放一个下拉就放不下一行。写法照
/// [`crate::widgets::setting_row_checks`]（同样是因为限行另行拼装）。
fn item_row(label: &str, desc: &str, control: Element) -> Element {
    let th = windui::theme::current();
    let (f, m) = (&th.form, &th.metrics);
    let left = Element::col()
        .weight(1.0)
        .spacing(2)
        .child(
            Element::label(label)
                .font_size(f.label_size(m))
                .font_weight(f.label_weight())
                .fg_role(Role::Text)
                .width_match()
                .max_lines(1)
                .truncate(Truncate::End),
        )
        .child(
            Element::label(desc)
                .font_size(f.desc_size(m))
                .fg_role(Role::TextSubtle)
                .width_match()
                .max_lines(2)
                .truncate(Truncate::End)
                .tooltip(desc),
        );
    Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(0, f.row_pad_y())
        .spacing(f.gap())
        .child(left)
        .child(control)
}

/// 欢迎页的一行链接：标题 + 说明 + 右侧可点的地址。
fn link_row(label: &str, sub: &str, text: &str, url: &'static str) -> Element {
    item_row(
        label,
        sub,
        Element::link(text)
            .font_size(12.5)
            .on_click(move |ctx| ctx.open_url(url)),
    )
}

/// 欢迎页的一条要点：圆形序号 + 一行短句。
fn point(n: usize, text: &str) -> Element {
    let num = Element::stack()
        .size(20, 20)
        .corner(10.0)
        .bg_role_alpha(Role::Accent, 0.12)
        .child(
            Element::label(n.to_string())
                .font_size(11.5)
                .font_weight(600)
                .fg_role(Role::Accent)
                .align(Align::Center),
        );
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(10)
        .child(num)
        .child(
            Element::label(text)
                .font_size(13.0)
                .fg_role(Role::Text)
                .weight(1.0)
                .max_lines(1),
        )
}

fn welcome_body() -> Element {
    use crate::pages::{DOCS_URL, MARKET_URL, THEME_EDITOR_URL};
    body()
        .child(page_head(
            "欢迎使用清风输入法",
            "花一分钟，按你的打字习惯配好最常用的设置。",
        ))
        .child(
            Element::col()
                .width_match()
                .spacing(8)
                .padding_xy(2, 0)
                .child(point(1, "选择常用的输入方式：五笔、全拼或双拼"))
                .child(point(2, "调整几项最常用的输入习惯与功能"))
                .child(point(3, "最后一步确认后才会生效，中途随时可以关闭")),
        )
        .child(dialog_section(
            "了解更多",
            vec![
                link_row(
                    "使用文档",
                    "每项功能、每个设置的详细说明",
                    "windinput.com/docs",
                    DOCS_URL,
                ),
                link_row(
                    "资源市场",
                    "主题皮肤、输入方案等资源，下载即用",
                    "market.windinput.com",
                    MARKET_URL,
                ),
                link_row(
                    "主题编辑器",
                    "在线设计属于自己的皮肤",
                    "theme.windinput.com",
                    THEME_EDITOR_URL,
                ),
            ],
        ))
        .child(
            Element::label("以后可随时从左侧导航最下方的「设置向导」再次打开。")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(1),
        )
}

fn dirty_body() -> Element {
    body().child(page_head(
        "设置页有未保存的修改",
        "向导写入后会重新读取配置，未保存的修改会和它互相覆盖。请先保存或放弃这些修改，再继续。",
    ))
}

/// 第 1 步的一张类别选项卡片：整卡可点，选中时强调色描边 + 淡底；该类别没有可选的
/// 主方案时置灰。只有名称——类别名本身就够直白（维护者意见）。
///
/// 选中态叠层的写法照方案管理页的方案行（`dialogs/schema_manager.rs`）：描边与底色是
/// 垫在内容下面、按选中显隐的叶子，内容层 `clickable` 吞命中。
fn kind_card(kind: Signal<usize>, avail: Signal<bool>, i: usize, title: &str) -> Element {
    let sel = move || kind.get() == i;
    Element::stack()
        .enabled_when(move || avail.get())
        .child(
            Element::leaf()
                .align(Align::Stretch)
                .corner(8.0)
                .border_role(Role::Border, 1)
                .visible_when(move || !sel()),
        )
        .child(
            Element::leaf()
                .align(Align::Stretch)
                .corner(8.0)
                .bg_role_alpha(Role::Accent, 0.08)
                .border_role(Role::Accent, 2)
                .visible_when(sel),
        )
        .child(
            Element::col()
                .align(Align::Stretch)
                .clickable()
                .on_click(move |_| kind.set(i))
                .corner(8.0)
                .padding_xy(14, 12)
                .child(
                    Element::label(title)
                        .font_size(14.0)
                        .font_weight(600)
                        .fg_role(Role::Text)
                        .width_match()
                        .max_lines(1),
                ),
        )
}

/// 方案清单的一格：正常尺寸的勾选框，默认方案带「默认」徽标。
fn scheme_cell(app: &Rc<RefCell<AppState>>, row: SchemeRow) -> Element {
    let th = windui::theme::current();
    let app = app.clone();
    let id = row.id.clone();
    let mut cell = Element::row().cross(Align::Center).spacing(6).child(
        Element::checkbox(row.name.clone(), signal(row.checked))
            .font_size(th.form.label_size(&th.metrics))
            .fg_role(Role::Text)
            .enabled(!row.locked)
            // 接管点击：勾选态由草稿决定，改完草稿整张清单随 `sync` 重建。
            .on_click(move |_| toggle_scheme(&app, &id)),
    );
    if row.locked {
        cell = cell.child(Element::badge("默认"));
    }
    cell
}

/// 方案清单：两列网格。`c.schemes` 已按两个一行分好，一行一个子树。
fn scheme_grid(app: &Rc<RefCell<AppState>>, schemes: Signal<Vec<Vec<SchemeRow>>>) -> Element {
    let app = app.clone();
    let rows = Element::host_signal(schemes, move |pair: Vec<SchemeRow>| {
        let mut r = Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(12)
            .height(SCHEME_ROW_H);
        for i in 0..SCHEME_COLS {
            r = r.child(match pair.get(i) {
                Some(row) => scheme_cell(&app, row.clone()).weight(1.0),
                None => Element::leaf().weight(1.0),
            });
        }
        r
    });
    Element::col().width_match().padding_xy(0, 8).child(rows)
}

fn step1_body(app: &Rc<RefCell<AppState>>, c: Controls) -> Element {
    let kind = c.kind;
    let is_sp = c.is_sp;
    let cards = Element::grid(
        KINDS.len(),
        10,
        vec![
            kind_card(kind, c.kind_avail[0], 0, "五笔 / 码表"),
            kind_card(kind, c.kind_avail[1], 1, "拼音"),
        ],
    );
    let main_row = setting_row(
        "主方案",
        None,
        Element::dropdown_signal(c.main_opts, c.main_idx).width(180),
    );
    let layouts = c.layout_opts;
    let layout_row = setting_row(
        "双拼布局",
        None,
        Element::stack()
            .child(
                Element::dropdown_signal(layouts, c.layout_idx)
                    .width(180)
                    .visible_when(move || !layouts.get().is_empty()),
            )
            .child(
                Element::label_signal(c.layout_text)
                    .font_size(12.5)
                    .fg_role(Role::Text)
                    .visible_when(move || layouts.get().is_empty()),
            ),
    );
    body()
        .child(page_head(
            "选择输入方式",
            "你平时主要用哪类输入方式？再选一个主方案作为默认。",
        ))
        .child(cards)
        .child(card(vec![with_cond(
            main_row,
            vec![layout_row],
            move || is_sp.get(),
        )]))
        .child(dialog_section_hint(
            "同时启用的方案",
            "勾选的方案可在打字时切换使用；主方案始终启用",
            vec![scheme_grid(app, c.schemes)],
        ))
}

fn switch_row(label: &str, sub: &str, sig: Signal<bool>) -> Element {
    item_row(label, sub, Element::switch(sig))
}

/// 模糊音分项相对总开关的缩进。
const FUZZY_INDENT: i32 = 24;

fn step2_body(c: Controls) -> Element {
    let kind = c.kind;
    let is_sp = c.is_sp;
    let fuzzy = c.fuzzy;
    // 模糊音分项：与本对话框其余行同一种行型（标题 + 右侧开关），缩进挂在总开关下面，
    // 读作它的子项。
    let pairs: Vec<Element> = FUZZY_PAIRS
        .iter()
        .enumerate()
        .map(|(i, (_, label))| {
            Element::col()
                .width_match()
                .padding_edges(Insets::new(FUZZY_INDENT, 0, 0, 0))
                .child(setting_row(label, None, Element::switch(c.pairs[i])))
        })
        .collect();
    let lead = kind.map(|k| {
        if *k == 0 {
            "几项常用的码表习惯，按你的偏好开关。"
        } else {
            "几项常用的拼音习惯，按你的偏好开关。"
        }
        .to_string()
    });
    let codetable = dialog_section(
        "五笔 / 码表",
        vec![
            switch_row(
                "四码唯一时自动上屏",
                "编码打满且只有一个候选时直接上屏，不用再按空格",
                c.auto_commit,
            ),
            switch_row(
                "空码时自动清空",
                "编码打满却没有候选时，自动清掉已打的编码",
                c.clear_empty,
            ),
            switch_row(
                "拼音反查",
                "打拼音时，在候选旁显示对应的五笔编码，方便学习和查码",
                c.code_hint,
            ),
        ],
    )
    .visible_when(move || kind.get() == 0);
    let pinyin = dialog_section(
        "拼音",
        vec![
            switch_row(
                "Emoji 候选",
                "打出词语后附带对应的 emoji，如「开心」→ 😄",
                c.emoji,
            ),
            with_cond(
                with_cond(
                    switch_row(
                        "模糊音",
                        "分不清平翘舌、前后鼻音时开启，下方选择要互通的音",
                        c.fuzzy,
                    ),
                    pairs,
                    move || fuzzy.get(),
                ),
                vec![switch_row(
                    "允许按全拼输入",
                    "双拼方案下也能直接打全拼，如 zaijian → 再见",
                    c.allow_full,
                )],
                move || is_sp.get(),
            ),
        ],
    )
    .visible_when(move || kind.get() != 0);
    body()
        .child(page_head("输入习惯", lead))
        .child(codetable)
        .child(pinyin)
}

fn step3_body(c: Controls) -> Element {
    let kind = c.kind;
    let temp = c.temp;
    let blockers = c.blockers;
    body()
        .child(page_head("常用功能", "几个高频小功能，按一个键就能用。"))
        .child(dialog_section(
            "通用",
            vec![
                item_row(
                    "快捷输入",
                    "按触发键进入，可输入日期、数学计算、数字金额（大写）等；\
                     还可在设置中加入拼音、英文、生僻字等候选来源。",
                    Element::dropdown_signal(c.quick_opts, c.quick_idx).width(TRIGGER_W),
                ),
                with_cond(
                    switch_row(
                        "/ 键输出顿号",
                        "中文标点下按 / 直接得到「、」，英文标点下不受影响",
                        c.slash,
                    ),
                    vec![with_cond(
                        switch_row(
                            "临时拼音",
                            "遇到不会拆的字，按触发键临时用拼音打，候选旁会显示它的五笔编码",
                            c.temp,
                        ),
                        vec![item_row(
                            "临时拼音触发键",
                            "按下后进入临时拼音",
                            Element::dropdown_signal(c.temp_opts, c.temp_idx).width(TRIGGER_W),
                        )],
                        move || temp.get(),
                    )],
                    move || kind.get() == 0,
                ),
            ],
        ))
        .child(
            Element::label_signal(blockers)
                .font_size(12.0)
                .fg_role(Role::Danger)
                .width_match()
                .max_lines(6)
                .visible_when(move || !blockers.get().is_empty()),
        )
}

/// 触发键下拉宽度：容得下「右方括号 ( ] )」与「保持现状（当前：…）」。
const TRIGGER_W: i32 = 170;

fn step4_body(c: Controls) -> Element {
    let summary = c.summary;
    let hints = c.hints;
    let list = Element::host_signal(summary, |line: SumLine| match line {
        SumLine::Group(g) => crate::manifest::dialog_group_header(&g),
        SumLine::Row { label, old, new } => Element::row()
            .width_match()
            .cross(Align::Start)
            .spacing(10)
            .padding_xy(4, 4)
            .child(
                Element::label(label)
                    .font_size(13.0)
                    .fg_role(Role::Text)
                    .width(170)
                    .max_lines(1)
                    .truncate(Truncate::End),
            )
            .child(
                Element::row()
                    .weight(1.0)
                    .spacing(8)
                    .child(
                        Element::label(old)
                            .font_size(13.0)
                            .fg_role(Role::TextMuted)
                            .max_lines(2),
                    )
                    .child(Element::label("→").font_size(13.0).fg_role(Role::TextMuted))
                    .child(
                        Element::label(new)
                            .font_size(13.0)
                            .font_weight(600)
                            .fg_role(Role::Accent)
                            .weight(1.0)
                            .max_lines(2),
                    ),
            ),
    });
    body()
        .child(page_head("确认修改", c.sum_lead))
        // 不自带定高滚动：摘要长了由整段正文区滚动。
        .child(
            Element::col()
                .width_match()
                .child(list)
                .visible_when(move || !summary.get().is_empty()),
        )
        .child(
            Element::label_signal(hints)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(4)
                .visible_when(move || !hints.get().is_empty()),
        )
}

fn confirm_body(text: Signal<String>) -> Element {
    body().child(
        Element::label_signal(text)
            .font_size(15.0)
            .font_weight(600)
            .fg_role(Role::Text)
            .width_match()
            .max_lines(2),
    )
}

fn result_body(text: Signal<String>) -> Element {
    body()
        .child(
            Element::label("设置未能全部应用")
                .font_size(17.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(1),
        )
        .child(
            Element::label_signal(text)
                .font_size(12.5)
                .fg_role(Role::Danger)
                .width_match()
                .max_lines(6),
        )
}

fn footer(app: &Rc<RefCell<AppState>>, phase: Signal<WizardPhase>, c: Controls) -> Element {
    let is = move |p: WizardPhase| move || phase.get() == p;
    let dont = app.borrow().wizard_dont_show;
    let blockers = c.blockers;
    let step = move || match phase.get() {
        WizardPhase::Step(n) => n,
        _ => 0,
    };
    let go = move |f: fn(WizardPhase) -> Option<WizardPhase>| {
        move |_: &mut windui::core::EventCtx| {
            if let Some(p) = f(phase.get()) {
                phase.set(p);
            }
        }
    };

    let save_app = app.clone();
    let discard_app = app.clone();
    let cancel_app = app.clone();
    let done_app = app.clone();
    let ok_app = app.clone();
    let close_app = app.clone();

    Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 12)
        .spacing(8)
        .child(
            Element::checkbox("下次不再弹出", dont)
                .font_size(12.0)
                .visible_when(move || phase.get() != WizardPhase::Result),
        )
        .child(Element::leaf().weight(1.0))
        // Dirty
        .child(
            Element::button("取消")
                .small()
                .outline_soft()
                .on_click(move |_| close(&cancel_app))
                .visible_when(is(WizardPhase::Dirty)),
        )
        .child(
            Element::button("放弃修改")
                .small()
                .outline_soft()
                .on_click(move |_| discard_and_continue(&discard_app))
                .visible_when(is(WizardPhase::Dirty)),
        )
        .child(
            Element::button("保存并继续")
                .small()
                .on_click(move |_| save_and_continue(&save_app))
                .visible_when(is(WizardPhase::Dirty)),
        )
        // 欢迎页
        .child(
            Element::button("开始")
                .small()
                .on_click(go(forward_of))
                .visible_when(is(WizardPhase::Welcome)),
        )
        // 1–4 步
        .child(
            Element::button("上一步")
                .small()
                .outline_soft()
                .on_click(go(back_of))
                .visible_when(move || (1..=4).contains(&step())),
        )
        .child(
            Element::button("下一步")
                .small()
                .on_click(go(forward_of))
                .enabled_when(move || step() != 3 || blockers.get().is_empty())
                .visible_when(move || (1..=3).contains(&step())),
        )
        .child(
            Element::button("完成")
                .small()
                .on_click(move |_| complete(&done_app))
                .visible_when(is(WizardPhase::Step(4))),
        )
        // Confirm
        .child(
            Element::button("返回")
                .small()
                .outline_soft()
                .on_click(go(back_of))
                .visible_when(is(WizardPhase::Confirm)),
        )
        .child(
            Element::button("确定")
                .small()
                .on_click(move |_| finish(&ok_app))
                .visible_when(is(WizardPhase::Confirm)),
        )
        // Result
        .child(
            Element::button("关闭")
                .small()
                .on_click(move |_| close(&close_app))
                .visible_when(is(WizardPhase::Result)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wizard::open_wizard;
    use crate::wizard::tests::{mock_app, with_mock};

    fn draft_of(app: &Rc<RefCell<AppState>>) -> Draft {
        app.borrow().wizard.borrow().as_ref().unwrap().draft.clone()
    }

    /// 控制方裁定 #1：点了与当前相同的主类型不重套预设——自定义码表默认方案要原样保留。
    #[test]
    fn same_kind_does_not_reapply_preset() {
        with_mock(|| {
            let app = mock_app();
            open_wizard(&app);
            {
                let st = app.borrow();
                let mut s = st.wizard.borrow_mut();
                let s = s.as_mut().unwrap();
                s.draft.active = "my_codetable".into();
                s.draft.auto_commit_at_full = false;
            }
            let c = Controls::new();
            sync(&app, &c);
            // 选中「五笔 / 码表」（与当前相同），再动一个无关开关触发回写。
            c.kind.set(0);
            c.clear_empty.set(!c.clear_empty.get());
            write_back(&app, &c);
            let d = draft_of(&app);
            assert_eq!(d.active, "my_codetable", "同主类型不该换默认方案");
            assert!(!d.auto_commit_at_full, "同主类型不该重套预设");
        });
    }

    /// 切主类型：草稿套预设，`sync` 把预设值送上第 2、3 步的控件。
    #[test]
    fn kind_change_applies_preset_and_syncs_controls() {
        with_mock(|| {
            let app = mock_app();
            open_wizard(&app);
            let c = Controls::new();
            sync(&app, &c);
            let rev0 = app.borrow().wizard_rev.get();
            c.kind.set(1);
            write_back(&app, &c);
            let d = draft_of(&app);
            assert_eq!(d.kind, Kind::Pinyin);
            // mock 没有 id 为 pinyin 的方案：取拼音类候选第一个
            assert_eq!(d.active, "pinyin_simp");
            assert!(app.borrow().wizard_rev.get() != rev0, "草稿变了要推 rev");
            sync(&app, &c);
            assert!(c.emoji.get(), "拼音预设 emoji 开");
            assert!(!c.fuzzy.get());
            // 控件与草稿一致后再回写一次，草稿不变。
            write_back(&app, &c);
            assert_eq!(draft_of(&app), d);
        });
    }

    /// 主方案下拉随类别刷新；选中双拼主方案（mock 的 double_pinyin，按 scheme 认）后
    /// 双拼布局行出现，布局取该方案打开时的布局。
    #[test]
    fn main_dropdown_follows_kind_and_shuangpin_shows_layout() {
        with_mock(|| {
            let app = mock_app();
            open_wizard(&app);
            let c = Controls::new();
            sync(&app, &c);
            assert_eq!(
                c.main_opts.get(),
                vec!["五笔 86", "五笔 98", "五笔拼音混输"]
            );
            assert!(!c.is_sp.get());
            c.kind.set(1);
            write_back(&app, &c);
            sync(&app, &c);
            assert_eq!(c.main_opts.get(), vec!["全拼·简", "双拼"]);
            assert_eq!(c.main_idx.get(), 0);
            assert!(!c.is_sp.get());
            c.main_idx.set(1);
            write_back(&app, &c);
            sync(&app, &c);
            let d = draft_of(&app);
            assert_eq!(d.active, "double_pinyin");
            assert!(d.enabled.contains(&"double_pinyin".to_string()));
            let cur_enabled = app
                .borrow()
                .wizard
                .borrow()
                .as_ref()
                .unwrap()
                .cur
                .enabled
                .clone();
            assert_eq!(
                d.enabled.contains(&"pinyin_simp".to_string()),
                cur_enabled.contains(&"pinyin_simp".to_string()),
                "旧主方案：原本启用的保留、向导自动加的去掉"
            );
            assert!(c.is_sp.get(), "双拼主方案显示布局行");
            assert_eq!(d.layout, "ziranma", "mock 里 double_pinyin 的布局");
            assert!(!crate::wizard::model::layout_changed(&d));
        });
    }

    /// 第 1 步切走再切回原主类型：默认方案与启用表回到打开时的样子，而不是预设
    /// （自定义码表用户不能被改成 wubi86）。
    #[test]
    fn switching_back_to_original_kind_restores_schemes() {
        with_mock(|| {
            let app = mock_app();
            open_wizard(&app);
            let (active0, enabled0) = {
                let st = app.borrow();
                let mut s = st.wizard.borrow_mut();
                let s = s.as_mut().unwrap();
                s.cur.active = "my_codetable".into();
                s.cur.enabled = vec!["my_codetable".into(), "english".into()];
                s.draft.active = s.cur.active.clone();
                s.draft.enabled = s.cur.enabled.clone();
                (s.cur.active.clone(), s.cur.enabled.clone())
            };
            let c = Controls::new();
            sync(&app, &c);
            c.kind.set(1);
            write_back(&app, &c);
            sync(&app, &c);
            assert_eq!(draft_of(&app).active, "pinyin_simp");
            c.kind.set(0);
            write_back(&app, &c);
            let d = draft_of(&app);
            assert_eq!(d.kind, Kind::Codetable);
            assert_eq!(d.active, active0);
            assert_eq!(d.enabled, enabled0);
        });
    }

    /// 写入失败：进结果段，结果文案如实列出已完成的步骤。setItems 的被拒项只是跳过、合法项
    /// 照样落盘，不能说成「没有改动任何配置」。
    #[test]
    fn rejected_items_report_partial_write_on_result_page() {
        struct Skips;
        impl crate::wizard::apply::CoreCalls for Skips {
            fn call(
                &mut self,
                m: &str,
                _p: &serde_json::Value,
            ) -> Result<serde_json::Value, String> {
                Ok(match m {
                    "config.setItems" => serde_json::json!({
                        "applied": 2,
                        "skipped": [{"key": "input.emoji.enabled", "reason": "x"}]
                    }),
                    _ => serde_json::json!({}),
                })
            }
        }
        with_mock(|| {
            let app = mock_app();
            open_wizard(&app);
            {
                let st = app.borrow();
                let mut s = st.wizard.borrow_mut();
                let s = s.as_mut().unwrap();
                s.draft = s.draft.with_preset(Kind::Pinyin);
            }
            crate::wizard::finish_with(&app, &mut Skips, crate::state::LoadedState::fetch);
            let st = app.borrow();
            assert_eq!(st.wizard_phase.get(), WizardPhase::Result);
            let s = st.wizard.borrow();
            let text = result_text(s.as_ref().unwrap());
            assert!(text.contains("input.emoji.enabled"), "{text}");
            assert!(!text.contains("没有改动任何配置"), "{text}");
            assert!(text.contains("其余 2 项已写入"), "{text}");
        });
    }

    /// 当前布局不在 `schema.layouts` 清单里时，同步/回写不得把它悄悄换成清单里的某一项。
    #[test]
    fn unlisted_layout_survives_write_back() {
        with_mock(|| {
            let app = mock_app();
            open_wizard(&app);
            {
                let st = app.borrow();
                let mut s = st.wizard.borrow_mut();
                let s = s.as_mut().unwrap();
                s.cur.layout = "my_layout".into();
                s.draft.layout = "my_layout".into();
            }
            let c = Controls::new();
            c.layout_idx.set(2);
            sync(&app, &c);
            c.slash.set(!c.slash.get());
            write_back(&app, &c);
            assert_eq!(draft_of(&app).layout, "my_layout");
        });
    }

    /// 键冲突时第 3 步的冲突文案非空（「下一步」据此禁用）。
    #[test]
    fn conflicting_keys_fill_blockers() {
        with_mock(|| {
            let app = mock_app();
            open_wizard(&app);
            {
                let st = app.borrow();
                let mut s = st.wizard.borrow_mut();
                let d = &mut s.as_mut().unwrap().draft;
                d.kind = Kind::Codetable;
                d.temp_pinyin = true;
                d.quick = Trigger::Key("backtick".into());
                d.temp_key = Trigger::Key("backtick".into());
            }
            let c = Controls::new();
            sync(&app, &c);
            assert!(!c.blockers.get().is_empty());
        });
    }

    /// 方案勾选：默认方案锁定且勾上；点其它方案在草稿里增删。
    #[test]
    fn scheme_toggle_edits_enabled_but_not_locked_main() {
        with_mock(|| {
            let app = mock_app();
            open_wizard(&app);
            let c = Controls::new();
            sync(&app, &c);
            let rows = c.schemes.get().concat();
            let main = rows.iter().find(|r| r.locked).expect("默认方案应锁定");
            assert!(main.checked);
            let main_id = main.id.clone();
            let before = draft_of(&app);
            toggle_scheme(&app, &main_id);
            assert_eq!(draft_of(&app), before, "点锁定的默认方案不改草稿");
            let other = rows
                .iter()
                .find(|r| !r.locked)
                .expect("应有其它方案")
                .clone();
            toggle_scheme(&app, &other.id);
            assert_ne!(
                draft_of(&app).enabled.contains(&other.id),
                other.checked,
                "点一下应翻转勾选"
            );
        });
    }

    #[test]
    fn welcome_and_step_one_navigate_both_ways() {
        assert_eq!(
            forward_of(WizardPhase::Welcome),
            Some(WizardPhase::Step(1)),
            "欢迎页「开始」进第 1 步"
        );
        assert_eq!(
            back_of(WizardPhase::Step(1)),
            Some(WizardPhase::Welcome),
            "第 1 步「上一步」回欢迎页"
        );
        assert_eq!(back_of(WizardPhase::Step(3)), Some(WizardPhase::Step(2)));
        assert_eq!(forward_of(WizardPhase::Step(3)), Some(WizardPhase::Step(4)));
        assert_eq!(forward_of(WizardPhase::Step(4)), None, "第 4 步走「完成」");
        assert_eq!(back_of(WizardPhase::Confirm), Some(WizardPhase::Step(4)));
        assert_eq!(back_of(WizardPhase::Welcome), None);
    }

    // ── 布局：定高与分隔线 ──

    use windui::core::{NodeId, Tree};
    use windui::geometry::Size;
    use windui::spec::Dimension;

    fn is_divider(tree: &Tree, id: NodeId) -> bool {
        let n = tree.get(id).unwrap();
        n.children.is_empty() && n.height == Dimension::Px(1)
    }

    /// 前序遍历可见子树（不可见的整棵剪掉），记下每个可见叶子。
    fn visible_leaves(tree: &Tree, id: NodeId, out: &mut Vec<NodeId>) {
        let n = tree.get(id).unwrap();
        if !n.effective_visible() {
            return;
        }
        if n.children.is_empty() {
            out.push(id);
        }
        for c in &n.children {
            visible_leaves(tree, *c, out);
        }
    }

    /// 每条可见分隔线归到最近的带边框祖先（卡片、对话框外框）；同一容器内按前序排开，
    /// 分隔线不得打头、收尾或两条相邻——即「分隔线两侧都有可见内容」。返回违例描述。
    fn divider_violations(tree: &Tree, root: NodeId) -> Vec<String> {
        let mut leaves = Vec::new();
        visible_leaves(tree, root, &mut leaves);
        let owner = |id: NodeId| {
            let mut p = tree.get(id).unwrap().parent;
            while let Some(x) = p {
                let n = tree.get(x).unwrap();
                if n.style.border.is_some() {
                    return Some(x);
                }
                p = n.parent;
            }
            None
        };
        let mut seqs: Vec<(Option<NodeId>, Vec<bool>)> = Vec::new();
        for id in leaves {
            let o = owner(id);
            let d = is_divider(tree, id);
            match seqs.iter_mut().find(|(k, _)| *k == o) {
                Some((_, v)) => v.push(d),
                None => seqs.push((o, vec![d])),
            }
        }
        let mut bad = Vec::new();
        for (o, v) in seqs {
            if !v.iter().any(|d| *d) {
                continue;
            }
            if v.first() == Some(&true) || v.last() == Some(&true) {
                bad.push(format!("{o:?}：分隔线打头或收尾 {v:?}"));
            }
            if v.windows(2).any(|w| w[0] && w[1]) {
                bad.push(format!("{o:?}：两条分隔线相邻 {v:?}"));
            }
        }
        bad
    }

    /// 护栏自检：老写法（条件行直接进 `rows`）必须被判出来，否则下面的断言恒空过。
    #[test]
    fn divider_check_catches_hidden_last_row() {
        let mut tree = Tree::new();
        let root = crate::widgets::card(vec![
            Element::label("常显行"),
            Element::label("条件行").visible_when(|| false),
        ])
        .build(&mut tree);
        tree.root = Some(root);
        assert!(!divider_violations(&tree, root).is_empty());

        let mut tree = Tree::new();
        let root = crate::widgets::card(vec![with_cond(
            Element::label("常显行"),
            vec![Element::label("条件行")],
            || false,
        )])
        .build(&mut tree);
        tree.root = Some(root);
        assert!(divider_violations(&tree, root).is_empty());
    }

    const ALL_PHASES: [WizardPhase; 8] = [
        WizardPhase::Welcome,
        WizardPhase::Dirty,
        WizardPhase::Step(1),
        WizardPhase::Step(2),
        WizardPhase::Step(3),
        WizardPhase::Step(4),
        WizardPhase::Confirm,
        WizardPhase::Result,
    ];

    /// 搭好整块面板（无对话框遮罩），返回 (树, 面板节点, 阶段信号, 控件信号)。
    fn build_frame(app: &Rc<RefCell<AppState>>) -> (Tree, NodeId, Signal<WizardPhase>, Controls) {
        let phase = app.borrow().wizard_phase;
        let c = Controls::new();
        sync(app, &c);
        let mut tree = Tree::new();
        let root = Element::stack()
            .child(frame(app, phase, c))
            .build(&mut tree);
        tree.root = Some(root);
        let panel = tree.get(root).unwrap().children[0];
        (tree, panel, phase, c)
    }

    fn layout(tree: &mut Tree) {
        tree.layout_root(
            Size::new(1200, 1000),
            &mut windui::text::LineAwareTextEngine,
        );
    }

    /// 开关组合：主类型 × 模糊音 × 临时拼音。
    /// 第 1 步的三种形态：0 码表、1 拼音（全拼）、2 拼音（双拼主方案）。
    fn set_shape(c: &Controls, shape: usize) {
        c.kind.set(shape.min(1));
        c.is_sp.set(shape == 2);
    }

    /// 开关组合：形态 × 模糊音 × 临时拼音。
    fn combos() -> Vec<(usize, bool, bool)> {
        let mut v = Vec::new();
        for kind in 0..3 {
            for fuzzy in [false, true] {
                for temp in [false, true] {
                    v.push((kind, fuzzy, temp));
                }
            }
        }
        v
    }

    /// 维护者反馈：「最后一个选项的最下面也有一个分割线」。任意阶段、任意开关组合下，
    /// 卡片里都不能出现收尾、打头或叠在一起的分隔线。
    #[test]
    fn no_stray_dividers_in_any_phase_or_toggle_combo() {
        with_mock(|| {
            let app = mock_app();
            open_wizard(&app);
            let (mut tree, panel, phase, c) = build_frame(&app);
            for p in ALL_PHASES {
                for (kind, fuzzy, temp) in combos() {
                    phase.set(p);
                    set_shape(&c, kind);
                    c.fuzzy.set(fuzzy);
                    c.temp.set(temp);
                    layout(&mut tree);
                    let bad = divider_violations(&tree, panel);
                    assert!(
                        bad.is_empty(),
                        "{p:?} kind={kind} fuzzy={fuzzy} temp={temp}：{bad:?}"
                    );
                }
            }
        });
    }

    /// 维护者反馈：「开关后对话框一会大一会小」。外框高度与按钮栏位置在任何阶段、任何
    /// 开关组合、摘要再长也不变；内容超出时溢出成正文区的滚动量。
    #[test]
    fn panel_size_and_footer_stay_put() {
        with_mock(|| {
            let app = mock_app();
            open_wizard(&app);
            let (mut tree, panel, phase, c) = build_frame(&app);
            let footer = *tree.get(panel).unwrap().children.last().unwrap();
            let mut footer_y = None;
            for p in ALL_PHASES {
                for (kind, fuzzy, temp) in combos() {
                    phase.set(p);
                    set_shape(&c, kind);
                    c.fuzzy.set(fuzzy);
                    c.temp.set(temp);
                    layout(&mut tree);
                    let n = tree.get(panel).unwrap();
                    assert_eq!(n.measured.h, DIALOG_H, "{p:?} kind={kind}");
                    assert_eq!(n.measured.w, DIALOG_W, "{p:?} kind={kind}");
                    let y = tree.get(footer).unwrap().bounds.y;
                    assert_eq!(*footer_y.get_or_insert(y), y, "{p:?} 按钮栏移位");
                }
            }
            // 超长摘要：外框不撑高，溢出进第 4 步的滚动量。
            phase.set(WizardPhase::Step(4));
            c.summary.set(
                (0..60)
                    .map(|i| SumLine::Row {
                        label: format!("第 {i} 项"),
                        old: "开".into(),
                        new: "关".into(),
                    })
                    .collect(),
            );
            layout(&mut tree);
            assert_eq!(tree.get(panel).unwrap().measured.h, DIALOG_H);
            let (h, content_h) = page_scroll(&tree, panel, 5);
            assert!(content_h > h, "长摘要应可滚动：{content_h} <= {h}");
        });
    }

    /// 第 `i` 个阶段页的滚动容器 (视口高, 内容高)。次序同 [`frame`] 里 stack 的子项。
    fn page_scroll(tree: &Tree, panel: NodeId, i: usize) -> (i32, i32) {
        let pages = tree.get(panel).unwrap().children[2];
        let sc = tree.get(pages).unwrap().children[i];
        let n = tree.get(sc).unwrap();
        (n.measured.h, n.content_h)
    }

    /// 造 `n` 行方案（首行为锁定的默认方案），按网格列数分好。名字取较长的自定义码表名。
    fn fake_schemes(n: usize) -> Vec<Vec<SchemeRow>> {
        let rows: Vec<SchemeRow> = (0..n)
            .map(|i| SchemeRow {
                id: format!("s{i}"),
                name: format!("五笔86（含拼音）{i}"),
                checked: i < 3,
                locked: i == 0,
            })
            .collect();
        rows.chunks(SCHEME_COLS)
            .map(<[SchemeRow]>::to_vec)
            .collect()
    }

    /// 正文区底部至少留出的余量：真机字体与测试文字引擎的度量有出入，不能贴着放。
    const FIT_MARGIN: i32 = 24;

    /// [`DIALOG_H`] 的取值依据：最高的常见形态（第 1 步双拼 + 8 个方案、第 2 步双拼 +
    /// 模糊音开、第 3 步码表 + 临时拼音开）以及欢迎页，不滚动即可放下，且留 [`FIT_MARGIN`]。
    #[test]
    fn tallest_steps_fit_without_scrolling() {
        with_mock(|| {
            let app = mock_app();
            open_wizard(&app);
            let (mut tree, panel, phase, c) = build_frame(&app);
            c.schemes.set(fake_schemes(8));
            for (p, i, kind, fuzzy, temp) in [
                (WizardPhase::Welcome, 0, 0, false, false),
                (WizardPhase::Step(1), 2, 2, false, false),
                (WizardPhase::Step(2), 3, 2, true, false),
                (WizardPhase::Step(3), 4, 0, false, true),
            ] {
                phase.set(p);
                set_shape(&c, kind);
                c.fuzzy.set(fuzzy);
                c.temp.set(temp);
                layout(&mut tree);
                let (h, content_h) = page_scroll(&tree, panel, i);
                eprintln!("{p:?}: viewport={h} content={content_h}");
                assert!(
                    content_h + FIT_MARGIN <= h,
                    "{p:?} 余量不足：{content_h} + {FIT_MARGIN} > {h}"
                );
            }
        });
    }

    /// 方案清单两列网格：N=1..8 时格子一个不少、都排在两列里、不越出清单右缘。
    #[test]
    fn scheme_grid_holds_every_row_in_two_columns() {
        with_mock(|| {
            let app = mock_app();
            let schemes = signal(Vec::new());
            // 第 1 步正文宽：对话框宽 − 正文左右内边距 − 卡片左右内边距。
            let grid_w = DIALOG_W - 2 * 18 - 2 * 16;
            let mut tree = Tree::new();
            let root = Element::stack()
                .child(scheme_grid(&app, schemes).width(grid_w))
                .build(&mut tree);
            tree.root = Some(root);
            for n in 1..=8 {
                schemes.set(fake_schemes(n));
                layout(&mut tree);
                let grid = tree.get(root).unwrap().children[0];
                let host = tree.get(grid).unwrap().children[0];
                assert!(tree.get(host).unwrap().bounds.w <= grid_w, "n={n}");
                let mut cells = 0;
                // bounds 相对父节点：格子不越出所在行，行不越出网格。
                for r in tree.get(host).unwrap().children.clone() {
                    let row = tree.get(r).unwrap();
                    assert!(row.bounds.w <= grid_w, "n={n} 行越界 {:?}", row.bounds);
                    assert_eq!(row.children.len(), SCHEME_COLS, "n={n}");
                    for cell in row.children.clone() {
                        let c = tree.get(cell).unwrap();
                        if c.children.is_empty() {
                            continue; // 末行补位
                        }
                        cells += 1;
                        let b = c.bounds;
                        assert!(b.w > 0 && b.h > 0, "n={n} 空格子 {b:?}");
                        assert!(
                            b.x >= 0 && b.x + b.w <= row.bounds.w,
                            "n={n} 越界 {b:?} / {:?}",
                            row.bounds
                        );
                        let cb = tree.get(c.children[0]).unwrap().bounds;
                        assert!(cb.x + cb.w <= b.w, "n={n} 勾选框越出格子");
                    }
                }
                assert_eq!(cells, n, "n={n} 缺项");
                assert_eq!(
                    tree.get(host).unwrap().children.len(),
                    n.div_ceil(SCHEME_COLS),
                    "n={n} 行数"
                );
            }
        });
    }
}
