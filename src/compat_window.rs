//! 应用兼容独立窗 — 与主窗同进程的 windui 子窗（`EventCtx::open_window`），做法同
//! [`crate::dict_window`]：`Signal` 是 `Copy` 句柄可直接传进来，换肤 / toast 天然联动。
//!
//! 窗内三栏：左栏分类（应用规则 / 窗口作用域 / 上屏换行）与整体操作，中栏进程列表，右栏配置信息。
//! 入口在设置窗「高级」页的「应用兼容」卡片，以及全局搜索里的应用兼容条目。
//!
//! 约束与词库窗相同（违反都是静默失败）：开窗只能在带 `EventCtx` 且副作用被采纳的时机；
//! `content` 闭包里新建的 `Signal` 归本窗所有、关窗即回收，所以页内的载体信号都在
//! [`crate::pages::compat::build_content`] 里现建，不存回长命的 `CompatState`。

use std::cell::RefCell;
use std::rc::Rc;

use windui::core::EventCtx;
use windui::prelude::*;

use crate::state::AppState;

/// 窗口标题。`App` 主窗承载本界面时（`--page compat` 冷启动）也用这个。
pub const TITLE: &str = "清风输入法 · 应用兼容性与独立配置";

/// 单例键：同键窗口同时只存在一个，再点入口只是把已开着的那个激活到前台。
pub(crate) const SINGLE_KEY: &str = "compat_manager";

/// 打开应用兼容窗。
///
/// 已开着时 `single` 激活既有窗口；无论新开还是激活，都由内容构建 / 激活回调各刷新一次数据
/// ——`compat_mgr` 活在 `AppState` 里、比窗口长命，重开窗不刷新就会显示上次留下的旧规则。
pub fn open(ctx: &mut EventCtx, app: &Rc<RefCell<AppState>>) {
    let app_for_content = app.clone();
    let compat = app.borrow().compat_mgr.clone();
    ctx.open_window(
        Window::new(TITLE, crate::COMPAT_W, crate::COMPAT_H)
            .single(SINGLE_KEY)
            .frameless(true)
            .centered(true)
            .min_size(crate::COMPAT_MIN_W, crate::COMPAT_MIN_H)
            // 回到本窗就刷新：右键菜单等外部途径改过 compat.toml 而本窗一直开着时，这是
            // 唯一能让它看到最新值的时机（菜单的改动同时会广播事件，但断线重连期间可能漏掉）。
            .on_window_activated(move |_ctx, active| {
                if active {
                    compat.request_refresh();
                }
            })
            // Ctrl+W 关窗：`App::on_shortcut` 只归主窗，子窗要自己接（见 `shortcuts::is_close_window`）。
            .on_shortcut(|ctx, ev| {
                if crate::shortcuts::is_close_window(&ev) {
                    ctx.request_close();
                    true
                } else {
                    false
                }
            })
            .content(move || build_content(app_for_content)),
    );
}

/// 窗内容：标题栏 + 三栏主体 + 浮层对话框。
pub fn build_content(app: Rc<RefCell<AppState>>) -> Element {
    let mgr = app.borrow().compat_mgr.clone();
    crate::pages::compat::build_content(
        &mgr,
        crate::shell::build_titlebar("应用兼容性与独立配置"),
        build_back_button(app),
    )
}

/// 左栏底部的「← 设置」：开着设置窗就激活它，关了就重建，随后关掉本窗（同词库窗）。
fn build_back_button(app: Rc<RefCell<AppState>>) -> Element {
    Element::button("← 设置")
        .small()
        .outline_soft()
        .on_click(move |ctx| {
            crate::open_main_window(ctx, &app);
            ctx.request_close();
        })
}
