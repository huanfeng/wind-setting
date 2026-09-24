//! 「主字颜色」对话框：任务栏图标上「中」「英」主字的四格颜色
//! （`ui.langbar.text_color_{cn,en}_{light,dark}`）。
//!
//! 四格是 2×2 的表：行 = 中文 / 英文，列 = 浅色 / 深色任务栏。每格一个取色器
//! （windui 的 `color_picker`，带透明度），外加「跟随主题」把这一格写回 `""`。
//! 优先级与回落见 config-design-rules R3「外观覆盖主题」：用户值 > 主题值 > 内置，逐格回落。
//!
//! # ★ 取色器绑的是 `Color`，配置存的是字符串，两边靠「版本回声」隔开
//!
//! 「跟随主题」这一格在取色器里也得有个颜色可显示（取内置默认：浅色栏黑、深色栏白），
//! 而那个颜色**不是用户选的**。若把 `Color → 字符串` 做成无条件同步，播种 / 恢复默认 /
//! 点「跟随主题」时程序自己写进去的颜色会被当成用户选色写回，`""` 就再也存不住了。
//! 故每次程序写 `Color` 后记下它的版本号（[`TextColorsUi::echo_ver`]），同步时版本没变
//! 就不写回——只有用户在面板上动过，版本才会前进。不用「颜色相等」判：用户完全可能
//! 恰好挑中内置默认那个黑。

use std::cell::Cell;
use std::rc::Rc;

use windui::prelude::*;

use crate::widgets::{color_swatch, normalize_hex_color, parse_color};

/// 四格：`(配置键, 行名, 列名)`，按行优先排列（中浅、中深、英浅、英深）。
pub const CELLS: [(&str, &str, &str); 4] = [
    ("ui.langbar.text_color_cn_light", "中文", "浅色任务栏"),
    ("ui.langbar.text_color_cn_dark", "中文", "深色任务栏"),
    ("ui.langbar.text_color_en_light", "英文", "浅色任务栏"),
    ("ui.langbar.text_color_en_dark", "英文", "深色任务栏"),
];

/// 第 `i` 格在「跟随主题」时取色器里显示的颜色：内置默认（浅色栏黑、深色栏白）。
///
/// 只是个起点——主题可能另有定义，而设置端拿不到解析后的主题色，故不冒充「当前实际色」，
/// 格子旁另有「跟随主题」字样说明。
pub fn fallback_color(i: usize) -> Color {
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

/// 格内文字。
pub fn cell_text(s: &str) -> String {
    match cell_state(s) {
        CellState::FollowTheme => "跟随主题".to_string(),
        CellState::Custom(v) => v,
        CellState::Invalid(v) => format!("{v}（无效，按主题）"),
    }
}

/// 配置值 → 取色器的初值。跟随主题 / 非法时用 [`fallback_color`]。
pub fn color_of(s: &str, i: usize) -> Color {
    match parse_color(s) {
        Some((v, a)) => Color::rgba(
            ((v >> 16) & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
            a.unwrap_or(0xFF),
        ),
        None => fallback_color(i),
    }
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
    /// 四格的配置值（`""` = 跟随主题）。写回 / 取值 / 摘要都读它。
    pub values: [Signal<String>; 4],
    /// 四格取色器绑定的颜色。
    pub colors: [Signal<Color>; 4],
    /// 程序最后一次写 `colors[i]` 后的版本号。见模块文档。
    echo_ver: Rc<[Cell<u64>; 4]>,
}

impl TextColorsUi {
    pub fn new() -> Self {
        let ui = Self {
            visible: signal(false),
            values: std::array::from_fn(|_| signal(String::new())),
            colors: std::array::from_fn(fallback_color).map(signal),
            echo_ver: Rc::new(std::array::from_fn(|_| Cell::new(0))),
        };
        for i in 0..4 {
            ui.echo_ver[i].set(ui.colors[i].version());
        }
        ui
    }

    /// 程序侧设置第 `i` 格（播种 / 加载 / 恢复默认 / 跟随主题共用）：值与取色器一起改，
    /// 并记下回声版本，免得同步把它当成用户选色。
    pub fn set_cell(&self, i: usize, value: &str) {
        self.values[i].set(value.to_string());
        self.colors[i].set(color_of(value, i));
        self.echo_ver[i].set(self.colors[i].version());
    }

    /// 「跟随主题」：写回 `""`。
    pub fn follow_theme(&self, i: usize) {
        self.set_cell(i, "");
    }

    /// 取色器 → 配置值。版本自上次程序写入后没动过就什么都不做（见模块文档）。
    pub fn sync_from_picker(&self, i: usize) {
        let ver = self.colors[i].version();
        if ver == self.echo_ver[i].get() {
            return;
        }
        self.echo_ver[i].set(ver);
        self.values[i].set(value_of(self.colors[i].get()));
    }

    /// 四格当前值（摘要用）。
    pub fn snapshot(&self) -> Vec<String> {
        self.values.iter().map(|s| s.get()).collect()
    }
}

impl Default for TextColorsUi {
    fn default() -> Self {
        Self::new()
    }
}

/// 一枚缩略色样：跟随主题 / 非法值画成描边空块（同 [`color_swatch`]），并挂提示。
fn summary_swatch(i: usize, s: &str) -> Element {
    let (_, row, col) = CELLS[i];
    color_swatch(s).tooltip(format!("{row} · {col}：{}", cell_text(s)))
}

/// 卡片行右侧：四枚缩略色样（中浅、中深、英浅、英深）+ [设置]。
pub fn build_summary_control(ui: &TextColorsUi) -> Element {
    let summary = signal(vec![ui.snapshot()]);
    let ui_s = ui.clone();
    let vers = ui.values;
    let sync = crate::pages::dict::reactive::reactive_effect(
        move || vers.iter().map(|s| s.version()).sum(),
        move || summary.set(vec![ui_s.snapshot()]),
    );
    let visible = ui.visible;
    Element::row()
        .cross(Align::Center)
        .spacing(8)
        // effect 节点零宽，但放行尾仍多吃一份 spacing、把按钮推离右缘，故挂最前。
        .child(sync)
        .child(
            Element::host_signal(summary, |vals: Vec<String>| {
                let mut r = Element::row().cross(Align::Center).spacing(4);
                for (i, v) in vals.iter().enumerate() {
                    r = r.child(summary_swatch(i, v));
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
fn cell(ui: &TextColorsUi, i: usize) -> Element {
    let value = ui.values[i];
    let ui_f = ui.clone();
    let ui_p = ui.clone();
    let colors = ui.colors[i];
    // 取色器写 `colors[i]` → 同步进配置值。
    let sync = crate::pages::dict::reactive::reactive_effect(
        move || colors.version(),
        move || ui_p.sync_from_picker(i),
    );
    // 一行排完：色块 + 当前值 + 「清除」。按钮不再叫「跟随主题」——跟随时旁边的
    // 状态文字已写着「跟随主题」，同名按钮读着重复；单独占一行又让对话框松散。
    Element::row()
        .weight(1.0)
        .cross(Align::Center)
        .spacing(8)
        .child(Element::color_picker_opts(
            ui.colors[i],
            ColorPickerOpts::default().trigger_text(false),
        ))
        .child(
            Element::label_signal(value.map(|s| cell_text(s)))
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width(62)
                .max_lines(1),
        )
        .child(
            Element::button("清除")
                .small()
                .outline_soft()
                .enabled_when(move || !value.get().trim().is_empty())
                .on_click(move |_| ui_f.follow_theme(i)),
        )
        .child(sync)
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
        assert_eq!(cell_text(""), "跟随主题");
    }

    /// alpha 规范化：不透明存六位（不写 FF），半透明存八位。
    #[test]
    fn picked_color_normalizes_alpha() {
        assert_eq!(value_of(Color::rgba(0x12, 0x34, 0x56, 0xFF)), "#123456");
        assert_eq!(value_of(Color::rgba(0x12, 0x34, 0x56, 0x80)), "#12345680");
        // 八位往返不丢透明度。
        assert_eq!(value_of(color_of("#12345680", 0)), "#12345680");
    }

    #[test]
    fn follow_theme_cells_show_builtin_fallback_in_picker() {
        assert_eq!(color_of("", 0), Color::hex(0x000000), "浅色栏回落黑");
        assert_eq!(color_of("", 1), Color::hex(0xFFFFFF), "深色栏回落白");
        assert_eq!(color_of("bogus", 3), Color::hex(0xFFFFFF));
    }

    /// 四格各自读写，互不串格。
    #[test]
    fn set_cell_writes_only_that_cell() {
        let ui = TextColorsUi::new();
        ui.set_cell(2, "#AABBCC");
        assert_eq!(ui.snapshot(), ["", "", "#AABBCC", ""]);
        assert_eq!(ui.colors[2].get(), Color::hex(0xAABBCC));
    }

    /// ★ 程序写进取色器的颜色（播种 / 跟随主题）不得被同步成用户值——否则 `""` 存不住。
    #[test]
    fn programmatic_color_is_not_synced_back() {
        let ui = TextColorsUi::new();
        ui.set_cell(0, "#112233");
        ui.follow_theme(0);
        ui.sync_from_picker(0);
        assert_eq!(ui.values[0].get(), "", "跟随主题被回写成了颜色");
    }

    /// 用户在面板上选色 → 规范化后写进配置值；选中恰好等于回落色也照写。
    #[test]
    fn user_pick_is_synced_even_if_equal_to_fallback() {
        let ui = TextColorsUi::new();
        ui.colors[1].set(Color::rgba(0x20, 0x40, 0x60, 0xCC));
        ui.sync_from_picker(1);
        assert_eq!(ui.values[1].get(), "#204060CC");

        ui.colors[0].set(fallback_color(0));
        ui.sync_from_picker(0);
        assert_eq!(ui.values[0].get(), "#000000", "挑中内置黑也是用户值");

        ui.follow_theme(1);
        assert_eq!(ui.values[1].get(), "", "跟随主题写回空串");
    }
}
