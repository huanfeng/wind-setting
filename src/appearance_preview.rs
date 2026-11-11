//! 外观页顶部的「效果预览」：草稿配置里影响候选窗与工具栏的那几段 `ui` 子树交给 core
//! 出一张预览图（`theme.renderPreview` 的 `ui` 参数，协议见 WindInput
//! `docs/design/wind-scene-extraction.md` §7.2），改了字号、字体、每页候选数、工具栏格子
//! 这类设置，不必保存就能看到效果。
//!
//! - **草稿**：与按键冲突检测同一取法——`current_config` 的副本跑一遍全部写回器
//!   （[`AppState::draft_config`]），只取 [`DRAFT_SECTIONS`] 这几段。只在外观页可见时算。
//! - **何时重取**：草稿那几段、缩放、主题列表（导入 / 删除 / 覆盖）、主题编辑器推送的改动、
//!   活跃方案（方案级候选覆盖）任一变化；草稿明暗跟随系统时，系统切了明暗也算
//!   （[`AppState::system_theme_flips`]）。
//! - **防抖、只认最新**：一变就取消上一个还没回来的请求、重新计时 [`DEBOUNCE`]；取消的请求回执
//!   直接丢掉（[`NetTasks::cancel`] 摘掉续体），屏上永远是最后一次的结果。
//! - **失败不崩**：保留上一张图，下方一行简短原因。「忙 / 超时」隔 [`RETRY_DELAY`] 重试，至多
//!   [`MAX_RETRIES`] 次；连不上服务端不原地重发，等切回本页或连接恢复再取。
//! - **形态**：卡头（标题行右侧）的分段控件在「候选窗 / 工具栏」间切换（`theme.renderPreview`
//!   的 `kind` 参数，候选窗走协议缺省），选中段会话内记住、不落盘。换形态先回到「生成中」占位，
//!   图画不出时落「无法生成」——不留另一个形态的旧图冒充新预览。
//! - **fit 目标框**：候选窗形态请求带 `fit_w`/`fit_h`（图区内容区的逻辑尺寸，见 [`FIT_W`]），
//!   core 拿它折算渲染倍率（clamp 1–3），预览图更能撑满图区；工具栏形态不带（core 回到自然
//!   逻辑尺寸渲染、显示端 1:1），预览贴近实际观感。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use windui::core::{EventCtx, Widget};
use windui::geometry::Rect;
use windui::prelude::*;
use windui::render::Canvas;
use windui::style::Style;

use crate::net_task::{NetTasks, TaskId};
use crate::state::AppState;
use crate::theme_gallery::{Png, RawPng, Receipt, ThemeGallery};

/// 卡片标题（二级导航里也叫这个）。
pub const SECTION_TITLE: &str = "效果预览";
/// 搜索锚点。
pub const ANCHOR: &str = "ui:appearance-preview";
/// 草稿变化到发请求之间至少隔这么久。
const DEBOUNCE: Duration = Duration::from_millis(200);
/// 「忙 / 超时」后隔多久重试、至多几次（同画廊）。
const RETRY_DELAY: Duration = Duration::from_millis(1000);
const MAX_RETRIES: u32 = 3;
/// 图区固定高度：加载与换图时布局不随之涨缩，进页不抖。先前 160（曾与主题画廊导入预览的
/// `IMPORT_PREVIEW_MAX_H` 一致），用户嫌预览偏小提到 200，内边距收入 [`preview_area`]。
const AREA_H: i32 = 200;
// 2026-11 反馈的钉子：改回去必须有意识地改（布局测试钉的是这两条的外在表现，这里钉数值）。
const _: () = assert!(AREA_H == 200);
/// 图区内边距（水平、垂直同值）：`preview_area` 摆图用，fit 尺寸从外框推导时也扣它。
const AREA_PAD: i32 = 8;
/// fit 目标框：图区内容区的逻辑尺寸，随请求发给 core（`fit_w`/`fit_h`，正整数，core 拿它
/// 折算渲染倍率、clamp 1–3；缺省不带 = 旧行为）。取常量是因为请求发出时布局还没跑，拿不到
/// 实时尺寸；宽度没有可写的代码级推导链（窗口 − 主导航 − 二级导航 − 页边距 − 卡边距 − 图区
/// 内边距，散在四个文件里），默认窗口下实测 588，由 `fit_consts_match_real_layout` 在真树上
/// 钉住，布局一动这里跟着红。高度 = [`AREA_H`] − 上下内边距。
pub(crate) const FIT_W: i32 = 588;
pub(crate) const FIT_H: i32 = AREA_H - 2 * AREA_PAD;
const _: () = assert!(FIT_H == 184);
const _: () = assert!(FIT_W == 588);
/// 草稿里出预览要带上的段（`ui.<段>`）。工具栏预览（`kind = toolbar`）取 `ui.toolbar` 段
/// 渲染（core 端 `candidate_preview_draft` 折算 `toolbar_items`），缺了它改工具栏设置
/// 永远出的是生效配置的图、不反映草稿；状态提示等与预览无关的段不带。
pub const DRAFT_SECTIONS: &[&str] = &["theme", "candidate", "font", "comment", "toolbar"];

/// `ui.theme.style` 是不是「跟随系统」：同 core `ThemeStyle::from_config`，不是 light / dark 都算。
pub fn follows_system(style: Option<&str>) -> bool {
    !matches!(
        style.map(|s| s.trim().to_lowercase()).as_deref(),
        Some("light" | "dark")
    )
}

/// 形态段标签（顺序即 `kind` 信号的索引）。
pub const KIND_LABELS: &[&str] = &["候选窗", "工具栏"];

/// 段索引 → 请求参数里的 `kind`：候选窗是协议缺省形态、参数里省略，工具栏显式带上；
/// 越界按候选窗算。
pub fn kind_param(idx: usize) -> Option<&'static str> {
    (idx == 1).then_some("toolbar")
}

/// 段索引 → fit 目标框：候选窗带图区内容区尺寸（588×184，core 折算渲染倍率撑满图区）；
/// 工具栏不带（两维省键，core 回到自然逻辑尺寸渲染；省键的理由是协议只收正整数、
/// 省键即旧版请求形状），显示端 1:1 居中、不放大。越界按候选窗算。
pub(crate) fn kind_fit(idx: usize) -> (i32, i32) {
    match idx {
        1 => (0, 0),
        _ => (FIT_W, FIT_H),
    }
}

/// 段索引 → 请求 scale：工具栏固定 1.0。`Fit::None` 是「1 图片像素 = 1 逻辑 dp」，
/// 显示物理 = PNG 像素 × 窗口 DPI——core 按 scale 出图，scale 带窗口 DPI 会出
/// dpi 倍像素、屏上再乘一次 DPI，预览偏大 dpi²/dpi = dpi 倍（125% 屏大 25%）。
/// 固定 1.0 后显示物理 = 自然逻辑 × DPI = 恰好实际工具栏的物理大小。副产物：
/// 请求 Key 的 scale_pct 恒 100，跨屏拖动窗口不触发工具栏预览重取。
/// 越界按候选窗算（取窗口 DPI）。
pub(crate) fn kind_scale(idx: usize, dpi: f32) -> f32 {
    match idx {
        1 => 1.0,
        _ => dpi,
    }
}

/// 屏上显示的那一张。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Shown {
    Waiting,
    /// 形态对应的图没画出来、屏上又没有可保留的图：图区里的占位。
    Failed,
    Ready(Png),
}

/// 草稿配置 → 要发的 `ui` 参数（只留 [`DRAFT_SECTIONS`]）。
pub fn draft_ui(draft: &crate::config::Config) -> Value {
    let mut out = Map::new();
    for sec in DRAFT_SECTIONS {
        if let Some(v) = draft.get_path(&format!("ui.{sec}")) {
            out.insert((*sec).to_string(), v.clone());
        }
    }
    Value::Object(out)
}

/// 一次请求要带的参数：草稿 `ui` 子树 + 缩放 + 形态（候选窗为协议缺省、省略）+ fit 目标框。
///
/// `fit` 是图区内容区的逻辑尺寸 `(宽, 高)`。协议要求 `fit_w`/`fit_h` 为正整数、**可只给
/// 一维**，对非法值整条报错；本函数对非正那一维省键不发（所以发出去的必是合法值）。
/// 两维都不给 = 旧版请求的形状。
pub fn request_params(ui: &Value, scale: f32, kind: Option<&str>, fit: (i32, i32)) -> Value {
    let mut out = match kind {
        Some(k) => json!({ "ui": ui, "scale": scale, "inline": true, "kind": k }),
        None => json!({ "ui": ui, "scale": scale, "inline": true }),
    };
    let obj = out.as_object_mut().expect("刚 json! 出来的必是对象");
    if fit.0 > 0 {
        obj.insert("fit_w".into(), json!(fit.0));
    }
    if fit.1 > 0 {
        obj.insert("fit_h".into(), json!(fit.1));
    }
    out
}

/// 一次请求的结论。
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Ready(RawPng),
    /// 服务端「忙 / 超时」：过会儿再要。
    Busy,
    /// 服务端说画不了。
    Failed,
    /// 连不上服务端。
    Offline,
}

fn outcome(r: Result<Value, String>) -> Outcome {
    match r.map(|v| crate::theme_gallery::parse_receipt(&v)) {
        Ok(Receipt::Ready(raw)) => Outcome::Ready(raw),
        Ok(Receipt::Retry) => Outcome::Busy,
        Ok(Receipt::Failed) => Outcome::Failed,
        Err(e) => {
            tracing::debug!("外观预览取图失败：{e}");
            Outcome::Offline
        }
    }
}

/// 一次取图要比对的全部输入：草稿那几段、缩放 ×100、主题列表版本、编辑器推送次数、活跃方案、
/// 预览形态。
#[derive(Clone, Debug, PartialEq)]
struct Key {
    ui: Value,
    scale_pct: u32,
    themes_rev: u64,
    external_edits: u64,
    schema: String,
    /// 草稿明暗跟随系统时的系统切换次数；不跟随时恒 0（系统切了也不重取）。
    system_flips: u64,
    /// 预览形态（[`KIND_LABELS`] 的索引）。
    kind: usize,
}

/// 失败（画不出 / 连不上）时图区显示什么：屏上已有**本形态**的图就保留它（同形态换设置后
/// 失败的常态），否则落「无法生成」占位——`None` = 不动屏上的内容。
fn shown_on_failure(current: &Shown, shown_kind: usize, kind: usize) -> Option<Shown> {
    match current {
        Shown::Ready(_) if shown_kind == kind => None,
        _ => Some(Shown::Failed),
    }
}

/// 页内哨兵：每次更新时看外观页是否可见、输入是否变了，变了就重新计时取图；绘制时记下缩放。
struct Watcher {
    app: Rc<RefCell<AppState>>,
    net: Rc<NetTasks>,
    gallery: ThemeGallery,
    tab: Signal<usize>,
    page: usize,
    conn_failed: Signal<bool>,
    system_flips: Signal<u64>,
    shown: Signal<Shown>,
    error: Signal<String>,
    /// 最近一次发出去的输入；`None` = 下一次更新必发。
    sent: Rc<RefCell<Option<Key>>>,
    /// 「忙」后的重试：隔一会儿把它加一，逼下一次更新重发同一份输入。
    retry_tick: Signal<u64>,
    retried: Rc<Cell<u32>>,
    sent_tick: Cell<u64>,
    /// 最近一次是连不上：切回本页、连接恢复时重取。
    offline: Rc<Cell<bool>>,
    on_page: Cell<bool>,
    was_conn_failed: Cell<bool>,
    task: Rc<Cell<Option<TaskId>>>,
    scale: Cell<f32>,
    /// 预览形态（分段控件的选中段，与 [`build_card`] 共享同一信号）。
    kind: Signal<usize>,
    /// 屏上那张图属于哪个形态；刚清成「生成中 / 无法生成」时不算数，只认最近一次
    /// `Ready` 时的形态。
    shown_kind: Rc<Cell<usize>>,
}

impl Watcher {
    fn request(&self, key: &Key, scale: f32) {
        if let Some(t) = self.task.take() {
            self.net.cancel(t);
        }
        let (shown, error, task) = (self.shown, self.error, self.task.clone());
        let (retry_tick, retried, offline, net, shown_kind) = (
            self.retry_tick,
            self.retried.clone(),
            self.offline.clone(),
            self.net.clone(),
            self.shown_kind.clone(),
        );
        let params = request_params(
            &key.ui,
            kind_scale(key.kind, scale),
            kind_param(key.kind),
            kind_fit(key.kind),
        );
        let kind = key.kind;
        let id = self.net.spawn(
            move |cancel| {
                crate::theme_gallery::after_debounce(cancel, DEBOUNCE, || {
                    outcome(crate::rpc::call_core("theme.renderPreview", &params))
                })
            },
            move |r: Option<Option<Outcome>>| {
                task.set(None);
                let Some(r) = r.flatten() else { return };
                offline.set(r == Outcome::Offline);
                let failed = matches!(r, Outcome::Failed | Outcome::Offline);
                let msg = match r {
                    Outcome::Ready(raw) => {
                        retried.set(0);
                        shown_kind.set(kind);
                        shown.set(Shown::Ready(raw.into_png(scale)));
                        String::new()
                    }
                    Outcome::Busy if retried.get() < MAX_RETRIES => {
                        retried.set(retried.get() + 1);
                        net.spawn(
                            |cancel| {
                                crate::theme_gallery::after_debounce(cancel, RETRY_DELAY, || ())
                            },
                            move |_: Option<Option<()>>| retry_tick.update(|t| *t += 1),
                        );
                        "预览生成忙，稍后自动重试…".to_string()
                    }
                    Outcome::Busy => "预览生成忙，改动设置后会再试".to_string(),
                    Outcome::Failed => "暂时无法生成预览".to_string(),
                    // 不原地重发：提示一显示就会重绘，立刻重发会在服务端不在时空转。
                    Outcome::Offline => {
                        "暂时无法生成预览（输入法服务未连接），连接恢复后自动刷新".to_string()
                    }
                };
                if failed {
                    if let Some(s) =
                        shown_on_failure(&shown.with(Clone::clone), shown_kind.get(), kind)
                    {
                        shown.set(s);
                    }
                }
                if error.with(|e| *e != msg) {
                    error.set(msg);
                }
            },
        );
        self.task.set(Some(id));
    }
}

impl Widget for Watcher {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
        let on_page = self.tab.get() == self.page;
        let entered = on_page && !self.on_page.get();
        self.on_page.set(on_page);
        let conn_failed = self.conn_failed.get();
        let reconnected = self.was_conn_failed.get() && !conn_failed;
        self.was_conn_failed.set(conn_failed);
        // 上次连不上：切回本页或连接恢复时重取（不必等草稿变）。
        if self.offline.get() && (entered || reconnected) {
            self.sent.borrow_mut().take();
        }
        if !on_page {
            return;
        }
        let Ok(st) = self.app.try_borrow() else {
            return;
        };
        let draft = st.draft_config();
        drop(st);
        let scale = crate::theme_gallery::clamp_scale(self.scale.get());
        let key = Key {
            ui: draft_ui(&draft),
            scale_pct: crate::theme_gallery::scale_pct(scale),
            themes_rev: self.gallery.revision(),
            external_edits: crate::theme_gallery::external_edits(),
            schema: draft
                .get_str("schema.active")
                .unwrap_or_default()
                .to_string(),
            system_flips: if follows_system(draft.get_str("ui.theme.style")) {
                self.system_flips.get()
            } else {
                0
            },
            kind: self.kind.get(),
        };
        let tick = self.retry_tick.get();
        let retrying = tick != self.sent_tick.get();
        let prev_kind = self.sent.borrow().as_ref().map(|k| k.kind);
        if self.sent.borrow().as_ref() == Some(&key) && !retrying {
            return;
        }
        if !retrying {
            // 输入真变了：重试计数从头算。
            self.retried.set(0);
        }
        if prev_kind != Some(key.kind) {
            // 换形态：先回到「生成中」，不留另一个形态的旧图冒充新预览。
            self.shown.set(Shown::Waiting);
        }
        self.sent_tick.set(tick);
        self.request(&key, scale);
        *self.sent.borrow_mut() = Some(key);
    }

    fn paint(
        &self,
        _bounds: Rect,
        _content: Rect,
        _focused: bool,
        _enabled: bool,
        canvas: &mut dyn Canvas,
        _style: &Style,
    ) {
        self.scale.set(canvas.dpi_scale());
    }

    fn hit_opaque(&self) -> bool {
        false
    }
}

/// 图区（switcher + 定高底板）。抽出来给单测共用：单测在这里的原样结构上钉「换图之时
/// 没有任何节点挪动」。
///
/// 换图抖动的推理链（2026-11 真机反馈「改字体后预览往左抖一下」）：switcher 是**层叠
/// 容器**，新旧内容交叠期按两者宽度取大者、旧内容**左对齐**（Frame 子缺省 `Start`）；
/// 而 switcher 自己在底板里是居中摆放。曾给图片 `max_width(自然宽)`，旧图宽新图窄时
/// 交叠期整框比落定状态宽，新图先出现在偏左的位置、旧图淡出那一刻整框收成新宽向中间
/// 归位——于是「往左边抖一下，又回到正确位置」。修法是给两帧**恒定跨度**：全部
/// `width_match().height_match()` 填满底板内容区，任意状态层叠出的都是同一矩形，抖动
/// 无从谈起。
///
/// 两种形态**同一结构**、只有 fit 与绘制适配不同（[`frame_fit`]）：候选窗 `Fit::Contain`
/// 小图放大撑满、居中恒定；工具栏 `Fit::None`——draw_image 对所有 fit 都在 dst 内居中，
/// `None` 是 1 图片像素 = 1 逻辑 dp（不是物理 px；见 [`kind_scale`]：配 scale=1.0 的请求，
/// 屏上物理 = 自然逻辑 × DPI = 实际工具栏的物理大小）。（2026-11 真机反馈：放大到 588 宽
/// 后格子巨大，与实际使用的小横条差太远。）
/// 图区里的居中占位文字（生成中 / 生成失败）。
fn placeholder(text: &str) -> Element {
    Element::stack().width_match().height_match().child(
        Element::label(text.to_string())
            .font_size(12.0)
            .fg_role(Role::TextSubtle)
            .max_lines(1)
            .align(Align::Center),
    )
}

/// 形态 → 显示帧的图片适配：候选窗 Contain 恒定填满；工具栏 None 1:1 居中不放大。
fn frame_fit(kind: Signal<usize>) -> Fit {
    match kind.get() {
        1 => Fit::None,
        _ => Fit::Contain,
    }
}

fn preview_area(shown: Signal<Shown>, kind: Signal<usize>) -> Element {
    let image = Element::switcher(shown, move |s: Shown| match s {
        Shown::Ready(png) => Element::image_bytes(&png.bytes)
            .fit(frame_fit(kind.clone()))
            .width_match()
            .height_match(),
        Shown::Waiting => placeholder("预览生成中…"),
        Shown::Failed => placeholder("无法生成预览"),
    });
    Element::stack()
        .width_match()
        .height(AREA_H)
        .padding_xy(AREA_PAD, AREA_PAD)
        .bg_role(Role::SurfaceAlt)
        .corner(6.0)
        .child(image.height_match().align(Align::Center))
}

/// 「效果预览」卡片。
pub fn build_card(app: &Rc<RefCell<AppState>>, state: &AppState, pinned: Signal<bool>) -> Element {
    let shown: Signal<Shown> = signal(Shown::Waiting);
    let error: Signal<String> = signal(String::new());
    // 形态选中段：会话内记住即可（页面状态，不落盘配置）。
    let kind: Signal<usize> = signal(0);
    // 图钉（锁定 = 预览卡固定页顶、其余内容单独滚动）：会话内记住、不落盘。
    // 两态共用一枚 pin 图标，未锁定弱化色 / 锁定强调色，状态色区分。
    // 翻转走 AppState::set_appearance_pinned(同一时刻清 fields registry,理由见其文档),
    // 所以闭包捕获 app 而不是只拿信号句柄。
    let make_pin = move || {
        let app = app.clone();
        let role = if pinned.get() {
            Role::Accent
        } else {
            Role::TextSubtle
        };
        Element::row()
            .clickable()
            .on_click(move |_| {
                let v = !pinned.get();
                app.borrow().set_appearance_pinned(v);
            })
            .child(crate::icons::tinted_role(crate::icons::PIN, 14, role))
    };
    let area = preview_area(shown, kind.clone());
    let note = Element::label_signal(error)
        .font_size(12.0)
        .fg_role(Role::TextSubtle)
        .max_lines(2)
        .visible_when(move || !error.with(String::is_empty));
    let watcher = Element::leaf()
        .widget(Watcher {
            app: app.clone(),
            net: state.net.clone(),
            tab: state.tab,
            page: crate::cli::page_to_index("ui").unwrap_or(usize::MAX),
            gallery: state.theme_gallery.clone(),
            conn_failed: state.conn_failed,
            system_flips: state.system_theme_flips,
            shown,
            error,
            sent: Rc::new(RefCell::new(None)),
            retry_tick: signal(0),
            retried: Rc::new(Cell::new(0)),
            sent_tick: Cell::new(0),
            offline: Rc::new(Cell::new(false)),
            on_page: Cell::new(false),
            was_conn_failed: Cell::new(false),
            task: Rc::new(Cell::new(None)),
            scale: Cell::new(0.0),
            kind,
            shown_kind: Rc::new(Cell::new(0)),
        })
        .reactive()
        .width_match()
        .height(1);
    let body = Element::col()
        .width_match()
        .padding_xy(0, 14)
        .spacing(8)
        .child(watcher)
        .child(area)
        .child(note);
    // 卡头动作：形态分段控件（2026-11 真机反馈：从卡体顶部挪进卡头，不占图区上方的空间）。
    // section_impl 的三种标题写法各建一份，互相独立但绑同一枚 kind 信号，选中段一致
    // （同 manifest 头部「重置」按钮的现建模式）。
    let make_switcher =
        move || Element::segmented(KIND_LABELS.iter().map(|s| (*s).to_string()).collect(), kind);
    let card = crate::widgets::section_with_items_menu_after_title(
        SECTION_TITLE,
        &make_pin,
        &make_switcher,
        vec![crate::widgets::CardItem::Bare(crate::widgets::card_pad(
            body,
        ))],
        &|el: Element| el,
    );
    crate::search::anchor::wrap(card, ANCHOR, state.jump)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 布局坐标可测（见笔记）：在 [`preview_area`] 的原生结构上，把 `shown` 从一张图换到
    /// 另一张自然宽不同的图，**所有节点的矩形逐个相同**——恒定跨度这条规矩由真布局钉住，
    /// 不再靠上面那段推理链自证。
    #[test]
    fn swapping_images_moves_no_node() {
        use windui::core::Tree;
        use windui::geometry::Size;

        let shown = signal(Shown::Waiting);
        let mut tree = Tree::new();
        // 真实卡片里图区坐在 body 列里、定高生效；直接做根会被 layout_root 拉伸到窗高，
        // 测不出 200。同构地包一层列。
        let root = Element::col()
            .width_match()
            .child(preview_area(shown, signal(0)))
            .build(&mut tree);
        tree.root = Some(root);
        // 同一份 PNG 字节、两份独立分配的 `Vec`：哈希为空时按字节指针判等，指针不同才
        // 会触发 switcher 换内容——正是要测的两帧。
        let png = |w: u32| {
            RawPng {
                hash: String::new(),
                bytes: include_bytes!("mockdata/theme_previews/default.png").to_vec(),
                width: w,
                height: 64,
            }
            .into_png(1.0)
        };
        let rects = |tree: &Tree| -> Vec<windui::geometry::Rect> {
            let mut out = Vec::new();
            let mut stack = vec![tree.root.expect("已设根")];
            while let Some(id) = stack.pop() {
                if let Some(n) = tree.get(id) {
                    if n.bounds.w > 0 {
                        out.push(n.bounds);
                    }
                    stack.extend(n.children.iter().copied());
                }
            }
            out
        };
        let layout = |tree: &mut Tree| -> Vec<windui::geometry::Rect> {
            tree.layout_root(Size::new(600, 400), &mut windui::text::NullTextEngine);
            rects(tree)
        };
        let _waiting = layout(&mut tree);
        shown.set(Shown::Ready(png(240)));
        let a = layout(&mut tree);
        // 外框占满定高、图填满内容区（600 宽下内容区 584×184，即内边距 8）：外框/内容两
        // 个矩形都在，padding 与高度这两条就不只是数值断言，而是真布局的外在表现。
        assert!(
            a.iter().any(|r| r.h == AREA_H),
            "图区外框应占满定高 {}（{:?}）",
            AREA_H,
            a
        );
        assert!(
            a.iter().any(|r| r.h == AREA_H - 16 && r.w == 600 - 16),
            "换图后的图没填满图区内容区（{:?}）",
            a
        );
        shown.set(Shown::Ready(png(480)));
        let b = layout(&mut tree);
        assert_eq!(a, b, "换到自然宽不同的图不该挪动任何节点");
    }

    /// 工具栏形态：显示帧 1:1 不放大（[`frame_fit`] 给 `Fit::None`），且换图（不同自然宽）
    /// 同样**不挪动任何节点**——两帧同一矩形这条防左抖的前提在工具栏形态下原样成立。
    #[test]
    fn toolbar_frame_is_fit_none_and_swapping_moves_no_node() {
        use windui::core::Tree;
        use windui::geometry::Size;

        let (shown, kind) = (signal(Shown::Waiting), signal(1usize));
        let mut tree = Tree::new();
        let root = Element::col()
            .width_match()
            .child(preview_area(shown, kind))
            .build(&mut tree);
        tree.root = Some(root);
        let png = |w: u32| {
            RawPng {
                hash: String::new(),
                bytes: include_bytes!("mockdata/theme_previews/default.png").to_vec(),
                width: w,
                height: 64,
            }
            .into_png(1.0)
        };
        let rects = |tree: &Tree| -> Vec<windui::geometry::Rect> {
            let mut out = Vec::new();
            let mut stack = vec![tree.root.expect("已设根")];
            while let Some(id) = stack.pop() {
                if let Some(n) = tree.get(id) {
                    if n.bounds.w > 0 {
                        out.push(n.bounds);
                    }
                    stack.extend(n.children.iter().copied())
                }
            }
            out
        };
        let layout = |tree: &mut Tree| -> Vec<windui::geometry::Rect> {
            tree.layout_root(Size::new(600, 400), &mut windui::text::NullTextEngine);
            rects(tree)
        };
        let _waiting = layout(&mut tree);
        shown.set(Shown::Ready(png(240)));
        let a = layout(&mut tree);
        // 两形态同一结构：外框定高、帧填满内容区（584×184），工具栏不例外。
        assert!(
            a.iter().any(|r| r.h == AREA_H),
            "图区外框应占满定高 {}（{:?}）",
            AREA_H,
            a
        );
        assert!(
            a.iter().any(|r| r.h == AREA_H - 16 && r.w == 600 - 16),
            "工具栏图帧没填满图区内容区（{:?}）",
            a
        );
        shown.set(Shown::Ready(png(480)));
        let b = layout(&mut tree);
        assert_eq!(a, b, "工具栏形态换图同样不该挪动任何节点");
    }

    /// fit 常量对表真实布局：默认窗口尺寸下，图区内容区（图片/占位实际占的矩形）必须
    /// 恰好是 [`FIT_W`]×[`FIT_H`]。宽度没有可写的代码级推导链（窗口 − 主导航 − 二级导航 −
    /// 页边距 − 卡边距 − 图区内边距，散在四个文件里），在真树上量出来钉住；布局一动这里红。
    #[test]
    fn fit_consts_match_real_layout() {
        use windui::core::Tree;
        use windui::geometry::Size;
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let app = Rc::new(RefCell::new(AppState::empty()));
        let ui_page = crate::cli::page_to_index("ui").unwrap();
        let mut tree = Tree::new();
        let root = crate::shell::build(&app)
            .width(crate::MAIN_W)
            .height(crate::MAIN_H)
            .build(&mut tree);
        tree.root = Some(root);
        app.borrow().tab.set(ui_page);
        tree.layout_root(
            Size::new(crate::MAIN_W, crate::MAIN_H),
            &mut windui::text::NullTextEngine,
        );
        // 内容区矩形 = 图片/占位实际占的地方：全树找高恰好 FIT_H 的矩形。
        let mut hits = Vec::new();
        let mut stack = vec![tree.root.expect("已设根")];
        while let Some(id) = stack.pop() {
            let Some(n) = tree.get(id) else {
                continue;
            };
            let b = tree.abs_bounds(id);
            if b.w > 0 && b.h == FIT_H {
                hits.push((b.w, b.h));
            }
            stack.extend(n.children.iter().copied());
        }
        assert!(
            !hits.is_empty(),
            "图区内容区没找到：高 = FIT_H 的矩形一处都没有"
        );
        assert!(
            hits.iter().all(|&r| r == (FIT_W, FIT_H)),
            "图区内容区尺寸应为 {FIT_W}×{FIT_H}，实测 {hits:?}"
        );
    }

    #[test]
    fn params_carry_draft_ui_inline() {
        let ui = json!({ "candidate": { "font_size": 20 } });
        let p = request_params(&ui, 1.5, kind_param(0), (FIT_W, FIT_H));
        assert_eq!(p["fit_w"], FIT_W);
        assert_eq!(p["fit_h"], FIT_H);
        assert_eq!(p["ui"], ui);
        assert_eq!(p["scale"], 1.5);
        assert_eq!(p["inline"], true);
        assert!(p.get("kind").is_none(), "候选窗走协议缺省，不带 kind");
        assert!(p.get("name").is_none(), "主题取草稿里的 ui.theme.name");
    }

    #[test]
    fn params_carry_kind_only_for_toolbar() {
        let ui = json!({});
        assert_eq!(kind_param(0), None, "候选窗缺省");
        assert_eq!(kind_param(9), None, "越界按候选窗算");
        let t = request_params(&ui, 1.0, kind_param(1), kind_fit(1));
        assert_eq!(t["kind"], "toolbar");
        assert_eq!(t["inline"], true);
        assert!(
            t.get("fit_w").is_none() && t.get("fit_h").is_none(),
            "工具栏不带 fit 两维：core 回自然逻辑尺寸渲染"
        );
    }

    /// fit 与请求 scale 按形态切：候选窗带图区内容区尺寸 + 窗口 DPI，工具栏两维省键 +
    /// scale 固定 1.0（Fit::None 是 1 图片像素 = 1 逻辑 dp，配 1.0 才与实际工具栏物理同大，
    /// 详见 kind_scale 注释）；越界按候选窗。显示帧的适配同款二分（Contain / None）。
    #[test]
    fn fit_split_by_kind_request_and_frame() {
        assert_eq!(kind_fit(0), (FIT_W, FIT_H));
        assert_eq!(kind_fit(1), (0, 0), "工具栏不带 fit 两维");
        assert_eq!(kind_fit(9), (FIT_W, FIT_H), "越界按候选窗算");
        assert_eq!(kind_scale(0, 1.25), 1.25, "候选窗取窗口 DPI");
        assert_eq!(kind_scale(1, 1.25), 1.0, "工具栏固定 1.0，见 kind_scale");
        assert_eq!(kind_scale(9, 1.25), 1.25, "越界按候选窗算");
        assert_eq!(frame_fit(signal(0)), Fit::Contain);
        assert_eq!(frame_fit(signal(1)), Fit::None, "工具栏 1:1 不放大");
    }

    /// fit 目标框：两维都给、只给一维、都不给（= 旧版请求形状）三种形状都要对。
    #[test]
    fn params_carry_fit_only_when_positive() {
        let ui = json!({});
        let both = request_params(&ui, 1.0, None, (552, 184));
        assert_eq!(both["fit_w"], 552);
        assert_eq!(both["fit_h"], 184);
        let w_only = request_params(&ui, 1.0, None, (552, 0));
        assert_eq!(w_only["fit_w"], 552);
        assert!(w_only.get("fit_h").is_none(), "非正 = 这一维缺省");
        let h_only = request_params(&ui, 1.0, None, (0, 184));
        assert_eq!(h_only["fit_h"], 184);
        assert!(h_only.get("fit_w").is_none(), "非正 = 这一维缺省");
        let none = request_params(&ui, 1.0, None, (0, 0));
        assert!(
            none.get("fit_w").is_none() && none.get("fit_h").is_none(),
            "两维都缺省 = 旧版请求的形状"
        );
    }

    #[test]
    fn draft_ui_keeps_candidate_and_toolbar_sections() {
        let cfg = crate::config::Config::from_value(json!({ "ui": {
            "candidate": { "font_size": 20 },
            "toolbar": { "visible": true },
            "comment": { "enabled": false },
        }}));
        let ui = draft_ui(&cfg);
        assert_eq!(
            ui,
            json!({
                "candidate": { "font_size": 20 },
                "comment": { "enabled": false },
                "toolbar": { "visible": true },
            })
        );
    }

    #[test]
    fn receipts_map_to_outcomes() {
        assert_eq!(outcome(Err("连接 core 失败".into())), Outcome::Offline);
        let busy = outcome(Ok(json!({ "ok": false, "error": "预览渲染忙" })));
        assert_eq!(busy, Outcome::Busy);
        assert_eq!(
            outcome(Ok(json!({ "ok": false, "error": "x" }))),
            Outcome::Failed
        );
    }

    /// mock 认 `ui` 参数：主题取草稿里的 `ui.theme.name`。
    #[test]
    fn mock_renders_draft_theme() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let ui = json!({ "theme": { "name": "jade" } });
        let r = outcome(crate::rpc::call_core(
            "theme.renderPreview",
            &request_params(&ui, 1.0, None, (0, 0)),
        ));
        let jade = outcome(crate::rpc::call_core(
            "theme.renderPreview",
            &json!({ "name": "jade", "inline": true }),
        ));
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert_eq!(r, jade);
        assert!(matches!(r, Outcome::Ready(_)));
    }

    fn watcher(st: &Rc<RefCell<AppState>>) -> Watcher {
        let s = st.borrow();
        Watcher {
            app: st.clone(),
            net: s.net.clone(),
            gallery: s.theme_gallery.clone(),
            tab: s.tab,
            page: crate::cli::page_to_index("ui").unwrap(),
            conn_failed: s.conn_failed,
            system_flips: s.system_theme_flips,
            shown: signal(Shown::Waiting),
            error: signal(String::new()),
            sent: Rc::new(RefCell::new(None)),
            retry_tick: signal(0),
            retried: Rc::new(Cell::new(0)),
            sent_tick: Cell::new(0),
            offline: Rc::new(Cell::new(false)),
            on_page: Cell::new(false),
            was_conn_failed: Cell::new(false),
            task: Rc::new(Cell::new(None)),
            scale: Cell::new(1.0),
            kind: signal(0),
            shown_kind: Rc::new(Cell::new(0)),
        }
    }

    /// 什么时候发请求：只在本页；输入不变不重发；主题列表变、重试计时到、连不上之后切回本页或
    /// 连接恢复都重发。
    #[test]
    fn requests_only_when_inputs_change_or_recovery_is_due() {
        let st = Rc::new(RefCell::new(AppState::empty()));
        let mut w = watcher(&st);
        let update = |w: &mut Watcher| {
            windui::testing::run_with_ctx(|ctx| w.on_update(ctx));
            w.task.get()
        };
        let ui_page = crate::cli::page_to_index("ui").unwrap();
        st.borrow().tab.set(0);
        assert_eq!(update(&mut w), None, "别的页不取");

        st.borrow().tab.set(ui_page);
        let first = update(&mut w).expect("进本页即取");
        assert_eq!(update(&mut w), Some(first), "输入没变不重发");

        st.borrow().theme_gallery.invalidate();
        let second = update(&mut w).unwrap();
        assert_ne!(second, first, "主题列表变了要重取");

        w.retry_tick.update(|t| *t += 1);
        let third = update(&mut w).unwrap();
        assert_ne!(third, second, "重试计时到点，同一份输入也重发");

        // 连不上：留在本页不重发；切走再切回、或连接恢复，就重取。
        w.offline.set(true);
        assert_eq!(update(&mut w), Some(third));
        st.borrow().tab.set(0);
        update(&mut w);
        st.borrow().tab.set(ui_page);
        let fourth = update(&mut w).unwrap();
        assert_ne!(fourth, third, "切回本页重取");

        st.borrow().conn_failed.set(true);
        assert_eq!(update(&mut w), Some(fourth));
        st.borrow().conn_failed.set(false);
        assert_ne!(update(&mut w), Some(fourth), "连接恢复重取");
    }

    /// 草稿明暗跟随系统（缺省即是）：系统切了明暗就重取；显式选了亮 / 暗不重取；不在本页时不取，
    /// 切回来再取。
    /// 改工具栏设置项（items / vertical 这类）要重取：草稿 `ui.toolbar` 段进 Key.ui，
    /// 不再「改了不重取、应用设置才刷新」。改完回退，同样各重取一次。
    #[test]
    fn toolbar_draft_change_refetches() {
        let st = Rc::new(RefCell::new(AppState::empty()));
        let mut w = watcher(&st);
        let update = |w: &mut Watcher| {
            windui::testing::run_with_ctx(|ctx| w.on_update(ctx));
            w.task.get()
        };
        let ui_page = crate::cli::page_to_index("ui").unwrap();
        st.borrow().tab.set(ui_page);
        let first = update(&mut w).unwrap();
        assert_eq!(update(&mut w), Some(first), "输入没变不重发");
        // 写 current_config 的对应字段（写回器最终写的就是这些字段），与真实链路同构。
        st.borrow_mut()
            .current_config
            .set_path("ui.toolbar.vertical", json!(true));
        let second = update(&mut w).unwrap();
        assert_ne!(second, first, "改工具栏项要重取");
        assert_eq!(update(&mut w), Some(second), "同一份输入不重发");
        st.borrow_mut()
            .current_config
            .set_path("ui.toolbar.vertical", json!(false));
        let third = update(&mut w).unwrap();
        assert_ne!(third, second, "改回去也要重取");
    }

    #[test]
    fn system_theme_flip_refetches_only_when_following_system() {
        let st = Rc::new(RefCell::new(AppState::empty()));
        let mut w = watcher(&st);
        let update = |w: &mut Watcher| {
            windui::testing::run_with_ctx(|ctx| w.on_update(ctx));
            w.task.get()
        };
        let ui_page = crate::cli::page_to_index("ui").unwrap();
        st.borrow().tab.set(ui_page);
        let first = update(&mut w).unwrap();
        st.borrow().system_theme_flips.update(|n| *n += 1);
        let second = update(&mut w).unwrap();
        assert_ne!(second, first, "跟随系统：系统切了明暗要重取");
        assert_eq!(update(&mut w), Some(second), "只取一次");

        st.borrow().tab.set(0);
        st.borrow().system_theme_flips.update(|n| *n += 1);
        update(&mut w);
        assert_eq!(w.task.get(), Some(second), "不在本页不取");
        st.borrow().tab.set(ui_page);
        let third = update(&mut w).unwrap();
        assert_ne!(third, second, "切回本页补取");

        st.borrow_mut()
            .current_config
            .set_path("ui.theme.style", json!("dark"));
        let fourth = update(&mut w).unwrap();
        assert_ne!(fourth, third, "改了明暗本身要重取");
        st.borrow().system_theme_flips.update(|n| *n += 1);
        assert_eq!(update(&mut w), Some(fourth), "显式暗色：系统切换不重取");
    }

    /// 换形态要重取，且图区先回到「生成中」——不留另一个形态的旧图冒充；换回候选窗同样重取。
    #[test]
    fn switching_kind_refetches_and_resets_shown() {
        let st = Rc::new(RefCell::new(AppState::empty()));
        let mut w = watcher(&st);
        let update = |w: &mut Watcher| {
            windui::testing::run_with_ctx(|ctx| w.on_update(ctx));
            w.task.get()
        };
        let ui_page = crate::cli::page_to_index("ui").unwrap();
        st.borrow().tab.set(ui_page);
        let first = update(&mut w).unwrap();
        // 模拟候选窗图已就绪：图区真从 Ready 出发，换形态的清屏才有东西可清。
        w.shown_kind.set(0);
        w.shown.set(Shown::Ready(
            RawPng {
                hash: String::new(),
                bytes: include_bytes!("mockdata/theme_previews/default.png").to_vec(),
                width: 240,
                height: 64,
            }
            .into_png(1.0),
        ));
        w.kind.set(1);
        let second = update(&mut w).unwrap();
        assert_ne!(second, first, "换到工具栏要重取");
        assert_eq!(
            w.shown.get(),
            Shown::Waiting,
            "换形态先回到生成中，不留候选窗旧图"
        );
        w.kind.set(0);
        let third = update(&mut w).unwrap();
        assert_ne!(third, second, "换回候选窗也重取");
        assert_eq!(update(&mut w), Some(third), "形态不变不重发");
    }

    /// 失败时图区显示什么：本形态已有图就保留，否则落「无法生成」占位——包括刚换形态清成
    /// 「生成中」的情形，不许别的形态旧图或「生成中」占位卡死在屏上。
    #[test]
    fn failure_keeps_same_kind_image_otherwise_placeholder() {
        let png = Shown::Ready(
            RawPng {
                hash: String::new(),
                bytes: include_bytes!("mockdata/theme_previews/default.png").to_vec(),
                width: 240,
                height: 64,
            }
            .into_png(1.0),
        );
        // 本形态旧图在：保留（换设置后失败是常态）。
        assert_eq!(shown_on_failure(&png, 0, 0), None);
        // 别的形态旧图 / 生成中 / 已是占位：都落「无法生成」。
        assert_eq!(shown_on_failure(&png, 0, 1), Some(Shown::Failed));
        assert_eq!(shown_on_failure(&Shown::Waiting, 0, 0), Some(Shown::Failed));
        assert_eq!(shown_on_failure(&Shown::Failed, 1, 1), Some(Shown::Failed));
    }

    /// 图钉锁定的布局分叉(真树):未锁定 = 整页滚动(预览卡锚点向上第一个 Scroll 先祖
    /// 是页面滚动容器);锁定 = 预览卡不在任何滚动子树里(固定区),清单行锚点向上的
    /// 第一个 Scroll 是独立滚动容器、且不包含预览卡;解锁恢复原结构。
    #[test]
    fn pin_lock_changes_scroll_layout_and_unlock_restores() {
        use std::cell::RefCell;
        use windui::core::Tree;
        use windui::geometry::Size;

        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        crate::search::anchor::reset_anchor_ids();
        let app = Rc::new(RefCell::new(AppState::empty()));
        let mut tree = Tree::new();
        let (w, h) = (crate::MAIN_W, crate::MAIN_H);
        let root = crate::shell::build(&app)
            .width(w)
            .height(h)
            .build(&mut tree);
        tree.root = Some(root);
        let st = app.borrow();
        st.tab.set(crate::cli::page_to_index("ui").unwrap());
        let layout = |t: &mut windui::core::Tree| {
            t.layout_root(Size::new(w, h), &mut windui::text::NullTextEngine);
        };
        layout(&mut tree);
        let aid = crate::search::anchor::anchor_node_id(&tree, "ui:appearance-preview")
            .expect("预览卡锚点应已挂载");
        let first_scroll =
            |t: &windui::core::Tree, id: windui::core::NodeId| -> Option<windui::core::NodeId> {
                let mut cur = t.get(id).and_then(|n| n.parent);
                while let Some(i) = cur {
                    let n = t.get(i)?;
                    if matches!(n.layout, windui::core::Layout::Scroll) {
                        return Some(i);
                    }
                    cur = n.parent;
                }
                None
            };
        let page_scroll = first_scroll(&tree, aid).expect("未锁定时预览卡应有滚动先祖");
        let contains = |t: &windui::core::Tree,
                        root: windui::core::NodeId,
                        target: windui::core::NodeId|
         -> bool {
            let mut stack = vec![root];
            while let Some(i) = stack.pop() {
                if i == target {
                    return true;
                }
                if let Some(n) = t.get(i) {
                    stack.extend(n.children.iter().copied());
                }
            }
            false
        };
        // 锁定:骨架重建(switcher),两帧让新子树落地。
        st.set_appearance_pinned(true);
        layout(&mut tree);
        layout(&mut tree);
        layout(&mut tree);
        let aid2 = crate::search::anchor::anchor_node_id(&tree, "ui:appearance-preview")
            .expect("重建后预览卡锚点应重新登记");
        assert!(
            first_scroll(&tree, aid2).is_none(),
            "锁定后预览卡不应在任何滚动子树里"
        );
        let row_anchor = crate::search::anchor::anchor_node_id(&tree, "ui.toolbar.vertical")
            .or_else(|| crate::search::anchor::anchor_node_id(&tree, "ui.candidate.font_size"))
            .expect("外观页应有清单行锚点");
        let inner = first_scroll(&tree, row_anchor).expect("锁定后清单行应有独立滚动容器");
        assert_ne!(inner, page_scroll, "独立滚动容器应是新节点");
        assert!(!contains(&tree, inner, aid2), "独立滚动容器不应包含预览卡");
        // 解锁:恢复原结构。
        st.set_appearance_pinned(false);
        st.fields.reset();
        layout(&mut tree);
        layout(&mut tree);
        let aid3 = crate::search::anchor::anchor_node_id(&tree, "ui:appearance-preview")
            .expect("解锁后预览卡锚点应重新登记");
        assert_eq!(
            first_scroll(&tree, aid3),
            Some(page_scroll),
            "解锁后预览卡应回到原页面滚动容器"
        );
    }

    #[test]
    fn follows_system_matches_core_parsing() {
        for s in [
            None,
            Some(""),
            Some("system"),
            Some("auto"),
            Some(" System "),
        ] {
            assert!(follows_system(s), "{s:?}");
        }
        for s in [Some("light"), Some(" Dark ")] {
            assert!(!follows_system(s), "{s:?}");
        }
    }
}
