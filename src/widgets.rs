//! 通用设置控件便捷函数 — panel / section_header / setting_row 等。
//!
//! 纯组合函数，仅供本项目用。视觉走 wind-ui-rust 的**角色化主题**（`*_role`），
//! 故所有控件随亮/暗主题自动适配。样式对齐 examples/ime.rs 的设置面板。

use std::cell::Cell;

use windui::prelude::*;

// 设置行的排版（字号/字重/行高/间距/限行）统一在 `theme_def::apply_form_theme` 里，
// 由 windui 的 `FormTheme` 承载——本文件曾有一组同名私有常量，行的实现收敛到库之后
// 它们只剩一份重复的真相，已删除。调排版改那一处。

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

/// 分区标题：强调色淡底图标块（[`crate::icons::CHIP_PX`]）+ 14px 加粗标题（参考稿 sec-head）。
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
            .size(crate::icons::CHIP_PX, crate::icons::CHIP_PX)
            .corner(7.0)
            .bg(Color::hex(accent).scale_alpha(0.12))
            .child(
                crate::icons::tinted(svg, crate::icons::ICON_PX, Color::hex(accent))
                    .align(Align::Center),
            )
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

/// 页面头部全局开关（预留：后续可接设置项/命令行参数）。当前关闭——观感待打磨，
/// 各页仍保留 page_header 调用点，改此常量即整体启用。
pub const PAGE_HEADER_ENABLED: bool = false;

/// 页面头部（左右布局）：17px 页标题在左，12px 弱化描述同行随后，不占额外纵向空间。
/// 是否渲染由调用方按 [`PAGE_HEADER_ENABLED`] 决定。
pub fn page_header(title: &str, desc: &str) -> Element {
    Element::row()
        .width_match()
        .cross(Align::End)
        .spacing(10)
        .child(
            Element::label(title)
                .font_size(17.0)
                .font_weight(700)
                .fg_role(Role::Text)
                .max_lines(1)
                .height(24),
        )
        .child(
            Element::label(desc)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .weight(1.0)
                .max_lines(1)
                .truncate(Truncate::End)
                .height(18),
        )
}

/// 设置行：左标签（+可选副标题）+ 右控件。
///
/// 实现委托给 windui 的 `Element::setting_row` / `setting_row_desc`（0.12+），本函数只剩
/// 一件事：把本仓惯用的 `Option<&str>` 副标题分派到那两个构造器上——库刻意不做
/// `Option` 参数（`None` 推断不出类型，且占多数的无副标题行还得白写一个 `None`）。
///
/// 排版（行高/标签字号字重/说明字号/内边距/限行）全部走 `FormTheme`，见
/// [`crate::theme_def::apply_form_theme`]。限行 + 末尾省略 + 悬浮看全文这套，
/// 本仓曾各写一遍，现由库统一提供：说明文字来自 core 的 manifest、长度不可控，
/// 不限行会把行撑成三四行、整列高度参差。
pub fn setting_row(label: &str, sub: Option<&str>, control: Element) -> Element {
    match sub {
        Some(s) => Element::setting_row_desc(label, s, control),
        None => Element::setting_row(label, control),
    }
}

/// 副标题随信号变化的设置行：左标签 + 下方动态说明 + 右控件。
///
/// 用于展示**运行期状态**（如 `windinput://` 关联当前指向谁）——库的
/// `setting_row_desc` 只收 `String`，套不进随注册表变化的文案。除副标题换成
/// `label_signal` 外，字号/文字色/间距/内边距一律走同一套 `FormTheme`，
/// 与相邻的清单行对得齐。
///
/// 与库版的一处有意偏离：**不挂 tooltip**。库的 `clamp_lines` 在截断时用
/// `tooltip(full_text)` 兜底看全文，而 tooltip 只收静态字符串（且 `debug_assert`
/// 禁多行），动态文本给不了。故限行放宽到 3，让带完整路径的长状态尽量显示完整；
/// 真被截断也不至于丢信息 —— 需要完整路径的场合（覆盖/解除确认）会在对话框里全文呈现。
pub fn setting_row_desc_signal(label: &str, desc: Signal<String>, control: Element) -> Element {
    let th = windui::theme::current();
    let (f, m) = (&th.form, &th.metrics);
    let left = Element::col()
        .weight(1.0)
        .spacing(2)
        .child(
            Element::label(label)
                .font_size(f.label_size(m))
                .font_weight(f.label_weight())
                .fg_role(Role::Text)
                .width_match()
                .max_lines(1)
                .truncate(Truncate::End),
        )
        .child(
            Element::label_signal(desc)
                .font_size(f.desc_size(m))
                .fg_role(Role::TextSubtle)
                .width_match()
                .max_lines(3)
                .truncate(Truncate::End),
        );
    Element::row()
        .width_match()
        // Center 而非 Start：右侧是一组按钮，副标题一行时贴顶会让按钮与标签错位。
        .cross(Align::Center)
        .padding_xy(0, f.row_pad_y())
        .spacing(f.gap())
        .child(left)
        .child(control)
}

/// 多选键组专用行：左侧弹性标签 + 右侧固定宽网格（靠右对齐）。
///
/// 网格固定 300px（两列各 ~145），不会撑宽；左侧 weight(1) 自由伸展，
/// 窗口变宽时标签获得更多空间，与 hotkey 行的控件组视觉一致靠右。
pub fn setting_row_checks(label: &str, sub: Option<&str>, grid: Element) -> Element {
    // 字号/字重/间距与库版设置行同源（`FormTheme`），只是本行型自行拼装：库没有
    // 「右侧固定宽网格」这一种，而它与普通行的差别（cross 对齐、控件定宽、允许两行）
    // 都在骨架上，套不进 `setting_row_desc`。
    let th = windui::theme::current();
    let (f, m) = (&th.form, &th.metrics);
    // 限行取 2 而非跟随 `form.*_max_lines`：键组行的标签常是「候选翻页键（上一页/下一页）」
    // 这类长名，右侧又被 300px 网格占死，压成一行会把大半个名字吃掉；而本行型总共只有
    // 三五行，不像清单页那样需要严格等高。
    let mut left = Element::col().weight(1.0).spacing(2).child(
        Element::label(label)
            .font_size(f.label_size(m))
            .font_weight(f.label_weight())
            .fg_role(Role::Text)
            .width_match()
            .max_lines(2)
            .truncate(Truncate::End),
    );
    if let Some(s) = sub {
        left = left.child(
            Element::label(s)
                .font_size(f.desc_size(m))
                .fg_role(Role::TextSubtle)
                .width_match()
                .max_lines(2)
                .truncate(Truncate::End)
                .tooltip(s),
        );
    }
    Element::row()
        .width_match()
        .cross(Align::Start)
        .padding_xy(0, f.row_pad_y())
        .spacing(f.gap())
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
