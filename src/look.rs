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

/// 一个选项的取值表。下标 0 = 试验开始前的写法；缺省值见 [`DEFAULT`]。
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

/// 各选项的缺省下标（未写 `[ui.look]` 时用它）。2026-10-14 用户在真机上比较后定下：
/// 仅淡底 / 仅图标变色 / 卡内标题 / 通栏分割线 / 浅色底条 / 边框+阴影。
pub const DEFAULT: [usize; 6] = [1, 1, 1, 1, 2, 0];

pub const OPTIONS: [LookOption; 6] = [
    LookOption {
        title: "导航选中样式",
        values: &["bar_tint", "tint", "solid"],
        labels: &["竖条+淡底", "仅淡底", "实心"],
    },
    LookOption {
        title: "主导航图标块",
        values: &["gradient", "tint_only"],
        labels: &["渐变图标块", "仅图标变色"],
    },
    LookOption {
        title: "分组标题",
        values: &["outside", "inside", "inside_plain"],
        labels: &["卡外", "卡内", "卡内无图标"],
    },
    LookOption {
        title: "行分割线",
        values: &["inset", "full", "leading"],
        labels: &["两端内缩", "通栏", "仅左缩"],
    },
    LookOption {
        title: "卡内二级分类",
        values: &["accent_rule", "muted_gap", "band"],
        labels: &["强调色+线", "灰字+留白", "浅色底条"],
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

    #[test]
    fn explicit_index_zero_is_kept_when_it_differs_from_default() {
        // 缺省不再是 0：选回「试验前的写法」必须落盘，否则重开后又变回缺省。
        let c = LookConfig::from_indices([0; 6]);
        assert!(!c.is_default());
        assert_eq!(c.indices(), [0; 6]);
    }

    #[test]
    fn options_tables_are_consistent() {
        for o in &OPTIONS {
            assert_eq!(o.values.len(), o.labels.len(), "{}", o.title);
            assert!(o.values.len() >= 2, "{}", o.title);
        }
    }
}
