//! 响应式副作用小部件 — 在依赖信号版本变化时运行一次闭包（如"导航变化即重载"）。
//!
//! windui 的 dropdown/segmented 只写回绑定的 `Signal`，无变更回调；也无全局 effect 原语。
//! 本部件挂一个不可见的响应式叶子，框架每帧 layout 前调 [`Widget::on_update`]，
//! 检测依赖信号的组合版本号变化时触发一次 `effect`——把"信号变→跑副作用"这件事集中在一处。

use windui::core::{EventCtx, Widget};
use windui::prelude::*;

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
