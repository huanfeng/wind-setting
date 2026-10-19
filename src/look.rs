//! 设置窗「界面细节」试验选项（见 docs/design/settings-look-prefs.md）。
//!
//! **过渡代码**：用来在真实界面里并排比较几种写法。每个选项定案后删掉落选变体，
//! 全部定案后整个模块连同 `look_window` 一起删除。
//!
//! windui 的响应式样式只有可见性一轴，故各处把变体都建出来、按这里的信号
//! `visible_when` 只显示一个（同 `widgets::dark_sig` 的亮 / 暗双元素做法）——
//! 改选项不需要重建设置树。

use serde::{Deserialize, Serialize};
use windui::prelude::*;

/// 一个选项的取值表。**下标 0 = 缺省**（2026-10-14 用户在真机比较后定下的写法）。
/// 代码里一律用下面各模块的具名下标比较，别写裸数字——顺序调过一次，以后还可能再调。
pub struct LookOption {
    pub title: &'static str,
    /// 写进 `[ui.look]` 的配置串。
    pub values: &'static [&'static str],
    pub labels: &'static [&'static str],
}

pub const NAV_INDICATOR: usize = 0;
pub const NAV_ICON_CHIP: usize = 1;
pub const SECTION_HEADER: usize = 2;
pub const ROW_DIVIDER: usize = 3;
pub const SUBHEADING: usize = 4;
pub const CARD_EDGE: usize = 5;

/// 各选项的缺省下标（未写 `[ui.look]` 时用它）：各表第一项。
pub const DEFAULT: [usize; 6] = [0; 6];

/// 导航选中样式的下标。
pub mod nav {
    pub const TINT: usize = 0;
    pub const BAR: usize = 1;
    pub const SOLID: usize = 2;
}
/// 主导航图标块的下标。
pub mod chip {
    pub const TINT_ONLY: usize = 0;
    pub const GRADIENT: usize = 1;
}
/// 分组标题的下标。
pub mod header {
    pub const INSIDE: usize = 0;
    pub const OUTSIDE: usize = 1;
    pub const INSIDE_PLAIN: usize = 2;
}
/// 行分割线的下标。
pub mod divider {
    pub const FULL: usize = 0;
    pub const INSET: usize = 1;
    pub const LEADING: usize = 2;
}
/// 卡内二级分类的下标。
pub mod sub {
    pub const BAND: usize = 0;
    pub const ACCENT_RULE: usize = 1;
    pub const MUTED_GAP: usize = 2;
}
/// 卡片边界的下标。
pub mod edge {
    pub const BORDER_SHADOW: usize = 0;
    pub const BORDER: usize = 1;
    pub const SHADOW: usize = 2;
}

pub const OPTIONS: [LookOption; 6] = [
    LookOption {
        title: "导航选中样式",
        values: &["tint", "bar_tint", "solid"],
        labels: &["仅淡底", "竖条+淡底", "实心"],
    },
    LookOption {
        title: "主导航图标块",
        values: &["tint_only", "gradient"],
        labels: &["仅图标变色", "渐变图标块"],
    },
    LookOption {
        title: "分组标题",
        values: &["inside", "outside", "inside_plain"],
        labels: &["卡内", "卡外", "卡内无图标"],
    },
    LookOption {
        title: "行分割线",
        values: &["full", "inset", "leading"],
        labels: &["通栏", "两端内缩", "仅左缩"],
    },
    LookOption {
        title: "卡内二级分类",
        values: &["band", "accent_rule", "muted_gap"],
        labels: &["浅色底条", "强调色+线", "灰字+留白"],
    },
    LookOption {
        // 建树时读一次（见 `widgets::card_shell`），故标题里写明。
        title: "卡片边界（重开生效）",
        values: &["border_shadow", "border", "shadow"],
        labels: &["边框+阴影", "仅边框", "仅阴影"],
    },
];

/// `[ui.look]`。字段用 `Option<String>` 而不是枚举：未知取值若让 serde 报错，整个
/// `wind_setting.toml` 都会加载失败，明暗、升级偏好会跟着一起丢。解析在 [`Self::indices`]
/// 里做，认不出的回落缺省。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LookConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nav_indicator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nav_icon_chip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_divider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subheading: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_edge: Option<String>,
}

impl LookConfig {
    /// 字段顺序必须与 [`OPTIONS`] 一致。
    fn fields(&self) -> [&Option<String>; 6] {
        [
            &self.nav_indicator,
            &self.nav_icon_chip,
            &self.section_header,
            &self.row_divider,
            &self.subheading,
            &self.card_edge,
        ]
    }

    pub fn indices(&self) -> [usize; 6] {
        let f = self.fields();
        std::array::from_fn(|i| {
            f[i].as_deref()
                .and_then(|v| OPTIONS[i].values.iter().position(|x| *x == v))
                .unwrap_or(DEFAULT[i])
        })
    }

    /// 等于 [`DEFAULT`] 的写成 `None`，于是全缺省时整节不落盘。
    pub fn from_indices(ix: [usize; 6]) -> Self {
        let v = |i: usize| {
            (ix[i] != DEFAULT[i])
                .then(|| OPTIONS[i].values.get(ix[i]).map(|s| (*s).to_string()))
                .flatten()
        };
        Self {
            nav_indicator: v(NAV_INDICATOR),
            nav_icon_chip: v(NAV_ICON_CHIP),
            section_header: v(SECTION_HEADER),
            row_divider: v(ROW_DIVIDER),
            subheading: v(SUBHEADING),
            card_edge: v(CARD_EDGE),
        }
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// 六个选项的选中下标。存在 `AppState` 里、比任何窗口都长命：子窗的分段控件直接绑它们，
/// 主窗各处 `visible_when` 读同一组。
#[derive(Clone, Copy)]
pub struct LookSignals(pub [Signal<usize>; 6]);

impl LookSignals {
    pub fn new(ix: [usize; 6]) -> Self {
        Self(std::array::from_fn(|i| signal(ix[i])))
    }

    pub fn snapshot(&self) -> [usize; 6] {
        std::array::from_fn(|i| self.0[i].get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_come_from_default_table_and_are_not_serialized() {
        let c = LookConfig::default();
        assert_eq!(c.indices(), DEFAULT);
        assert!(c.is_default());
        assert_eq!(
            LookConfig::from_indices(DEFAULT),
            c,
            "全选缺省必须回到全 None，才不落盘"
        );
    }

    #[test]
    fn indices_roundtrip_through_config_strings() {
        let ix = [2, 1, 1, 2, 1, 2];
        assert_eq!(LookConfig::from_indices(ix).indices(), ix);
    }

    #[test]
    fn unknown_value_falls_back_to_default() {
        let c = LookConfig {
            row_divider: Some("zigzag".into()),
            ..Default::default()
        };
        assert_eq!(c.indices()[ROW_DIVIDER], DEFAULT[ROW_DIVIDER]);
    }

    /// 具名下标必须指向同名配置串：调顺序时漏改一处，变体就会张冠李戴。
    #[test]
    fn named_indices_match_config_strings() {
        let v = |opt: usize, ix: usize| OPTIONS[opt].values[ix];
        assert_eq!(v(NAV_INDICATOR, nav::TINT), "tint");
        assert_eq!(v(NAV_INDICATOR, nav::BAR), "bar_tint");
        assert_eq!(v(NAV_INDICATOR, nav::SOLID), "solid");
        assert_eq!(v(NAV_ICON_CHIP, chip::TINT_ONLY), "tint_only");
        assert_eq!(v(NAV_ICON_CHIP, chip::GRADIENT), "gradient");
        assert_eq!(v(SECTION_HEADER, header::INSIDE), "inside");
        assert_eq!(v(SECTION_HEADER, header::OUTSIDE), "outside");
        assert_eq!(v(SECTION_HEADER, header::INSIDE_PLAIN), "inside_plain");
        assert_eq!(v(ROW_DIVIDER, divider::FULL), "full");
        assert_eq!(v(ROW_DIVIDER, divider::INSET), "inset");
        assert_eq!(v(ROW_DIVIDER, divider::LEADING), "leading");
        assert_eq!(v(SUBHEADING, sub::BAND), "band");
        assert_eq!(v(SUBHEADING, sub::ACCENT_RULE), "accent_rule");
        assert_eq!(v(SUBHEADING, sub::MUTED_GAP), "muted_gap");
        assert_eq!(v(CARD_EDGE, edge::BORDER_SHADOW), "border_shadow");
        assert_eq!(v(CARD_EDGE, edge::BORDER), "border");
        assert_eq!(v(CARD_EDGE, edge::SHADOW), "shadow");
    }

    /// 配置里存的是串不是下标：调顺序前写下的值读回来仍是同一种写法。
    #[test]
    fn saved_strings_survive_reordering() {
        let c = LookConfig {
            row_divider: Some("inset".into()),
            subheading: Some("accent_rule".into()),
            ..Default::default()
        };
        let ix = c.indices();
        assert_eq!(ix[ROW_DIVIDER], divider::INSET);
        assert_eq!(ix[SUBHEADING], sub::ACCENT_RULE);
    }

    #[test]
    fn options_tables_are_consistent() {
        for o in &OPTIONS {
            assert_eq!(o.values.len(), o.labels.len(), "{}", o.title);
            assert!(o.values.len() >= 2, "{}", o.title);
        }
    }
}
