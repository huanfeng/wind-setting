//! 「应用兼容」独立窗的界面：左栏分类与整体操作 + 中栏进程列表 + 右栏配置信息。
//!
//! 界面**完全由核心给的元数据驱动**（`compat.schema`）：字段清单、分组、控件类型、说明文字
//! 都来自核心，这里只按 `Kind` 挑控件。核心新增一个字段，设置端不改一行就能在「添加覆盖项」里
//! 选到它。
//!
//! 右栏**只列被设置过的字段**（出厂设了的、用户改了的、用户取消了出厂值的，再加用户刚挑出来
//! 等值的），要覆盖别的功能走「添加覆盖项」对话框——一条规则通常只动一两项，全列出来只会淹没它。
//!
//! 每个控件都是「以当前值为基线新建」的：任何一次写入成功后详情整体重建（见 `state.rs`
//! 的模块说明），所以控件上的信号永远只反映用户刚做的动作，不存在「回填时被误当成改动」。

use serde_json::{Value, json};
use windui::prelude::*;

use super::model::{FieldMeta, FieldSource, FieldView, Kind, RuleState, RuleView, Schema, Section};
use super::state::{CompatState, FILTERS};
use crate::pages::dict::reactive::{reactive_effect, toast_sink};

/// 左栏（分类 + 整体操作）宽度。
const NAV_W: i32 = 184;
/// 中栏（进程列表）宽度。
const LIST_W: i32 = 270;
/// 左栏条目的行高，与词库窗左栏一致。
const NAV_ITEM_H: i32 = 36;

/// 整个窗口内容：标题栏 + 三栏主体 + 全部浮层对话框。
///
/// `back` 是左栏底部的「← 设置」按钮，由窗口层构造（它要 `AppState`，这里只认识 `CompatState`）。
pub fn build_content(mgr: &CompatState, titlebar: Element, back: Element) -> Element {
    // `host_signal` 只吃 `Signal<Vec<T>>` 并对每个元素各建一份子树，所以这里用「恒一个元素」
    // 的 Vec 做载体：effect 在各 epoch 变化时把最新的视图装进去，信号一变子树即整体重建。
    //
    // 先加载再按当下状态给初值：effect 以构建时的版本为基线，构建帧之前发生的加载不会再触发它。
    mgr.open_refresh();
    let list_sig: Signal<Vec<Vec<RuleView>>> = signal(vec![mgr.visible_rules()]);
    let detail_sig: Signal<Vec<Option<RuleView>>> = signal(vec![mgr.selected_rule()]);
    let nav_sig: Signal<Vec<usize>> = signal(vec![mgr.section.get()]);

    let divider = || {
        Element::leaf()
            .width(1)
            .height_match()
            .bg_role(Role::Divider)
    };
    let body = Element::row()
        .fill()
        .weight(1.0)
        .child(build_nav(mgr, nav_sig, back))
        .child(divider())
        .child(build_list(mgr, list_sig))
        .child(divider())
        .child(build_right(mgr, detail_sig))
        .child(sync_effects(mgr, list_sig, detail_sig, nav_sig))
        .child(toast_sink(mgr.feedback));

    let main_col = Element::col()
        .fill()
        .child(titlebar)
        .child(Element::divider())
        .child(body);

    let mut root = Element::stack().fill().bg_role(Role::Bg).child(main_col);
    for dlg in super::dialogs::build_dialogs(mgr) {
        root = root.child(dlg);
    }
    root
}

/// 把状态里的各类「变了」信号接到具体动作上。dropdown / segmented / stepper 没有变更回调，
/// 只能靠版本号轮询（见 `reactive_effect`）。
fn sync_effects(
    mgr: &CompatState,
    list_sig: Signal<Vec<Vec<RuleView>>>,
    detail_sig: Signal<Vec<Option<RuleView>>>,
    nav_sig: Signal<Vec<usize>>,
) -> Element {
    let m = mgr.clone();
    let list_fx = reactive_effect(
        {
            let m = m.clone();
            move || m.list_epoch.version() + m.schema.version()
        },
        {
            let m = m.clone();
            move || list_sig.set(vec![m.visible_rules()])
        },
    );
    let detail_fx = reactive_effect(
        {
            let m = m.clone();
            move || m.detail_epoch.version() + m.schema.version() + m.show_advanced.version()
        },
        {
            let m = m.clone();
            move || detail_sig.set(vec![m.selected_rule()])
        },
    );
    let nav_fx = reactive_effect(
        {
            let m = m.clone();
            move || m.section.version()
        },
        {
            let m = m.clone();
            move || nav_sig.set(vec![m.section.get()])
        },
    );
    let section_fx = reactive_effect(
        {
            let m = m.clone();
            move || m.section.version()
        },
        {
            let m = m.clone();
            move || m.switch_section()
        },
    );
    let filter_fx = reactive_effect(
        {
            let m = m.clone();
            move || m.filter.version() + m.search.version()
        },
        {
            let m = m.clone();
            move || m.refilter()
        },
    );
    let refresh_fx = reactive_effect(
        {
            let m = m.clone();
            move || m.refresh_tick.version()
        },
        move || m.reload(),
    );
    Element::col()
        .width(0)
        .height(0)
        .child(list_fx)
        .child(detail_fx)
        .child(nav_fx)
        .child(section_fx)
        .child(filter_fx)
        .child(refresh_fx)
}

// ───────────────────────── 左栏：分类与整体操作 ─────────────────────────

fn build_nav(mgr: &CompatState, nav_sig: Signal<Vec<usize>>, back: Element) -> Element {
    let m = mgr.clone();
    let items = Element::host_signal(nav_sig, move |cur: usize| nav_items(&m, cur));

    let (add, imp, exp, reset, help, refresh) = (
        mgr.clone(),
        mgr.clone(),
        mgr.clone(),
        mgr.clone(),
        mgr.clone(),
        mgr.clone(),
    );
    let actions = Element::col()
        .width_match()
        .spacing(6)
        .padding_xy(12, 10)
        .child(
            Element::button("新增规则")
                .small()
                .width_match()
                .on_click(move |_| add.open_add()),
        )
        .child(
            Element::button("导入")
                .small()
                .outline_soft()
                .width_match()
                .on_click(move |ctx| imp.start_import(ctx)),
        )
        .child(
            Element::button("导出")
                .small()
                .outline_soft()
                .width_match()
                .on_click(move |ctx| exp.start_export(ctx)),
        )
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .padding_xy(2, 6)
                .child(
                    Element::label("显示高级选项")
                        .font_size(12.5)
                        .fg_role(Role::TextMuted),
                )
                .child(Element::flex_spacer())
                .child(Element::switch(mgr.show_advanced)),
        );

    let footer = Element::col()
        .width_match()
        .padding_xy(12, 8)
        .spacing(6)
        .child(
            Element::label_signal(mgr.load_error)
                .font_size(12.0)
                .fg_role(Role::Danger)
                .width_match()
                .max_lines(4)
                .visible_when({
                    let e = mgr.load_error;
                    move || !e.get().is_empty()
                }),
        )
        .child(
            Element::button("选项说明")
                .small()
                .outline_soft()
                .width_match()
                .on_click(move |_| help.dlg.help_visible.set(true)),
        )
        .child(
            Element::button("全部还原")
                .small()
                .outline_soft()
                .width_match()
                .on_click(move |_| reset.dlg.reset_visible.set(true)),
        )
        .child(
            Element::row()
                .width_match()
                .spacing(6)
                .child(back.weight(1.0))
                .child(
                    Element::button("刷新")
                        .small()
                        .outline_soft()
                        .weight(1.0)
                        .on_click(move |_| refresh.reload()),
                ),
        );

    Element::col()
        .width(NAV_W)
        .height_match()
        .bg_role(Role::Surface)
        .padding_xy(0, 10)
        .child(items.padding_xy(10, 0))
        .child(Element::divider())
        .child(actions)
        .child(Element::flex_spacer())
        .child(Element::divider())
        .child(footer)
}

/// 三个分类。选中样式：淡底 + 粗体（与词库窗左栏同一读法）。
fn nav_items(mgr: &CompatState, current: usize) -> Element {
    let mut col = Element::col().width_match().spacing(2);
    for (i, sec) in Section::ALL.iter().enumerate() {
        let sig = mgr.section;
        let mut row = Element::row()
            .width_match()
            .height(NAV_ITEM_H)
            .cross(Align::Center)
            .padding_xy(8, 0)
            .corner(6.0)
            .clickable()
            .on_click(move |_| {
                if sig.get() != i {
                    sig.set(i);
                }
            })
            .child(
                Element::label(sec.title())
                    .font_size(13.5)
                    .font_weight(if i == current { 600 } else { 400 })
                    .fg_role(if i == current {
                        Role::Text
                    } else {
                        Role::TextMuted
                    }),
            );
        if i == current {
            row = row.bg_role(Role::Track);
        }
        col = col.child(row);
    }
    col
}

// ───────────────────────── 中栏：进程列表 ─────────────────────────

fn build_list(mgr: &CompatState, list_sig: Signal<Vec<Vec<RuleView>>>) -> Element {
    let m = mgr.clone();
    let rows = Element::host_signal(list_sig, move |rules: Vec<RuleView>| rule_list(&m, &rules));

    Element::col()
        .width(LIST_W)
        .height_match()
        .bg_role(Role::Surface)
        .padding_xy(12, 12)
        .spacing(8)
        .child(Element::text_input(mgr.search, "搜索进程名"))
        .child(Element::segmented(FILTERS.to_vec(), mgr.filter))
        .child(
            Element::scroll()
                .weight(1.0)
                .width_match()
                .child(Element::col().width_match().spacing(2).child(rows)),
        )
}

fn rule_list(mgr: &CompatState, rules: &[RuleView]) -> Element {
    let mut col = Element::col().width_match().spacing(2);
    if rules.is_empty() {
        return col.child(
            Element::label("没有匹配的规则")
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .padding_xy(6, 12),
        );
    }
    let selected = mgr.selected.get();
    for r in rules {
        col = col.child(rule_row(mgr, r, r.process == selected));
    }
    col
}

fn state_intent(s: RuleState) -> Intent {
    match s {
        RuleState::System => Intent::Neutral,
        RuleState::Modified => Intent::Primary,
        RuleState::User => Intent::Success,
        RuleState::Disabled => Intent::Warning,
    }
}

fn comment_of(r: &RuleView) -> String {
    r.effective
        .get("comment")
        .and_then(Value::as_str)
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

fn rule_row(mgr: &CompatState, r: &RuleView, selected: bool) -> Element {
    let m = mgr.clone();
    let name = r.process.clone();
    let comment = comment_of(r);
    let mut text = Element::col().weight(1.0).spacing(2).child(
        Element::label(&r.process)
            .font_size(13.0)
            .font_weight(600)
            .fg_role(if r.state == RuleState::Disabled {
                Role::TextMuted
            } else {
                Role::Text
            })
            .max_lines(1)
            .truncate(Truncate::End),
    );
    if !comment.is_empty() {
        text = text.child(
            Element::label(comment)
                .font_size(11.5)
                .fg_role(Role::TextMuted)
                .max_lines(1)
                .truncate(Truncate::End),
        );
    }
    let mut row = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(6)
        .padding_xy(8, 7)
        .corner(6.0)
        .clickable()
        .on_click(move |_| m.select(&name))
        .child(text);
    if selected {
        row = row.bg_role(Role::Track);
    }
    if r.state != RuleState::System {
        row = row.child(Element::badge_intent(
            r.state.label(),
            state_intent(r.state),
        ));
    }
    row
}

// ───────────────────────── 右栏：配置信息 ─────────────────────────

fn build_right(mgr: &CompatState, detail_sig: Signal<Vec<Option<RuleView>>>) -> Element {
    let m = mgr.clone();
    let body = Element::host_signal(detail_sig, move |rule: Option<RuleView>| match rule {
        Some(r) => build_detail(&m, &r),
        None => Element::label("在左侧选择一条规则，或新增一条。")
            .font_size(13.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .padding_xy(0, 60)
            .text_align(Align::Center),
    });
    Element::scroll().weight(1.0).child(
        Element::col()
            .width_match()
            .padding_xy(20, 16)
            .spacing(14)
            .child(body),
    )
}

/// 这个字段要不要出现在详情里：被某一层设置过，或用户刚挑出来等着给值。
fn is_shown(mgr: &CompatState, rule: &RuleView, key: &str, fv: &FieldView) -> bool {
    fv.source != FieldSource::Default || mgr.is_pending(&rule.process, key)
}

fn build_detail(mgr: &CompatState, rule: &RuleView) -> Element {
    let schema = mgr.schema.get();
    let section = mgr.current_section();

    let mut col = Element::col()
        .width_match()
        .spacing(14)
        .child(detail_header(mgr, rule));

    // 被当成「没写」忽略的无效内容：让用户知道为什么某个字段没生效。
    let warns: Vec<String> = mgr.warnings.with(|ws| {
        ws.iter()
            .filter(|w| w.section == section.id() && w.process.eq_ignore_ascii_case(&rule.process))
            .map(|w| {
                if w.key.is_empty() {
                    w.message.clone()
                } else {
                    format!("{}：{}", w.key, w.message)
                }
            })
            .collect()
    });
    for w in warns {
        col = col.child(
            Element::label(format!("⚠ {w}"))
                .font_size(12.0)
                .fg_role(Role::Warning)
                .width_match()
                .max_lines(3),
        );
    }
    if !rule.unknown_keys.is_empty() {
        col = col.child(
            Element::label(format!(
                "此条目里有本版本不认识的键（{}），已原样保留，不会被改动。",
                rule.unknown_keys.join("、")
            ))
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(3),
        );
    }

    let mut shown = 0usize;
    for (group, fields) in schema.groups_of(section) {
        let rows: Vec<Element> = fields
            .into_iter()
            .filter_map(|f| {
                let v = rule.fields.get(&f.key)?;
                is_shown(mgr, rule, &f.key, v).then(|| field_row(mgr, rule, &schema, f, v))
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        shown += rows.len();
        col = col.child(crate::widgets::section(&group.label, rows));
    }

    if shown == 0 {
        col = col.child(
            Element::label(
                "这条规则还没有任何配置项。点下面的「添加覆盖项」，选择要为它单独设置的功能。",
            )
            .font_size(12.5)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(3),
        );
    }

    let m = mgr.clone();
    col.child(
        Element::row().width_match().child(
            Element::button("＋ 添加覆盖项")
                .small()
                .on_click(move |_| m.dlg.field_visible.set(true)),
        ),
    )
}

fn detail_header(mgr: &CompatState, rule: &RuleView) -> Element {
    let comment = comment_of(rule);
    let mut title = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::label(&rule.process)
                .font_size(17.0)
                .font_weight(600)
                .fg_role(Role::Text),
        );
    if rule.state != RuleState::System {
        title = title.child(Element::badge_intent(
            rule.state.label(),
            state_intent(rule.state),
        ));
    }
    title = title.child(Element::flex_spacer());

    let draft = rule.is_draft();
    let disabled = rule.state == RuleState::Disabled;
    if !draft {
        let m = mgr.clone();
        let p = rule.process.clone();
        title = title.child(
            Element::button(if disabled { "启用" } else { "禁用" })
                .small()
                .outline_soft()
                .on_click(move |_| {
                    m.set_disabled(&p, !disabled);
                }),
        );
    }
    if rule.can_reset() {
        let m = mgr.clone();
        let p = rule.process.clone();
        title = title.child(
            Element::button("还原为出厂")
                .small()
                .outline_soft()
                .on_click(move |_| {
                    m.reset_rule(&p);
                }),
        );
    }
    if draft {
        let m = mgr.clone();
        title = title.child(
            Element::button("放弃")
                .small()
                .outline_soft()
                .on_click(move |_| m.drop_draft()),
        );
    } else if rule.is_user_only() {
        let m = mgr.clone();
        let p = rule.process.clone();
        title = title.child(Element::button("删除").small().danger().on_click(move |_| {
            m.reset_rule(&p);
        }));
    }

    let mut col = Element::col().width_match().spacing(4).child(title);
    if draft {
        col = col.child(
            Element::label("新规则尚未保存：设置任意一项后才会写入。")
                .font_size(12.0)
                .fg_role(Role::TextMuted),
        );
    }
    if !comment.is_empty() {
        col = col.child(
            Element::label(comment)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3),
        );
    }
    col
}

// ───────────────────────── 字段行 ─────────────────────────

/// 联动条件是否满足：`dependsOn` 指向的字段当前生效值等于要求值才有意义。
fn dep_met(rule: &RuleView, meta: &FieldMeta) -> bool {
    match &meta.depends_on {
        None => true,
        Some(d) => rule
            .fields
            .get(&d.key)
            .is_some_and(|v| v.value.as_str() == Some(d.value.as_str())),
    }
}

fn source_tag(src: FieldSource) -> Option<Element> {
    match src {
        FieldSource::Default => None,
        FieldSource::System => Some(Element::badge_intent("出厂", Intent::Neutral)),
        FieldSource::User => Some(Element::badge_intent("已修改", Intent::Primary)),
        FieldSource::Cleared => Some(Element::badge_intent("已取消出厂值", Intent::Warning)),
    }
}

fn field_row(
    mgr: &CompatState,
    rule: &RuleView,
    schema: &Schema,
    meta: &FieldMeta,
    fv: &FieldView,
) -> Element {
    let mut ctl = Element::row().cross(Align::Center).spacing(8);
    if let Some(tag) = source_tag(fv.source) {
        ctl = ctl.child(tag);
    }
    ctl = ctl.child(field_control(mgr, &rule.process, meta, fv));
    let met = dep_met(rule, meta);
    // 「还原」放在置灰的包裹之外：依赖条件不满足时用户层可能仍留着这个字段的值，
    // 不能让它变成清不掉的孤儿。
    let pending = mgr.is_pending(&rule.process, &meta.key);
    let mut reset_btn = None;
    if pending || fv.can_reset() {
        let m = mgr.clone();
        let (p, k) = (rule.process.clone(), meta.key.clone());
        // 用户新增的规则、或还没给值的覆盖项，没有「出厂」可还原，去掉就是去掉。
        let remove = pending || rule.system.is_none();
        reset_btn = Some(
            Element::button(if remove { "移除" } else { "还原" })
                .small()
                .outline_soft()
                .tooltip(if remove {
                    "去掉这个覆盖项，恢复为跟随全局"
                } else {
                    "还原为出厂设置"
                })
                .on_click(move |_| {
                    if remove {
                        m.remove_override(&p, &k);
                    } else {
                        m.reset_field(&p, &k);
                    }
                }),
        );
    }

    let mut sub = meta.summary.clone();
    if !met {
        if let Some(d) = &meta.depends_on {
            let dep_label = schema
                .field(mgr.current_section(), &d.key)
                .map_or(d.key.as_str(), |f| f.label.as_str());
            sub = format!(
                "{sub}（需先把「{dep_label}」设为 {}）",
                option_label(&d.value)
            );
        }
    }
    let mut right = Element::row()
        .cross(Align::Center)
        .spacing(8)
        .child(ctl.disabled(!met));
    if let Some(b) = reset_btn {
        right = right.child(b);
    }
    crate::widgets::setting_row(&meta.label, Some(&sub), right).tooltip(meta.problem.clone())
}

/// 枚举值的展示名。只给语义确定的值配中文，其余原样显示（配置文件里写的就是它）。
fn option_label(id: &str) -> String {
    let zh = match id {
        "english" => "英文",
        "chinese" => "中文",
        "follow_caret" => "跟随光标",
        "fixed" => "固定位置",
        "screen_center" => "屏幕中央",
        "screen_top_left" => "屏幕左上",
        "screen_top_right" => "屏幕右上",
        "screen_bottom_left" => "屏幕左下",
        "screen_bottom_right" => "屏幕右下",
        "window_center" => "窗口中央",
        "window_bottom_left" => "窗口左下",
        "last" => "沿用上次位置",
        "hide" => "隐藏",
        "keep" => "保持原样",
        "cr" => "CR",
        "lf" => "LF",
        "crlf" => "CRLF",
        _ => return id.to_string(),
    };
    format!("{zh}（{id}）")
}

/// 按字段类型造控件。写入成功后详情整体重建，所以这里的局部信号只管「用户刚做了什么」。
fn field_control(mgr: &CompatState, process: &str, meta: &FieldMeta, fv: &FieldView) -> Element {
    let key = meta.key.clone();
    let proc = process.to_string();
    let m = mgr.clone();
    match meta.kind {
        Kind::Bool => {
            let sig = signal(fv.value.as_bool().unwrap_or(false));
            Element::switch(sig).on_switch_change(move |_, on| {
                m.set_field(&proc, &key, json!(on));
            })
        }
        Kind::TriBool => {
            let idx = match fv.value.as_bool() {
                None => 0,
                Some(true) => 1,
                Some(false) => 2,
            };
            let sig = signal(idx);
            let fx = reactive_effect(
                move || sig.version(),
                move || match sig.get() {
                    0 => {
                        m.clear_field(&proc, &key);
                    }
                    i => {
                        m.set_field(&proc, &key, json!(i == 1));
                    }
                },
            );
            Element::row()
                .child(Element::segmented(vec!["跟随全局", "开", "关"], sig))
                .child(fx)
        }
        Kind::Enum => {
            let mut labels = vec!["（跟随全局）".to_string()];
            labels.extend(meta.options.iter().map(|o| option_label(o)));
            let idx = fv
                .value
                .as_str()
                .and_then(|v| meta.options.iter().position(|o| o == v))
                .map_or(0, |i| i + 1);
            let sig = signal(idx);
            let options = meta.options.clone();
            let fx = reactive_effect(
                move || sig.version(),
                move || match sig.get() {
                    0 => {
                        m.clear_field(&proc, &key);
                    }
                    i => {
                        if let Some(o) = options.get(i - 1) {
                            m.set_field(&proc, &key, json!(o));
                        }
                    }
                },
            );
            Element::row()
                .child(Element::dropdown(labels, sig).min_width(190))
                .child(fx)
        }
        Kind::Int => {
            // stepper 的输入框每打一个字就回写信号，按信号变化写盘会在第一位数字后
            // 就重建详情、丢掉焦点；改成与文本类一致的显式提交。
            let sig = signal(fv.value.as_i64().unwrap_or(0) as f64);
            Element::row()
                .cross(Align::Center)
                .spacing(6)
                .child(Element::stepper(sig, -10000.0, 100000.0, 1.0).width(120))
                .child(
                    Element::button("应用")
                        .small()
                        .outline_soft()
                        .on_click(move |_| {
                            m.set_field(&proc, &key, json!(sig.get().round() as i64));
                        }),
                )
        }
        Kind::Text => {
            let sig = signal(fv.value.as_str().unwrap_or("").to_string());
            text_editor(sig, "回车或点「应用」", move |text| {
                let t = text.trim();
                if t.is_empty() {
                    m.clear_field(&proc, &key);
                } else {
                    m.set_field(&proc, &key, json!(t));
                }
            })
        }
        Kind::TextList => {
            let items: Vec<String> = fv
                .value
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let sig = signal(items.join(", "));
            text_editor(sig, "多个值用逗号分隔", move |text| {
                let list = split_list(text);
                if list.is_empty() {
                    m.clear_field(&proc, &key);
                } else {
                    m.set_field(&proc, &key, json!(list));
                }
            })
        }
    }
}

/// 文本类字段：输入框 + 「应用」。text_input 只有回车回调，逐键写盘不合适，所以显式提交。
fn text_editor(
    sig: Signal<String>,
    placeholder: &str,
    commit: impl Fn(&str) + Clone + 'static,
) -> Element {
    let c1 = commit.clone();
    let c2 = commit;
    Element::row()
        .cross(Align::Center)
        .spacing(6)
        .child(
            Element::text_input(sig, placeholder)
                .width(200)
                .on_submit(move |_| c1(&sig.get())),
        )
        .child(
            Element::button("应用")
                .small()
                .outline_soft()
                .on_click(move |_| c2(&sig.get())),
        )
}

/// 逗号（半角 / 全角）、顿号、换行分隔，去空白去空项、保序去重。
fn split_list(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in text.split([',', '，', '、', '\n', '\r']) {
        let p = part.trim();
        if !p.is_empty() && !out.iter().any(|x| x == p) {
            out.push(p.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use windui::core::Tree;
    use windui::geometry::Size;

    /// mock 模式 + 全 crate 唯一的锁（同 `state::tests::MockGuard`）。
    struct MockGuard(std::sync::MutexGuard<'static, ()>);
    impl MockGuard {
        fn new() -> Self {
            let g = crate::rpc::ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
            MockGuard(g)
        }
    }
    impl Drop for MockGuard {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        }
    }

    fn frame(tree: &mut Tree) {
        let mut te = windui::text::NullTextEngine;
        tree.layout_root(Size::new(1000, 700), &mut te);
    }

    fn mount(st: &AppState) -> Tree {
        let mut tree = Tree::new();
        let id = build_content(&st.compat_mgr, Element::leaf(), Element::button("back"))
            .build(&mut tree);
        tree.root = Some(id);
        frame(&mut tree);
        tree
    }

    /// 三段里每一条规则的详情都能建出来并排版（控件的 `debug_assert` 误用会在这里炸）。
    #[test]
    fn every_rule_in_every_section_builds_a_detail() {
        let _g = MockGuard::new();
        let st = AppState::empty();
        let mgr = st.compat_mgr.clone();
        mgr.ensure_loaded();
        mgr.show_advanced.set(true);
        for i in 0..Section::ALL.len() {
            mgr.section.set(i);
            mgr.switch_section();
            for r in mgr.rules.get() {
                let mut tree = Tree::new();
                let id = build_detail(&mgr, &r).build(&mut tree);
                tree.root = Some(id);
                frame(&mut tree);
            }
        }
    }

    /// 页面上的「变了」信号确实接到了动作：切段 / 筛选走 effect，而不是只改了个信号。
    #[test]
    fn section_and_filter_signals_drive_the_state_through_effects() {
        let _g = MockGuard::new();
        let st = AppState::empty();
        let mgr = st.compat_mgr.clone();
        mgr.ensure_loaded();
        let mut tree = mount(&st);

        mgr.section.set(1);
        frame(&mut tree);
        assert_eq!(mgr.current_section(), Section::InitialModeScope);
        assert!(
            mgr.rules
                .with(|r| r.iter().all(|x| x.fields.contains_key("classes"))),
            "切段后应换成新一段的规则"
        );

        mgr.section.set(0);
        frame(&mut tree);
        let before = mgr.selected.get();
        mgr.filter.set(3);
        frame(&mut tree);
        // `visible_rules` 直接读筛选信号，光看它不能证明 effect 接上了；
        // 选中项被 `refilter` 修正到已禁用的规则上才是 effect 跑过的证据。
        let now = mgr.selected.get();
        assert_ne!(now, before, "筛选变了，effect 应把选中项挪进可见集");
        assert!(mgr.rules.with(|r| {
            r.iter()
                .any(|x| x.process == now && x.state == RuleState::Disabled)
        }));
    }

    /// 详情只列被设置过的字段；没设置的要走「添加覆盖项」，选出来才占一行。
    #[test]
    fn detail_lists_only_fields_that_are_set_or_picked() {
        let _g = MockGuard::new();
        let st = AppState::empty();
        let mgr = st.compat_mgr.clone();
        mgr.ensure_loaded();
        mgr.select("Weixin.exe");
        let rule = mgr.selected_rule().unwrap();
        let shown = |mgr: &CompatState, rule: &RuleView| -> Vec<String> {
            rule.fields
                .iter()
                .filter(|(k, v)| is_shown(mgr, rule, k, v))
                .map(|(k, _)| k.clone())
                .collect()
        };
        let before = shown(&mgr, &rule);
        assert!(
            before.contains(&"caret_use_top".to_string()),
            "出厂设了的要列出"
        );
        assert!(
            !before.contains(&"initial_mode".to_string()),
            "没设置过的不列"
        );
        assert!(before.len() < rule.fields.len(), "不是把全部字段都摆出来");

        mgr.add_override("Weixin.exe", "initial_mode");
        let rule = mgr.selected_rule().unwrap();
        assert!(shown(&mgr, &rule).contains(&"initial_mode".to_string()));
    }

    /// 候选 = 尚未设置的字段：已列出的不再提供，高级项默认不给、打开开关才给。
    #[test]
    fn override_candidates_exclude_set_fields_and_hide_advanced_by_default() {
        let _g = MockGuard::new();
        let st = AppState::empty();
        let mgr = st.compat_mgr.clone();
        mgr.ensure_loaded();
        mgr.select("Weixin.exe");
        let keys = |mgr: &CompatState| -> Vec<String> {
            super::super::dialogs::override_candidates(mgr)
                .into_iter()
                .flat_map(|(_, items)| items.into_iter().map(|c| c.key))
                .collect()
        };
        let basic = keys(&mgr);
        assert!(
            !basic.contains(&"caret_use_top".to_string()),
            "已设置的不再候选"
        );
        assert!(basic.contains(&"initial_mode".to_string()));
        // Weixin 没设置 host_render（高级项）：默认不提供，打开开关才提供。
        let advanced_key = "host_render".to_string();
        assert!(!basic.contains(&advanced_key), "高级项默认不提供");
        mgr.show_advanced.set(true);
        assert!(
            keys(&mgr).contains(&advanced_key),
            "打开「显示高级选项」后才提供"
        );
    }

    #[test]
    fn split_list_handles_mixed_separators_and_dedups() {
        assert_eq!(split_list(" A, B，C、A\nD ,, "), vec!["A", "B", "C", "D"]);
        assert!(split_list("  , ，").is_empty());
    }

    #[test]
    fn option_label_keeps_unknown_ids_verbatim() {
        assert_eq!(option_label("wait"), "wait");
        assert_eq!(option_label("english"), "英文（english）");
    }
}
