//! 加词时的「自动生成编码」接线 — 词条一变就按当前方案反查编码填进编码框。
//!
//! windui 的 `text_input` 只写回绑定的 `Signal`，既没有 on-change 也没有失焦回调
//! （同 `pages::dict::reactive` 的说明），故这里同样挂一个 0×0 的响应式叶子，
//! 每帧比对 `Signal::version()`。
//!
//! **刻意不做防抖**：空闲时框架零帧（渲染由交互驱动，见 `pages/mod.rs` 的注释），
//! 「停手 300ms 后再算」必须给窗口挂常驻 `App::on_interval` 才会在停手后被唤醒，
//! 代价是设置窗从「空闲零 CPU」变成每秒被唤醒数次。词条通常两三个字，逐字出码
//! 反而是即时反馈。
//!
//! 三条约束都由 [`AutoEncode`] 这个状态对象承载：
//! - **出不了码就不动编码**：`encode` 回 `None`（纯符号 / 空码 / RPC 失败）时保持原编码，
//!   绝不清空——用户可能已经手填了一个能用的码，一个符号把它抹掉是纯粹的损失。
//!   **词条被删空是另一回事**：那时编码跟着清掉（见 `on_update`）。「出不了码」是「这个词
//!   暂时算不出来」，「没有词条」是「没有可算的东西」——后者留着上一条词的码，用户分不清
//!   那是残留还是刚生成的。
//! - **用户手改编码即让位**：`code` 被本部件以外的人写过一次即置 `code_dirty`，此后不再
//!   自动生成，直到 [`AutoEncode::reseed`] 复位。
//! - **外部改写不算手改**：预填字段、复位表单、点「生成编码」同样会 bump `code` 的版本号，
//!   故这些地方**必须**调 [`AutoEncode::reseed`]，否则一开对话框就被判成「用户改过编码」，
//!   自动生成从第一秒起就是死的。

use windui::core::{EventCtx, Widget};
use windui::prelude::*;

/// 自动出码的状态对象（两个信号，`Copy`，可直接进闭包与状态结构体）。
#[derive(Clone, Copy)]
pub struct AutoEncode {
    /// 用户是否手动改过编码。true = 自动生成让位于手动。
    pub code_dirty: Signal<bool>,
    /// 基线纪元：外部合法改写字段后 +1，令叶子重取版本基线并跳过这一轮。
    epoch: Signal<u64>,
}

impl Default for AutoEncode {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoEncode {
    pub fn new() -> Self {
        Self {
            code_dirty: signal(false),
            epoch: signal(0),
        }
    }

    /// 外部改写了词条/编码之后调用：这次改写不算「用户手改编码」，且恢复自动生成。
    ///
    /// 三处调用点：打开加/改词对话框（预填字段）、二次深链重填、点「生成编码」。
    pub fn reseed(&self) {
        self.code_dirty.set(false);
        self.epoch.set(self.epoch.get().wrapping_add(1));
    }
}

/// 「本轮是否生效」判据（对话框开着、且当前类别支持出码）。
type ActiveFn = Box<dyn Fn() -> bool>;
/// 反查编码。`None` = 出不了码，调用方据此保持原编码不动。
type EncodeFn = Box<dyn Fn(&str) -> Option<String>>;

struct AutoEncodeWidget {
    state: AutoEncode,
    text: Signal<String>,
    code: Signal<String>,
    active: ActiveFn,
    encode: EncodeFn,
    last_text: u64,
    last_code: u64,
    last_epoch: u64,
}

impl AutoEncodeWidget {
    /// 重取版本基线：此刻之后的改动才算「用户干的」。
    fn resync(&mut self) {
        self.last_text = self.text.version();
        self.last_code = self.code.version();
        self.last_epoch = self.state.epoch.version();
    }
}

impl Widget for AutoEncodeWidget {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
        // 外部改写（预填 / 复位 / 手动生成）：只重取基线，本轮什么都不做。
        if self.state.epoch.version() != self.last_epoch {
            self.resync();
            return;
        }
        // 不在生效范围（对话框关着、当前类别没有出码这回事）：仍要跟住基线，
        // 否则关窗期间的字段改动会攒到重开那一刻，被当成用户编辑一次性爆发。
        if !(self.active)() {
            self.resync();
            return;
        }

        // 编码被别人写过 = 用户手改（本部件自己的写回在下面就地记进基线，不会走到这里）。
        let cv = self.code.version();
        if cv != self.last_code {
            self.last_code = cv;
            if !self.state.code_dirty.get() {
                self.state.code_dirty.set(true);
            }
        }

        let tv = self.text.version();
        if tv == self.last_text {
            return;
        }
        self.last_text = tv;
        if self.state.code_dirty.get() {
            return;
        }
        let t = self.text.get();
        // 词条被删空（逐字删光，或点「清空」）：编码跟着清掉。
        //
        // 与上面「出不了码保留原编码」不矛盾，两者问的不是同一件事：出不了码时编码框里那个
        // 值仍是**为当前词条**服务的（用户手填的、或上一版算出来的），留着有用；词条都没有
        // 了，那个码就成了与界面上任何内容都对不上的残留。
        if t.trim().is_empty() {
            if !self.code.get().is_empty() {
                self.code.set(String::new());
                self.last_code = self.code.version();
            }
            return;
        }
        if let Some(c) = (self.encode)(&t) {
            self.code.set(c);
            // 自己写的不算用户手改——这一句是整块逻辑的命门，漏了则首次自动出码
            // 就会把自己判成用户编辑，此后永不再自动。
            self.last_code = self.code.version();
        }
    }
}

/// 构造自动出码叶子（0×0 不可见）。挂进任意包含加词表单的容器即可。
///
/// - `active`：本轮是否生效（典型：对话框可见 && 当前类别支持出码）。
/// - `encode`：反查编码，`None` 表示出不了码（保持原编码）。**同步调用**，
///   实现里就是一次本地 RPC。
pub fn auto_encode_node(
    state: AutoEncode,
    text: Signal<String>,
    code: Signal<String>,
    active: impl Fn() -> bool + 'static,
    encode: impl Fn(&str) -> Option<String> + 'static,
) -> Element {
    let mut w = AutoEncodeWidget {
        state,
        text,
        code,
        active: Box::new(active),
        encode: Box::new(encode),
        last_text: 0,
        last_code: 0,
        last_epoch: 0,
    };
    // 以构建时的版本为基线：构建帧不触发（同 `reactive_effect`）。
    w.resync();
    Element::leaf().widget(w).reactive().width(0).height(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use windui::core::Tree;
    use windui::geometry::Size;

    /// 建一棵只含自动出码叶子的最小树，返回「跑一帧」的闭包。
    /// 走真实的 `layout_root`——`dispatch_reactive_updates` 是 `on_update` 唯一的驱动点，
    /// 只调 widget 方法的话「有没有真挂上 `.reactive()`」这一半就没人管。
    fn harness(
        state: AutoEncode,
        text: Signal<String>,
        code: Signal<String>,
        calls: Rc<RefCell<Vec<String>>>,
        result: Rc<RefCell<Option<String>>>,
    ) -> impl FnMut() {
        let node = auto_encode_node(
            state,
            text,
            code,
            || true,
            move |t| {
                calls.borrow_mut().push(t.to_string());
                result.borrow().clone()
            },
        );
        let mut tree = Tree::new();
        let id = node.build(&mut tree);
        tree.root = Some(id);
        move || {
            tree.layout_root(Size::new(400, 300), &mut windui::text::NullTextEngine);
        }
    }

    /// 词条一变就出码，且**连续两次**都出得来——第二次是真正的守门点：
    /// 首次自动写回若被自己判成「用户手改」，第二次就再也不会发生。
    #[test]
    fn text_change_fills_code_and_keeps_working() {
        let st = AutoEncode::new();
        let (text, code) = (signal(String::new()), signal(String::new()));
        let calls = Rc::new(RefCell::new(Vec::new()));
        let result = Rc::new(RefCell::new(Some("nh".to_string())));
        let mut frame = harness(st, text, code, calls.clone(), result.clone());

        text.set("你好".into());
        frame();
        assert_eq!(code.get(), "nh", "词条变化后应自动填入编码");
        assert!(!st.code_dirty.get(), "自动写回不得被判成用户手改");

        *result.borrow_mut() = Some("nhm".to_string());
        text.set("你好吗".into());
        frame();
        assert_eq!(code.get(), "nhm", "第二次词条变化仍应自动出码");
        assert_eq!(*calls.borrow(), vec!["你好", "你好吗"]);
    }

    /// 出不了码（纯符号）时保留已存在的编码，不清空。
    #[test]
    fn unencodable_text_keeps_existing_code() {
        let st = AutoEncode::new();
        let (text, code) = (signal(String::new()), signal("nh".to_string()));
        let calls = Rc::new(RefCell::new(Vec::new()));
        let result = Rc::new(RefCell::new(None));
        let mut frame = harness(st, text, code, calls, result);

        text.set("※★".into());
        frame();
        assert_eq!(code.get(), "nh", "出不了码时必须保留原编码");
    }

    /// 词条删空 → 编码跟着清掉（且不发出码请求）。
    ///
    /// 与 `unencodable_text_keeps_existing_code` 是一对：出不了码保留、没有词条清掉。
    /// 两条一起钉住，免得日后为了满足其中一条把另一条改反。
    #[test]
    fn empty_text_clears_code() {
        let st = AutoEncode::new();
        let (text, code) = (signal("你好".to_string()), signal("nh".to_string()));
        let calls = Rc::new(RefCell::new(Vec::new()));
        let result = Rc::new(RefCell::new(Some("xx".to_string())));
        let mut frame = harness(st, text, code, calls.clone(), result);

        text.set(String::new());
        frame();
        assert_eq!(code.get(), "", "词条删空后编码应跟着清掉");
        assert!(calls.borrow().is_empty(), "空词条不该发出码请求");
    }

    /// 手改过编码之后，词条删空也不许动编码——自动生成此时已整体让位。
    #[test]
    fn empty_text_keeps_manually_edited_code() {
        let st = AutoEncode::new();
        let (text, code) = (signal("你好".to_string()), signal(String::new()));
        let calls = Rc::new(RefCell::new(Vec::new()));
        let result = Rc::new(RefCell::new(Some("xx".to_string())));
        let mut frame = harness(st, text, code, calls, result);

        code.set("mine".into());
        frame();
        text.set(String::new());
        frame();
        assert_eq!(code.get(), "mine", "手改过编码后删空词条也不得清掉它");
    }

    /// 用户手改编码后停止自动生成；`reseed`（点「生成编码」/重开对话框）后恢复。
    #[test]
    fn manual_code_edit_stops_auto_until_reseed() {
        let st = AutoEncode::new();
        let (text, code) = (signal(String::new()), signal(String::new()));
        let calls = Rc::new(RefCell::new(Vec::new()));
        let result = Rc::new(RefCell::new(Some("auto".to_string())));
        let mut frame = harness(st, text, code, calls.clone(), result);

        code.set("mine".into()); // 用户手改编码
        frame();
        assert!(st.code_dirty.get(), "手改编码应置位 code_dirty");

        text.set("你好".into());
        frame();
        assert_eq!(code.get(), "mine", "手改过编码后不得再自动覆盖");
        assert!(calls.borrow().is_empty(), "停用后不该再发出码请求");

        st.reseed(); // 相当于点了「生成编码」或重开对话框
        frame();
        text.set("你好吗".into());
        frame();
        assert_eq!(code.get(), "auto", "reseed 后应恢复自动生成");
    }

    /// 预填字段（开对话框那一刻同时写词条与编码）不得被判成用户手改。
    #[test]
    fn prefill_then_reseed_is_not_a_manual_edit() {
        let st = AutoEncode::new();
        let (text, code) = (signal(String::new()), signal(String::new()));
        let calls = Rc::new(RefCell::new(Vec::new()));
        let result = Rc::new(RefCell::new(Some("auto".to_string())));
        let mut frame = harness(st, text, code, calls.clone(), result);

        // open_* 的做法：先写字段，再 reseed。
        text.set("你好".into());
        code.set("nihao".into());
        st.reseed();
        frame();
        assert!(!st.code_dirty.get(), "预填不是用户手改");
        assert_eq!(code.get(), "nihao", "预填这一帧不该顺手重算编码");
        assert!(calls.borrow().is_empty());

        // 此后用户改词条，自动生成照常工作。
        text.set("你好吗".into());
        frame();
        assert_eq!(code.get(), "auto");
    }
}
