//! 跳转锚点 —— 每个可被搜到的行/卡片上都盖着一个，负责「把自己滚进视口并闪一下」。
//!
//! ## 为什么由目标自己认领，而不是搜索那边直接滚过去
//!
//! 点结果的那一刻，目标还不在布局里：它所在的页上一帧还不可见，
//! `scroll_into_view` 只会拿到一个没参与过布局的过期矩形。所以搜索侧只负责切页 +
//! 挂一个 [`JumpRequest`]，滚动交给目标位置上的这枚哨兵——它在自己**真的可见**的
//! 那一帧才动手，时序天然正确，不需要数帧。
//!
//! ## 三个不能想当然的点
//!
//! 1. **`on_update` 不看可见性**。`Tree::dispatch_reactive_updates` 对所有响应式节点
//!    每帧广播，不管祖先是不是 `visible`；而设置窗把 8 个页面全建好、叠在一个 stack 里
//!    靠 `visible_when` 切。故认领前必须自查祖先链（[`ancestors_visible`]），
//!    否则未选中页里的锚点会拿过期 bounds 把某个滚动容器滚到莫名其妙的位置。
//! 2. **闪烁不能靠 `Anim` 的多段 `retarget`**。`request_repaint` 只请求绘制，而
//!    `on_update` 挂在 `layout_root` 上、只在 `needs_relayout` 时才跑——多段补间会卡在
//!    第一段。故把动画写成「时刻 → 值」的纯函数 [`flash_alpha`]，状态只有一个起始时刻。
//! 3. **高亮只能自绘，不能设 `style.bg`**。`hit_node` 的「吞命中」判据里
//!    `style.bg.is_some()` 与 `hit_opaque()` 是并列的：给覆盖层设了背景色，那一行就此
//!    点不动。

use std::cell::Cell;

use windui::anim;
use windui::core::{EventCtx, NodeId, Tree, Widget};
use windui::geometry::Rect;
use windui::prelude::*;
use windui::render::{Canvas, Paint};
use windui::style::Style;

use super::index::JumpTarget;
use super::jump::JumpRequest;

/// 闪烁总时长与次数。三下、约一秒——够看清是哪一行，又不至于闪到让人烦。
const FLASH_TOTAL_MS: u64 = 960;
const FLASH_PULSES: u64 = 3;
/// 高亮底的峰值不透明度。
const FLASH_PEAK: f32 = 0.22;
/// 高亮圆角，与设置行的视觉节奏一致。
const FLASH_CORNER: f32 = 6.0;

/// 闪烁强度：距起始 `t` 毫秒时的不透明度。
///
/// 三次正弦半波，起止都归零。纯函数，故可直接测波形，也不依赖任何逐帧推进
/// （见模块文档第 2 点）。
fn flash_alpha(t: u64) -> f32 {
    if t >= FLASH_TOTAL_MS {
        return 0.0;
    }
    let pulse = FLASH_TOTAL_MS / FLASH_PULSES;
    let phase = (t % pulse) as f32 / pulse as f32;
    (phase * std::f32::consts::PI).sin() * FLASH_PEAK
}

/// 滚动到位后再多滚一点，让目标不贴着视口边缘。
///
/// `Tree::scroll_into_view` 是**最小滚动**：目标恰好擦着视口下沿（或上沿）就停。
/// 那正是「刚冒个头」的观感——尤其目标是整张卡片时，用户只看得见它的标题。
/// 多滚这一点点，目标就落在视口里而不是边界上。
const SCROLL_MARGIN: i32 = 24;

/// 最近的滚动祖先。
fn scroll_ancestor(tree: &Tree, id: NodeId) -> Option<NodeId> {
    let mut cur = tree.get(id).and_then(|n| n.parent);
    while let Some(i) = cur {
        let n = tree.get(i)?;
        if matches!(n.layout, windui::core::Layout::Scroll) {
            return Some(i);
        }
        cur = n.parent;
    }
    None
}

/// 把 `id` 滚进视口，并留出 [`SCROLL_MARGIN`] 的余量。返回是否真的滚动了。
///
/// 余量的方向跟着这次滚动的方向走：向下滚（目标原本在下方）就再往下一点，
/// 反之亦然。越界不必自己钳制——`arrange_scroll` 下一帧会做。
fn scroll_into_view_with_margin(tree: &mut Tree, id: NodeId) -> bool {
    let host = scroll_ancestor(tree, id);
    let before = host.and_then(|i| tree.get(i).map(|n| n.scroll_y));
    let scrolled = tree.scroll_into_view(id);
    if !scrolled {
        return false; // 本来就在视口里：不动它，免得平白晃一下
    }
    if let (Some(i), Some(before)) = (host, before)
        && let Some(after) = tree.get(i).map(|n| n.scroll_y)
    {
        let dir = if after >= before { 1 } else { -1 };
        tree.set_scroll_y(i, after + dir * SCROLL_MARGIN);
    }
    true
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

struct Anchor {
    /// 本锚点的键：manifest 项即其 key，手写卡片用 `<page>:<slug>`。
    key: String,
    jump: Signal<Option<JumpRequest>>,
    /// 闪烁起始时刻，`None` = 不在闪。
    ///
    /// 用 `Option` 而不是「0 表示没在闪」的哨兵：`anim::clock_ms()` 是帧时钟，
    /// 起点就是 0——哨兵会与「第 0 毫秒开始闪」撞上。单测里当场撞到了。
    /// `Cell` 是因为 `paint` 只拿得到 `&self`。
    flash_start: Cell<Option<u64>>,
    /// 已经认出目标是自己、正等下一帧布局落定。
    ///
    /// **为什么不能当帧就滚**：`effective_visible()` 在切页的**同一帧**就翻成 true
    /// （`vis_cond` 直接读 tab 信号），而 `on_update` 跑在 `measure` 之前——此刻本节点的
    /// `bounds` 还是上次可见时的旧值，从没显示过的页干脆是零。拿零矩形调
    /// `scroll_into_view`，它会算出「已经在视口里、不用滚」并**静默**返回 false。
    ///
    /// 可见性门控挡的是「不可见页误滚」，挡不住「刚可见、还没布局」。故再等一帧：
    /// 置位 + 请求重排，下一帧几何就是真的了。
    armed: bool,
}

impl Widget for Anchor {
    fn on_update(&mut self, ctx: &mut EventCtx) {
        // 绝大多数帧都在这里返回：常态下只读一个信号，不做父链上溯。
        let Some(req) = self.jump.get() else {
            self.armed = false;
            return;
        };
        if req.pending_anchor() != Some(self.key.as_str()) {
            return;
        }
        let id = ctx.id();
        let tree = ctx.tree_mut();
        // 还没轮到我可见（目标在别的页上）——什么都不做，等那一页被切出来。
        if !ancestors_visible(tree, id) {
            self.armed = false;
            return;
        }
        if !self.armed {
            // 刚可见，几何还是上一帧的（见 `armed` 的说明）。置位 + 请求重排，
            // 下一帧再动手；请求**不清空**，故不会被别的锚点抢走。
            //
            // ⚠ 这里必须用全局的 `anim::request_relayout()`，不能用 `ctx.mark_layout_dirty()`：
            // `Tree::call_on_update` 把本相位 ctx 请求的副作用**除 toast 与焦点外全部丢弃**
            // （见 core.rs 里那段注释）。用 ctx 那条路，下一帧永远不会来，armed 就此卡死。
            self.armed = true;
            anim::request_relayout();
            return;
        }
        self.armed = false;
        let scrolled = scroll_into_view_with_margin(tree, id);
        // 第一跳（对话框内的项，框还没开）：滚到入口行就够了，**不闪**——真正的目标
        // 在框里，闪入口行会把注意力引错地方。请求改标记后留着，交给框内那一行。
        let first_hop = matches!(req.target, JumpTarget::RowInDialog { .. }) && !req.dialog_opened;
        if first_hop {
            self.jump.set(Some(JumpRequest {
                dialog_opened: true,
                ..req
            }));
        } else {
            self.flash_start.set(Some(anim::clock_ms()));
            // 认领即清：这次请求到此为止，不会再被别的锚点捡走。
            self.jump.set(None);
        }
        // 同上：on_update 相位的 ctx 副作用会被丢弃，故走全局请求。
        // 滚动后要重排让新的 scroll_y 落地；闪烁本身每帧由 `paint` 自驱。
        if scrolled {
            anim::request_relayout();
        }
        anim::request_repaint();
    }

    fn paint(
        &self,
        bounds: Rect,
        _content: Rect,
        _focused: bool,
        _enabled: bool,
        canvas: &mut dyn Canvas,
        _style: &Style,
    ) {
        let Some(start) = self.flash_start.get() else {
            return;
        };
        let t = anim::clock_ms().saturating_sub(start);
        if t >= FLASH_TOTAL_MS {
            return; // 闪完就什么都不画——静止时零开销
        }
        let a = flash_alpha(t);
        let accent = windui::theme::current().palette.accent;
        let (x, y, w, h) = (
            bounds.x as f32,
            bounds.y as f32,
            bounds.w as f32,
            bounds.h as f32,
        );
        canvas.fill_round_rect(
            x,
            y,
            w,
            h,
            FLASH_CORNER,
            &Paint::fill(accent.scale_alpha(a)),
        );
        canvas.stroke_round_rect(
            x + 0.5,
            y + 0.5,
            w - 1.0,
            h - 1.0,
            FLASH_CORNER,
            1.0,
            &Paint::fill(accent.scale_alpha(a * 2.0)),
        );
        // 自驱下一帧：动画期间没有别的东西会来推它（见模块文档第 2 点）。
        anim::request_repaint();
    }

    /// 不吃点击：本层只是块装饰，行上的控件照常可用。
    fn hit_opaque(&self) -> bool {
        false
    }
}

/// 造一枚锚点覆盖层。
///
/// ⚠ 调用方必须给它 `.align(Align::Stretch)` 才会撑满整行：`arrange_frame` 判的是
/// `align == Stretch`，`.fill()` 给的 `Dimension::Match` 在 Frame 的 unbounded 测量下
/// 会算成 0——那样锚点是个 0×0 的点，不报错、不闪、看不出为什么。
/// [`wrap`] 已经代劳，直接用它即可。
fn anchor(key: impl Into<String>, jump: Signal<Option<JumpRequest>>) -> Element {
    Element::leaf()
        .widget(Anchor {
            key: key.into(),
            jump,
            flash_start: Cell::new(None),
            armed: false,
        })
        .reactive()
        .align(Align::Stretch)
}

/// 给一行/一张卡片挂上锚点，返回包好的元素。
///
/// 锚点排在**内容之前**（底层）：高亮是背景，压在文字上会让那一行的字发蓝。
pub fn wrap(
    content: Element,
    key: impl Into<String>,
    jump: Signal<Option<JumpRequest>>,
) -> Element {
    let key = key.into();
    #[cfg(test)]
    MOUNTED.with(|m| {
        m.borrow_mut().insert(key.clone());
    });
    Element::stack()
        .width_match()
        .child(anchor(key, jump))
        .child(content)
}

// 测试期记录「界面上到底挂了哪些锚点」。
//
// 手写卡片的锚点键分散在各页的构建代码里，与 `search::index` 那张表是两份知识，
// 编译器一句话都不会说。靠 `handwritten_anchors_are_all_mounted` 比对两边。
// （文档注释在 `thread_local!` 上不生效，故用普通注释。）
#[cfg(test)]
thread_local! {
    static MOUNTED: std::cell::RefCell<std::collections::BTreeSet<String>> =
        const { std::cell::RefCell::new(std::collections::BTreeSet::new()) };
}

/// 本次进程里被 [`wrap`] 挂过的锚点键。
#[cfg(test)]
pub fn mounted_anchors() -> std::collections::BTreeSet<String> {
    MOUNTED.with(|m| m.borrow().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flash_starts_and_ends_at_zero() {
        assert_eq!(flash_alpha(0), 0.0);
        assert_eq!(flash_alpha(FLASH_TOTAL_MS), 0.0);
        assert_eq!(flash_alpha(FLASH_TOTAL_MS + 500), 0.0);
    }

    /// 三次脉冲：每段中点都到峰值，段界都归零。
    #[test]
    fn flash_pulses_three_times() {
        let pulse = FLASH_TOTAL_MS / FLASH_PULSES;
        for k in 0..FLASH_PULSES {
            let mid = k * pulse + pulse / 2;
            assert!(
                (flash_alpha(mid) - FLASH_PEAK).abs() < 0.01,
                "第 {} 次脉冲的峰值不对：{}",
                k + 1,
                flash_alpha(mid)
            );
            assert!(flash_alpha(k * pulse) < 0.01, "第 {} 段起点该归零", k + 1);
        }
    }

    #[test]
    fn flash_never_exceeds_peak() {
        for t in 0..FLASH_TOTAL_MS {
            let a = flash_alpha(t);
            assert!((0.0..=FLASH_PEAK + 0.001).contains(&a), "t={t} → {a}");
        }
    }

    /// 页面直出行与手写卡片认领自己的锚点。
    #[test]
    fn pending_anchor_points_at_row() {
        let r = JumpRequest {
            seq: 1,
            dialog_opened: false,
            target: JumpTarget::Row {
                tab: 0,
                anchor: "a.b".into(),
            },
        };
        assert_eq!(r.pending_anchor(), Some("a.b"));

        let c = JumpRequest {
            seq: 1,
            dialog_opened: false,
            target: JumpTarget::Card {
                tab: 0,
                anchor: "dict:manage".into(),
            },
        };
        assert_eq!(c.pending_anchor(), Some("dict:manage"));
    }

    /// 对话框内的项先由**宿主行**认领（滚到入口）；开框与框内闪烁是 P3。
    #[test]
    fn dialog_target_waits_on_host_row() {
        let r = JumpRequest {
            seq: 1,
            dialog_opened: false,
            target: JumpTarget::RowInDialog {
                tab: 0,
                host_anchor: "host".into(),
                dialog: "host".into(),
                anchor: "inner".into(),
            },
        };
        assert_eq!(r.pending_anchor(), Some("host"));
    }

    /// 方案级目标不进锚点——它没有可滚过去的行，只会得到一条指示（P4）。
    #[test]
    fn schema_level_has_no_anchor() {
        let r = JumpRequest {
            seq: 1,
            dialog_opened: false,
            target: JumpTarget::SchemaLevel {
                hint_path: vec!["方案".into()],
            },
        };
        assert_eq!(r.pending_anchor(), None);
    }

    /// **守门**：索引里每个手写卡片目标的锚点，界面上都得真的挂着。
    ///
    /// 这是两份知识——索引在 `search::index` 的表里，锚点在各页的构建代码里，
    /// 改一边漏一边编译器不会响。漏掉的症状是「搜到了、点了、页也切了，就是不滚
    /// 也不闪」，没有任何报错可查。
    #[test]
    fn handwritten_anchors_are_all_mounted() {
        use crate::search::index::build_index;
        use std::cell::RefCell;
        use std::rc::Rc;

        // 构建整棵页面树：各页的 `wrap` 会把自己的键登记进 MOUNTED。
        let app = Rc::new(RefCell::new(crate::state::AppState::empty()));
        {
            let st = app.borrow();
            let _tabs = crate::pages::all_tabs(&app, &st);
        }
        let mounted = mounted_anchors();

        let idx = build_index(&crate::manifest::Manifest::from_embedded());
        let missing: std::collections::BTreeSet<&str> = idx
            .iter()
            .filter_map(|e| match &e.target {
                JumpTarget::Card { anchor, .. } => Some(anchor.as_str()),
                // 对话框内项的**入口行**也必须挂着，否则第一跳无人认领——框永远不会开，
                // 表现是「切了页就停在那儿」。方案页那四个「全局方案配置」按钮行正是
                // 手写的，一开始就漏了。
                JumpTarget::RowInDialog { host_anchor, .. } => Some(host_anchor.as_str()),
                _ => None,
            })
            .filter(|a| !mounted.contains(*a))
            .collect();
        assert!(
            missing.is_empty(),
            "这些手写卡片的锚点没在界面上挂：{missing:?}\n\
             → 在对应页面的卡片外面包一层 search::anchor::wrap(卡片, \"<键>\", state.jump)"
        );
    }

    #[test]
    fn wrap_builds_without_panicking() {
        let jump = signal(None);
        let _ = wrap(Element::label("行"), "k", jump);
    }

    /// 在一棵自建树上把锚点的 `on_update` 真跑一遍。
    ///
    /// `visible` 决定那个节点可不可见，返回 (请求是否被认领, 是否开始闪烁)。
    fn run_anchor(key: &str, target: JumpTarget, visible: bool) -> (bool, bool) {
        use windui::core::Tree;

        let jump = signal(Some(JumpRequest {
            seq: 7,
            target,
            dialog_opened: false,
        }));
        let mut tree = Tree::new();
        let el = if visible {
            Element::leaf()
        } else {
            Element::leaf().visible_when(|| false)
        };
        let id = el.build(&mut tree);
        tree.root = Some(id);

        let mut w = Anchor {
            key: key.to_string(),
            jump,
            flash_start: Cell::new(None),
            armed: false,
        };
        // 跑两帧：第一帧只置位等布局，第二帧才真动手（见 `Anchor::armed`）。
        windui::testing::run_with_ctx_in(&mut tree, id, |ctx| w.on_update(ctx));
        windui::testing::run_with_ctx_in(&mut tree, id, |ctx| w.on_update(ctx));
        (jump.get().is_none(), w.flash_start.get().is_some())
    }

    /// **回归**：只跑一帧不认领。
    ///
    /// 切页那一帧 `effective_visible()` 已经是 true，但布局还没跑、几何还是旧的。
    /// 此时若当场滚，`scroll_into_view` 会拿零矩形算出「不用滚」并静默返回 false——
    /// 表现是「跳过去了，但页面纹丝不动」。实测就是这么错的（见 `Anchor::armed`）。
    #[test]
    fn first_frame_only_arms_does_not_claim() {
        use windui::core::Tree;

        let jump = signal(Some(JumpRequest {
            seq: 7,
            dialog_opened: false,
            target: JumpTarget::Row {
                tab: 0,
                anchor: "a.b".into(),
            },
        }));
        let mut tree = Tree::new();
        let id = Element::leaf().build(&mut tree);
        tree.root = Some(id);
        let mut w = Anchor {
            key: "a.b".into(),
            jump,
            flash_start: Cell::new(None),
            armed: false,
        };

        windui::testing::run_with_ctx_in(&mut tree, id, |ctx| w.on_update(ctx));
        assert!(jump.get().is_some(), "第一帧只置位，不该认领");
        assert!(w.armed, "第一帧应置位等布局");
        assert!(w.flash_start.get().is_none(), "第一帧不该开始闪");

        windui::testing::run_with_ctx_in(&mut tree, id, |ctx| w.on_update(ctx));
        assert!(jump.get().is_none(), "第二帧才认领");
    }

    /// 目标是自己、且可见 → 认领：清空请求并开始闪烁。
    #[test]
    fn visible_anchor_claims_matching_request() {
        let (claimed, flashing) = run_anchor(
            "a.b",
            JumpTarget::Row {
                tab: 0,
                anchor: "a.b".into(),
            },
            true,
        );
        assert!(claimed, "可见的目标锚点应认领请求");
        assert!(flashing, "认领后应开始闪烁");
    }

    /// **回归**：不可见页里的锚点绝不能认领。
    ///
    /// `dispatch_reactive_updates` 对所有响应式节点每帧广播、不看可见性，而设置窗
    /// 把 8 个页面全建好叠在一个 stack 里。少了这道门控，未选中页里的同名锚点会抢先
    /// 认领，拿一个没参与过布局的矩形去滚——表现是「跳过去什么也没发生，而且别处的
    /// 滚动条莫名动了一下」。
    #[test]
    fn invisible_anchor_does_not_claim() {
        let (claimed, flashing) = run_anchor(
            "a.b",
            JumpTarget::Row {
                tab: 0,
                anchor: "a.b".into(),
            },
            false,
        );
        assert!(
            !claimed,
            "不可见的锚点不该认领请求——它得等自己那一页被切出来"
        );
        assert!(!flashing, "不可见时不该开始闪烁");
    }

    /// 目标不是自己 → 不认领（否则第一个被广播到的锚点就把请求吃了）。
    #[test]
    fn non_matching_anchor_ignores_request() {
        let (claimed, _) = run_anchor(
            "other.key",
            JumpTarget::Row {
                tab: 0,
                anchor: "a.b".into(),
            },
            true,
        );
        assert!(!claimed);
    }

    /// 方案级请求没有锚点可认领，任何锚点都不该动它（留给指示条）。
    #[test]
    fn schema_level_request_is_left_alone() {
        let (claimed, _) = run_anchor(
            "a.b",
            JumpTarget::SchemaLevel {
                hint_path: vec!["方案".into()],
            },
            true,
        );
        assert!(!claimed);
    }
}
