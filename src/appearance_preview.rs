//! 外观页顶部的「效果预览」：草稿配置里影响候选窗的那几段 `ui` 子树交给 core 出一张候选窗
//! 预览（`theme.renderPreview` 的 `ui` 参数，协议见 WindInput `docs/design/wind-scene-extraction.md`
//! §7.2），改了字号、字体、每页候选数这类设置，不必保存就能看到效果。
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
//! - **形态**：卡内分段控件在「候选窗 / 工具栏」间切换（`theme.renderPreview` 的 `kind`
//!   参数，候选窗走协议缺省），选中段会话内记住、不落盘。换形态先回到「生成中」占位，
//!   图画不出时落「无法生成」——不留另一个形态的旧图冒充新预览。

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
use crate::theme_gallery::{GalleryEntry, Png, RawPng, Receipt, ThemeGallery};

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
/// 草稿里影响候选窗外观的段（`ui.<段>`）；工具栏、状态提示等改了不重取。
pub const DRAFT_SECTIONS: &[&str] = &["theme", "candidate", "font", "comment"];

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

/// 一次请求要带的参数：草稿 `ui` 子树 + 缩放 + 形态（候选窗为协议缺省、省略）。
pub fn request_params(ui: &Value, scale: f32, kind: Option<&str>) -> Value {
    match kind {
        Some(k) => json!({ "ui": ui, "scale": scale, "inline": true, "kind": k }),
        None => json!({ "ui": ui, "scale": scale, "inline": true }),
    }
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
        let params = request_params(&key.ui, scale, kind_param(key.kind));
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
/// 无从谈起。代价是 `Fit::Contain` 会把小图放大（Contain 不区分缩小与放大，缓存
/// `skia::draw_image`：`s = min(pw/iw, ph/ih)`），换来带装饰主题显示更大、居中恒定。
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

fn preview_area(shown: Signal<Shown>) -> Element {
    let image = Element::switcher(shown, |s: Shown| match s {
        Shown::Ready(png) => Element::image_bytes(&png.bytes)
            .fit(Fit::Contain)
            .width_match()
            .height_match(),
        Shown::Waiting => placeholder("预览生成中…"),
        Shown::Failed => placeholder("无法生成预览"),
    });
    Element::stack()
        .width_match()
        .height(AREA_H)
        .padding_xy(8, 8)
        .bg_role(Role::SurfaceAlt)
        .corner(6.0)
        .child(image.height_match().align(Align::Center))
}

/// 当前主题的显示名：选中索引在取值表里查出 id，再在画廊清单里换成显示名。
/// 显示名缺了退 id；索引/取值表拿不到（画廊没建、清单没到、越界）退配置值；再没有退
/// `None`（链接只显示「主题选择」）。
fn display_name(
    idx: Option<usize>,
    values: &[String],
    entries: &[GalleryEntry],
    cfg: Option<&str>,
) -> Option<String> {
    let id = idx
        .and_then(|i| values.get(i))
        .map(String::as_str)
        .or(cfg)?;
    Some(
        entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.display.as_str())
            .unwrap_or(id)
            .to_string(),
    )
}

/// 链接文案：有名字带上，没有就只留「主题选择」。
fn theme_link_label(name: Option<&str>) -> String {
    match name {
        Some(n) if !n.is_empty() => format!("主题选择：{n}"),
        _ => "主题选择".to_string(),
    }
}

/// 链接落点：主题页「主题选择」卡里 `ui.theme.name` 那一行（行锚点即清单 key）。
///
/// `pub(crate)` 是给 `search::anchor` 的守门测试用的：手写目标不经 `build_index`
/// 派生，不入比对集就会在清单键改名时静默退化成「只切页、不滚不闪」。
pub(crate) fn theme_link_target() -> crate::search::index::JumpTarget {
    crate::search::index::JumpTarget::Row {
        tab: crate::cli::page_to_index("theme").unwrap_or(usize::MAX),
        anchor: "ui.theme.name".to_string(),
    }
}

/// 卡头链接的反应式文案 + 驱动它的 reactive_effect 挂件（要一并挂树）。
///
/// 预览图跟的是草稿，草稿里的主题跟画廊选中走（点格即变、点「应用」才落盘），名字也
/// 跟同一处走：选中索引、取值表、画廊清单三个信号任一变化都重算，预览与标题永远同一
/// 张脸。三个源没有组合派生信号的现成机制（`Signal::map` 只跟单源），用 reactive_effect
/// 把重算结果写进本地信号——这也是方案页启停等处同款的做法。
fn theme_display(state: &AppState) -> (Signal<String>, Element) {
    let label = signal("主题选择".to_string());
    let idx = state
        .fields
        .select_indices
        .borrow()
        .get("ui.theme.name")
        .copied();
    let (values, entries) = (state.theme_value_sig, state.theme_gallery.entries);
    // 画廊缺席时的退路：配置此刻的值（id）。签名在本地，不会自己变，读一次就够。
    let cfg_name = state
        .current_config
        .get_str("ui.theme.name")
        .map(str::to_string);
    let apply = move || {
        let name = display_name(
            idx.map(|s| s.get()),
            &values.get(),
            &entries.get(),
            cfg_name.as_deref(),
        );
        label.set(theme_link_label(name.as_deref()));
    };
    apply();
    let version_of = move || {
        // 三源无先后地混在一起，各乘一个奇素数再相加，避免「你加一我也加一」相抵。
        idx.map_or(0, |s| s.version())
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(values.version().wrapping_mul(0xBF58_476D_1CE4_E5B9))
            .wrapping_add(entries.version())
    };
    let eff = crate::pages::dict::reactive::reactive_effect(version_of, apply);
    (label, eff)
}

/// 卡头右侧的「主题选择：〈当前主题名〉」链接：点击回主题页画廊（选中主题所在行）。
/// `request_quiet`：人是自己点过去的，落定不闪。
fn theme_link(label: Signal<String>, state: &AppState) -> Element {
    let (tab, jump) = (state.tab, state.jump);
    let target = theme_link_target();
    Element::link(label)
        .underline(false)
        .on_click(move |_| crate::search::jump::request_quiet(tab, jump, target.clone()))
}

/// 「效果预览」卡片。
pub fn build_card(app: &Rc<RefCell<AppState>>, state: &AppState) -> Element {
    let shown: Signal<Shown> = signal(Shown::Waiting);
    let error: Signal<String> = signal(String::new());
    // 形态选中段：会话内记住即可（页面状态，不落盘配置）。
    let kind: Signal<usize> = signal(0);
    let switcher = Element::segmented(KIND_LABELS.iter().map(|s| (*s).to_string()).collect(), kind);
    let area = preview_area(shown);
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
        .child(switcher)
        .child(area)
        .child(note);
    // 卡头动作：反应式驱动挂件与链接同一处现建。section_impl 的三种标题写法各建一份，
    // 互相独立但读同一批源信号，文案一致（同 manifest 头部「重置」按钮的现建模式）。
    let make_link = || {
        let (label, eff) = theme_display(state);
        Element::stack().child(eff).child(theme_link(label, state))
    };
    let card = crate::widgets::section_with_items_menu(
        SECTION_TITLE,
        &make_link,
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
            .child(preview_area(shown))
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

    /// 显示名逐层退让：索引→id→显示名，每层缺了退上一层，全缺退 `None`。
    #[test]
    fn display_name_falls_back_layer_by_layer() {
        let entries = vec![
            GalleryEntry {
                id: "default".into(),
                display: "清风".into(),
                ..Default::default()
            },
            GalleryEntry {
                id: "mono".into(),
                display: "墨".into(),
                ..Default::default()
            },
        ];
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // 索引 → id → 显示名。
        assert_eq!(
            display_name(Some(1), &ids(&["default", "mono"]), &entries, None).as_deref(),
            Some("墨")
        );
        // 清单里没有这个 id（清单没到/陌生取值）→ id 顶着。
        assert_eq!(
            display_name(Some(0), &ids(&["jade"]), &entries, None).as_deref(),
            Some("jade")
        );
        // 索引拿不到（画廊没建/被藏）→ 配置值，且照样查显示名。
        assert_eq!(
            display_name(None, &ids(&["default"]), &entries, Some("mono")).as_deref(),
            Some("墨")
        );
        // 索引越界（列表刚整份替换）→ 配置值；配置值也不在清单里 → 裸 id 顶着。
        assert_eq!(
            display_name(Some(9), &ids(&["default"]), &entries, Some("mono")).as_deref(),
            Some("墨")
        );
        assert_eq!(
            display_name(Some(9), &ids(&["default"]), &entries, Some("jade")).as_deref(),
            Some("jade")
        );
        // 全没有 → 只显示「主题选择」。
        assert_eq!(display_name(None, &[], &entries, None), None);
    }

    #[test]
    fn theme_link_label_carries_name() {
        assert_eq!(theme_link_label(Some("墨")), "主题选择：墨");
        assert_eq!(theme_link_label(None), "主题选择");
        assert_eq!(theme_link_label(Some("")), "主题选择");
    }

    /// 链接落点 = 主题页「主题选择」卡里 `ui.theme.name` 那行（行锚点即清单 key，与
    /// manifest 建行时挂的锚点同一把键）。
    #[test]
    fn theme_link_targets_the_gallery_row() {
        match theme_link_target() {
            crate::search::index::JumpTarget::Row { tab, anchor } => {
                assert_eq!(tab, crate::cli::page_to_index("theme").unwrap());
                assert_eq!(anchor, "ui.theme.name");
            }
            other => panic!("落点应是行锚点，实为 {other:?}"),
        }
    }

    /// 反应式接线：选中索引一变，链接文案跟着变。走真树 + `layout_root`——
    /// `dispatch_reactive_updates` 是 reactive_effect 唯一的驱动点，只调闭包的话
    /// 「有没有真挂上 `.reactive()`」这一半就没人管（同 auto_encode 的测法）。
    #[test]
    fn theme_link_label_follows_selection() {
        use windui::core::Tree;
        use windui::geometry::Size;
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let st = AppState::empty();
        st.theme_value_sig
            .set(vec!["default".to_string(), "mono".to_string()]);
        st.theme_gallery.entries.set(vec![
            GalleryEntry {
                id: "default".into(),
                display: "清风".into(),
                ..Default::default()
            },
            GalleryEntry {
                id: "mono".into(),
                display: "墨".into(),
                ..Default::default()
            },
        ]);
        let idx = signal(0usize);
        st.fields
            .select_indices
            .borrow_mut()
            .insert("ui.theme.name".to_string(), idx);
        let (label, eff) = theme_display(&st);
        let mut tree = Tree::new();
        let id = eff.build(&mut tree);
        tree.root = Some(id);
        assert_eq!(label.get(), "主题选择：清风", "构建时即算出当前文案");
        idx.set(1);
        tree.layout_root(Size::new(100, 100), &mut windui::text::NullTextEngine);
        assert_eq!(
            label.get(),
            "主题选择：墨",
            "选中索引变化应经 reactive_effect 触达链接文案"
        );
    }

    #[test]
    fn params_carry_draft_ui_inline() {
        let ui = json!({ "candidate": { "font_size": 20 } });
        let p = request_params(&ui, 1.5, kind_param(0));
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
        let t = request_params(&ui, 1.0, kind_param(1));
        assert_eq!(t["kind"], "toolbar");
        assert_eq!(t["inline"], true);
    }

    #[test]
    fn draft_ui_keeps_only_candidate_window_sections() {
        let cfg = crate::config::Config::from_value(json!({ "ui": {
            "candidate": { "font_size": 20 },
            "toolbar": { "visible": true },
            "comment": { "enabled": false },
        }}));
        let ui = draft_ui(&cfg);
        assert_eq!(
            ui,
            json!({ "candidate": { "font_size": 20 }, "comment": { "enabled": false } })
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
            &request_params(&ui, 1.0, None),
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
