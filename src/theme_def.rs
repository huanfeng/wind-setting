//! 设置窗口的亮/暗主题定义（对齐 wind-ui-rust examples/ime.rs 配色）。
//!
//! 通过 `App::theme_handle()` 在运行期切换：标题栏的"亮/暗"按钮调
//! `handle.set(theme_for(dark))` 即可整窗换肤（所有 `*_role` 节点自动跟随）。

use windui::prelude::*;

/// 亮色主题强调蓝（静态渐变/淡底用；tint/渐变不走 *_role，需静态常量）。
pub const ACCENT_LIGHT: u32 = 0x2F6BED;
/// 暗色主题强调蓝（暗背景上提亮一档）。
pub const ACCENT_DARK: u32 = 0x5B8CF5;

/// 设置行 hint 悬浮提示的换行宽度（逻辑 px）。设置窗口比库默认场景更宽，
/// 稍微放宽避免过短说明也被强制拆成两三行。
pub const TOOLTIP_MAX_W: f32 = 320.0;

/// 亮色主题（参考稿蓝灰冷调）。
pub fn light_theme() -> Theme {
    let mut t = Theme::default();
    let p = &mut t.palette;
    p.accent = Color::hex(0x2F6BED);
    p.accent_hover = Color::hex(0x4A80F0);
    p.accent_active = Color::hex(0x2159D6);
    p.on_accent = Color::WHITE;
    // 左栏(Bg)=#F5F7FA、右内容(SurfaceAlt)=#FBFCFE、卡片(Surface)=#FFFFFF（高一档 + 边框分隔）。
    p.bg = Color::hex(0xF5F7FA);
    p.surface = Color::hex(0xFFFFFF);
    p.surface_alt = Color::hex(0xFBFCFE);
    p.text = Color::hex(0x182031);
    p.text_muted = Color::hex(0x5A6577);
    p.text_disabled = Color::hex(0xB6BDC9);
    p.border = Color::hex(0xE4E8EE);
    p.divider = Color::hex(0xEDF0F4);
    // track 兼作 outline_soft 按钮静默边（须深于 border，禁用边才有层级）与开关关闭轨道。
    p.track = Color::hex(0xCBD2DD);
    // 输入框提示（"待填写"语义）。
    p.placeholder = Color::hex(0x97A1B2);
    // 设置行描述这类三级弱化正文（参考稿 text3；text_muted 留给次级正文）。
    // 取值与 placeholder 相同是参考稿如此，语义则分家：windui 0.12 起有正牌的
    // `Role::TextSubtle`，此前本仓借 `Role::Placeholder` 上色，把可读说明说成了待填写。
    p.text_subtle = Color::hex(0x97A1B2);
    t.metrics.border_width = Len::Physical { px: 1.0 };
    t.metrics.border_width_focus = Len::Physical { px: 2.0 };
    t.tooltip.max_width = Some(TOOLTIP_MAX_W);
    t
}

/// 暗色主题（参考稿深夜蓝调）。
pub fn dark_theme() -> Theme {
    let mut t = Theme::dark();
    let p = &mut t.palette;
    p.accent = Color::hex(0x5B8CF5);
    p.accent_hover = Color::hex(0x6F9BF7);
    p.accent_active = Color::hex(0x4A7CE8);
    p.on_accent = Color::WHITE;
    // 左栏(Bg)=#10161F、右内容(SurfaceAlt)=#131A27、卡片(Surface)=#1A2333（高一档，便于卡片浮起）。
    p.bg = Color::hex(0x10161F);
    p.surface = Color::hex(0x1A2333);
    p.surface_alt = Color::hex(0x131A27);
    p.text = Color::hex(0xEBEEF5);
    p.text_muted = Color::hex(0x9AA3B5);
    p.text_disabled = Color::hex(0x6A7284);
    p.border = Color::hex(0x2A3447);
    // 分割线须在暗色卡片(0x1A2333)上可辨（历史教训：太接近会不可见）。
    p.divider = Color::hex(0x2C374C);
    // track 兼作 outline_soft 按钮静默边（须深/亮于 border）与开关关闭轨道。
    p.track = Color::hex(0x3A4560);
    // 输入框提示（"待填写"语义）。
    p.placeholder = Color::hex(0x6C7890);
    // 设置行描述这类三级弱化正文（参考稿 text3 ≈ 白 42% 合成色）。选型同亮色主题。
    p.text_subtle = Color::hex(0x6C7890);
    t.metrics.border_width = Len::Physical { px: 1.0 };
    t.metrics.border_width_focus = Len::Physical { px: 2.0 };
    t.tooltip.max_width = Some(TOOLTIP_MAX_W);
    t
}

/// 按明暗标志取主题。
pub fn theme_for(dark: bool) -> Theme {
    if dark { dark_theme() } else { light_theme() }
}

/// 检测系统是否为暗色（窗口默认主题随系统，不持久化用户切换）。
///
/// Windows：读注册表 `AppsUseLightTheme`（0=暗 / 1=亮）。
/// 失败或非 Windows：回退暗色（与 ime.rs 一致，暗色效果最佳）。
pub fn system_dark() -> bool {
    #[cfg(windows)]
    {
        // 直接调用 winreg API，避免原先启动 reg.exe 子进程带来的 50-200ms 开销。
        use winreg::{RegKey, enums::HKEY_CURRENT_USER};
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(key) =
            hkcu.open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
            && let Ok(v) = key.get_value::<u32, _>("AppsUseLightTheme")
        {
            return v == 0; // 0 = 暗色
        }
        true
    }
    #[cfg(not(windows))]
    {
        true
    }
}
