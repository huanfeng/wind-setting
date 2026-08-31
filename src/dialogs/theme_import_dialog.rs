//! 主题导入确认对话框。

use std::cell::RefCell;
use std::rc::Rc;

use windui::prelude::*;

use crate::state::AppState;
use crate::theme_import::ThemeImportError;

pub fn build_theme_import_dialog(app_state: &Rc<RefCell<AppState>>) -> Element {
    let state = app_state.borrow();
    let visible = state.theme_import_dialog_visible;
    let url = state.pending_theme_import_url;
    let name = state.pending_theme_import_name;
    let url_display = state.theme_import_url_display;
    let name_display = state.theme_import_name_display;
    let ov_text = state.theme_overwrite_text;
    let ov_msg = state.theme_overwrite_msg;
    let ov_visible = state.theme_overwrite_visible;
    drop(state);
    let import_state = app_state.clone();

    let frame = Element::col()
        .width(520)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("导入主题")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(
                "即将从 HTTPS 链接下载 TOML 主题。请确认来源可信，导入前会先交给 core 校验。",
            )
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(3)
            .padding_xy(18, 10),
        )
        .child(
            Element::label_signal(url_display)
                .font_size(12.0)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(2)
                .padding_xy(18, 4),
        )
        .child(
            Element::label_signal(name_display)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(1)
                .padding_xy(18, 4),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| {
                            AppState::clear_theme_import_signals(
                                url,
                                name,
                                url_display,
                                name_display,
                            );
                            visible.set(false);
                        }),
                )
                .child(Element::button("下载并导入").small().on_click(move |_| {
                    let u = url.get();
                    // 直接下载 → 导入；import_text 内部（core）已做校验，无需再单独预览
                    // （core theme.preview 只支持按已安装 name 预览，不接受原始文本）。
                    //
                    // 下载与导入分两步写（而非 and_then 串起来）：撞上同名主题时要拿**已下载的
                    // 原文**去弹覆盖确认，串起来的话 text 在闭包里就丢了，确认后只能重下一次。
                    let text = match crate::theme_import::download_https_text(&u) {
                        Ok(t) => t,
                        Err(err) => {
                            import_state.borrow().notify_err(err.user_message());
                            return;
                        }
                    };
                    match crate::theme_import::import_text(&text, false, None) {
                        Ok(result) => {
                            let mut st = import_state.borrow_mut();
                            st.refresh_theme_names();
                            st.notify_ok(format!("已导入主题：{}", result.display_name));
                            AppState::clear_theme_import_signals(
                                url,
                                name,
                                url_display,
                                name_display,
                            );
                            st.theme_import_dialog_visible.set(false);
                        }
                        // 同名已存在：这不是「导入不了」，而是「要不要更新」——转成覆盖确认，
                        // 用户确认后以 force=true 重推同一份原文。早先它和下载失败共用一条
                        // toast 出口，用户只看到「主题已存在」而没有任何前进的路，等于无法更新。
                        //
                        // 先收掉本框再弹确认框：两个对话框同时可见会互相遮挡，且冲突不是地址
                        // 写错，留着本框让用户「改地址重试」并无意义。
                        Err(ThemeImportError::Conflict(msg)) => {
                            AppState::clear_theme_import_signals(
                                url,
                                name,
                                url_display,
                                name_display,
                            );
                            visible.set(false);
                            AppState::ask_overwrite_theme(ov_text, ov_msg, ov_visible, text, &msg);
                        }
                        // 下载/预览/导入任一环节失败都保留对话框（用户可改地址重试或取消），
                        // 但错误改走 toast——早先写的是底栏常驻状态文字（现已只剩状态点），
                        // 一条一次性的导入失败提示混在那里容易被忽略或被后续状态覆盖。
                        Err(err) => import_state.borrow().notify_err(err.user_message()),
                    }
                })),
        );

    Element::dialog(visible, frame)
}
