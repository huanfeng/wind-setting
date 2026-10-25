//! 「界面细节」子窗 — 与主窗同进程的非模态窗，做法同 [`crate::compat_window`]。
//!
//! 两件事：
//! 1. **界面外观试验选项**（`crate::look`）：分段控件直接绑 `AppState::look` 的信号，
//!    主窗各处 `visible_when` 读的是同一组，改即生效；持久化由 `look_watcher` 负责。
//! 2. **设置项隐藏**（`crate::user_hide`）：开关 + 「显示已隐藏的设置」+「恢复默认」。
//!    用户隐藏走运行期判据（`visible_when` 读 `UserHideStore`），改完即刻生效，无需重开窗口。
//!
//! 入口：主窗底栏右键菜单「界面细节…」（只在底栏，侧栏不放）。
//!
//! 外观试验选项是**过渡代码**，全部定案后删除（见 docs/design/settings-look-prefs.md §9）；
//! 设置项隐藏是正式功能，届时整体搬到别处（入口由同一份菜单给出，迁移只改入口）。

use std::cell::RefCell;
use std::rc::Rc;

use windui::core::EventCtx;
use windui::prelude::*;

use crate::look::{LookSignals, OPTIONS};
use crate::state::AppState;

/// 单例键：再点一次入口只把已开着的那个激活到前台。
pub(crate) const SINGLE_KEY: &str = "look_prefs";

pub fn open(ctx: &mut EventCtx, app: &Rc<RefCell<AppState>>) {
    let look = app.borrow().look;
    let app_c = app.clone();
    ctx.open_window(
        Window::new("清风输入法 · 界面细节", 480, 620)
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
            Element::scroll().width_match().weight(1.0).child(
                Element::col()
                    .width_match()
                    .padding_xy(16, 12)
                    .spacing(10)
                    .child(crate::widgets::section("界面外观", rows))
                    .child(
                        Element::label(
                            "试验选项：用来比较几种界面写法，选定后会移除。改动即时生效。",
                        )
                        .font_size(12.0)
                        .fg_role(Role::TextMuted)
                        .width_match()
                        .max_lines(2),
                    )
                    .child(build_hide_section(app)),
            ),
        )
}

/// 「设置项精简」一节：开启开关 + 逃生口 + 恢复默认 + 当前条数。
///
/// 三个控件都直接读写 [`crate::user_hide::UserHideStore`]（它自己落盘），不碰 core 配置，
/// 也不走 `FieldRegistry`——这份配置是设置程序自己的运行偏好，与输入法功能无关。
fn build_hide_section(app: &Rc<RefCell<AppState>>) -> Element {
    let store = app.borrow().user_hide_store.clone();
    let version = store.version();

    let enabled_on = signal(store.enabled());
    let show_on = signal(store.show_hidden());
    let summary = signal(hide_summary(&store));
    let errors = signal(hide_errors_text(&store));

    // 开关的初值与 store 保持同步：写盘失败时 store 会回滚，界面得跟着退回去。
    let (st_eff, ver_eff) = (store.clone(), version);
    let sync_effect = crate::pages::dict::reactive::reactive_effect(
        move || ver_eff.get(),
        move || {
            enabled_on.set(st_eff.enabled());
            show_on.set(st_eff.show_hidden());
        },
    );

    let (st_ver, sum_sig, err_sig) = (store.clone(), summary, errors);
    let refresh_effect = crate::pages::dict::reactive::reactive_effect(
        move || version.get(),
        move || {
            sum_sig.set(hide_summary(&st_ver));
            err_sig.set(hide_errors_text(&st_ver));
        },
    );

    let st_en = store.clone();
    let enabled_row = crate::widgets::setting_row(
        "开启设置项隐藏功能",
        Some(
            "开启后，在任意设置项或卡片上按 Ctrl+右键，可复制其配置 key、隐藏这一项 / 这一节。\
             隐藏只影响显示，设置照常生效；改动即时生效。",
        ),
        Element::switch(enabled_on).on_switch_change(move |ctx, v| {
            if let Err(e) = st_en.set_enabled(v) {
                ctx.toast_err(e);
            } else if v {
                ctx.toast("可在设置项上按 Ctrl+右键隐藏；改动即时生效");
            }
        }),
    );

    let st_show = store.clone();
    let show_row = crate::widgets::setting_row(
        "显示已隐藏的设置",
        Some(
            "临时把已隐藏的设置全部放回来，并用警示色描边标出，便于按 Ctrl+右键取消隐藏。\
             与定制版的隐藏互不影响——定制版藏掉的设置仍要用高级页的开关找回。",
        ),
        Element::switch(show_on)
            .enabled_signal(enabled_on)
            .on_switch_change(move |ctx, v| {
                if let Err(e) = st_show.set_show_hidden(v) {
                    ctx.toast_err(e);
                } else if v {
                    ctx.toast("已显示隐藏的设置，它们带警示色描边");
                }
            }),
    );

    let st_reset = store.clone();
    let reset_row = crate::widgets::setting_row(
        "恢复默认",
        Some("清空你自己隐藏的全部设置项与分区。定制版的隐藏不受影响。"),
        Element::button("全部恢复")
            .small()
            .outline_soft()
            .enabled_signal(enabled_on)
            .on_click(move |ctx| match st_reset.reset() {
                Ok(0) => ctx.toast("当前没有你自己隐藏的设置"),
                Ok(n) => ctx.toast(format!("已恢复 {n} 项")),
                Err(e) => ctx.toast_err(e),
            }),
    );

    let summary_row = crate::widgets::setting_row(
        "当前隐藏",
        None,
        Element::label_signal(summary)
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .max_lines(2)
            .width(180),
    );

    let err_row = Element::label_signal(errors)
        .font_size(11.5)
        .fg_role(Role::Warning)
        .width_match()
        .max_lines(3)
        .visible_when(move || !errors.get().is_empty());

    Element::col()
        .width_match()
        .child(crate::widgets::section(
            "设置项精简",
            vec![enabled_row, show_row, summary_row, reset_row, err_row],
        ))
        .child(sync_effect)
        .child(refresh_effect)
}

fn hide_summary(store: &crate::user_hide::UserHideStore) -> String {
    let n = store.count();
    if n == 0 {
        "尚未隐藏任何设置".to_string()
    } else if !store.enabled() {
        format!("已隐藏 {n} 项（功能已关闭，不生效）")
    } else if store.show_hidden() {
        format!("已隐藏 {n} 项（当前正在显示）")
    } else {
        format!("已隐藏 {n} 项")
    }
}

fn hide_errors_text(store: &crate::user_hide::UserHideStore) -> String {
    let errors = store.errors();
    if errors.is_empty() {
        String::new()
    } else {
        format!(
            "有 {} 条规则无法解析，已忽略：{}",
            errors.len(),
            errors.join("；")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_reflects_store_state() {
        let s =
            crate::user_hide::UserHideStore::in_memory(crate::user_hide::UserHideConfig::default());
        assert_eq!(hide_summary(&s), "尚未隐藏任何设置");
        s.add("key:ui.font.family").unwrap();
        s.set_enabled(true).unwrap();
        assert_eq!(hide_summary(&s), "已隐藏 1 项");
        s.set_show_hidden(true).unwrap();
        assert_eq!(hide_summary(&s), "已隐藏 1 项（当前正在显示）");
        s.set_show_hidden(false).unwrap();
        s.set_enabled(false).unwrap();
        assert_eq!(hide_summary(&s), "已隐藏 1 项（功能已关闭，不生效）");
    }

    #[test]
    fn errors_text_is_empty_without_bad_rules() {
        let s =
            crate::user_hide::UserHideStore::in_memory(crate::user_hide::UserHideConfig::default());
        assert!(hide_errors_text(&s).is_empty());
    }
}
