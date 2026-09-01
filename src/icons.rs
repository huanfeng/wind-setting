//! UI 图标库 — 白色 SVG 模板（res/icons/ui）+ tint 着色。
//!
//! 导航图标按 Tab 名精确匹配；分区图标按 section 名映射、未命中回退 sliders
//! （section 名来自 core manifest，新 section 自动获得回退图标，不会缺图）。
//!
//! **网格约定**（新增图标必须遵守，否则在高 DPI 下发虚）：
//!
//! 1. 18×18 viewBox，按 [`ICON_PX`]（= 18）渲染 —— 网格与输出尺寸 1:1。
//! 2. 描边一律 2px，**描边中心线坐标取奇数**。
//! 3. 端点、填充边界取**偶数**。
//!
//! 第 2 条是关键：描边物理宽 = `2 × DPI缩放`，该宽度为偶数时中心须落整数物理
//! 坐标、为奇数时须落半整数。150% 下 2px→3px（奇），要求 `坐标 × 1.5` 为半整数，
//! 即坐标必须是奇数。奇数坐标同时满足 100%（×1=整数）与 200%（×2=偶整数），
//! 于是这三档全部 1:1 落像素。偶数坐标会在 150% 下把描边摊成上下两行灰边——
//! 同一图标里几条本该相同的线会粗细不一，比整体发虚更刺眼。
//!
//! 125%/175% 无解（2px 算出 2.5 物理像素，非整数），只能接受轻微软化。

use windui::prelude::*;

const GRID: &str = include_str!("../res/icons/ui/grid.svg");
const LINES: &str = include_str!("../res/icons/ui/lines.svg");
const KEYBOARD: &str = include_str!("../res/icons/ui/keyboard.svg");
const CONTRAST: &str = include_str!("../res/icons/ui/contrast.svg");
const LAYERS: &str = include_str!("../res/icons/ui/layers.svg");
const SLIDERS: &str = include_str!("../res/icons/ui/sliders.svg");
/// 信息角标。侧栏「关于」按 [`ICON_PX`] 整尺寸渲染，遵守上面的 2px 网格约定。
pub const INFO: &str = include_str!("../res/icons/ui/info.svg");
/// [`INFO`] 的细描边版（1.4px），**专供缩小渲染的角标**
/// （[`crate::widgets::setting_row_schema_hint`] 按 14px 画它）。
///
/// # 为什么它可以不守 2px 约定
///
/// 那条约定的前提是「18×18 网格按 18px 渲染，1:1 落像素」。角标是**缩到 14px** 的，
/// 2px 描边到那儿已经变成 1.56 物理像素——本来就不在整数网格上，守不守都一样软。
/// 而线宽占比是不变的：2/18 的线画在 14px 的图上，看起来就比同一个图标画在 18px 上粗
/// 一圈，用户报的正是这个。
///
/// ⚠️ 别拿它替换侧栏那个 [`INFO`]：那里是 1:1 渲染，1.4px 描边会在 150% 下摊成灰边。
/// **两个尺寸各用各的**，这不是重复资源。
pub const INFO_THIN: &str = include_str!("../res/icons/ui/info_thin.svg");
const LIST: &str = include_str!("../res/icons/ui/list.svg");
const CHECK: &str = include_str!("../res/icons/ui/check.svg");
/// 搜索框的前置放大镜（侧栏搜索框左侧）。
pub const SEARCH: &str = include_str!("../res/icons/ui/search.svg");
/// 「在新窗口打开」提示角标（侧栏词库项右侧）。
pub const EXTERNAL: &str = include_str!("../res/icons/ui/external.svg");

/// 侧栏导航图标（Tab 名精确匹配；新 Tab 回退 sliders）。
pub fn nav_icon(tab: &str) -> &'static str {
    match tab {
        "方案" => GRID,
        "输入" => LINES,
        "按键" => KEYBOARD,
        "外观" => CONTRAST,
        "词库" => LAYERS,
        "高级" => SLIDERS,
        "关于" => INFO,
        _ => SLIDERS,
    }
}

/// 分区标题图标（section 名映射；未命中回退 sliders）。
pub fn section_icon(title: &str) -> &'static str {
    match title {
        "主方案设置" | "输入方案" => GRID,
        "主题" => CONTRAST,
        "候选窗口" | "候选行为" | "候选操作" | "候选词管理" | "候选项提示信息" | "候选无效按键" => {
            LIST
        }
        "功能快捷键" | "中英文切换" | "按键行为" => KEYBOARD,
        "字符与标点" | "标点配对" | "简入繁出" | "模糊音" => LINES,
        "上屏行为" | "默认状态" => CHECK,
        _ => SLIDERS,
    }
}

/// 图标渲染边长（逻辑 px）：与 SVG 网格同为 18，保证描边 1:1 落像素。
/// 改动此值必须同步重绘全部图标的网格，否则又会落回半像素。
///
/// 取偶数网格且**网格中心为奇数**（18/2=9）：三条等间距线才能既落在全奇数坐标上、
/// 又保持上下对称（如 y=3/9/15）。中心为偶数的网格（如 20）会逼着中间那条线踩偶数，
/// 只能在「150% 对齐」与「上下对称」之间二选一。
pub const ICON_PX: i32 = 18;

/// 图标底板（chip）边长：图标四周留 4px。
pub const CHIP_PX: i32 = 26;

const _: () = assert!(CHIP_PX >= ICON_PX, "chip 放不下图标");

/// SVG 图标 → 着色 Element。`px` 为逻辑边长（正常用 [`ICON_PX`]）。
///
/// 传 `None` 走 windui 的 **DPI 感知**光栅化：paint 期按实际物理尺寸现场光栅并
/// 缓存，125%/150% 等缩放下同样 1:1，不再有"写死 2× 光栅再降采样"的糊边。
pub fn tinted(svg: &'static str, px: i32, color: Color) -> Element {
    Element::image_svg(svg.as_bytes(), None)
        .size(px, px)
        .tint(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[(&str, &str)] = &[
        ("grid", GRID),
        ("lines", LINES),
        ("keyboard", KEYBOARD),
        ("contrast", CONTRAST),
        ("layers", LAYERS),
        ("sliders", SLIDERS),
        ("info", INFO),
        ("list", LIST),
        ("check", CHECK),
        ("search", SEARCH),
    ];

    /// 取 XML 属性值（`attr="..."` 的第 `nth` 次出现）。
    fn attr(s: &str, name: &str) -> Vec<String> {
        s.split(&format!("{name}=\""))
            .skip(1)
            .filter_map(|seg| seg.split('"').next().map(str::to_owned))
            .collect()
    }

    /// 从 `d` 里提取轴对齐描边的中心坐标：`M x y h…` 取 y，`M x y v…` 取 x。
    /// 斜线/弧线本就靠抗锯齿，不受奇数约束，跳过。
    fn axis_stroke_centers(d: &str) -> Vec<f32> {
        let mut out = Vec::new();
        for seg in d.split('M').skip(1) {
            let (head, kind) = match (seg.find('h'), seg.find('v')) {
                (Some(i), _) => (&seg[..i], 'h'),
                (None, Some(i)) => (&seg[..i], 'v'),
                _ => continue,
            };
            let mut it = head.split_whitespace();
            let (Some(x), Some(y)) = (it.next(), it.next()) else {
                continue;
            };
            if let Ok(v) = if kind == 'h' { y } else { x }.parse::<f32>() {
                out.push(v);
            }
        }
        out
    }

    fn is_odd(v: f32) -> bool {
        v.fract() == 0.0 && (v as i32) % 2 != 0
    }

    /// 网格尺寸须与 `ICON_PX` 一致：两者不等就意味着渲染时要缩放，描边必然离开像素栅格。
    #[test]
    fn viewbox_matches_icon_px() {
        let want = format!("0 0 {ICON_PX} {ICON_PX}");
        for (name, svg) in ALL {
            assert_eq!(
                attr(svg, "viewBox"),
                vec![want.clone()],
                "{name}.svg 的 viewBox 应为 {want}（与 ICON_PX 一致才能 1:1 落像素）"
            );
        }
    }

    /// 描边宽度必须恒为 2：只有整数物理像素宽的描边才可能落满像素行，
    /// 1.6 这类值在任何 DPI 下都会被抗锯齿摊成灰边。
    #[test]
    fn stroke_width_is_always_two() {
        for (name, svg) in ALL {
            for w in attr(svg, "stroke-width") {
                assert_eq!(w, "2", "{name}.svg 的描边宽度应恒为 2，实得 {w}");
            }
        }
    }

    /// 轴对齐描边的中心坐标必须为奇数——150% 下 2px 描边变 3 物理像素（奇数宽），
    /// 中心须落在半整数上，而只有奇数逻辑坐标 ×1.5 才是半整数。偶数坐标会让这条
    /// 描边摊成上下两行灰边，同一图标里几条本该相同的线于是粗细不一。
    #[test]
    fn axis_strokes_sit_on_odd_coordinates() {
        for (name, svg) in ALL {
            for d in attr(svg, "d") {
                for c in axis_stroke_centers(&d) {
                    assert!(
                        is_odd(c),
                        "{name}.svg 的轴对齐描边中心坐标 {c} 应为奇数（见模块头网格约定）"
                    );
                }
            }
        }
    }

    /// 矩形描边的四条边同样是轴对齐描边，其中心（x、y、x+w、y+h）须为奇数。
    #[test]
    fn rect_edges_sit_on_odd_coordinates() {
        for (name, svg) in ALL {
            // 属性名带前导空格：否则 "x" 会撞上 circle 的 `cx=`、"width" 撞上 `stroke-width=`。
            let (xs, ys) = (attr(svg, " x"), attr(svg, " y"));
            let (ws, hs) = (attr(svg, " width"), attr(svg, " height"));
            assert!(
                xs.len() == ys.len() && xs.len() == ws.len() && xs.len() == hs.len(),
                "{name}.svg 的 rect 属性数量不齐"
            );
            for i in 0..xs.len() {
                let p = |v: &str| v.parse::<f32>().unwrap_or(f32::NAN);
                let (x, y, w, h) = (p(&xs[i]), p(&ys[i]), p(&ws[i]), p(&hs[i]));
                for (label, v) in [("x", x), ("y", y), ("x+w", x + w), ("y+h", y + h)] {
                    assert!(
                        is_odd(v),
                        "{name}.svg 第 {i} 个 rect 的 {label}={v} 应为奇数（见模块头网格约定）"
                    );
                }
            }
        }
    }
}
