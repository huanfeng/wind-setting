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
use crate::pages::dict::{build_dict_dialogs, view::build_page_for};
use crate::state::AppState;

/// 窗口标题。`App` 主窗承载词库时（`--page dict` 冷启动）也用这个，两条路同名。
pub const TITLE: &str = "清风输入法 · 词库管理";

/// 单例键。同键窗口同时只存在一个：再点一次侧栏不会开出第二个，已开着的那个被激活到
/// 前台（最小化的会还原）。键随窗口登记表自动释放，`request_close` 这类绕过关闭决策链
/// 的路径也不会漏复位 —— 故**不要**再另设一个「开着没有」的信号去挡重复开窗。
///
/// `pub(crate)`：`--page dict` 冷启动时词库由 `App` 那个窗口承载，那一趟得拿本键去登记
/// （见 `main::main_window_single_key`），否则它是个匿名窗口，此后「词库管理」再开一次
/// 就会得到第二个词库窗。
pub(crate) const SINGLE_KEY: &str = "dict_manager";

// 尺寸与设置窗一致，共用 `crate::MAIN_*` 那一份常量（见其说明）。

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
    app.borrow().dict_mgr.reload();
    let app_for_content = app.clone();

    ctx.open_window(
        Window::new(TITLE, crate::MAIN_W, crate::MAIN_H)
            .single(SINGLE_KEY)
            .frameless(true)
            .centered(true)
            .min_size(crate::MAIN_MIN_W, crate::MAIN_MIN_H)
            .content(move || build_content(app_for_content)),
    );
}

/// 窗内容：标题栏 + 主体 + 全部浮层对话框。
///
/// 对话框挂在**本窗**的根 stack 上，不再是主窗那份（`shell::build` 里已摘掉）。挂错窗
/// 的症状是「点删除没有任何反应」—— 遮罩确实弹了，弹在另一个窗口里。
pub fn build_content(app: Rc<RefCell<AppState>>) -> Element {
    let mgr = app.borrow().dict_mgr.clone();
    // 竖分隔线与主窗 sidebar/content 之间那条同源（`Role::Divider`），两窗并排时对得上。
    let vdivider = Element::leaf()
        .width(1)
        .height_match()
        .bg_role(Role::Divider);

    // 快速导入右键菜单：挂在左栏空白处，与主窗（`shell::build` 的侧栏与底栏）同一处
    // 语义 —— 右侧表格有自己的行菜单，整片挂上去两者会打架。
    //
    // 这个入口在词库窗里比在设置窗里更用得上：统一导入除了配置片段，还吃短语分发文本
    // （`wind:p1`，见 `phrase_import`），而那正是本窗第一栏的东西 —— 别人发来一条短语，
    // 右键就能装上，不必先自己绕回设置窗。
    //
    // 但**界面在主窗**：菜单项走 `import_menu_items_via_main_window`，先把设置窗拉到
    // 前台再弹导入框。本窗不挂那份对话框 —— `visible` 是同一个 `Signal`，两棵树各画
    // 一遍就是两个窗口同时弹同一个框（见该函数说明）。
    let app_menu = app.clone();
    let sidebar = build_sidebar(&mgr, build_back_button(app.clone())).on_context_menu(move || {
        crate::pages::config_import::import_menu_items_via_main_window(&app_menu)
    });

    let body = Element::row()
        .fill()
        .weight(1.0)
        .child(sidebar)
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

/// 左栏底栏的「返回设置」按钮（与「刷新」并排）。
///
/// 一个动作覆盖两种情形，用户不必先判断设置窗还在不在：开着就把它激活到前台，关了就
/// 重建一个，随后关掉本窗。判断在 [`crate::open_main_window`] 内部由单例键做掉——
/// 此处刻意**不**再查 `main_window_closed`：主窗开着时也要调，否则它只是被本窗关闭后
/// "露"出来，若它当时是最小化的，用户点了「← 设置」会看着词库窗消失、什么都没出现。
///
/// 两件事在**同一次点击**里排队，顺序是安全的：`ctx.open_window` 与 `ctx.request_close`
/// 都经宿主延后执行，而平台先建窗、后关窗（win32 `apply_window_op` 那句注释写着
/// 「在可能的关窗之前执行」）。故本窗即使是当前唯一的窗口，也不会先关掉导致进程退出、
/// 新窗根本没机会建出来。
fn build_back_button(app: Rc<RefCell<AppState>>) -> Element {
    Element::button("← 设置")
        .small()
        .outline_soft()
        .on_click(move |ctx| {
            crate::open_main_window(ctx, &app);
            ctx.request_close();
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windui::core::Tree;
    use windui::event::{MouseButton, PointerEvent, PointerKind};
    use windui::geometry::{Point, Size};

    const W: i32 = 800;
    const H: i32 = 600;

    fn build_tree(app: &Rc<RefCell<AppState>>) -> Tree {
        let mut tree = Tree::new();
        let root = build_content(app.clone())
            .width(W)
            .height(H)
            .build(&mut tree);
        tree.root = Some(root);
        tree.layout_root(Size::new(W, H), &mut windui::text::NullTextEngine);
        tree
    }

    /// 接线守护：在**真实词库窗**的左栏空白处右击，快速导入菜单要真的弹出来。
    ///
    /// 只测菜单内容（`config_import` 里那两条）挡不住本仓最容易犯的错：菜单忘了接、
    /// 或接到了右侧表格上（那里已有行菜单，两者会打架）—— 那些测试照样全绿。
    #[test]
    fn right_clicking_the_sidebar_opens_the_import_menu() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let app = Rc::new(RefCell::new(AppState::empty()));
        let mut tree = build_tree(&app);

        // x=100 在左栏内（宽 200），y=300 已在条目列表下方的空白处。
        let (mut hover, mut capture) = (None, None);
        let res = tree.dispatch_pointer(
            PointerEvent::single(PointerKind::Down, Point::new(100, 300), MouseButton::Right),
            &mut hover,
            &mut capture,
        );
        let menu = res.menu.expect("右击左栏空白应弹出快速导入菜单");
        let labels: Vec<String> = menu
            .items
            .iter()
            .filter(|i| !i.separator)
            .map(|i| i.label.clone())
            .collect();
        assert_eq!(
            labels,
            vec!["从剪贴板导入配置", "导入文件…"],
            "词库窗与主窗必须是同一份菜单（`import_menu_items`），实得 {labels:?}"
        );
    }

    /// 导入的两个对话框**不**在本窗挂载：它们的 `visible` 是跨窗共享的 `Signal`，本窗
    /// 若也挂一份，用户在这儿点一次导入，主窗那边会同时弹出同一个框。
    ///
    /// 反向断言：把两个 `visible` 都置位，本窗的树也不该因此多出可见的遮罩层——用根
    /// stack 的挂载件数守着（主体 + 词库自己的对话框组，就这两件）。
    ///
    /// 界面在主窗、词库窗只做转交，见 `config_import::import_menu_items_via_main_window`。
    #[test]
    fn import_dialogs_stay_in_the_main_window() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let app = Rc::new(RefCell::new(AppState::empty()));
        let (import, schema_mgr) = {
            let st = app.borrow();
            (st.config_import.clone(), st.schema_mgr.clone())
        };
        import.visible.set(true);
        schema_mgr.import_confirm_visible.set(true);

        let tree = build_tree(&app);

        let root = tree.root.unwrap();
        assert_eq!(
            tree.get(root).unwrap().children.len(),
            2,
            "词库窗根 stack 只该有主体与词库自己的对话框组；多出来的那件多半是又把导入\
             对话框挂回本窗了，症状是两个窗一起弹同一个框"
        );
    }
}
