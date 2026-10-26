//! 模板输入框的两件辅助：**预览行**与**插入颜色**。候选注释对话框与提示内容的段表单共用。
//!
//! 设计见 core 仓 `docs/design/text-span-colors.md` §11。
//!
//! # 预览行：值全部来自 core
//!
//! `appearance.previewTemplate` 用固定样例（「你好」）求值，按当前主题求好每段的颜色再下发。
//! 设置端不依赖 wind-theme，求色只能在 core——这里只负责把 `text` + `runs` 画出来、把
//! `problems` 列出来。**旧 core 没有这个方法**（或未连接）时静默隐藏整个预览，不弹错、不占位，
//! 其余编辑功能照常；每次打开对话框重新探一次。
//!
//! # ★ 两类区间坐标系不同
//!
//! `runs` 是**输出偏移**（`text` 里的 UTF-8 字节区间），只用来上色；`problems` 是**模板偏移**
//! （发给 core 的模板里的 UTF-8 字节区间）。而段内容、注释模板输入框显示的是**转义后**的模板（制表符显示
//! 成 `\t` 两个字符，见 [`super::tooltip_sections_dialog::escape_template`]），发给 core 前要还原。
//! [`to_template`] 在还原的同时记下「模板每个字节 ← 显示文本第几个字符」，`problems` 经它换算
//! 回输入框里的字符位置，点一条问题就能在输入框里选中对应的那段。
//!
//! # ★ 插入颜色时焦点不能丢
//!
//! 用户的动作是「在输入框里选中一段 → 点色块」，期望色块插进去以后光标还在输入框里、接着就能打字。
//! 但 windui 的规矩是「点在焦点控件之外 ⇒ 清焦点」、按钮按下还会把焦点抢过去。所以：
//! 触发器与色块都用富文本的可点 span（span 点击不请求焦点），外面再套一层 [`KeepFocus`]
//! （`Widget::preserves_focus`，同菜单栏的豁免）。
//!
//! 例外是「自定义」的 #hex：要打字就得让那个小输入框拿焦点，插完焦点**回不到**模板输入框——
//! windui 的 `EventCtx::request_focus` 只能把焦点给回调自己所在的节点，没有「把焦点给别的节点」
//! 的入口（Tree 上的 `set_focused` 只改节点标志，宿主的焦点状态不跟着变）。光标位置照样摆好，
//! 用户点回输入框（或 Tab 回去）即可接着打。
//!
//! 输入框节点：点击时经 `on_click` 记下；键盘 Tab 进来、没点过的，用时从触发控件往上找最近的、
//! 正文等于本字段的输入框（[`find_input`]）。两条路都找不到才追加到末尾。
//!
//! 选区读自 windui `Tree::selection_of`（字符下标）。插入后光标的落点用**合成按键**摆
//! （Home + →×n + Shift+→×m）：`TextInput::set_selection` 虽然直接，但它记成「预置选区」，
//! 此后每次对话框隐藏再打开都会重新兑现一次——段表单是新建 / 编辑共用的，下一次打开别的段
//! 会带着这里留下的选区，一打字就把那段覆盖掉。

use std::cell::Cell;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::core::{EventCtx, NodeId, Tree, Widget};
use windui::event::KeyEvent;
use windui::geometry::{Rect, Size};
use windui::prelude::*;
use windui::render::{Canvas, Paint};
use windui::style::Style;
use windui::text::TextEngine;

/// 插入颜色的色块：`(名字, 中文名)`。取自标准色契约（core 设计 §5.4），任何继承 `_base` 的
/// 主题都查得到；气泡里写同一个名字取的是 `tooltip_<名>`，色块按场景现取，所见即所插。
///
/// 不含 `accent`：它是强调**底**色，在白底上当文字对比度不够，写字该用 `accent_text`；
/// 不含 `on_accent` / `selection_text`：只在强调底 / 选中底上有意义。
pub const SWATCHES: &[(&str, &str)] = &[
    ("text", "正文"),
    ("text_dim", "次要"),
    ("text_hint", "提示"),
    ("accent_text", "强调"),
    ("success", "成功"),
    ("warning", "提醒"),
    ("error", "错误"),
    ("info", "说明"),
];

/// 预览 RPC 的方法名。
const METHOD: &str = "appearance.previewTemplate";

/// 预览里的制表符显示成几个空格（富文本不认 `\t`；气泡里它是分列，这里只求看得出分了列）。
const TAB_AS: &str = "    ";

// ---------------------------------------------------------------------------
// 纯函数：转义映射、包选区、区间换算（可单测）
// ---------------------------------------------------------------------------

/// 输入框里的显示文本 → 发给 core 的模板，外加「模板第 i 个字节 ← 显示文本第几个字符」的表
/// （长度 = 模板字节数 + 1，末项 = 显示文本字符数）。
///
/// `escaped = false`（段名）时模板就是显示文本本身；`true`（段内容、注释模板）时按
/// [`super::tooltip_sections_dialog::unescape_template`] 的规则还原，**逐字同一套判据**——
/// 不能先还原、再对结果 `escape_template` 反推位置：不成对的 `\x` 还原后是 `\x`，再转义就成了
/// `\\x`，从那往后的位置全部错一格。
pub fn to_template(display: &str, escaped: bool) -> (String, Vec<usize>) {
    let chars: Vec<char> = display.chars().collect();
    let mut raw = String::with_capacity(display.len());
    let mut map = Vec::with_capacity(display.len() + 1);
    let mut i = 0;
    while i < chars.len() {
        let (c, width) = if escaped && chars[i] == '\\' {
            match chars.get(i + 1) {
                Some('\\') => ('\\', 2),
                Some('t') => ('\t', 2),
                Some('n') => ('\n', 2),
                _ => ('\\', 1),
            }
        } else {
            (chars[i], 1)
        };
        map.extend(std::iter::repeat_n(i, c.len_utf8()));
        raw.push(c);
        i += width;
    }
    map.push(chars.len());
    (raw, map)
}

/// 模板字节区间 → 显示文本字符区间。越界（core 与本端对不上）返回 `None`，调用方不标位置。
pub fn display_range(map: &[usize], start: usize, end: usize) -> Option<(usize, usize)> {
    let s = *map.get(start)?;
    let e = *map.get(end)?;
    (s <= e).then_some((s, e))
}

/// 把颜色插进显示文本：有选区 `[s, e)` 就包成 `$[spec]{选区}`，否则在光标处插 `$[spec]{}`。
/// `sel = None`（不知道光标在哪：用户还没点过输入框）时追加到末尾。
///
/// 返回 `(新文本, 光标字符位置)`：包选区时光标落在收尾的 `}` 之后，空插时落在花括号里。
/// 下标一律是**字符**下标（windui `TextInput` 的口径）。
pub fn wrap_color(display: &str, sel: Option<(usize, usize)>, spec: &str) -> (String, usize) {
    let chars: Vec<char> = display.chars().collect();
    let n = chars.len();
    let (s, e) = match sel {
        Some((a, b)) => (a.min(b).min(n), a.max(b).min(n)),
        None => (n, n),
    };
    let open = format!("$[{spec}]{{");
    let open_len = open.chars().count();
    let mut out: String = chars[..s].iter().collect();
    out.push_str(&open);
    out.extend(&chars[s..e]);
    out.push('}');
    out.extend(&chars[e..]);
    let caret = if s == e {
        s + open_len
    } else {
        e + open_len + 1
    };
    (out, caret)
}

/// 把 `snippet` 插进显示文本：替换选区 `[s, e)`，没有选区信息时追加到末尾。返回
/// `(新文本, 光标字符位置)`，光标在插入段末尾往回 `back` 个字符。下标是字符下标。
pub fn insert_at(
    display: &str,
    sel: Option<(usize, usize)>,
    snippet: &str,
    back: usize,
) -> (String, usize) {
    let chars: Vec<char> = display.chars().collect();
    let n = chars.len();
    let (s, e) = match sel {
        Some((a, b)) => (a.min(b).min(n), a.max(b).min(n)),
        None => (n, n),
    };
    let mut out: String = chars[..s].iter().collect();
    out.push_str(snippet);
    out.extend(&chars[e..]);
    let len = snippet.chars().count();
    (out, s + len - back.min(len))
}

/// 自定义颜色的写法校验：`#` + 3 / 6 / 8 位十六进制（与 core `parse_hex` 同一口径）。
pub fn is_hex_color(s: &str) -> bool {
    s.strip_prefix('#')
        .is_some_and(|h| matches!(h.len(), 3 | 6 | 8) && h.chars().all(|c| c.is_ascii_hexdigit()))
}

/// `[R, G, B, A]` → 颜色；形状不对返回 `None`。
fn rgba_of(v: &Value) -> Option<Color> {
    let a = v.as_array()?;
    let c = |i: usize| a.get(i)?.as_u64().and_then(|x| u8::try_from(x).ok());
    Some(Color::rgba(c(0)?, c(1)?, c(2)?, c(3)?))
}

/// 一行输出 → 段落：`runs` 覆盖到的字节区间用下发的颜色，其余用 `fg`。
/// 区间不在字符边界上（协议被破坏）时整段按 `fg` 画，不 panic。
fn para_of(text: &str, fg: Color, runs: &[Value], bold: bool) -> Para {
    let base = if bold {
        SpanStyle::new().bold()
    } else {
        SpanStyle::new()
    };
    runs_into(Para::new(), text, fg, runs, base)
}

/// 同 [`para_of`]，但接在已有段落后面、每段在 `base` 上换前景色（候选预览要先写序号与候选词，
/// 选中的那个还要带底色）。
fn runs_into(mut para: Para, text: &str, fg: Color, runs: &[Value], base: SpanStyle) -> Para {
    let style = |c: Color| base.clone().fg(c);
    let mut at = 0usize;
    let push = |p: Para, piece: &str, c: Color| p.span(piece.replace('\t', TAB_AS), style(c));
    let mut spans: Vec<(usize, usize, Color)> = runs
        .iter()
        .filter_map(|r| {
            let s = r.get("start")?.as_u64()? as usize;
            let e = r.get("end")?.as_u64()? as usize;
            Some((s, e, rgba_of(r.get("rgba")?)?))
        })
        .collect();
    spans.sort_by_key(|x| x.0);
    let valid = spans.iter().all(|&(s, e, _)| {
        s <= e && text.is_char_boundary(s) && text.is_char_boundary(e) && e <= text.len()
    });
    if !valid {
        return push(para, text, fg);
    }
    for (s, e, c) in spans {
        if s < at {
            continue; // 重叠：协议上不会发生，发生了宁可少上一段色
        }
        if s > at {
            para = push(para, &text[at..s], fg);
        }
        para = push(para, &text[s..e], c);
        at = e;
    }
    if at < text.len() {
        para = push(para, &text[at..], fg);
    }
    para
}

// ---------------------------------------------------------------------------
// 控件
// ---------------------------------------------------------------------------

/// 一个模板输入框：它的文本、是否转义显示，以及它在树里的节点（用来读 / 摆选区）。
#[derive(Clone)]
pub struct TemplateField {
    pub text: Signal<String>,
    escaped: bool,
    /// 输入框节点的缓存。windui 没有「按 Element 查节点」的入口，也没有聚焦通知：点击时经
    /// `on_click` 记下；没记到（键盘 Tab 进来的）就在用时经 [`find_input`] 从触发控件往上找。
    target: Rc<Cell<Option<NodeId>>>,
    /// 弹层（插入变量）打开时记下的选区：弹层里的搜索框会拿走焦点，插入时按它写。
    saved: Rc<Cell<Option<(usize, usize)>>>,
    /// 插入后把焦点与光标还给输入框（`select_when` 的触发计数与区间）。
    jump: Signal<u64>,
    range: Signal<(usize, usize)>,
}

impl TemplateField {
    pub fn new(text: Signal<String>, escaped: bool) -> Self {
        Self {
            text,
            escaped,
            target: Rc::new(Cell::new(None)),
            saved: Rc::new(Cell::new(None)),
            jump: signal(0),
            range: signal((0, 0)),
        }
    }

    /// 给输入框挂上「记住自己是哪个节点」与「插入后取回焦点」。**必须**挂在
    /// `Element::text_input(..)` 上。
    pub fn hook(&self, input: Element) -> Element {
        let target = self.target.clone();
        input
            .on_click(move |ctx| target.set(Some(ctx.id())))
            .select_when(self.jump, self.range)
    }

    /// 记下输入框此刻的选区（打开会拿走焦点的弹层之前调）。找不到输入框时记 `None`，
    /// 插入就追加到末尾。
    pub fn remember_selection(&self, ctx: &mut EventCtx) {
        let from = ctx.id();
        let tree = ctx.tree_mut();
        let sel = self.locate(tree, from).and_then(|id| tree.selection_of(id));
        self.saved.set(sel);
    }

    /// 把 `snippet` 插到记下的选区（替换选中的文字），光标落在插入段末尾往回 `caret_back`
    /// 个字符处，并把焦点还给输入框。
    pub fn insert_text(&self, snippet: &str, caret_back: usize) {
        let (text, caret) = insert_at(&self.text.get(), self.saved.get(), snippet, caret_back);
        self.text.set(text);
        self.saved.set(Some((caret, caret)));
        self.range.set((caret, caret));
        self.jump.update(|n| *n += 1);
    }

    /// 输入框是否转义显示（单行框里 `\n` 两个字符代表换行）。
    pub fn escaped(&self) -> bool {
        self.escaped
    }

    /// 当前显示文本对应的模板与位置表，见 [`to_template`]。
    pub fn template(&self) -> (String, Vec<usize>) {
        to_template(&self.text.get(), self.escaped)
    }

    /// 本字段输入框的节点：缓存仍有效（节点还在、正文还是本字段的）就用它，否则从 `from`
    /// （触发这次操作的控件）往上找。
    fn locate(&self, tree: &Tree, from: NodeId) -> Option<NodeId> {
        let text = self.text.get();
        let cached = self
            .target
            .get()
            .filter(|&id| tree.ime_text_of(id).as_deref() == Some(text.as_str()));
        let id = cached.or_else(|| find_input(tree, from, &text))?;
        self.target.set(Some(id));
        Some(id)
    }

    /// 在当前选区 / 光标处插入颜色，并把光标摆到该在的位置。
    fn insert_color(&self, ctx: &mut EventCtx, spec: &str) {
        let from = ctx.id();
        insert_into(ctx.tree_mut(), self, from, spec);
        ctx.mark_dirty_all();
    }

    /// 在输入框里选中字符区间 `[s, e)`（点一条问题时用）。
    fn select(&self, ctx: &mut EventCtx, s: usize, e: usize) {
        let from = ctx.id();
        if let Some(id) = self.locate(ctx.tree_mut(), from) {
            select_by_keys(ctx.tree_mut(), id, s, e);
            ctx.mark_dirty_all();
        }
    }
}

/// 从 `from` 往上逐层找：最近的一个祖先子树里、正文等于 `text` 的输入框。
///
/// 触发控件（「插入颜色」的色块、预览里的问题）与它服务的输入框同在一个对话框行 / 表单里，
/// 最近的那个就是；按正文认，是因为 windui 不暴露控件类型之外的身份——横排、竖排两个模板
/// 正文相同时也不会认错，因为更近的那一层先被找到。
fn find_input(tree: &Tree, from: NodeId, text: &str) -> Option<NodeId> {
    let mut anc = tree.get(from)?.parent;
    while let Some(a) = anc {
        let mut stack = vec![a];
        while let Some(id) = stack.pop() {
            let Some(n) = tree.get(id) else { continue };
            if tree.ime_text_of(id).as_deref() == Some(text) {
                return Some(id);
            }
            stack.extend(n.children.iter().copied());
        }
        anc = tree.get(a)?.parent;
    }
    None
}

/// 插入颜色的全过程（纯树操作，可在测试里直接驱动）：读选区 → 改正文 → 摆光标。
/// 找不到输入框（理论上不会）时追加到末尾。
fn insert_into(tree: &mut Tree, field: &TemplateField, from: NodeId, spec: &str) {
    let id = field.locate(tree, from);
    let sel = id.and_then(|id| tree.selection_of(id));
    let (text, caret) = wrap_color(&field.text.get(), sel, spec);
    field.text.set(text);
    if let Some(id) = id {
        select_by_keys(tree, id, caret, caret);
    }
}

/// 用合成按键在输入框里摆选区。为什么不用 `set_selection`，见模块文档。回到开头用
/// Ctrl+A 再 ←（选区收到起点）：多行框里 Home 只到当前行首。
fn select_by_keys(tree: &mut Tree, id: NodeId, s: usize, e: usize) {
    let key = |k: Key, shift: bool| {
        let mut ev = KeyEvent::pressed(k);
        ev.shift = shift;
        ev
    };
    let mut all = KeyEvent::pressed(Key::Other(0x41));
    all.ctrl = true;
    tree.dispatch_key(all, Some(id));
    tree.dispatch_key(key(Key::Left, false), Some(id));
    for _ in 0..s {
        tree.dispatch_key(key(Key::Right, false), Some(id));
    }
    for _ in s..e {
        tree.dispatch_key(key(Key::Right, true), Some(id));
    }
}

/// 点在这棵子树里不清焦点（`Widget::preserves_focus`）。见模块文档「焦点不能丢」。
struct KeepFocus;

impl Widget for KeepFocus {
    fn preserves_focus(&self) -> bool {
        true
    }
    fn hit_opaque(&self) -> bool {
        false
    }
}

/// 底色取自 Signal 的容器（预览的底色随主题来，构建期不知道）。
struct SignalBg(Signal<Color>);

impl Widget for SignalBg {
    fn measure(&self, _avail: Size, _style: &Style, _text: &mut dyn TextEngine) -> Size {
        Size::ZERO
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
        let (x, y, w, h) = (
            bounds.x as f32,
            bounds.y as f32,
            bounds.w as f32,
            bounds.h as f32,
        );
        canvas.fill_round_rect(x, y, w, h, 6.0, &Paint::fill(self.0.get()));
    }
    fn hit_opaque(&self) -> bool {
        false
    }
}

/// 预览的一个来源：一个输入框 + 它的场景。
pub struct PreviewSource {
    pub field: TemplateField,
    /// `comment` / `label` / `content`（core 协议值）。
    pub scene: &'static str,
    /// 段内容的求值粒度（`each`）；其余场景 `None`。
    pub each: Option<Rc<dyn Fn() -> String>>,
    /// 问题列表里的前缀（多个来源共用一个预览时区分是谁的问题）；空 = 不加。
    pub caption: &'static str,
    /// 这一行用粗体（气泡的段名行）。
    pub bold: bool,
}

/// 预览：一块按主题底色画的效果（注释另有一块选中态）+ 问题列表。
///
/// `visible` 是所在对话框的可见性：每次打开重新探 core 支不支持；`extra_version` 是除各输入框
/// 文本外、还会影响预览的信号（段表单的求值粒度）。
pub fn preview(
    visible: Signal<bool>,
    sources: Vec<PreviewSource>,
    extra_version: impl Fn() -> u64 + 'static,
) -> Element {
    let shown = signal(false);
    let normal_bg = signal(Color::rgba(0, 0, 0, 0));
    let normal_doc = signal(RichDoc::new());
    let selected_bg = signal(Color::rgba(0, 0, 0, 0));
    let selected_doc = signal(RichDoc::new());
    let has_selected = signal(false);
    let problems_doc = signal(RichDoc::new());
    let has_problems = signal(false);
    // 每条问题的（来源下标, 字符区间）；span id 是它在这里的下标。
    let targets: Rc<std::cell::RefCell<Vec<(usize, usize, usize)>>> = Rc::default();
    // `None` = 这次打开还没探过；`Some(false)` = core 不支持，本次打开期间不再请求。
    let supported: Rc<Cell<Option<bool>>> = Rc::new(Cell::new(None));
    let sources = Rc::new(sources);

    let versions = {
        let texts: Vec<Signal<String>> = sources.iter().map(|s| s.field.text).collect();
        move || {
            texts
                .iter()
                .fold(visible.version(), |acc, t| {
                    acc.wrapping_mul(31).wrapping_add(t.version())
                })
                .wrapping_add(extra_version())
        }
    };
    let refresh = {
        let (sources, targets, supported) = (sources.clone(), targets.clone(), supported.clone());
        let was_visible = Cell::new(false);
        move || {
            let now = visible.get();
            if now && !was_visible.get() {
                supported.set(None);
            }
            was_visible.set(now);
            if !now || supported.get() == Some(false) {
                return;
            }
            let mut normal = RichDoc::new();
            let mut selected = RichDoc::new();
            let mut any_line = false;
            let mut any_selected = false;
            let mut probs = RichDoc::new();
            let mut probs_at = Vec::new();
            for (si, src) in sources.iter().enumerate() {
                let (template, map) = src.field.template();
                if template.trim().is_empty() {
                    continue;
                }
                let mut params = json!({ "template": template, "scene": src.scene });
                if let Some(each) = &src.each {
                    params["each"] = json!(each());
                }
                let r = match crate::rpc::call_core(METHOD, &params) {
                    Ok(r) => r,
                    Err(_) => {
                        supported.set(Some(false));
                        shown.set(false);
                        return;
                    }
                };
                supported.set(Some(true));
                let text = r["text"].as_str().unwrap_or("");
                let empty = Vec::new();
                if let (Some(fg), Some(bg)) = (rgba_of(&r["fg"]), rgba_of(&r["bg"])) {
                    normal_bg.set(bg);
                    let runs = r["runs"].as_array().unwrap_or(&empty);
                    normal = normal.para(para_of(text, fg, runs, src.bold));
                    any_line = true;
                }
                let sel = &r["selected"];
                if let (Some(fg), Some(bg)) = (rgba_of(&sel["fg"]), rgba_of(&sel["bg"])) {
                    selected_bg.set(bg);
                    let runs = sel["runs"].as_array().unwrap_or(&empty);
                    selected = selected.para(para_of(text, fg, runs, src.bold));
                    any_selected = true;
                }
                let display: Vec<char> = src.field.text.get().chars().collect();
                for p in r["problems"].as_array().unwrap_or(&empty) {
                    let msg = p["message"].as_str().unwrap_or("");
                    let range = p["start"]
                        .as_u64()
                        .zip(p["end"].as_u64())
                        .and_then(|(s, e)| display_range(&map, s as usize, e as usize));
                    let mut para = Para::new();
                    if !src.caption.is_empty() {
                        para = para.span(format!("{}：", src.caption), SpanStyle::new());
                    }
                    // 空区间（主题加载失败那类整体提示）不标位置。
                    if let Some((s, e)) = range.filter(|(s, e)| s < e) {
                        let snippet: String = display[s..e.min(display.len())].iter().collect();
                        para = para.span_id(
                            probs_at.len().to_string(),
                            format!("「{snippet}」"),
                            SpanStyle::new().underline(),
                        );
                        probs_at.push((si, s, e));
                    }
                    probs = probs.para(para.span(msg, SpanStyle::new()));
                }
            }
            normal_doc.set(normal);
            selected_doc.set(selected);
            has_selected.set(any_selected);
            has_problems.set(!probs_at.is_empty() || probs.plain_text().trim() != "");
            problems_doc.set(probs);
            *targets.borrow_mut() = probs_at;
            shown.set(any_line);
        }
    };
    let effect = crate::pages::dict::reactive::reactive_effect(versions, refresh);

    // 注释有常态、选中两块，各带一个角注说明是哪一态；气泡只有一块，不需要。
    // 角注用固定的中灰：底色随主题可深可浅，中灰在两头都看得见。
    let captioned = sources.iter().any(|s| s.scene == "comment");
    let block = |bg: Signal<Color>, doc: Signal<RichDoc>, caption: &str| {
        let mut row = Element::row()
            .widget(SignalBg(bg))
            .width_match()
            .cross(Align::Center)
            .padding_xy(10, 6)
            .child(Element::rich_signal(doc).weight(1.0).copy_menu(false));
        if captioned {
            row = row.child(
                Element::label(caption)
                    .font_size(11.0)
                    .fg(Color::rgba(140, 140, 140, 255)),
            );
        }
        row
    };
    let problems = {
        let (sources, targets) = (sources.clone(), targets.clone());
        Element::rich_signal(problems_doc)
            .width_match()
            .fg_role(Role::Danger)
            .font_size(12.0)
            .copy_menu(false)
            .on_span_click(move |ctx, id| {
                let hit = id
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| targets.borrow().get(i).copied());
                if let Some((si, s, e)) = hit
                    && let Some(src) = sources.get(si)
                {
                    src.field.select(ctx, s, e);
                }
            })
            .visible_when(move || has_problems.get())
    };

    Element::col().width_match().spacing(4).child(effect).child(
        Element::col()
            .widget(KeepFocus)
            .width_match()
            .spacing(4)
            .child(block(normal_bg, normal_doc, "常态"))
            .child(
                block(selected_bg, selected_doc, "选中时").visible_when(move || has_selected.get()),
            )
            .child(problems)
            .visible_when(move || shown.get()),
    )
}

/// 预览样例的语种（core 回包 `samples[].lang`）：英文类型与临时英文模式用英文样例，其余中文。
pub const LANG_ZH: &str = "zh";
pub const LANG_EN: &str = "en";

/// 一个样例候选：候选词与它的注释（常态 / 选中态的片段色各一份，区间按本条注释计）。
#[derive(Debug, Clone, PartialEq)]
pub struct SampleComment {
    pub word: String,
    pub text: String,
    pub runs: Vec<Value>,
    pub sel_runs: Vec<Value>,
}

/// core 下发的一组注释求值：每个样例一条，前景色与底色是主题的、各条共用。
#[derive(Debug, Clone, PartialEq)]
pub struct CommentLook {
    pub items: Vec<SampleComment>,
    pub fg: Color,
    pub bg: Color,
    pub sel_fg: Color,
    pub sel_bg: Color,
}

/// 回包 → 一个样例候选。候选词取 `candidate`（旧 core 没有这个字段时按「你好」）。
fn sample_comment(r: &Value) -> SampleComment {
    let runs = |v: &Value| v.as_array().cloned().unwrap_or_default();
    SampleComment {
        word: r["candidate"].as_str().unwrap_or("你好").to_string(),
        // 首行上方的注释（`\n`）在这里压成一行：预览只示意排布，不画上方注释条。
        text: r["text"].as_str().unwrap_or("").replace('\n', " "),
        runs: runs(&r["runs"]),
        sel_runs: runs(&r["selected"]["runs"]),
    }
}

impl CommentLook {
    /// `appearance.previewTemplate`（comment 场景）回包 → 只有这一个样例的外观；缺颜色
    /// （协议不符）时 `None`。
    pub fn from_reply(r: &Value) -> Option<Self> {
        let sel = &r["selected"];
        Some(Self {
            items: vec![sample_comment(r)],
            fg: rgba_of(&r["fg"])?,
            bg: rgba_of(&r["bg"])?,
            sel_fg: rgba_of(&sel["fg"]).unwrap_or(rgba_of(&r["fg"])?),
            sel_bg: rgba_of(&sel["bg"]).unwrap_or(Color::rgba(0, 0, 0, 0)),
        })
    }

    /// 清掉全部注释（模板留空 = 不显示注释），只留候选词。
    fn clear_text(&mut self) {
        for it in &mut self.items {
            it.text.clear();
        }
    }

    /// 候选词的颜色：core 只下发注释色，候选词按底色深浅取近黑 / 近白。
    fn word_color(&self) -> Color {
        let lum = 0.299 * self.bg.r as f32 + 0.587 * self.bg.g as f32 + 0.114 * self.bg.b as f32;
        if lum > 140.0 {
            Color::rgba(30, 30, 30, 255)
        } else {
            Color::rgba(235, 235, 235, 255)
        }
    }

    /// 一个候选：序号 + 候选词 + 注释；第一个按选中态画（带选中底色）。
    fn candidate(&self, para: Para, i: usize, it: &SampleComment) -> Para {
        let selected = i == 0;
        let base = if selected {
            SpanStyle::new().bg(self.sel_bg)
        } else {
            SpanStyle::new()
        };
        let word_fg = self.word_color();
        let para = para
            .span(
                format!("{}.", i + 1),
                base.clone().fg(word_fg.scale_alpha(0.6)),
            )
            .span(it.word.as_str(), base.clone().fg(word_fg));
        if it.text.is_empty() {
            return para;
        }
        let para = para.span(" ", base.clone());
        if selected {
            runs_into(para, &it.text, self.sel_fg, &it.sel_runs, base)
        } else {
            runs_into(para, &it.text, self.fg, &it.runs, base)
        }
    }

    /// 横排：全部样例候选同一行。
    pub fn horizontal(&self) -> RichDoc {
        let mut para = Para::new();
        for (i, it) in self.items.iter().enumerate() {
            if i > 0 {
                para = para.span("   ", SpanStyle::new());
            }
            para = self.candidate(para, i, it);
        }
        RichDoc::new().para(para)
    }

    /// 竖排：每个样例候选一行。
    pub fn vertical(&self) -> RichDoc {
        self.items
            .iter()
            .enumerate()
            .fold(RichDoc::new(), |doc, (i, it)| {
                doc.para(self.candidate(Para::new(), i, it))
            })
    }
}

/// 回包里 `lang` 语种的样例 id（按 core 的顺序）。旧 core 不列样例时为空。
fn sample_ids(r: &Value, lang: &str) -> Vec<String> {
    r["samples"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["lang"] == lang)
        .filter_map(|s| s["id"].as_str().map(str::to_string))
        .collect()
}

/// 一个注释模板串在 `lang` 语种的各样例上的外观：先按缺省样例问一次（顺带拿样例清单与
/// 问题列表），再每个样例一次 RPC。空串 = 不显示注释：照样问 core 拿底色，再清掉注释。
/// 返回首个回包（`problems` 与样例无关，取它）与外观；连不上 core 时 `Err`。
fn fetch_comment_look(template: &str, lang: &str) -> Result<(Value, Option<CommentLook>), String> {
    let empty = template.trim().is_empty();
    let probe = if empty { " " } else { template };
    let ask = |sample: Option<&str>| {
        let mut p = json!({ "template": probe, "scene": "comment" });
        if let Some(id) = sample {
            p["sample"] = json!(id);
        }
        crate::rpc::call_core(METHOD, &p)
    };
    let first = ask(None)?;
    let Some(mut look) = CommentLook::from_reply(&first) else {
        return Ok((first, None));
    };
    let ids = sample_ids(&first, lang);
    if !ids.is_empty() {
        look.items = ids
            .iter()
            .filter_map(|id| {
                if first["sample"].as_str() == Some(id) {
                    Some(sample_comment(&first))
                } else {
                    ask(Some(id)).ok().map(|r| sample_comment(&r))
                }
            })
            .collect();
    }
    if empty {
        look.clear_text();
    }
    Ok((first, Some(look)))
}

/// 一个注释模板串在 `lang` 语种样例候选上的外观（测试用）。
#[cfg(test)]
fn comment_look(template: &str, lang: &str) -> Option<CommentLook> {
    fetch_comment_look(template, lang).ok()?.1
}

/// 注释模板的候选窗预览：横排、竖排两种排布，各画一组样例候选（第一个是选中态；中文 / 英文
/// 样例可切换，缺省中文），下面是问题列表。
///
/// 与 [`preview`] 同一个 RPC、同一套「core 不支持就整块隐藏」的规矩；模板留空时只画候选词
/// （留空 = 不显示注释）。`visible` 是所在对话框的可见性。
pub fn comment_layout_preview(visible: Signal<bool>, field: TemplateField) -> Element {
    let shown = signal(false);
    // 样例语种：0 = 中文、1 = 英文。
    let lang = signal(0usize);
    let bg = signal(Color::rgba(0, 0, 0, 0));
    let h_doc = signal(RichDoc::new());
    let v_doc = signal(RichDoc::new());
    let problems_doc = signal(RichDoc::new());
    let has_problems = signal(false);
    let targets: Rc<std::cell::RefCell<Vec<(usize, usize)>>> = Rc::default();
    let supported: Rc<Cell<Option<bool>>> = Rc::new(Cell::new(None));
    // 上一次拿到的外观：模板清空时沿用它的底色与选中色画出光秃秃的候选。
    let last: Rc<std::cell::RefCell<Option<CommentLook>>> = Rc::default();
    let text = field.text;
    let versions = move || {
        visible
            .version()
            .wrapping_mul(31)
            .wrapping_add(text.version())
            .wrapping_mul(31)
            .wrapping_add(lang.version())
    };
    let refresh = {
        let (field, targets, supported) = (field.clone(), targets.clone(), supported.clone());
        let was_visible = Cell::new(false);
        move || {
            let now = visible.get();
            if now && !was_visible.get() {
                supported.set(None);
            }
            was_visible.set(now);
            if !now || supported.get() == Some(false) {
                return;
            }
            let (template, map) = field.template();
            // 空模板也问 core：拿到底色与选中色才画得出候选。
            let code = if lang.get() == 1 { LANG_EN } else { LANG_ZH };
            let (r, look) = match fetch_comment_look(&template, code) {
                Ok(v) => v,
                Err(_) => {
                    supported.set(Some(false));
                    shown.set(false);
                    return;
                }
            };
            supported.set(Some(true));
            let Some(mut look) = look.or_else(|| last.borrow().clone()) else {
                shown.set(false);
                return;
            };
            if template.trim().is_empty() {
                look.clear_text();
            }
            *last.borrow_mut() = Some(look.clone());
            bg.set(look.bg);
            h_doc.set(look.horizontal());
            v_doc.set(look.vertical());
            let display: Vec<char> = field.text.get().chars().collect();
            let mut probs = RichDoc::new();
            let mut probs_at = Vec::new();
            let empty = Vec::new();
            for p in r["problems"].as_array().unwrap_or(&empty) {
                let msg = p["message"].as_str().unwrap_or("");
                let range = p["start"]
                    .as_u64()
                    .zip(p["end"].as_u64())
                    .and_then(|(s, e)| display_range(&map, s as usize, e as usize));
                let mut para = Para::new();
                if let Some((s, e)) = range.filter(|(s, e)| s < e) {
                    let snippet: String = display[s..e.min(display.len())].iter().collect();
                    para = para.span_id(
                        probs_at.len().to_string(),
                        format!("「{snippet}」"),
                        SpanStyle::new().underline(),
                    );
                    probs_at.push((s, e));
                }
                probs = probs.para(para.span(msg, SpanStyle::new()));
            }
            has_problems.set(!probs_at.is_empty() || probs.plain_text().trim() != "");
            problems_doc.set(probs);
            *targets.borrow_mut() = probs_at;
            shown.set(true);
        }
    };
    let effect = crate::pages::dict::reactive::reactive_effect(versions, refresh);

    let block = |caption: &str, doc: Signal<RichDoc>| {
        Element::col()
            .width_match()
            .spacing(2)
            .child(
                Element::label(caption)
                    .font_size(11.5)
                    .fg_role(Role::TextMuted),
            )
            .child(
                Element::row()
                    .widget(SignalBg(bg))
                    .width_match()
                    .padding_xy(10, 8)
                    .child(Element::rich_signal(doc).weight(1.0).copy_menu(false)),
            )
    };
    let problems = Element::rich_signal(problems_doc)
        .width_match()
        .fg_role(Role::Danger)
        .font_size(12.0)
        .copy_menu(false)
        .on_span_click(move |ctx, id| {
            let hit = id
                .parse::<usize>()
                .ok()
                .and_then(|i| targets.borrow().get(i).copied());
            if let Some((s, e)) = hit {
                field.select(ctx, s, e);
            }
        })
        .visible_when(move || has_problems.get());

    Element::col().width_match().spacing(6).child(effect).child(
        Element::col()
            .widget(KeepFocus)
            .width_match()
            .spacing(6)
            .child(
                Element::row()
                    .width_match()
                    .spacing(8)
                    .child(
                        Element::label("样例候选")
                            .font_size(11.5)
                            .fg_role(Role::TextMuted)
                            .weight(1.0),
                    )
                    .child(
                        Element::segmented(vec!["中文".to_string(), "英文".to_string()], lang)
                            .width(120),
                    ),
            )
            .child(block("横排", h_doc))
            .child(block("竖排", v_doc))
            .child(problems)
            .visible_when(move || shown.get()),
    )
}

/// 色块文档：每个标准色一枚胶囊，写中文名、用该色写在预览底色上。`colors` 为空（core 不支持）
/// 时用胶囊默认色——仍然可以插，只是看不到颜色。
fn swatch_doc(colors: &Value, bg: Option<Color>) -> RichDoc {
    let mut para = Para::new();
    for (i, (name, label)) in SWATCHES.iter().enumerate() {
        if i > 0 {
            para = para.text(" ");
        }
        let mut style = SpanStyle::new().chip();
        if let Some(c) = rgba_of(&colors[*name]) {
            style = style.fg(c);
            if let Some(bg) = bg {
                style = style.bg(bg);
            }
        }
        para = para.span_id(*name, *label, style);
    }
    RichDoc::new().para(para)
}

/// 「插入颜色」触发器（带弹层）。`scene` 决定色块按候选窗还是气泡取色。
pub fn insert_color_button(field: &TemplateField, scene: &'static str) -> Element {
    let open = signal(false);
    let chips = signal(swatch_doc(&Value::Null, None));
    let hex = signal(String::new());
    let hex_err = signal(String::new());

    let trigger = Element::button("插入颜色 ▾")
        .small()
        .outline_soft()
        .on_click(move |_| {
            if !open.get() {
                let names: Vec<&str> = SWATCHES.iter().map(|(n, _)| *n).collect();
                let params = json!({ "template": "", "scene": scene, "swatches": names });
                let r = crate::rpc::call_core(METHOD, &params).unwrap_or(Value::Null);
                chips.set(swatch_doc(&r["swatches"], rgba_of(&r["bg"])));
                hex_err.set(String::new());
            }
            open.set(!open.get());
        });

    let f_chip = field.clone();
    let f_hex = field.clone();
    let insert_hex = Rc::new(move |ctx: &mut EventCtx| {
        let v = hex.get().trim().to_string();
        if !is_hex_color(&v) {
            hex_err.set("写 #RGB、#RRGGBB 或 #RRGGBBAA".to_string());
            return;
        }
        f_hex.insert_color(ctx, &v);
        open.set(false);
    });
    let (submit, click) = (insert_hex.clone(), insert_hex);

    let panel = Element::col()
        .width(300)
        .padding(12)
        .spacing(8)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(8.0)
        .shadow(Shadow::new(0.0, 8.0, 24.0, Color::rgba(0, 0, 0, 90)))
        .child(
            Element::label("包住选区或插在光标处；颜色随主题与明暗变化")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(2),
        )
        .child(
            Element::rich_signal(chips)
                .width_match()
                .copy_menu(false)
                .on_span_click(move |ctx, name| {
                    f_chip.insert_color(ctx, name);
                    open.set(false);
                }),
        )
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::label("自定义").font_size(12.5).fg_role(Role::Text))
                .child(
                    Element::text_input(hex, "#RRGGBB")
                        .width(110)
                        .height(windui::theme::current().metrics.control_h_sm)
                        .on_submit(move |ctx| submit(ctx)),
                )
                .child(
                    Element::rich(RichDoc::new().para(Para::new().span_id(
                        "hex",
                        "插入",
                        SpanStyle::new().chip(),
                    )))
                    .copy_menu(false)
                    .on_span_click(move |ctx, _| click(ctx)),
                ),
        )
        .child(
            Element::label_signal(hex_err)
                .font_size(12.0)
                .fg_role(Role::Danger)
                .visible_when(move || !hex_err.get().is_empty()),
        );

    Element::row()
        .widget(KeepFocus)
        .cross(Align::Center)
        .child(trigger.popup(open, panel))
}

#[cfg(test)]
mod tests {
    use super::super::tooltip_sections_dialog::{escape_template, unescape_template};
    use super::*;

    /// 插入：替换选区、光标按 back 往回退；没有选区信息时追加到末尾。下标是字符下标。
    #[test]
    fn insert_at_replaces_selection_and_places_caret() {
        assert_eq!(
            insert_at("拼音abc", Some((2, 3)), "${pinyin}", 0),
            ("拼音${pinyin}bc".to_string(), 11)
        );
        assert_eq!(
            insert_at("ab", Some((1, 1)), "${|}", 2),
            ("a${|}b".to_string(), 3)
        );
        assert_eq!(insert_at("ab", None, "\n", 0), ("ab\n".to_string(), 3));
        assert_eq!(
            insert_at("ab", Some((5, 9)), "x", 0),
            ("abx".to_string(), 3),
            "越界钳到末尾"
        );
    }

    /// 每个样例一次 RPC：中文三条、英文三条各是自己的候选词与注释；空模板只留候选词。
    #[test]
    fn comment_look_has_one_item_per_sample() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let pairs = |look: &CommentLook| -> Vec<(String, String)> {
            look.items
                .iter()
                .map(|i| (i.word.clone(), i.text.clone()))
                .collect()
        };
        let zh = comment_look("${pinyin}{ · ${dict:zh_en}}", LANG_ZH).unwrap();
        assert_eq!(
            pairs(&zh),
            [
                ("你好".into(), "nǐ hǎo · int. hello".into()),
                ("中文".into(), "zhōng wén · n. Chinese language".into()),
                ("输入法".into(), "shū rù fǎ · n. input method".into()),
            ]
        );
        let en = comment_look("${dict:en_zh}{ · ${pinyin}}", LANG_EN).unwrap();
        assert_eq!(
            pairs(&en),
            [
                ("apple".into(), "n. 苹果".into()),
                ("hello".into(), "int. 喂；你好".into()),
                ("world".into(), "n. 世界".into()),
            ],
            "英文样例没有拼音，可选段整段消失"
        );
        let blank = comment_look("", LANG_ZH).unwrap();
        assert_eq!(blank.items.len(), 3);
        assert!(blank.items.iter().all(|i| i.text.is_empty()));
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 还原规则与段表单确认时的 `unescape_template` 逐字相同（含不成对的反斜杠）。
    #[test]
    fn to_template_matches_unescape() {
        for s in [
            "",
            "a\\tb",
            "\\\\t",
            "x\\y\\",
            "${char}：\\t${readings}\\n",
            "你\\t好",
        ] {
            assert_eq!(to_template(s, true).0, unescape_template(s), "{s:?}");
            assert_eq!(to_template(s, false).0, s, "{s:?}");
        }
    }

    /// ★ problems 的模板字节区间经转义映射落回显示文本：`\t` 在显示里占两个字符，
    /// 其后的位置整体后移一格；多字节字符按字符计。
    #[test]
    fn problem_range_maps_through_tab_escape() {
        let raw = "${char}：{${chaizi}\t}$[nope]{${readings}}";
        let display = escape_template(raw);
        assert_eq!(display, "${char}：{${chaizi}\\t}$[nope]{${readings}}");
        let (tpl, map) = to_template(&display, true);
        assert_eq!(tpl, raw);
        // core 报 `nope` 的位置：模板里的字节区间。
        let s = raw.find("nope").unwrap();
        let (ds, de) = display_range(&map, s, s + 4).unwrap();
        let shown: String = display.chars().skip(ds).take(de - ds).collect();
        assert_eq!(shown, "nope");
        // 未转义字段：同一模板不含制表符时，位置只做字节 → 字符换算。
        let raw2 = "你好$[#GG]{x}";
        let (_, map2) = to_template(raw2, false);
        let s2 = raw2.find("#GG").unwrap();
        assert_eq!(display_range(&map2, s2, s2 + 3), Some((4, 7)));
        // 越界不 panic。
        assert_eq!(display_range(&map2, 0, 999), None);
    }

    /// 不成对的反斜杠：还原后仍是一个字符，位置不能因此错位。
    #[test]
    fn lone_backslash_keeps_positions() {
        let display = "a\\x$[bad]{y}";
        let (tpl, map) = to_template(display, true);
        assert_eq!(tpl, "a\\x$[bad]{y}");
        let s = tpl.find("bad").unwrap();
        assert_eq!(display_range(&map, s, s + 3), Some((5, 8)));
    }

    #[test]
    fn wrap_color_wraps_selection_or_inserts_empty() {
        // 有选区：包住，光标落在 `}` 后。
        let (t, c) = wrap_color("注音${pinyin}", Some((2, 11)), "accent_text");
        assert_eq!(t, "注音$[accent_text]{${pinyin}}");
        assert_eq!(c, t.chars().count());
        // 反向选区同样处理。
        let (t2, _) = wrap_color("注音${pinyin}", Some((11, 2)), "accent_text");
        assert_eq!(t2, t);
        // 无选区：插在光标处，光标进花括号。
        let (t, c) = wrap_color("ab", Some((1, 1)), "#C00");
        assert_eq!(t, "a$[#C00]{}b");
        assert_eq!(t.chars().nth(c), Some('}'));
        // 不知道光标：追加到末尾。
        let (t, c) = wrap_color("ab", None, "error");
        assert_eq!(t, "ab$[error]{}");
        assert_eq!(c, t.chars().count() - 1);
        // 越界选区钳到文末。
        assert_eq!(wrap_color("ab", Some((5, 9)), "x").0, "ab$[x]{}");
    }

    /// 两行：各一个正文相同的输入框 + 一个触发控件（模拟横排 / 竖排两个模板）。
    /// 返回 (树, [第一行输入框, 第一行触发], [第二行输入框, 第二行触发], 两个字段)。
    fn two_rows() -> (Tree, [NodeId; 2], [NodeId; 2], TemplateField, TemplateField) {
        let (a, b) = (signal("abcd".to_string()), signal("abcd".to_string()));
        let row = |sig| {
            Element::row()
                .child(Element::text_input(sig, "").width(200).height(28))
                .child(Element::leaf().width(10).height(10))
        };
        let el = Element::col().child(row(a)).child(row(b));
        let mut tree = Tree::new();
        let root = el.build(&mut tree);
        tree.root = Some(root);
        tree.layout_root(Size::new(400, 200), &mut windui::text::NullTextEngine);
        let rows = tree.get(root).unwrap().children.clone();
        let kids = |r: NodeId| {
            let c = &tree.get(r).unwrap().children;
            [c[0], c[1]]
        };
        let (r0, r1) = (kids(rows[0]), kids(rows[1]));
        (
            tree,
            r0,
            r1,
            TemplateField::new(a, false),
            TemplateField::new(b, false),
        )
    }

    /// 合成按键摆出的选区就是 `selection_of` 读到的选区（字符下标）。
    #[test]
    fn select_by_keys_places_selection() {
        let (mut tree, [input, _], _, _, _) = two_rows();
        select_by_keys(&mut tree, input, 1, 3);
        assert_eq!(tree.selection_of(input), Some((1, 3)));
        select_by_keys(&mut tree, input, 2, 2);
        assert_eq!(tree.selection_of(input), Some((2, 2)));
    }

    /// 插入颜色：包住选区、光标落在 `}` 后；没点过输入框（键盘聚焦）也从触发控件找到它，
    /// 且找的是**同一行**那个（两行正文相同也不串）。
    #[test]
    fn insert_into_wraps_selection_and_finds_nearest_input() {
        let (mut tree, _, [input1, from1], _, f1) = two_rows();
        select_by_keys(&mut tree, input1, 1, 3);
        insert_into(&mut tree, &f1, from1, "error");
        assert_eq!(f1.text.get(), "a$[error]{bc}d");
        let caret = "a$[error]{bc}".chars().count();
        assert_eq!(tree.selection_of(input1), Some((caret, caret)));
        // 空选区：插在光标处，光标进花括号。
        let (mut tree, [input0, from0], _, f0, _) = two_rows();
        select_by_keys(&mut tree, input0, 2, 2);
        insert_into(&mut tree, &f0, from0, "#C00");
        assert_eq!(f0.text.get(), "ab$[#C00]{}cd");
        assert_eq!(tree.selection_of(input0), Some((10, 10)));
    }

    #[test]
    fn hex_color_rule_matches_core() {
        for ok in ["#C00", "#C00000", "#C0000080", "#abcdef"] {
            assert!(is_hex_color(ok), "{ok}");
        }
        for bad in ["C00000", "#C0", "#C000", "#GG0000", "#+1+2+3", "#", ""] {
            assert!(!is_hex_color(bad), "{bad}");
        }
    }

    /// runs 的字节区间切分：区间内用下发色、区间外用正文色；制表符换成空格。
    #[test]
    fn para_of_splits_by_runs() {
        let text = "hao\t(nǐ)";
        let runs = vec![json!({"start": 4, "end": 9, "rgba": [1, 2, 3, 255]})];
        let p = para_of(text, Color::rgba(9, 9, 9, 255), &runs, false);
        let doc = RichDoc::new().para(p);
        assert_eq!(doc.plain_text(), format!("hao{TAB_AS}(nǐ)"));
        // 坏区间（不在字符边界）整段回落正文色而不 panic。
        let bad = vec![json!({"start": 5, "end": 6, "rgba": [1, 2, 3, 255]})];
        let doc = RichDoc::new().para(para_of("nǐ", Color::rgba(9, 9, 9, 255), &bad, false));
        assert_eq!(doc.plain_text(), "nǐ");
    }
}
