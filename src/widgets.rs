//! 通用设置控件便捷函数 — panel / section_header / setting_row 等。
//!
//! 纯组合函数，仅供本项目用。视觉走 wind-ui-rust 的**角色化主题**（`*_role`），
//! 故所有控件随亮/暗主题自动适配。样式对齐 examples/ime.rs 的设置面板。

use std::cell::Cell;

use windui::prelude::*;

thread_local! {
    /// 全局明暗信号（shell::build 注册）。静态色图标/渐变无法走 *_role 换肤，
    /// 用它做「亮/暗双元素 visible_when」双态。UI 单线程，thread_local 即全局。
    static DARK_SIG: Cell<Option<Signal<bool>>> = const { Cell::new(None) };
}

/// 注册明暗信号（shell::build 调用一次）。
pub fn init_dark_sig(sig: Signal<bool>) {
    DARK_SIG.with(|c| c.set(Some(sig)));
}

/// 取明暗信号；未注册时返回恒亮信号（测试/独立构建兜底）。
pub fn dark_sig() -> Signal<bool> {
    DARK_SIG.with(|c| c.get()).unwrap_or_else(|| signal(false))
}

/// 应用图标（嵌入 PNG，与 exe/任务栏图标同源，均由主仓库 688px `pic/logo.png`
/// 经 Lanczos 离线生成）。`px` 为逻辑边长；小尺寸（标题栏）用 64px 专用资源，
/// 避免 256px 大图实时缩到 ~30 物理px 的大缩比采样发糊。
pub fn app_icon(px: i32) -> Element {
    let bytes: &'static [u8] = if px <= 32 {
        include_bytes!("../res/wind_setting_icon_sm.png")
    } else {
        include_bytes!("../res/wind_setting_icon.png")
    };
    Element::image_bytes(bytes).size(px, px)
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

/// 分区标题：26px 强调色淡底图标块 + 14px 加粗标题（参考稿 sec-head）。
///
/// 图标块底色/图标色为静态 accent（tint/半透明底不走 *_role），按 [`dark_sig`]
/// 做亮/暗双态 visible_when 跟随换肤；图标按 section 名查 [`crate::icons::section_icon`]。
///
/// 标题 Label `weight(1.0)` + `max_lines(1)`：windui 的 Label 在「无显式宽度」时，
/// measure 收敛到文字紧贴宽，draw 再按该窄宽折行（见 ui/mod.rs 注释），导致明明有
/// 空间也换行、甚至 2 字标题也折。给足整行宽度并锁单行即可根治。
pub fn section_header(title: &str) -> Element {
    let svg = crate::icons::section_icon(title);
    let chip = |accent: u32, on_dark: bool| {
        Element::stack()
            .size(26, 26)
            .corner(7.0)
            .bg(Color::hex(accent).scale_alpha(0.12))
            .child(crate::icons::tinted(svg, 15, Color::hex(accent)).align(Align::Center))
            .visible_when(move || dark_sig().get() == on_dark)
    };
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(9)
        .margin_xy(2, 4)
        .child(
            Element::stack()
                .child(chip(crate::theme_def::ACCENT_DARK, true))
                .child(chip(crate::theme_def::ACCENT_LIGHT, false)),
        )
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .weight(1.0)
                .max_lines(1)
                .height(20),
        )
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

/// 多选键组专用行：左侧弹性标签 + 右侧固定宽网格（靠右对齐）。
///
/// 网格固定 300px（两列各 ~145），不会撑宽；左侧 weight(1) 自由伸展，
/// 窗口变宽时标签获得更多空间，与 hotkey 行的控件组视觉一致靠右。
pub fn setting_row_checks(label: &str, sub: Option<&str>, grid: Element) -> Element {
    let mut left = Element::col().weight(1.0).spacing(2).child(
        Element::label(label)
            .font_size(13.0)
            .font_weight(500)
            .fg_role(Role::Text)
            .width_match()
            .max_lines(2)
            .truncate(Truncate::End),
    );
    if let Some(s) = sub {
        left = left.child(
            Element::label(s)
                .font_size(11.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(2)
                .truncate(Truncate::End),
        );
    }
    Element::row()
        .width_match()
        .cross(Align::Start)
        .padding_xy(0, 11)
        .spacing(12)
        .child(left)
        .child(grid.width(300))
}

/// 卡片容器：圆角 surface + 边框，承载分区内的若干设置行（行间自动插分隔线）。
/// 卡片内置水平/垂直内边距，分隔线随之内缩（iOS/macOS 分组列表观感）。
pub fn card(rows: Vec<Element>) -> Element {
    let mut card = flat_panel()
        .corner(13.0)
        .width_match()
        .padding_xy(16, 2)
        .shadow(Shadow::new(0.0, 1.0, 2.0, Color::rgba(20, 30, 50, 8)));
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
