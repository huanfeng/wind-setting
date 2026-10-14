//! 通用设置控件便捷函数 — panel / section_header / setting_row 等。
//!
//! 纯组合函数，仅供本项目用。视觉走 wind-ui-rust 的**角色化主题**（`*_role`），
//! 故所有控件随亮/暗主题自动适配。样式对齐 examples/ime.rs 的设置面板。

use std::cell::Cell;
use std::rc::Rc;

use windui::core::{EventCtx, Widget};
use windui::event::{Event, PointerKind};
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

thread_local! {
    /// 界面细节试验选项（shell::build 注册），同 `DARK_SIG`。见 [`crate::look`]。
    static LOOK: Cell<Option<crate::look::LookSignals>> = const { Cell::new(None) };
}

/// 注册界面细节信号（shell::build 调用）。
pub fn init_look(s: crate::look::LookSignals) {
    LOOK.with(|c| c.set(Some(s)));
}

/// 取界面细节信号；未注册（测试 / 独立构建）时给全缺省。
pub fn look() -> crate::look::LookSignals {
    LOOK.with(|c| c.get())
        .unwrap_or_else(|| crate::look::LookSignals::new(crate::look::DEFAULT))
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
/// 图标块底色 / 图标色走 accent 角色（淡底 `bg_role_alpha` + `tint_role`），换明暗与换强调色
/// 都自动跟随；图标按 section 名查 [`crate::icons::section_icon`]。
///
/// 标题 Label `weight(1.0)` + `max_lines(1)`：windui 的 Label 在「无显式宽度」时，
/// measure 收敛到文字紧贴宽，draw 再按该窄宽折行（见 ui/mod.rs 注释），导致明明有
/// 空间也换行、甚至 2 字标题也折。给足整行宽度并锁单行即可根治。
pub fn section_header(title: &str) -> Element {
    let svg = crate::icons::section_icon(title);
    let chip = Element::stack()
        .size(crate::icons::CHIP_PX, crate::icons::CHIP_PX)
        .corner(7.0)
        .bg_role_alpha(Role::Accent, 0.12)
        .child(
            crate::icons::tinted_role(svg, crate::icons::ICON_PX, Role::Accent)
                .align(Align::Center),
        );
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(9)
        .margin_xy(2, 4)
        .child(chip)
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

/// 悬停提示取自 `Signal` 的旁挂件。
///
/// [`Element::tooltip`] 收的是**构建期**的静态串，而「未保存了哪些项」要到关窗那一刻
/// 才知道。`Widget::tooltip()` 是绘制时按当前悬停节点现取的（见 windui `app::tooltip`
/// 的模块说明），正好接得住 Signal；同一节点上动态提示优先于静态文本。
///
/// ⚠️ 必须挂在**自己的叶子节点**上，不能指望父容器兜着：windui `hit_node` 的「吞命中」
/// 判据里有静态 `n.tooltip.is_some()`，**没有**动态提示这一项，所以透明容器挂上它
/// 永远不会触发、也不会有任何报错。带 widget 的叶子默认 `hit_opaque() == true`，
/// 这一条正是本部件成立的前提（同 `stats::charts` 的两张图）。
struct SignalTip {
    tip: Signal<String>,
    /// 点击回调。
    ///
    /// ⚠️ **必须自己接**：`Element::on_click` 是经 `Widget::take_click` 交给控件的，而
    /// 那个 trait 方法的默认实现是**空操作**（windui `core.rs`），只有交互控件才实现它。
    /// 不接就是把回调静默丢掉——不报错、类型也对，截图上什么都看不出来，只有去读剪贴板
    /// 才发现点了没反应。本仓第一版正是这么漏的。
    ///
    /// 走不了 `clickable()`：那个会把本节点的 widget 整个换成 `containers::Clickable`，
    /// 连带把动态提示挤掉（它自己的 `debug_assert` 也拦叶子控件）。同理用不了 `IconButton`
    /// ——一个节点只有一个 widget 槽，而命中只落在最上层那一个，提示与点击必须同源。
    on_click: Option<windui::core::ClickFn>,
    /// 是否处于按下态，用于 `Up` 时判定「按下与抬起在同一个节点内」。
    pressed: bool,
}

impl SignalTip {
    fn new(tip: Signal<String>) -> Self {
        Self {
            tip,
            on_click: None,
            pressed: false,
        }
    }
}

impl Widget for SignalTip {
    fn tooltip(&self) -> Option<String> {
        let s = self.tip.get();
        (!s.is_empty()).then_some(s)
    }

    /// 手型：这个角标是可以点的（复制清单），而光标是唯一的提示——它没有按钮外观，
    /// 也没有 hover 底色（那些都得换掉 widget 槽，见 [`Self::on_click`]）。
    fn cursor(&self) -> CursorShape {
        CursorShape::Hand
    }

    fn take_click(&mut self, f: windui::core::ClickFn) {
        self.on_click = Some(f);
    }

    /// 指针按下→抬起且仍在界内才算一次点击，与 `containers::Clickable` 同口径：
    /// 按下后拖出去再松手是「反悔」，不该触发。
    fn on_event(&mut self, ctx: &mut EventCtx, ev: &Event) -> bool {
        let Event::Pointer(p) = ev else {
            return false;
        };
        match p.kind {
            PointerKind::Down => {
                self.pressed = true;
                ctx.capture();
                true
            }
            PointerKind::Up => {
                let fire = self.pressed && ctx.bounds().contains(p.pos);
                self.pressed = false;
                ctx.release_capture();
                if fire && let Some(cb) = self.on_click.as_mut() {
                    cb(ctx);
                }
                true
            }
            _ => false,
        }
    }
}

/// 「未保存项清单」角标：info 图标 + 悬停列出 + 点击复制。
///
/// 为什么收进角标而不是铺在对话框正文里：绝大多数用户不关心哪几个键在动，正文里列
/// 一串 `input.auto_pair.english_pairs` 只会喧宾夺主；可它又恰恰是排查「什么都没动
/// 就说有未保存」的唯一现场线索，不能不给。收进一个图标，两边都成立。
///
/// 悬停看、点击取，这个分工是 tooltip 的形态定的：浮层跟着指针走、不可交互，鼠标
/// 移不进去，没法在里面选中文字——所以"复制"只能挂在图标的点击上。
///
/// `tip` 为空串时图标仍在，只是不弹提示（[`SignalTip::tooltip`] 返回 None）。调用方
/// 若不想让它出现，用 `visible_when` 收掉整个角标。
pub fn info_badge(tip: Signal<String>, on_click: impl FnMut(&mut EventCtx) + 'static) -> Element {
    // 亮/暗双态叠加：`tint` 收的是静态色，换肤不跟随，两套主题下的 muted 色不是同一个。
    let icon = |on_dark: bool| {
        let c = if on_dark {
            Color::hex(0x9AA4B2)
        } else {
            Color::hex(0x8A94A6)
        };
        crate::icons::tinted(crate::icons::INFO_THIN, 14, c)
            .visible_when(move || dark_sig().get() == on_dark)
    };
    Element::stack()
        .size(14, 14)
        .child(icon(false))
        .child(icon(true))
        // 提示层排在最后、尺寸与容器等大：`hit_node` 倒序测子节点，故它先于两个图标
        // 被命中，提示与点击都落在它身上。图标层因此永远轮不到命中——给图标再挂静态
        // 提示是死代码（`node_tooltip` 只沿**父链**上溯，够不着兄弟节点）。
        .child(
            Element::leaf()
                .widget(SignalTip::new(tip))
                .size(14, 14)
                .on_click(on_click),
        )
}

/// 带「可被方案覆盖」提示的设置行：标签后跟一个信息角标，悬停显示说明，点击跳转。
///
/// # 它回答什么
///
/// 「我在全局改了，为什么某个方案里没变」——因为那个方案自己声明了这一项。这是本仓
/// 配置模型里用户最常撞到的一层，而全局设置页此前**没有任何地方提示过它的存在**。
/// [`setting_row_override`] 已经在方案设置那一端把「这项被我覆盖了」标出来了，
/// 本函数补的是全局这一端。
///
/// # 为什么走 Signal 而不是静态串（R8.6 起）
///
/// 「可被方案覆盖」是静态事实（来自 core 的 `SCHEMA_OVERRIDES`），但「**已被**哪些方案
/// 覆盖」是运行期数据（`schema.overrideSummary`），且页面树建于数据到达之前——
/// 见 [`crate::state::AppState::override_summary`]。故 `tip` 必须是 `Signal<String>`，
/// 调用方按 [`crate::manifest::override_badge_text`] 现算，静态说明与「已被 N 个方案
/// 覆盖」共用同一枚角标，不再新增一个视觉元素。
///
/// # 为什么用中性色而不是警告色
///
/// 这里表达的是**可能性**（这一项存在方案级形态），不是**已发生**（某个方案确实改了它）。
/// 用琥珀会让每个装了码表方案的用户在一屏设置里看到几十个警告色角标——而其中绝大多数
/// 方案根本没声明那一项。真正「已发生」的提示归 [`setting_row_override`] 那一端——
/// 那里同样不用警告色：来源不是故障，见该函数的三态三色表。
///
/// 点击角标跳到（首个）覆盖方案的设置对话框——调用方传入的 `on_click` 通常就是
/// `schema_mgr.open_settings_at_id(id)`；覆盖列表为空时调用方应做成空操作，角标仍只是
/// 一个提示。
pub fn setting_row_schema_hint(
    label: &str,
    sub: Option<&str>,
    tip: Signal<String>,
    on_click: impl FnMut(&mut EventCtx) + 'static,
    control: Element,
) -> Element {
    setting_row_badged(label, sub, info_badge(tip, on_click), control)
}

/// 「这一项**不能**按方案单独设置」的行。
///
/// # 什么时候用它
///
/// 只在容器走 [`crate::manifest::SchemaHintMode::Header`] 时——那里顶部已统一说过「本页
/// 可被方案覆盖」，剩下的几项若不加区分，那句话就把它们说错了。**反向标记的前提是正向
/// 说明已经存在**；单独用它（在一个多数项都不可覆盖的页面上逐个标「仅全局」）就是把
/// 噪音换了个方向。
///
/// 用文字不用图标：它是「例外」，要一眼读到，不该藏在悬停里。
pub fn setting_row_global_only(label: &str, sub: Option<&str>, control: Element) -> Element {
    let badge = Element::label("仅全局")
        .font_size(11.0)
        .fg_role(Role::TextSubtle)
        .max_lines(1)
        .tooltip("这一项不分方案，改了对所有输入方案生效");
    setting_row_badged(label, sub, badge, control)
}

/// 标签后跟一枚角标的设置行。[`setting_row_schema_hint`] 与
/// [`setting_row_global_only`] 的共同骨架——两者只有角标不同，行的度量必须一致，
/// 否则同一个对话框里带标记的行会比不带的高出几像素。
fn setting_row_badged(label: &str, sub: Option<&str>, badge: Element, control: Element) -> Element {
    let th = windui::theme::current();
    let (f, m) = (&th.form, &th.metrics);
    // 标签与角标同排；标签取 weight(1.0) 把角标推到文字右边而不是行尾。
    let title = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(5)
        .child(
            Element::label(label)
                .font_size(f.label_size(m))
                .font_weight(f.label_weight())
                .fg_role(Role::Text)
                .max_lines(1)
                .truncate(Truncate::End),
        )
        .child(badge);
    let mut left = Element::col().weight(1.0).spacing(2).child(title);
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
        .cross(Align::Center)
        .padding_xy(0, f.row_pad_y())
        .spacing(f.gap())
        .child(left)
        .child(control)
}

/// 副标题随信号变化的设置行：左标签 + 下方动态说明 + 右控件。
///
/// 用于展示**运行期状态**（如 `windinput://` 关联当前指向谁）——库的
/// `setting_row_desc` 只收 `String`，套不进随注册表变化的文案。除副标题换成
/// `label_signal` 外，字号/文字色/间距/内边距一律走同一套 `FormTheme`，
/// 与相邻的清单行对得齐。
///
/// 与库版的一处有意偏离：**不挂 tooltip**。库的 `clamp_lines` 在截断时用
/// `tooltip(full_text)` 兜底看全文，而 `Element::tooltip` 收的是构建期的静态串，
/// 随注册表变化的文案给不了（要动态提示得走 [`info_badge`] 那条 `Widget::tooltip()`
/// 的路，为一行副标题不值当）。故限行放宽到 3，让带完整路径的长状态尽量显示完整；
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
/// | 方案作者定的 | 方案自带 | `Text`（满对比度正文色，「这项与全局不同，但不是你改的」） |
/// | 用户改的 | 已自定义 | `Accent`（与整个设置界面的主色一致，＝「你的改动」） |
///
/// 一律灰的话，「方案自带」在一屏「跟随全局」里毫无存在感——而它恰恰是最需要被看见的：
/// 用户改了全局却发现这个方案没跟着变，答案就在这一列。
///
/// 它曾用 `Warning`（琥珀）。醒目是够了，但语义错了：这一列描述的是**来源**，
/// 方案作者定了个值不是故障，橙字却让人以为这行报了错。改用满对比度正文色——
/// 在一列 `TextMuted` 灰字里同样一眼看得到，且不附带任何好坏判断。
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
    override_control_signal(
        over,
        gate,
        editor,
        signal(follow_text.to_string()),
        reason,
        width,
    )
}

/// [`override_control`] 的跟随值随数据变化的版本：外观页的「跟随主题」值要等
/// `theme.resolved` 回来、且切主题后会变，而页面树早于数据建成，只能绑信号。
pub fn override_control_signal(
    over: Signal<bool>,
    gate: std::rc::Rc<dyn Fn() -> bool>,
    editor: Element,
    follow_text: Signal<String>,
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
            Element::label_signal(follow_text)
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

/// 小标题右侧的状态摘要（「已自定义 N 项」/「全部跟随」），随信号实时变。
pub fn summary_label(text: Signal<String>) -> Element {
    let th = windui::theme::current();
    let (f, m) = (&th.form, &th.metrics);
    Element::label_signal(text)
        .font_size(f.desc_size(m))
        .fg_role(Role::TextMuted)
        .max_lines(1)
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
    card_with_attachment(rows, None)
}

/// [`card`] + 一个**尾部挂件**：平时不占位的元素（toast 槽之类），追加在卡片末尾但
/// **不参与分隔线计数**。
///
/// ★ 为什么必须与 `rows` 分开传：分隔线是按「不是最后一行就加」算的，而挂件隐藏时
/// 不占高度——把它塞进 `rows` 的后果是真正的最后一条设置行下面多出一条分隔线，
/// 紧贴卡片下边框，看着像画歪了。高级页那三张卡（系统集成 / 导入 / 备份与还原）
/// 曾经都是这样，三处同一个形状。
///
/// 判据是**这个元素会不会隐藏**，不是它长什么样：一个恒可见的尾行照旧进 `rows`。
pub fn card_with_attachment(rows: Vec<Element>, attachment: Option<Element>) -> Element {
    card_with_attachments(None, rows, attachment)
}

/// [`card`] + **头尾各一个**挂件。两端的挂件都不参与分隔线计数，理由见
/// [`card_with_attachment`]。
///
/// 头部挂件用于「平时不在、条件满足才跳出来」的整行提示（日志设置的隐私示警条），
/// 尾部用于 toast 槽。两者都不能混进 `rows`。
pub fn card_with_attachments(
    head: Option<Element>,
    rows: Vec<Element>,
    tail: Option<Element>,
) -> Element {
    card_items(None, head, rows_to_items(rows), tail)
}

/// 卡片内的一项：设置行，或二级分类小标题。
///
/// 小标题要与行区分开传，不能预先拼成 `Element` 混进行里：它前后的分割线怎么画、
/// 它自己长什么样，都随「界面细节」的 `subheading` 选项变（见 [`card_items`]）。
// 只在建树时短暂存在、随即被消费，变体大小悬殊无所谓；装箱反而让每个调用点多一层。
#[allow(clippy::large_enum_variant)]
pub enum CardItem {
    Row(Element),
    Heading(String),
    /// 自管水平内边距与内部分割线的整块（可拖列表、动态列表之类）：卡片原样挂上，
    /// 不包 [`card_pad`]。块内的行要自己用 [`card_pad`] 包、行间用 [`row_divider`]，
    /// 「通栏」分割线才贴得到卡片边。
    Bare(Element),
}

impl CardItem {
    /// 按卡片里的摆法拼成元素：行包 [`card_pad`]、小标题走统一写法、`Bare` 原样。
    /// 供需要把几项先并成一块（再作为 `Bare` 交给卡片）的调用方用。
    pub fn into_element(self) -> Element {
        match self {
            CardItem::Row(e) => padded(e),
            CardItem::Heading(t) => card_heading(&t),
            CardItem::Bare(e) => e,
        }
    }
}

fn rows_to_items(rows: Vec<Element>) -> Vec<CardItem> {
    rows.into_iter().map(CardItem::Row).collect()
}

/// 卡片水平内边距。分割线的「通栏 / 仅左缩」变体要贴到卡片边上，故卡片本身不加水平
/// padding，改由每一项自己承担（[`padded`]）。
const CARD_PAD_X: i32 = 16;

fn padded(e: Element) -> Element {
    Element::col()
        .width_match()
        .padding_xy(CARD_PAD_X, 0)
        .child(e)
}

/// 卡片里一行的水平内边距（[`CardItem::Bare`] 块内的行用）。
pub fn card_pad(e: Element) -> Element {
    padded(e)
}

/// 行与行之间的分割线，跟随「界面细节」`row_divider`。供不走 [`card_items`] 的列表
///（可拖列表、动态列表）在行间使用；容器本身不能再加水平 padding，否则「通栏」贴不到边。
pub fn row_divider() -> Element {
    gap_divider(Gap::Between)
}

/// 列表里的一行 = 顶部分割线 + 带卡片内边距的行本体。
///
/// 给可拖 / 动态列表用：分割线做进**每一行的顶部**（含首行），拖动中行重新堆叠时任何
/// 排列都对。容器本身别再加水平 padding——否则「通栏」分割线贴不到边。
pub fn list_row(e: Element) -> Element {
    Element::col()
        .width_match()
        .child(row_divider())
        .child(padded(e))
}

/// 列表里的二级分类小标题（含前后分割线），与 [`CardItem::Heading`] 同一套写法。
/// 前后两条线只在「强调色+线」下出现，与卡片里一致。
pub fn list_heading(title: &str) -> Element {
    list_heading_ext(title, None, false)
}

/// [`list_heading`] 完整形态：`trailing` 挂在标题右侧（三种写法各建一份）；`first` 为
/// 卡片里的第一项时不画上方那条线——卡片顶边已经是分隔，与 [`card_items`] 首项一致。
pub fn list_heading_ext(
    title: &str,
    trailing: Option<&dyn Fn() -> Element>,
    first: bool,
) -> Element {
    let mut col = Element::col().width_match();
    if !first {
        col = col.child(gap_divider(Gap::AroundHeading));
    }
    col.child(card_heading_with(title, trailing))
        .child(gap_divider(Gap::AroundHeading))
}

/// 相邻两项之间的间隙类型。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Gap {
    /// 行—行。
    Between,
    /// 至少一侧是小标题。
    AroundHeading,
}

/// `kinds[i]` = 第 i 项是不是小标题。返回 `kinds.len() - 1` 个间隙（不足两项时为空）。
fn divider_plan(kinds: &[bool]) -> Vec<Gap> {
    kinds
        .windows(2)
        .map(|w| {
            if w[0] || w[1] {
                Gap::AroundHeading
            } else {
                Gap::Between
            }
        })
        .collect()
}

/// 一个间隙上的分割线。「界面细节」试验：三种画法（两端内缩 / 通栏 / 仅左缩）都建出来，
/// 按 `row_divider` 只显示一个；挨着小标题的间隙在「灰字+留白」「浅色底条」下整条不画——
/// 那两种小标题自己就是分隔。
fn gap_divider(gap: Gap) -> Element {
    let look = look();
    let rd = look.0[crate::look::ROW_DIVIDER];
    let sh = look.0[crate::look::SUBHEADING];
    let shown = move || gap == Gap::Between || sh.get() == 0;
    Element::col()
        .width_match()
        .child(padded(Element::divider()).visible_when(move || shown() && rd.get() == 0))
        .child(Element::divider().visible_when(move || shown() && rd.get() == 1))
        .child(
            Element::col()
                .width_match()
                .padding_edges(Insets::new(CARD_PAD_X, 0, 0, 0))
                .child(Element::divider())
                .visible_when(move || shown() && rd.get() == 2),
        )
}

/// 统一的小标题文字：对话框分组名、方案配置卡里的分类名、覆盖卡片标题都用它。
///
/// 此前三处各写各的（字号 13.5 / 12.5 混用），同一种东西看着像两种。
///
/// ⚠ 不带 `weight`：weight 沿父容器主轴伸展，放进纵向 col 会在**纵轴**上被压成 0 高、
/// 整个标题消失。放进横向行（右侧挂按钮）时由调用方补 `.weight(1.0)`。
pub fn subheading_label(title: &str) -> Element {
    Element::label(title)
        .font_size(crate::manifest::DIALOG_HEADER_SIZE)
        .font_weight(600)
        .fg_role(Role::Accent)
        .max_lines(1)
}

/// 卡内二级分类。「界面细节」试验：三种写法按 `subheading` 显示一个——
/// 强调色字（现状，前后靠分割线隔开）/ 次要色小字 + 上方留白 / 通栏浅色底条。
fn card_heading(title: &str) -> Element {
    card_heading_with(title, None)
}

/// [`card_heading`] + 右侧挂件（摘要文字之类）。三种写法各要一份，故传构造闭包。
fn card_heading_with(title: &str, trailing: Option<&dyn Fn() -> Element>) -> Element {
    let sh = look().0[crate::look::SUBHEADING];
    let line = |label: Element| {
        let r = Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(8)
            .child(label.weight(1.0));
        match trailing {
            Some(make) => r.child(make()),
            None => r,
        }
    };
    let accent =
        padded(line(subheading_label(title)).padding_xy(0, 5)).visible_when(move || sh.get() == 0);
    let muted = Element::col()
        .width_match()
        .padding_edges(Insets::new(CARD_PAD_X, 16, CARD_PAD_X, 4))
        .child(line(
            Element::label(title)
                .font_size(12.0)
                .font_weight(600)
                .fg_role(Role::TextMuted)
                .max_lines(1),
        ))
        .visible_when(move || sh.get() == 1);
    let band = Element::col()
        .width_match()
        .padding_xy(CARD_PAD_X, 6)
        .bg_role_alpha(Role::Accent, 0.07)
        .child(line(
            Element::label(title)
                .font_size(crate::manifest::DIALOG_HEADER_SIZE)
                .font_weight(600)
                .fg_role(Role::Text)
                .max_lines(1),
        ))
        .visible_when(move || sh.get() == 2);
    Element::col()
        .width_match()
        .child(accent)
        .child(muted)
        .child(band)
}

/// 卡片外壳：圆角 surface，边框 / 阴影按「界面细节」`card_edge` 取舍。
///
/// ★ 这一项**建树时读一次**，改了要重开设置窗才生效（子窗里注明）。边框与阴影都不能绑
/// 信号；若改成「底板 / 边框各一层叠放、按信号显隐」，内容就不再是带边框容器的后代——
/// 设置向导的分隔线护栏（`wizard::view::tests::divider_violations`）正是靠「最近的带边框
/// 祖先」认卡片的，叠放写法会让它把整页当成一张卡、静默失效。
fn card_shell(body: Element) -> Element {
    card_frame().width_match().child(body)
}

/// 卡片底板（无内容）：圆角 surface + 按 `card_edge` 取舍的边框 / 阴影，建树时读一次。
/// 给不走 [`card_items`] 但外观应与设置卡片一致的大块容器用（词库表格外框）。
pub fn card_frame() -> Element {
    let ce = look().0[crate::look::CARD_EDGE].get();
    let mut card = Element::col().bg_role(Role::Surface).corner(13.0);
    if ce != 2 {
        card = card.border_role(Role::Border, 1);
    }
    if ce != 1 {
        card = card.shadow(Shadow::new(0.0, 1.0, 2.0, Color::rgba(20, 30, 50, 8)));
    }
    card
}

/// 卡片的完整形态：可选标题块（卡内分组标题）+ 头部挂件 + 若干项 + 尾部挂件。
///
/// 只有 `items` 之间插分割线；挂件不参与（理由见 [`card_with_attachment`]）。
/// `title_block` 由 [`section_impl`] 给，自带水平内边距和其下那条通栏线，这里原样挂上。
pub fn card_items(
    title_block: Option<Element>,
    head: Option<Element>,
    items: Vec<CardItem>,
    tail: Option<Element>,
) -> Element {
    let kinds: Vec<bool> = items
        .iter()
        .map(|i| matches!(i, CardItem::Heading(_)))
        .collect();
    let gaps = divider_plan(&kinds);
    let mut body = Element::col().width_match().padding_xy(0, 2);
    if let Some(t) = title_block {
        body = body.child(t);
    }
    if let Some(h) = head {
        body = body.child(padded(h));
    }
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            body = body.child(gap_divider(gaps[i - 1]));
        }
        body = body.child(item.into_element());
    }
    if let Some(t) = tail {
        body = body.child(padded(t));
    }
    card_shell(body)
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
///
/// `action` 传**构造闭包**：「界面细节」`section_header` 下标题有卡外 / 卡内两处，
/// 按钮两处各要一份（一个 `Element` 只能挂一处）。
pub fn dialog_section_full(
    title: &str,
    hint: Option<&str>,
    action: Option<&dyn Fn() -> Element>,
    rows: Vec<Element>,
) -> Element {
    dialog_section_items(title, hint, action, rows_to_items(rows))
}

/// [`dialog_section_full`]，卡内是 [`CardItem`] 序列（可含 [`CardItem::Bare`] 动态块）。
///
/// 「界面细节」`section_header`：0 = 小标题在卡外（现状）；1 / 2 = 标题行进卡片、其下
/// 一条通栏线（对话框的节没有图标，两档相同），与主窗 [`section`] 的卡内写法一致。
pub fn dialog_section_items(
    title: &str,
    hint: Option<&str>,
    action: Option<&dyn Fn() -> Element>,
    items: Vec<CardItem>,
) -> Element {
    let sh = look().0[crate::look::SECTION_HEADER];
    let hint_label = |h: &str| {
        Element::label(h)
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(2)
            .padding_xy(0, 2)
    };
    let outside_head: Element = match action {
        // 整宽 label 塞不进带按钮的行，故用不带 `width_match` 的 [`subheading_label`]。
        Some(make) => Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(10)
            .child(subheading_label(title).weight(1.0).padding_xy(0, 5))
            .child(make()),
        None => crate::manifest::dialog_group_header(title),
    };
    let mut outside = Element::col().width_match().spacing(2).child(outside_head);
    if let Some(h) = hint {
        outside = outside.child(hint_label(h));
    }

    let mut inside_row = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(10)
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .weight(1.0)
                .max_lines(1)
                .height(20),
        );
    if let Some(make) = action {
        inside_row = inside_row.child(make());
    }
    let mut inside = Element::col()
        .width_match()
        .spacing(2)
        .padding_xy(CARD_PAD_X, 10)
        .child(inside_row);
    if let Some(h) = hint {
        inside = inside.child(hint_label(h));
    }
    let title_block = Element::col()
        .width_match()
        .child(inside)
        .child(Element::divider())
        .visible_when(move || sh.get() != 0);

    Element::col()
        .width_match()
        .spacing(2)
        .padding_xy(0, 4)
        .child(outside.visible_when(move || sh.get() == 0))
        .child(card_items(Some(title_block), None, items, None))
}

/// 分组 = 分区标题 + 卡片（承载各设置行）。对齐 ime.rs 的 section 卡片观感。
/// `rows` 为已构建好的设置行（如 `setting_row(...)` 或清单引擎 `field(...)`）。
pub fn section(title: &str, rows: Vec<Element>) -> Element {
    section_impl(title, None, None, rows_to_items(rows), None)
}

/// [`section`] + 标题行右侧的动作按钮（如「恢复默认」），卡内是带小标题的 [`CardItem`]
/// 序列（方案配置卡）。
///
/// 按钮挂在标题行而不是卡片末尾另起一行：卡片里每一行都是「设置项」，多出一行只有一个
/// 按钮的行会被读成又一个设置项；标题行右端则天然读作「这一节的操作」。
///
/// `action` 传**构造闭包**：「界面细节」试验下标题有卡外 / 卡内两处，按钮两处各要一份
/// （一个 `Element` 只能挂一处）。
pub fn section_with_items(
    title: &str,
    action: &dyn Fn() -> Element,
    items: Vec<CardItem>,
) -> Element {
    section_impl(title, Some(action), None, items, None)
}

/// [`section`]，卡内是 [`CardItem`] 序列（含 [`CardItem::Bare`] 整块）。
pub fn section_items(title: &str, items: Vec<CardItem>) -> Element {
    section_impl(title, None, None, items, None)
}

/// [`section`] + 尾部挂件（toast 槽之类平时不占位的元素）。理由见
/// [`card_with_attachment`]——挂件混进 `rows` 会让最后一条设置行下面多一条分隔线。
pub fn section_with_attachment(title: &str, rows: Vec<Element>, attachment: Element) -> Element {
    section_impl(title, None, None, rows_to_items(rows), Some(attachment))
}

/// [`section`] + 头部挂件（条件满足才出现的整行提示）。理由同上。
pub fn section_with_head(title: &str, head: Option<Element>, rows: Vec<Element>) -> Element {
    section_impl(title, None, head, rows_to_items(rows), None)
}

/// 全部 `section*` 的实现。「界面细节」试验 `section_header`：
/// 0 卡外标题（现状）/ 1 卡内标题行 / 2 卡内标题行、不带图标。三种都建出来按信号显示。
fn section_impl(
    title: &str,
    action: Option<&dyn Fn() -> Element>,
    head: Option<Element>,
    items: Vec<CardItem>,
    tail: Option<Element>,
) -> Element {
    let sh = look().0[crate::look::SECTION_HEADER];
    let with_action = |row: Element| match action {
        Some(make) => row.child(make()),
        None => row,
    };
    let outside = with_action(section_header(title)).visible_when(move || sh.get() == 0);
    let inside_icon =
        with_action(section_header(title).margin_xy(0, 0)).visible_when(move || sh.get() == 1);
    let inside_plain = with_action(
        Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(9)
            .child(
                Element::label(title)
                    .font_size(14.0)
                    .font_weight(600)
                    .fg_role(Role::Text)
                    .weight(1.0)
                    .max_lines(1)
                    .height(20),
            ),
    )
    .visible_when(move || sh.get() == 2);
    // 标题行与其下那条通栏线整体显隐：卡外模式下两者都不该占位。
    let title_block = Element::col()
        .width_match()
        .child(
            Element::col()
                .width_match()
                .padding_xy(CARD_PAD_X, 10)
                .child(inside_icon)
                .child(inside_plain),
        )
        .child(Element::divider())
        .visible_when(move || sh.get() != 0);
    Element::col()
        .width_match()
        .spacing(6)
        .child(outside)
        .child(card_items(Some(title_block), head, items, tail))
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

// ── 加/改词表单的尾部小按钮 ────────────────────────────────────────────────
//
// 两处加词界面（快捷加词小窗 `add_word_window`、词库页加/改词对话框 `pages::dict::dialogs`）
// 共用这一组，连宽度一起共用：`ENCODE_BTN_W == TEXT_BTN_W * 2 + 8`（两按钮间距 8），
// 「编码」行与「词条」行的输入框右边线才能对齐。改宽度改这里一处。

/// 「粘贴」「清空」单个按钮的最小宽度。
pub const TEXT_BTN_W: i32 = 46;
/// 「生成编码」按钮的最小宽度（= 两个 [`TEXT_BTN_W`] + 一个 8px 间距）。
pub const ENCODE_BTN_W: i32 = TEXT_BTN_W * 2 + 8;

/// 词条输入框尾部的「粘贴」按钮：从剪贴板取文本**整体替换** `text`。
///
/// `single_line=true` 时抹掉换行——单行 `text_input` 显示不了换行，粘一段多行文本进去
/// 会得到一串看不见的控制字符，直到提交才发现词条不对。多行框只归一 `\r\n` → `\n`。
///
/// 用 `min_width` 而非 `width`：定宽会把文字裁掉（同下拉那个坑），这里只要一个下限来对齐。
pub fn paste_button(text: Signal<String>, single_line: bool) -> Element {
    Element::button("粘贴")
        .small()
        .outline_soft()
        .min_width(TEXT_BTN_W)
        .on_click(move |ctx| {
            let raw = ctx.clipboard_get().unwrap_or_default();
            let s = if single_line {
                raw.replace(['\r', '\n'], "")
            } else {
                raw.replace("\r\n", "\n").replace('\r', "\n")
            };
            let s = s.trim().to_string();
            if s.is_empty() {
                ctx.toast("剪贴板里没有可粘贴的文本");
                return;
            }
            text.set(s);
        })
}

/// 词条输入框尾部的「清空」按钮：只清词条本身。
///
/// 编码不在这里清，但**会跟着空掉**——`auto_encode` 那边把「词条为空」当成「没有可算的
/// 东西」，自动把编码一并清掉（前提是用户没手改过编码）。两件事分开写是有意的：清空按钮
/// 只管自己那个框，编码怎么跟随由自动出码一处说了算，逐字删到空与点清空因此表现一致。
pub fn clear_text_button(text: Signal<String>) -> Element {
    Element::button("清空")
        .small()
        .outline_soft()
        .min_width(TEXT_BTN_W)
        .on_click(move |_| text.set(String::new()))
}

// ── 色值（`#RRGGBB` / `#RRGGBBAA`）：manifest 的 color 控件与角标规则对话框共用 ──

/// 色值文本 → `(RGB, 不透明度)`。非法值（含语言栏角标的 `auto`）返回 `None`。
///
/// 第二项 `None` = **没写末两位**，这一条的不透明度跟随全局 `badge_alpha`。
/// 这个区别必须在这里就分出来：写了 `FF` 与没写不是一回事，前者会把这一条切到
/// 挖空档（角标实心 + 周围切掉一圈主字），core 侧的判据同样是原串长度。
///
/// 只认 `#RRGGBB` / `#RRGGBBAA` 两种长度，与 core 的 `parse_hex` 逐字对齐：这里放宽
/// （比如认 `#RGB` 短写）会让用户在设置页看着好好的、装到任务栏上却退回了 auto。
pub fn parse_color(s: &str) -> Option<(u32, Option<u8>)> {
    // `#` 可省：核心 `wind_theme::palette::parse_hex` 也不强制它，两边认定一致，
    // 手写 `FF0000` 在核心里生效、这里却报红的话，界面与实际对不上。
    let t = s.trim();
    let hex = t.strip_prefix('#').unwrap_or(t);
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok().map(|v| (v, None)),
        8 => Some((
            u32::from_str_radix(&hex[0..6], 16).ok()?,
            Some(u8::from_str_radix(&hex[6..8], 16).ok()?),
        )),
        _ => None,
    }
}

/// `#RRGGBB` / `#RRGGBBAA` 的规范写法（十六进制大写、去首尾空白）；非法返回 `None`。
///
/// **不补也不删末两位**：长度本身是一位信息（写了 `FF` 与没写在 core 侧含义不同）。
pub fn normalize_hex_color(s: &str) -> Option<String> {
    match parse_color(s)? {
        (rgb, None) => Some(format!("#{rgb:06X}")),
        (rgb, Some(a)) => Some(format!("#{rgb:06X}{a:02X}")),
    }
}

/// 一小块色样。非法值（含 `auto` 与空串）画成描边空块——颜色是这一屏最该一眼看清的东西。
///
/// **按色值自带的不透明度半透明绘制**：透明度是这次新加的自由度，只写在副标题里
/// 用户得自己把 `CC` 换算成百分比才知道多淡，而色样直接把结果摆出来。没写末两位的
/// 画成不透明——那种情况的实际淡度由全局值决定，这一屏没有权威可依。
pub fn color_swatch(color: &str) -> Element {
    let base = Element::leaf().width(14).height(14).corner(3.0);
    match parse_color(color) {
        Some((v, a)) => base.bg(Color::rgba(
            ((v >> 16) & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
            a.unwrap_or(0xFF),
        )),
        None => base.border_role(Role::Border, 1),
    }
}

/// 色值文本 → 取色器颜色。非法值（空串、`auto` 等）用 `fallback`；没写末两位按不透明。
pub fn color_from_value(s: &str, fallback: Color) -> Color {
    match parse_color(s) {
        Some((v, a)) => Color::rgba(
            ((v >> 16) & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
            a.unwrap_or(0xFF),
        ),
        None => fallback,
    }
}

/// 一个「取色器 ↔ 配置字符串」的绑定：主字颜色、角标颜色两处共用。
///
/// # ★ 取色器绑的是 `Color`，配置存的是字符串，两边靠「版本回声」隔开
///
/// 特殊值（主字色的 `""` 跟随主题、角标的 `auto`）在取色器里也得显示个颜色，而那个颜色
/// **不是用户选的**。若把 `Color → 字符串` 做成无条件同步，程序自己写进去的颜色（播种 /
/// 加载 / 恢复默认 / 点特殊值按钮）会被当成用户选色写回，特殊值就再也存不住了。故程序每次
/// 写 `color` 后记下它的版本号，同步时版本没变就不写回——只有用户在面板上动过，版本才前进。
/// 不用「颜色相等」判：用户完全可能恰好挑中回落色。
#[derive(Clone)]
pub struct PickedColor {
    /// 配置值。写回 / 取值 / 摘要都读它。
    pub value: Signal<String>,
    /// 取色器绑定的颜色。
    pub color: Signal<Color>,
    /// 程序最后一次写 `color` 后的版本号。
    echo_ver: Rc<Cell<u64>>,
}

impl PickedColor {
    pub fn new(value: &str, shown: Color) -> Self {
        let pc = Self {
            value: signal(value.to_string()),
            color: signal(shown),
            echo_ver: Rc::new(Cell::new(0)),
        };
        pc.echo_ver.set(pc.color.version());
        pc
    }

    /// 程序侧设置：值与取色器显示色一起改，并记下回声版本，免得同步把它当成用户选色。
    pub fn set(&self, value: &str, shown: Color) {
        self.value.set(value.to_string());
        self.color.set(shown);
        self.echo_ver.set(self.color.version());
    }

    /// 取色器 → 配置值：用户动过才调 `to_value(旧值, 新色)` 写回；程序写入不回写。
    pub fn sync(&self, to_value: impl FnOnce(&str, Color) -> String) {
        let ver = self.color.version();
        if ver == self.echo_ver.get() {
            return;
        }
        self.echo_ver.set(ver);
        let v = to_value(&self.value.get(), self.color.get());
        self.value.set(v);
    }
}

/// 一格取色：色块取色器 + 当前值文字（`text` 由值算出，宽 `text_w`）+ 若干按钮。
/// 用户在面板上选色时按 `to_value` 写回 [`PickedColor::value`]。
///
/// `alpha`：面板是否带透明度条。**必须**与 `to_value` 是否会把 `Color::a` 编进配置值
/// 对齐——留 `true` 却让 `to_value` 扔掉 alpha，用户拖了条、色块跟着变淡，一松手
/// 又变回不透明，会被当成"控件坏了"（主字颜色对话框两者都是 `true`；角标规则的
/// 颜色格透明度走独立勾选行，这里必须是 `false`，同 `badge_value_of` 的说明）。
pub fn color_picker_cell(
    pc: &PickedColor,
    text: impl Fn(&String) -> String + 'static,
    text_w: i32,
    to_value: impl Fn(&str, Color) -> String + 'static,
    alpha: bool,
    buttons: Vec<Element>,
) -> Element {
    let colors = pc.color;
    let pc_s = pc.clone();
    let sync = crate::pages::dict::reactive::reactive_effect(
        move || colors.version(),
        move || pc_s.sync(&to_value),
    );
    let mut row = Element::row()
        .cross(Align::Center)
        .spacing(8)
        .child(Element::color_picker_opts(
            pc.color,
            ColorPickerOpts::default().trigger_text(false).alpha(alpha),
        ))
        .child(
            Element::label_signal(pc.value.map(text))
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width(text_w)
                .max_lines(2),
        );
    for b in buttons {
        row = row.child(b);
    }
    row.child(sync)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 小标题前后的间隙要能单独区分：「灰字+留白」「浅色底条」两种小标题下这些间隙不画线。
    #[test]
    fn divider_plan_marks_gaps_touching_headings() {
        // 小标题, 行, 行, 小标题, 行
        let kinds = [true, false, false, true, false];
        assert_eq!(
            divider_plan(&kinds),
            vec![
                Gap::AroundHeading,
                Gap::Between,
                Gap::AroundHeading,
                Gap::AroundHeading
            ]
        );
    }

    /// 一项或零项没有间隙，不能凭空多出分割线。
    #[test]
    fn divider_plan_single_item_has_no_gaps() {
        assert!(divider_plan(&[false]).is_empty());
        assert!(divider_plan(&[]).is_empty());
    }

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
