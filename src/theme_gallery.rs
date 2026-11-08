//! 「主题」页的已装主题画廊与主题包导入框里的预览图。
//!
//! 缩略图由服务端用实时候选窗的渲染器出 PNG（`theme.renderPreview(s)`，协议见 WindInput
//! `docs/design/wind-scene-extraction.md` §7.2），像素与实机一致；设置端只负责取图、摆格子。
//!
//! - **取图不占 UI 线程**：RPC 与读缓存文件都在 [`NetTasks`] 的工作线程里做，结果回 UI 线程
//!   只写一个信号。
//! - **防抖**：外观相关配置变更、主题增删都只是「标脏」，由页内一枚哨兵在本页可见时发起
//!   取图；工作线程先等 [`DEBOUNCE`] 再发 RPC，期间再来一次就取消旧的重排。连续改几项
//!   字号 / 颜色只换来一次往返（服务端有缓存，没变的主题直接命中）。
//! - **失败不打扰**：服务端不在、回执 `ok:false`、文件读不到，都只让那一格显示占位文字。

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use windui::core::{EventCtx, Widget};
use windui::event::MenuItem;
use windui::geometry::Rect;
use windui::prelude::*;
use windui::render::Canvas;
use windui::style::Style;

use crate::net_task::{NetTasks, TaskId};
use crate::pages::dict::reactive::Feedback;
use crate::state::{AppState, FieldRegistry};

/// 画廊卡片的标题（二级导航里也叫这个）。
pub const SECTION_TITLE: &str = "已装主题";

/// 一格的目标宽度（逻辑 px）：列数按可用宽度取最接近这个宽度的那一档，各列等分。
const CELL_W: i32 = 260;
/// 格与格之间的间距。
const GAP: i32 = 12;
/// 格内边距：图与名字都缩进这么多。
const CELL_PAD: i32 = 10;
/// 格子描边宽度（当前主题那格换成强调色）。
const RING: i32 = 2;
/// 图区高度：固定，免得横排 / 竖排主题混排时一行格子高低不齐。
const THUMB_H: i32 = 120;
/// 防抖时长：标脏到真正发 RPC 之间至少隔这么久。
const DEBOUNCE: Duration = Duration::from_millis(300);
/// `theme.renderPreviews` 一次最多几条（服务端 `MAX_BATCH`）。
const MAX_BATCH: usize = 64;
/// 导入框里预览图的最大高度。
const IMPORT_PREVIEW_MAX_H: i32 = 160;

/// 画廊里的一个已装主题。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GalleryEntry {
    pub id: String,
    pub display: String,
    pub builtin: bool,
}

/// 一格的图。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Thumb {
    /// 还没取到（首轮在途）。
    Pending,
    Ready(Png),
    /// 取不到：服务端不可用、回执失败或文件读不到。
    Failed,
}

/// 一张取到手的预览图。`width` / `height` 是物理像素，`scale` 是请求时的缩放。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Png {
    pub hash: String,
    pub bytes: Rc<[u8]>,
    pub width: u32,
    pub height: u32,
    /// 缩放 ×100（存整数好派生 `Eq` / `Hash`）。
    pub scale_pct: u32,
}

/// 工作线程交回的一张图（`Rc` 不能跨线程，回到 UI 线程再转成 [`Png`]）。
#[derive(Debug, PartialEq, Eq)]
pub struct RawPng {
    pub hash: String,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl RawPng {
    fn into_png(self, scale: f32) -> Png {
        Png {
            hash: self.hash,
            bytes: self.bytes.into(),
            width: self.width,
            height: self.height,
            scale_pct: scale_pct(scale),
        }
    }
}

fn scale_pct(scale: f32) -> u32 {
    (scale * 100.0).round().max(1.0) as u32
}

/// 服务端接受的缩放区间是 0.5–4；探针还没画过时按 1。
fn clamp_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale.clamp(0.5, 4.0)
    } else {
        1.0
    }
}

/// 这个配置键变了，画廊要不要重取：主题本身、候选窗外观与字体都会改像素。
pub fn affects_previews(key: &str) -> bool {
    ["ui.theme", "ui.candidate", "ui.font"]
        .iter()
        .any(|p| key == *p || key.strip_prefix(p).is_some_and(|r| r.starts_with('.')))
}

/// 解析一条回执。有内联 `png` 就直接用，否则按 `path` 读缓存文件；`ok` 不为真、两者都取不到
/// 时为 `None`。宽高缺失时从 PNG 头里取。
pub fn parse_receipt(v: &Value, read: &dyn Fn(&str) -> Option<Vec<u8>>) -> Option<RawPng> {
    use base64::Engine;
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let bytes = match v.get("png").and_then(Value::as_str) {
        Some(b64) => base64::engine::general_purpose::STANDARD.decode(b64).ok()?,
        None => read(v.get("path").and_then(Value::as_str)?)?,
    };
    let dim = |k: &str| v.get(k).and_then(Value::as_u64).map(|n| n as u32);
    let (width, height) = match (dim("width"), dim("height")) {
        (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
        _ => png_size(&bytes)?,
    };
    Some(RawPng {
        hash: v
            .get("hash")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        bytes,
        width,
        height,
    })
}

/// PNG 头（IHDR）里的宽高。不是 PNG 时为 `None`。
fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 24 || &b[..8] != b"\x89PNG\r\n\x1a\n" || &b[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(b[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(b[20..24].try_into().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// 解析批量回执：与请求同序，缺条或失败的位置为 `None`。
pub fn parse_batch(
    v: &Value,
    n: usize,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Vec<Option<RawPng>> {
    let items = v.get("items").and_then(Value::as_array);
    (0..n)
        .map(|i| {
            items
                .and_then(|a| a.get(i))
                .and_then(|r| parse_receipt(r, read))
        })
        .collect()
}

fn read_file(path: &str) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

/// 先等 `delay`（每 10 ms 查一次取消），没被取消才跑 `work`。被取消时为 `None`。
pub fn after_debounce<T>(
    cancel: &AtomicBool,
    delay: Duration,
    work: impl FnOnce() -> T,
) -> Option<T> {
    let step = Duration::from_millis(10);
    let mut left = delay;
    while !left.is_zero() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let d = left.min(step);
        std::thread::sleep(d);
        left -= d;
    }
    (!cancel.load(Ordering::Relaxed)).then(work)
}

/// 整批取图（工作线程里跑）。整批 RPC 失败（服务端不在等）时为 `Err`。
fn fetch_batch(ids: &[String], scale: f32) -> Result<Vec<Option<RawPng>>, String> {
    let mut out = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(MAX_BATCH) {
        let items: Vec<Value> = chunk
            .iter()
            .map(|id| json!({ "name": id, "scale": scale }))
            .collect();
        let v = crate::rpc::call_core("theme.renderPreviews", &json!({ "items": items }))?;
        out.extend(parse_batch(&v, chunk.len(), &read_file));
    }
    Ok(out)
}

/// 把一轮取图的结果落成「主题 id → 图」。整批失败时每格都是 [`Thumb::Failed`]。
pub fn thumbs_from(
    ids: &[String],
    result: Option<Result<Vec<Option<RawPng>>, String>>,
    scale: f32,
) -> HashMap<String, Thumb> {
    let mut items = match result {
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            tracing::debug!("主题预览取图失败：{e}");
            Vec::new()
        }
        None => Vec::new(),
    }
    .into_iter();
    ids.iter()
        .map(|id| {
            let t = match items.next().flatten() {
                Some(raw) => Thumb::Ready(raw.into_png(scale)),
                None => Thumb::Failed,
            };
            (id.clone(), t)
        })
        .collect()
}

/// 图在格子里的显示尺寸（逻辑 px）：按宽缩放、保持比例，不放大超过原图的逻辑尺寸，
/// 并收进 `max_w × max_h`。
pub fn fit_size(png: &Png, max_w: i32, max_h: i32) -> (i32, i32) {
    let s = png.scale_pct.max(1) as f32 / 100.0;
    let (nw, nh) = (png.width as f32 / s, png.height as f32 / s);
    if nw <= 0.0 || nh <= 0.0 {
        return (0, 0);
    }
    let k = (max_w as f32 / nw).min(max_h as f32 / nh).min(1.0);
    (
        ((nw * k).round() as i32).max(1),
        ((nh * k).round() as i32).max(1),
    )
}

/// 可用宽度 `w`（逻辑 px）下分几列：让每格最接近 [`CELL_W`]，至少一列。
pub fn cols_for(w: i32) -> usize {
    (((w + GAP) as f32 / (CELL_W + GAP) as f32).round() as usize).max(1)
}

/// 图的逻辑尺寸（物理像素 ÷ 请求时的缩放）。
pub fn natural_size(png: &Png) -> (i32, i32) {
    let s = png.scale_pct.max(1) as f32 / 100.0;
    (
        (png.width as f32 / s).round() as i32,
        (png.height as f32 / s).round() as i32,
    )
}

/// 画廊状态。克隆即共享（内部全是信号与 `Rc`）。
#[derive(Clone)]
pub struct ThemeGallery {
    net: Rc<NetTasks>,
    pub entries: Signal<Vec<GalleryEntry>>,
    pub thumbs: Signal<HashMap<String, Thumb>>,
    /// 网格的重建键：主题列表 + 列数。列数只在跨档时变，拖窗口改宽度不逐像素重建。
    layout: Signal<(Vec<GalleryEntry>, usize)>,
    /// 每标脏一次加一：哨兵据此知道要重取（信号写入也负责唤起下一帧）。
    rev: Signal<u64>,
    /// 在途的取图任务。
    task: Rc<Cell<Option<TaskId>>>,
}

impl ThemeGallery {
    pub fn new(net: Rc<NetTasks>) -> Self {
        Self {
            net,
            entries: signal(Vec::new()),
            thumbs: signal(HashMap::new()),
            layout: signal((Vec::new(), 3)),
            rev: signal(0),
            task: Rc::new(Cell::new(None)),
        }
    }

    /// 换上新的主题列表（导入 / 删除之后经 `AppState::apply_theme_entries` 调）。
    pub fn set_entries(&self, entries: Vec<GalleryEntry>) {
        if self.entries.with(|e| *e != entries) {
            self.entries.set(entries.clone());
            self.layout.update(|l| l.0 = entries);
            self.invalidate();
        }
    }

    fn set_cols(&self, cols: usize) {
        if self.layout.with(|l| l.1 != cols) {
            self.layout.update(|l| l.1 = cols);
        }
    }

    /// 标脏：下一次本页可见时（防抖后）整批重取。
    pub fn invalidate(&self) {
        self.rev.update(|r| *r += 1);
    }

    /// 发起一轮取图（取消上一轮）。工作线程先防抖再发 RPC。
    fn fetch(&self, scale: f32) {
        if let Some(t) = self.task.take() {
            self.net.cancel(t);
        }
        let ids: Vec<String> = self
            .entries
            .with(|e| e.iter().map(|x| x.id.clone()).collect());
        if ids.is_empty() {
            self.thumbs.set(HashMap::new());
            return;
        }
        let (thumbs, task) = (self.thumbs, self.task.clone());
        let ids_done = ids.clone();
        let id = self.net.spawn(
            move |cancel| after_debounce(cancel, DEBOUNCE, || fetch_batch(&ids, scale)),
            move |r: Option<Option<Result<Vec<Option<RawPng>>, String>>>| {
                task.set(None);
                thumbs.set(thumbs_from(&ids_done, r.flatten(), scale));
            },
        );
        self.task.set(Some(id));
    }
}

/// 页内哨兵（整宽、1 px 高、不画东西）：绘制时记下窗口所在屏的缩放、按自己的宽度定列数；
/// 本页可见且标脏过或缩放变了，就发起一轮取图。不可见时只攒着，不替用户没打开的页去渲染。
struct Driver {
    gallery: ThemeGallery,
    tab: Signal<usize>,
    page: usize,
    /// 最近一次绘制时画布的缩放（0 = 还没画过）。
    painted: Cell<f32>,
    /// 已按哪一版、哪个缩放发过请求。
    fetched: Option<(u64, f32)>,
}

impl Widget for Driver {
    fn on_update(&mut self, ctx: &mut EventCtx) {
        if self.tab.get() != self.page {
            return;
        }
        // 上一帧排出来的宽度（on_update 在 measure 之前跑）。没排过时为 0，先按默认列数。
        let id = ctx.id();
        let w = ctx.tree_mut().get(id).map_or(0, |n| n.bounds.w);
        if w > 0 {
            self.gallery.set_cols(cols_for(w));
        }
        let want = (self.gallery.rev.get(), clamp_scale(self.painted.get()));
        if self.fetched != Some(want) {
            self.fetched = Some(want);
            self.gallery.fetch(want.1);
        }
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
        let s = canvas.dpi_scale();
        let before = clamp_scale(self.painted.get());
        self.painted.set(s);
        // 缩放变了（首帧发现不是 1、拖到别的屏）：标脏好让下一帧的 on_update 按新缩放重取。
        // 首帧前按 1 取的那轮还在防抖里，会被这一轮取消，不多一次渲染。
        if (clamp_scale(s) - before).abs() > f32::EPSILON {
            self.gallery.invalidate();
        }
    }

    fn hit_opaque(&self) -> bool {
        false
    }
}

/// 画廊卡片：网格 + 页内哨兵。
pub fn build_card(state: &AppState) -> Element {
    let gallery = state.theme_gallery.clone();
    let ctx = CellCtx {
        fields: state.fields.clone(),
        values: state.theme_value_sig,
        thumbs: gallery.thumbs,
        feedback: state.feedback,
        del_id: state.theme_delete_id,
        del_msg: state.theme_delete_msg,
        del_visible: state.theme_delete_visible,
    };
    let grid = Element::switcher(gallery.layout, move |(list, cols)| {
        if list.is_empty() {
            return placeholder_text("还没有可用的主题").padding_xy(0, 12);
        }
        Element::grid(cols, GAP, list.into_iter().map(|e| cell(&ctx, e)).collect())
    })
    .width_match();
    let driver = Element::leaf()
        .widget(Driver {
            gallery,
            tab: state.tab,
            page: crate::cli::page_to_index("theme").unwrap_or(usize::MAX),
            painted: Cell::new(0.0),
            fetched: None,
        })
        .reactive()
        .width_match()
        .height(1);
    let body = Element::col()
        .width_match()
        .padding_xy(0, 14)
        .spacing(4)
        .child(grid)
        .child(driver);
    crate::widgets::section_items(
        SECTION_TITLE,
        vec![crate::widgets::CardItem::Bare(crate::widgets::card_pad(
            body,
        ))],
    )
}

/// 每格要用的共享句柄。
#[derive(Clone)]
struct CellCtx {
    fields: Rc<FieldRegistry>,
    values: Signal<Vec<String>>,
    thumbs: Signal<HashMap<String, Thumb>>,
    feedback: Feedback,
    del_id: Signal<String>,
    del_msg: Signal<String>,
    del_visible: Signal<bool>,
}

impl CellCtx {
    /// 主题下拉此刻选中的 id。画廊与下拉共用 `ui.theme.name` 的那个索引信号，两处天然同步。
    fn selected(&self) -> Option<String> {
        let idx = self
            .fields
            .select_indices
            .borrow()
            .get("ui.theme.name")?
            .get();
        self.values.with(|v| v.get(idx).cloned())
    }

    /// 选中某个主题：写下拉的索引信号（未保存改动的口径与下拉一致，点「应用」才落盘）。
    fn select(&self, id: &str) {
        let Some(sig) = self
            .fields
            .select_indices
            .borrow()
            .get("ui.theme.name")
            .copied()
        else {
            return;
        };
        if let Some(i) = self.values.with(|v| v.iter().position(|x| x == id)) {
            sig.set(i);
        }
    }
}

fn placeholder_text(text: &str) -> Element {
    Element::label(text)
        .font_size(12.0)
        .fg_role(Role::TextSubtle)
        .max_lines(1)
}

fn cell(ctx: &CellCtx, e: GalleryEntry) -> Element {
    let id = e.id.clone();
    let thumb_key = ctx
        .thumbs
        .map(move |m| m.get(&id).cloned().unwrap_or(Thumb::Pending));
    let thumb = Element::switcher(thumb_key, move |t: Thumb| match t {
        // 格宽随列数变：图按格宽等比缩小（Contain），但不放大到超过原图的逻辑尺寸。
        Thumb::Ready(png) => {
            let (w, h) = natural_size(&png);
            Element::image_bytes(&png.bytes)
                .fit(Fit::Contain)
                .width_match()
                .max_width(w.max(1))
                .height(h.clamp(1, THUMB_H))
        }
        Thumb::Pending => placeholder_text("预览生成中…"),
        Thumb::Failed => placeholder_text("暂无预览"),
    });
    let image_area = Element::stack()
        .width_match()
        .height(THUMB_H)
        .bg_role(Role::SurfaceAlt)
        .corner(6.0)
        .child(thumb.align(Align::Center));

    let sel = ctx.clone();
    let id = e.id.clone();
    let is_current = move || sel.selected().as_deref() == Some(id.as_str());
    let check = {
        let f = is_current.clone();
        Element::label("✓")
            .font_size(13.0)
            .font_weight(700)
            .fg_role(Role::AccentText)
            .visible_when(f)
    };
    let name = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(6)
        .child(
            Element::label(e.display.clone())
                .font_size(13.0)
                .fg_role(Role::Text)
                .max_lines(1)
                .weight(1.0),
        )
        .child(check);

    // 描边做成一圈外框底色：没有按状态换边框的修饰符，而底色有（`bg_when`）。外框留
    // `RING` 宽的内边距，里面那层再铺卡面色，露出来的一圈就是边框——当前主题用强调色。
    let body = Element::col()
        .width_match()
        .padding(CELL_PAD - RING)
        .spacing(8)
        .bg_role(Role::Surface)
        .corner(10.0 - RING as f32)
        .child(image_area)
        .child(name);
    let ring = is_current.clone();
    let click = ctx.clone();
    let click_id = e.id.clone();
    let menu_ctx = ctx.clone();
    Element::col()
        .width_match()
        .padding(RING)
        .corner(10.0)
        .bg_when(move || {
            let t = windui::theme::current();
            if ring() { Role::Accent } else { Role::Border }.resolve(&t)
        })
        .clickable()
        .tooltip(e.display.clone())
        .on_click(move |_| click.select(&click_id))
        .on_context_menu(move || cell_menu(&menu_ctx, &e))
        .child(body)
}

/// 每格的右键菜单：使用 / 导出 / 删除（内置主题不能删）。
fn cell_menu(ctx: &CellCtx, e: &GalleryEntry) -> Vec<MenuItem> {
    let (use_ctx, use_id) = (ctx.clone(), e.id.clone());
    let (export_id, feedback) = (e.id.clone(), ctx.feedback);
    let mut items = vec![
        MenuItem::run("使用此主题", move |_| use_ctx.select(&use_id), false),
        MenuItem::run(
            "导出主题包…",
            move |c: &mut EventCtx| {
                let id = export_id.clone();
                c.defer_blocking(move || crate::pages::export_theme_package(id, feedback));
            },
            false,
        ),
    ];
    if !e.builtin {
        let (id, disp) = (e.id.clone(), e.display.clone());
        let (del_id, del_msg, del_visible) = (ctx.del_id, ctx.del_msg, ctx.del_visible);
        items.push(MenuItem::separator());
        items.push(MenuItem::run(
            "删除主题…",
            move |_| crate::state::ask_delete_theme(del_id, del_msg, del_visible, &id, &disp),
            false,
        ));
    }
    items
}

// ───────────────────────── 主题包导入框的预览 ─────────────────────────

/// 主题包确认框里的预览图：`path` 一换就按新包取一张，框收起就取消在途请求。
/// 失败只是不出图，不挡导入。
pub fn package_preview(net: Rc<NetTasks>, path: Signal<String>, visible: Signal<bool>) -> Element {
    let thumb: Signal<Thumb> = signal(Thumb::Failed);
    let task: Rc<Cell<Option<TaskId>>> = Rc::new(Cell::new(None));
    let image = Element::switcher(thumb, |t: Thumb| match t {
        Thumb::Ready(png) => {
            let (w, h) = fit_size(&png, 324, IMPORT_PREVIEW_MAX_H);
            Element::col()
                .width_match()
                .cross(Align::Center)
                .padding_edges(Insets::new(18, 0, 18, 16))
                .child(
                    Element::image_bytes(&png.bytes)
                        .fit(Fit::Contain)
                        .width(w)
                        .height(h),
                )
        }
        // 取图中 / 失败都不占位：图来了再撑开，不让确认框先空出一块。
        _ => Element::leaf().width(0).height(0),
    })
    .width_match();
    let effect = crate::pages::dict::reactive::reactive_effect(
        move || path.version().wrapping_add(visible.version()),
        move || {
            if let Some(t) = task.take() {
                net.cancel(t);
            }
            thumb.set(Thumb::Failed);
            let p = path.get();
            if !visible.get() || p.is_empty() {
                return;
            }
            let task_done = task.clone();
            let id = net.spawn(
                move |_| {
                    crate::rpc::call_core("theme.renderPreview", &json!({ "package": p }))
                        .ok()
                        .and_then(|v| parse_receipt(&v, &read_file))
                },
                move |r: Option<Option<RawPng>>| {
                    task_done.set(None);
                    if let Some(raw) = r.flatten() {
                        thumb.set(Thumb::Ready(raw.into_png(1.0)));
                    }
                },
            );
            task.set(Some(id));
        },
    );
    Element::col().width_match().child(image).child(effect)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = include_bytes!("mockdata/theme_previews/default.png");

    fn no_read(_: &str) -> Option<Vec<u8>> {
        None
    }

    #[test]
    fn preview_refresh_keys_are_theme_candidate_and_font() {
        for k in [
            "ui.theme.name",
            "ui.theme.style",
            "ui.candidate.font_size",
            "ui.font.family",
            "ui.theme",
            "ui.candidate",
        ] {
            assert!(affects_previews(k), "{k} 应触发重取");
        }
        for k in [
            "ui.toolbar.buttons",
            "ui.themes",
            "ui.candidatex",
            "schema.active",
            "ui",
        ] {
            assert!(!affects_previews(k), "{k} 不应触发重取");
        }
    }

    #[test]
    fn receipt_prefers_inline_png_over_path() {
        use base64::Engine;
        let v = json!({
            "ok": true, "hash": "h1", "width": 240, "height": 64, "cached": true,
            "path": "/nope.png",
            "png": base64::engine::general_purpose::STANDARD.encode(PNG),
        });
        let read = |_: &str| -> Option<Vec<u8>> { panic!("有内联数据就不该读文件") };
        let r = parse_receipt(&v, &read).unwrap();
        assert_eq!(r.bytes, PNG);
        assert_eq!((r.width, r.height, r.hash.as_str()), (240, 64, "h1"));
    }

    #[test]
    fn receipt_reads_path_and_falls_back_to_png_header_size() {
        let v = json!({ "ok": true, "hash": "h2", "path": "/cache/a.png", "cached": false });
        let read = |p: &str| (p == "/cache/a.png").then(|| PNG.to_vec());
        let r = parse_receipt(&v, &read).unwrap();
        assert_eq!((r.width, r.height), (240, 64), "宽高缺失时取 PNG 头");
    }

    #[test]
    fn failed_or_unreadable_receipts_are_none() {
        assert!(parse_receipt(&json!({ "ok": false, "error": "找不到主题" }), &no_read).is_none());
        assert!(parse_receipt(&json!({ "ok": true, "path": "/x.png" }), &no_read).is_none());
        assert!(parse_receipt(&json!({ "ok": true, "png": "!!!" }), &no_read).is_none());
        assert!(parse_receipt(&json!({}), &no_read).is_none());
    }

    #[test]
    fn batch_keeps_request_order_and_pads_missing_items() {
        let read = |p: &str| (p == "/a.png").then(|| PNG.to_vec());
        let v = json!({ "items": [
            { "ok": false, "error": "x" },
            { "ok": true, "path": "/a.png", "width": 240, "height": 64, "hash": "a" },
        ]});
        let out = parse_batch(&v, 3, &read);
        assert_eq!(out.len(), 3);
        assert!(out[0].is_none());
        assert_eq!(out[1].as_ref().unwrap().hash, "a");
        assert!(out[2].is_none(), "回执短了一截，缺的那格按失败");
    }

    #[test]
    fn whole_batch_failure_turns_every_cell_into_placeholder() {
        let ids = vec!["default".to_string(), "jade".to_string()];
        let m = thumbs_from(&ids, Some(Err("连接 core 失败".into())), 1.0);
        assert_eq!(m.len(), 2);
        assert!(m.values().all(|t| *t == Thumb::Failed));
        let m = thumbs_from(&ids, None, 1.0);
        assert!(
            m.values().all(|t| *t == Thumb::Failed),
            "工作线程 panic 同样只出占位"
        );
    }

    #[test]
    fn thumbs_map_by_id_with_scale() {
        let ids = vec!["default".to_string(), "gone".to_string()];
        let raw = RawPng {
            hash: "a".into(),
            bytes: PNG.to_vec(),
            width: 480,
            height: 128,
        };
        let m = thumbs_from(&ids, Some(Ok(vec![Some(raw), None])), 2.0);
        match &m["default"] {
            Thumb::Ready(p) => assert_eq!(p.scale_pct, 200),
            t => panic!("应取到图：{t:?}"),
        }
        assert_eq!(m["gone"], Thumb::Failed);
    }

    #[test]
    fn fit_size_scales_by_width_keeps_ratio_and_never_upscales() {
        let png = |w, h, pct| Png {
            hash: String::new(),
            bytes: Rc::from(&[][..]),
            width: w,
            height: h,
            scale_pct: pct,
        };
        // 2× 屏上 960×128 物理 = 480×64 逻辑，按宽收进 240 ⇒ 240×32。
        assert_eq!(fit_size(&png(960, 128, 200), 240, 120), (240, 32));
        // 小图不放大。
        assert_eq!(fit_size(&png(200, 50, 100), 240, 120), (200, 50));
        // 竖排高图按高收。
        assert_eq!(fit_size(&png(200, 600, 100), 240, 120), (40, 120));
    }

    #[test]
    fn debounce_skips_work_when_cancelled_meanwhile() {
        let cancel = AtomicBool::new(false);
        assert_eq!(
            after_debounce(&cancel, Duration::from_millis(20), || 7),
            Some(7)
        );
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let t = std::thread::spawn(move || {
            after_debounce(&c, Duration::from_millis(500), || {
                panic!("取消后不该再发 RPC")
            })
        });
        std::thread::sleep(Duration::from_millis(30));
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(t.join().unwrap(), None::<()>);
    }

    #[test]
    fn columns_track_the_target_cell_width() {
        assert_eq!(cols_for(0), 1);
        assert_eq!(cols_for(200), 1);
        assert_eq!(cols_for(544), 2, "两格正好各 266");
        assert_eq!(cols_for(900), 3);
        assert_eq!(cols_for(1200), 4);
        let png = Png {
            hash: String::new(),
            bytes: Rc::from(&[][..]),
            width: 960,
            height: 128,
            scale_pct: 200,
        };
        assert_eq!(natural_size(&png), (480, 64));
    }

    #[test]
    fn clamp_scale_stays_in_server_range() {
        assert_eq!(clamp_scale(0.0), 1.0, "还没画过按 1");
        assert_eq!(clamp_scale(1.5), 1.5);
        assert_eq!(clamp_scale(8.0), 4.0);
        assert_eq!(clamp_scale(0.25), 0.5);
    }

    #[test]
    fn mock_batch_reply_parses_into_inline_images() {
        let items = json!({ "items": [{ "name": "default" }, { "name": "no-such-theme" }] });
        let v = crate::rpc::mock_reply("theme.renderPreviews", &items);
        let out = parse_batch(&v, 2, &no_read);
        assert!(out[0].is_some(), "mock 回内联 PNG，不读 path 也能出图");
        assert!(out[1].is_none());
        let one = crate::rpc::mock_reply("theme.renderPreview", &json!({ "package": "/x.wtheme" }));
        assert!(parse_receipt(&one, &no_read).is_some());
    }

    #[test]
    fn set_entries_only_invalidates_on_real_change() {
        let g = ThemeGallery::new(Rc::new(NetTasks::default()));
        let e = vec![GalleryEntry {
            id: "a".into(),
            display: "A".into(),
            builtin: true,
        }];
        g.set_entries(e.clone());
        let r = g.rev.get();
        g.set_entries(e);
        assert_eq!(g.rev.get(), r, "同一份列表不该再取一次");
        g.set_entries(Vec::new());
        assert_eq!(g.rev.get(), r + 1);
    }
}
