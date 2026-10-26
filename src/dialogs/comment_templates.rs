//! 「注释模板」对话框：模板库（左栏内置 / 自定义列表，右栏名称、模板串与候选窗预览）。
//!
//! 设计见 core 仓 `docs/design/candidate-comment-templates.md` §13.4。草稿与「候选注释」
//! 对话框共用一份（[`comment_settings::shared_ui`]）：分配表的选项就是这里的模板。
//!
//! # 选中与写回
//!
//! 左栏是 windui 列表行（[`ListRow`]，同 `Element::list_pill`），选中的是**行号**；真相是模板 id
//! （[`TplEditor::sel`]），行号只是它在当前列表里的位置，列表一变就按 id 重新定位。
//! 编辑框逐键写回选中的用户模板；切换选中走 [`CommentSettingsUi::select`]：先把编辑框写回
//! **旧**模板，再换 id、按新模板重新播种编辑框——三步同步做完，编辑框里的字不会串到新模板。

use windui::prelude::*;
use windui::ui::list::ListRow;

use wind_config::comment_lib::CommentTemplateDef;

use crate::config::Config;
use crate::dialogs::comment_settings::{
    self, CommentSettingsUi, LibraryInfo, Origin, TplInfo, unique_template_id,
};
use crate::state::AppState;

/// 左栏的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TplRow {
    pub id: String,
    pub label: String,
    /// 自定义组（`Origin::User`）；其余（内置与已修改的内置）归内置组。
    pub user: bool,
}

/// 列表行：内置组在前、自定义组在后（行号即此顺序）；覆盖内置的用户版标「已修改」。
pub fn tpl_rows(lib: &LibraryInfo) -> Vec<TplRow> {
    let row = |t: &TplInfo| {
        let mut label = t.display_name().to_string();
        if t.origin == Origin::UserOverridesBuiltin {
            label.push_str(" · 已修改");
        }
        TplRow {
            id: t.id.clone(),
            label,
            user: t.origin == Origin::User,
        }
    };
    let builtin = lib.templates.iter().filter(|t| t.origin != Origin::User);
    let user = lib.templates.iter().filter(|t| t.origin == Origin::User);
    builtin.chain(user).map(row).collect()
}

/// 左栏列表的一项：组标题、空组提示或一行模板（带行号）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ListItem {
    Header(&'static str),
    Empty,
    Row(usize, String),
}

/// 列表行 → 左栏各项：内置 / 自定义两组，各带标题，空组给一句提示。
fn list_items(rows: &[TplRow]) -> Vec<ListItem> {
    let mut out = Vec::new();
    for (title, user) in [("内置", false), ("自定义", true)] {
        out.push(ListItem::Header(title));
        let before = out.len();
        out.extend(
            rows.iter()
                .enumerate()
                .filter(|(_, r)| r.user == user)
                .map(|(i, r)| ListItem::Row(i, r.label.clone())),
        );
        if out.len() == before {
            out.push(ListItem::Empty);
        }
    }
    out
}

/// 模板库对话框的状态（挂在共用草稿 [`CommentSettingsUi`] 上）。
#[derive(Clone)]
pub struct TplEditor {
    pub visible: Signal<bool>,
    /// 选中的模板 id（真相）。
    pub(super) sel: Signal<String>,
    /// 选中行号（列表控件绑的就是它）；不在列表里时为 `usize::MAX`。
    sel_idx: Signal<usize>,
    rows: Signal<Vec<TplRow>>,
    /// 左栏各项（由 `rows` 派生，供 `host_signal` 逐项建）。
    items: Signal<Vec<ListItem>>,
    /// 选中模板的来源（`None` = 没选中），供按钮显隐与编辑框置灰每帧读，免得每帧克隆整份库。
    sel_origin: Signal<Option<Origin>>,
    sel_info: Signal<String>,
    pub(super) edit_name: Signal<String>,
    /// 模板串的**显示形态**（真换行显示成 `\n`，同悬停提示的段内容框），写回时还原。
    pub(super) edit_text: Signal<String>,
}

impl TplEditor {
    pub fn new() -> Self {
        Self {
            visible: signal(false),
            sel: signal(String::new()),
            sel_idx: signal(usize::MAX),
            rows: signal(Vec::new()),
            items: signal(Vec::new()),
            sel_origin: signal(None),
            sel_info: signal(String::new()),
            edit_name: signal(String::new()),
            edit_text: signal(String::new()),
        }
    }
}

impl CommentSettingsUi {
    /// 库视图变了：列表行按新库重算（没变就不动，免得逐键改名之外的刷新重建整列），
    /// 行号按选中 id 重新定位。
    pub(super) fn sync_tpl_rows(&self) {
        let rows = tpl_rows(&self.lib());
        let idx = rows
            .iter()
            .position(|r| r.id == self.tpl.sel.get())
            .unwrap_or(usize::MAX);
        if self.tpl.rows.with(|r| *r != rows) {
            self.tpl.items.set(list_items(&rows));
            self.tpl.rows.set(rows);
        }
        if self.tpl.sel_idx.get() != idx {
            self.tpl.sel_idx.set(idx);
        }
    }

    pub(super) fn selected_template(&self) -> Option<TplInfo> {
        let id = self.tpl.sel.get();
        self.lib().templates.into_iter().find(|t| t.id == id)
    }

    /// 选中另一个模板：编辑框先写回当前模板，再换选中、按新模板播种。
    pub(super) fn select(&self, id: &str) {
        if self.tpl.sel.get() == id {
            return;
        }
        self.commit_editor();
        self.tpl.sel.set(id.to_string());
        self.sync_tpl_rows();
        self.load_editor();
    }

    /// 列表点了第 `idx` 行。
    fn select_row(&self, idx: usize) {
        let id = self.tpl.rows.with(|r| r.get(idx).map(|r| r.id.clone()));
        if let Some(id) = id {
            self.select(&id);
        }
    }

    /// 选中的模板换了：编辑框、来源与说明按它重新播种。
    pub(super) fn load_editor(&self) {
        use crate::dialogs::tooltip_sections_dialog::escape_template;
        let t = self.selected_template();
        let info = match &t {
            None => "选左侧一个模板查看或编辑。".to_string(),
            Some(t) => match t.origin {
                Origin::Builtin => format!("内置模板 {}，只读；要改先「复制为自定义」。", t.id),
                Origin::User => format!("自定义模板 {}。", t.id),
                Origin::UserOverridesBuiltin => {
                    format!("内置模板 {} 已修改；「恢复内置」丢弃修改。", t.id)
                }
            },
        };
        self.tpl.sel_origin.set(t.as_ref().map(|t| t.origin));
        self.tpl.sel_info.set(info);
        let (name, text) = t
            .map(|t| (t.name, escape_template(&t.text)))
            .unwrap_or_default();
        if self.tpl.edit_name.get() != name {
            self.tpl.edit_name.set(name);
        }
        if self.tpl.edit_text.get() != text {
            self.tpl.edit_text.set(text);
        }
    }

    /// 编辑框改了：写进选中的用户模板（内置模板只读，不写）。
    pub(super) fn commit_editor(&self) {
        use crate::dialogs::tooltip_sections_dialog::unescape_template;
        let Some(t) = self.selected_template().filter(TplInfo::editable) else {
            return;
        };
        let (name, text) = (
            self.tpl.edit_name.get(),
            unescape_template(&self.tpl.edit_text.get()),
        );
        if t.name == name && t.text == text {
            return;
        }
        self.templates.update(|v| {
            if let Some(slot) = v.iter_mut().find(|u| u.id == t.id) {
                slot.name = name.clone();
                slot.text = text.clone();
            }
        });
        // 改名、改模板串不改任何格落到哪个 id，就地改库里那一条即可：逐键打字不打 RPC。
        self.view.update(|v| {
            if let Some(slot) = v
                .first_mut()
                .and_then(|lib| lib.templates.iter_mut().find(|u| u.id == t.id))
            {
                slot.name = name;
                slot.text = text;
            }
        });
        self.sync_tpl_rows();
    }

    fn taken_ids(&self) -> Vec<String> {
        self.lib().templates.into_iter().map(|t| t.id).collect()
    }

    pub(super) fn add_template(&self, base: &str, name: String, text: String) {
        self.commit_editor();
        let taken = self.taken_ids();
        let refs: Vec<&str> = taken.iter().map(String::as_str).collect();
        let id = unique_template_id(base, &refs);
        self.templates.update(|v| {
            v.push(CommentTemplateDef {
                id: id.clone(),
                name,
                text,
            })
        });
        self.tpl.sel.set(id);
        self.refresh();
        self.load_editor();
    }

    pub(super) fn copy_selected(&self) {
        self.commit_editor();
        if let Some(t) = self.selected_template() {
            self.add_template(
                &format!("{}_copy", t.id),
                format!("{} 副本", t.display_name()),
                t.text,
            );
        }
    }

    /// 恢复内置 / 删除自定义：都是删掉同 id 的用户模板。删除自定义时引用它的格一并清掉。
    pub(super) fn drop_user_template(&self) {
        let Some(t) = self.selected_template().filter(TplInfo::editable) else {
            return;
        };
        self.templates.update(|v| v.retain(|u| u.id != t.id));
        if t.origin == Origin::User {
            self.assign
                .update(|a| comment_settings::drop_references(a, &t.id));
            self.tpl.sel.set(String::new());
        }
        self.refresh();
        self.load_editor();
    }
}

/// 建对话框，返回打开它的信号。清单项是 `ui.comment.templates`。
pub fn build_dialog_button_templates(state: &AppState, cfg: &Config) -> Signal<bool> {
    let ui = comment_settings::shared_ui(state, cfg);
    let dlg = build_dialog(&ui);
    state.fields.dialogs.borrow_mut().push(dlg);
    ui.tpl.visible
}

/// 左栏列表高度（逻辑 px）。
const LIST_H: i32 = 430;

fn build_dialog(ui: &CommentSettingsUi) -> Element {
    let tpl = ui.tpl.clone();
    ui.sync_tpl_rows();
    ui.load_editor();

    let list = {
        let sel_idx = tpl.sel_idx;
        Element::host_signal(tpl.items, move |it: ListItem| list_item(it, sel_idx))
    };
    let pick = {
        let u = ui.clone();
        let idx = tpl.sel_idx;
        crate::pages::dict::reactive::reactive_effect(
            move || idx.version(),
            move || u.select_row(idx.get()),
        )
    };
    // 初次拉到库（`apply_reply` 补上默认选中）之类不经 `select` 的换选中：按新 id 播种。
    let load = {
        let u = ui.clone();
        let sel = tpl.sel;
        crate::pages::dict::reactive::reactive_effect(
            move || sel.version(),
            move || u.load_editor(),
        )
    };
    let commit = {
        let u = ui.clone();
        let (n, t) = (tpl.edit_name, tpl.edit_text);
        crate::pages::dict::reactive::reactive_effect(
            move || n.version().wrapping_add(t.version()),
            move || u.commit_editor(),
        )
    };

    let origin = tpl.sel_origin;
    let (un, uc, ud) = (ui.clone(), ui.clone(), ui.clone());
    let list_buttons = Element::row()
        .spacing(6)
        .child(
            Element::button("＋ 新建")
                .small()
                .outline_soft()
                .on_click(move |_| un.add_template("custom", "新模板".into(), String::new())),
        )
        .child(
            Element::button("复制")
                .small()
                .outline_soft()
                .on_click(move |_| uc.copy_selected())
                .enabled_when(move || origin.get().is_some()),
        )
        .child(
            Element::button("删除")
                .small()
                .outline_soft()
                .on_click(move |_| ud.drop_user_template())
                .enabled_when(move || origin.get() == Some(Origin::User)),
        );
    let left = Element::col()
        .width(210)
        .spacing(8)
        .child(Element::scroll().width_match().height(LIST_H).child(list))
        .child(list_buttons);

    let right = build_editor(ui);

    let content = Element::col()
        .width_match()
        .padding_xy(16, 10)
        .child(
            Element::row()
                .width_match()
                .spacing(16)
                .child(left)
                .child(right),
        )
        .child(pick)
        .child(load)
        .child(commit);
    crate::dialogs::field_dialogs::build_group_dialog_shell_dyn_sized(
        tpl.visible,
        signal("注释模板".to_string()),
        content,
        vec![
            Element::link("查看文档")
                .url(crate::dialogs::field_dialogs::COMMENT_DOC_URL)
                .font_size(12.0),
        ],
        780,
        520,
    )
}

/// 左栏的一项。模板行是 windui 列表行（同 `Element::list_pill`），行号按 [`tpl_rows`] 的顺序。
fn list_item(it: ListItem, sel_idx: Signal<usize>) -> Element {
    match it {
        ListItem::Header(title) => Element::label(title)
            .font_size(11.5)
            .font_weight(600)
            .fg_role(Role::TextSubtle)
            .padding_xy(8, 4),
        ListItem::Empty => Element::label("还没有")
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .padding_xy(16, 6),
        ListItem::Row(i, label) => Element::leaf()
            .widget(ListRow::new(label, sel_idx, i).pill())
            .width_match()
            .height(windui::theme::current().metrics.row_h),
    }
}

/// 右栏：说明、名称、模板串（插入颜色）、候选窗预览、内置模板的操作。
fn build_editor(ui: &CommentSettingsUi) -> Element {
    use crate::dialogs::template_assist::{
        TemplateField, comment_layout_preview, insert_color_button,
    };
    let tpl = &ui.tpl;
    let origin = tpl.sel_origin;
    let editable = move || origin.get().is_some_and(|o| o != Origin::Builtin);

    let info = Element::label_signal(tpl.sel_info)
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(2);

    let field = TemplateField::new(tpl.edit_text, true);
    let input = field.hook(Element::text_input(tpl.edit_text, "留空则不显示注释").width_match());
    let fields = Element::col()
        .width_match()
        .spacing(6)
        .child(crate::widgets::setting_row(
            "名称",
            None,
            Element::text_input(tpl.edit_name, "模板名称").width(220),
        ))
        .child(crate::widgets::setting_row(
            "模板串",
            Some("换行写 \\n"),
            insert_color_button(&field, "comment"),
        ))
        .child(input)
        .enabled_when(editable);

    let (u1, u2) = (ui.clone(), ui.clone());
    let actions = Element::row()
        .spacing(8)
        .child(
            Element::button("复制为自定义")
                .small()
                .outline_soft()
                .on_click(move |_| u1.copy_selected())
                .visible_when(move || origin.get() == Some(Origin::Builtin)),
        )
        .child(
            Element::button("恢复内置")
                .small()
                .outline_soft()
                .on_click(move |_| u2.drop_user_template())
                .visible_when(move || origin.get() == Some(Origin::UserOverridesBuiltin)),
        );

    Element::col()
        .weight(1.0)
        .spacing(10)
        .child(info)
        .child(actions)
        .child(fields)
        .child(comment_layout_preview(tpl.visible, field))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn mock_ui(cfg: Value) -> CommentSettingsUi {
        let ui = CommentSettingsUi::new();
        ui.seed(&Config::from_value(cfg));
        ui
    }

    fn with_mock(f: impl FnOnce()) {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        f();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 列表：内置组在前、自定义在后；覆盖内置的标「已修改」、留在内置组。
    #[test]
    fn rows_group_builtin_first_and_mark_overrides() {
        let t = |id: &str, origin| TplInfo {
            id: id.into(),
            name: id.to_uppercase(),
            text: String::new(),
            origin,
        };
        let lib = LibraryInfo {
            templates: vec![
                t("mine", Origin::User),
                t("pinyin", Origin::UserOverridesBuiltin),
                t("code_hint", Origin::Builtin),
            ],
            ..Default::default()
        };
        let rows = tpl_rows(&lib);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["pinyin", "code_hint", "mine"]);
        assert_eq!(rows[0].label, "PINYIN · 已修改");
        assert!(!rows[0].user && rows[2].user);
    }

    /// 点第二行：选中与编辑框换成第二个模板。
    #[test]
    fn clicking_second_row_switches_editor() {
        with_mock(|| {
            let ui = mock_ui(json!({}));
            ui.sync_tpl_rows();
            ui.load_editor();
            let rows = ui.tpl.rows.get();
            assert!(rows.len() >= 2);
            assert_eq!(ui.tpl.sel.get(), rows[0].id);
            assert_eq!(ui.tpl.sel_idx.get(), 0);

            ui.select_row(1);
            assert_eq!(ui.tpl.sel.get(), rows[1].id);
            assert_eq!(ui.tpl.sel_idx.get(), 1);
            let second = ui.selected_template().unwrap();
            assert_eq!(
                ui.tpl.edit_text.get(),
                crate::dialogs::tooltip_sections_dialog::escape_template(&second.text)
            );
        });
    }

    /// 编辑到一半切走：改动写回旧模板，不串到新模板；切回来还在。
    #[test]
    fn switching_selection_keeps_edit_on_the_old_template() {
        with_mock(|| {
            let ui = mock_ui(json!({ "ui": { "comment": { "templates": [
                { "id": "a", "name": "甲", "text": "${pinyin}" },
                { "id": "b", "name": "乙", "text": "${chaizi}" },
            ] } } }));
            ui.sync_tpl_rows();
            ui.select("a");
            // 不经逐键写回的 effect：模拟「打完字立刻点别的行」。
            ui.tpl.edit_text.set("${pinyin} 改".into());
            ui.select("b");
            assert_eq!(ui.tpl.edit_text.get(), "${chaizi}", "编辑框换成新模板");
            let t = ui.templates.get();
            assert_eq!(t.iter().find(|t| t.id == "a").unwrap().text, "${pinyin} 改");
            assert_eq!(t.iter().find(|t| t.id == "b").unwrap().text, "${chaizi}");
            ui.select("a");
            assert_eq!(ui.tpl.edit_text.get(), "${pinyin} 改");
        });
    }

    /// 新建落在自定义组并选中它，行号跟着定位。
    #[test]
    fn new_template_is_selected_in_custom_group() {
        with_mock(|| {
            let ui = mock_ui(json!({}));
            ui.sync_tpl_rows();
            ui.add_template("custom", "新模板".into(), String::new());
            let rows = ui.tpl.rows.get();
            let idx = ui.tpl.sel_idx.get();
            assert_eq!(rows[idx].id, "custom");
            assert!(rows[idx].user);
            assert_eq!(ui.tpl.edit_name.get(), "新模板");
        });
    }

    /// 模板库：内置只读、复制为自定义、编辑框显示 `\n` 而存真换行、恢复内置删掉同 id 用户项。
    #[test]
    fn template_library_edits_user_copies_only() {
        with_mock(|| {
            let ui = mock_ui(json!({}));
            ui.select("code_hint");
            ui.tpl.edit_text.set("改不动".into());
            ui.commit_editor();
            assert!(ui.templates.get().is_empty(), "内置模板只读");

            ui.copy_selected();
            let t = ui.templates.get();
            assert_eq!(t.len(), 1);
            assert_eq!(t[0].id, "code_hint_copy");
            assert_eq!(ui.tpl.sel.get(), "code_hint_copy");
            ui.tpl.edit_text.set("${pinyin}\\n${chaizi}".into());
            ui.commit_editor();
            assert_eq!(ui.templates.get()[0].text, "${pinyin}\n${chaizi}");

            // 同 id 覆盖内置 → 已修改；恢复内置即删掉用户项。
            ui.templates.update(|v| {
                v.push(CommentTemplateDef {
                    id: "pinyin".into(),
                    name: "拼音".into(),
                    text: "x".into(),
                })
            });
            ui.refresh();
            ui.select("pinyin");
            assert_eq!(
                ui.selected_template().map(|t| t.origin),
                Some(Origin::UserOverridesBuiltin)
            );
            ui.drop_user_template();
            assert!(ui.templates.get().iter().all(|t| t.id != "pinyin"));
            assert_eq!(
                ui.selected_template().map(|t| t.origin),
                Some(Origin::Builtin)
            );
        });
    }

    /// 编辑框的转义往返：带 `\\`、`\t`、不成对 `\x`、真换行的模板，打开不改就不脏；
    /// 手打 `\n` 存真换行；改名就地更新库视图与列表行（不等 RPC）。
    #[test]
    fn editor_escape_round_trip_is_not_dirty() {
        with_mock(|| {
            let raws = ["a\\b\tc\\x", "${pinyin}\n${chaizi}", "\\n 字面", "末尾\\"];
            let templates: Vec<Value> = raws
                .iter()
                .enumerate()
                .map(|(i, t)| json!({ "id": format!("t{i}"), "name": "n", "text": t }))
                .collect();
            let cfg = json!({ "ui": { "comment": { "templates": templates } } });
            let ui = mock_ui(cfg.clone());
            let key = comment_settings::TEMPLATES_KEY;
            let base = ui.value_of(key);
            assert_eq!(Some(&base), Config::from_value(cfg).get_path(key));
            for i in 0..raws.len() {
                ui.select(&format!("t{i}"));
                ui.commit_editor();
                assert_eq!(ui.value_of(key), base, "t{i} 打开不改却变了");
            }
            assert_eq!(
                ui.tpl.edit_text.get(),
                crate::dialogs::tooltip_sections_dialog::escape_template(raws[3])
            );

            ui.select("t1");
            assert_eq!(ui.tpl.edit_text.get(), "${pinyin}\\n${chaizi}");
            ui.tpl.edit_name.set("改名".into());
            ui.commit_editor();
            assert!(
                ui.lib()
                    .templates
                    .iter()
                    .any(|t| t.id == "t1" && t.name == "改名")
            );
            assert!(ui.tpl.rows.get().iter().any(|r| r.label == "改名"));
        });
    }
}
