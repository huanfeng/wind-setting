//! 设置窗口的亮/暗主题定义（对齐 wind-ui-rust examples/ime.rs 配色）。
//!
//! 通过 `App::theme_handle()` 在运行期切换：标题栏的"亮/暗"按钮调
//! `handle.set(theme_for(dark))` 即可整窗换肤（所有 `*_role` 节点自动跟随）。

use windui::prelude::*;

/// 设置行 hint 悬浮提示的换行宽度（逻辑 px）。设置窗口比库默认场景更宽，
/// 稍微放宽避免过短说明也被强制拆成两三行。
pub const TOOLTIP_MAX_W: f32 = 320.0;

/// 表单行排版（`Element::setting_row` / `setting_row_desc` / `field` 一并适用）。
///
/// 这些值原是 `widgets.rs` 顶部的一组私有常量，本仓自建的行构造器逐个读它们；行的实现
/// 收敛到库之后，它们的去处只能是主题——库刻意不让尺寸进构造器签名（一个应用里的表单行
/// 必须整齐划一，逐行传尺寸只会让每处各写一个近似值）。
///
/// 层次策略仍是**字号分层**（标题 14 / 说明 11）而非字重：DirectWrite 下雅黑 300–500 均落
/// Regular（细分字重无效），600 起才是 Bold，而整页行标题全加粗过重、显乱。
///
/// 限行取 1：说明文字来自 core 的 manifest，长度不可控（真实数据里四分之一超出左列单行
/// 宽度）。限行后库会自动加末尾省略并挂上悬浮看全文的提示，没真截断时不弹。
fn apply_form_theme(t: &mut Theme) {
    t.form.label_size = Some(14.0);
    t.form.label_weight = Some(500);
    t.form.desc_size = Some(11.0);
    t.form.row_height = Some(40);
    t.form.gap = Some(12);
    t.form.row_pad_y = Some(11);
    t.form.label_max_lines = Some(1);
    t.form.desc_max_lines = Some(1);
}

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
    apply_form_theme(&mut t);
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
    apply_form_theme(&mut t);
    t
}

/// 按明暗标志取主题。
pub fn theme_for(dark: bool) -> Theme {
    if dark { dark_theme() } else { light_theme() }
}

/// 按明暗取主题，并换上给定的强调色组（`None` = 设置窗默认蓝）。
///
/// 只动强调色四个槽位：底色、卡片、文字等仍是设置窗自己的（见 [`crate::accent`] 的取舍）。
/// 用到强调色的静态控件一律走角色色（`bg_role_alpha` / `bg_gradient_roles` / `tint_role`），
/// 换这四个槽位即整窗跟随。
pub fn theme_with_accent(dark: bool, accent: Option<crate::accent::AccentSet>) -> Theme {
    let mut t = theme_for(dark);
    if let Some(a) = accent {
        let p = &mut t.palette;
        p.accent = a.accent;
        p.accent_hover = a.hover;
        p.accent_active = a.active;
        p.on_accent = a.on_accent;
    }
    t
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 守护：强调色只能经调色板（角色色）取用。把默认蓝写成静态色的控件不会跟随
    /// 「主题色：跟随输入法」——换了主题它还是蓝的，而且截图上要切到非默认主题才看得出来。
    #[test]
    fn accent_hex_only_lives_in_theme_def() {
        let palette_hex = ["2F6BED", "4A80F0", "2159D6", "5B8CF5", "6F9BF7", "4A7CE8"];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![root];
        let mut hits = Vec::new();
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs")
                    && !p.ends_with("theme_def.rs")
                    && !p.ends_with("accent.rs")
                {
                    let text = std::fs::read_to_string(&p).unwrap().to_uppercase();
                    for h in palette_hex {
                        if text.contains(&format!("0X{h}")) {
                            hits.push(format!("{} 含 0x{h}", p.display()));
                        }
                    }
                }
            }
        }
        assert!(
            hits.is_empty(),
            "强调色要用 Role::Accent* 取，别写静态色：\n{}",
            hits.join("\n")
        );
    }

    #[test]
    fn theme_with_accent_only_touches_accent_slots() {
        let set = crate::accent::AccentSet {
            accent: Color::hex(0x05875F),
            hover: Color::hex(0x05A573),
            active: Color::hex(0x036546),
            on_accent: Color::WHITE,
        };
        let base = theme_for(false);
        let t = theme_with_accent(false, Some(set));
        assert_eq!(t.palette.accent, set.accent);
        assert_eq!(t.palette.accent_hover, set.hover);
        assert_eq!(t.palette.accent_active, set.active);
        assert_eq!(t.palette.surface, base.palette.surface);
        assert_eq!(t.palette.text, base.palette.text);
        assert_eq!(
            theme_with_accent(true, None).palette.accent,
            theme_for(true).palette.accent
        );
    }
}
