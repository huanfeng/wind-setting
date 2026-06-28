//! dialog_button 各定制对话框主体（按 key 注册）。

use serde_json::{Map, Value};
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
            // padding 包裹：text_input 用 width_match 填满内边距后的宽，避免 margin 外推溢出对话框。
            Element::col().width_match().padding_xy(18, 6).child(
                Element::text_input(text, "按空格分隔")
                    .multiline()
                    .wrap(true)
                    .width_match()
                    .height(96),
            ),
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
                        .outline()
                        .on_click(move |_| text.set(default_text.clone())),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .neutral()
                        .outline()
                        .on_click(move |_| visible.set(false)),
                )
                .child(
                    Element::button("确认")
                        .outline()
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// 单行字符串编辑对话框：提示 + 单行 text_input + [恢复默认 | 取消 确认]。
/// 用于 str 类型的「启用 + 设置」项（如智能标点字符集），需要「恢复默认」故走对话框。
pub fn build_text_dialog(
    visible: Signal<bool>,
    title: &str,
    hint: &str,
    text: Signal<String>,
    default_text: String,
    placeholder: &str,
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
            Element::col().width_match().padding_xy(18, 6).child(
                Element::text_input(text, placeholder).width_match().height(36),
            ),
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
                        .outline()
                        .on_click(move |_| text.set(default_text.clone())),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .neutral()
                        .outline()
                        .on_click(move |_| visible.set(false)),
                )
                .child(
                    Element::button("确认")
                        .outline()
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// map → multi-line text（每行一对 `原符号=替换`）。
///
/// 不变量：键不含 `=`——`=` 是行内键值分隔符，含 `=` 的键经 [`rows_to_map`]
/// 回读会被错误截断；故此降级编辑器仅适用于键无 `=` 的映射（标点映射满足）。
pub fn map_to_rows(map: &Map<String, Value>) -> String {
    let mut pairs: Vec<String> = map
        .iter()
        .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or("")))
        .collect();
    pairs.sort(); // deterministic order for tests
    pairs.join("\n")
}

/// multi-line text → map（去空白、跳过无 `=` 行）。
///
/// 不变量：键不含 `=`——按首个 `=` 切分（`split_once`），故键侧出现 `=` 会被
/// 截断到分隔符前；与 [`map_to_rows`] 对称，仅适用于键无 `=` 的映射。
pub fn rows_to_map(text: &str) -> Map<String, Value> {
    let mut m = Map::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            m.insert(k.trim().to_string(), Value::String(v.trim().to_string()));
        }
    }
    m
}

/// 映射表编辑对话框（降级实现：每行一对 `原符号=替换`）。
/// windui 无动态增删行的 table_editable，故用 multiline text_input 替代。
pub fn build_map_dialog(
    visible: Signal<bool>,
    title: &str,
    hint: &str,
    text: Signal<String>,
    default_text: String,
) -> Element {
    let full_hint = if hint.is_empty() {
        "每行一对，格式：原符号=替换符号".to_string()
    } else {
        format!("{}\n每行一对，格式：原符号=替换符号", hint)
    };
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
            Element::label(&full_hint)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(4)
                .padding_xy(18, 10),
        )
        .child(
            Element::col().width_match().padding_xy(18, 6).child(
                Element::text_input(text, "，=，")
                    .multiline()
                    .width_match()
                    .height(120),
            ),
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
                        .outline()
                        .on_click(move |_| text.set(default_text.clone())),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .neutral()
                        .outline()
                        .on_click(move |_| visible.set(false)),
                )
                .child(
                    Element::button("确认")
                        .outline()
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// 配对复选网格对话框（用于 auto_pair *_pairs 类 strlist）。
/// `sigs`: (value, Signal<bool>)；`labels`: 每项对应的展示标签（1:1 with sigs）；
/// `defaults`: capability 默认 strlist（恢复默认用）。
pub fn build_pairs_dialog(
    visible: Signal<bool>,
    title: &str,
    hint: &str,
    sigs: Vec<(String, Signal<bool>)>,
    labels: Vec<String>,
    defaults: Vec<String>,
) -> Element {
    let sigs_reset = sigs.clone();
    let checkboxes: Vec<Element> = sigs
        .iter()
        .zip(labels.iter())
        .map(|((_, s), lbl)| Element::checkbox(lbl, *s).fg_role(Role::Text))
        .collect();
    let grid = Element::grid(2, 10, checkboxes);
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
        .child(grid.width_match().margin_xy(18, 8))
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
                        .outline()
                        .on_click(move |_| {
                            for (v, s) in &sigs_reset {
                                s.set(defaults.iter().any(|d| d == v));
                            }
                        }),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .neutral()
                        .outline()
                        .on_click(move |_| visible.set(false)),
                )
                .child(
                    Element::button("确认")
                        .outline()
                        .on_click(move |_| visible.set(false)),
                ),
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

    #[test]
    fn map_roundtrip() {
        let mut m = serde_json::Map::new();
        m.insert("，".to_string(), Value::String("，".to_string()));
        m.insert("。".to_string(), Value::String("。".to_string()));
        let text = map_to_rows(&m);
        let m2 = rows_to_map(&text);
        assert_eq!(m, m2);
    }

    #[test]
    fn rows_to_map_skips_blank_and_no_eq() {
        let text = "  \n，=，\nbadline\n。=。\n";
        let m = rows_to_map(text);
        assert_eq!(m.len(), 2);
        assert_eq!(m["，"], Value::String("，".to_string()));
    }

    #[test]
    fn map_to_rows_deterministic() {
        // map_to_rows sorts keys, output must be stable
        let mut m = serde_json::Map::new();
        m.insert("b".to_string(), Value::String("B".to_string()));
        m.insert("a".to_string(), Value::String("A".to_string()));
        let text = map_to_rows(&m);
        assert_eq!(text, "a=A\nb=B");
    }
}
