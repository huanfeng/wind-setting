//! 「正在下载方案包」对话框：协议导入（`windinput://import/schema`）下载期间的出口。
//!
//! 协议导入由浏览器唤起，设置窗上没有别的地方可挂取消按钮；而包最大 64 MB，慢网下
//! 要等好一会儿。框收起（取消按钮、ESC、下载完成）即取消在途任务，完成时由
//! [`AppState::start_schema_download`] 先收本框再弹导入确认框。

use std::cell::RefCell;
use std::rc::Rc;

use windui::prelude::*;

use crate::state::AppState;

pub fn build_schema_download_dialog(app_state: &Rc<RefCell<AppState>>) -> Element {
    let visible = app_state.borrow().schema_download_visible;
    let cancel_state = app_state.clone();
    let effect_state = app_state.clone();

    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("下载方案包")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label("正在从官方站点下载方案包，完成后会弹出导入确认。")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3)
                .padding_xy(18, 12),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| cancel_state.borrow().cancel_schema_download()),
                ),
        )
        // 框一收就取消在途下载。挂在可见性信号上而不是取消按钮上：ESC 也能收框。
        .child(crate::pages::dict::reactive::reactive_effect(
            move || visible.version(),
            move || {
                if !visible.get() {
                    effect_state.borrow().cancel_schema_download();
                }
            },
        ));

    Element::dialog(visible, frame)
}
