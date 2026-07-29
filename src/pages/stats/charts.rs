//! 统计页的自绘图表部件。
//!
//! 这些图形没有对应的内建控件，且用 `Element` 堆砌代价明显：
//! 占比条要能自定义高度与配色（`Element::progress` 固定 6px 且只认主题 accent，
//! 三组图表就无法靠颜色区分）。故实现 `Widget` 直接走 `Canvas` 图元。
//!
//! 配色：图表色属于品牌语义色（"绿=多、橙=字符、靛=方案"），不在主题 palette 里，
//! 因此每种色备明暗两个值，`paint` 时按 [`crate::widgets::dark_sig`] 取——
//! 运行期换肤自动跟随，不需要重建节点。

use windui::core::Widget;
use windui::geometry::{Color, Rect, Size};
use windui::prelude::*;
use windui::render::{Canvas, Paint};
use windui::style::Style;
use windui::text::TextEngine;

use crate::widgets::dark_sig;

/// 图表配色：`(亮色主题值, 暗色主题值)`。暗色下统一提亮，保证深底上的对比度。
pub type ChartColor = (u32, u32);

/// 绿——「量」的主色（码长分布、热力图色阶、时段柱）。
pub const GREEN: ChartColor = (0x30A1_4E, 0x39D3_53);
/// 橙——字符分类。
pub const AMBER: ChartColor = (0xF59E_0B, 0xFBBF_24);
/// 靛蓝——方案占比。
pub const INDIGO: ChartColor = (0x6366_F1, 0x818C_F8);

/// 按当前明暗取色。
pub fn pick(c: ChartColor) -> Color {
    Color::hex(if dark_sig().get() { c.1 } else { c.0 })
}

/// 占比条行高（逻辑 px）。对齐参考稿的 16px 轨道。
const BAR_H: i32 = 16;
/// 占比条圆角。
const BAR_R: f32 = 4.0;

/// 水平占比条：同色淡底轨道 + 实心填充。
///
/// 宽度由父容器给（自身 `measure` 宽度报 0，靠 `.weight(1.0)` 撑开），
/// 故行内可以「固定宽标签 + 弹性条 + 固定宽百分比」三段布局。
struct BarTrack {
    /// 0.0..=1.0。
    ratio: f32,
    color: ChartColor,
}

impl Widget for BarTrack {
    fn measure(&self, _avail: Size, _style: &Style, _text: &mut dyn TextEngine) -> Size {
        Size::new(0, BAR_H)
    }

    fn paint(
        &self,
        bounds: Rect,
        _content: Rect,
        _focused: bool,
        _enabled: bool,
        canvas: &mut dyn Canvas,
        _style: &Style,
    ) {
        let color = pick(self.color);
        let (x, y, w) = (bounds.x as f32, bounds.y as f32, bounds.w as f32);
        let h = BAR_H.min(bounds.h) as f32;
        let y = y + (bounds.h as f32 - h) / 2.0;

        canvas.fill_round_rect(x, y, w, h, BAR_R, &Paint::fill(color.scale_alpha(0.14)));
        if self.ratio > 0.0 {
            // 极小占比也留一段可见宽度，否则 1% 的项看起来和 0 一样。
            let fw = (w * self.ratio.clamp(0.0, 1.0)).max(BAR_R * 2.0).min(w);
            canvas.fill_round_rect(x, y, fw, h, BAR_R, &Paint::fill(color));
        }
    }
}

/// 占比条元素（宽度弹性，需放进 `row` 并由兄弟节点定住左右两栏）。
pub fn bar_track(ratio: f32, color: ChartColor) -> Element {
    Element::leaf()
        .widget(BarTrack { ratio, color })
        .weight(1.0)
        .height(BAR_H)
}

/// 完整的一行占比条：`标签 · 条 · 百分比`。
///
/// 左右两栏定宽，中间条弹性——窗口变宽时只有条变长，标签与数字不会跟着漂移。
pub fn bar_row(label: &str, pct: u32, color: ChartColor) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::label(label)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width(64)
                .max_lines(1)
                .truncate(Truncate::End)
                .text_align(Align::End),
        )
        .child(bar_track(pct as f32 / 100.0, color))
        .child(
            Element::label(format!("{pct}%"))
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width(36)
                .max_lines(1)
                .text_align(Align::End),
        )
}
