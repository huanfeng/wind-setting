//! 「应用兼容」页的三个对话框：新增规则、全部还原确认、导入预览。挂在设置窗根层（全窗遮罩）。

use windui::core::EventCtx;
use windui::prelude::*;

use super::actions::IMPORT_MODES;
use super::state::CompatState;
use crate::pages::dict::reactive::reactive_effect;

/// 返回要挂到根层的所有元素（含一个切换导入方式时重算预览的 effect）。
pub fn build_dialogs(mgr: &CompatState) -> Vec<Element> {
    vec![
        build_add_dialog(mgr),
        build_reset_dialog(mgr),
        build_import_dialog(mgr),
        import_mode_effect(mgr),
    ]
}

fn footer(buttons: Vec<Element>) -> Element {
    let mut row = Element::row()
        .width_match()
        .spacing(8)
        .child(Element::flex_spacer());
    for b in buttons {
        row = row.child(b);
    }
    row
}

fn build_add_dialog(mgr: &CompatState) -> Element {
    let (close, cancel, ok, submit) = (mgr.clone(), mgr.clone(), mgr.clone(), mgr.clone());
    let d = &mgr.dlg;
    let body = Element::col()
        .width_match()
        .spacing(8)
        .child(
            Element::label(
                "填写软件的进程名（如 Feishu.exe，不区分大小写）。\n\
                 可在任务管理器的「详细信息」页查到。新规则在设置任意一项后才会写入。",
            )
            .font_size(12.5)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(4),
        )
        .child(
            Element::text_input(d.add_name, "进程名，如 Feishu.exe")
                .width_match()
                .on_submit(move |_| submit.confirm_add()),
        )
        .child(
            Element::label_signal(d.add_error)
                .font_size(12.0)
                .fg_role(Role::Danger)
                .width_match()
                .max_lines(2),
        );
    let foot = footer(vec![
        Element::button("取消")
            .small()
            .outline_soft()
            .on_click(move |_| cancel.dlg.add_visible.set(false)),
        Element::button("新增")
            .small()
            .on_click(move |_| ok.confirm_add()),
    ]);
    Element::dialog_panel(
        d.add_visible,
        "新增规则",
        420,
        move |_: &mut EventCtx| close.dlg.add_visible.set(false),
        body,
        foot,
    )
}

fn build_reset_dialog(mgr: &CompatState) -> Element {
    let (close, cancel, ok) = (mgr.clone(), mgr.clone(), mgr.clone());
    let body = Element::label(
        "将丢弃你对全部规则做过的改动（包括新增、禁用和逐项修改），恢复为出厂设置。\n\
         原有的用户配置文件会先自动备份。",
    )
    .font_size(13.0)
    .fg_role(Role::Text)
    .width_match()
    .max_lines(5);
    let foot = footer(vec![
        Element::button("取消")
            .small()
            .outline_soft()
            .on_click(move |_| cancel.dlg.reset_visible.set(false)),
        Element::button("全部还原")
            .small()
            .danger()
            .on_click(move |_| ok.confirm_reset_all()),
    ]);
    Element::dialog_panel(
        mgr.dlg.reset_visible,
        "全部还原",
        420,
        move |_: &mut EventCtx| close.dlg.reset_visible.set(false),
        body,
        foot,
    )
}

fn build_import_dialog(mgr: &CompatState) -> Element {
    let (close, cancel, ok) = (mgr.clone(), mgr.clone(), mgr.clone());
    let d = &mgr.dlg;
    let body = Element::col()
        .width_match()
        .spacing(10)
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(10)
                .child(
                    Element::label("导入方式")
                        .font_size(13.0)
                        .fg_role(Role::Text),
                )
                .child(Element::segmented(
                    IMPORT_MODES.iter().map(|m| m.0).collect::<Vec<_>>(),
                    d.import_mode,
                )),
        )
        .child(
            Element::scroll()
                .width_match()
                .height(220)
                .border_role(Role::Border, 1)
                .corner(6.0)
                .padding_xy(10, 8)
                .child(
                    Element::label_signal(d.import_summary)
                        .font_size(12.5)
                        .fg_role(Role::Text)
                        .width_match()
                        .max_lines(60),
                ),
        );
    let can_apply = d.import_can_apply;
    let foot = footer(vec![
        Element::button("取消")
            .small()
            .outline_soft()
            .on_click(move |_| cancel.cancel_import()),
        Element::button("导入")
            .small()
            .enabled_when(move || can_apply.get())
            .on_click(move |_| ok.confirm_import()),
    ]);
    Element::dialog_panel(
        d.import_visible,
        "导入预览",
        520,
        move |_: &mut EventCtx| close.cancel_import(),
        body,
        foot,
    )
}

/// 对话框开着时切换合并 / 替换，预览要跟着重算。
fn import_mode_effect(mgr: &CompatState) -> Element {
    let m = mgr.clone();
    let mode = mgr.dlg.import_mode;
    reactive_effect(
        move || mode.version(),
        move || {
            if m.dlg.import_visible.get() {
                m.refresh_import_preview();
            }
        },
    )
}
