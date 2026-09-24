//! 「主字颜色」对话框：任务栏图标上「中」「英」主字的四格颜色
//! （`ui.langbar.text_color_{cn,en}_{light,dark}`）。
//!
//! 四格是 2×2 的表：行 = 中文 / 英文，列 = 浅色 / 深色任务栏。每格一个取色器
//! （windui 的 `color_picker`，带透明度），外加「跟随主题」把这一格写回 `""`。
//! 优先级与回落见 config-design-rules R3「外观覆盖主题」：用户值 > 主题值 > 内置，逐格回落。
//!
//! 取色器与配置字符串之间的「版本回声」隔离见 [`PickedColor`]（与角标规则对话框共用）。

use windui::prelude::*;

use crate::theme_follow::ThemeFollow;
use crate::widgets::{
    PickedColor, color_from_value, color_picker_cell, color_swatch, normalize_hex_color,
};

/// 四格：`(配置键, 行名, 列名)`，按行优先排列（中浅、中深、英浅、英深）。
pub const CELLS: [(&str, &str, &str); 4] = [
    ("ui.langbar.text_color_cn_light", "中文", "浅色任务栏"),
    ("ui.langbar.text_color_cn_dark", "中文", "深色任务栏"),
    ("ui.langbar.text_color_en_light", "英文", "浅色任务栏"),
    ("ui.langbar.text_color_en_dark", "英文", "深色任务栏"),
];

/// 第 `i` 格「跟随主题」时取色器里显示、以及格内文字带出的颜色。
///
/// 优先取 `theme_follow.langbar_text[i]`（当前主题对这一格的实际解析值）；主题数据
/// 缺席或该格 `None`（两级主题都没定义）时回落内置默认（浅色栏黑、深色栏白）——
/// 与字号 / 字重那几个键共用同一套「跟随主题（值）」裁决（R8.1）。
pub fn fallback_color(i: usize, theme_hex: Option<&str>) -> Color {
    let builtin = builtin_fallback_color(i);
    match theme_hex {
        Some(s) => color_from_value(s, builtin),
        None => builtin,
    }
}

fn builtin_fallback_color(i: usize) -> Color {
    if i % 2 == 0 {
        Color::hex(0x000000)
    } else {
        Color::hex(0xFFFFFF)
    }
}

/// 一格的摘要状态（卡片行缩略色样与对话框格内文字都据此画）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellState {
    /// `""`：跟随主题。
    FollowTheme,
    /// 合法色值（已规范成大写 `#RRGGBB` / `#RRGGBBAA`）。
    Custom(String),
    /// 手写配置里的非法值，原样保留（core 侧会 warn 并让这一格回落主题）。
    Invalid(String),
}

pub fn cell_state(s: &str) -> CellState {
    let t = s.trim();
    if t.is_empty() {
        return CellState::FollowTheme;
    }
    match normalize_hex_color(t) {
        Some(v) => CellState::Custom(v),
        None => CellState::Invalid(t.to_string()),
    }
}

/// 格内文字：跟随主题时带上主题当前解析出的色值；拿不到主题数据只写「跟随主题」。
pub fn cell_text_with(s: &str, theme_hex: Option<&str>) -> String {
    match cell_state(s) {
        CellState::FollowTheme => match theme_hex.and_then(normalize_hex_color) {
            Some(v) => format!("跟随主题（{v}）"),
            None => "跟随主题".to_string(),
        },
        CellState::Custom(v) => v,
        CellState::Invalid(v) => format!("{v}（无效，按主题）"),
    }
}

/// 配置值 → 取色器的初值。跟随主题 / 非法时用 [`fallback_color`]。
pub fn color_of(s: &str, i: usize, theme_hex: Option<&str>) -> Color {
    color_from_value(s, fallback_color(i, theme_hex))
}

/// 取色器选出的颜色 → 配置值。完全不透明存六位，否则八位（`#RRGGBBAA`）。
///
/// 不透明时**不写 `FF`**：六位是「没配透明度」的规范写法，八位 FF 与它等价却多两位，
/// 同一个色在配置里长两样只会让人疑惑。
pub fn value_of(c: Color) -> String {
    c.to_hex_string()
}

/// 对话框的全部可变状态。
#[derive(Clone)]
pub struct TextColorsUi {
    pub visible: Signal<bool>,
    /// 四格（配置值 `""` = 跟随主题）。写回 / 取值 / 摘要都读 `cells[i].value`。
    pub cells: [PickedColor; 4],
    /// 当前主题的跟随值（`theme.resolved`）。外部（`manifest.rs`）接到
    /// `AppState::theme_follow` 上；构造时先给 `None` 占位。
    pub theme_follow: Signal<Option<ThemeFollow>>,
}

impl TextColorsUi {
    pub fn new() -> Self {
        Self {
            visible: signal(false),
            cells: std::array::from_fn(|i| PickedColor::new("", fallback_color(i, None))),
            theme_follow: signal(None),
        }
    }

    /// 第 `i` 格在 [`Self::theme_follow`] 里对应的主题色（拿不到为 `None`）。
    fn theme_hex(&self, i: usize) -> Option<String> {
        self.theme_follow
            .with(|tf| tf.as_ref().and_then(|t| t.langbar_text[i].clone()))
    }

    /// 程序侧设置第 `i` 格（播种 / 加载 / 恢复默认 / 跟随主题共用），不会被同步回写。
    pub fn set_cell(&self, i: usize, value: &str) {
        let theme_hex = self.theme_hex(i);
        self.cells[i].set(value, color_of(value, i, theme_hex.as_deref()));
    }

    /// 「跟随主题」：写回 `""`。
    pub fn follow_theme(&self, i: usize) {
        self.set_cell(i, "");
    }

    /// 四格当前值（摘要用）。
    pub fn snapshot(&self) -> Vec<String> {
        self.cells.iter().map(|c| c.value.get()).collect()
    }
}

impl Default for TextColorsUi {
    fn default() -> Self {
        Self::new()
    }
}

/// 一枚缩略色样：跟随主题 / 非法值画成描边空块（同 [`color_swatch`]），并挂提示
/// （跟随主题时带上主题当前解析出的色值）。
fn summary_swatch(ui: &TextColorsUi, i: usize, s: &str) -> Element {
    let (_, row, col) = CELLS[i];
    let theme_hex = ui.theme_hex(i);
    color_swatch(s).tooltip(format!(
        "{row} · {col}：{}",
        cell_text_with(s, theme_hex.as_deref())
    ))
}

/// 卡片行右侧：四枚缩略色样（中浅、中深、英浅、英深）+ [设置]。
pub fn build_summary_control(ui: &TextColorsUi) -> Element {
    let summary = signal(vec![ui.snapshot()]);
    let ui_s = ui.clone();
    let vers = ui.cells.clone().map(|c| c.value);
    let tf_sig = ui.theme_follow;
    let sync = crate::pages::dict::reactive::reactive_effect(
        move || {
            let cells_ver: u64 = vers.iter().map(|s| s.version()).sum();
            cells_ver.wrapping_add(tf_sig.version().wrapping_mul(1_000_003))
        },
        move || summary.set(vec![ui_s.snapshot()]),
    );
    let visible = ui.visible;
    let ui_sw = ui.clone();
    Element::row()
        .cross(Align::Center)
        .spacing(8)
        // effect 节点零宽，但放行尾仍多吃一份 spacing、把按钮推离右缘，故挂最前。
        .child(sync)
        .child(
            Element::host_signal(summary, move |vals: Vec<String>| {
                let mut r = Element::row().cross(Align::Center).spacing(4);
                for (i, v) in vals.iter().enumerate() {
                    r = r.child(summary_swatch(&ui_sw, i, v));
                }
                r
            })
            .width(68)
            .height(14),
        )
        .child(
            Element::button("设置")
                .small()
                .outline_soft()
                .on_click(move |_| visible.set(true)),
        )
}

/// 对话框里的一格：取色器色块 + 当前值文字 + 「清除」（回到跟随主题）。
///
/// 按钮不叫「跟随主题」——跟随时旁边的状态文字已写着「跟随主题」，同名按钮读着重复。
fn cell(ui: &TextColorsUi, i: usize) -> Element {
    let value = ui.cells[i].value;
    let ui_f = ui.clone();
    let ui_t = ui.clone();
    color_picker_cell(
        &ui.cells[i],
        move |s| {
            let theme_hex = ui_t.theme_hex(i);
            cell_text_with(s, theme_hex.as_deref())
        },
        70,
        |_, c| value_of(c),
        vec![
            Element::button("清除")
                .small()
                .outline_soft()
                .enabled_when(move || !value.get().trim().is_empty())
                .on_click(move |_| ui_f.follow_theme(i)),
        ],
    )
    .weight(1.0)
}

/// 2×2 表格：表头一行（列名），其下中文 / 英文两行。
pub fn build_grid(ui: &TextColorsUi) -> Element {
    const ROW_LABEL_W: i32 = 48;
    let head = |t: &str| {
        Element::label(t)
            .font_size(12.5)
            .font_weight(600)
            .fg_role(Role::Text)
            .weight(1.0)
    };
    let mut grid = Element::col()
        .width_match()
        .spacing(12)
        .padding_xy(0, 10)
        .child(
            Element::row()
                .width_match()
                .spacing(16)
                .child(Element::leaf().width(ROW_LABEL_W))
                .child(head(CELLS[0].2))
                .child(head(CELLS[1].2)),
        );
    for r in 0..2 {
        grid = grid.child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(16)
                .child(
                    Element::label(CELLS[r * 2].1)
                        .font_size(12.5)
                        .fg_role(Role::Text)
                        .width(ROW_LABEL_W),
                )
                .child(cell(ui, r * 2))
                .child(cell(ui, r * 2 + 1)),
        );
    }
    grid.child(
        Element::label("点色块选色，可调透明度；「清除」回到跟随主题。")
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 键名是跨仓契约，拼错的表现是「选了没反应」，两边都不报错。
    #[test]
    fn cells_cover_the_four_core_keys_row_major() {
        let keys: Vec<&str> = CELLS.iter().map(|c| c.0).collect();
        assert_eq!(
            keys,
            [
                "ui.langbar.text_color_cn_light",
                "ui.langbar.text_color_cn_dark",
                "ui.langbar.text_color_en_light",
                "ui.langbar.text_color_en_dark",
            ]
        );
    }

    #[test]
    fn cell_state_distinguishes_follow_custom_invalid() {
        assert_eq!(cell_state(""), CellState::FollowTheme);
        assert_eq!(cell_state("  "), CellState::FollowTheme);
        assert_eq!(cell_state("#1a2b3c"), CellState::Custom("#1A2B3C".into()));
        assert_eq!(
            cell_state("#1a2b3cdd"),
            CellState::Custom("#1A2B3CDD".into())
        );
        assert_eq!(cell_state("auto"), CellState::Invalid("auto".into()));
        assert_eq!(cell_text_with("", None), "跟随主题");
    }

    /// alpha 规范化：不透明存六位（不写 FF），半透明存八位。
    #[test]
    fn picked_color_normalizes_alpha() {
        assert_eq!(value_of(Color::rgba(0x12, 0x34, 0x56, 0xFF)), "#123456");
        assert_eq!(value_of(Color::rgba(0x12, 0x34, 0x56, 0x80)), "#12345680");
        // 八位往返不丢透明度。
        assert_eq!(value_of(color_of("#12345680", 0, None)), "#12345680");
    }

    #[test]
    fn follow_theme_cells_show_builtin_fallback_in_picker() {
        assert_eq!(color_of("", 0, None), Color::hex(0x000000), "浅色栏回落黑");
        assert_eq!(color_of("", 1, None), Color::hex(0xFFFFFF), "深色栏回落白");
        assert_eq!(color_of("bogus", 3, None), Color::hex(0xFFFFFF));
    }

    /// 主题给了这一格的值时，取色器回落到主题色而不是内置默认。
    #[test]
    fn fallback_color_prefers_theme_over_builtin() {
        assert_eq!(fallback_color(0, Some("#123456")), Color::hex(0x123456));
        assert_eq!(fallback_color(0, None), Color::hex(0x000000));
        // 主题给的值非法时仍回落内置默认，不崩溃。
        assert_eq!(fallback_color(1, Some("bogus")), Color::hex(0xFFFFFF));
    }

    /// 跟随主题时格内文字带上主题当前解析出的色值；拿不到主题数据只写「跟随主题」。
    #[test]
    fn follow_theme_cell_text_carries_theme_color() {
        assert_eq!(cell_text_with("", Some("#000000")), "跟随主题（#000000）");
        assert_eq!(cell_text_with("", None), "跟随主题");
        // 自定义值 / 非法值不受主题影响。
        assert_eq!(cell_text_with("#112233", Some("#000000")), "#112233");
    }

    /// `set_cell` 播种跟随主题格时，取色器显示的是主题色而不是内置默认。
    #[test]
    fn set_cell_follow_theme_uses_linked_theme_signal() {
        let ui = TextColorsUi::new();
        ui.theme_follow.set(Some(ThemeFollow {
            langbar_text: [Some("#ABCDEF".into()), None, None, None],
            ..Default::default()
        }));
        ui.set_cell(0, "");
        assert_eq!(ui.cells[0].color.get(), Color::hex(0xABCDEF));
    }

    /// 四格各自读写，互不串格。
    #[test]
    fn set_cell_writes_only_that_cell() {
        let ui = TextColorsUi::new();
        ui.set_cell(2, "#AABBCC");
        assert_eq!(ui.snapshot(), ["", "", "#AABBCC", ""]);
        assert_eq!(ui.cells[2].color.get(), Color::hex(0xAABBCC));
    }

    /// ★ 程序写进取色器的颜色（播种 / 跟随主题）不得被同步成用户值——否则 `""` 存不住。
    #[test]
    fn programmatic_color_is_not_synced_back() {
        let ui = TextColorsUi::new();
        ui.set_cell(0, "#112233");
        ui.follow_theme(0);
        ui.cells[0].sync(|_, c| value_of(c));
        assert_eq!(ui.cells[0].value.get(), "", "跟随主题被回写成了颜色");
    }

    /// 用户在面板上选色 → 规范化后写进配置值；选中恰好等于回落色也照写。
    #[test]
    fn user_pick_is_synced_even_if_equal_to_fallback() {
        let ui = TextColorsUi::new();
        ui.cells[1].color.set(Color::rgba(0x20, 0x40, 0x60, 0xCC));
        ui.cells[1].sync(|_, c| value_of(c));
        assert_eq!(ui.cells[1].value.get(), "#204060CC");

        ui.cells[0].color.set(fallback_color(0, None));
        ui.cells[0].sync(|_, c| value_of(c));
        assert_eq!(ui.cells[0].value.get(), "#000000", "挑中内置黑也是用户值");

        ui.follow_theme(1);
        assert_eq!(ui.cells[1].value.get(), "", "跟随主题写回空串");
    }
}
