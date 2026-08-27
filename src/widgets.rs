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

/// 应用图标（嵌入 PNG，与 exe/任务栏图标同源，均由主仓库 `pic/logo.png` 经
/// Lanczos 离线生成）。`px` 为逻辑边长；小尺寸（标题栏）用 64px 专用资源，
/// 避免 256px 大图实时缩到 ~30 物理px 的大缩比采样发糊。
///
/// **安全边距**：图标资源满画布无留白（有意为之——任务栏 16/24px 那种极小尺寸
/// 要靠铺满换取有效像素），但本页面渲染的是 26/72px 这类较大尺寸，铺满会让圆角
/// 直接顶死容器边界，观感就是"边缘被裁了一点"。故按 [`ICON_INSET_RATIO`] 内缩，
/// 布局占位仍为 `px`（排版不动）。
///
/// 内缩还顺带消掉一个真裁剪：windui 的 `draw_image` 以 `dst` 作 ALIASED 裁剪框，
/// 铺满时图边界与裁剪框零余量重合，而落点会吸附到整数物理像素——125%/150% 下
/// 这半像素位移会把一侧最外半个物理像素切掉。留边即留出这份余量。
pub fn app_icon(px: i32) -> Element {
    let bytes: &'static [u8] = if px <= 32 {
        include_bytes!("../res/wind_setting_icon_sm.png")
    } else {
        include_bytes!("../res/wind_setting_icon.png")
    };
    let inner = app_icon_inner_px(px);
    Element::stack().size(px, px).child(
        Element::image_bytes(bytes)
            .size(inner, inner)
            .align(Align::Center),
    )
}

/// 应用图标四周留白占边长的比例。4% ≈ 72px 留 3px。
const ICON_INSET_RATIO: f32 = 0.04;

/// 留白下限（逻辑 px）。小图标不能只按比例给：28px 处 4% 仅 1px，
/// 而图标资源最外一圈本就是抗锯齿边（`_sm.png` 四边 alpha 247/248），
/// 叠加落点吸附到整数物理像素后，150% 下实测会丢掉一整列——余量必须
/// 大于那半个物理像素的位移，2px 逻辑留白在 150% 下即 3 物理像素，足够。
const ICON_INSET_MIN: i32 = 2;

/// 图标实际绘制边长（[`app_icon`] 的占位 `px` 扣掉两侧安全边距）。
/// 下限 1px 防极小尺寸下算出非正值。
fn app_icon_inner_px(px: i32) -> i32 {
    let inset = ((px as f32 * ICON_INSET_RATIO).round() as i32).max(ICON_INSET_MIN);
    (px - inset * 2).max(1)
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

/// 三态设置行：**行首覆盖勾选** + 左标签（+副标题）+ 来源文案 + 右控件区。
///
/// 用于方案级覆盖（当前是码表配置）：每一项各自决定跟随基线还是自己设一个值，勾选框
/// 就是这个决定。与 [`setting_row`] 的差别只有行首那个框和控件左侧那句来源——排版
/// （字号/字重/行高/限行）全部走同一套 `FormTheme`，两种行混排时对得齐。
///
/// ★ 勾选框**必须在行首而不是控件旁**：这一列要能被竖着扫一眼就看出「我改过哪几项」。
/// 混在右侧一堆开关下拉里就没有这个作用了。
///
/// 来源列**三态三色**，不是一律灰：
///
/// | 状态 | 文案 | 颜色 |
/// | --- | --- | --- |
/// | 跟随基线 | 跟随全局 / 跟随基线 | `TextMuted`（灰，默认态不该抢眼） |
/// | 方案作者定的 | 方案自带 | `Warning`（琥珀，提示「这项与全局不同，但不是你改的」） |
/// | 用户改的 | 已自定义 | `Accent`（与整个设置界面的主色一致，＝「你的改动」） |
///
/// 一律灰的话，「方案自带」在一屏「跟随全局」里毫无存在感——而它恰恰是最需要被看见的：
/// 用户改了全局却发现这个方案没跟着变，答案就在这一列。
///
/// 用两个叠加 label + `visible_when` 而不是给一个 label 换色：`fg_role` 不收信号，
/// 而勾选是随时会变的。未勾选那半的颜色由 `follow_role` 静态给出（来源是 seed 时定的）。
///
/// ⚠️ 本函数**不**给整行挂 `enabled_when`。调用方只在**真不可用**（上游开关没开）时才挂，
/// 那会把勾选框一起置灰——这是对的：上游关着时，这一项连覆盖都不该谈。**「未勾选」
/// 绝不能用置灰表达**，理由见 [`override_control`]。
pub fn setting_row_override(
    over: Signal<bool>,
    label: &str,
    sub: Option<&str>,
    follow_source: &str,
    follow_role: Role,
    control: Element,
) -> Element {
    let th = windui::theme::current();
    let (f, m) = (&th.form, &th.metrics);
    let mut left = Element::col().weight(1.0).spacing(2).child(
        Element::label(label)
            .font_size(f.label_size(m))
            .font_weight(f.label_weight())
            .fg_role(Role::Text)
            .width_match()
            .max_lines(1)
            .truncate(Truncate::End),
    );
    if let Some(s) = sub {
        left = left.child(
            Element::label(s)
                .font_size(f.desc_size(m))
                .fg_role(Role::TextSubtle)
                .width_match()
                .max_lines(2)
                .truncate(Truncate::End),
        );
    }
    Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(0, f.row_pad_y())
        .spacing(f.gap())
        // 空标签的勾选框：标签文字在左列，这里只要那个方框。
        .child(Element::checkbox("", over).small().fg_role(Role::Text))
        .child(left)
        .child(
            Element::row()
                .width(SOURCE_W)
                .cross(Align::Center)
                .child(
                    Element::label(follow_source)
                        .font_size(f.desc_size(m))
                        .fg_role(follow_role)
                        .max_lines(1)
                        .truncate(Truncate::End)
                        .visible_when(move || !over.get()),
                )
                .child(
                    Element::label("已自定义")
                        .font_size(f.desc_size(m))
                        .fg_role(Role::Accent)
                        .max_lines(1)
                        .visible_when(move || over.get()),
                ),
        )
        .child(control)
}

/// 来源文案列宽。**定宽**：文案随勾选在「已自定义」「跟随全局」「方案自带」之间变，
/// 不定宽的话右侧控件会跟着左右跳。
const SOURCE_W: i32 = 64;

/// 控件位的三种形态，叠在同一个定宽容器里各挂一个 `visible_when`：
///
/// | 情形 | 显示 |
/// | --- | --- |
/// | 上游没开（`gate` 为假） | 原因文字（「需先开启「精确匹配模式」」） |
/// | 已勾选 | 可编辑控件 |
/// | 未勾选 | **跟随值**，正常文字色 |
///
/// ★★★ **未勾选不是禁用，绝不能置灰。** 这一屏绝大多数行都是跟随态，把跟随做成灰色
/// 只读，整屏就是一片灰，看起来像坏了——这正是上一版被推翻的原因。灰只留给「上游没开」
/// 那一种，且那时整行都淡下去、并直接写出原因，用户不必挨个试上游开关。
///
/// ⚠️ 未勾选时显示的是**跟随值**，不是控件当前值：用户勾上改了值又取消勾选，落盘的是
/// 「取消覆盖」，界面若还显示他刚改的那个数就是在说谎。
///
/// 用显隐而不是重建控件：windui 的信号没有变更回调，「取消勾选时把控件值改回跟随值」
/// 只能在渲染闭包里做 side effect（每帧求值 + 条件写信号），既脆又会污染脏判定。
/// 显隐是纯函数，每帧求值天然正确。
pub fn override_control(
    over: Signal<bool>,
    gate: std::rc::Rc<dyn Fn() -> bool>,
    editor: Element,
    follow_text: &str,
    reason: &str,
    width: i32,
) -> Element {
    let th = windui::theme::current();
    let (f, m) = (&th.form, &th.metrics);
    let (g1, g2, g3) = (gate.clone(), gate.clone(), gate);
    Element::row()
        .width(width)
        .cross(Align::Center)
        // ⚠️ 靠右**必须**靠这个弹性空白撑，不能用 `Element::align(Align::End)`：那个设的是
        // 元素自身在父容器里的对齐，不是 row 的主轴对齐——写上去毫无反应，三种形态会全部
        // 贴在容器左边，与右侧的全局页控件列对不齐。同 `setting_row_checks` 的做法。
        .child(Element::col().weight(1.0))
        .child(editor.visible_when(move || g1() && over.get()))
        .child(
            Element::label(follow_text)
                .font_size(f.label_size(m))
                // 跟随值用**正常文字色**——它是这一项当前真实生效的值，不是残缺状态。
                .fg_role(Role::Text)
                .max_lines(1)
                .truncate(Truncate::End)
                .visible_when(move || g2() && !over.get()),
        )
        .child(
            Element::label(reason)
                .font_size(f.desc_size(m))
                .fg_role(Role::TextMuted)
                .max_lines(2)
                .truncate(Truncate::End)
                .visible_when(move || !g3()),
        )
}

/// 分组卡片：圆角边框 + 标题行（左标题、右状态摘要）+ 组内各行。
///
/// ★ 卡片而不是一行分组标题：上一版用的是普通标题行，被行首勾选框和来源列的视觉重量
/// 压得看不见了——分类是这一屏的骨架，得有边框把它撑住。
///
/// `summary` 取信号：标题右侧报本组改过几项，随勾选实时变。
/// `rows` 收 `Vec` 而不是拼好的 `Element`：**行之间要插分割线**。
///
/// 原先收一个已拼好的 col，于是卡片里各行紧贴在一起——只有标题下那一条线，
/// 组内十来行连成一片（2026-08-26 用户原话：「加了 card，但也缺少分割线」）。
pub fn override_group_card(title: &str, summary: Signal<String>, rows: Vec<Element>) -> Element {
    let th = windui::theme::current();
    let (f, m) = (&th.form, &th.metrics);
    let mut col = Element::col().width_match();
    if !title.is_empty() {
        col = col.child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .padding_xy(0, 2)
                .child(
                    // 字号/字重/颜色与 `manifest::dialog_group_header` 同款——设置界面里
                    // 所有小标题都是这一套强调色，卡片标题自成一格会让人以为它是另一种东西。
                    Element::label(title)
                        .font_size(12.5)
                        .font_weight(600)
                        .fg_role(Role::Accent)
                        .weight(1.0)
                        .max_lines(1),
                )
                .child(
                    Element::label_signal(summary)
                        .font_size(f.desc_size(m))
                        .fg_role(Role::TextMuted)
                        .max_lines(1),
                ),
        );
        col = col.child(Element::divider());
    }
    let last = rows.len().saturating_sub(1);
    for (i, row) in rows.into_iter().enumerate() {
        col = col.child(row);
        if i != last {
            col = col.child(Element::divider());
        }
    }
    flat_panel()
        .width_match()
        .padding_xy(12, 6)
        .margin_xy(0, 3)
        .child(col)
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

/// 对话框内的一节：小标题 + 卡片（**行间自动分割线**）。
///
/// 方案设置对话框此前每节都是手写 `col + 小标题 + 若干裸 row`：没有卡片、没有分割线，
/// 行与行贴在一起，说明文字有的在行内、有的另起一行 ⇒ 一屏看下来分不出条目边界
/// （2026-08-26 用户原话：「条目的分割不是很明显」「比较杂乱」）。
///
/// 本函数把那套统一到主设置页的观感上：**行一律用 [`setting_row`]**（标题 + 说明 + 右控件），
/// 节用卡片包住。⇒ 新增一节时别再手写外壳，否则又会长出一种"看着像但不一样"的排版。
pub fn dialog_section(title: &str, rows: Vec<Element>) -> Element {
    dialog_section_full(title, None, None, rows)
}

/// 同上 + 节说明（标题下、卡片上方的一句灰字）。
///
/// 只写**整节共有**的话；单项的说明属于那一行自己的 `desc`（[`setting_row`] 的第二参）。
/// 混着写会让人分不出这句管的是哪一项——本对话框此前就有几节把单项说明摆在了节级位置。
pub fn dialog_section_hint(title: &str, hint: &str, rows: Vec<Element>) -> Element {
    dialog_section_full(title, Some(hint), None, rows)
}

/// 完整形态：小标题（右侧可挂动作按钮）+ 可选节说明 + 卡片。
///
/// 按钮挂在标题行而不是单独占一行：单独一行时左边一整条空白会把它推成右上角的孤浮标。
pub fn dialog_section_full(
    title: &str,
    hint: Option<&str>,
    action: Option<Element>,
    rows: Vec<Element>,
) -> Element {
    let head: Element = match action {
        // 整宽 label 塞不进带按钮的行，故此处复刻样式而不调 `dialog_group_header`。
        // 字号引用同一个常量，避免两处各写各的字面量再漂移。
        Some(btn) => Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(10)
            .child(
                Element::label(title)
                    .font_size(crate::manifest::DIALOG_HEADER_SIZE)
                    .font_weight(600)
                    .fg_role(Role::Accent)
                    .weight(1.0)
                    .max_lines(1)
                    .padding_xy(0, 5),
            )
            .child(btn),
        None => crate::manifest::dialog_group_header(title),
    };
    let mut col = Element::col()
        .width_match()
        .spacing(2)
        .padding_xy(0, 4)
        .child(head);
    if let Some(h) = hint {
        col = col.child(
            Element::label(h)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(2)
                .padding_xy(0, 2),
        );
    }
    col.child(card(rows))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 图标必须**小于**占位框：等于就是零余量铺满，圆角顶死边界、且与 windui 的
    /// ALIASED 裁剪框重合（见 [`app_icon`] 文档）。实际调用的两处尺寸逐一钉死。
    #[test]
    fn app_icon_keeps_safe_margin() {
        for px in [28, 72] {
            let inner = app_icon_inner_px(px);
            assert!(
                inner < px,
                "app_icon({px}) 实绘 {inner}px 应小于占位，否则安全边距为零"
            );
            assert!(
                (px - inner) % 2 == 0,
                "app_icon({px}) 留白 {} 应为偶数，否则居中后两侧不等宽",
                px - inner
            );
        }
    }

    /// 留白比例落在 3%~8%：太小吸收不了亚像素位移，太大图标会明显缩水。
    /// 上界给到 8% 是因为小尺寸受 [`ICON_INSET_MIN`] 托底（28px 处即 7.1%）。
    #[test]
    fn app_icon_inset_ratio_is_moderate() {
        for px in [28, 32, 48, 72, 128] {
            let ratio = (px - app_icon_inner_px(px)) as f32 / 2.0 / px as f32;
            assert!(
                (0.03..=0.08).contains(&ratio),
                "app_icon({px}) 单边留白占比 {ratio:.3} 应在 3%~8%"
            );
        }
    }

    /// 小尺寸的留白必须由 [`ICON_INSET_MIN`] 托底，不能退回按比例算出的 1px：
    /// 1px 逻辑留白在 150% 下只有 1.5 物理像素，吸收不掉落点吸附的位移。
    #[test]
    fn app_icon_small_size_uses_min_inset() {
        for px in [24, 28, 32] {
            assert_eq!(
                px - app_icon_inner_px(px),
                ICON_INSET_MIN * 2,
                "app_icon({px}) 应走 ICON_INSET_MIN 托底"
            );
        }
    }

    /// 极小尺寸不得算出非正边长（`.max(1)` 兜底），否则布局塌陷或 panic。
    #[test]
    fn app_icon_inner_px_stays_positive() {
        for px in 1..=16 {
            assert!(app_icon_inner_px(px) >= 1, "app_icon({px}) 实绘边长应 >= 1");
        }
    }
}
