//! 跳转请求 —— 「点了这条结果之后，界面该往哪走」。
//!
//! 一次跳转分两半：**切页**当场做（`tab.set`），**滚动+闪烁**交给目标位置上的锚点
//! 自己认领（P2）。之所以不当场做完，是因为目标那一刻还不在布局里：它所在的页
//! 上一帧还不可见，`scroll_into_view` 拿到的会是过期 bounds（见规划 §3.1）。

use std::cell::Cell;

use windui::prelude::*;

use super::index::JumpTarget;

/// 一次待执行的跳转。
#[derive(Clone, Debug)]
pub struct JumpRequest {
    /// 每次搜索递增。锚点据此判断「这是不是这一次的目标」，也让新的一次跳转
    /// 自动作废前一次——用户在动画期间再搜一条时，不会有两处同时闪。
    pub seq: u64,
    pub target: JumpTarget,
}

thread_local! {
    static SEQ: Cell<u64> = const { Cell::new(0) };
}

/// 发起一次跳转：切到目标页并挂上请求。
///
/// 方案级目标（`SchemaLevel`）也走这里——它的 `tab()` 就是方案页，请求则由 P4 的
/// 指示条消费，不进锚点。
pub fn request(tab: Signal<usize>, jump: Signal<Option<JumpRequest>>, target: JumpTarget) {
    let seq = SEQ.with(|s| {
        let n = s.get().wrapping_add(1);
        s.set(n);
        n
    });
    tab.set(target.tab());
    jump.set(Some(JumpRequest { seq, target }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_switches_tab_and_bumps_seq() {
        let tab = signal(0usize);
        let jump = signal(None);
        request(
            tab,
            jump,
            JumpTarget::Row {
                tab: 3,
                anchor: "a".into(),
            },
        );
        assert_eq!(tab.get(), 3, "应切到目标页");
        let first = jump.get().expect("应挂上请求").seq;

        request(
            tab,
            jump,
            JumpTarget::Row {
                tab: 1,
                anchor: "b".into(),
            },
        );
        let second = jump.get().expect("应挂上请求").seq;
        assert_ne!(first, second, "每次跳转的 seq 必须不同，否则前一次不会作废");
        assert_eq!(tab.get(), 1);
    }
}
