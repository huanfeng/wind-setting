//! UI 图标库 — 白色 SVG 模板（res/icons/ui）+ tint 着色。
//!
//! 导航图标按 Tab 名精确匹配；分区图标按 section 名映射、未命中回退 sliders
//! （section 名来自 core manifest，新 section 自动获得回退图标，不会缺图）。

use windui::prelude::*;

const GRID: &str = include_str!("../res/icons/ui/grid.svg");
const LINES: &str = include_str!("../res/icons/ui/lines.svg");
const KEYBOARD: &str = include_str!("../res/icons/ui/keyboard.svg");
const CONTRAST: &str = include_str!("../res/icons/ui/contrast.svg");
const LAYERS: &str = include_str!("../res/icons/ui/layers.svg");
const SLIDERS: &str = include_str!("../res/icons/ui/sliders.svg");
const INFO: &str = include_str!("../res/icons/ui/info.svg");
const LIST: &str = include_str!("../res/icons/ui/list.svg");
const CHECK: &str = include_str!("../res/icons/ui/check.svg");

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
        "候选窗口" | "候选行为" | "候选操作" | "候选词管理" | "候选项提示信息"
        | "候选无效按键" => LIST,
        "功能快捷键" | "中英文切换" | "按键行为" => KEYBOARD,
        "字符与标点" | "标点配对" | "简入繁出" | "模糊音" => LINES,
        "上屏行为" | "默认状态" => CHECK,
        _ => SLIDERS,
    }
}

/// SVG 图标 → 着色 Element。`px` 为逻辑边长，内部按 2× 光栅求 HiDPI 清晰。
pub fn tinted(svg: &'static str, px: i32, color: Color) -> Element {
    Element::image_svg(svg.as_bytes(), Some((px * 2) as u32))
        .size(px, px)
        .tint(color)
}
