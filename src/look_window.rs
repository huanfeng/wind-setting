//! 「界面细节」试验子窗 — 与主窗同进程的非模态窗，做法同 [`crate::compat_window`]。
//!
//! 分段控件直接绑 `AppState::look` 的信号：主窗各处 `visible_when` 读的是同一组，
//! 改即生效，主窗不重建；持久化由主窗里的 `look_watcher` 负责。
//!
//! 入口：Dev 版底栏图标 + Ctrl+F8；发布版只有 Ctrl+F8。
//! **过渡代码**，选项全部定案后删除（见 docs/design/settings-look-prefs.md §9）。

use std::cell::RefCell;
use std::rc::Rc;

use windui::core::EventCtx;
use windui::prelude::*;

use crate::look::{LookSignals, OPTIONS};
use crate::state::AppState;

/// 单例键：再按 Ctrl+F8 只把已开着的那个激活到前台。
pub(crate) const SINGLE_KEY: &str = "look_prefs";

pub fn open(ctx: &mut EventCtx, app: &Rc<RefCell<AppState>>) {
    let look = app.borrow().look;
    let app_c = app.clone();
    ctx.open_window(
        Window::new("清风输入法 · 界面细节", 460, 430)
            .single(SINGLE_KEY)
            .frameless(true)
            .centered(true)
            // Ctrl+W 关窗：`App::on_shortcut` 只归主窗，子窗要自己接。
            .on_shortcut(|ctx, ev| {
                if crate::shortcuts::is_close_window(&ev) {
                    ctx.request_close();
                    true
                } else {
                    false
                }
            })
            .content(move || build_content(look, &app_c)),
    );
}

fn build_content(look: LookSignals, app: &Rc<RefCell<AppState>>) -> Element {
    let rows = OPTIONS
        .iter()
        .enumerate()
        .map(|(i, o)| {
            crate::widgets::setting_row(
                o.title,
                None,
                Element::segmented(o.labels.to_vec(), look.0[i]),
            )
        })
        .collect();
    Element::col()
        .fill()
        .bg_role(Role::SurfaceAlt)
        .child(crate::shell::build_titlebar("界面细节"))
        // 本窗也挂一份落盘哨兵：主窗关着时切的选项要靠它写进 [ui.look]（见 `look_watcher`）。
        .child(crate::shell::look_watcher(app))
        .child(
            Element::col()
                .width_match()
                .weight(1.0)
                .padding_xy(16, 12)
                .spacing(10)
                .child(crate::widgets::card(rows))
                .child(
                    Element::label("试验选项：用来比较几种界面写法，选定后会移除。改动即时生效。")
                        .font_size(12.0)
                        .fg_role(Role::TextMuted)
                        .width_match()
                        .max_lines(2),
                ),
        )
}
