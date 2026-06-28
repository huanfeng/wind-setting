//! 通用设置控件便捷函数 — panel / section_header / setting_row 等。
//!
//! 纯组合函数，仅供本项目用。视觉走 wind-ui-rust 的**角色化主题**（`*_role`），
//! 故所有控件随亮/暗主题自动适配。样式对齐 examples/ime.rs 的设置面板。

use windui::prelude::*;

/// 应用图标（嵌入 PNG，与 exe/任务栏图标同源）。`px` 为逻辑边长。
/// 资源 `res/wind_setting_icon.png` 由 build 期从 `wind_setting.ico` 的 256px 帧提取。
pub fn app_icon(px: i32) -> Element {
    Element::image_bytes(include_bytes!("../res/wind_setting_icon.png")).size(px, px)
}

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
/// 置于卡片上方，左侧微缩进与卡片内容对齐。
///
/// `width_match()` + `max_lines(1)`：windui 的 Label 在「无显式宽度」时，measure 收敛到
/// 文字紧贴宽，draw 再按该窄宽折行（见 ui/mod.rs 注释），导致明明有空间也换行、甚至
/// 2 字标题也折。给足整行宽度并锁单行即可根治。
pub fn section_header(title: &str) -> Element {
    Element::label(title)
        .font_size(11.5)
        .font_weight(600)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(1)
        .height(18)
        .margin_xy(4, 4)
}

/// 设置行：左标签（+可选副标题）+ 右控件。对齐 ime.rs setting_row。
///
/// 标签 `width_match()` + `max_lines(1)`：占满左列可用宽，避免 windui Label 在紧贴宽下
/// 误折行（详见 [`section_header`]）。副标题同样占满宽，单行省略。
pub fn setting_row(label: &str, sub: Option<&str>, control: Element) -> Element {
    let mut left = Element::col().weight(1.0).spacing(2).child(
        Element::label(label)
            .font_size(13.0)
            .font_weight(500)
            .fg_role(Role::Text)
            .width_match()
            .max_lines(1)
            .truncate(Truncate::End)
            .height(18),
    );
    if let Some(s) = sub {
        left = left.child(
            Element::label(s)
                .font_size(11.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(1)
                .truncate(Truncate::End)
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

/// 卡片容器：圆角 surface + 边框，承载分区内的若干设置行（行间自动插分隔线）。
/// 卡片内置水平/垂直内边距，分隔线随之内缩（iOS/macOS 分组列表观感）。
pub fn card(rows: Vec<Element>) -> Element {
    let mut card = flat_panel().corner(10.0).width_match().padding_xy(16, 2);
    let last = rows.len().saturating_sub(1);
    for (i, row) in rows.into_iter().enumerate() {
        card = card.child(row);
        if i != last {
            card = card.child(Element::divider());
        }
    }
    card
}

/// 分组 = 分区小标题（卡片上方）+ 卡片（承载各设置行）。对齐 ime.rs 的 section 卡片观感。
/// `rows` 为已构建好的设置行（如 `setting_row(...)` 或清单引擎 `field(...)`）。
pub fn section(title: &str, rows: Vec<Element>) -> Element {
    Element::col()
        .width_match()
        .spacing(6)
        .child(section_header(title))
        .child(card(rows))
}

/// 页面内容外层：纵向滚动 + 内边距 + 分区间距，承载多个 section 卡片。
/// 内容区背景用 SurfaceAlt（右栏色），卡片用 Surface（高一档）浮于其上。
pub fn page(body: Element) -> Element {
    Element::scroll().fill().bg_role(Role::SurfaceAlt).child(
        Element::col()
            .width_match()
            .padding_xy(20, 16)
            .spacing(18)
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
