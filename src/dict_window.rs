//! 词库管理独立窗 — 与主窗同进程的 windui 子窗（`EventCtx::open_window`）。
//!
//! 与快捷加词小窗（[`crate::add_word_window`]）**不是一回事**：那个是另起一个进程、
//! 另一个 `App::run`、另一个单例 id，跨窗共享状态只能绕 RPC 回 core；这里是同进程子窗，
//! `Signal` 是 `Copy` 句柄可直接传进来，`ThemeHandle` 也共享 —— 换肤、数据、toast 全部
//! 天然联动，不需要任何窗间通信机制。
//!
//! 窗内结构：自绘标题栏（同主窗三段式，末段作「词库管理」）+ 左栏词库类型 + 右侧沿用
//! 词库页原有的子标签/工具栏/表格/分页 + 全部浮层对话框。
//!
//! ## 三条约束（违反都是静默失败，不会编译报错）
//!
//! 1. **开窗只能在带 `EventCtx` 且副作用被采纳的时机**。`Tree::call_on_update` 只把
//!    `toast` 捞出来，整个 `EventOutcome`（含 `open_windows`）随后丢弃 —— 响应式哨兵里
//!    `ctx.open_window` **永不生效**，与 `add_word_window` 头注释记的 `request_close`
//!    是同一个坑。可用的时机是控件事件回调、`App::channel` 的 `on_message`、
//!    `on_interval`，这三者都走 `apply_dispatch_effects`。
//! 2. **`content` 闭包里创建的 `Signal` 归本窗所有，关窗即回收**。故闭包内新建的信号
//!    绝不能存回长命的 [`DictManagerState`]（它活在 `AppState` 里），否则关窗后就是一个
//!    悬垂句柄，下次开窗读到的是别人的槽位。窗内只读写既有信号。
//! 3. **本窗独立于主窗存活**：主窗关掉后它照常工作（写操作全是同步 RPC），此时由它
//!    接手排空 App 级通道，使「再点一次设置」还能开出主窗。见 [`open`] 与
//!    [`crate::state::AppState::main_window_closed`]。

use std::cell::RefCell;
use std::rc::Rc;

use windui::core::EventCtx;
use windui::prelude::*;

use crate::pages::dict::sidebar::build_sidebar;
use crate::pages::dict::state::DictManagerState;
use crate::pages::dict::{build_dict_dialogs, view::build_page_for};
use crate::state::AppState;

/// 单例键。同键窗口同时只存在一个：再点一次侧栏不会开出第二个，已开着的那个被激活到
/// 前台（最小化的会还原）。键随窗口登记表自动释放，`request_close` 这类绕过关闭决策链
/// 的路径也不会漏复位 —— 故**不要**再另设一个「开着没有」的信号去挡重复开窗。
const SINGLE_KEY: &str = "dict_manager";

/// 默认尺寸。1100 宽足以放下 200 的左栏 + 表格全部列不挤，720 高约 15 行数据；
/// 最小尺寸压到 860×560 仍能完整显示子标签行与工具栏（再窄导入/导出按钮会换行）。
const WIN_W: i32 = 1100;
const WIN_H: i32 = 720;
const MIN_W: i32 = 860;
const MIN_H: i32 = 560;

/// 打开词库管理窗。已经开着时由 `single` 激活既有窗口，本次请求被丢弃。
///
/// 无论开新窗还是激活既有窗都会触发一次 [`DictManagerState::reload`]：`dict_mgr` 活在
/// `AppState` 里、比窗口长命，重开窗会原样显示上次留下的 `table_rows`，而这中间 core
/// 侧完全可能已经变了（用户在别处加过词、导入过词库）—— 不刷新就是一屏看起来权威的
/// 陈旧数据。
///
/// **本窗不随主窗关闭而关闭**，是有意的：它的写操作全是同步 RPC（`call_core` 自己起
/// 线程 + 超时等待），一条都不依赖主窗；core 也还在后台跑着。主窗只是「设置界面」，
/// 关掉它没有理由连带关掉一个正在用的词库管理器。
///
/// 这条能成立的前提是 windui 把 `App::channel` 的通道挂到了 App 级：主窗关闭后通道仍
/// 活着，由还在出帧的窗口（也就是本窗）接手排空，「再点一次设置」才能重新开出主窗。
pub fn open(ctx: &mut EventCtx, app: &Rc<RefCell<AppState>>) {
    let mgr = app.borrow().dict_mgr.clone();
    mgr.reload();

    ctx.open_window(
        Window::new("清风输入法 · 词库管理", WIN_W, WIN_H)
            .single(SINGLE_KEY)
            .frameless(true)
            .centered(true)
            .min_size(MIN_W, MIN_H)
            .content(move || build_content(mgr)),
    );
}

/// 窗内容：标题栏 + 主体 + 全部浮层对话框。
///
/// 对话框挂在**本窗**的根 stack 上，不再是主窗那份（`shell::build` 里已摘掉）。挂错窗
/// 的症状是「点删除没有任何反应」—— 遮罩确实弹了，弹在另一个窗口里。
fn build_content(mgr: DictManagerState) -> Element {
    // 竖分隔线与主窗 sidebar/content 之间那条同源（`Role::Divider`），两窗并排时对得上。
    let vdivider = Element::leaf()
        .width(1)
        .height_match()
        .bg_role(Role::Divider);

    let body = Element::row()
        .fill()
        .weight(1.0)
        .child(build_sidebar(&mgr))
        .child(vdivider)
        // 右侧沿用词库页原有的子标签/工具栏/表格/分页。显式 `weight(1.0)` 而不只靠
        // `fill()`：在 row 里没有权重的话它按内容宽度收缩，表格会缩成窄窄一条。
        .child(build_page_for(mgr.clone()).weight(1.0));

    let main_col = Element::col()
        .fill()
        .child(crate::shell::build_titlebar("词库管理"))
        .child(Element::divider())
        .child(body);

    Element::stack()
        .fill()
        .bg_role(Role::Bg)
        .child(main_col)
        .child(build_dict_dialogs(&mgr))
}
