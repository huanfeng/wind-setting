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
    /// 提交词库页正开着的加/改词类对话框。没开着就**不处理**——Enter 在别处
    /// （列表、按钮、搜索框）还有它自己的用途，吃掉就成了误伤。
    SubmitDictDialog,
}

/// 词库页哪个对话框在等着提交——两张表的提交出口不是同一个函数。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DictSubmit {
    /// 加/改词、快捷输入、常用字：`DictManagerState::submit_word`。
    Word,
    /// 候选调整（固定位置/隐藏）：`DictManagerState::submit_shadow`。
    Shadow,
}

/// 此刻有没有加词类对话框在前台等着提交。
///
/// **只认这四个**：删除确认（`confirm_visible`）、只读查看、导入导出等对话框一概不接
/// 回车——确认框上回车直接删东西，是拿不回来的那种误伤。
fn pending_dict_submit(st: &AppState) -> Option<DictSubmit> {
    let m = &st.dict_mgr;
    if m.shadow_add_visible.get() {
        return Some(DictSubmit::Shadow);
    }
    if m.edit_visible.get() || m.quick_edit_visible.get() || m.common_edit_visible.get() {
        return Some(DictSubmit::Word);
    }
    None
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
    // 加词类对话框的键盘出口。**两条都要**：裸 Enter 覆盖焦点停在单行框以外
    // （对话框刚开、点过某个按钮）的情形，Ctrl+Enter 覆盖焦点在多行正文框里——
    // 那里裸 Enter 是换行，键根本到不了这里。
    //
    // 这是**兜底**：`on_shortcut` 排在焦点控件之后，输入框自己的 `on_submit`
    // （见 `pages::dict::dialogs`）先接，接不着才轮到这里。
    Binding::new(
        Key::Enter,
        false,
        Action::SubmitDictDialog,
        "提交加词对话框",
    ),
    Binding::new(
        Key::Enter,
        true,
        Action::SubmitDictDialog,
        "提交加词对话框（正文框内）",
    ),
];

/// 这次按键是不是「关闭窗口」（Ctrl+W）。
///
/// 给**子窗**用：`App::on_shortcut` 只归主窗，词库窗 / 应用兼容窗这类 `open_window` 出来的
/// 子窗要靠自己的 `Window::on_shortcut`，而它们只需要这一条——搜索框、加词对话框那几条
/// 都是设置窗里的东西，拿整张表去分派只会误触（例如在子窗里 Ctrl+F 去聚焦设置窗的搜索框）。
pub fn is_close_window(ev: &KeyEvent) -> bool {
    ev.pressed
        && BINDINGS
            .iter()
            .any(|b| b.action == Action::CloseWindow && b.matches(ev))
}

/// 分派结果：`main.rs` 据此调用 `ShortcutCtx`。
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Outcome {
    /// 是否已处理（`on_shortcut` 的返回值）。false = 让位给宿主默认行为。
    pub handled: bool,
    pub request_close: bool,
    pub focus_main_input: bool,
    /// 该提交词库页的哪张表；`None` = 这次按键与加词对话框无关。
    ///
    /// 与其余字段一样是**意图**而非动作：提交要发 RPC，放进 `dispatch` 就把这个纯函数
    /// 连同它的单测一起绑到了 core 上。落地在 `main.rs`。
    pub submit_dict: Option<DictSubmit>,
}

impl Outcome {
    const HANDLED: Self = Self {
        handled: true,
        request_close: false,
        focus_main_input: false,
        submit_dict: None,
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
            st.search.reopen();
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
        Action::SubmitDictDialog => match pending_dict_submit(st) {
            Some(kind) => Outcome {
                submit_dict: Some(kind),
                ..Outcome::HANDLED
            },
            // 没有加词对话框开着就别吃这个键——Enter 在列表、按钮、搜索框上各有各的用途。
            None => Outcome::default(),
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
            alt: false,
            meta: false,
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

    /// ★ **没有对话框开着时，回车必须放行。** 这是本条快捷键的全部风险所在：Enter 是
    /// 全窗共享的键，门开得松一点，用户在词库列表里、在某个按钮上按回车都会莫名其妙地
    /// 提交一次加词表单。
    #[test]
    fn enter_is_left_alone_when_no_add_word_dialog_is_open() {
        let st = state();
        for ctrl in [false, true] {
            let out = dispatch(&ev(Key::Enter, ctrl), &st);
            assert!(
                !out.handled && out.submit_dict.is_none(),
                "没有加词对话框开着时，回车（ctrl={ctrl}）不该被吃掉"
            );
        }
    }

    /// 三张走 `submit_word` 的表：开着哪一张，回车就提交哪一张。
    ///
    /// 用「名字 + 开窗闭包」而不是按字符串 match 分派：后者把某个名字拼错时会静默落进
    /// 兜底分支，测试照绿，而那一张表其实一次都没测到。
    #[test]
    fn enter_submits_the_open_word_dialog() {
        fn check(name: &str, show: impl Fn(&crate::pages::dict::state::DictManagerState)) {
            let st = state();
            show(&st.dict_mgr);
            for ctrl in [false, true] {
                let out = dispatch(&ev(Key::Enter, ctrl), &st);
                assert!(out.handled, "{name} 开着时回车应被处理");
                assert_eq!(
                    out.submit_dict,
                    Some(DictSubmit::Word),
                    "{name}（ctrl={ctrl}）应走 submit_word"
                );
            }
        }
        check("加/改词", |m| m.edit_visible.set(true));
        check("快捷输入", |m| m.quick_edit_visible.set(true));
        check("常用字", |m| m.common_edit_visible.set(true));
    }

    /// 候选调整那张表的提交出口是 `submit_shadow`，不是 `submit_word`——接错了表面上
    /// 一样「回车有反应」，写进去的却是另一张表。
    #[test]
    fn enter_in_shadow_dialog_routes_to_shadow_submit() {
        let st = state();
        st.dict_mgr.shadow_add_visible.set(true);
        let out = dispatch(&ev(Key::Enter, false), &st);
        assert_eq!(out.submit_dict, Some(DictSubmit::Shadow));
    }

    /// 删除确认框上回车**不提交**：那是拿不回来的误伤，只有加词类的四张表接回车。
    #[test]
    fn enter_does_not_touch_the_confirm_dialog() {
        let st = state();
        st.dict_mgr.confirm_visible.set(true);
        let out = dispatch(&ev(Key::Enter, false), &st);
        assert!(
            !out.handled && out.submit_dict.is_none(),
            "确认框开着时回车必须放行给它自己的按钮"
        );
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

    /// 子窗只认 Ctrl+W，且按下沿才算（释放沿不能再关一次）。
    #[test]
    fn sub_windows_only_close_on_ctrl_w_press() {
        assert!(is_close_window(&ev(Key::Other(vk::W), true)));
        assert!(!is_close_window(&ev(Key::Other(vk::W), false)));
        assert!(!is_close_window(&ev(Key::Other(vk::F), true)));
        let mut up = ev(Key::Other(vk::W), true);
        up.pressed = false;
        assert!(!is_close_window(&up));
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
