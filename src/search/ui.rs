//! 搜索框与结果浮层。
//!
//! 布局上是两块分开的东西：
//! - **搜索框**挂在侧栏导航之上（`shell::build` 的 sidebar 内）；
//! - **结果浮层**挂在**根 stack**，靠固定内边距定位到搜索框正下方。
//!
//! 浮层不挂在侧栏里，是因为它要盖到右边的内容区上；侧栏是固定 184 宽的列，
//! 浮层放进去就只能有 184 宽，面包屑会挤成两行。
//!
//! ## 键盘为什么全都挂在输入框上
//!
//! `Tree::dispatch_key` 只把按键发给焦点节点、**不冒泡**（未消费也不上溯）。
//! 焦点在输入框上时，结果列表根本收不到 ↑↓。所以选择项、回车确认全部走
//! `on_nav_key` / `on_submit`——这两个回调的存在正是为了「查询框 + 结果列表」。
//!
//! Esc 是个例外：TextInput 不处理它，未消费的 Escape 会被宿主当成**关窗兜底**。
//! 所以 Esc 收浮层必须走 `App::on_shortcut`（见 `main.rs`），否则搜索时按 Esc
//! 会把整个设置窗关掉。

use std::rc::Rc;

use windui::core::{EventCtx, Widget};
use windui::event::{Event, PointerKind};
use windui::prelude::*;
use windui::style::Shadow;

use super::index::{JumpTarget, SearchEntry, build_index};
use super::jump::{self, JumpRequest};
use super::matcher::{DEFAULT_LIMIT, search};
use crate::manifest::Manifest;

/// 搜索框尺寸（侧栏宽 − padding_xy(8) 两侧）。
///
/// ⚠ **必须从 [`SIDEBAR_W`] 推出来，不能再写字面量**。此前这里硬编码 168（配 184 的侧栏），
/// 而下面那条守门断言比的是 `BOX_W == SIDEBAR_W - OVERLAY_LEFT * 2` —— 两个常量都在本
/// 文件里，一起对得上就算过，`shell` 那边把侧栏改窄了它一无所知。二级导航把侧栏压到
/// 140 时，搜索框仍按 168 排，右端被侧栏裁掉一截。
const BOX_W: i32 = SIDEBAR_W - OVERLAY_LEFT * 2;
const BOX_H: i32 = 30;

/// 浮层左上角（根坐标）。都是常量：标题栏 44 + 分隔线 1 + 侧栏上内边距 10
/// + 搜索框 30 + 间隙 4 = 89；左侧对齐侧栏内边距 8。
///
/// 用固定值而不是测量搜索框位置，是因为这几个量都由 `shell::build` 写死，
/// 测量反而要引入一条「浮层依赖别的节点几何」的耦合。改了侧栏内边距或标题栏高度
/// 时，`overlay_top_matches_layout` 会提醒同步。
const OVERLAY_LEFT: i32 = 8;
/// 一级导航栏宽度。捕获层要在这块之上给搜索框让路。
///
/// **直接引用 `shell` 那份**，不再抄一个字面量：搜索框宽度、捕获层让位宽度都由它推出来，
/// 抄一份的下场见 [`BOX_W`]。
const SIDEBAR_W: i32 = crate::shell::NAV_W;
const OVERLAY_TOP: i32 = 89;
const OVERLAY_W: i32 = 320;
const OVERLAY_MAX_H: i32 = 380;

/// 结果列表里的一行。
#[derive(Clone)]
pub struct ResultRow {
    /// 在当前结果里的序号，用于和 `selected` 比对。
    pub idx: usize,
    pub title: String,
    /// 面包屑，已拼成一行。
    pub path: String,
    /// 方案级条目：到不了，只能给指示。列表上标一下，免得点了觉得「没反应」。
    pub needs_schema: bool,
    pub target: JumpTarget,
}

/// 搜索状态。信号在 `AppState::empty()` 里创建（与 `tab`/`dark` 同级），
/// 故不属于任何窗口的 `SignalScope`，主窗关掉再开也不会失效。
#[derive(Clone)]
pub struct SearchState {
    /// 索引：启动构建一次，之后只读。
    entries: Rc<Vec<SearchEntry>>,
    /// 输入框绑定的查询串。
    pub query: Signal<String>,
    /// 当前结果（浮层的数据源）。
    pub rows: Signal<Vec<ResultRow>>,
    /// 高亮项下标。有结果时恒为合法值（默认 0），故 Enter 总有东西可跳。
    pub selected: Signal<usize>,
    /// 浮层是否展开。
    pub open: Signal<bool>,
    /// 方案级指示条的文案（空 = 不显示）。见 [`crate::search::hint`]。
    pub hint: Signal<String>,
    /// 「请求打开词库窗」的计数器。
    ///
    /// 词库是独立窗，开窗要走 `AppState::request_dict_window`，而那需要 `AppState`
    /// ——它反过来持有本结构，直接存一份 `Rc<RefCell<AppState>>` 就成了循环引用。
    /// 故这里只放一个计数信号，由 `shell` 侧的哨兵监听它去开窗。
    pub dict_request: Signal<u64>,
}

impl SearchState {
    pub fn new(manifest: &Manifest) -> Self {
        Self {
            entries: Rc::new(build_index(manifest)),
            query: signal(String::new()),
            rows: signal(Vec::new()),
            selected: signal(0),
            open: signal(false),
            hint: signal(String::new()),
            dict_request: signal(0),
        }
    }

    /// 按当前 `query` 重算结果。查询为空即收起浮层。
    pub fn refresh(&self) {
        let q = self.query.get();
        let hits = search(&self.entries, &q, DEFAULT_LIMIT);
        let rows: Vec<ResultRow> = hits
            .iter()
            .enumerate()
            .map(|(idx, h)| {
                let e = &self.entries[h.index];
                ResultRow {
                    idx,
                    title: e.title.clone(),
                    path: e.path.join(" › "),
                    needs_schema: matches!(e.target, JumpTarget::SchemaLevel { .. }),
                    target: e.target.clone(),
                }
            })
            .collect();
        self.rows.set(rows);
        self.selected.set(0);
        // 查询非空就展开——即使没有结果：浮层要显示「未找到匹配项」，
        // 直接收起会让人以为搜索框坏了。
        self.open.set(!q.trim().is_empty());
    }

    /// 收起浮层（不清查询：用户可能还想回来点第二条）。
    pub fn dismiss(&self) {
        self.open.set(false);
    }

    /// 用户回到搜索框时，把收起过的浮层重新亮出来。**有查询词才弹**（空查询本就不该有浮层）。
    ///
    /// 三条路共用一个判据：Ctrl+F（[`crate::shortcuts::dispatch`]）、焦点从别处回来
    /// （[`FocusWatcher`]）、点击输入框（[`build_search_box`] 的 on_click）。
    /// 各写各的就迟早会分叉成「这条路弹、那条路不弹」。
    ///
    /// 返回**是否真的展开了**：调用方据此决定要不要请求重排，本来就开着时别白跑一趟。
    pub fn reopen(&self) -> bool {
        if self.open.get() || self.query.get().trim().is_empty() {
            return false;
        }
        self.open.set(true);
        true
    }

    /// 清空并收起。
    pub fn clear(&self) {
        self.query.set(String::new());
        self.rows.set(Vec::new());
        self.selected.set(0);
        self.open.set(false);
    }

    /// 上下移动高亮项，**在首尾之间循环**。
    fn move_selection(&self, delta: isize) {
        let n = self.rows.with(Vec::len);
        if n == 0 {
            return;
        }
        let cur = self.selected.get() as isize;
        let next = (cur + delta).rem_euclid(n as isize) as usize;
        self.selected.set(next);
    }

    /// 当前高亮项的跳转目标。
    fn selected_target(&self) -> Option<JumpTarget> {
        let i = self.selected.get();
        self.rows.with(|r| r.get(i).map(|row| row.target.clone()))
    }
}

/// 点击/回车确认一条结果：切页 + 挂跳转请求 + 收浮层。
fn commit(st: &SearchState, tab: Signal<usize>, jmp: Signal<Option<JumpRequest>>, t: JumpTarget) {
    let opens_dict = matches!(t, JumpTarget::DictWindow);
    // 上一条方案级指示到此为止。它说的是「去某个方案里找这一项」，而这次跳转已经把
    // 用户送到别处了；不清的话它会一直挂在内容区顶部——**切页也不消失**，那正是它
    // 常驻的位置语义（同 conflict_bar），于是在关于页上继续指挥用户「选中下面某个
    // 方案」，而那一页根本没有方案列表。
    //
    // 方案级目标不在此列：它自己会在 jump sink 里写上新文案，先清一次是白推版本。
    if !matches!(t, JumpTarget::SchemaLevel { .. }) && !st.hint.get().is_empty() {
        st.hint.set(String::new());
    }
    jump::request(tab, jmp, t);
    if opens_dict {
        // 交给 shell 侧的哨兵去开窗（见 `SearchState::dict_request`）。
        st.dict_request.update(|n| *n += 1);
    }
    st.dismiss();
}

/// 「请求开词库窗」的哨兵（0×0）：计数一变就开窗。
///
/// 挂在 shell 里，因为只有那儿同时拿得到 `AppState` 与元素树。
pub fn build_dict_request_watcher(
    st: &SearchState,
    app: &std::rc::Rc<std::cell::RefCell<crate::state::AppState>>,
) -> Element {
    let req = st.dict_request;
    let app = app.clone();
    crate::pages::dict::reactive::reactive_effect(
        move || req.version(),
        move || app.borrow().request_dict_window(true),
    )
}

/// 跳转副作用汇（0×0）：锚点管不了的那两件事在这里做。
///
/// - **开对话框**：第一跳完成后，把目标所在的框打开。锚点只知道自己是谁、不知道
///   注册表，而「开哪个框」是请求的属性——两边各管一段，谁都不必知道对方。
/// - **方案级指示**：这类目标没有锚点可落（`pending_anchor` 返回 `None`），
///   请求会一直挂着；在此消费掉并写出指示条文案。
///
/// 幂等：对话框信号已是 true 时不再写（`Signal::set` 会推高版本、白白触发一轮重绘）。
pub fn build_jump_sink(
    jmp: Signal<Option<JumpRequest>>,
    hint: Signal<String>,
    fields: std::rc::Rc<crate::state::FieldRegistry>,
) -> Element {
    crate::pages::dict::reactive::reactive_effect(
        move || jmp.version(),
        move || {
            let Some(req) = jmp.get() else { return };
            match &req.target {
                JumpTarget::SchemaLevel { hint_path } => {
                    hint.set(crate::search::hint::hint_text(hint_path));
                    jmp.set(None); // 消费掉：没有锚点会来认领它
                }
                JumpTarget::RowInDialog { dialog, .. } if req.dialog_opened => {
                    let opens = fields.dialog_opens.borrow();
                    if let Some(vis) = opens.get(dialog.as_str())
                        && !vis.get()
                    {
                        vis.set(true);
                    }
                }
                JumpTarget::DictWindow => {
                    // 同 SchemaLevel：没有锚点会来认领它（`pending_anchor` 返回 None），
                    // 开窗是 `dict_request` 哨兵的活。不在这儿消费掉的话，`jump` 信号会
                    // **永久**停在 Some——锚点每帧的快速返回路径（"jump 为 None 就走人"）
                    // 从此失效，`armed` 的复位通道也跟着少一条。
                    jmp.set(None);
                }
                _ => {}
            }
        },
    )
}

/// 侧栏顶部的搜索框。
pub fn build_search_box(
    st: &SearchState,
    tab: Signal<usize>,
    jmp: Signal<Option<JumpRequest>>,
) -> Element {
    let query = st.query;
    // ↑↓ 选择、Enter 确认。两个闭包各持一份 SearchState 克隆（内部都是 Copy 的信号
    // 与一个 Rc，克隆很轻）。
    let nav_st = st.clone();
    let submit_st = st.clone();
    let clear_st = st.clone();
    let click_st = st.clone();

    let input = Element::text_input(query, "搜索设置")
        // 前置图标区留白，图标本身由下面的 SVG 叠上去（见 `icon` 的说明）。
        .leading_icon(' ')
        .width(BOX_W)
        .height(BOX_H)
        // Ctrl+F 的落点：`ShortcutCtx::focus_main_input()` 把焦点交还给声明了
        // autofocus 的控件，全窗只此一个。副作用是打开设置窗即聚焦搜索框
        // （与 Windows 11 设置一致）。
        .autofocus()
        .on_nav_key(move |_ctx, ev| match ev.key {
            Key::Up => {
                nav_st.move_selection(-1);
                true
            }
            Key::Down => {
                nav_st.move_selection(1);
                true
            }
            // 其余（尤其 Tab）必须交回去：无脑返回 true 会吃掉 Shift+Tab，
            // 用户就失去了离开输入框的唯一键盘途径。
            _ => false,
        })
        .on_submit(move |_ctx| {
            if let Some(t) = submit_st.selected_target() {
                commit(&submit_st, tab, jmp, t);
            }
        })
        // 点框即把收起过的浮层唤回来。
        //
        // 这条路 `FocusWatcher` 接不住：按 Escape 收起浮层时**焦点根本没离开输入框**，
        // 于是没有 false→true 的跳变可看，用户再点几下都没反应——只能再敲一个字。
        // 点击是这一路唯一的信号。
        //
        // （windui 的 TextInput 直到 0.14.0 之后才实现 `take_click`；在那之前这句
        // 链上去是静默无效的，本仓因此一直缺这块。）
        .on_click(move |_ctx| {
            click_st.reopen();
        });

    // 清除按钮：只在有输入时出现，压在输入框右端。
    //
    // ⚠ `.clickable()` 只能加在容器（col/row/stack）上，叠到 label 这类叶子控件上会在
    // debug 构建下**运行时** panic（链式 API 是 downcast，编译与单测都拦不住）。
    // 故这里先用 row 包一层，再让容器可点。
    let clear = Element::row()
        .padding_xy(6, 6)
        .child(
            Element::label("✕")
                .font_size(12.0)
                .fg_role(Role::TextDisabled),
        )
        .clickable()
        .corner(4.0)
        .on_click(move |_| clear_st.clear())
        // ⚠ 同前置图标那处：`align` 在 stack 里**一个值同时管两轴**，只写 End
        // 会把按钮压到右**下**角。要「贴右 + 垂直居中」必须分开给值。
        .align_xy(Align::End, Align::Center)
        .margin_xy(4, 0)
        .visible_when(move || !query.get().is_empty());

    // 放大镜用 SVG 而不是 `leading_icon` 的字形：
    // - 🔍(U+1F50D) 是 emoji，Windows 按**彩色**字形渲染，`draw_text` 的颜色对它无效，
    //   与整体单色风格打架（用户反馈的正是这个）；
    // - U+2315 虽是单色，但字形细弱、与侧栏那套 2px 线性图标不是一路。
    // 故 `leading_icon(' ')` 只借它留出左侧图标区（宽度 = 字号 + 8），真正的图标
    // 用与导航同一套 SVG 管线画上去，颜色随主题走 placeholder。
    //
    // ⚠ `tint` 只吃具体颜色、没有语义色版本，而颜色是**构建时**取的——换肤后不会跟着变。
    // 故按本仓既有做法（见 `widgets` 里那些 `dark_sig()` 双态块）备好亮暗两枚，
    // 由 `visible_when` 切换。
    let icon_for = |on_dark: bool| {
        let pal = crate::theme_def::theme_for(on_dark).palette;
        crate::icons::tinted(crate::icons::SEARCH, 14, pal.placeholder)
            // ⚠ `align` 在 stack 里**一个值同时管两轴**，只写 Start 会把图标顶到左上角。
            // 要「贴左 + 垂直居中」必须分开给值。
            .align_xy(Align::Start, Align::Center)
            .margin_xy(9, 0)
            .visible_when(move || crate::widgets::dark_sig().get() == on_dark)
    };

    // ⚠ 子节点的**顺序有约定**：`FocusWatcher` 靠「父节点的第一个子就是输入框」
    // 找到它读焦点态（见该 widget 的说明）。调整顺序前先看那里。
    Element::stack()
        .width(BOX_W)
        .height(BOX_H)
        .child(input)
        .child(icon_for(false))
        .child(icon_for(true))
        .child(clear)
        .child(focus_watcher(st))
}

/// 焦点哨兵（0×0）：搜索框**重新获得焦点**时，把收起过的浮层再弹出来。
///
/// 没有它的话，用户按 Esc 或点别处收起浮层后回到搜索框，必须再敲一个字才会重新出结果
/// ——而他心里那次搜索并没有结束。
///
/// windui 没有焦点变化事件，但 `Node::focused` 是公开字段，故每帧读一次即可。
/// 这就是为什么本哨兵必须和输入框同一个父节点：它靠父的**首个子节点**定位输入框。
fn focus_watcher(st: &SearchState) -> Element {
    Element::leaf()
        .widget(FocusWatcher {
            st: st.clone(),
            was_focused: false,
        })
        .reactive()
        .size(0, 0)
}

struct FocusWatcher {
    st: SearchState,
    /// 上一帧输入框是否持有焦点。只在 false→true 那一刻动作，避免每帧重复展开
    /// （用户主动收起后，只要焦点还在框里就不该被弹回来）。
    was_focused: bool,
}

impl Widget for FocusWatcher {
    fn on_update(&mut self, ctx: &mut EventCtx) {
        let id = ctx.id();
        let tree = ctx.tree_mut();
        // 父的首个子 = 输入框（顺序约定见 `build_search_box`）。
        let focused = tree
            .get(id)
            .and_then(|n| n.parent)
            .and_then(|p| tree.get(p))
            .and_then(|p| p.children.first().copied())
            .and_then(|input| tree.get(input))
            .map(|n| n.focused)
            .unwrap_or(false);

        // 焦点从别处回到框里（Tab 导航等）：把结果重新亮出来。
        // 焦点始终在框内的那一路由输入框的 on_click 接（见 `build_search_box`）。
        if focused && !self.was_focused && self.st.reopen() {
            // on_update 相位的 ctx 副作用会被丢弃，故走全局请求（同 anchor.rs）。
            windui::anim::request_relayout();
        }
        self.was_focused = focused;
    }
}

/// 查询变化即重算结果的响应式哨兵（0×0，挂在根 stack 上）。
///
/// **不能挂侧栏**：那个 col 有 `spacing`，塞 0×0 节点进去会把搜索框与导航的间距
/// 加好几遍（见 `shell::build` 里 sidebar 处的说明）。
///
/// windui 的 TextInput 只写回绑定的信号、没有变更回调，故沿用本仓既有的
/// `reactive_effect` 模式（见 `pages::dict::reactive`）。
pub fn build_query_watcher(st: &SearchState) -> Element {
    let query = st.query;
    let st = st.clone();
    crate::pages::dict::reactive::reactive_effect(move || query.version(), move || st.refresh())
}

/// 结果浮层（挂根 stack）。返回 (点击外部的捕获层, 浮层本体)，两者都要挂，
/// **捕获层在前**：它得在浮层之下，否则点浮层里的项会先被它吃掉。
pub fn build_results_overlay(
    st: &SearchState,
    tab: Signal<usize>,
    jmp: Signal<Option<JumpRequest>>,
) -> (Element, Element) {
    let open = st.open;
    // 点击浮层以外任何地方即收起。用自绘的透明层而不是 `.clickable()`：后者会画
    // hover/press 半透明叠层，糊在全窗上就是整屏发灰。
    //
    // ⚠ **搜索框那一块要挖空**。捕获层挂在根 stack 的末尾（比主体更靠上层），铺满全窗
    // 就连搜索框一起盖住了：浮层开着时点搜索框，点击被捕获层吃掉——浮层收起、
    // 光标却没定位到点的位置。而 `hit_opaque` 拿不到坐标，没法「按位置决定吃不吃」，
    // 故只能从几何上让开：顶部那一行的左侧留出侧栏宽度不铺。
    let block = |st: &SearchState| {
        let st = st.clone();
        Element::leaf().widget(Scrim::new(move || st.dismiss()))
    };
    let scrim = Element::col()
        .fill()
        .child(
            Element::row()
                .width_match()
                .height(OVERLAY_TOP)
                // 洞：纯占位、无 widget 也无背景，故不吞命中（见 `hit_node` 的判据），
                // 点击径直落到下面的搜索框上。
                .child(Element::leaf().width(SIDEBAR_W).height_match())
                .child(block(st).weight(1.0).height_match()),
        )
        .child(block(st).fill().weight(1.0))
        .visible_when(move || open.get());

    let rows = st.rows;
    let selected = st.selected;
    let row_st = st.clone();
    let list = Element::host_signal(rows, move |row: ResultRow| {
        build_result_row(row, selected, &row_st, tab, jmp)
    });

    let empty_hint = Element::label("未找到匹配项")
        .font_size(12.0)
        .fg_role(Role::TextDisabled)
        .padding_xy(12, 14)
        .visible_when(move || rows.with(Vec::is_empty));

    let panel = Element::col()
        .width(OVERLAY_W)
        .bg_role(Role::Surface)
        .corner(8.0)
        .border_role(Role::Border, 1)
        .shadow(Shadow::new(0.0, 8.0, 24.0, Color::rgba(0, 0, 0, 110)))
        .padding_xy(4, 4)
        .child(empty_hint)
        // ⚠ 限高必须加在 **scroll 自己**身上，不能只加在外层 col 上：
        // `measure_scroll` 返回的是**完整内容高**（同时把它记进 content_h），而
        // `arrange_linear` 按子节点的 measured 高度排布——外层 col 被 max_height 截短了，
        // 里面这个 scroll 仍按完整高度排，于是内容整片溢出到浮层外面，也不会滚动。
        .child(
            Element::scroll()
                .width_match()
                .max_height(OVERLAY_MAX_H)
                .child(list),
        );

    // 定位层：铺满根 stack、只靠内边距把面板推到搜索框正下方。自身无背景、
    // 用的是纯容器 widget，故不吞指针事件（`hit_node` 的「吞命中」判据）。
    let overlay = Element::col()
        .fill()
        .padding_edges(Insets::new(OVERLAY_LEFT, OVERLAY_TOP, 0, 0))
        .child(panel)
        .visible_when(move || open.get());

    (scrim, overlay)
}

/// 一条结果：高亮底（选中时）+ 两行文字，整行可点。
fn build_result_row(
    row: ResultRow,
    selected: Signal<usize>,
    st: &SearchState,
    tab: Signal<usize>,
    jmp: Signal<Option<JumpRequest>>,
) -> Element {
    let i = row.idx;
    // ⚠ 撑满整行靠 `Align::Stretch`，不是 `.fill()`：`arrange_frame` 判的是
    // align == Stretch 才用父的可用尺寸，`Dimension::Match` 在这里会测成 0
    // ——那样高亮就是一条看不见的 0 高线。
    let highlight = Element::leaf()
        .corner(6.0)
        .bg_role_alpha(Role::Accent, 0.12)
        .align(Align::Stretch)
        .visible_when(move || selected.get() == i);

    let mut title_row = Element::row()
        .width_match()
        .spacing(6)
        .cross(Align::Center)
        .child(
            Element::label(row.title.clone())
                .font_size(13.0)
                .font_weight(500)
                .max_lines(1),
        );
    if row.needs_schema {
        // 方案级条目够不着：点了只会跳到方案页并给一条路径提示。先说清楚，
        // 免得用户以为点坏了。
        title_row = title_row.child(
            Element::label("需先选方案")
                .font_size(10.0)
                // 用 accent 而不是 TextDisabled：这枚标记压在选中行的淡 accent 底上，
                // 灰字与底几乎糊在一起（浅色主题下尤其），等于没标。
                .fg_role(Role::Accent)
                .bg_role_alpha(Role::Accent, 0.14)
                .corner(4.0)
                .padding_xy(5, 1)
                .max_lines(1),
        );
    }

    let content = Element::col()
        .width_match()
        .padding_xy(10, 6)
        .spacing(2)
        .child(title_row)
        .child(
            Element::label(row.path.clone())
                .font_size(11.0)
                .fg_role(Role::TextDisabled)
                .max_lines(1),
        )
        .align(Align::Stretch)
        .clickable()
        .corner(6.0);

    let click_st = st.clone();
    let target = row.target.clone();
    let content = content.on_click(move |_| {
        commit(&click_st, tab, jmp, target.clone());
    });

    Element::stack()
        .width_match()
        .child(highlight)
        .child(content)
}

/// 透明的全窗捕获层：吃掉一次点击并回调，自己什么都不画。
struct Scrim {
    on_dismiss: Box<dyn Fn()>,
}

impl Scrim {
    fn new(f: impl Fn() + 'static) -> Self {
        Self {
            on_dismiss: Box::new(f),
        }
    }
}

impl Widget for Scrim {
    fn on_event(&mut self, ctx: &mut EventCtx, ev: &Event) -> bool {
        match ev {
            Event::Pointer(p) if p.kind == PointerKind::Down => {
                (self.on_dismiss)();
                // 收起会改可见性 → 影响自身矩形之外的区域，故整窗失效。
                ctx.mark_dirty_all();
                true
            }
            // 其余指针事件一并吞掉：漏下去会点到浮层背后的控件上。
            Event::Pointer(_) => true,
            _ => false,
        }
    }

    /// 命中即落定——没有背景色的节点默认是穿透的（`hit_node` 的判据里
    /// `style.bg.is_some()` 与 `hit_opaque()` 并列），不这样它就形同虚设。
    fn hit_opaque(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> SearchState {
        SearchState::new(&Manifest::from_embedded())
    }

    #[test]
    fn empty_query_keeps_overlay_closed() {
        let st = state();
        st.query.set(String::new());
        st.refresh();
        assert!(!st.open.get());
        assert!(st.rows.with(Vec::is_empty));
    }

    #[test]
    fn query_opens_overlay_with_rows() {
        let st = state();
        st.query.set("码表".into());
        st.refresh();
        assert!(st.open.get(), "有查询就该展开");
        assert!(!st.rows.with(Vec::is_empty), "「码表」应有结果");
        assert_eq!(st.selected.get(), 0, "刷新后高亮回到第一条");
    }

    /// **回归**：Escape 收起浮层后，再点搜索框应该把它唤回来。
    ///
    /// 这一路 `FocusWatcher` 接不住——按 Escape 时焦点没离开输入框，没有跳变可看。
    /// 修好之前用户只能再敲一个字才能让结果回来。
    #[test]
    fn reopen_brings_back_a_dismissed_overlay() {
        let st = state();
        st.query.set("码表".into());
        st.refresh();
        st.dismiss();
        assert!(!st.open.get(), "前置：浮层已收起");

        assert!(st.reopen(), "有查询词时应重新展开，并报告确实展开了");
        assert!(st.open.get());
    }

    /// 空查询不弹空浮层。
    #[test]
    fn reopen_does_nothing_without_a_query() {
        let st = state();
        assert!(!st.reopen());
        assert!(!st.open.get());

        st.query.set("   ".into()); // 只有空白也算空
        assert!(!st.reopen());
        assert!(!st.open.get());
    }

    /// 已经开着就报 false：调用方据此跳过重排，别白跑一趟。
    #[test]
    fn reopen_reports_false_when_already_open() {
        let st = state();
        st.query.set("码表".into());
        st.refresh();
        assert!(st.open.get(), "前置：浮层开着");
        assert!(!st.reopen(), "已经开着时不该报「我展开了」");
        assert!(st.open.get(), "但也不能把它关掉");
    }

    /// 没有结果也要展开——浮层里显示「未找到匹配项」，收起会让人以为搜索坏了。
    #[test]
    fn no_result_still_opens() {
        let st = state();
        st.query.set("zzzzzzz不存在的东西".into());
        st.refresh();
        assert!(st.open.get());
        assert!(st.rows.with(Vec::is_empty));
    }

    #[test]
    fn selection_wraps_around() {
        let st = state();
        st.query.set("候选".into());
        st.refresh();
        let n = st.rows.with(Vec::len);
        assert!(n >= 2, "这条测试需要至少两条结果");

        st.move_selection(-1);
        assert_eq!(st.selected.get(), n - 1, "从第一条上移应绕到最后一条");
        st.move_selection(1);
        assert_eq!(st.selected.get(), 0, "从最后一条下移应绕回第一条");
    }

    /// 没有结果时按 ↑↓ 不该 panic，也不该把 selected 推到非法值。
    #[test]
    fn selection_move_on_empty_is_noop() {
        let st = state();
        st.refresh();
        st.move_selection(1);
        assert_eq!(st.selected.get(), 0);
    }

    #[test]
    fn clear_resets_everything() {
        let st = state();
        st.query.set("码表".into());
        st.refresh();
        st.clear();
        assert!(st.query.get().is_empty());
        assert!(st.rows.with(Vec::is_empty));
        assert!(!st.open.get());
    }

    /// 确认一条结果后浮层收起、页面切到目标页。
    #[test]
    fn commit_switches_tab_and_closes() {
        let st = state();
        let tab = signal(0usize);
        let jmp = signal(None);
        st.query.set("码表".into());
        st.refresh();
        let target = st.selected_target().expect("应有高亮项");
        let want = target.tab();
        commit(&st, tab, jmp, target);
        assert_eq!(tab.get(), want);
        assert!(!st.open.get(), "确认后浮层应收起");
        assert!(jmp.get().is_some(), "应挂上跳转请求供锚点认领");
    }

    /// 词库条目不切页、改为请求开独立窗。
    ///
    /// 切过去只有一屏空白：`pages::dict_page` 返回的是永不显示的占位叶子，
    /// 词库早已迁进独立窗口。
    #[test]
    fn dict_entry_opens_window_instead_of_switching_tab() {
        let st = state();
        let tab = signal(3usize); // 停在某个非词库页
        let jmp = signal(None);
        let before = st.dict_request.get();

        commit(&st, tab, jmp, JumpTarget::DictWindow);

        assert_eq!(tab.get(), 3, "词库条目不该切页——切过去是空白占位");
        assert_eq!(st.dict_request.get(), before + 1, "应请求开词库窗");
    }

    /// 索引里的词库条目确实都是开窗目标。
    #[test]
    fn dict_index_entries_are_window_targets() {
        let st = state();
        st.query.set("词库".into());
        st.refresh();
        let has_dict_window = st.rows.with(|rows| {
            rows.iter()
                .any(|r| matches!(r.target, JumpTarget::DictWindow))
        });
        assert!(has_dict_window, "搜「词库」应有开窗目标的条目");
    }

    /// 结果行带上「需先选方案」标记的，正是方案级条目。
    #[test]
    fn schema_level_rows_are_flagged() {
        let st = state();
        st.query.set("码表配置".into());
        st.refresh();
        st.rows.with(|rows| {
            for r in rows {
                assert_eq!(
                    r.needs_schema,
                    matches!(r.target, JumpTarget::SchemaLevel { .. }),
                    "「{}」的方案级标记与目标类型不一致",
                    r.title
                );
            }
        });
    }

    /// **建树不 panic**。
    ///
    /// windui 的链式 API 是运行时 downcast：`.clickable()` 落到叶子控件上、
    /// `.small()` 落到非按钮上，都会在 debug 构建下当场 panic，而
    /// `cargo check`、`cargo clippy` 与其余单测**全绿**。本仓已因此崩过一次
    /// （`SettingsSection::ALL` 那份手写列表就是为此存在的）。
    ///
    /// P1 也栽过同一个坑：清除按钮曾是 `label("✕").clickable()`，
    /// 编译测试都过、一跑就崩。故此处把三个构建入口都走一遍。
    #[test]
    fn search_ui_builds_without_panicking() {
        let st = state();
        let tab = signal(0usize);
        let jmp = signal(None);
        let _ = build_search_box(&st, tab, jmp);
        let _ = build_query_watcher(&st);
        let _ = build_results_overlay(&st, tab, jmp);

        // 结果行两种形态都要建：带「需先选方案」标记的分支是另一条路径。
        st.query.set("码表".into());
        st.refresh();
        st.rows.with(|rows| {
            for row in rows {
                let _ = build_result_row(row.clone(), st.selected, &st, tab, jmp);
            }
        });
    }

    /// 浮层的固定定位与 `shell::build` 的实际布局对齐。
    ///
    /// 这几个量分散在两个文件里，靠常量对齐；改了侧栏内边距或标题栏高度而忘了
    /// 同步，浮层就会错位——那种偏移在截图上不明显，但用起来别扭。
    #[test]
    fn overlay_top_matches_layout() {
        const TITLEBAR_H: i32 = 44;
        const DIVIDER_H: i32 = 1;
        const SIDEBAR_PAD_V: i32 = 10;
        const GAP: i32 = 4;
        assert_eq!(
            OVERLAY_TOP,
            TITLEBAR_H + DIVIDER_H + SIDEBAR_PAD_V + BOX_H + GAP
        );
        // 侧栏左右各 8 内边距。两者现在都由 `shell::NAV_W` 推出，这条断言退化成
        // 「没人把它们改回字面量」的守门。
        assert_eq!(SIDEBAR_W, crate::shell::NAV_W, "侧栏宽必须跟着 shell 走");
        assert_eq!(BOX_W, SIDEBAR_W - OVERLAY_LEFT * 2);
    }
}
