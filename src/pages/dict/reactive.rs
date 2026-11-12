//! 响应式副作用小部件 — 在依赖信号版本变化时运行一次闭包（如"导航变化即重载"）。
//!
//! windui 的 dropdown/segmented 只写回绑定的 `Signal`，无变更回调；也无全局 effect 原语。
//! 本部件挂一个不可见的响应式叶子，框架每帧 layout 前调 [`Widget::on_update`]，
//! 检测依赖信号的组合版本号变化时触发一次 `effect`——把"信号变→跑副作用"这件事集中在一处。

use windui::core::{EventCtx, Widget};
use windui::prelude::*;

/// 反馈信号类型别名：三态 kind + 文案。写操作只写此信号，[`toast_sink`] 按 kind 分派。
pub type Feedback = Signal<Option<(ToastKind, String)>>;

struct ReactiveEffect {
    /// 返回依赖信号的组合版本号（任一依赖变化时结果必变）。
    version_of: Box<dyn Fn() -> u64>,
    effect: Box<dyn FnMut()>,
    last: u64,
    /// 作用域见证：构造时创建，落进**构造它的那个收集帧**（通常是某一行/某一代页面的
    /// 作用域）。见证已死 = 构造它的作用域已回收 = 闭包捕获的信号多半也已回收。
    witness: Signal<()>,
}

impl Widget for ReactiveEffect {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
        // 节点可能比构造它的作用域活得久：经 `register_dialog` 挂到根层的哨兵，所属页面
        // 整代重建后仍在树上残留几帧。此时读死句柄的 `version()` 返回 0，与上次值不同，
        // 会被误判成「有变化」而执行 effect，在 effect 里读写死句柄即崩（图钉翻转崩溃的
        // 直接成因）。见证死了就整体停用，不再求版本、不再执行。
        if !self.witness.is_alive() {
            return;
        }
        let cur = (self.version_of)();
        if cur != self.last {
            self.last = cur;
            (self.effect)();
        }
    }
}

/// toast 反馈叶子：监听 feedback 信号版本变化，在 on_update 里取值并按 kind 分派 ctx.toast_*。
struct ToastSink {
    feedback: Feedback,
    last: u64,
}

impl Widget for ToastSink {
    fn on_update(&mut self, ctx: &mut EventCtx) {
        let cur = self.feedback.version();
        if cur != self.last {
            self.last = cur;
            if let Some((kind, msg)) = self.feedback.get() {
                match kind {
                    ToastKind::Success => ctx.toast_ok(msg),
                    ToastKind::Error => ctx.toast_err(msg),
                    ToastKind::Info => ctx.toast(msg),
                }
            }
        }
    }
}

/// 构造不可见的 toast 反馈节点：feedback 变化即按 kind 弹 toast。
pub fn toast_sink(feedback: Feedback) -> Element {
    let last = feedback.version();
    let w = ToastSink { feedback, last };
    Element::leaf().widget(w).reactive().width(0).height(0)
}

/// 构造一个不可见的响应式 effect 节点。
///
/// - `version_of`：把所监视信号的 `.version()` 组合成一个 u64（任一变则整体变）。
/// - `effect`：检测到变化时运行一次。
///
/// 初始化即以当前版本为基线，故构建帧不会立即触发（初始加载由调用方另行发起）。
pub fn reactive_effect(
    version_of: impl Fn() -> u64 + 'static,
    effect: impl FnMut() + 'static,
) -> Element {
    let last = version_of();
    // 必须在**构造时**创建：`KeyedList::build_row` 是 `scope.collect(|| row_fn(..))` 之后
    // 才 `el.build(tree)`，build 已在收集帧之外，那时创建就归不到行作用域里了。
    let witness = signal(());
    let w = ReactiveEffect {
        version_of: Box::new(version_of),
        effect: Box::new(effect),
        last,
        witness,
    };
    Element::leaf().widget(w).reactive().width(0).height(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use windui::core::Tree;
    use windui::geometry::Size;
    use windui::signal::SignalScope;

    fn frame(tree: &mut Tree) {
        tree.layout_root(Size::new(10, 10), &mut windui::text::NullTextEngine);
    }

    /// 节点比构造它的作用域活得久时（挂根哨兵 + 所属页面整代重建），effect 必须停用：
    /// 死句柄的 `version()` 返回 0，会被误判成「有变化」，effect 里一读死句柄就 panic。
    #[test]
    fn effect_outliving_its_scope_stops_instead_of_reading_dead_signals() {
        let hits = Rc::new(Cell::new(0u32));
        let mut scope = SignalScope::new();
        let (src, el) = scope.collect(|| {
            let src = signal(0u32);
            let h = hits.clone();
            let el = reactive_effect(
                move || src.version(),
                move || {
                    h.set(h.get() + 1);
                    // 真机崩点的同款动作：在 effect 里读写所监视的行作用域信号。
                    let _ = src.get();
                },
            );
            (src, el)
        });
        let mut tree = Tree::new();
        let root = el.build(&mut tree);
        tree.root = Some(root);
        frame(&mut tree);
        assert_eq!(hits.get(), 0, "构建帧以当前版本为基线，不应触发");

        src.set(5);
        frame(&mut tree);
        assert_eq!(hits.get(), 1, "活着时信号变化应触发一次");

        // 作用域回收，但节点仍在树上（挂在作用域之外）。
        scope.dispose();
        frame(&mut tree);
        frame(&mut tree);
        assert_eq!(hits.get(), 1, "作用域回收后 effect 应停用，不应再执行");
    }

    /// 不在任何收集帧里构造的 effect（窗口级哨兵）没有归属，见证永远活着，行为不变。
    #[test]
    fn effect_built_outside_any_scope_keeps_working() {
        let hits = Rc::new(Cell::new(0u32));
        let src = signal(0u32);
        let h = hits.clone();
        let el = reactive_effect(move || src.version(), move || h.set(h.get() + 1));
        let mut tree = Tree::new();
        let root = el.build(&mut tree);
        tree.root = Some(root);
        frame(&mut tree);
        src.set(1);
        frame(&mut tree);
        src.set(2);
        frame(&mut tree);
        assert_eq!(hits.get(), 2);
    }
}
