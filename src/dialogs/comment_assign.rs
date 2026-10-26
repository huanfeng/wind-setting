//! 候选注释的「分配」视图：上方全局，下方只列有覆盖的作用范围（可折叠卡片），底栏是添加覆盖、
//! 恢复默认与说明。
//!
//! 格值的读写规则与草稿在 [`crate::dialogs::comment_settings`]；这里只有界面。每一态只有
//! 「跟随上级 | 自定义」两种状态，跟随的文字固定、不随值变（设计 §13.5）。
//! [`build_assign_body`] 是对话框正文，[`build_assign_footer`] 是底栏左侧，
//! [`build_assign_subdialogs`] 是添加覆盖的选择框。
//!
//! 设计见 core 仓 `docs/design/candidate-comment-templates.md` §13.5。

use windui::prelude::*;
use windui::ui::select::DropdownItem;

use crate::dialogs::comment_settings::{
    Choice, CommentSettingsUi, CommentState, LibraryInfo, Names, Scope, cell_options, choice_label,
    deleted_rows, from_scope_label, has_user_cells, override_rows, scope_level, scope_name,
    shown_choice, state_follows,
};
use crate::pages::dict::reactive::reactive_effect;
use wind_config::comment_lib::CommentAssign;

/// (i) 的说明：优先级与两种跟随。
pub const ASSIGN_TIP: &str = "优先级（从高到低，第一个有设置的胜出）：\
     模式（临时拼音、网址等）> 方案 > 方案类型（码表、拼音…）> 全局。\n\
     全局两态都有值；其余各项没设置的态就跟随上级。\n\
     「默认」是平时显示的注释，「详细」是按切换键翻出的注释。\n\
     竖排选「同横排」时与本项本态的横排相同。";

/// 两个格下拉的宽度。
const CELL_W: i32 = 170;
/// 态标签列宽（默认 / 详细）。
const STATE_W: i32 = 44;

/// 添加覆盖的三类，与选择框里分段控件的顺序一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickKind {
    Schema,
    Type,
    Mode,
}

impl PickKind {
    const ALL: [PickKind; 3] = [PickKind::Schema, PickKind::Type, PickKind::Mode];

    fn label(self) -> &'static str {
        match self {
            PickKind::Schema => "方案",
            PickKind::Type => "方案类型",
            PickKind::Mode => "模式",
        }
    }
}

/// 选择框里可选的项：尚未列出的方案类型 / 方案 / 模式；删过的出厂项可以再选（加回即恢复出厂）。
pub(super) fn picker_options(
    names: &Names,
    user: &CommentAssign,
    system: &CommentAssign,
    kind: PickKind,
) -> Vec<(Scope, String)> {
    let rows = override_rows(user, system);
    match kind {
        PickKind::Schema => names.addable_schemas(&rows),
        PickKind::Type => names.addable_types(&rows),
        PickKind::Mode => names.addable_modes(&rows),
    }
}

/// 分配视图的界面状态：展开着的卡片、恢复默认的确认气泡、添加覆盖的选择框。
#[derive(Clone)]
pub struct AddPicker {
    pub visible: Signal<bool>,
    kind: Signal<usize>,
    options: Signal<Vec<(Scope, String)>>,
    labels: Signal<Vec<String>>,
    sel: Signal<usize>,
    ok: Signal<bool>,
    /// 展开着的卡片（[`Scope::key`]）。卡片随每次刷新重建，展开状态得留在这里。
    open: Signal<Vec<String>>,
    confirm_reset: Signal<bool>,
}

impl AddPicker {
    pub(super) fn new() -> Self {
        Self {
            visible: signal(false),
            kind: signal(0),
            options: signal(Vec::new()),
            labels: signal(Vec::new()),
            sel: signal(0),
            ok: signal(false),
            open: signal(Vec::new()),
            confirm_reset: signal(false),
        }
    }
}

impl CommentSettingsUi {
    /// 按当前分段重算选择框的选项；没有可选项时「确定」不可点。
    fn fill_picker(&self) {
        let kind = PickKind::ALL[self.pick.kind.get().min(2)];
        let opts = picker_options(
            &self.names.borrow(),
            &self.assign.get(),
            &self.system_assign(),
            kind,
        );
        let p = &self.pick;
        p.labels.set(opts.iter().map(|(_, l)| l.clone()).collect());
        p.ok.set(!opts.is_empty());
        p.options.set(opts);
        p.sel.set(0);
    }

    pub(super) fn open_picker(&self) {
        self.pick.kind.set(0);
        self.fill_picker();
        self.pick.visible.set(true);
    }

    /// 选择框点「添加」：加选中的那一项并展开它。
    pub(super) fn confirm_picker(&self) {
        let p = &self.pick;
        let Some((scope, _)) = p.options.get().get(p.sel.get()).cloned() else {
            return;
        };
        self.add(&scope);
        let key = scope.key();
        p.open.update(|v| {
            if !v.contains(&key) {
                v.push(key);
            }
        });
    }

    fn toggle_card(&self, key: &str) {
        self.pick
            .open
            .update(|v| match v.iter().position(|k| k == key) {
                Some(i) => {
                    v.remove(i);
                }
                None => v.push(key.to_string()),
            });
    }
}

/// 分配视图正文：全局卡、覆盖列表、已删除出厂项的提示。随库视图整块重建。
pub fn build_assign_body(ui: &CommentSettingsUi) -> Element {
    let u = ui.clone();
    Element::col()
        .width_match()
        .padding_xy(0, 10)
        .child(Element::host_signal(ui.view, move |lib: LibraryInfo| {
            build_list(&u, &lib)
        }))
}

fn build_list(ui: &CommentSettingsUi, lib: &LibraryInfo) -> Element {
    let user = ui.assign.get();
    let names = ui.names.borrow().clone();
    let rows = override_rows(&user, &lib.system_assign);
    let mut col = Element::col()
        .width_match()
        .spacing(10)
        .child(global_card(ui, lib, &user))
        .child(section_title(
            "覆盖（优先级：模式 → 方案 → 方案类型 → 全局）",
        ));
    if rows.is_empty() {
        col = col.child(
            Element::label(
                "没有单独设置，全部按全局显示。点底栏「添加覆盖」为某个方案或模式单独指定。",
            )
            .font_size(12.5)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(2)
            .padding_xy(12, 14)
            .border_role(Role::Border, 1)
            .corner(8.0),
        );
    }
    for scope in &rows {
        col = col.child(scope_card(ui, lib, &user, scope, &names));
    }
    let deleted = deleted_rows(&user, &lib.system_assign);
    if !deleted.is_empty() {
        let list: Vec<String> = deleted.iter().map(|s| scope_name(s, &names)).collect();
        col = col.child(
            Element::label(format!(
                "已删除 {} 项出厂设置（{}）。重新添加或「恢复默认」可找回。",
                deleted.len(),
                list.join("、")
            ))
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(2),
        );
    }
    col
}

fn section_title(text: &str) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(0, 2)
        .child(
            Element::label(text)
                .font_size(12.0)
                .fg_role(Role::TextMuted),
        )
        .child(Element::divider().weight(1.0))
}

fn card_shell() -> Element {
    Element::col()
        .width_match()
        .bg_role(Role::SurfaceAlt)
        .border_role(Role::Border, 1)
        .corner(8.0)
}

fn global_card(ui: &CommentSettingsUi, lib: &LibraryInfo, user: &CommentAssign) -> Element {
    let head = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(12, 9)
        .child(
            Element::label("全局")
                .font_size(13.0)
                .font_weight(600)
                .fg_role(Role::Text),
        )
        .child(
            Element::label("下方没有单独设置的方案与模式，都按这里显示")
                .font_size(12.0)
                .fg_role(Role::TextMuted),
        );
    let mut body = Element::col().width_match().padding_xy(12, 4);
    for st in CommentState::ALL {
        body = body.child(state_row(ui, lib, user, &Scope::Global, st, None));
    }
    card_shell()
        .child(head)
        .child(Element::divider())
        .child(body)
}

/// 折叠行上一态的概括：「默认 跟随上级」「详细 拼音+英文」（竖排单独指定时再加「竖排 …」）。
fn state_summary(
    lib: &LibraryInfo,
    user: &CommentAssign,
    scope: &Scope,
    st: CommentState,
) -> Element {
    let sys = &lib.system_assign;
    let (h, v) = st.cols();
    let follows = state_follows(user, sys, scope, st);
    let mut text = Element::row()
        .cross(Align::Center)
        .spacing(4)
        .padding_xy(7, 2)
        .corner(4.0)
        .bg_role(Role::Track)
        .child(
            Element::label(st.label())
                .font_size(11.5)
                .fg_role(Role::TextMuted),
        );
    if follows {
        return text.child(
            Element::label("跟随上级")
                .font_size(11.5)
                .fg_role(Role::TextMuted),
        );
    }
    text = text.child(
        Element::label(choice_label(lib, &shown_choice(user, sys, scope, h)))
            .font_size(11.5)
            .fg_role(Role::Text),
    );
    let vc = shown_choice(user, sys, scope, v);
    if vc != Choice::SameAsHorizontal {
        text = text
            .child(
                Element::label("竖排")
                    .font_size(11.5)
                    .fg_role(Role::TextMuted),
            )
            .child(
                Element::label(choice_label(lib, &vc))
                    .font_size(11.5)
                    .fg_role(Role::Text),
            );
    }
    text
}

fn scope_card(
    ui: &CommentSettingsUi,
    lib: &LibraryInfo,
    user: &CommentAssign,
    scope: &Scope,
    names: &Names,
) -> Element {
    let key = scope.key();
    let open = ui.pick.open;
    let is_open = {
        let k = key.clone();
        move || open.with(|v| v.contains(&k))
    };
    let is_open2 = is_open.clone();
    let caret = Element::stack()
        .size(12, 16)
        .child(
            Element::label("▸")
                .font_size(11.0)
                .fg_role(Role::TextMuted)
                .visible_when({
                    let f = is_open.clone();
                    move || !f()
                }),
        )
        .child(
            Element::label("▾")
                .font_size(11.0)
                .fg_role(Role::TextMuted)
                .visible_when(is_open.clone()),
        );
    let mut summary = Element::row().cross(Align::Center).spacing(6);
    for st in CommentState::ALL {
        summary = summary.child(state_summary(lib, user, scope, st));
    }
    let (ud, sd) = (ui.clone(), scope.clone());
    let (ut, kt) = (ui.clone(), key.clone());
    let head = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(12, 8)
        .clickable()
        .on_click(move |_| ut.toggle_card(&kt))
        .child(caret)
        .child(Element::badge_intent(scope_level(scope), Intent::Neutral))
        .child(
            Element::label(scope_name(scope, names))
                .font_size(13.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .max_lines(1),
        )
        .child(summary)
        .child(Element::leaf().weight(1.0))
        .child(
            Element::row()
                .padding_xy(8, 3)
                .corner(4.0)
                .clickable()
                .tooltip("删除后这一项按上级显示")
                .on_click(move |_| ud.delete(&sd))
                .child(Element::label("删除").font_size(12.5).fg_role(Role::Danger)),
        );

    let mut body = Element::col().width_match().padding_xy(12, 4);
    for st in CommentState::ALL {
        body = body.child(state_row(ui, lib, user, scope, st, Some(names)));
    }
    if scope.in_factory(&lib.system_assign) {
        let modified = has_user_cells(user, scope);
        let (ur, sr) = (ui.clone(), scope.clone());
        body = body.child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .padding_xy(0, 6)
                .child(
                    Element::label(if modified {
                        "已在出厂设置的基础上修改"
                    } else {
                        "这一项是出厂设置"
                    })
                    .font_size(12.0)
                    .fg_role(Role::TextMuted),
                )
                .child(
                    Element::button("恢复此项出厂设置")
                        .small()
                        .outline_soft()
                        .on_click(move |_| ur.restore(&sr))
                        .visible(modified),
                ),
        );
    }
    card_shell()
        .child(head)
        .child(Element::divider().visible_when(is_open2.clone()))
        .child(body.visible_when(is_open2))
}

/// 一态一行：态名、「跟随上级 | 自定义」（全局没有）、跟随文字或横竖两格下拉。
/// `names` 为 `None` 即全局行。
fn state_row(
    ui: &CommentSettingsUi,
    lib: &LibraryInfo,
    user: &CommentAssign,
    scope: &Scope,
    st: CommentState,
    names: Option<&Names>,
) -> Element {
    let mut row = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(10)
        .padding_xy(0, 6)
        .child(
            Element::label(st.label())
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width(STATE_W),
        );
    let follows = names.is_some() && state_follows(user, &lib.system_assign, scope, st);
    if names.is_some() {
        let sel = signal(usize::from(!follows));
        let (u, s) = (ui.clone(), scope.clone());
        row = row
            .child(Element::segmented(vec!["跟随上级", "自定义"], sel))
            .child(reactive_effect(
                move || sel.version(),
                move || u.set_state(&s, st, sel.get() == 0),
            ));
    }
    if follows {
        let names = names.expect("跟随只出现在覆盖项上");
        row.child(follow_text(lib, scope, st, names))
    } else {
        let (h, v) = st.cols();
        row.child(cell_field("横排", ui, lib, user, scope, h))
            .child(cell_field("竖排", ui, lib, user, scope, v))
    }
}

/// 跟随态的说明：模式层的上级随当前方案变，只说一句；其余写出跟到谁、值是什么。
fn follow_text(lib: &LibraryInfo, scope: &Scope, st: CommentState, names: &Names) -> Element {
    let muted = |t: String| Element::label(t).font_size(12.5).fg_role(Role::TextMuted);
    if matches!(scope, Scope::Mode(_)) {
        return muted("当前方案怎么显示，这里就怎么显示".into());
    }
    let (h, v) = st.cols();
    let (Some((hid, from)), Some((vid, _))) =
        (lib.effective_of(scope, h), lib.effective_of(scope, v))
    else {
        return muted("跟随上级".into());
    };
    let mut row = Element::row()
        .cross(Align::Center)
        .child(muted(format!("与{}相同：", from_scope_label(from, names))))
        .child(
            Element::label(lib.name_of(hid))
                .font_size(12.5)
                .fg_role(Role::Text),
        );
    if vid != hid {
        row = row.child(muted("，竖排 ".into())).child(
            Element::label(lib.name_of(vid))
                .font_size(12.5)
                .fg_role(Role::Text),
        );
    }
    row
}

fn cell_field(
    caption: &'static str,
    ui: &CommentSettingsUi,
    lib: &LibraryInfo,
    user: &CommentAssign,
    scope: &Scope,
    col: crate::dialogs::comment_settings::Col,
) -> Element {
    let current = shown_choice(user, &lib.system_assign, scope, col);
    let options = cell_options(lib, user, scope, col, &current);
    let items: Vec<DropdownItem> = options
        .iter()
        .map(|c| DropdownItem::new(choice_label(lib, c)))
        .collect();
    let sel = signal(options.iter().position(|c| *c == current).unwrap_or(0));
    let (u, s) = (ui.clone(), scope.clone());
    let effect = reactive_effect(
        move || sel.version(),
        move || {
            if let Some(c) = options.get(sel.get()) {
                u.choose(&s, col, c);
            }
        },
    );
    Element::row()
        .cross(Align::Center)
        .spacing(6)
        .child(
            Element::label(caption)
                .font_size(12.0)
                .fg_role(Role::TextSubtle),
        )
        .child(Element::dropdown_items(items, sel).width(CELL_W))
        .child(effect)
}

/// 底栏左侧：添加覆盖、恢复默认（气泡确认）、优先级说明、文档链接。
pub fn build_assign_footer(ui: &CommentSettingsUi) -> Vec<Element> {
    let ua = ui.clone();
    let confirm = ui.pick.confirm_reset;
    let ur = ui.clone();
    let bubble = Element::col()
        .width(260)
        .padding(12)
        .spacing(8)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(8.0)
        .shadow(Shadow::new(0.0, 8.0, 24.0, Color::rgba(0, 0, 0, 90)))
        .child(
            Element::label("清空你的全部分配，回到出厂设置？")
                .font_size(12.5)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(2),
        )
        .child(
            Element::label("自定义模板会保留，只是不再被分配。")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(2),
        )
        .child(
            Element::row()
                .width_match()
                .spacing(6)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| confirm.set(false)),
                )
                .child(Element::button("恢复").small().on_click(move |_| {
                    ur.restore_all();
                    confirm.set(false);
                })),
        );
    vec![
        Element::button("＋ 添加覆盖")
            .small()
            .outline_soft()
            .on_click(move |_| ua.open_picker()),
        Element::button("恢复默认")
            .small()
            .outline_soft()
            .on_click(move |_| confirm.set(!confirm.get()))
            .popup(confirm, bubble),
        crate::widgets::info_badge(signal(ASSIGN_TIP.to_string()), |_| {}),
        Element::link("查看文档")
            .url(crate::dialogs::field_dialogs::COMMENT_DOC_URL)
            .font_size(12.0),
    ]
}

/// 二级弹层：添加覆盖（作用范围分段 + 对象下拉，确认才加）。
pub fn build_assign_subdialogs(ui: &CommentSettingsUi) -> Element {
    let p = ui.pick.clone();
    let uk = ui.clone();
    let kind = p.kind;
    let refill = reactive_effect(move || kind.version(), move || uk.fill_picker());
    let field = |caption: &'static str, control: Element| {
        Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(12)
            .child(
                Element::label(caption)
                    .font_size(12.5)
                    .fg_role(Role::TextMuted)
                    .width(64),
            )
            .child(control)
    };
    let options = p.options;
    let content = Element::col()
        .width_match()
        .padding_xy(18, 12)
        .spacing(12)
        .child(field(
            "作用范围",
            Element::segmented(PickKind::ALL.iter().map(|k| k.label()).collect(), kind),
        ))
        .child(field(
            "对象",
            Element::dropdown_signal(p.labels, p.sel)
                .width(260)
                .visible_when(move || options.with(|o| !o.is_empty())),
        ))
        .child(
            Element::label("这一类都已有单独设置。")
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .padding_xy(76, 0)
                .visible_when(move || options.with(Vec::is_empty)),
        )
        .child(
            Element::label("已有设置的不在列表里。添加后两态都先跟随上级；删过的出厂项重新添加会恢复出厂设置。")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(2),
        )
        .child(refill);
    let u = ui.clone();
    crate::dialogs::field_dialogs::build_confirm_dialog_shell_sized(
        p.visible,
        signal("添加覆盖".to_string()),
        content,
        p.ok,
        move || u.confirm_picker(),
        "添加",
        230,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::comment_settings::{
        Col, add_row, delete_row, deleted_factory, restore_factory, set_state_custom,
        set_state_follow, write_choice,
    };
    use wind_config::comment_lib::CommentCells;

    fn names() -> Names {
        Names {
            schemas: vec![
                ("wubi86".into(), "五笔 86".into(), false),
                ("pinyin_simp".into(), "全拼".into(), false),
                // schema.list 里同一个 id 出现两次（安装目录 + 用户目录）。
                ("pinyin_simp".into(), "全拼".into(), false),
                ("emoji".into(), "表情".into(), true),
            ],
            mixes: vec![("quick".into(), "快捷".into())],
        }
    }

    fn system() -> CommentAssign {
        let mut a = CommentAssign::default();
        a.global.default_horizontal = Some("code_hint".into());
        a.by_type.insert(
            "codetable".into(),
            CommentCells {
                detail_horizontal: Some("chaizi_pinyin".into()),
                ..Default::default()
            },
        );
        a.mode.insert(
            "temp_english".into(),
            CommentCells {
                default_horizontal: Some("none".into()),
                ..Default::default()
            },
        );
        a
    }

    fn keys(v: &[(Scope, String)]) -> Vec<String> {
        v.iter().map(|(s, _)| s.key()).collect()
    }

    #[test]
    fn picker_lists_each_missing_scope_once() {
        let mut user = CommentAssign::default();
        user.schema.insert("wubi86".into(), CommentCells::default());
        let n = names();
        assert_eq!(
            keys(&picker_options(&n, &user, &system(), PickKind::Schema)),
            ["schema:pinyin_simp", "schema:emoji"]
        );
        assert_eq!(
            keys(&picker_options(&n, &user, &system(), PickKind::Type)),
            [
                "type:pinyin",
                "type:shuangpin",
                "type:mixed",
                "type:english"
            ],
            "出厂已有的码表不列"
        );
        assert_eq!(
            keys(&picker_options(&n, &user, &system(), PickKind::Mode)),
            [
                "mode:temp_pinyin",
                "mode:url",
                "mode:email",
                "mode:unicode",
                "mode:mix:quick",
                "mode:special:emoji",
            ],
            "出厂已有的临时英文不列"
        );
    }

    #[test]
    fn same_scope_cannot_be_added_twice() {
        let sys = system();
        let mut user = CommentAssign::default();
        let s = Scope::Schema("pinyin_simp".into());
        assert!(add_row(&mut user, &sys, &s));
        assert!(!add_row(&mut user, &sys, &s), "第二次不加");
        assert!(
            !add_row(&mut user, &sys, &Scope::Mode("temp_english".into())),
            "出厂项已列出"
        );
        assert!(!add_row(&mut user, &sys, &Scope::Global));
        assert_eq!(
            override_rows(&user, &sys)
                .iter()
                .filter(|r| **r == s)
                .count(),
            1
        );
    }

    /// 只列有覆盖的项：方案类型不再常驻，没有出厂也没有用户格的类型不出现。
    #[test]
    fn only_scopes_with_overrides_are_listed() {
        let sys = system();
        let user = CommentAssign::default();
        let rows: Vec<String> = override_rows(&user, &sys).iter().map(Scope::key).collect();
        assert_eq!(rows, ["type:codetable", "mode:temp_english"]);
    }

    /// 删除：用户加的整段删掉；出厂项写四格 inherit、不再列出，重新添加即恢复出厂。
    #[test]
    fn delete_user_scope_and_factory_scope() {
        let sys = system();
        let mut user = CommentAssign::default();
        let url = Scope::Mode("url".into());
        let ct = Scope::Type("codetable".into());
        add_row(&mut user, &sys, &url);
        assert!(delete_row(&mut user, &sys, &url));
        assert!(!user.mode.contains_key("url"));

        assert!(delete_row(&mut user, &sys, &ct));
        assert!(deleted_factory(&user, &sys, &ct));
        assert!(!override_rows(&user, &sys).contains(&ct));
        assert_eq!(deleted_rows(&user, &sys), [ct.clone()]);
        assert!(
            keys(&picker_options(&names(), &user, &sys, PickKind::Type)).contains(&ct.key()),
            "删过的出厂项可以再加"
        );
        assert!(add_row(&mut user, &sys, &ct));
        assert!(!has_user_cells(&user, &ct), "加回即恢复出厂");
        assert!(!delete_row(&mut user, &sys, &Scope::Global));
    }

    /// 两态都跟随与删除不是一回事：没值的出厂格不写 inherit，仍列出。
    #[test]
    fn following_both_states_keeps_the_scope_listed() {
        let sys = system();
        let mut user = CommentAssign::default();
        let ct = Scope::Type("codetable".into());
        for st in CommentState::ALL {
            set_state_follow(&mut user, &sys, &ct, st);
        }
        assert!(
            CommentState::ALL
                .iter()
                .all(|st| state_follows(&user, &sys, &ct, *st))
        );
        assert_eq!(
            user.by_type["codetable"].detail_horizontal.as_deref(),
            Some("inherit")
        );
        assert_eq!(user.by_type["codetable"].default_horizontal, None);
        assert!(override_rows(&user, &sys).contains(&ct));
    }

    /// 改为自定义：横排填上级的生效值、竖排同横排；已是自定义的不动。
    #[test]
    fn custom_state_starts_from_inherited_value() {
        let sys = system();
        let mut user = CommentAssign::default();
        let s = Scope::Schema("wubi86".into());
        add_row(&mut user, &sys, &s);
        set_state_custom(&mut user, &sys, &s, CommentState::Detail, "chaizi_pinyin");
        assert!(!state_follows(&user, &sys, &s, CommentState::Detail));
        assert_eq!(
            shown_choice(&user, &sys, &s, Col::DetailH),
            Choice::Template("chaizi_pinyin".into())
        );
        assert_eq!(
            shown_choice(&user, &sys, &s, Col::DetailV),
            Choice::SameAsHorizontal
        );
        write_choice(&mut user, &sys, &s, Col::DetailH, &Choice::Hide);
        set_state_custom(&mut user, &sys, &s, CommentState::Detail, "pinyin");
        assert_eq!(shown_choice(&user, &sys, &s, Col::DetailH), Choice::Hide);
    }

    /// 恢复此项出厂：用户格清掉，出厂项仍列出。
    #[test]
    fn restore_factory_keeps_factory_scope() {
        let sys = system();
        let mut user = CommentAssign::default();
        let te = Scope::Mode("temp_english".into());
        write_choice(
            &mut user,
            &sys,
            &te,
            Col::DetailH,
            &Choice::Template("pinyin".into()),
        );
        assert!(has_user_cells(&user, &te));
        restore_factory(&mut user, &te);
        assert!(!has_user_cells(&user, &te));
        assert!(override_rows(&user, &sys).contains(&te));
    }

    #[test]
    fn picker_confirm_adds_selected_scope_once_and_opens_it() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let ui = CommentSettingsUi::new();
        ui.seed(&crate::config::Config::from_value(serde_json::json!({})));
        ui.open_picker();
        let first = ui.pick.options.get()[0].0.clone();
        ui.confirm_picker();
        assert!(override_rows(&ui.assign.get(), &ui.system_assign()).contains(&first));
        assert!(ui.pick.open.get().contains(&first.key()), "加完就展开");
        ui.open_picker();
        assert!(
            ui.pick.options.get().iter().all(|(s, _)| *s != first),
            "重开选择框不再列出"
        );
        ui.pick.kind.set(1);
        ui.fill_picker();
        assert!(
            ui.pick
                .options
                .get()
                .iter()
                .all(|(s, _)| matches!(s, Scope::Type(_))),
            "切到方案类型只列类型"
        );
        ui.restore_all();
        assert_eq!(ui.assign.get(), CommentAssign::default());
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }
}
