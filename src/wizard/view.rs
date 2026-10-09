//! 设置向导对话框。框架照 `dialogs/update_dialog.rs`：一个面板，各段正文与按钮按
//! [`WizardPhase`] `visible_when` 切换。
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
use super::{close, complete, finish, layout_label, open_wizard, step3_blockers, summary_of};
use crate::pages::dict::reactive::reactive_effect;
use crate::state::{AppState, WizardPhase, WizardSession};
use crate::widgets::{dialog_section, setting_row};

const DIALOG_W: i32 = 600;
const KINDS: [Kind; 3] = [Kind::Codetable, Kind::Pinyin, Kind::Shuangpin];

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
    layout_opts: Signal<Vec<String>>,
    layout_idx: Signal<usize>,
    /// 布局清单拿不到时只显示当前值的名字。
    layout_text: Signal<String>,
    schemes: Signal<Vec<SchemeRow>>,
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
    hints: Signal<String>,
    confirm_text: Signal<String>,
    result_text: Signal<String>,
}

impl Controls {
    fn new() -> Self {
        Controls {
            kind: signal(0),
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
            hints: signal(String::new()),
            confirm_text: signal(String::new()),
            result_text: signal(String::new()),
        }
    }

    /// 全部「用户可改」控件的组合版本号：任一变了即变。
    fn input_version(&self) -> u64 {
        let mut v = self.kind.version();
        for x in [
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
    let cur = &s.cur.layout;
    if !v.is_empty() && !cur.is_empty() && !v.iter().any(|(id, _)| id == cur) {
        v.push((cur.clone(), cur.clone()));
    }
    v
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
    let layouts = layout_choices(s);
    set_if(
        c.layout_opts,
        layouts.iter().map(|(_, n)| n.clone()).collect(),
    );
    if let Some(i) = layouts.iter().position(|(id, _)| *id == d.layout) {
        set_if(c.layout_idx, i);
    }
    set_if(c.layout_text, layout_label(&s.layouts, &d.layout));
    set_if(c.schemes, scheme_rows(&st, d));
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
        c.confirm_text,
        format!("将覆盖以下 {} 项现有设置，确定吗？", changes.len()),
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
        let next = if kind != s.draft.kind {
            s.draft.switch_kind(&s.cur, kind, &st.schemas)
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
    let is = move |p: WizardPhase| move || phase.get() == p;

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

    let frame = Element::col()
        .width(DIALOG_W)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(title_row(phase))
        .child(Element::divider())
        .child(dirty_body().visible_when(is(WizardPhase::Dirty)))
        .child(step1_body(app, c).visible_when(is(WizardPhase::Step(1))))
        .child(step2_body(c).visible_when(is(WizardPhase::Step(2))))
        .child(step3_body(c).visible_when(is(WizardPhase::Step(3))))
        .child(step4_body(c).visible_when(is(WizardPhase::Step(4))))
        .child(text_body(c.confirm_text, Role::Text).visible_when(is(WizardPhase::Confirm)))
        .child(text_body(c.result_text, Role::Danger).visible_when(is(WizardPhase::Result)))
        .child(Element::divider())
        .child(footer(app, phase, c))
        // 次序即同帧执行序：先收控件、再推草稿，套预设的那一帧就能把新值送上界面。
        .child(writer)
        .child(syncer)
        .child(esc_watch);

    let app_x = app.clone();
    Element::dialog_closable(visible, frame, move |_| close(&app_x))
}

fn title_row(phase: Signal<WizardPhase>) -> Element {
    let title = phase.map(|p| match p {
        WizardPhase::Step(n) => format!("设置向导 · 第 {n}/4 步"),
        WizardPhase::Confirm => "设置向导 · 确认".to_string(),
        WizardPhase::Dirty | WizardPhase::Result => "设置向导".to_string(),
    });
    Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(18, 14)
        .child(
            Element::label_signal(title)
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text),
        )
}

fn body() -> Element {
    Element::col().width_match().spacing(8).padding_xy(18, 12)
}

fn note(text: &str) -> Element {
    Element::label(text)
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(3)
}

fn dirty_body() -> Element {
    body()
        .child(
            Element::label("设置页有未保存的修改。")
                .font_size(13.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match(),
        )
        .child(note(
            "向导写入后会重新读取配置，未保存的修改会和它互相覆盖。请先保存或放弃这些修改，再继续设置向导。",
        ))
}

fn step1_body(app: &Rc<RefCell<AppState>>, c: Controls) -> Element {
    let kind = c.kind;
    let kind_row = Element::row()
        .width_match()
        .spacing(18)
        .child(Element::radio("五笔 / 码表", kind, 0))
        .child(Element::radio("全拼", kind, 1))
        .child(Element::radio("双拼", kind, 2));
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
    )
    .visible_when(move || kind.get() == 2);

    let app_c = app.clone();
    let boxes = Element::host_signal(c.schemes, move |row: SchemeRow| {
        let app = app_c.clone();
        let id = row.id.clone();
        let label = if row.locked {
            format!("{}（默认方案）", row.name)
        } else {
            row.name.clone()
        };
        Element::checkbox(label, signal(row.checked))
            .small()
            .fg_role(Role::Text)
            .enabled(!row.locked)
            // 接管点击：勾选态由草稿决定，改完草稿整行列表随 `sync` 重建。
            .on_click(move |_| toggle_scheme(&app, &id))
    });
    body()
        .child(dialog_section(
            "输入习惯",
            vec![
                Element::col()
                    .width_match()
                    .padding_xy(12, 10)
                    .child(kind_row),
                layout_row,
            ],
        ))
        .child(crate::manifest::dialog_group_header("一起启用的方案"))
        .child(
            Element::scroll()
                .width_match()
                .height(150)
                .border_role(Role::Border, 1)
                .corner(6.0)
                .padding_xy(10, 6)
                .child(boxes),
        )
        .child(note("主类型对应的方案会设为默认方案，并强制启用。"))
}

fn switch_row(label: &str, sub: Option<&str>, sig: Signal<bool>) -> Element {
    setting_row(label, sub, Element::switch(sig))
}

fn step2_body(c: Controls) -> Element {
    let kind = c.kind;
    let fuzzy = c.fuzzy;
    let mut pairs = Element::row().width_match().spacing(16).padding_xy(12, 8);
    for (i, (_, label)) in FUZZY_PAIRS.iter().enumerate() {
        pairs = pairs.child(Element::checkbox(*label, c.pairs[i]).small());
    }
    let codetable = dialog_section(
        "五笔 / 码表",
        vec![
            switch_row("全码唯一自动上屏", None, c.auto_commit),
            switch_row("满码空码清空", None, c.clear_empty),
            switch_row("拼音反查", Some("打拼音时显示五笔编码"), c.code_hint),
        ],
    )
    .visible_when(move || kind.get() == 0);
    let pinyin = dialog_section(
        "拼音",
        vec![
            switch_row("emoji 候选", None, c.emoji),
            switch_row("模糊音", None, c.fuzzy),
            pairs.visible_when(move || fuzzy.get()),
            switch_row(
                "允许按全拼输入",
                Some("双拼方案下也可直接打全拼"),
                c.allow_full,
            )
            .visible_when(move || kind.get() == 2),
        ],
    )
    .visible_when(move || kind.get() != 0);
    body().child(codetable).child(pinyin)
}

fn step3_body(c: Controls) -> Element {
    let kind = c.kind;
    let temp = c.temp;
    let blockers = c.blockers;
    body()
        .child(dialog_section(
            "通用",
            vec![
                setting_row(
                    "快捷输入启用键",
                    None,
                    Element::dropdown_signal(c.quick_opts, c.quick_idx).width(200),
                ),
                switch_row("/ 键输出顿号", None, c.slash),
                switch_row("临时拼音", Some("五笔里临时打拼音"), c.temp)
                    .visible_when(move || kind.get() == 0),
                setting_row(
                    "临时拼音触发键",
                    None,
                    Element::dropdown_signal(c.temp_opts, c.temp_idx).width(200),
                )
                .visible_when(move || kind.get() == 0 && temp.get()),
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

fn step4_body(c: Controls) -> Element {
    let summary = c.summary;
    let hints = c.hints;
    let list = Element::host_signal(summary, |line: SumLine| match line {
        SumLine::Group(g) => crate::manifest::dialog_group_header(&g),
        SumLine::Row { label, old, new } => Element::row()
            .width_match()
            .spacing(10)
            .padding_xy(4, 3)
            .child(
                Element::label(label)
                    .font_size(12.5)
                    .fg_role(Role::Text)
                    .width(150)
                    .max_lines(1),
            )
            .child(
                Element::label(format!("{old}  →  {new}"))
                    .font_size(12.5)
                    .fg_role(Role::Text)
                    .weight(1.0)
                    .max_lines(2),
            ),
    });
    body()
        .child(
            Element::label("当前配置已符合所选，无需修改")
                .font_size(13.0)
                .fg_role(Role::Text)
                .width_match()
                .visible_when(move || summary.get().is_empty()),
        )
        .child(
            Element::scroll()
                .width_match()
                .height(240)
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

fn text_body(text: Signal<String>, role: Role) -> Element {
    body().child(
        Element::label_signal(text)
            .font_size(12.5)
            .fg_role(role)
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
                .on_click(move |_| {
                    // 与设置页「重新加载」同一条路：丢弃未保存改动（含方案 override 待提交项）。
                    discard_app.borrow_mut().reload();
                    open_wizard(&discard_app);
                })
                .visible_when(is(WizardPhase::Dirty)),
        )
        .child(
            Element::button("保存并继续")
                .small()
                .on_click(move |_| {
                    let ok = save_app.borrow_mut().save();
                    if ok {
                        open_wizard(&save_app);
                    }
                })
                .visible_when(is(WizardPhase::Dirty)),
        )
        // 1–4 步
        .child(
            Element::button("上一步")
                .small()
                .outline_soft()
                .on_click(move |_| phase.set(WizardPhase::Step(step() - 1)))
                .visible_when(move || (2..=4).contains(&step())),
        )
        .child(
            Element::button("下一步")
                .small()
                .on_click(move |_| phase.set(WizardPhase::Step(step() + 1)))
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
                .on_click(move |_| phase.set(WizardPhase::Step(4)))
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
            assert_eq!(d.active, "pinyin");
            assert!(app.borrow().wizard_rev.get() != rev0, "草稿变了要推 rev");
            sync(&app, &c);
            assert!(c.emoji.get(), "拼音预设 emoji 开");
            assert!(!c.fuzzy.get());
            // 控件与草稿一致后再回写一次，草稿不变。
            write_back(&app, &c);
            assert_eq!(draft_of(&app), d);
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
            assert_eq!(draft_of(&app).active, "pinyin");
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
                s.draft = s.draft.with_preset(Kind::Pinyin, &st.schemas);
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
            let rows = c.schemes.get();
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
}
