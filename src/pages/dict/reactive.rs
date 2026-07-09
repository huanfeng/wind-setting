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
}

impl Widget for ReactiveEffect {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
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

/// 旧版 bool 反馈叶子（Task 9/10 前的过渡）：监听 `(是否成功, 文案)` 信号，弹 ok/err 两态 toast。
///
/// `schema_manager`/`AppState` 尚未升级到三态 [`Feedback`]（属 Task 9/10 范围），
/// 此处保留独立的 bool 版本以避免耦合升级，待其迁移完成后本函数可删除。
struct ToastSinkBool {
    feedback: Signal<Option<(bool, String)>>,
    last: u64,
}

impl Widget for ToastSinkBool {
    fn on_update(&mut self, ctx: &mut EventCtx) {
        let cur = self.feedback.version();
        if cur != self.last {
            self.last = cur;
            if let Some((ok, msg)) = self.feedback.get() {
                if ok {
                    ctx.toast_ok(msg);
                } else {
                    ctx.toast_err(msg);
                }
            }
        }
    }
}

/// 构造不可见的 toast 反馈节点（旧版 bool 反馈，见 [`ToastSinkBool`]）。
pub fn toast_sink_bool(feedback: Signal<Option<(bool, String)>>) -> Element {
    let last = feedback.version();
    let w = ToastSinkBool { feedback, last };
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
    let w = ReactiveEffect {
        version_of: Box::new(version_of),
        effect: Box::new(effect),
        last,
    };
    Element::leaf().widget(w).reactive().width(0).height(0)
}
