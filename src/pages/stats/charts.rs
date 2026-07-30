//! 统计页的自绘图表部件。
//!
//! 这些图形没有对应的内建控件，且用 `Element` 堆砌代价明显：
//! 占比条要能自定义高度与配色（`Element::progress` 固定 6px 且只认主题 accent，
//! 三组图表就无法靠颜色区分）。故实现 `Widget` 直接走 `Canvas` 图元。
//!
//! 配色：图表色属于品牌语义色（"绿=多、橙=字符、靛=方案"），不在主题 palette 里，
//! 因此每种色备明暗两个值，`paint` 时按 [`crate::widgets::dark_sig`] 取——
//! 运行期换肤自动跟随，不需要重建节点。

use std::rc::Rc;

use windui::core::{EventCtx, Widget};
use windui::event::{Event, PointerKind};
use windui::geometry::{Color, Rect, Size};
use windui::prelude::*;
use windui::render::{Canvas, Paint};
use windui::style::Style;
use windui::text::{TextEngine, TextStyle};

use super::data;
use crate::widgets::dark_sig;

/// 图表配色：`(亮色主题值, 暗色主题值)`。暗色下统一提亮，保证深底上的对比度。
pub type ChartColor = (u32, u32);

/// 绿——「量」的主色（码长分布、热力图色阶、时段柱）。
pub const GREEN: ChartColor = (0x30A14E, 0x39D353);
/// 橙——字符分类。
pub const AMBER: ChartColor = (0xF59E0B, 0xFBBF24);
/// 靛蓝——方案占比。
pub const INDIGO: ChartColor = (0x6366F1, 0x818CF8);

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

// ───────────────────────── 日历热力图 ─────────────────────────

/// 热力图色阶（GitHub 风格 5 档）：亮色一套、暗色一套。
const HEAT_LIGHT: [u32; 5] = [0xEBEDF0, 0x9BE9A8, 0x40C463, 0x30A14E, 0x216E39];
const HEAT_DARK: [u32; 5] = [0x161B22, 0x0E4429, 0x006D32, 0x26A641, 0x39D353];

/// 格子最小/最大边长。小于最小值宁可少显示几周，也不挤成看不清的点。
const CELL_MIN: f32 = 8.0;
const CELL_MAX: f32 = 14.0;
/// 格子间距。
const CELL_GAP: f32 = 3.0;
/// 左侧星期标签列宽。
const WEEKDAY_W: f32 = 16.0;
/// 网格底部到图例带的间距。
const LEGEND_GAP: f32 = 14.0;
/// 图例带高度。
const LEGEND_BAND_H: f32 = 16.0;
/// 星期标签（与 `data::weekday_mon0` 同为周一起始）。
const WEEKDAY_LABELS: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];

/// 绘制文字用的浮点矩形（`Canvas` 只收整数 `Rect`，这里保留亚像素定位再统一取整）。
#[derive(Debug, Clone, Copy)]
struct TextBox {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl TextBox {
    fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

/// 一次布局解算的结果。
///
/// `measure` / `paint` / `on_event` 必须看到同一套几何，故抽成纯函数各自调用，
/// 而不是在 measure 里算完存进 `self`——`paint` 拿到的是 `&self`，存不回去。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Grid {
    /// 格子边长。
    cell: f32,
    /// 格子步进（边长 + 间距）。
    step: f32,
    /// 实际绘制的列数（周数）。
    cols: usize,
    /// 前面被舍弃的天数：开头不满一周的零头 + 宽度不够时多丢的整周。
    skip_days: usize,
}

/// 按可用宽度解算网格。
///
/// windui 的 `scroll()` 只有纵向，横向滚动不可用，因此不能像网页版那样固定格子大小
/// 再横滚——只能反过来让**格子尺寸随宽度自适应**，窄到放不下半年就自动少显示几周。
/// 这样 560px 的最小窗口也不会溢出。
fn grid_for(width: f32, days: usize, first_weekday: usize) -> Grid {
    let grid_w = (width - WEEKDAY_W).max(CELL_MIN);
    // 开头不满一周的零头直接丢弃，首列必从周一起始。
    //
    // 左上角的缺口与右下角的缺口性质不同：右下是「日期还没到」，属日历的正常留白；
    // 左上只是「统计窗口的起点恰好落在周中」，留着就是块无意义的空洞。
    // 代价是最旧的至多 6 天不显示。
    let head = (7 - first_weekday % 7) % 7;
    let usable = days.saturating_sub(head);
    let want_cols = usable.div_ceil(7).max(1);
    // +CELL_GAP：末列不需要尾随间距，故可用宽度按「含一个虚拟尾隙」折算。
    let max_cols = (((grid_w + CELL_GAP) / (CELL_MIN + CELL_GAP)).floor() as usize).max(1);
    let cols = want_cols.min(max_cols);
    let step = ((grid_w + CELL_GAP) / cols as f32).min(CELL_MAX + CELL_GAP);
    let cell = (step - CELL_GAP).clamp(CELL_MIN, CELL_MAX);
    // 宽度不够时再往后丢整周，首列仍落在周一。
    let skip_days = (head + (want_cols - cols) * 7).min(days);
    Grid {
        cell,
        step,
        cols,
        skip_days,
    }
}

/// 日历热力图：一列一周、一行一个星期几，格子深浅表示当日输入量。
///
/// 用自绘而非 180+ 个 `Element`：其一，节点与信号数会随天数线性膨胀；
/// 其二，逐格提示要求每格都是独立节点，自绘只需在 `on_event` 里按坐标反算命中格，
/// 再由 [`Widget::tooltip`] 交给框架弹浮层。
struct Heatmap {
    /// `(日期, 当日总字数)`，按日期升序且连续（core 已补齐空白日）。
    days: Rc<Vec<(String, u32)>>,
    /// 首日的周内序号（周一 = 0）。
    first_weekday: usize,
    /// 当前命中的天索引，`tooltip()` 据此给文案。
    last_hit: std::cell::Cell<Option<usize>>,
}

impl Heatmap {
    /// 网格总高度（7 行 + 间距 + 图例带）。
    fn height_of(g: &Grid) -> f32 {
        7.0 * g.step - CELL_GAP + LEGEND_GAP + LEGEND_BAND_H
    }

    /// 命中检测：控件内相对坐标 → 天索引。
    fn hit(&self, g: &Grid, rel_x: f32, rel_y: f32) -> Option<usize> {
        let gx = rel_x - WEEKDAY_W;
        if gx < 0.0 || rel_y < 0.0 {
            return None;
        }
        let col = (gx / g.step).floor() as usize;
        let row = (rel_y / g.step).floor() as usize;
        if col >= g.cols || row >= 7 {
            return None;
        }
        // 落在格子间隙上不算命中，否则相邻格会互抢边缘像素。
        if gx - col as f32 * g.step > g.cell || rel_y - row as f32 * g.step > g.cell {
            return None;
        }
        // 首列从周一起始、无前导空位，故槽位与天索引只差一个 skip_days。
        let idx = g.skip_days + col * 7 + row;
        (idx < self.days.len()).then_some(idx)
    }

    /// 悬停文案：首行日期与星期，次行字数。
    ///
    /// 分两行是为了让浮层窄一些——跟着指针走的提示越宽越容易挡住相邻格子。
    fn describe(&self, idx: usize) -> Option<String> {
        let (date, chars) = self.days.get(idx)?;
        let head = format!(
            "{} {}",
            data::format_date_label(date),
            data::format_weekday_label(date)
        );
        Some(if *chars == 0 {
            format!("{head}\n无输入")
        } else {
            format!("{head}\n{} 字", data::format_num(*chars as u64))
        })
    }

    /// 画一段标签文字（星期列、图例两端、时段刻度共用）。
    fn text_at(canvas: &mut dyn Canvas, s: &str, r: TextBox, c: Color, a: Align, size: f32) {
        canvas.draw_text(
            s,
            Rect::new(r.x as i32, r.y as i32, r.w as i32, r.h as i32),
            c,
            a,
            &TextStyle::new(size),
        );
    }

    /// 星期标签字号：跟着格子缩放，小格子下也塞得进七行。
    fn label_size(g: &Grid) -> f32 {
        (g.cell * 0.72).clamp(7.5, 10.0)
    }
}

impl Widget for Heatmap {
    fn measure(&self, avail: Size, _style: &Style, _text: &mut dyn TextEngine) -> Size {
        let g = grid_for(avail.w as f32, self.days.len(), self.first_weekday);
        Size::new(avail.w, Self::height_of(&g).ceil() as i32)
    }

    fn paint(
        &self,
        _bounds: Rect,
        content: Rect,
        _focused: bool,
        _enabled: bool,
        canvas: &mut dyn Canvas,
        _style: &Style,
    ) {
        let dark = dark_sig().get();
        let scale = if dark { HEAT_DARK } else { HEAT_LIGHT };
        let g = grid_for(content.w as f32, self.days.len(), self.first_weekday);
        let (ox, oy) = (content.x as f32, content.y as f32);
        let label_color = Color::hex(if dark { 0x8B949E } else { 0x8C959F });
        let radius = (g.cell * 0.18).clamp(1.5, 3.0);

        // 星期标签：七行全标，字号跟格子缩放以免小格子下挤成一团。
        let ls = Self::label_size(&g);
        for (row, name) in WEEKDAY_LABELS.iter().enumerate() {
            let r = TextBox::new(ox, oy + row as f32 * g.step, WEEKDAY_W - 4.0, g.cell);
            Self::text_at(canvas, name, r, label_color, Align::End, ls);
        }

        for (i, (_, chars)) in self.days.iter().enumerate().skip(g.skip_days) {
            let slot = i - g.skip_days;
            let (col, row) = (slot / 7, slot % 7);
            if col >= g.cols {
                break;
            }
            canvas.fill_round_rect(
                ox + WEEKDAY_W + col as f32 * g.step,
                oy + row as f32 * g.step,
                g.cell,
                g.cell,
                radius,
                &Paint::fill(Color::hex(scale[data::heat_level(*chars)])),
            );
        }

        // 图例带：左侧悬停说明、右侧「少 ▢▢▢▢▢ 多」，与网格拉开 LEGEND_GAP。
        //
        // 图例右对齐到**网格右缘**而非内容区右缘：格子受 CELL_MAX 限制，网格往往画不满
        // 可用宽度（820px 下网格右缘比内容区右缘靠左约 90px）。若按内容区右对齐，图例会
        // 孤零零悬在网格右侧的空白之下，看着像"上方缺了一块"。
        let band_y = oy + 7.0 * g.step - CELL_GAP + LEGEND_GAP;
        let legend_y = band_y + (LEGEND_BAND_H - g.cell) / 2.0;
        let box_step = g.cell + CELL_GAP;
        let boxes_w = 5.0 * box_step - CELL_GAP;
        let text_w = 16.0;
        let grid_right = ox + WEEKDAY_W + g.cols as f32 * g.step - CELL_GAP;
        let boxes_x = (grid_right - text_w - boxes_w).max(ox + WEEKDAY_W);

        let less = TextBox::new(boxes_x - text_w - 3.0, legend_y, text_w, g.cell);
        Self::text_at(canvas, "少", less, label_color, Align::End, ls);
        for (i, c) in scale.iter().enumerate() {
            canvas.fill_round_rect(
                boxes_x + i as f32 * box_step,
                legend_y,
                g.cell,
                g.cell,
                radius,
                &Paint::fill(Color::hex(*c)),
            );
        }
        let more = TextBox::new(boxes_x + boxes_w + 3.0, legend_y, text_w, g.cell);
        Self::text_at(canvas, "多", more, label_color, Align::Start, ls);
    }

    fn on_event(&mut self, ctx: &mut EventCtx, ev: &Event) -> bool {
        let Event::Pointer(p) = ev else { return false };
        let hit = match p.kind {
            PointerKind::Move | PointerKind::Enter => {
                let b = ctx.bounds();
                let g = grid_for(b.w as f32, self.days.len(), self.first_weekday);
                self.hit(&g, (p.pos.x - b.x) as f32, (p.pos.y - b.y) as f32)
            }
            PointerKind::Leave => None,
            _ => return false,
        };
        // 只在命中格变化时请求重绘：在同一格里移动不该每帧刷新。
        if hit != self.last_hit.get() {
            self.last_hit.set(hit);
            ctx.mark_dirty();
        }
        // 不消费：滚轮/拖动仍要穿透给外层滚动容器。
        false
    }

    fn tooltip(&self) -> Option<String> {
        self.describe(self.last_hit.get()?)
    }
}

/// 热力图元素。`days` 需按日期升序且连续。
pub fn heatmap(days: Vec<(String, u32)>, first_weekday: usize) -> Element {
    Element::leaf()
        .widget(Heatmap {
            days: Rc::new(days),
            first_weekday,
            last_hit: std::cell::Cell::new(None),
        })
        .width_match()
}

// ───────────────────────── 今日时段分布 ─────────────────────────

/// 柱状图绘图区高度。
const HOUR_AREA_H: f32 = 80.0;
/// 时刻标签带高度。
const HOUR_LABEL_H: f32 = 16.0;
/// 柱间距。
const HOUR_GAP: f32 = 2.0;
/// 空时段的底座高度——画一小截而非全空，才看得出「这里有个时段但没输入」。
const HOUR_ZERO_H: f32 = 2.0;
/// 标注刻度的整点。
const HOUR_TICKS: [usize; 8] = [0, 3, 6, 9, 12, 15, 18, 21];

/// 今日 24 小时输入量柱状图。
///
/// 与热力图同构：几何由 [`Self::bar_x`] 统一解算，悬停详情写信号交外部行显示。
struct HourBars {
    hours: [u32; 24],
    /// 当日总量（算占比用；恒等于 hours 之和，但由调用方给以免重复求和）。
    total: u32,
    /// 当前命中的时段，`tooltip()` 据此给文案。
    last_hit: std::cell::Cell<Option<usize>>,
}

impl HourBars {
    /// 第 `i` 根柱子的左边界与宽度。
    fn bar_x(width: f32, i: usize) -> (f32, f32) {
        let step = width / 24.0;
        let w = (step - HOUR_GAP).max(1.0);
        (i as f32 * step, w)
    }

    fn describe(&self, i: usize) -> Option<String> {
        let v = *self.hours.get(i)?;
        let mut s = format!("{i:02}:00–{i:02}:59\n{} 字", data::format_num(v as u64));
        if v > 0 && self.total > 0 {
            s.push_str(&format!(
                "（占今日 {:.1}%）",
                v as f64 * 100.0 / self.total as f64
            ));
        }
        Some(s)
    }
}

impl Widget for HourBars {
    fn measure(&self, avail: Size, _style: &Style, _text: &mut dyn TextEngine) -> Size {
        Size::new(avail.w, (HOUR_AREA_H + HOUR_LABEL_H) as i32)
    }

    fn paint(
        &self,
        _bounds: Rect,
        content: Rect,
        _focused: bool,
        _enabled: bool,
        canvas: &mut dyn Canvas,
        _style: &Style,
    ) {
        let dark = dark_sig().get();
        let fill = pick(GREEN);
        let zero = Color::hex(if dark { 0x3A414B } else { 0xD8DEE4 });
        let label_color = Color::hex(if dark { 0x8B949E } else { 0x8C959F });
        let (ox, oy, w) = (content.x as f32, content.y as f32, content.w as f32);
        let peak = self.hours.iter().copied().max().unwrap_or(0).max(1);
        let base = oy + HOUR_AREA_H;

        for (i, v) in self.hours.iter().enumerate() {
            let (bx, bw) = Self::bar_x(w, i);
            // 非零时段至少 3px 高，否则凌晨的零星输入会被压成看不见的一条线。
            let h = if *v == 0 {
                HOUR_ZERO_H
            } else {
                (*v as f32 / peak as f32 * HOUR_AREA_H).max(3.0)
            };
            let color = if *v == 0 { zero } else { fill };
            canvas.fill_round_rect(ox + bx, base - h, bw, h, 2.0, &Paint::fill(color));
        }

        // 刻度：每 3 小时一个，标在对应柱子下方。
        for t in HOUR_TICKS {
            let (bx, _) = Self::bar_x(w, t);
            let r = TextBox::new(ox + bx, base + 3.0, w / 24.0 * 3.0, HOUR_LABEL_H - 3.0);
            Heatmap::text_at(canvas, &t.to_string(), r, label_color, Align::Start, 10.0);
        }
    }

    fn on_event(&mut self, ctx: &mut EventCtx, ev: &Event) -> bool {
        let Event::Pointer(p) = ev else { return false };
        let hit = match p.kind {
            PointerKind::Move | PointerKind::Enter => {
                let b = ctx.bounds();
                let (rx, ry) = ((p.pos.x - b.x) as f32, (p.pos.y - b.y) as f32);
                (rx >= 0.0 && (0.0..=HOUR_AREA_H).contains(&ry))
                    .then(|| (rx / (b.w as f32 / 24.0)).floor() as usize)
                    .filter(|i| *i < 24)
            }
            PointerKind::Leave => None,
            _ => return false,
        };
        if hit != self.last_hit.get() {
            self.last_hit.set(hit);
            ctx.mark_dirty();
        }
        false
    }

    fn tooltip(&self) -> Option<String> {
        self.describe(self.last_hit.get()?)
    }
}

/// 今日时段柱状图元素。
pub fn hour_bars(hours: [u32; 24], total: u32) -> Element {
    Element::leaf()
        .widget(HourBars {
            hours,
            total,
            last_hit: std::cell::Cell::new(None),
        })
        .width_match()
        .height((HOUR_AREA_H + HOUR_LABEL_H) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_of(days: usize, fw: usize) -> Heatmap {
        let v: Vec<(String, u32)> = (0..days)
            .map(|i| (format!("2026-01-{:02}", i % 28 + 1), i as u32))
            .collect();
        Heatmap {
            days: Rc::new(v),
            first_weekday: fw,
            last_hit: std::cell::Cell::new(None),
        }
    }

    /// 宽度足够时显示全部完整周，只丢开头不满一周的零头。
    #[test]
    fn grid_shows_all_whole_weeks_when_wide_enough() {
        // 首日周三（fw=2）→ 丢 5 天凑到下个周一，剩 176 天 = 26 整周。
        let g = grid_for(800.0, 181, 2);
        assert_eq!(g.skip_days, 5);
        assert_eq!(g.cols, (181 - 5usize).div_ceil(7));
        assert!(g.cell >= CELL_MIN && g.cell <= CELL_MAX);
    }

    /// 首日已是周一时不该丢任何数据。
    #[test]
    fn grid_skips_nothing_when_first_day_is_monday() {
        let g = grid_for(800.0, 182, 0);
        assert_eq!(g.skip_days, 0);
        assert_eq!(g.cols, 26);
    }

    /// 窄窗口下宁可少显示几周也不把格子挤没，且绝不溢出可用宽度。
    #[test]
    fn grid_drops_weeks_instead_of_shrinking_below_min() {
        let width = 200.0;
        let g = grid_for(width, 181, 2);
        assert!(g.cell >= CELL_MIN, "格子不应小于最小边长");
        assert!(g.skip_days > 5, "放不下时应在零头之外再舍弃整周");
        let used = WEEKDAY_W + g.cols as f32 * g.step - CELL_GAP;
        assert!(used <= width + 0.01, "绘制宽度 {used} 不应超出 {width}");
    }

    /// 无论首日是星期几，保留段的第一天都必须是周一——否则星期行整体错位，
    /// 且左上角会空出无意义的缺口。
    #[test]
    fn first_kept_day_is_always_monday() {
        for fw in 0..7 {
            for width in [180.0, 400.0, 800.0] {
                let g = grid_for(width, 181, fw);
                assert_eq!(
                    (fw + g.skip_days) % 7,
                    0,
                    "首日周内序号 {fw} / 宽度 {width} 下保留段未落在周一"
                );
            }
        }
    }

    #[test]
    fn grid_never_returns_zero_columns() {
        // 极窄（甚至窄于标签列）时仍要给出可绘制的最小网格，不能除零或 panic。
        for w in [0.0, 1.0, 10.0, WEEKDAY_W] {
            let g = grid_for(w, 181, 0);
            assert!(g.cols >= 1);
            assert!(g.step > 0.0);
        }
    }

    /// 命中检测与绘制必须共用同一套几何：格子中心必命中它自己。
    #[test]
    fn hit_matches_painted_cell_centers() {
        let hm = map_of(181, 2);
        let g = grid_for(800.0, 181, 2);
        for i in [g.skip_days, g.skip_days + 1, g.skip_days + 7, 100, 180] {
            let slot = i - g.skip_days;
            let (col, row) = (slot / 7, slot % 7);
            let cx = WEEKDAY_W + col as f32 * g.step + g.cell / 2.0;
            let cy = row as f32 * g.step + g.cell / 2.0;
            assert_eq!(hm.hit(&g, cx, cy), Some(i), "第 {i} 天的中心应命中自身");
        }
    }

    /// 左上角第一格必须就是保留段的首日——没有前导空位。
    #[test]
    fn first_cell_is_the_first_kept_day() {
        for fw in 0..7 {
            let hm = map_of(181, fw);
            let g = grid_for(800.0, 181, fw);
            let hit = hm.hit(&g, WEEKDAY_W + g.cell / 2.0, g.cell / 2.0);
            assert_eq!(hit, Some(g.skip_days), "首日周内序号 {fw} 下左上角应有格子");
        }
    }

    /// 窄窗口多丢几周后，被保留的首日同样落在第 0 列第 0 行。
    #[test]
    fn hit_matches_centers_after_clipping() {
        let hm = map_of(181, 3);
        let g = grid_for(200.0, 181, 3);
        assert!(g.skip_days > 4);
        let cx = WEEKDAY_W + g.cell / 2.0;
        let cy = g.cell / 2.0;
        assert_eq!(hm.hit(&g, cx, cy), Some(g.skip_days));
    }

    #[test]
    fn hit_returns_none_outside_grid() {
        let hm = map_of(181, 2);
        let g = grid_for(800.0, 181, 2);
        assert_eq!(hm.hit(&g, 2.0, 5.0), None, "星期标签列不算命中");
        assert_eq!(hm.hit(&g, -5.0, 5.0), None);
        assert_eq!(hm.hit(&g, 100.0, -1.0), None);
        assert_eq!(hm.hit(&g, 5_000.0, 5.0), None, "右侧空白不算命中");
        assert_eq!(
            hm.hit(&g, 100.0, 7.0 * g.step + 20.0),
            None,
            "图例带不算命中"
        );
    }

    /// 末列尾部（今天之后的日期）不应命中——那是日历的正常留白。
    #[test]
    fn hit_ignores_trailing_blanks() {
        let hm = map_of(181, 0);
        let g = grid_for(800.0, 181, 0);
        let last_slot = hm.days.len() - 1 - g.skip_days;
        let last_col = last_slot / 7;
        // 末列中最后一天之后的那些行。
        for row in (last_slot % 7 + 1)..7 {
            let cx = WEEKDAY_W + last_col as f32 * g.step + g.cell / 2.0;
            let cy = row as f32 * g.step + g.cell / 2.0;
            assert_eq!(hm.hit(&g, cx, cy), None, "末列第 {row} 行应为空");
        }
    }

    /// 格子之间的缝隙不应命中。
    #[test]
    fn hit_skips_gaps_between_cells() {
        let hm = map_of(181, 0);
        let g = grid_for(800.0, 181, 0);
        let gap_y = g.cell + CELL_GAP / 2.0;
        assert_eq!(hm.hit(&g, WEEKDAY_W + g.cell / 2.0, gap_y), None);
    }

    // ── 时段柱状图 ──

    fn bars_of(hours: [u32; 24]) -> HourBars {
        HourBars {
            hours,
            total: hours.iter().sum(),
            last_hit: std::cell::Cell::new(None),
        }
    }

    /// 24 根柱子必须铺满且不溢出可用宽度。
    #[test]
    fn hour_bars_span_full_width() {
        let w = 480.0;
        let (x0, _) = HourBars::bar_x(w, 0);
        let (x23, w23) = HourBars::bar_x(w, 23);
        assert_eq!(x0, 0.0);
        assert!(x23 + w23 <= w + 0.01, "末柱右缘 {} 超出 {w}", x23 + w23);
        // 相邻柱间距恒定。
        let (x1, w1) = HourBars::bar_x(w, 1);
        let (x2, _) = HourBars::bar_x(w, 2);
        assert!((x2 - x1 - (w1 + HOUR_GAP)).abs() < 0.01);
    }

    #[test]
    fn hour_tooltip_reports_share_of_today() {
        let mut h = [0u32; 24];
        h[9] = 300;
        h[14] = 700;
        let b = bars_of(h);
        let s = b.describe(14).expect("14 时应有文案");
        assert!(s.contains("14:00–14:59"), "实际: {s}");
        assert!(s.contains("700"), "实际: {s}");
        assert!(s.contains("70.0%"), "实际: {s}");
    }

    /// 空时段不报占比（0% 是噪音），但仍要给出时刻与字数。
    #[test]
    fn hour_tooltip_omits_share_when_empty() {
        let b = bars_of([0; 24]);
        let s = b.describe(3).expect("空时段也应有文案");
        assert!(s.contains("03:00–03:59"), "实际: {s}");
        assert!(!s.contains('%'), "空时段不应出现占比: {s}");
    }

    /// 越界索引不该 panic——`tooltip()` 每帧被调用，命中状态可能滞后于数据。
    #[test]
    fn describe_out_of_range_is_none() {
        assert!(bars_of([0; 24]).describe(24).is_none());
        assert!(map_of(10, 0).describe(10).is_none());
    }

    /// 未悬停时不给提示，避免弹出空浮层。
    #[test]
    fn tooltip_is_none_until_a_cell_is_hit() {
        use windui::core::Widget;
        let hm = map_of(30, 0);
        assert!(hm.tooltip().is_none());
        hm.last_hit.set(Some(3));
        assert!(hm.tooltip().is_some());
    }
}
