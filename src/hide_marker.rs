//! 已隐藏设置的**视觉标识**：逃生口打开时，被用户藏掉的项重新出现，用一圈警示色
//! 描边标出来，免得「显示已隐藏的设置」打开后分不清哪些是自己藏过的。
//!
//! 只在 [`UserHideStore::marking_hidden`] 为真（总开关 + 逃生口都开着）时出现；平时
//! 被隐藏的项根本不显示，标识也就无从谈起。
//!
//! # 为什么用覆盖层而不是给行本身加边框
//!
//! 行的外框由各自的控件决定（卡片有卡片的外框），统一加边框会和既有观感打架。这里
//! 在行外叠一层**只画描边、不吞命中**的叶子：`Element::leaf()` 用的是 `EmptyWidget`
//! （`hit_opaque() == false`），且本身无背景、无右键菜单、无提示，`Tree::hit_node` 的
//! 落定判据一条都不满足，于是指针事件与 Ctrl+右键照常落到下面那一行。
//!
//! 反过来，若给它挂 `tooltip` 或 `bg`，命中就会在它这里落定——用户在「显示已隐藏」
//! 模式下右键那行会弹不出菜单，且没有任何报错。

use std::rc::Rc;

use windui::prelude::*;

use crate::user_hide::UserHideStore;

/// 描边颜色。用警示色而不是强调色：强调色在这套界面里表示「选中」，描边表示
/// 「这一项你藏过、现在只是临时露出来」。
///
/// 取警示色的**提亮档**（与暗色主题那份同值）：亮色主题原生的 `0xB45309` 是深琥珀，
/// 压在 `Role::Surface` 上像一条脏边，2px 的细框尤其闷。描边只需要「一眼认得出」，
/// 不需要正文那样的对比度。
fn marker_color() -> Color {
    Color::hex(0xF59E0B)
}

/// 给一个**确实被用户藏过**的节点套标识框。
///
/// `hidden` 是「这一项是否被当前清单藏掉」的判据（忽略逃生口与总开关）——**必须传**：
/// 只按「逃生口开着」判断的话，用户一打开「显示已隐藏的设置」，页面上每一项都会套框，
/// 反而分不出哪些是自己藏过的（2026-10-25 真机反馈）。
pub fn mark(
    store: Rc<UserHideStore>,
    hidden: impl Fn(&UserHideStore) -> bool + 'static,
    el: Element,
) -> Element {
    let shown = store.clone();
    let hidden = Rc::new(hidden);
    let overlay = Element::leaf()
        .corner(8.0)
        .align(Align::Stretch)
        .border(marker_color(), 2)
        .visible_when(move || shown.marking_hidden() && hidden(&shown));
    Element::stack().width_match().child(el).child(overlay)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_hide::{UserHideConfig, UserHideStore};

    fn store(show_hidden: bool) -> Rc<UserHideStore> {
        Rc::new(UserHideStore::in_memory(UserHideConfig {
            enabled: true,
            show_hidden,
            hide: vec!["key:ui.font.family".into()],
        }))
    }

    /// 逃生口不开时标识不显示（项本身也不显示）；开了才画框。
    #[test]
    fn marker_only_shows_with_escape_hatch() {
        assert!(!store(false).marking_hidden());
        assert!(store(true).marking_hidden());
    }

    /// 「此刻该不该藏」与「有没有规则」是两件事：逃生口打开时前者为假、后者仍真。
    #[test]
    fn hides_key_respects_escape_hatch_but_rule_lookup_does_not() {
        let s = store(true);
        assert!(!s.hides_key("ui.font.family"), "逃生口打开，界面要放它出来");
        assert_eq!(
            s.rule_hiding_key("ui.font.family").as_deref(),
            Some("key:ui.font.family"),
            "规则还在，菜单才能给「取消隐藏」"
        );
    }
}
