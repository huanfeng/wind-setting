//! 候选注释的「分配」视图：总表（作用范围 × 四格）、每行的预览浮层、添加覆盖的选择框。
//!
//! 格值的读写规则与草稿在 [`crate::dialogs::comment_settings`]；这里只有界面。
//! [`build_assign_body`] 是自足的对话框正文（顶部按钮行 + 只在内部滚动的表格），
//! [`build_assign_subdialogs`] 是它的二级弹层，两者挂在同一个外壳旁即可。
//!
//! 设计见 core 仓 `docs/design/candidate-comment-templates.md` §13.4。

use windui::prelude::*;

use crate::dialogs::comment_settings::{
    Choice, Col, CommentSettingsUi, LibraryInfo, Names, Scope, cell_options, choice_label,
    has_user_cells, scope_label, shown_choice, table_rows,
};
use wind_config::comment_lib::CommentAssign;

/// 表格高度：顶部按钮行之下恰好放满外壳的视口，外壳不出滚动条，只有表格内部滚。
pub const TABLE_H: i32 = 344;
const SCOPE_W: f32 = 0.85;
const POPUP_W: i32 = 560;

/// (i) 的说明：优先级与「跟随 / 同横排」。
pub const ASSIGN_TIP: &str = "优先级：模式 > 方案 > 方案类型 > 全局。\n\
     全局是默认值；码表 / 拼音 / 双拼 / 混输 / 英文是方案类型；其余行是具体方案或模式的覆盖。\n\
     「跟随」= 用下一层同排布那格；竖排没写 = 同本行横排。\n\
     灰色斜体的格后缀的是它实际落到的模板。";

/// 添加覆盖的两类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickKind {
    Schema,
    Mode,
}

/// 选择框里可选的行：尚未在表里的方案 / 模式。
pub(super) fn picker_options(
    names: &Names,
    user: &CommentAssign,
    system: &CommentAssign,
    kind: PickKind,
) -> Vec<(Scope, String)> {
    let rows = table_rows(user, system);
    match kind {
        PickKind::Schema => names.addable_schemas(&rows),
        PickKind::Mode => names.addable_modes(&rows),
    }
}

/// 「添加方案 / 模式覆盖」选择框的状态。
#[derive(Clone)]
pub struct AddPicker {
    pub visible: Signal<bool>,
    title: Signal<String>,
    /// `(序号, 行, 显示名)`。
    options: Signal<Vec<(usize, Scope, String)>>,
    sel: Signal<usize>,
    ok: Signal<bool>,
}

impl AddPicker {
    pub(super) fn new() -> Self {
        Self {
            visible: signal(false),
            title: signal(String::new()),
            options: signal(Vec::new()),
            sel: signal(usize::MAX),
            ok: signal(false),
        }
    }
}

impl CommentSettingsUi {
    /// 打开选择框：选项按此刻的表现算，什么都没选时「确定」不可点。
    pub(super) fn open_picker(&self, kind: PickKind) {
        let opts = picker_options(
            &self.names.borrow(),
            &self.assign.get(),
            &self.system_assign(),
            kind,
        );
        let p = &self.pick;
        p.title.set(
            match kind {
                PickKind::Schema => "添加方案覆盖",
                PickKind::Mode => "添加模式覆盖",
            }
            .to_string(),
        );
        p.options.set(
            opts.into_iter()
                .enumerate()
                .map(|(i, (s, l))| (i, s, l))
                .collect(),
        );
        p.sel.set(usize::MAX);
        p.ok.set(false);
        p.visible.set(true);
    }

    /// 选择框点「确定」：加选中的那一行（已在表里的 [`CommentSettingsUi::add`] 不重复加）。
    pub(super) fn confirm_picker(&self) {
        let p = &self.pick;
        let i = p.sel.get();
        if let Some((_, scope, _)) = p.options.get().into_iter().find(|(j, ..)| *j == i) {
            self.add(&scope);
        }
    }
}

/// 分配视图正文：顶部「添加方案覆盖」「添加模式覆盖」+ (i)，下面是表格。
pub fn build_assign_body(ui: &CommentSettingsUi) -> Element {
    let (us, um) = (ui.clone(), ui.clone());
    let top = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(0, 8)
        .child(
            Element::button("添加方案覆盖…")
                .small()
                .outline_soft()
                .on_click(move |_| us.open_picker(PickKind::Schema)),
        )
        .child(
            Element::button("添加模式覆盖…")
                .small()
                .outline_soft()
                .on_click(move |_| um.open_picker(PickKind::Mode)),
        )
        .child(crate::widgets::info_badge(
            signal(ASSIGN_TIP.to_string()),
            |_| {},
        ));
    let u = ui.clone();
    let table = Element::host_signal(ui.view, move |lib: LibraryInfo| build_table(&u, &lib));
    Element::col()
        .width_match()
        .padding_xy(0, 2)
        .child(top)
        .child(Element::col().width_match().height(TABLE_H).child(table))
}

/// 列标题：作用范围 + [`Col::ALL`] 的顺序。
pub fn column_titles() -> Vec<String> {
    let mut out = vec!["作用范围".to_string()];
    out.extend(Col::ALL.iter().map(|c| c.title().to_string()));
    out
}

fn build_table(ui: &CommentSettingsUi, lib: &LibraryInfo) -> Element {
    let user = ui.assign.get();
    let rows = table_rows(&user, &lib.system_assign);
    let names = ui.names.borrow().clone();
    let columns: Vec<(String, f32)> = column_titles()
        .into_iter()
        .enumerate()
        .map(|(i, t)| (t, if i == 0 { SCOPE_W } else { 1.0 }))
        .collect();
    let body: Vec<Vec<Element>> = rows
        .iter()
        .map(|scope| {
            let mut cells = vec![scope_cell(ui, lib, &user, scope, &names)];
            for col in Col::ALL {
                cells.push(cell_dropdown(ui, lib, &user, scope, col));
            }
            cells
        })
        .collect();
    Element::table_custom(columns, body).fill()
}

fn scope_cell(
    ui: &CommentSettingsUi,
    lib: &LibraryInfo,
    user: &CommentAssign,
    scope: &Scope,
    names: &Names,
) -> Element {
    let label = scope_label(scope, names);
    let mut row = Element::row()
        .cross(Align::Center)
        .spacing(2)
        .padding_xy(8, 4)
        .child(
            Element::label(short_scope_label(&label))
                .font_size(12.5)
                .fg_role(Role::Text)
                .max_lines(1)
                .tooltip(label.clone())
                .weight(1.0),
        )
        .child(preview_button(lib, scope, &label));
    let (u, s) = (ui.clone(), scope.clone());
    row = if scope.user_added(&lib.system_assign) {
        row.child(
            Element::icon_button("×")
                .size(22, 22)
                .tooltip("删除这一行覆盖")
                .on_click(move |_| u.delete(&s)),
        )
    } else {
        row.child(
            Element::icon_button("↺")
                .size(22, 22)
                .tooltip("恢复出厂：清掉这一行自己改过的格")
                .enabled(has_user_cells(user, scope))
                .on_click(move |_| u.restore(&s)),
        )
    };
    row
}

/// 表格里的范围名去掉「方案 · 」「模式 · 」前缀（列窄），全名在悬停提示与预览标题里。
fn short_scope_label(full: &str) -> &str {
    full.strip_prefix("方案 · ")
        .or_else(|| full.strip_prefix("模式 · "))
        .unwrap_or(full)
}

/// 预览浮层里的一块：标题、候选文档、底色。
#[derive(Clone)]
struct PreviewBlock {
    caption: String,
    doc: RichDoc,
    bg: Color,
}

/// 一格实际落到的模板：`(显示名, 模板串)`；`none` 的串为空。
fn effective_template(lib: &LibraryInfo, scope: &Scope, col: Col) -> (String, String) {
    match lib.effective_of(scope, col) {
        Some((id, _)) if id == "none" => (lib.name_of(id), String::new()),
        Some((id, _)) => (lib.name_of(id), lib.text_of(id)),
        None => ("未知".into(), String::new()),
    }
}

fn preview_blocks(lib: &LibraryInfo, scope: &Scope) -> Vec<PreviewBlock> {
    use crate::dialogs::template_assist::comment_look;
    let mut out = Vec::new();
    for col in Col::ALL {
        let (name, text) = effective_template(lib, scope, col);
        let Some(look) = comment_look(&text) else {
            return Vec::new();
        };
        out.push(PreviewBlock {
            caption: format!("{} · {name}", col.title()),
            doc: if col.horizontal() {
                look.horizontal()
            } else {
                look.vertical()
            },
            bg: look.bg,
        });
    }
    out
}

/// 范围名右侧的预览图标：点开浮层，画这一行四格在样例候选上的效果（打开时才求值）。
fn preview_button(lib: &LibraryInfo, scope: &Scope, label: &str) -> Element {
    let open = signal(false);
    let blocks: Signal<Vec<PreviewBlock>> = signal(Vec::new());
    let empty = signal(false);
    let (lib, scope) = (lib.clone(), scope.clone());
    let block = |b: PreviewBlock| {
        Element::col()
            .width_match()
            .spacing(2)
            .child(
                Element::label(b.caption)
                    .font_size(11.5)
                    .fg_role(Role::TextMuted),
            )
            .child(
                Element::row()
                    .width_match()
                    .bg(b.bg)
                    .corner(4.0)
                    .padding_xy(10, 6)
                    .child(Element::rich(b.doc).weight(1.0).copy_menu(false)),
            )
    };
    let panel = Element::col()
        .width(POPUP_W)
        .padding(12)
        .spacing(8)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(8.0)
        .shadow(Shadow::new(0.0, 8.0, 24.0, Color::rgba(0, 0, 0, 90)))
        .child(
            Element::label(format!("预览：{label}"))
                .font_size(12.5)
                .font_weight(600)
                .fg_role(Role::Text),
        )
        .child(
            Element::col()
                .width_match()
                .spacing(8)
                .child(Element::host_signal(blocks, block)),
        )
        .child(
            Element::label("core 未连接或不支持预览")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .visible_when(move || empty.get()),
        );
    Element::stack()
        .size(22, 22)
        .clickable()
        .corner(4.0)
        .tooltip("预览这一行")
        .child(
            crate::icons::tinted_role(crate::icons::EYE, 14, Role::TextMuted).align(Align::Center),
        )
        .on_click(move |_| {
            if !open.get() {
                let b = preview_blocks(&lib, &scope);
                empty.set(b.is_empty());
                blocks.set(b);
            }
            open.set(!open.get());
        })
        .popup(open, panel)
}

fn cell_dropdown(
    ui: &CommentSettingsUi,
    lib: &LibraryInfo,
    user: &CommentAssign,
    scope: &Scope,
    col: Col,
) -> Element {
    let current = shown_choice(user, &lib.system_assign, scope, col);
    let options = cell_options(lib, scope, col, &current);
    let resolved = lib.effective_of(scope, col).map(|(id, _)| lib.name_of(id));
    let labels: Vec<String> = options
        .iter()
        .map(|c| choice_label(lib, c, resolved.as_deref()))
        .collect();
    let idx = options.iter().position(|c| *c == current).unwrap_or(0);
    // 格子窄，「同横排 · 拆字+拼音」常被截断：悬停给全名。
    let tip = labels.get(idx).cloned().unwrap_or_default();
    let items: Vec<windui::ui::select::DropdownItem> = labels
        .into_iter()
        .map(windui::ui::select::DropdownItem::new)
        .collect();
    let sel = signal(idx);
    let following = matches!(current, Choice::Follow | Choice::SameAsHorizontal);
    let mut dd = Element::dropdown_items(items, sel)
        .weight(1.0)
        .font_size(11.5)
        .tooltip(tip);
    if following {
        dd = dd.italic().opacity(0.6);
    }
    let (u, s) = (ui.clone(), scope.clone());
    let effect = crate::pages::dict::reactive::reactive_effect(
        move || sel.version(),
        move || {
            if let Some(c) = options.get(sel.get()) {
                u.choose(&s, col, c);
            }
        },
    );
    Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(1, 3)
        .child(dd)
        .child(effect)
}

/// 二级弹层：添加覆盖的选择框（单选，确认才加行）。
pub fn build_assign_subdialogs(ui: &CommentSettingsUi) -> Element {
    let p = ui.pick.clone();
    let (sel, options, ok) = (p.sel, p.options, p.ok);
    let list = Element::host_signal(options, move |(i, _, label): (usize, Scope, String)| {
        Element::row()
            .width_match()
            .padding_xy(4, 4)
            .child(Element::radio(label, sel, i))
    });
    let enable = crate::pages::dict::reactive::reactive_effect(
        move || sel.version().wrapping_add(options.version()),
        move || {
            let valid = options.with(|o| o.iter().any(|(j, ..)| *j == sel.get()));
            if ok.get() != valid {
                ok.set(valid);
            }
        },
    );
    let content = Element::col()
        .width_match()
        .padding_xy(18, 10)
        .spacing(4)
        .child(
            Element::label("只列出还没有覆盖的项；加上后各格先跟随下层，再按需改。")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(2),
        )
        .child(Element::col().width_match().child(list))
        .child(
            Element::label("都已经加过了。")
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .padding_xy(4, 8)
                .visible_when(move || options.with(Vec::is_empty)),
        )
        .child(enable);
    let u = ui.clone();
    crate::dialogs::field_dialogs::build_confirm_dialog_shell(
        p.visible,
        p.title,
        content,
        ok,
        move || u.confirm_picker(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::comment_settings::{add_row, delete_row, restore_factory, write_choice};
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
            "出厂行已在表里"
        );
        assert_eq!(
            table_rows(&user, &sys).iter().filter(|r| **r == s).count(),
            1
        );
        assert!(
            !keys(&picker_options(&names(), &user, &sys, PickKind::Schema)).contains(&s.key()),
            "加过的不再出现在选择框里"
        );
    }

    #[test]
    fn user_rows_delete_and_factory_rows_restore() {
        let sys = system();
        let mut user = CommentAssign::default();
        let url = Scope::Mode("url".into());
        let te = Scope::Mode("temp_english".into());
        add_row(&mut user, &sys, &url);
        write_choice(
            &mut user,
            &sys,
            &te,
            Col::DetailH,
            &Choice::Template("pinyin".into()),
        );
        assert!(url.user_added(&sys) && !te.user_added(&sys));
        assert!(!delete_row(&mut user, &sys, &te), "出厂行删不掉");
        assert!(delete_row(&mut user, &sys, &url));
        assert!(!table_rows(&user, &sys).contains(&url));
        assert!(has_user_cells(&user, &te));
        restore_factory(&mut user, &te);
        assert!(!has_user_cells(&user, &te));
        assert!(table_rows(&user, &sys).contains(&te), "恢复出厂后行还在");
        assert!(!Scope::Global.user_added(&sys));
    }

    #[test]
    fn columns_put_horizontal_first() {
        assert_eq!(
            column_titles(),
            ["作用范围", "默认·横", "默认·竖", "详细·横", "详细·竖"]
        );
    }

    #[test]
    fn picker_confirm_adds_selected_row_once() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let ui = CommentSettingsUi::new();
        ui.seed(&crate::config::Config::from_value(serde_json::json!({})));
        ui.open_picker(PickKind::Schema);
        ui.confirm_picker();
        assert!(ui.assign.get().schema.is_empty(), "没选就确认：不加");
        let first = ui.pick.options.get()[0].1.clone();
        ui.pick.sel.set(0);
        ui.confirm_picker();
        ui.confirm_picker();
        assert_eq!(ui.assign.get().schema.len(), 1);
        ui.open_picker(PickKind::Schema);
        assert!(
            ui.pick.options.get().iter().all(|(_, s, _)| *s != first),
            "重开选择框不再列出"
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }
}
