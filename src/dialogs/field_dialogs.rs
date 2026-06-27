//! dialog_button 各定制对话框主体（按 key 注册）。

use windui::prelude::*;

/// strlist → 空格分隔字符串（对话框文本框展示）。
pub fn strlist_to_spaced(items: &[String]) -> String {
    items.join(" ")
}

/// 空格分隔字符串 → strlist（去空白、跳过空项）。
pub fn spaced_to_strlist(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

/// 空格分隔文本编辑对话框：提示 + text_input + [恢复默认 | 取消 确认]。
/// 确认即写入 `text`（writer 随主保存提交）；取消仅关闭，未确认改动仍保留在 text。
pub fn build_spaced_text_dialog(
    visible: Signal<bool>,
    title: &str,
    hint: &str,
    text: Signal<String>,
    default_text: String,
) -> Element {
    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(hint)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3)
                .padding_xy(18, 10),
        )
        .child(
            Element::text_input(text, "按空格分隔")
                .multiline()
                .width_match()
                .height(96)
                .margin_xy(18, 6),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    Element::button("恢复默认")
                        .small()
                        .neutral()
                        .on_click(move |_| text.set(default_text.clone())),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .neutral()
                        .on_click(move |_| visible.set(false)),
                )
                .child(Element::button("确认").on_click(move |_| visible.set(false))),
        );
    Element::dialog(visible, frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spaced_strlist_roundtrip() {
        let v = vec!["http://".to_string(), "https://".to_string(), "ftp://".to_string()];
        let s = strlist_to_spaced(&v);
        assert_eq!(s, "http:// https:// ftp://");
        assert_eq!(spaced_to_strlist(&s), v);
    }

    #[test]
    fn spaced_to_strlist_trims_and_skips_empty() {
        assert_eq!(spaced_to_strlist("  a   b  "), vec!["a".to_string(), "b".to_string()]);
        assert!(spaced_to_strlist("   ").is_empty());
    }
}
