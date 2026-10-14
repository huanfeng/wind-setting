//! 二级导航 —— 「这一页有哪些分块，现在正看着哪一块」。
//!
//! 一级导航（左栏 Tab）之外再加一层：把当前页的分区卡片列成一条可点的清单，点击滚到
//! 那张卡，滚动时高亮跟着走。输入页有 15 张卡，光靠滚是找不到东西的。
//!
//! ## 为什么滚动不需要新的原语
//!
//! `ScrollWidget` 收到滚轮后走 `EventCtx::scroll_by` → `mark_layout_dirty()`，宿主据此
//! 置 `needs_relayout`，下一帧 `Tree::layout_root` 开头必跑 `dispatch_reactive_updates`。
//! 也就是说**滚动过程中每一帧都会广播 `on_update`** —— 逐帧读一次各卡片的绝对矩形就够了，
//! 不必给 windui 加滚动事件。
//!
//! ## 三个与 `search::anchor` 同源的坑
//!
//! 1. **`on_update` 不看可见性**。八个页面全建好叠在一个 stack 里靠 `visible_when` 切，
//!    未选中页的哨兵照样每帧被叫到。不自查祖先链的话，它们会拿上一次可见时的陈旧矩形
//!    去覆写高亮——表现为切到 A 页、B 页的哨兵把 A 页的高亮改掉。
//!    反过来，**祖先可见也不代表已经排过版**：`arrange` 只排 `visible_children`，切页
//!    那一帧新页整棵子树还是零矩形。两道门都要（见 `Spy::on_update`）。
//! 2. **`on_update` 跑在 `measure/arrange` 之前**，读到的 bounds 是上一帧的，而节点上的
//!    `scroll_y` 已是当前值。**两者不能混着用**：高亮判定只吃 bounds（滞后一帧无感），
//!    跳转落点也只吃 bounds（取卡片在内容列里的偏移，与 scroll_y 无关）。
//!    混用的后果见 `Spy::on_update` 里那段说明。
//! 3. **`on_update` 相位的 `ctx` 副作用会被丢弃**（见 `Tree::call_on_update`）。要重排
//!    必须走全局的 `anim::request_relayout()`，用 `ctx.mark_layout_dirty()` 那一帧永远不来。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use windui::anim;
use windui::core::{EventCtx, Layout, NodeId, Tree, Widget};
use windui::prelude::*;

/// 分块顶端滚到视口内多深处就算「正在看它」（逻辑 px，相对滚动视口顶端）。
///
/// 必须大于 [`TOP_INSET`]：点击跳转会把目标卡片的顶端停在 `TOP_INSET` 处，判定线若比它
/// 还高，用户点了一项、高亮却停在上一项。
const ACTIVE_LINE: i32 = 40;

/// 点击跳转后目标卡片顶端距视口顶端的距离，取 `widgets::page` 的纵向内边距，
/// 让跳转落点与「把这一页滚到这张卡开头」看起来是同一件事。
const TOP_INSET: i32 = 16;

/// 「已经滚到底」的容差（逻辑 px）。见 [`SubNav::resolve`] 里末项的特判。
const BOTTOM_EPS: i32 = 2;

/// 哨兵尚未上报过位置。
const UNSET: i32 = i32::MIN;

/// 二级导航栏宽度（逻辑 px）。
///
/// 按**曾经最长的那条**定：「主题编辑器直连」7 个汉字，12.5px 字号约 88px，加上左右
/// 各 8 的栏内边距与行内 10 的左内边距、右侧留一点余量，132 刚好装得下且不省略。
/// 那条后来改名为「主题编辑器」，现存最长的是 6 字（「拼音方案配置」那批），宽度不跟着
/// 收：它是**上限**，按 7 字留着，下次再加一条长名字不必重算。
/// 分块名再长就该先想想是不是名字起长了——导航是用来扫的，扫不完等于没分类。
pub const SUBNAV_W: i32 = 132;

/// 导航清单上的一行。每一行都对应页面上一张真卡片、都点得动。
///
/// ★ 曾经还有一种「纯分组标题」行（`section: None`，点不动，只把下面几行缩进归拢），
/// 方案页拿它做过「全局方案配置」那一层。撤掉了：分组标题既不是卡片也点不动，却在
/// 导航里占一行、把真正的目标缩进去，层次凭空多一级而信息量为零
/// （2026-09-09 用户原话：「会让这个层次变的奇怪」）。要说明分组关系，说在卡片里。
#[derive(Clone)]
pub struct SubNavItem {
    pub label: String,
    /// 缩进层级。目前全是 0；留着是因为将来真有嵌套需求时，导航侧只差这一个值。
    pub depth: u8,
    /// 对应的分块下标。
    pub section: usize,
    /// 这一条现在该不该出现在导航里。恒为 true 的用 [`SubNav::wrap`]，
    /// 随运行期状态显隐的用 [`SubNav::wrap_when`]。
    pub visible: Rc<dyn Fn() -> bool>,
}

struct Inner {
    /// 本导航属于哪个 Tab。解析器据此判断「现在轮不轮得到我」。
    tab: usize,
    items: RefCell<Vec<SubNavItem>>,
    /// 各分块顶端相对滚动视口顶端的偏移，下标即分块序号。
    tops: RefCell<Vec<i32>>,
    /// 滚动容器的 `(scroll_y, max_scroll)`。同页各分块共用一个滚动祖先，谁上报都一样。
    metrics: Cell<(i32, i32)>,
    /// 当前高亮的分块序号。
    active: Signal<usize>,
    /// 待认领的点击（分块序号）。同一项连点两次只是用同一个值覆盖，无需版本号区分：
    /// 认领方只关心「是不是找我」，认领即清。
    request: Cell<Option<usize>>,
    /// 点击跳转钉住的高亮：`(分块序号, 落定后的 scroll_y)`。见 [`SubNav::resolve`]。
    pinned: Cell<Option<(usize, i32)>>,
}

/// 一页的二级导航句柄。克隆很轻（一个 `Rc`）。
#[derive(Clone)]
pub struct SubNav(Rc<Inner>);

impl SubNav {
    pub fn new(tab: usize) -> Self {
        Self(Rc::new(Inner {
            tab,
            items: RefCell::new(Vec::new()),
            tops: RefCell::new(Vec::new()),
            metrics: Cell::new((0, 0)),
            active: signal(0usize),
            request: Cell::new(None),
            pinned: Cell::new(None),
        }))
    }

    /// 登记一个分块，并把卡片包上位置哨兵。`label` 是**导航栏上**的名字，可以比卡片
    /// 标题短（方案页导航写「码表」，卡片标题仍是「码表方案配置」）。
    ///
    /// 哨兵是覆盖层而不是兄弟节点：页面那个 col 带 `spacing`，插 0×0 兄弟等于凭空多出
    /// 一档间距（`spacing` 是加在**每对**相邻子节点之间的）。
    pub fn wrap(&self, label: &str, depth: u8, content: Element) -> Element {
        self.wrap_when(label, depth, content, || true)
    }

    /// 同 [`wrap`](Self::wrap)，但这一块**本身会隐藏**时，导航里那一条也跟着不出。
    ///
    /// 统计页的日历/时段/详情三块在数据没到（或加载失败、或不足一天数据）时整块隐藏。
    /// 导航若照列不误，用户点下去只会滚到一个 0 高度的空点：哨兵在不可见子树里根本
    /// 不会认领请求，那条请求就一直挂在 `request` 里——等数据到了、哨兵一活过来，
    /// 页面会毫无来由地自己跳一下。
    pub fn wrap_when(
        &self,
        label: &str,
        depth: u8,
        content: Element,
        visible: impl Fn() -> bool + 'static,
    ) -> Element {
        let visible = Rc::new(visible);
        let vis_row = visible.clone();
        let idx = {
            let mut tops = self.0.tops.borrow_mut();
            tops.push(UNSET);
            tops.len() - 1
        };
        self.0.items.borrow_mut().push(SubNavItem {
            label: label.to_string(),
            depth,
            section: idx,
            visible: vis_row,
        });
        Element::stack()
            .width_match()
            .visible_when(move || visible())
            .child(
                Element::leaf()
                    .widget(Spy {
                        idx,
                        nav: self.clone(),
                    })
                    .reactive()
                    // ⚠ 必须 Stretch。`.fill()` 给的 `Dimension::Match` 在 Frame 的
                    // unbounded 测量下会算成 0——哨兵成了个 0×0 的点，位置永远上报成
                    // 卡片左上角那一像素，不报错也看不出为什么（同 `search::anchor`）。
                    .align(Align::Stretch),
            )
            .child(content)
    }

    pub fn items(&self) -> Vec<SubNavItem> {
        self.0.items.borrow().clone()
    }

    /// 有几个**可跳转的**分块。少于两个时整条导航没有意义（见 `shell::build_subnav_col`）。
    pub fn section_count(&self) -> usize {
        self.0.tops.borrow().len()
    }

    pub fn active(&self) -> Signal<usize> {
        self.0.active
    }

    fn tab(&self) -> usize {
        self.0.tab
    }

    /// 哨兵每帧上报自己的位置与所在滚动容器的状态。
    fn report(&self, idx: usize, top: i32, scroll_y: i32, max_scroll: i32) {
        if let Some(slot) = self.0.tops.borrow_mut().get_mut(idx) {
            *slot = top;
        }
        self.0.metrics.set((scroll_y, max_scroll));
    }

    /// 请求跳到某个分块（导航栏的点击回调调用）。
    fn request(&self, idx: usize) {
        self.0.request.set(Some(idx));
        // 点击本身会让宿主置 needs_relayout，这里再请求一次是为了不依赖那条链路的细节。
        anim::request_relayout();
    }

    /// 认领属于 `idx` 的跳转请求（认领即清）。
    fn take_request(&self, idx: usize) -> bool {
        match self.0.request.get() {
            Some(i) if i == idx => {
                self.0.request.set(None);
                true
            }
            _ => false,
        }
    }

    /// 记下一次点击跳转的落点。见 [`SubNav::resolve`] 里那段「钉住」的说明。
    fn pin(&self, idx: usize, scroll_y: i32) {
        self.0.pinned.set(Some((idx, scroll_y)));
    }

    /// 依据本帧（或上一帧）各分块的位置算出该高亮谁。
    ///
    /// 规则是「顶端已越过判定线的**最后**一个分块」。末项另有特判：短卡片在页尾根本
    /// 滚不到判定线以上，不特判的话它永远高亮不到——已经滚到底就直接认末项。
    ///
    /// ## 为什么点击要把高亮「钉住」
    ///
    /// 光有上面两条规则，**点导航反而会高亮错**：页尾那几张卡合起来不足一屏，点其中
    /// 任何一张都会把页面滚到底，于是「滚到底认末项」当场生效——高级页点倒数第二张卡，
    /// 高亮却落在末项「日志设置」上（2026-09 首版实测）。这不是边角情况，是页尾必然发生。
    ///
    /// 故点击跳转时连同**落定后的 scroll_y**一起钉住：只要滚动位置还停在那儿，高亮就
    /// 认点击的那一项；用户一动滚轮位置就变，钉子自动脱落，回到按位置解析。落点由
    /// `Spy` 自己 clamp 后给出（而不是等 `arrange_scroll` 下一帧钳制），否则越界的那次
    /// 跳转会因为「记的值和实际值对不上」立刻脱钉，等于没钉。
    fn resolve(&self) {
        if let Some((idx, at)) = self.0.pinned.get() {
            if self.0.metrics.get().0 == at {
                if self.0.active.get() != idx {
                    self.0.active.set(idx);
                }
                return;
            }
            self.0.pinned.set(None);
        }
        let next = {
            let tops = self.0.tops.borrow();
            if tops.is_empty() {
                return;
            }
            let (scroll_y, max_scroll) = self.0.metrics.get();
            if max_scroll > 0 && scroll_y >= max_scroll - BOTTOM_EPS {
                tops.len() - 1
            } else {
                tops.iter()
                    .enumerate()
                    .filter(|(_, t)| **t != UNSET && **t <= ACTIVE_LINE)
                    .map(|(i, _)| i)
                    .next_back()
                    .unwrap_or(0)
            }
        };
        // 必须先比再写：`Signal::set` 会请求重绘，无条件写等于每帧自激。
        if self.0.active.get() != next {
            self.0.active.set(next);
        }
    }
}

/// 最近的滚动祖先。与 `search::anchor::scroll_ancestor` 同一份判据。
fn scroll_ancestor(tree: &Tree, id: NodeId) -> Option<NodeId> {
    let mut cur = tree.get(id).and_then(|n| n.parent);
    while let Some(i) = cur {
        let n = tree.get(i)?;
        if matches!(n.layout, Layout::Scroll) {
            return Some(i);
        }
        cur = n.parent;
    }
    None
}

/// 从 `id` 起沿父链检查可见性。任一级不可见即返回 false。
fn ancestors_visible(tree: &Tree, id: NodeId) -> bool {
    let mut cur = Some(id);
    while let Some(i) = cur {
        let Some(n) = tree.get(i) else {
            return false;
        };
        if !n.effective_visible() {
            return false;
        }
        cur = n.parent;
    }
    true
}

/// 盖在一张分区卡片上的位置哨兵：逐帧上报自己在滚动视口里的位置，并认领点向自己的跳转。
struct Spy {
    idx: usize,
    nav: SubNav,
}

impl Widget for Spy {
    fn on_update(&mut self, ctx: &mut EventCtx) {
        let id = ctx.id();
        let tree = ctx.tree_mut();
        // 未选中页的哨兵到此为止（见模块文档第 1 条）。
        if !ancestors_visible(tree, id) {
            return;
        }
        let Some(host) = scroll_ancestor(tree, id) else {
            return;
        };
        let Some((scroll_y, content_h, view_h)) = tree
            .get(host)
            .map(|n| (n.scroll_y, n.content_h, n.bounds.h))
        else {
            return;
        };
        // ⚠ **本页这一帧还没被 arrange 过就别上报**。
        //
        // `effective_visible()` 在切页的**同一帧**就翻成 true（`vis_cond` 直接读 tab 信号），
        // 而 `arrange` 只排 `visible_children` —— 一个从没显示过的页，它整棵子树的 bounds
        // 都还是默认的零矩形。此时每一枚哨兵算出的 `top` 都是 `0 - 0 = 0`，于是
        // `resolve` 的「顶端已越过判定线的**最后**一个」当场选中**末项**：切到输入页，
        // 「默认状态」先亮一下再跳回「字符与标点」——用户看到的就是闪了一下。
        //
        // 判据取自身宽度：哨兵是 `Align::Stretch` 的整宽覆盖层，真排过的必然 > 0。
        // 不上报也不认领跳转——零矩形算出来的落点同样是错的。
        if tree.get(id).is_none_or(|n| n.bounds.w <= 0) {
            return;
        }
        // `widgets::page` 的滚动容器自身无内边距（内边距在里面那个 col 上），故视口顶端
        // 就是它的绝对矩形顶端。改了那里的话这里要跟着改。
        let top = tree.abs_bounds(id).y - tree.abs_bounds(host).y;
        let max_scroll = (content_h - view_h).max(0);
        self.nav.report(self.idx, top, scroll_y, max_scroll);

        if self.nav.take_request(self.idx) {
            // ⚠ 目标偏移**不能**写成 `scroll_y + top - TOP_INSET`。
            //
            // 那两个量来自**不同帧**：`scroll_y` 是节点上的当前值（滚轮/拖动条当场就改了
            // 它），而 `top` 由 `abs_bounds` 得出、反映的是**上一次 arrange 的结果**，也就是
            // 改动之前那个 scroll_y 下的位置。刚滚过一下就点导航，两者错开整整一次滚动量，
            // 页面跳到谁也没要的位置，而高亮被 `pin` 钉在用户点的那一项上——看起来就是
            // 「导航说这儿，画面在别处」。（拖滚动条后紧接着点导航，截图回放可稳定复现。）
            //
            // 改取**卡片在内容列里的偏移** P = `abs(自己).y - abs(内容列).y`：滚动只是把
            // 内容列整体平移，这个差值恒定，压根不含 scroll_y，也就无所谓帧不帧。
            // 由 `arrange_scroll` 的 `y = inner.y - scroll_y` 可得，滚到 s 时卡顶相对视口
            // 为 `P - s`；要它停在 `TOP_INSET`，即 `s = P - TOP_INSET`。
            //
            // 内容列 = 滚动容器的唯一子节点（`widgets::page` 建的那个 col）。
            let content_top = tree
                .get(host)
                .and_then(|n| n.children.first().copied())
                .map(|c| tree.abs_bounds(c).y);
            let Some(content_top) = content_top else {
                return;
            };
            // 自己 clamp 而不是交给下一帧的 `arrange_scroll`：钉住高亮要拿落定后的真值
            // 比对（见 `SubNav::resolve`），把越界值记进去会立刻脱钉。
            let target = (tree.abs_bounds(id).y - content_top - TOP_INSET).clamp(0, max_scroll);
            tree.set_scroll_y(host, target);
            self.nav.pin(self.idx, target);
            // on_update 相位的 ctx 副作用会被丢弃，故走全局请求（见模块文档第 3 条）。
            anim::request_relayout();
        }
    }

    /// 不吃点击：本层只是块探针，卡片里的控件照常可用。
    fn hit_opaque(&self) -> bool {
        false
    }
}

/// 高亮解析器：0×0 哨兵，每帧把「该高亮谁」算出来写进 `active`。
///
/// 单独一个节点而不是让最后一枚 `Spy` 顺手算：响应式节点按注册顺序广播，而注册顺序
/// 取决于建树顺序——把结论挂在某一枚哨兵上，将来页面里插一张卡就可能悄悄换了顺序。
/// 单独一枚就无所谓早晚：早跑一帧算的是上一帧的数据，滚动中肉眼无差。
struct Resolver {
    nav: SubNav,
    tab: Signal<usize>,
}

impl Widget for Resolver {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
        if self.tab.get() != self.nav.tab() {
            return;
        }
        self.nav.resolve();
    }
}

/// 造一枚解析器哨兵（0×0，不占位）。
pub fn resolver(nav: &SubNav, tab: Signal<usize>) -> Element {
    Element::leaf()
        .widget(Resolver {
            nav: nav.clone(),
            tab,
        })
        .reactive()
}

/// 二级导航栏的一页内容：分组标题 + 可点的分块行。
pub fn build_list(nav: &SubNav) -> Element {
    let active = nav.active();
    let mut col = Element::col().width_match().spacing(1);
    // 「界面细节」试验：竖条只在「竖条+淡底」下显示。「实心」只作用于主导航——两列都是
    // 实心色块太重，二级导航那一档按「仅淡底」处理。
    let li = crate::widgets::look().0[crate::look::NAV_INDICATOR];
    for item in nav.items() {
        let idx = item.section;
        let indent = 10 + i32::from(item.depth) * 12;
        // 选中/未选中两套元素 visible_when 切换：windui 没有响应式样式，同一个节点
        // 改不了 fg/bg（与一级栏 `shell::build_nav` 同一套做法）。
        //
        // ⚠ **两态字重必须相同（都是 500）**。选中态曾用 600，12.5px 的中文上是合成粗体，
        // 笔画糊成一团，字面越复杂越明显；而且加粗会让整行文字变宽，选中/取消时字符
        // 位置来回挪一两像素。区分选中已经有三重手段——accent 文字色、淡蓝底、左缘
        // 指示条，不缺这一重。
        // ⚠ 选中态**也必须可点**。它带 `.bg(...)` 且盖在 `off` 之上，而 `hit_node` 的吞命中
        // 判据里 `style.bg.is_some()` 与 `hit_opaque()` 是并列的（同 `search::anchor` 模块
        // 文档第 3 条踩过的坑）——不给它挂 on_click，当前高亮的那一项就点不动：在一张长卡
        // 中间往下滚了半屏，想点导航回到这张卡开头，点几下都没反应。
        let nav_on_click = nav.clone();
        let on = Element::row()
            .clickable()
            .on_click(move |_| nav_on_click.request(idx))
            .width_match()
            .height(30)
            .corner(7.0)
            .cross(Align::Center)
            .padding_xy(indent, 0)
            .bg_role_alpha(Role::Accent, 0.11)
            .child(
                Element::label(&item.label)
                    .font_size(12.5)
                    .font_weight(500)
                    .fg_role(Role::Accent)
                    .weight(1.0)
                    .max_lines(1)
                    .height(17),
            )
            .visible_when(move || active.get() == idx);
        let nav_click = nav.clone();
        let off = Element::row()
            .clickable()
            .on_click(move |_| nav_click.request(idx))
            .width_match()
            .height(30)
            .corner(7.0)
            .cross(Align::Center)
            .padding_xy(indent, 0)
            .child(
                Element::label(&item.label)
                    .font_size(12.5)
                    .font_weight(500)
                    .fg_role(Role::TextMuted)
                    .weight(1.0)
                    .max_lines(1)
                    .height(17),
            );
        // 左缘指示条作覆盖层贴行左缘，不占行内宽度——选中/未选中的 padding 一致，
        // 切换时文字不横向抖动（同一级栏）。
        let indicator = Element::row()
            .width_match()
            .height(30)
            .cross(Align::Center)
            .child(
                Element::leaf()
                    .width(3)
                    .height(12)
                    .corner(1.5)
                    .bg_role(Role::Accent),
            )
            .visible_when(move || active.get() == idx && li.get() == crate::look::nav::BAR);
        let vis = item.visible.clone();
        col = col.child(
            Element::stack()
                .width_match()
                .height(30)
                .child(off)
                .child(on)
                .child(indicator)
                // 隐藏的行连同它那一档 spacing 一起被 `arrange_linear` 跳过
                // （它取的是 `visible_children`），不会在清单里留下空位。
                .visible_when(move || vis()),
        );
    }
    col
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个带 `n` 个分块的导航（不建树，只要那份位置表）。
    fn nav_with(n: usize) -> SubNav {
        let nav = SubNav::new(0);
        for i in 0..n {
            // `wrap` 顺带产出 Element，这里只要它登记分块的副作用。
            let _ = nav.wrap(&format!("s{i}"), 0, Element::leaf());
        }
        nav
    }

    /// 各分块上报一轮位置。`tops` 与分块一一对应。
    fn report_all(nav: &SubNav, tops: &[i32], scroll_y: i32, max_scroll: i32) {
        for (i, t) in tops.iter().enumerate() {
            nav.report(i, *t, scroll_y, max_scroll);
        }
    }

    #[test]
    fn active_is_last_section_past_the_line() {
        let nav = nav_with(3);
        // 第 0、1 块顶端已滚过判定线，第 2 块还在下面 ⇒ 该高亮第 1 块。
        report_all(&nav, &[-300, 10, 500], 400, 1000);
        nav.resolve();
        assert_eq!(nav.active().get(), 1);
    }

    #[test]
    fn active_is_first_section_at_top() {
        let nav = nav_with(3);
        // 未滚动：首块停在页面内边距处（16 < ACTIVE_LINE），其余都在下面。
        report_all(&nav, &[16, 300, 700], 0, 1000);
        nav.resolve();
        assert_eq!(nav.active().get(), 0);
    }

    /// 页尾那几张卡合起来不足一屏时，末块的顶端**永远**越不过判定线。
    /// 不特判的话它就再也高亮不到——这条钉住「滚到底即认末项」。
    #[test]
    fn scrolled_to_bottom_activates_last_section() {
        let nav = nav_with(3);
        report_all(&nav, &[-800, -400, 200], 1000, 1000);
        nav.resolve();
        assert_eq!(nav.active().get(), 2);
    }

    /// 点击跳转必须钉住高亮：目标在页尾时滚动会停在底部，
    /// 上一条那个「滚到底认末项」会当场把高亮抢走（2026-09 首版实测的真实症状）。
    #[test]
    fn click_pins_highlight_even_at_bottom() {
        let nav = nav_with(3);
        nav.pin(1, 1000);
        report_all(&nav, &[-800, -400, 200], 1000, 1000);
        nav.resolve();
        assert_eq!(nav.active().get(), 1, "点了第 1 块就该高亮第 1 块");
    }

    /// 钉子只到用户自己滚动为止：位置一变就恢复按位置解析。
    #[test]
    fn scrolling_away_unpins() {
        let nav = nav_with(3);
        nav.pin(1, 1000);
        report_all(&nav, &[-800, -400, 200], 1000, 1000);
        nav.resolve();
        assert_eq!(nav.active().get(), 1);

        // 用户往回滚：scroll_y 与钉住时不同 ⇒ 脱钉。
        report_all(&nav, &[-300, 10, 500], 400, 1000);
        nav.resolve();
        assert_eq!(nav.active().get(), 1, "此处按位置解析也是第 1 块");
        report_all(&nav, &[10, 400, 900], 100, 1000);
        nav.resolve();
        assert_eq!(nav.active().get(), 0, "脱钉后应跟着位置走");
    }

    /// 没有任何分块上报过（页面还没显示过）时不该乱动高亮。
    #[test]
    fn unreported_sections_are_ignored() {
        let nav = nav_with(3);
        nav.resolve();
        assert_eq!(nav.active().get(), 0);
    }

    /// 切页那一帧新页整棵子树还是零矩形，所有分块的 `top` 都会算成 0。若那批数据被
    /// 报进来，「越过判定线的最后一个」当场选中**末项**——用户看到的就是切页时最后
    /// 一条导航闪一下（2026-09-09 实测，探针确认那几帧 `bounds.w == 0`）。
    ///
    /// 真正的防线在 `Spy::on_update`（未排版不上报）。本测试把「一旦漏进来会怎样」
    /// 钉住：它记录的是**故障形态**，将来谁想放宽那道门，这里会提醒代价是什么。
    #[test]
    fn all_zero_tops_would_select_last_item() {
        let nav = nav_with(3);
        report_all(&nav, &[0, 0, 0], 0, 0);
        nav.resolve();
        assert_eq!(
            nav.active().get(),
            2,
            "全 0 会选末项 —— 故哨兵必须在页面排过版之前拒绝上报"
        );
    }

    /// 导航行与分块一一对应、序号按登记顺序递增——`build_list` 直接拿 `section`
    /// 当高亮下标用，错位的话点 A 会跳到 B。
    #[test]
    fn items_map_one_to_one_onto_sections() {
        let nav = SubNav::new(0);
        let _ = nav.wrap("a", 0, Element::leaf());
        let _ = nav.wrap("b", 0, Element::leaf());
        let _ = nav.wrap("c", 0, Element::leaf());
        assert_eq!(nav.section_count(), 3);
        let items = nav.items();
        assert_eq!(items.len(), 3);
        for (i, it) in items.iter().enumerate() {
            assert_eq!(it.section, i);
        }
    }
}
