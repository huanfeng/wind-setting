//! 通用设置控件便捷函数 — panel / section_header / setting_row 等。
//!
//! 纯组合函数，仅供本项目用。视觉走 wind-ui-rust 的**角色化主题**（`*_role`），
//! 故所有控件随亮/暗主题自动适配。样式对齐 examples/ime.rs 的设置面板。

use windui::prelude::*;

/// 浮层面板：圆角 surface + 边框 + 投影（对话框等浮窗用）。
#[allow(dead_code)]
pub fn panel() -> Element {
    flat_panel().shadow(Shadow::new(0.0, 6.0, 14.0, Color::rgba(0, 0, 0, 110)))
}

/// 无投影面板（圆角 surface + 边框）。密集区域用，避免大量大阴影合成。
pub fn flat_panel() -> Element {
    Element::col()
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
}

/// 分组小标题（11px 弱化大写感，上下留白）。对齐 ime.rs settings_section_header。
pub fn section_header(title: &str) -> Element {
    Element::label(title)
        .font_size(11.0)
        .font_weight(600)
        .fg_role(Role::TextMuted)
        .height(16)
        .margin_xy(0, 9)
}

/// 设置行：左标签（+可选副标题）+ 右控件。对齐 ime.rs setting_row。
pub fn setting_row(label: &str, sub: Option<&str>, control: Element) -> Element {
    let mut left = Element::col().weight(1.0).spacing(2).child(
        Element::label(label)
            .font_size(13.0)
            .font_weight(500)
            .fg_role(Role::Text)
            .height(18),
    );
    if let Some(s) = sub {
        left = left.child(
            Element::label(s)
                .font_size(11.0)
                .fg_role(Role::TextMuted)
                .height(15),
        );
    }
    Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(0, 11)
        .spacing(12)
        .child(left)
        .child(control)
}

/// 分组：分组小标题 + 各行（行间自动插 1px 分隔线）。
/// `rows` 为已构建好的设置行（如 `setting_row(...)` 或清单引擎 `field(...)`）。
pub fn section(title: &str, rows: Vec<Element>) -> Element {
    let mut col = Element::col().width_match().child(section_header(title));
    let last = rows.len().saturating_sub(1);
    for (i, row) in rows.into_iter().enumerate() {
        col = col.child(row);
        if i != last {
            col = col.child(Element::divider());
        }
    }
    col
}

/// 页面内容外层：纵向滚动 + 内边距，承载多个 section。
pub fn page(body: Element) -> Element {
    Element::scroll().fill().child(
        Element::col()
            .width_match()
            .padding_xy(22, 14)
            .spacing(6)
            .child(body),
    )
}

/// 键位胶囊（快捷键展示用，灰底圆角小标签）。对齐 ime.rs key_chip。
#[allow(dead_code)]
pub fn key_chip(key: &str) -> Element {
    Element::label(key)
        .font_size(11.0)
        .fg_role(Role::Text)
        .height(24)
        .padding_xy(8, 0)
        .corner(5.0)
        .bg_role(Role::SurfaceAlt)
        .border_role(Role::Border, 1)
}

/// 待实现占位提示（标注 wind-ui-rust 能力缺口处）。
pub fn todo_hint(text: &str) -> Element {
    Element::label(text)
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .width_match()
        .padding_xy(0, 10)
}
