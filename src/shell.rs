//! 应用外壳 — 无边框窗口 + 自定义标题栏 + 8 Tab 导航 + 运行时明暗换肤。
//!
//! 标题栏：渐变 Logo + 标题（可拖动）+ 状态 + 明/暗切换 + 保存 + 窗口按钮。
//! 明暗切换通过 `ThemeHandle::set(theme_for(dark))` 整窗换肤（`*_role` 节点自动跟随）。

use std::cell::RefCell;
use std::rc::Rc;

use windui::prelude::*;

use crate::dialogs::schema_manager::build_schema_manager_dialog;
use crate::pages;
use crate::state::AppState;
use crate::theme_def::{ACCENT, theme_for};

/// 构建整窗 UI。`app` 为共享状态，`th` 为换肤句柄。
pub fn build(app: &Rc<RefCell<AppState>>, th: ThemeHandle) -> Element {
    // 借用一次构建页面与对话框，随后释放，便于保存闭包再借 mut。
    let (tabs_vec, tab_sig, dark, status, dialog) = {
        let st = app.borrow();
        (
            pages::all_tabs(&st),
            st.tab,
            st.dark,
            st.status,
            build_schema_manager_dialog(&st.schema_mgr),
        )
    };

    let titlebar = build_titlebar(app, th, dark, status);
    let tabs = Element::tabs(tab_sig, tabs_vec).weight(1.0);

    let main_col = Element::col()
        .fill()
        .child(titlebar)
        .child(Element::divider())
        .child(tabs);

    Element::stack()
        .fill()
        .bg_role(Role::Surface)
        .child(main_col)
        .child(dialog)
}

/// 自定义标题栏（无边框窗口的可拖动区 + 操作 + 窗口按钮）。
fn build_titlebar(
    app: &Rc<RefCell<AppState>>,
    th: ThemeHandle,
    dark: Signal<bool>,
    status: Signal<String>,
) -> Element {
    // 渐变 Logo（圆角块 + 白字）。
    let logo = Element::stack()
        .size(26, 26)
        .child(
            Element::leaf()
                .fill()
                .corner(7.0)
                .bg_gradient(Gradient::linear(
                    (0.0, 0.0),
                    (1.0, 1.0),
                    vec![(0.0, Color::hex(0x2563EB)), (1.0, Color::hex(ACCENT))],
                )),
        )
        .child(
            Element::label("清")
                .fill()
                .font_size(13.0)
                .fg(Color::WHITE)
                .text_align(Align::Center),
        );

    // 明/暗切换：翻转 dark 并换肤。
    let theme_btn = Element::button("明 / 暗")
        .small()
        .neutral()
        .on_click(move |_| {
            let d = !dark.get();
            dark.set(d);
            th.set(theme_for(d));
        });

    // 保存：借 mut 跑写回器 + diff + setItems。
    let app_save = app.clone();
    let save_btn = Element::button("保存").small().on_click(move |_| {
        if let Ok(mut s) = app_save.try_borrow_mut() {
            s.save();
        }
    });

    Element::row()
        .width_match()
        .height(40)
        .cross(Align::Center)
        .padding_xy(12, 0)
        .spacing(10)
        .window_drag()
        .child(logo)
        .child(
            Element::label("清风输入法 · 设置")
                .font_size(13.0)
                .font_weight(600)
                .fg_role(Role::Text),
        )
        .child(Element::leaf().weight(1.0))
        .child(
            Element::label_rc(status)
                .font_size(11.0)
                .fg_role(Role::TextMuted)
                .max_lines(1),
        )
        .child(theme_btn)
        .child(save_btn)
        .child(Element::leaf().width(8))
        .child(Element::window_button(WindowButtonKind::Minimize))
        .child(Element::window_button(WindowButtonKind::Maximize))
        .child(Element::window_button(WindowButtonKind::Close))
}
