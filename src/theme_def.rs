//! 设置窗口的亮/暗主题定义（对齐 wind-ui-rust examples/ime.rs 配色）。
//!
//! 通过 `App::theme_handle()` 在运行期切换：标题栏的"亮/暗"按钮调
//! `handle.set(theme_for(dark))` 即可整窗换肤（所有 `*_role` 节点自动跟随）。

use windui::prelude::*;

/// 设计主强调蓝，明暗一致（#3b82f6）。
pub const ACCENT: u32 = 0x3B82F6;

/// 亮色主题（设计浅色稿配色）。
pub fn light_theme() -> Theme {
    let mut t = Theme::default();
    let p = &mut t.palette;
    p.accent = Color::hex(ACCENT);
    p.accent_hover = Color::hex(0x60A5FA);
    p.accent_active = Color::hex(0x2563EB);
    p.on_accent = Color::WHITE;
    // 左栏(Bg)=#F9F8F8、右内容(SurfaceAlt)=#FFFEFE、卡片(Surface)=#FFFFFF（高一档 + 边框分隔）。
    p.bg = Color::hex(0xF9F8F8);
    p.surface = Color::hex(0xFFFFFF);
    p.surface_alt = Color::hex(0xFFFEFE);
    p.text = Color::hex(0x1A1F2A);
    p.text_muted = Color::hex(0x6B7280);
    p.text_disabled = Color::hex(0xB7BECA);
    p.border = Color::hex(0xE3E7EC);
    p.divider = Color::hex(0xECEEF2);
    p.track = Color::hex(0xD7DCE3);
    t.metrics.border_width = Len::Physical { px: 1.0 };
    t.metrics.border_width_focus = Len::Physical { px: 2.0 };
    t
}

/// 暗色主题（设计深色稿配色）。
pub fn dark_theme() -> Theme {
    let mut t = Theme::dark();
    let p = &mut t.palette;
    p.accent = Color::hex(ACCENT);
    p.accent_hover = Color::hex(0x60A5FA);
    p.accent_active = Color::hex(0x2563EB);
    p.on_accent = Color::WHITE;
    // 左栏(Bg)=#161925、右内容(SurfaceAlt)=#1A202E、卡片(Surface)=#232C3D（高一档，便于卡片浮起）。
    p.bg = Color::hex(0x161925);
    p.surface = Color::hex(0x232C3D);
    p.surface_alt = Color::hex(0x1A202E);
    p.text = Color::hex(0xE8EAED);
    p.text_muted = Color::hex(0x8B94A7);
    p.text_disabled = Color::hex(0x5A6377);
    p.border = Color::hex(0x2E3A54);
    // 分割线在暗色卡片(0x232C3D)上需足够对比，原 0x222C40 几乎同色不可见，提亮。
    p.divider = Color::hex(0x37425D);
    p.track = Color::hex(0x2B3550);
    t.metrics.border_width = Len::Physical { px: 1.0 };
    t.metrics.border_width_focus = Len::Physical { px: 2.0 };
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
