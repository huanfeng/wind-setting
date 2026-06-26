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
    p.bg = Color::hex(0xEEF1F5);
    p.surface = Color::hex(0xFFFFFF);
    p.surface_alt = Color::hex(0xF4F6F9);
    p.text = Color::hex(0x1A1F2A);
    p.text_muted = Color::hex(0x6B7280);
    p.text_disabled = Color::hex(0xB7BECA);
    p.border = Color::hex(0xE3E7EC);
    p.divider = Color::hex(0xECEEF2);
    p.track = Color::hex(0xD7DCE3);
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
    p.bg = Color::hex(0x0F1626);
    p.surface = Color::hex(0x141C2B);
    p.surface_alt = Color::hex(0x1B2436);
    p.text = Color::hex(0xE8EAED);
    p.text_muted = Color::hex(0x8B94A7);
    p.text_disabled = Color::hex(0x5A6377);
    p.border = Color::hex(0x28324A);
    p.divider = Color::hex(0x222C40);
    p.track = Color::hex(0x2B3550);
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
        use std::process::Command;
        let out = Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                "/v",
                "AppsUseLightTheme",
            ])
            .output();
        if let Ok(o) = out {
            let s = String::from_utf8_lossy(&o.stdout);
            // 形如 "AppsUseLightTheme    REG_DWORD    0x0"
            if let Some(pos) = s.find("0x") {
                let hex = s[pos + 2..]
                    .trim_start()
                    .chars()
                    .take_while(|c| c.is_ascii_hexdigit())
                    .collect::<String>();
                if let Ok(v) = u32::from_str_radix(&hex, 16) {
                    return v == 0; // 0 = 暗色
                }
            }
        }
        true
    }
    #[cfg(not(windows))]
    {
        true
    }
}
