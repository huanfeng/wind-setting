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
        // 整体深色 bg 衬托内容区的 surface 卡片，形成层次（对齐 ime.rs）。
        .fill()
        .bg_role(Role::Bg)
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

    // 明/暗切换：用随状态更新的 emoji 图标（暗色显示 ☀ 点击转亮；亮色显示 🌙 点击转暗）。
    // 两个按钮经 visible_when 互斥，套 stack 重叠占位避免布局抖动。
    let th_to_light = th.clone();
    let d_to_light = dark;
    let to_light = Element::button("☀")
        .small()
        .neutral()
        .on_click(move |_| {
            d_to_light.set(false);
            th_to_light.set(theme_for(false));
        })
        .visible_when(move || d_to_light.get());
    let th_to_dark = th.clone();
    let d_to_dark = dark;
    let to_dark = Element::button("🌙")
        .small()
        .neutral()
        .on_click(move |_| {
            d_to_dark.set(true);
            th_to_dark.set(theme_for(true));
        })
        .visible_when(move || !d_to_dark.get());
    let theme_btn = Element::stack().child(to_light).child(to_dark);

    // 保存：借 mut 跑写回器 + diff + setItems。
    let app_save = app.clone();
    let save_btn = Element::button("保存").small().on_click(move |_| {
        if let Ok(mut s) = app_save.try_borrow_mut() {
            s.save();
        }
    });

    Element::row()
        .width_match()
        .height(44)
        .cross(Align::Center)
        .padding_xy(14, 0)
        .spacing(10)
        // 标题栏作为窗口 chrome 置于 surface 上，与下方 bg 内容区分层。
        .bg_role(Role::Surface)
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
        // 图标随主题明暗（否则默认深色字在暗色标题栏上不可见）。
        .child(Element::window_button(WindowButtonKind::Minimize).fg_role(Role::Text))
        .child(Element::window_button(WindowButtonKind::Maximize).fg_role(Role::Text))
        .child(Element::window_button(WindowButtonKind::Close).fg_role(Role::Text))
}
