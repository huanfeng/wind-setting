//! 主题导入确认对话框。

use windui::prelude::*;

use crate::state::AppState;

pub fn build_theme_import_dialog(state: &AppState) -> Element {
    let visible = state.theme_import_dialog_visible;
    let url = state.pending_theme_import_url;
    let name = state.pending_theme_import_name;
    let url_display = state.theme_import_url_display;
    let name_display = state.theme_import_name_display;
    let status = state.status;

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
            Element::label_rc(url_display)
                .font_size(12.0)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(2)
                .padding_xy(18, 4),
        )
        .child(
            Element::label_rc(name_display)
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
                    match crate::theme_import::download_https_text(&u)
                        .and_then(|text| crate::theme_import::preview_text(&text).map(|_| text))
                        .and_then(|text| crate::theme_import::import_text(&text, false))
                    {
                        Ok(result) => {
                            status.set(format!("已导入主题：{}", result.display_name));
                            AppState::clear_theme_import_signals(
                                url,
                                name,
                                url_display,
                                name_display,
                            );
                            visible.set(false);
                        }
                        Err(err) => status.set(err.user_message()),
                    }
                })),
        );

    Element::dialog(visible, frame)
}
