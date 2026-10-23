//! 设置窗「主题色跟随输入法」：把输入法主题的强调色修正成设置窗可用的一套。
//!
//! 输入法主题的强调色是为候选窗调的，直接拿来当设置窗的按钮底色常常不可读：清风·绿
//! 亮档主色压白字只有 2.5:1、琥珀 2.1:1。所以只取主题的**色相意图**，亮度由这里按设置窗
//! 自己的底色修到达标：
//!
//! - **亮档**：强调色要当填充色压白字（主按钮、开关、选中块），与白字对比 ≥ 4.5，不够就
//!   沿 HSL 亮度往深调。白底卡片上当文字用时对比度与压白字相同，一并满足。
//! - **暗档**：强调色既当深色卡片上的文字 / 图标（与卡片底对比 ≥ 4.5），又当按钮底压白字
//!   （≥ [`DARK_WHITE_MIN`]）。两条约束在相对亮度约 0.22–0.30 之间同时成立，任何色相都能
//!   沿亮度调进去，所以前景始终是白字，与设置窗暗色默认配色（蓝底白字）一致。
//! - **近灰**（饱和度 < [`MIN_SATURATION`]）：主题本意就是中性色，硬套会让整个界面没有
//!   强调层次，回落设置窗默认蓝。
//!
//! 取色源是 core `theme.accent`：每档优先 `accentText`（作者按该档底色调过可读性），没有再
//! 用 `accent`。

use serde_json::Value;
use windui::prelude::Color;

/// 低于这个 HSL 饱和度视为中性色，回落默认。
pub const MIN_SATURATION: f32 = 0.15;
/// 低于这个色度（RGB 通道极差 / 255）也视为中性色，补 HSL 饱和度在极亮 / 极暗处失真。
const MIN_CHROMA: f32 = 0.08;
/// 正文级对比度（WCAG AA）。
const MIN_CONTRAST: f32 = 4.5;
/// 暗档填充色上白字的最低对比。按钮文字字重偏大，取大字号档的 3:1——设置窗暗色默认蓝
/// `#5B8CF5` 就是 3.2:1。
const DARK_WHITE_MIN: f32 = 3.0;

/// 悬停态的 HSL 亮度按此比例收缩（l × 0.90）。按比例而非减定值：近黑的强调色减定值会
/// 两态都夹到纯黑、分不出来。
const HOVER_DARKEN: f32 = 0.10;
/// 按下态的 HSL 亮度收缩比例（l × 0.77）。
const ACTIVE_DARKEN: f32 = 0.23;

/// 设置窗亮档卡片底（`theme_def::light_theme` 的 surface）。
const LIGHT_SURFACE: Color = Color::hex(0xFFFFFF);
/// 设置窗暗档卡片底（`theme_def::dark_theme` 的 surface）。
const DARK_SURFACE: Color = Color::hex(0x1A2333);

/// 一档的强调色组，直接落进 windui 调色板的四个槽位（`accent_text` 随 `accent` 一起设）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccentSet {
    pub accent: Color,
    pub hover: Color,
    pub active: Color,
    pub on_accent: Color,
}

/// 修正结论：为什么用了 / 没用主题色。界面据此给提示。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Derived {
    /// 主题色原样可用。
    AsIs(AccentSet),
    /// 调过亮度才达标。
    Adjusted(AccentSet),
    /// 近灰，回落默认色。
    Neutral,
}

impl Derived {
    pub fn set(&self) -> Option<AccentSet> {
        match self {
            Derived::AsIs(s) | Derived::Adjusted(s) => Some(*s),
            Derived::Neutral => None,
        }
    }
}

/// 把一档主题色修正成可用的强调色组。
pub fn derive(raw: Color, dark: bool) -> Derived {
    let surface = if dark { DARK_SURFACE } else { LIGHT_SURFACE };
    let base = over(raw, surface);
    let (h, s, l) = to_hsl(base);
    // HSL 饱和度在亮度接近 0 / 1 时会被放大（`#F8F8FF` 的 S 是 1.0），单看它会把近白、近黑
    // 的中性色当成鲜艳色硬压出色相来；再用色度（max − min）兜一道。
    let chroma = {
        let (mx, mn) = (
            base.r.max(base.g).max(base.b),
            base.r.min(base.g).min(base.b),
        );
        (mx - mn) as f32 / 255.0
    };
    if s < MIN_SATURATION || chroma < MIN_CHROMA {
        return Derived::Neutral;
    }
    let white_min = if dark { DARK_WHITE_MIN } else { MIN_CONTRAST };
    let mut l2 = l;
    // 先压白字（太亮往深调），暗档再保卡片上可读（太暗往亮调）。可行区间存在（见模块文档），
    // 每步 1 个百分点的亮度变化远小于区间宽度，不会来回振荡；上限只防浮点意外。
    for _ in 0..200 {
        let c = from_hsl(h, s, l2);
        if contrast(c, Color::WHITE) < white_min {
            l2 -= 0.01;
        } else if dark && contrast(c, DARK_SURFACE) < MIN_CONTRAST {
            l2 += 0.01;
        } else {
            break;
        }
        l2 = l2.clamp(0.0, 1.0);
    }
    // 亮度没动就原样用输入色：HSL 往返在 u8 取整下不保证逐位还原，拿它比相等会误报「已调整」。
    let unchanged = l2 == l;
    let accent = if unchanged { base } else { from_hsl(h, s, l2) };
    // 悬停 / 按下一律加深（同 windui 0.29 亮档：blue-600 → 700 → 800；默认蓝上约 −5.6 / −12.8
    // 个百分点）。
    // 暗档也加深而不是照库的习惯变亮：本仓暗档强调色还兼作卡片上的文字，亮度已贴着压白字
    // 3:1 的下限，再亮就不达标；加深只会让白字更清楚。
    let set = AccentSet {
        accent,
        hover: from_hsl(h, s, l2 * (1.0 - HOVER_DARKEN)),
        active: from_hsl(h, s, l2 * (1.0 - ACTIVE_DARKEN)),
        on_accent: Color::WHITE,
    };
    if unchanged {
        Derived::AsIs(set)
    } else {
        Derived::Adjusted(set)
    }
}

/// core `theme.accent` 的回包：两档原色（未修正）。
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeAccent {
    pub theme_id: String,
    pub light: Option<Color>,
    pub dark: Option<Color>,
}

impl ThemeAccent {
    /// 解析回包；`null`（主题全坏）或形状不对时为 `None`。每档优先 `accentText`。
    pub fn from_value(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        let band = |name: &str| {
            let b = obj.get(name)?;
            ["accentText", "accent"]
                .iter()
                .find_map(|k| b.get(*k)?.as_str().and_then(Color::from_hex_str))
        };
        Some(Self {
            theme_id: obj.get("themeId")?.as_str()?.to_string(),
            light: band("light"),
            dark: band("dark"),
        })
    }

    /// 取某档修正结果；该档没有颜色时为 `None`（按默认色处理）。
    pub fn derive(&self, dark: bool) -> Option<Derived> {
        let c = if dark { self.dark } else { self.light }?;
        Some(derive(c, dark))
    }
}

/// 设置窗主题色来源（`wind_setting.toml` 的 `ui.accent`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccentMode {
    /// 设置窗自带的蓝。
    Default,
    /// 跟随输入法主题。
    Ime,
}

impl AccentMode {
    /// 底栏下拉的选项，顺序即下标。
    pub const LABELS: [&'static str; 2] = ["主题色：默认", "主题色：跟随输入法"];

    /// 未设过或不认识的值都按默认：键名给将来的 `"#RRGGBB"`（自定义色）留着，
    /// 旧版本读到新取值时宁可回默认，也不该报错。
    pub fn from_config(v: Option<&str>) -> Self {
        match v {
            Some("ime") => Self::Ime,
            _ => Self::Default,
        }
    }

    /// 落盘值；默认不落盘（同 `ui.dark` 的「未设过」）。
    pub fn to_config(self) -> Option<String> {
        match self {
            Self::Default => None,
            Self::Ime => Some("ime".to_string()),
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Default => 0,
            Self::Ime => 1,
        }
    }

    pub fn from_index(i: usize) -> Self {
        if i == 1 { Self::Ime } else { Self::Default }
    }
}

/// 跟随模式下底栏提示的文案：用了什么、为什么没用。`name` 为主题显示名。
pub fn follow_note(fetched: Option<&ThemeAccent>, name: &str, dark: bool) -> String {
    let Some(t) = fetched else {
        return "取不到输入法主题色（输入法未运行？），暂用默认色".to_string();
    };
    match t.derive(dark) {
        Some(Derived::AsIs(_)) => format!("已跟随输入法主题「{name}」"),
        Some(Derived::Adjusted(s)) => {
            // 方向看修正前后的亮度，不按明暗档猜：暗档的亮色（亮绿、亮黄）为了压白字是往深调的。
            let raw = if dark { t.dark } else { t.light };
            let lighter = raw.is_some_and(|r| luminance(s.accent) > luminance(r));
            let how = if lighter { "调亮" } else { "调深" };
            format!("已跟随输入法主题「{name}」，原色在界面上对比度不足，已{how}以保证可读")
        }
        Some(Derived::Neutral) => format!("输入法主题「{name}」的强调色接近灰色，已用默认色"),
        None => format!("输入法主题「{name}」没有可用的强调色，已用默认色"),
    }
}

/// 半透明色先合成到底色上：设置窗不叠半透明强调色，按它在该底上的观感取实色。
fn over(c: Color, bg: Color) -> Color {
    if c.a == 255 {
        return c;
    }
    let a = c.a as f32 / 255.0;
    let m = |f: u8, b: u8| (f as f32 * a + b as f32 * (1.0 - a)).round() as u8;
    Color::rgb(m(c.r, bg.r), m(c.g, bg.g), m(c.b, bg.b))
}

/// WCAG 相对亮度。
fn luminance(c: Color) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// WCAG 对比度（1..=21）。
pub fn contrast(a: Color, b: Color) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn to_hsl(c: Color) -> (f32, f32, f32) {
    let (r, g, b) = (c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d == 0.0 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } * 60.0;
    (h, s, l)
}

fn from_hsl(h: f32, s: f32, l: f32) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    Color::rgb(to(r), to(g), to(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 出厂主题 `theme.accent` 的 accentText 真值（2026-10-08 在靶机上实测）。
    const BUILTIN: &[(&str, u32, u32)] = &[
        ("default", 0x2563EB, 0x60A5FA),
        ("jade", 0x059669, 0x34D399),
        ("amber", 0xD97706, 0xFBBF24),
        ("violet", 0x7C3AED, 0xA78BFA),
        ("msime", 0x0078D4, 0x0078D4),
    ];

    fn set_of(d: Derived) -> AccentSet {
        d.set().expect("出厂主题都不是近灰，不该回落")
    }

    #[test]
    fn builtin_themes_meet_contrast_in_both_bands() {
        for &(id, light, dark) in BUILTIN {
            let l = set_of(derive(Color::hex(light), false));
            assert!(
                contrast(l.accent, l.on_accent) >= MIN_CONTRAST,
                "{id} 亮档压白字不达标: {:?}",
                l.accent
            );
            let d = set_of(derive(Color::hex(dark), true));
            assert!(
                contrast(d.accent, DARK_SURFACE) >= MIN_CONTRAST,
                "{id} 暗档在卡片上不达标: {:?}",
                d.accent
            );
            assert!(
                contrast(d.accent, d.on_accent) >= DARK_WHITE_MIN,
                "{id} 暗档压白字不达标: {:?}",
                d.accent
            );
        }
    }

    #[test]
    fn readable_colors_are_kept_as_is() {
        // 亮档：清风·蓝 5.17、紫 5.70、微软蓝 4.53，都已达标。
        for c in [0x2563EB, 0x7C3AED, 0x0078D4] {
            assert!(
                matches!(derive(Color::hex(c), false), Derived::AsIs(s) if s.accent == Color::hex(c)),
                "#{c:06X} 已达标，不该被改"
            );
        }
        // 暗档：紫 #A78BFA 卡片上 5.8、压白字 2.7 不够；设置窗暗色默认蓝两条都达标。
        assert!(matches!(
            derive(Color::hex(0x5B8CF5), true),
            Derived::AsIs(_)
        ));
    }

    #[test]
    fn unreadable_light_accents_are_darkened_not_rehued() {
        for c in [0x059669, 0xD97706, 0x10B981, 0xF59E0B] {
            let raw = Color::hex(c);
            let Derived::Adjusted(s) = derive(raw, false) else {
                panic!("#{c:06X} 压白字不足 4.5，应调整");
            };
            let (h0, _, l0) = to_hsl(raw);
            let (h1, _, l1) = to_hsl(s.accent);
            assert!(l1 < l0, "#{c:06X} 应往深调");
            assert!((h0 - h1).abs() < 3.0, "#{c:06X} 色相不该漂移: {h0} → {h1}");
        }
    }

    #[test]
    fn bright_dark_band_accent_is_toned_down_for_white_text() {
        // 亮绿压白字只有 1.9:1：往深调到白字可读，同时仍在深色卡片上达标。
        let raw = Color::hex(0x34D399);
        let Derived::Adjusted(s) = derive(raw, true) else {
            panic!("#34D399 压白字不足，应调整");
        };
        assert_eq!(s.on_accent, Color::WHITE);
        assert!(to_hsl(s.accent).2 < to_hsl(raw).2);
        assert!(contrast(s.accent, DARK_SURFACE) >= MIN_CONTRAST);
    }

    #[test]
    fn too_dark_dark_band_accent_is_lightened() {
        // 微软蓝暗档也写 #0078D4：在深色卡片上不够亮。
        let Derived::Adjusted(s) = derive(Color::hex(0x0078D4), true) else {
            panic!("#0078D4 在暗卡片上应调亮");
        };
        assert!(to_hsl(s.accent).2 > to_hsl(Color::hex(0x0078D4)).2);
    }

    #[test]
    fn near_gray_falls_back() {
        // 末两个：近白 / 近黑的偏色，HSL 饱和度被放大成高值，要靠色度兜住。
        for c in [0x555555, 0xBBBBBB, 0x6B6F75, 0xF8F8FF, 0x0A0A12] {
            assert_eq!(derive(Color::hex(c), false), Derived::Neutral, "#{c:06X}");
            assert_eq!(derive(Color::hex(c), true), Derived::Neutral, "#{c:06X}");
        }
    }

    /// 悬停 / 按下阶差与 `theme_def` 的两套默认值对得上（每通道误差 ≤ 2，只容 HSL 往返取整）。
    #[test]
    fn hover_and_active_steps_match_default_palette() {
        let close = |a: Color, b: Color| {
            let d = |x: u8, y: u8| (x as i32 - y as i32).abs();
            d(a.r, b.r).max(d(a.g, b.g)).max(d(a.b, b.b)) <= 2
        };
        for dark in [false, true] {
            let p = crate::theme_def::theme_for(dark).palette;
            let s = set_of(derive(p.accent, dark));
            assert_eq!(s.accent, p.accent);
            assert!(close(s.hover, p.accent_hover), "{dark}: {:?}", s.hover);
            assert!(close(s.active, p.accent_active), "{dark}: {:?}", s.active);
        }
    }

    /// 悬停 / 按下压白字不低于强调色本身的门槛（亮档 4.5、暗档 3）：两态都比强调色深，
    /// 改回「悬停变亮」会在这里红。色相扫一圈，外加出厂主题。
    #[test]
    fn hover_and_active_keep_white_text_contrast() {
        let mut raws: Vec<u32> = (0..12)
            .map(|i| from_hsl(i as f32 * 30.0, 0.8, 0.55))
            .map(|c| (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32)
            .collect();
        raws.extend(BUILTIN.iter().flat_map(|&(_, l, d)| [l, d]));
        for raw in raws {
            for dark in [false, true] {
                let min = if dark { DARK_WHITE_MIN } else { MIN_CONTRAST };
                let Some(s) = derive(Color::hex(raw), dark).set() else {
                    continue;
                };
                for (state, c) in [
                    ("accent", s.accent),
                    ("hover", s.hover),
                    ("active", s.active),
                ] {
                    assert!(
                        contrast(c, s.on_accent) >= min,
                        "#{raw:06X} dark={dark} {state} 压白字 {:.2} < {min}",
                        contrast(c, s.on_accent)
                    );
                }
            }
        }
    }

    /// 近黑的强调色（HSL 亮度约 5%）悬停与按下仍分得开：减定值会把两态都夹到纯黑。
    #[test]
    fn near_black_accent_keeps_hover_and_active_apart() {
        let s = set_of(derive(Color::hex(0x00001A), false));
        assert_ne!(s.hover, s.active);
        assert_ne!(s.active, Color::hex(0x000000));
    }

    #[test]
    fn translucent_accent_is_composited_onto_surface() {
        // 50% 纯蓝合成到白卡片上 ≈ #8080FF。
        assert_eq!(
            over(Color::rgba(0, 0, 255, 128), LIGHT_SURFACE),
            Color::rgb(127, 127, 255)
        );
    }

    #[test]
    fn hsl_round_trips() {
        for c in [0x2563EB, 0x059669, 0xD97706, 0x7C3AED, 0x000000, 0xFFFFFF] {
            let (h, s, l) = to_hsl(Color::hex(c));
            assert_eq!(from_hsl(h, s, l), Color::hex(c), "#{c:06X}");
        }
    }

    #[test]
    fn parses_rpc_reply_preferring_accent_text() {
        let v = json!({
            "themeId": "jade",
            "light": { "accent": "#10B981", "accentText": "#059669" },
            "dark": { "accent": "#10B981" },
        });
        let t = ThemeAccent::from_value(&v).unwrap();
        assert_eq!(t.theme_id, "jade");
        assert_eq!(t.light, Some(Color::hex(0x059669)));
        // 缺 accentText 的档退到 accent。
        assert_eq!(t.dark, Some(Color::hex(0x10B981)));
        assert_eq!(ThemeAccent::from_value(&Value::Null), None);
    }

    #[test]
    fn mode_round_trips_and_tolerates_unknown_values() {
        for m in [AccentMode::Default, AccentMode::Ime] {
            assert_eq!(AccentMode::from_config(m.to_config().as_deref()), m);
            assert_eq!(AccentMode::from_index(m.index()), m);
        }
        // 将来的自定义色写法，旧版本读到按默认处理。
        assert_eq!(
            AccentMode::from_config(Some("#10B981")),
            AccentMode::Default
        );
        assert_eq!(AccentMode::Default.to_config(), None, "默认不落盘");
    }

    #[test]
    fn follow_note_explains_each_outcome() {
        let t = |light: u32| ThemeAccent {
            theme_id: "x".into(),
            light: Some(Color::hex(light)),
            dark: None,
        };
        assert!(follow_note(None, "x", false).contains("取不到"));
        assert_eq!(
            follow_note(Some(&t(0x2563EB)), "清风·蓝", false),
            "已跟随输入法主题「清风·蓝」"
        );
        assert!(follow_note(Some(&t(0xD97706)), "琥珀", false).contains("已调深"));
        // 暗档：亮绿压白字不足往深调，太暗的微软蓝往亮调——方向按实际修正说，不按档猜。
        let d = |dark: u32| ThemeAccent {
            theme_id: "x".into(),
            light: None,
            dark: Some(Color::hex(dark)),
        };
        assert!(follow_note(Some(&d(0x34D399)), "绿", true).contains("已调深"));
        assert!(follow_note(Some(&d(0x0078D4)), "微软", true).contains("已调亮"));
        assert!(follow_note(Some(&t(0x666666)), "墨", false).contains("接近灰色"));
        // 该档没有颜色。
        assert!(follow_note(Some(&t(0x2563EB)), "x", true).contains("没有可用的强调色"));
    }
}
