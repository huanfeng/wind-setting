//! 窗口级快捷键 —— 一张表，加一条就是加一行。
//!
//! 快捷键零散写在 `main.rs` 的 `on_shortcut` 闭包里，加到第三条就会开始互相遮挡，
//! 也没地方回答「现在到底有哪些快捷键」。故集中成 [`BINDINGS`] 一张表：
//! **键组合 → 动作**，动作的落地在 [`dispatch`]。新增一条只需在表里加一行 +
//! 在 [`Action`] 里加一个变体，编译器会提醒你把它接上。
//!
//! ## 为什么返回意图而不是直接调 ctx
//!
//! [`dispatch`] 返回 [`Outcome`]，由 `main.rs` 翻译成 `ShortcutCtx` 上的调用。
//! 这样分派逻辑（哪个键做什么、什么条件下才算处理）是纯函数，可以直接单测——
//! `ShortcutCtx` 的字段是 crate 私有的，测试里既造不出也读不到。
//!
//! ## 「没处理」也是一种结果
//!
//! `App::on_shortcut` 返回 false 表示让位给宿主的默认行为（Tab 焦点导航、Escape 关窗）。
//! Escape 正是靠这一点两用：浮层开着就收浮层，没开就让位给默认的关窗兜底。

use windui::event::{Key, KeyEvent};

use crate::state::AppState;

/// 虚拟键码。
///
/// ⚠ Windows 上 **Ctrl+字母到达时是 `Key::Other(VK码)`，不是 `Key::Char`**——
/// windui 自己的 TextInput 处理 Ctrl+A 用的就是 `Key::Other(0x41)`。
/// `App::on_shortcut` 的文档示例写作 `Key::Char('l') if ev.ctrl`，照抄会静默失效
/// （本功能第一版的 Ctrl+F 就是这么没的）。
mod vk {
    pub const F: u32 = 0x46;
    pub const W: u32 = 0x57;
}

/// 快捷键要做的事。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// 焦点回到搜索框（并展开已有查询的结果）。
    FocusSearch,
    /// 收起搜索浮层。浮层没开时**不处理**，让位给宿主的 Escape 关窗兜底。
    DismissSearch,
    /// 关闭窗口，走完整的关闭决策链（有未保存改动会弹确认框）。
    CloseWindow,
}

/// 一条绑定。
pub struct Binding {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub action: Action,
    /// 人读名称。将来做「快捷键一览」或自定义绑定时直接用这份。
    ///
    /// 现在只有测试读它（查重时报是哪两条撞了）。**先留着**：这张表存在的意义就是
    /// 「一处回答现在有哪些快捷键」，名字缺了就答不上来。
    #[allow(dead_code)]
    pub label: &'static str,
}

impl Binding {
    const fn new(key: Key, ctrl: bool, action: Action, label: &'static str) -> Self {
        Self {
            key,
            ctrl,
            shift: false,
            action,
            label,
        }
    }

    /// 这次按键是否命中本绑定。修饰键**逐个精确匹配**：不比 shift 的话，
    /// Ctrl+Shift+W 会被当成 Ctrl+W，撞掉将来可能要用的组合。
    ///
    /// （`KeyEvent` 目前只带 `ctrl` 与 `shift` 两个修饰位，没有 alt/meta；
    /// 要用 Alt 组合得先给 windui 补上。）
    fn matches(&self, ev: &KeyEvent) -> bool {
        self.key == ev.key && self.ctrl == ev.ctrl && self.shift == ev.shift
    }
}

/// 全部窗口级快捷键。**新增快捷键在此加一行。**
pub const BINDINGS: &[Binding] = &[
    Binding::new(Key::Other(vk::F), true, Action::FocusSearch, "搜索设置"),
    Binding::new(Key::Other(vk::W), true, Action::CloseWindow, "关闭窗口"),
    // Escape 不带修饰键；它只在浮层开着时才算处理（见 `Action::DismissSearch`）。
    Binding::new(Key::Escape, false, Action::DismissSearch, "收起搜索结果"),
];

/// 分派结果：`main.rs` 据此调用 `ShortcutCtx`。
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Outcome {
    /// 是否已处理（`on_shortcut` 的返回值）。false = 让位给宿主默认行为。
    pub handled: bool,
    pub request_close: bool,
    pub focus_main_input: bool,
}

impl Outcome {
    const HANDLED: Self = Self {
        handled: true,
        request_close: false,
        focus_main_input: false,
    };
}

/// 按下 `ev` 时该做什么。纯函数，副作用只有对 `AppState` 里搜索状态的写入。
pub fn dispatch(ev: &KeyEvent, st: &AppState) -> Outcome {
    let Some(b) = BINDINGS.iter().find(|b| b.matches(ev)) else {
        return Outcome::default();
    };
    match b.action {
        Action::FocusSearch => {
            // 展开在这里当场做，**不等焦点哨兵**：哨兵挂在 `on_update` 上，而那只在
            // 需要重排的帧才跑；焦点转移本身不保证后面还有这样一帧，于是浮层要等到
            // 下一次交互才弹出来——真机上表现为「按完 Ctrl+F 愣一秒才出结果」。
            if !st.search.query.get().trim().is_empty() {
                st.search.open.set(true);
            }
            Outcome {
                focus_main_input: true,
                ..Outcome::HANDLED
            }
        }
        Action::DismissSearch => {
            if st.search.open.get() {
                st.search.dismiss();
                Outcome::HANDLED
            } else {
                // 没开就别吃这个键：Escape 还要留给宿主做关窗兜底。
                Outcome::default()
            }
        }
        Action::CloseWindow => Outcome {
            request_close: true,
            ..Outcome::HANDLED
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(key: Key, ctrl: bool) -> KeyEvent {
        KeyEvent {
            key,
            ctrl,
            shift: false,
            pressed: true,
        }
    }

    fn state() -> AppState {
        AppState::empty()
    }

    /// 表里不能有两条撞在同一个组合上——后一条永远轮不到，且没有任何提示。
    #[test]
    fn bindings_are_unique() {
        for (i, a) in BINDINGS.iter().enumerate() {
            for b in &BINDINGS[i + 1..] {
                assert!(
                    !(a.key == b.key && a.ctrl == b.ctrl && a.shift == b.shift),
                    "「{}」与「{}」绑定了同一个组合",
                    a.label,
                    b.label
                );
            }
        }
    }

    /// 每条都要有名字：将来的「快捷键一览」直接读这份表。
    #[test]
    fn bindings_have_labels() {
        for b in BINDINGS {
            assert!(!b.label.is_empty());
        }
    }

    #[test]
    fn ctrl_f_focuses_search() {
        let st = state();
        let out = dispatch(&ev(Key::Other(vk::F), true), &st);
        assert!(out.handled);
        assert!(out.focus_main_input);
    }

    /// 已有查询时，Ctrl+F 当场把浮层展开——不留给哨兵，那要等下一个重排帧。
    #[test]
    fn ctrl_f_reopens_overlay_immediately() {
        let st = state();
        st.search.query.set("码表".into());
        st.search.dismiss();
        assert!(!st.search.open.get());

        dispatch(&ev(Key::Other(vk::F), true), &st);
        assert!(st.search.open.get(), "有查询时 Ctrl+F 应立刻展开结果");
    }

    /// 空查询时不弹空浮层。
    #[test]
    fn ctrl_f_with_empty_query_does_not_open() {
        let st = state();
        dispatch(&ev(Key::Other(vk::F), true), &st);
        assert!(!st.search.open.get());
    }

    #[test]
    fn ctrl_w_requests_close() {
        let st = state();
        let out = dispatch(&ev(Key::Other(vk::W), true), &st);
        assert!(out.handled);
        assert!(out.request_close);
    }

    /// 浮层开着时 Escape 收浮层。
    #[test]
    fn escape_dismisses_open_overlay() {
        let st = state();
        st.search.query.set("码表".into());
        st.search.open.set(true);

        let out = dispatch(&ev(Key::Escape, false), &st);
        assert!(out.handled, "浮层开着时应吃掉 Escape");
        assert!(!st.search.open.get());
    }

    /// **回归**：浮层没开时 Escape 必须放行。
    ///
    /// 吃掉它的话，宿主的关窗兜底就永远轮不到——用户按 Esc 关不掉设置窗。
    #[test]
    fn escape_passes_through_when_overlay_closed() {
        let st = state();
        let out = dispatch(&ev(Key::Escape, false), &st);
        assert!(!out.handled, "浮层没开时 Escape 该让位给关窗兜底");
    }

    /// 不带 Ctrl 的 F 是普通输入，不能被当成快捷键吃掉。
    #[test]
    fn plain_key_is_not_a_shortcut() {
        let st = state();
        assert!(!dispatch(&ev(Key::Other(vk::F), false), &st).handled);
    }

    /// 修饰键要精确匹配：Ctrl+Shift+W 不是 Ctrl+W。
    #[test]
    fn modifiers_match_exactly() {
        let st = state();
        let mut e = ev(Key::Other(vk::W), true);
        e.shift = true;
        assert!(!dispatch(&e, &st).handled);
    }
}
