//! 词库页浮层对话框 — 破坏性操作确认（删除/清空/恢复默认）。
//!
//! 挂到根窗口（见 `shell::build`）以获得全窗模态遮罩 + 居中，模式同 `schema_manager`。
//! 加/改词对话框在阶段 5 追加。

use windui::prelude::*;

use crate::widgets::flat_panel;

use super::state::DictManagerState;

/// 构建词库页所有浮层对话框：确认 + 加/改词 + 候选调整添加 + 系统短语只读查看（叠在一个 stack）。
pub fn build_dict_dialogs(mgr: &DictManagerState) -> Element {
    Element::stack()
        .fill()
        .child(build_confirm_dialog(mgr))
        .child(build_edit_dialog(mgr))
        .child(build_shadow_dialog(mgr))
        .child(build_view_dialog(mgr))
        .child(build_export_dialog(mgr))
        .child(build_import_dialog(mgr))
}

/// 数据类型勾选行（导入导出共用；仅当前方案域适用的类型显示）。
fn io_type_row(mgr: &DictManagerState) -> Element {
    let cb = |label: &'static str, sig: Signal<bool>, key: &'static str, m: DictManagerState| {
        Element::checkbox(label, sig)
            .small()
            .on_toggle(move |_| sig.set(!sig.get()))
            .visible_when(move || m.io_type_applicable(key))
    };
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(18)
        .child(cb("用户词库", mgr.io_user_words, "userWords", mgr.clone()))
        .child(cb("临时词库", mgr.io_temp_words, "tempWords", mgr.clone()))
        .child(cb("词频", mgr.io_freq, "freq", mgr.clone()))
        .child(cb("候选调整", mgr.io_shadow, "shadow", mgr.clone()))
}

/// 导出对话框（方案级）：标题含方案说明 + 数据类型勾选 + 导出到文件。
fn build_export_dialog(mgr: &DictManagerState) -> Element {
    let exp = mgr.clone();
    let cancel = mgr.clone();
    let content = flat_panel()
        .width(440)
        .padding(20)
        .spacing(12)
        .child(
            Element::label_rc(mgr.io_title)
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(24)
                .width_match(),
        )
        .child(Element::divider())
        .child(
            Element::label("选择要导出的数据类型")
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width_match(),
        )
        .child(io_type_row(mgr))
        .child(
            Element::label("勾选的类型将导出为一个 .wdict 文件")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(1),
        )
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("关闭")
                        .small()
                        .outline_soft()
                        .on_click(move |_| cancel.cancel_export()),
                )
                .child(
                    Element::button("导出到文件…")
                        .small()
                        .on_click(move |ctx| exp.do_export_io(ctx)),
                ),
        );
    Element::dialog(mgr.export_visible, content)
}

/// 导入对话框（方案级）：标题含方案说明 + 数据类型勾选（选文件后自动勾选文件所含段）+
/// 选文件·自动识别·只显数量预览·合并/替换（不兼容或未预览时禁用）。
fn build_import_dialog(mgr: &DictManagerState) -> Element {
    let pick = mgr.clone();
    let merge = mgr.clone();
    let replace = mgr.clone();
    let merge_ready = mgr.clone();
    let replace_ready = mgr.clone();
    let cancel = mgr.clone();
    let file_row = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::button("选择文件…")
                .small()
                .outline_soft()
                .on_click(move |ctx| pick.pick_import_file(ctx)),
        )
        .child(
            Element::label_rc(mgr.import_file_name)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .weight(1.0)
                .max_lines(1),
        );
    let content = flat_panel()
        .width(440)
        .padding(20)
        .spacing(12)
        .child(
            Element::label_rc(mgr.io_title)
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(24)
                .width_match(),
        )
        .child(Element::divider())
        .child(
            Element::label("从 WindDict / Rime / TSV 文件导入（按内容自动识别）")
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(1),
        )
        .child(file_row)
        .child(io_type_row(mgr))
        .child(
            Element::label_rc(mgr.import_preview_msg)
                .font_size(13.0)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(5),
        )
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("关闭")
                        .small()
                        .outline_soft()
                        .on_click(move |_| cancel.cancel_import()),
                )
                .child(
                    Element::button("替换导入")
                        .small()
                        .danger()
                        .enabled_when(move || replace_ready.import_ready.get())
                        .on_click(move |_| replace.confirm_import("replace")),
                )
                .child(
                    Element::button("合并导入")
                        .small()
                        .enabled_when(move || merge_ready.import_ready.get())
                        .on_click(move |_| merge.confirm_import("merge")),
                ),
        );
    Element::dialog(mgr.import_confirm_visible, content)
}

/// 加/改词对话框（用户词库 / 短语）：编码/缩写（+出码）· 词条/短语 · 权重。
/// 标签与标题随类别经 `label_rc` 响应式切换；保存逻辑见 `DictManagerState::submit_word`。
/// 短语专用的「次序」字段（用户词库无此维度，整块隐藏）。
///
/// 附一行说明：主程序的短语层排序是「weight 降序 → position 升序」，**权重不同时
/// position 完全不参与比较**。不写清楚的话，用户会以为它是全局排序位而反复调它却没反应。
fn position_field(mgr: &DictManagerState) -> Element {
    let vis = mgr.clone();
    let is_phrase = move || {
        matches!(
            vis.current_category(),
            super::spec::WordCategory::PhraseUser
        )
    };
    let hint_vis = is_phrase.clone();
    Element::col()
        .width_match()
        .spacing(4)
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(
                    Element::label("次序")
                        .font_size(12.5)
                        .fg_role(Role::TextMuted)
                        .width(48),
                )
                .child(
                    Element::text_input(mgr.edit_position, "")
                        .width(100)
                        .height(34),
                ),
        )
        .child(
            Element::label("仅在同编码、同权重时决定先后（数值小的靠前）")
                .font_size(11.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .padding_xy(56, 0)
                .visible_when(hint_vis),
        )
        .visible_when(is_phrase)
}

fn build_edit_dialog(mgr: &DictManagerState) -> Element {
    let submit = mgr.clone();
    let cancel = mgr.clone();
    let encode = mgr.clone();
    let enc_vis = mgr.clone();

    let field = |label: Signal<String>, input: Element| {
        Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(8)
            .child(
                Element::label_rc(label)
                    .font_size(12.5)
                    .fg_role(Role::TextMuted)
                    .width(48),
            )
            .child(input)
    };

    let code_input = Element::row()
        .weight(1.0)
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::text_input(mgr.edit_code, "")
                .height(34)
                .weight(1.0),
        )
        .child(
            Element::button("出码")
                .small()
                .outline_soft()
                .on_click(move |_| encode.encode_current())
                .visible_when(move || enc_vis.current_spec().ops.encode),
        );

    let is_phrase = mgr.clone();
    let is_word = mgr.clone();
    let text_multi = Element::text_input(mgr.edit_text, "")
        .multiline()
        .wrap(true)
        .height(88)
        .weight(1.0)
        .visible_when(move || {
            matches!(
                is_phrase.current_category(),
                super::spec::WordCategory::PhraseSystem | super::spec::WordCategory::PhraseUser
            )
        });
    let text_single = Element::text_input(mgr.edit_text, "")
        .height(34)
        .weight(1.0)
        .visible_when(move || {
            !matches!(
                is_word.current_category(),
                super::spec::WordCategory::PhraseSystem | super::spec::WordCategory::PhraseUser
            )
        });
    let text_field = Element::row()
        .weight(1.0)
        .cross(Align::Center)
        .child(text_multi)
        .child(text_single);

    let body = Element::col()
        .width_match()
        .padding_xy(18, 12)
        .spacing(10)
        .child(field(mgr.edit_l_code, code_input))
        .child(field(mgr.edit_l_text, text_field))
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(
                    Element::label("权重")
                        .font_size(12.5)
                        .fg_role(Role::TextMuted)
                        .width(48),
                )
                .child(
                    Element::text_input(mgr.edit_weight, "")
                        .width(100)
                        .height(34),
                ),
        )
        .child(position_field(mgr));

    let footer = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 12)
        .spacing(8)
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("取消")
                .small()
                .outline_soft()
                .on_click(move |_| cancel.cancel_edit()),
        )
        .child(
            Element::button("确定")
                .small()
                .on_click(move |_| submit.submit_word()),
        );

    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label_rc(mgr.edit_title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(footer);
    Element::dialog(mgr.edit_visible, frame)
}

/// 候选调整手动添加：编码 / 词条 / 类型(固定位置|隐藏) / 目标位置(仅固定位置)。
fn build_shadow_dialog(mgr: &DictManagerState) -> Element {
    let submit = mgr.clone();
    let cancel = mgr.clone();
    let pos_enabled = mgr.clone();

    let field = |label: &str, input: Element| {
        Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(8)
            .child(
                Element::label(label)
                    .font_size(12.5)
                    .fg_role(Role::TextMuted)
                    .width(64),
            )
            .child(input)
    };

    let body = Element::col()
        .width_match()
        .padding_xy(18, 12)
        .spacing(10)
        .child(field(
            "编码",
            Element::text_input(mgr.shadow_code, "")
                .height(34)
                .weight(1.0),
        ))
        .child(field(
            "词条",
            Element::text_input(mgr.shadow_word, "")
                .height(34)
                .weight(1.0),
        ))
        .child(field(
            "类型",
            Element::segmented(
                vec!["固定位置".to_string(), "隐藏".to_string()],
                mgr.shadow_type,
            )
            .width(200),
        ))
        .child(field(
            "目标位置",
            Element::text_input(mgr.shadow_pos, "")
                .width(100)
                .height(34)
                .enabled_when(move || pos_enabled.shadow_type.get() == 0),
        ));

    let footer = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 12)
        .spacing(8)
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("取消")
                .small()
                .outline_soft()
                .on_click(move |_| cancel.cancel_shadow_add()),
        )
        .child(
            Element::button("确定")
                .small()
                .on_click(move |_| submit.submit_shadow()),
        );

    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label_rc(mgr.shadow_title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(footer);
    Element::dialog(mgr.shadow_add_visible, frame)
}

/// 系统短语只读查看对话框：缩写 / 内容 / 权重用 text_input 展示（可选中复制），仅"关闭"按钮，不写任何数据。
fn build_view_dialog(mgr: &DictManagerState) -> Element {
    let cancel = mgr.clone();

    let field = |label: &'static str, input: Element| {
        Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(8)
            .child(
                Element::label(label)
                    .font_size(12.5)
                    .fg_role(Role::TextMuted)
                    .width(48),
            )
            .child(input)
    };

    let body = Element::col()
        .width_match()
        .padding_xy(18, 12)
        .spacing(10)
        .child(field(
            "编码",
            Element::text_input(mgr.view_code, "").weight(1.0),
        ))
        .child(field(
            "内容",
            Element::text_input(mgr.view_text, "")
                .multiline()
                .wrap(true)
                .height(88)
                .weight(1.0),
        ))
        .child(field(
            "权重",
            Element::text_input(mgr.view_weight, "").width(100),
        ));

    let footer = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 12)
        .spacing(8)
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("关闭")
                .small()
                .outline_soft()
                .on_click(move |_| cancel.cancel_view()),
        );

    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("查看系统短语")
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(footer);
    Element::dialog(mgr.view_visible, frame)
}

/// 破坏性操作二次确认：正文由 `confirm_msg` 驱动，确认执行 `confirm_pending`。
fn build_confirm_dialog(mgr: &DictManagerState) -> Element {
    let confirm = mgr.clone();
    let cancel = mgr.clone();
    let content = flat_panel()
        .width(380)
        .padding(20)
        .spacing(14)
        .child(
            Element::label("确认操作")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(24)
                .width_match(),
        )
        .child(Element::divider())
        .child(
            Element::label_rc(mgr.confirm_msg)
                .font_size(13.0)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(4),
        )
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| cancel.cancel_confirm()),
                )
                .child(
                    Element::button("确定")
                        .small()
                        .danger()
                        .on_click(move |_| confirm.confirm_pending()),
                ),
        );
    Element::dialog(mgr.confirm_visible, content)
}
