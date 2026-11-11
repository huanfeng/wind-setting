//! 「主题」页的已装主题画廊与主题包导入框里的预览图。
//!
//! 缩略图由服务端用实时候选窗的渲染器出 PNG（`theme.renderPreview(s)`，协议见 WindInput
//! `docs/design/wind-scene-extraction.md` §7.2），像素与实机一致；设置端只负责取图、摆格子。
//!
//! - **取图不占 UI 线程**：RPC 在 [`NetTasks`] 的工作线程里做，结果回 UI 线程只写信号。
//!   一律要内联 PNG（`inline: true`），不读服务端缓存目录里的文件。
//! - **小批、一次只一个在途**：每批 [`CHUNK`] 条（= 服务端同时交给 UI 线程的上限），一批回来
//!   就合并进图表，图逐格出现。服务端的名额按进程算，客户端取消只是不再等、那边照样在画；
//!   同时再发一批只会换来「忙」，所以在途时再标脏只记一笔，等这批回来再重来。
//! - **防抖**：外观相关配置变更、主题增删都只是「标脏」，由页内一枚哨兵在本页可见时发起；
//!   先等 [`DEBOUNCE`]，期间再标脏就重新计时，连改几项只换来一轮（服务端有缓存，没变的主题
//!   直接命中）。
//! - **失败不打扰**：服务端不在、整批出错时已有的图照留，没图的格显示占位；单条「忙」/「超时」
//!   约 1 秒后重试，至多 [`MAX_RETRIES`] 次；只有服务端明确说这张画不了（`ok:false`）才置失败。

use std::cell::Cell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
use crate::state::AppState;
use crate::widgets::Vis;

/// 画廊所在卡片的标题（清单里 `ui.theme.name` 的 section，二级导航里也叫这个）。
pub const SECTION_TITLE: &str = "主题选择";
/// 主题页第二张卡（风格、动画、导入导出、市场）的标题，即清单里那一节的名字。
pub const SETTINGS_SECTION: &str = "主题设置";

/// 一格的目标宽度（逻辑 px）：列数按可用宽度取最接近这个宽度的那一档，各列等分。
const CELL_W: i32 = 260;
/// 格与格之间的间距。
const GAP: i32 = 12;
/// 格内边距：图与名字都缩进这么多。
const CELL_PAD: i32 = 10;
/// 选中外环的外圈宽度：未选中时透明占位，选中时与内圈连成一道强调色粗环。
const RING_OUTER: i32 = 1;
/// 外环的内圈宽度：未选中是 1 px 描边色，选中换成强调色。
const RING_INNER: i32 = 1;
/// 选中格的卡面：在卡面色上叠这么多强调色。
const SELECTED_TINT: f32 = 0.12;
/// 图区高度：固定，免得横排 / 竖排主题混排时一行格子高低不齐。
const THUMB_H: i32 = 120;
/// 防抖时长：标脏到真正发 RPC 之间至少隔这么久。
const DEBOUNCE: Duration = Duration::from_millis(300);
/// 每批几条：服务端全进程同时交给 UI 线程的预览至多 8 张（`MAX_IN_FLIGHT`）。
const CHUNK: usize = 8;
/// 单条「忙」/「超时」后隔多久重试、至多几次。
const RETRY_DELAY: Duration = Duration::from_millis(1000);
const MAX_RETRIES: u32 = 3;
/// 导入框里预览图的最大高度。
const IMPORT_PREVIEW_MAX_H: i32 = 160;

/// 主题编辑器经本机服务改了主题文件的次数（服务线程里加，哨兵在 UI 线程比对）。
static EXTERNAL_EDITS: AtomicU64 = AtomicU64::new(0);

/// 主题文件在设置窗之外被改过（主题编辑器推送）：画廊下次排版时重取。
pub fn note_external_edit() {
    EXTERNAL_EDITS.fetch_add(1, Ordering::Relaxed);
}

/// [`note_external_edit`] 的累计次数。
pub fn external_edits() -> u64 {
    EXTERNAL_EDITS.load(Ordering::Relaxed)
}

/// 一个已装主题（`theme.list` 的一项）。core 协议：`name` = slug/id，`display_name` = 显示名。
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct GalleryEntry {
    /// slug/id，保存到 `ui.theme.name`。
    pub id: String,
    /// 界面显示名。
    pub display: String,
    /// 版本号（可空）。
    pub version: String,
    pub author: String,
    /// 是否内置主题（内置不可删除，用户导入的可删）。
    pub builtin: bool,
    /// 用户主题所在目录（内置为空）。
    pub dir: String,
    /// 主题文件里写的继承名（没写为空）。
    pub base: String,
    /// 带异形皮肤 / 皮肤装饰会动。
    pub skin: bool,
    pub animated: bool,
}

/// 一格的图。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Thumb {
    /// 还没取到（首轮在途、或「忙」了等重试）。
    Pending,
    Ready(Png),
    /// 取不到：服务端不可用且还没有旧图，或服务端明确说画不了。
    Failed,
}

/// 一张取到手的预览图。`width` / `height` 是物理像素。
///
/// 相等与哈希只看服务端给的内容哈希与缩放（哈希为空时看是不是同一份字节），不逐字节比
/// PNG：网格每次重取都要拿它当重建键比一遍。
#[derive(Clone, Debug)]
pub struct Png {
    pub hash: String,
    pub bytes: Rc<[u8]>,
    pub width: u32,
    pub height: u32,
    /// 请求时的缩放 ×100。
    pub scale_pct: u32,
}

impl PartialEq for Png {
    fn eq(&self, o: &Self) -> bool {
        self.scale_pct == o.scale_pct
            && if self.hash.is_empty() || o.hash.is_empty() {
                Rc::ptr_eq(&self.bytes, &o.bytes)
            } else {
                self.hash == o.hash
            }
    }
}

impl Eq for Png {}

impl Hash for Png {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.scale_pct.hash(h);
        if self.hash.is_empty() {
            (self.bytes.as_ptr() as usize).hash(h);
        } else {
            self.hash.hash(h);
        }
    }
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
    pub(crate) fn into_png(self, scale: f32) -> Png {
        Png {
            hash: self.hash,
            bytes: self.bytes.into(),
            width: self.width,
            height: self.height,
            scale_pct: scale_pct(scale),
        }
    }
}

/// 一条回执的结论。
#[derive(Debug, PartialEq, Eq)]
pub enum Receipt {
    Ready(RawPng),
    /// 服务端「忙」或「超时」：这次没画，过会儿再要。
    Retry,
    /// 画不了（找不到主题、主题坏了……），或回执里没有图。
    Failed,
}

pub(crate) fn scale_pct(scale: f32) -> u32 {
    (scale * 100.0).round().max(1.0) as u32
}

/// 服务端接受的缩放区间是 0.5–4；探针还没画过时按 1。
pub(crate) fn clamp_scale(scale: f32) -> f32 {
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

/// 解析一条回执。只认内联 `png`（请求都带 `inline: true`）；宽高缺失时从 PNG 头里取。
pub fn parse_receipt(v: &Value) -> Receipt {
    use base64::Engine;
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        let e = v.get("error").and_then(Value::as_str).unwrap_or_default();
        return if e.contains('忙') || e.contains("超时") {
            Receipt::Retry
        } else {
            Receipt::Failed
        };
    }
    let Some(bytes) = v
        .get("png")
        .and_then(Value::as_str)
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
    else {
        return Receipt::Failed;
    };
    let dim = |k: &str| v.get(k).and_then(Value::as_u64).map(|n| n as u32);
    let (width, height) = match (dim("width"), dim("height")) {
        (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
        _ => match png_size(&bytes) {
            Some(d) => d,
            None => return Receipt::Failed,
        },
    };
    Receipt::Ready(RawPng {
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

/// 解析批量回执：与请求同序，缺条的位置按失败。
pub fn parse_batch(v: &Value, n: usize) -> Vec<Receipt> {
    let items = v.get("items").and_then(Value::as_array);
    (0..n)
        .map(|i| {
            items
                .and_then(|a| a.get(i))
                .map_or(Receipt::Failed, parse_receipt)
        })
        .collect()
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

/// 一批取图（工作线程里跑）。整批 RPC 失败（服务端不在等）时为 `Err`。
fn fetch_chunk(ids: &[String], scale: f32) -> Result<Vec<Receipt>, String> {
    let items: Vec<Value> = ids
        .iter()
        .map(|id| json!({ "name": id, "scale": scale, "inline": true }))
        .collect();
    let v = crate::rpc::call_core("theme.renderPreviews", &json!({ "items": items }))?;
    Ok(parse_batch(&v, ids.len()))
}

/// 把一批结果并进图表，返回这批里有没有要重试的。
///
/// 整批出错（`Err` / 工作线程 panic）与单条「忙」都**保留已有的图**：服务端一时不可用不该把
/// 满屏缩略图换成占位。没图的格在整批出错时置失败（显示占位），「忙」的留在待取等重试。
pub fn merge_chunk(
    map: &mut HashMap<String, Thumb>,
    ids: &[String],
    result: Option<Result<Vec<Receipt>, String>>,
    scale: f32,
) -> bool {
    let receipts = match result {
        Some(Ok(v)) => v,
        other => {
            if let Some(Err(e)) = other {
                tracing::debug!("主题预览取图失败：{e}");
            }
            for id in ids {
                if !matches!(map.get(id), Some(Thumb::Ready(_))) {
                    map.insert(id.clone(), Thumb::Failed);
                }
            }
            return false;
        }
    };
    let mut retry = false;
    let mut receipts = receipts.into_iter();
    for id in ids {
        match receipts.next().unwrap_or(Receipt::Failed) {
            Receipt::Ready(raw) => {
                map.insert(id.clone(), Thumb::Ready(raw.into_png(scale)));
            }
            Receipt::Failed => {
                map.insert(id.clone(), Thumb::Failed);
            }
            Receipt::Retry => {
                retry = true;
                map.entry(id.clone()).or_insert(Thumb::Pending);
            }
        }
    }
    retry
}

/// 一轮收尾：去掉已不在列表里的主题；不再重试时，还在等的格改成占位。
pub fn finish_round(map: &mut HashMap<String, Thumb>, ids: &[String], give_up: bool) {
    map.retain(|k, _| ids.contains(k));
    if give_up {
        for t in map.values_mut() {
            if *t == Thumb::Pending {
                *t = Thumb::Failed;
            }
        }
    }
}

/// 图的逻辑尺寸（物理像素 ÷ 请求时的缩放）。
pub fn natural_size(png: &Png) -> (i32, i32) {
    let s = png.scale_pct.max(1) as f32 / 100.0;
    (
        (png.width as f32 / s).round() as i32,
        (png.height as f32 / s).round() as i32,
    )
}

/// 图在固定宽度的框里的显示尺寸（逻辑 px）：按宽缩放、保持比例，不放大，收进 `max_w × max_h`。
pub fn fit_size(png: &Png, max_w: i32, max_h: i32) -> (i32, i32) {
    let (nw, nh) = natural_size(png);
    if nw <= 0 || nh <= 0 {
        return (0, 0);
    }
    let k = (max_w as f32 / nw as f32)
        .min(max_h as f32 / nh as f32)
        .min(1.0);
    (
        ((nw as f32 * k).round() as i32).max(1),
        ((nh as f32 * k).round() as i32).max(1),
    )
}

/// 可用宽度 `w`（逻辑 px）下分几列：让每格最接近 [`CELL_W`]，至少一列。
pub fn cols_for(w: i32) -> usize {
    (((w + GAP) as f32 / (CELL_W + GAP) as f32).round() as usize).max(1)
}

/// 取图流程走到哪了。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    /// 防抖计时中（还没碰服务端，可以随便取消重来）。
    Debounce(TaskId),
    /// 有一批在服务端。
    Fetch(TaskId),
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
    phase: Rc<Cell<Phase>>,
    /// 在途时又标脏了：记下要用的缩放，这批回来就重来一轮。
    pending: Rc<Cell<Option<f32>>>,
    /// 本轮因「忙」重试过几次（外部标脏时清零）。
    retries: Rc<Cell<u32>>,
    /// 设置窗口所在屏的缩放（哨兵绘制时更新；导入框的预览也用它）。
    scale: Rc<Cell<f32>>,
    /// 「主题信息」对话框：显示哪一个、开没开。
    pub info: Signal<GalleryEntry>,
    pub info_visible: Signal<bool>,
}

impl ThemeGallery {
    pub fn new(net: Rc<NetTasks>) -> Self {
        Self {
            net,
            entries: signal(Vec::new()),
            thumbs: signal(HashMap::new()),
            layout: signal((Vec::new(), 3)),
            rev: signal(0),
            phase: Rc::new(Cell::new(Phase::Idle)),
            pending: Rc::new(Cell::new(None)),
            retries: Rc::new(Cell::new(0)),
            scale: Rc::new(Cell::new(1.0)),
            info: signal(GalleryEntry::default()),
            info_visible: signal(false),
        }
    }

    /// 换上新的主题列表（`AppState::apply_theme_entries` 调，之后它会无条件标脏一次）。
    pub fn set_entries(&self, entries: Vec<GalleryEntry>) {
        if self.entries.with(|e| *e != entries) {
            self.entries.set(entries.clone());
            self.layout.update(|l| l.0 = entries);
        }
    }

    fn set_cols(&self, cols: usize) {
        if self.layout.with(|l| l.1 != cols) {
            self.layout.update(|l| l.1 = cols);
        }
    }

    /// 标脏：下一次本页可见时（防抖后）整批重取。
    pub fn invalidate(&self) {
        self.retries.set(0);
        self.rev.update(|r| *r += 1);
    }

    /// 请求一轮取图：空闲就开始防抖；防抖中重新计时；有批在途只记一笔。
    fn request(&self, scale: f32) {
        match self.phase.get() {
            Phase::Fetch(_) => self.pending.set(Some(scale)),
            Phase::Debounce(t) => {
                self.net.cancel(t);
                self.start_debounce(scale);
            }
            Phase::Idle => self.start_debounce(scale),
        }
    }

    fn start_debounce(&self, scale: f32) {
        let g = self.clone();
        let id = self.net.spawn(
            move |cancel| after_debounce(cancel, DEBOUNCE, || ()),
            move |_: Option<Option<()>>| g.start_round(scale),
        );
        self.phase.set(Phase::Debounce(id));
    }

    /// 防抖到点：按此刻的主题列表从头取。
    fn start_round(&self, scale: f32) {
        let ids: Rc<Vec<String>> = Rc::new(
            self.entries
                .with(|e| e.iter().map(|x| x.id.clone()).collect()),
        );
        if ids.is_empty() {
            self.thumbs.set(HashMap::new());
            self.phase.set(Phase::Idle);
            return;
        }
        self.fetch_chunk_at(ids, 0, scale, false);
    }

    fn fetch_chunk_at(&self, ids: Rc<Vec<String>>, at: usize, scale: f32, retry: bool) {
        let chunk: Vec<String> = ids[at..(at + CHUNK).min(ids.len())].to_vec();
        let g = self.clone();
        let id = self.net.spawn(
            move |cancel| {
                // 发每一批之前查一次：被取消（窗口关了之类）就别再占服务端的名额。
                (!cancel.load(Ordering::Relaxed)).then(|| fetch_chunk(&chunk, scale))
            },
            move |r: Option<Option<Result<Vec<Receipt>, String>>>| {
                g.chunk_done(ids, at, scale, retry, r.flatten())
            },
        );
        self.phase.set(Phase::Fetch(id));
    }

    /// 一批回来：合并，然后看是重来一轮（在途时被标脏过）、接着取下一批，还是收尾。
    fn chunk_done(
        &self,
        ids: Rc<Vec<String>>,
        at: usize,
        scale: f32,
        retry: bool,
        result: Option<Result<Vec<Receipt>, String>>,
    ) {
        let end = (at + CHUNK).min(ids.len());
        // 整批出错多半是服务端不在：剩下的批不必再一批批等超时。
        let broken = !matches!(result, Some(Ok(_)));
        let mut retry_now = false;
        self.thumbs
            .update(|m| retry_now = merge_chunk(m, &ids[at..end], result, scale));
        let retry = retry || retry_now;
        if let Some(s) = self.pending.take() {
            self.phase.set(Phase::Idle);
            self.start_debounce(s);
            return;
        }
        if broken {
            self.thumbs.update(|m| {
                merge_chunk(m, &ids[end..], None, scale);
                finish_round(m, &ids, false);
            });
            self.phase.set(Phase::Idle);
            return;
        }
        if end < ids.len() {
            self.fetch_chunk_at(ids, end, scale, retry);
            return;
        }
        self.phase.set(Phase::Idle);
        let again = retry && self.retries.get() < MAX_RETRIES;
        self.thumbs
            .update(|m| finish_round(m, &ids, retry && !again));
        if again {
            self.retries.set(self.retries.get() + 1);
            let rev = self.rev;
            self.net.spawn(
                |cancel| after_debounce(cancel, RETRY_DELAY, || ()),
                // 不走 `invalidate`：那会把重试计数清零。
                move |_: Option<Option<()>>| rev.update(|r| *r += 1),
            );
        }
    }

    /// 标脏次数（主题列表换过、重试到点都会加一）：外观页预览据此知道主题可能变了像素。
    pub fn revision(&self) -> u64 {
        self.rev.get()
    }

    /// 设置窗口所在屏的缩放（还没画过时为 1）。
    pub fn scale_cell(&self) -> Rc<Cell<f32>> {
        self.scale.clone()
    }
}

/// 页内哨兵（整宽、1 px 高、不画东西，排在网格之前免得被裁掉不绘制）：绘制时记下窗口所在屏
/// 的缩放、按自己的宽度定列数；本页可见、画廊没被藏，且标脏过或缩放变了，就请求一轮取图。
/// 不可见时只攒着，不替用户没打开的页去渲染。
struct Driver {
    gallery: ThemeGallery,
    tab: Signal<usize>,
    page: usize,
    vis: Vis,
    /// 最近一次绘制时画布的缩放（0 = 还没画过）。
    painted: Cell<f32>,
    /// 已按哪一版（标脏次数、外部改动次数、缩放）请求过。
    fetched: Option<(u64, u64, f32)>,
}

impl Widget for Driver {
    fn on_update(&mut self, ctx: &mut EventCtx) {
        if self.tab.get() != self.page || !(self.vis)() {
            return;
        }
        // 上一帧排出来的宽度（on_update 在 measure 之前跑）。没排过时为 0，先按默认列数。
        let id = ctx.id();
        let w = ctx.tree_mut().get(id).map_or(0, |n| n.bounds.w);
        if w > 0 {
            self.gallery.set_cols(cols_for(w));
        }
        let want = (
            self.gallery.rev.get(),
            EXTERNAL_EDITS.load(Ordering::Relaxed),
            clamp_scale(self.painted.get()),
        );
        if self.fetched != Some(want) {
            self.fetched = Some(want);
            self.gallery.request(want.2);
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
        self.gallery.scale.set(clamp_scale(s));
        // 缩放变了（首帧发现不是 1、拖到别的屏）：标脏好让下一帧的 on_update 按新缩放重取。
        // 首帧前按 1 请求的那轮还在防抖里，会被重新计时，不多一次渲染。
        if (clamp_scale(s) - before).abs() > f32::EPSILON {
            self.gallery.invalidate();
        }
    }

    fn hit_opaque(&self) -> bool {
        false
    }
}

/// 主题选择：清单里 `ui.theme.name`（`type = "theme_gallery"`）渲染成的整宽画廊——页内哨兵 +
/// 网格。点一格即选中（未保存改动的口径与其它设置一致，点「应用」才落盘）；写回器、载入器、
/// 取值器与选中索引按响应式下拉的同一套登记（[`crate::manifest::register_reactive_select`]），
/// 搜索、定制隐藏、能力覆盖照常认这个键。`vis` 为假（被用户藏掉）时不取图。
pub fn build_selector(state: &AppState, key: &str, vis: Vis) -> Element {
    let gallery = state.theme_gallery.clone();
    let entries = gallery.entries;
    let index = crate::manifest::register_reactive_select(
        state,
        key,
        &state.current_config,
        state.theme_value_sig,
    );
    let ctx = CellCtx {
        index,
        values: state.theme_value_sig,
        thumbs: gallery.thumbs,
        feedback: state.feedback,
        del_id: state.theme_delete_id,
        del_msg: state.theme_delete_msg,
        del_visible: state.theme_delete_visible,
        info: gallery.info,
        info_visible: gallery.info_visible,
        // 导出跟着「主题导入导出」走：定制版藏了就不建，用户藏了就运行期不出。
        export: (!state.manifest.feature_hidden("theme_transfer"))
            .then(|| crate::manifest::feature_row_vis(state, "theme_transfer")),
        jump: state.jump,
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
            vis,
            painted: Cell::new(0.0),
            fetched: None,
        })
        .reactive()
        .width_match()
        .height(1);
    // 清单 hint（「候选窗与工具栏的主题样式」）照留：画廊没有左侧标签行，说明放在网格上方。
    let hint = state
        .manifest
        .get(key)
        .and_then(|it| it.hint.clone())
        .filter(|h| !h.is_empty())
        .map(|h| {
            Element::label(h)
                .font_size(12.0)
                .fg_role(Role::TextSubtle)
                .max_lines(2)
        });
    // 选中的是一个没装的主题（配置里写着、列表里没有）：说一句，免得用户以为哪一格都没选中。
    let missing = signal(missing_theme_note(index, state.theme_value_sig, entries));
    let watch = {
        let values = state.theme_value_sig;
        crate::pages::dict::reactive::reactive_effect(
            move || {
                index
                    .version()
                    .wrapping_add(values.version())
                    .wrapping_add(entries.version())
            },
            move || {
                let note = missing_theme_note(index, values, entries);
                if missing.with(|m| *m != note) {
                    missing.set(note);
                }
            },
        )
    };
    let note = Element::label_signal(missing)
        .font_size(12.0)
        .fg_role(Role::Warning)
        .max_lines(2)
        .visible_when(move || !missing.with(String::is_empty));
    let mut col = Element::col()
        .width_match()
        .padding_xy(0, 6)
        .spacing(6)
        .child(driver);
    if let Some(h) = hint {
        col = col.child(h);
    }
    col.child(note).child(watch).child(grid)
}

/// 选中项不在已装主题里时的提示（空 = 无需提示）。
fn missing_theme_note(
    index: Signal<usize>,
    values: Signal<Vec<String>>,
    entries: Signal<Vec<GalleryEntry>>,
) -> String {
    let Some(id) = values.with(|v| v.get(index.get()).cloned()) else {
        return String::new();
    };
    // 列表还没到（服务端未连上）时什么也不说：那时谁都「不在列表里」。
    if id.is_empty() || entries.with(|e| e.is_empty() || e.iter().any(|x| x.id == id)) {
        return String::new();
    }
    format!("当前配置的主题「{id}」未安装，输入法按默认主题显示；点选一个主题即可替换")
}

/// 每格要用的共享句柄。
#[derive(Clone)]
struct CellCtx {
    /// `ui.theme.name` 的选中索引（下标进 `values`）。
    index: Signal<usize>,
    values: Signal<Vec<String>>,
    thumbs: Signal<HashMap<String, Thumb>>,
    feedback: Feedback,
    del_id: Signal<String>,
    del_msg: Signal<String>,
    del_visible: Signal<bool>,
    info: Signal<GalleryEntry>,
    info_visible: Signal<bool>,
    /// 右键菜单里的「导出」此刻能不能出（`None` = 定制版藏了，永远不出）。
    export: Option<Vis>,
    /// 跳转请求信号：每格锚点（wrap_runtime）要挂它，「当前主题」链接也用它发请求。
    jump: Signal<Option<crate::search::jump::JumpRequest>>,
}

impl CellCtx {
    /// 此刻选中的 id。
    fn selected(&self) -> Option<String> {
        let idx = self.index.get();
        self.values.with(|v| v.get(idx).cloned())
    }

    /// 选中某个主题：只写索引信号（点「应用」才落盘）。
    fn select(&self, id: &str) {
        if let Some(i) = self.values.with(|v| v.iter().position(|x| x == id)) {
            self.index.set(i);
        }
    }

    fn ask_delete(&self, e: &GalleryEntry) {
        let current = self.selected().as_deref() == Some(e.id.as_str());
        crate::state::ask_delete_theme(
            self.del_id,
            self.del_msg,
            self.del_visible,
            &e.id,
            &e.display,
            current,
        );
    }

    fn show_info(&self, e: &GalleryEntry) {
        self.info.set(e.clone());
        self.info_visible.set(true);
    }
}

/// `ui.theme.name` 此刻选中的主题的显示名，逐层退让：选中索引在取值表里查出 id，再在画廊
/// 清单里换成显示名。显示名缺了退 id；索引/取值表拿不到（画廊没建、清单没到、越界）退配置值；
/// 再没有退 `None`。
pub(crate) fn display_name(
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

/// 链接文案：有名字带上，没有就只留「当前主题」。
fn current_theme_label(name: Option<&str>) -> String {
    match name {
        Some(n) if !n.is_empty() => format!("当前主题：{n}"),
        _ => "当前主题".to_string(),
    }
}

/// 链接落点：**此刻选中格**的锚点键（运行期动态键，画廊每格一格，不进守门清单——
/// 数量与键值依赖运行期数据；手写静态键仍走 anchor::wrap 进守门）。
///
/// 逐层退让：选中索引 → 取值表查 id；索引/取值表拿不到（画廊没建、清单没到）退配置值；
/// 再没有退空串——空串键必然无人认领，请求挂起，等画廊到达后锚点认领再滚。
pub(crate) fn theme_cell_target(
    idx: Option<usize>,
    values: &[String],
    cfg: Option<&str>,
) -> crate::search::index::JumpTarget {
    let id = idx
        .and_then(|i| values.get(i).cloned())
        .or(cfg.map(str::to_string))
        .unwrap_or_default();
    crate::search::index::JumpTarget::Row {
        tab: crate::cli::page_to_index("theme").unwrap_or(usize::MAX),
        anchor: format!("ui.theme.name:{id}"),
    }
}

/// 「当前主题：〈显示名〉」链接，坐在主题页「主题选择」卡（画廊）卡头 action 位（返回
/// 文案信号供测试断言）。2026-11 真机反馈的两跳：原是外观页预览卡头跳往主题页的链接
/// （「主题选择：〈名〉」），先改到主题页页首做纯展示，再改回**可点击**、落点就是
/// 本页画廊行。
///
/// 文案跟画廊选中走（点格即变、点「应用」才落盘）：选中索引、取值表、画廊清单三个信号
/// 任一变化都重算——三个源没有组合派生信号的现成机制（`Signal::map` 只跟单源），用
/// reactive_effect 把重算结果写进本地信号。这也是外观页预览卡头原链接同款的做法。
pub(crate) fn current_theme_header(state: &AppState) -> (Signal<String>, Element) {
    let label = signal("当前主题".to_string());
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
    // 链接闭包与 apply 各要一份退让用的配置值;apply 是 move 闭包,先克隆。
    let cfg_for_link = cfg_name.clone();
    let apply = move || {
        let name = display_name(
            idx.map(|s| s.get()),
            &values.get(),
            &entries.get(),
            cfg_name.as_deref(),
        );
        label.set(current_theme_label(name.as_deref()));
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
    // eff 要一并挂树：dispatch_reactive_updates 只驱动**树里**的响应式节点。
    // 点击**现算**落点:读此刻选中(索引→取值表→配置),拼选中格的锚点键。
    let (tab, jump) = (state.tab, state.jump);
    let link = Element::link(label.clone())
        .underline(false)
        .on_click(move |_| {
            let target =
                theme_cell_target(idx.map(|s| s.get()), &values.get(), cfg_for_link.as_deref());
            crate::search::jump::request_quiet(tab, jump, target);
        });
    (label, Element::stack().child(eff).child(link))
}

/// 画廊清单为空时的占位文字。
fn placeholder_text(text: &str) -> Element {
    Element::label(text)
        .font_size(12.0)
        .fg_role(Role::TextSubtle)
        .max_lines(1)
}

/// `a` 上叠 `t` 份 `b`（不透明结果）。
fn mix(a: Color, b: Color, t: f32) -> Color {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::rgb(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b))
}

/// 版本号的显示：已带 `v` / `V` 前缀的原样用，否则补一个 `v`。
pub fn version_text(v: &str) -> String {
    if v.starts_with(['v', 'V']) {
        v.to_string()
    } else {
        format!("v{v}")
    }
}

/// 名字行右侧的小字（版本号、「内置」）。
fn meta_text(text: String) -> Element {
    Element::label(text)
        .font_size(11.0)
        .fg_role(Role::TextSubtle)
        .max_lines(1)
}

/// 一格。选中与否**几何完全一致**：外环两圈恒占位（未选中时外圈透明、内圈是 1 px 描边色），
/// 「使用中」角标叠在缩略图右上角、不进排版流，名字行内容也与选中无关——点选时格子一像素
/// 都不动，只换颜色。
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

    let sel = ctx.clone();
    let id = e.id.clone();
    let is_current = Rc::new(move || sel.selected().as_deref() == Some(id.as_str()));
    let badge = {
        let f = is_current.clone();
        Element::label("✓ 使用中")
            .font_size(11.0)
            .font_weight(600)
            .fg_role(Role::OnAccent)
            .bg_role(Role::Accent)
            .padding_xy(8, 3)
            .corner(999.0)
            .max_lines(1)
            .margin(6)
            .align_xy(Align::End, Align::Start)
            .visible_when(move || f())
    };
    let image_area = Element::stack()
        .width_match()
        .height(THUMB_H)
        .bg_role(Role::SurfaceAlt)
        .corner(6.0)
        .child(thumb.align(Align::Center))
        .child(badge);

    let mut name = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(6)
        .child(
            Element::label(e.display.clone())
                .font_size(13.0)
                .fg_role(Role::Text)
                .max_lines(1)
                .weight(1.0),
        );
    if !e.version.is_empty() {
        name = name.child(meta_text(version_text(&e.version)));
    }
    if e.builtin {
        name = name.child(meta_text("内置".into()));
    } else {
        // 没有悬停显隐，删除入口做成常驻的小号淡色图标；确认框照旧。
        let (del, entry) = (ctx.clone(), e.clone());
        name = name.child(
            Element::icon_button("✕")
                .size(18, 18)
                .fg_role(Role::TextSubtle)
                .tooltip("删除主题")
                .on_click(move |_| del.ask_delete(&entry)),
        );
    }

    let tint = is_current.clone();
    let body = Element::col()
        .width_match()
        .padding(CELL_PAD - RING_OUTER - RING_INNER)
        .spacing(8)
        .corner(10.0 - (RING_OUTER + RING_INNER) as f32)
        .bg_when(move || {
            let t = windui::theme::current();
            let surface = Role::Surface.resolve(&t);
            if tint() {
                mix(surface, Role::Accent.resolve(&t), SELECTED_TINT)
            } else {
                surface
            }
        })
        .child(image_area)
        .child(name);
    let inner = is_current.clone();
    let ring = Element::col()
        .width_match()
        .padding(RING_INNER)
        .corner(10.0 - RING_OUTER as f32)
        .bg_when(move || {
            let t = windui::theme::current();
            if inner() { Role::Accent } else { Role::Border }.resolve(&t)
        })
        .child(body);
    let outer = is_current.clone();
    let click = ctx.clone();
    let click_id = e.id.clone();
    let menu_ctx = ctx.clone();
    let cell_key = e.id.clone();
    let cell = Element::col()
        .width_match()
        .padding(RING_OUTER)
        .corner(10.0)
        .bg_when(move || {
            if outer() {
                Role::Accent.resolve(&windui::theme::current())
            } else {
                Color::TRANSPARENT
            }
        })
        .clickable()
        .tooltip(e.display.clone())
        .on_click(move |_| click.select(&click_id))
        .on_context_menu(move || cell_menu(&menu_ctx, &e))
        .child(ring);
    crate::search::anchor::wrap_runtime(cell, format!("ui.theme.name:{}", cell_key), ctx.jump)
}

/// 每格的右键菜单：使用 / 主题信息 / 导出 / 删除（内置主题不能删；导出跟着 `feature:theme_transfer`）。
fn cell_menu(ctx: &CellCtx, e: &GalleryEntry) -> Vec<MenuItem> {
    let (use_ctx, use_id) = (ctx.clone(), e.id.clone());
    let (info_ctx, info_entry) = (ctx.clone(), e.clone());
    let mut items = vec![
        MenuItem::run("使用此主题", move |_| use_ctx.select(&use_id), false),
        MenuItem::run("主题信息…", move |_| info_ctx.show_info(&info_entry), false),
    ];
    if ctx.export.as_ref().is_some_and(|v| v()) {
        let (export_id, feedback) = (e.id.clone(), ctx.feedback);
        items.push(MenuItem::run(
            "导出主题包…",
            move |c: &mut EventCtx| {
                let id = export_id.clone();
                c.defer_blocking(move || crate::pages::export_theme_package(id, feedback));
            },
            false,
        ));
    }
    if !e.builtin {
        let (del, entry) = (ctx.clone(), e.clone());
        items.push(MenuItem::separator());
        items.push(MenuItem::run(
            "删除主题…",
            move |_| del.ask_delete(&entry),
            false,
        ));
    }
    items
}

/// 「主题信息」的各行（标签, 值）；值为空的行不列。
pub fn info_rows(e: &GalleryEntry) -> Vec<(&'static str, String)> {
    let yes_no = |b: bool| if b { "有" } else { "无" }.to_string();
    let rows = [
        ("名称", e.display.clone()),
        ("ID", e.id.clone()),
        ("版本", e.version.clone()),
        ("作者", e.author.clone()),
        (
            "来源",
            if e.builtin { "内置" } else { "用户导入" }.to_string(),
        ),
        ("目录", e.dir.clone()),
        ("继承自", e.base.clone()),
        ("异形皮肤", yes_no(e.skin)),
        ("皮肤动画", yes_no(e.animated)),
    ];
    rows.into_iter().filter(|(_, v)| !v.is_empty()).collect()
}

/// 「主题信息」对话框（挂在窗口根上）。
pub fn info_dialog(gallery: &ThemeGallery) -> Element {
    let visible = gallery.info_visible;
    let body = Element::switcher(gallery.info, |e: GalleryEntry| {
        let mut col = Element::col().width_match().spacing(8).padding_xy(18, 16);
        for (k, v) in info_rows(&e) {
            col = col.child(
                Element::row()
                    .width_match()
                    .spacing(12)
                    .child(
                        Element::label(k)
                            .font_size(13.0)
                            .fg_role(Role::TextMuted)
                            .width(72),
                    )
                    .child(
                        Element::label(v)
                            .font_size(13.0)
                            .fg_role(Role::Text)
                            .max_lines(3)
                            .weight(1.0),
                    ),
            );
        }
        col
    })
    .width_match();
    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("主题信息")
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(1)
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .padding_xy(16, 12)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("关闭")
                        .small()
                        .outline_soft()
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

// ───────────────────────── 主题包导入框的预览 ─────────────────────────

/// 主题包确认框里的预览图：`path` 一换就按新包取一张，框收起就取消在途请求。
/// 缩放取设置窗口所在屏的（`scale`，同画廊）。失败只是不出图，不挡导入。
pub fn package_preview(
    net: Rc<NetTasks>,
    path: Signal<String>,
    visible: Signal<bool>,
    scale: Rc<Cell<f32>>,
) -> Element {
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
            let s = clamp_scale(scale.get());
            let task_done = task.clone();
            let id = net.spawn(
                move |_| {
                    crate::rpc::call_core(
                        "theme.renderPreview",
                        &json!({ "package": p, "scale": s, "inline": true }),
                    )
                    .ok()
                    .map(|v| parse_receipt(&v))
                },
                move |r: Option<Option<Receipt>>| {
                    // 迟到门禁：thumb 是**每格**画廊格子的行作用域信号（图钉翻转/切页
                    // 整代回收），缩略图 RPC 完成回调可能在此之后才跑——与外观预览
                    // Watcher 的收尾门禁同一崩溃链（真机 0xc0000409：写死句柄踩
                    // write_raw 的 debug 断言，Win32 回调栈内无法 unwind 直接 abort）。
                    // 探活失败连 task_done 也不用清（普通 Cell，随格子同亡）。
                    if !thumb.is_alive() {
                        return;
                    }
                    task_done.set(None);
                    if let Some(Receipt::Ready(raw)) = r.flatten() {
                        thumb.set(Thumb::Ready(raw.into_png(s)));
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

    fn b64(b: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(b)
    }

    fn raw(hash: &str) -> RawPng {
        RawPng {
            hash: hash.into(),
            bytes: PNG.to_vec(),
            width: 240,
            height: 64,
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn ready(m: &HashMap<String, Thumb>, id: &str) -> Option<String> {
        match m.get(id) {
            Some(Thumb::Ready(p)) => Some(p.hash.clone()),
            _ => None,
        }
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
    fn receipt_uses_inline_png_and_falls_back_to_png_header_size() {
        let v = json!({ "ok": true, "hash": "h1", "width": 240, "height": 64, "png": b64(PNG) });
        assert_eq!(parse_receipt(&v), Receipt::Ready(raw("h1")));
        let v = json!({ "ok": true, "hash": "h2", "png": b64(PNG) });
        match parse_receipt(&v) {
            Receipt::Ready(r) => assert_eq!((r.width, r.height), (240, 64), "宽高缺失时取 PNG 头"),
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn receipt_without_inline_png_is_a_failure() {
        let v =
            json!({ "ok": true, "hash": "h", "path": "/cache/a.png", "width": 240, "height": 64 });
        assert_eq!(parse_receipt(&v), Receipt::Failed, "不读服务端回的 path");
        assert_eq!(
            parse_receipt(&json!({ "ok": true, "png": "!!!" })),
            Receipt::Failed
        );
        assert_eq!(parse_receipt(&json!({})), Receipt::Failed);
    }

    #[test]
    fn busy_and_timeout_are_retryable_other_errors_are_not() {
        let e = |m: &str| parse_receipt(&json!({ "ok": false, "error": m }));
        assert_eq!(e("预览渲染忙，稍后再试"), Receipt::Retry);
        assert_eq!(e("预览渲染超时（3 秒）"), Receipt::Retry);
        assert_eq!(e("找不到主题"), Receipt::Failed);
    }

    #[test]
    fn batch_keeps_request_order_and_pads_missing_items() {
        let v = json!({ "items": [
            { "ok": false, "error": "x" },
            { "ok": true, "png": b64(PNG), "width": 240, "height": 64, "hash": "a" },
        ]});
        let out = parse_batch(&v, 3);
        assert_eq!(out[0], Receipt::Failed);
        assert_eq!(out[1], Receipt::Ready(raw("a")));
        assert_eq!(out[2], Receipt::Failed, "回执短了一截，缺的那格按失败");
    }

    #[test]
    fn whole_batch_failure_keeps_old_images() {
        let mut m = HashMap::new();
        m.insert(
            "default".to_string(),
            Thumb::Ready(raw("old").into_png(1.0)),
        );
        let all = ids(&["default", "jade"]);
        assert!(!merge_chunk(
            &mut m,
            &all,
            Some(Err("连接 core 失败".into())),
            1.0
        ));
        assert_eq!(ready(&m, "default").as_deref(), Some("old"), "旧图照留");
        assert_eq!(m["jade"], Thumb::Failed, "没图的格出占位");
        merge_chunk(&mut m, &all, None, 1.0);
        assert_eq!(
            ready(&m, "default").as_deref(),
            Some("old"),
            "工作线程 panic 同理"
        );
    }

    #[test]
    fn busy_item_keeps_old_image_and_asks_for_retry() {
        let mut m = HashMap::new();
        m.insert(
            "default".to_string(),
            Thumb::Ready(raw("old").into_png(1.0)),
        );
        let all = ids(&["default", "jade", "mono"]);
        let got = Some(Ok(vec![Receipt::Retry, Receipt::Retry, Receipt::Failed]));
        assert!(merge_chunk(&mut m, &all, got, 1.0));
        assert_eq!(ready(&m, "default").as_deref(), Some("old"));
        assert_eq!(m["jade"], Thumb::Pending, "忙的格等重试");
        assert_eq!(m["mono"], Thumb::Failed, "只有 ok:false 置失败");
        finish_round(&mut m, &all, true);
        assert_eq!(m["jade"], Thumb::Failed, "重试用完改占位");
        assert_eq!(ready(&m, "default").as_deref(), Some("old"));
    }

    #[test]
    fn finish_round_drops_themes_no_longer_listed() {
        let mut m = HashMap::new();
        m.insert("gone".to_string(), Thumb::Failed);
        m.insert("default".to_string(), Thumb::Pending);
        finish_round(&mut m, &ids(&["default"]), false);
        assert_eq!(m.len(), 1);
        assert_eq!(m["default"], Thumb::Pending, "还要重试的不动");
    }

    /// 显示名逐层退让：索引→id→显示名，每层缺了退上一层，全缺退 `None`。
    /// 原在外观页预览卡头链接下（2026-11 反馈后该链接撤了），随「当前主题」展示一起搬来。
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
        // 全没有 → 退 None（链接只显示「当前主题」）。
        assert_eq!(display_name(None, &[], &entries, None), None);
    }

    #[test]
    fn png_equality_compares_hash_and_scale_not_bytes() {
        let a = raw("h").into_png(1.0);
        let mut b = raw("h").into_png(1.0);
        b.bytes = Rc::from(&[1u8, 2, 3][..]);
        assert_eq!(a, b, "同哈希同缩放即相等");
        assert_ne!(a, raw("h").into_png(2.0));
        assert_ne!(a, raw("x").into_png(1.0));
        let c = raw("").into_png(1.0);
        assert_eq!(c, c.clone(), "无哈希时同一份字节相等");
        assert_ne!(c, raw("").into_png(1.0), "无哈希时不同份不相等");
    }

    #[test]
    fn current_theme_label_carries_name() {
        assert_eq!(current_theme_label(Some("墨")), "当前主题：墨");
        assert_eq!(current_theme_label(None), "当前主题");
        assert_eq!(current_theme_label(Some("")), "当前主题");
    }

    /// 反应式接线：选中索引一变，链接文案跟着变。走真树 + `layout_root`——
    /// `dispatch_reactive_updates` 是 reactive_effect 唯一的驱动点，只调闭包的话
    /// 「有没有真挂上 `.reactive()`」这一半就没人管（同 auto_encode 的测法）。
    #[test]
    fn current_theme_header_follows_selection() {
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
        let (label, header) = current_theme_header(&st);
        let mut tree = Tree::new();
        let id = header.build(&mut tree);
        tree.root = Some(id);
        assert_eq!(label.get(), "当前主题：清风", "构建时即算出当前文案");
        idx.set(1);
        tree.layout_root(Size::new(600, 100), &mut windui::text::NullTextEngine);
        assert_eq!(
            label.get(),
            "当前主题：墨",
            "选中索引变化应经 reactive_effect 触达链接文案"
        );
    }

    /// 链接落点 = **此刻选中格**的锚点键(运行期动态键 ui.theme.name:<id>);索引/取值表
    /// 拿不到时退配置值,再没有退空串(请求挂起等画廊到达)。
    #[test]
    fn theme_link_targets_the_selected_cell() {
        use crate::search::index::JumpTarget;
        let values = vec!["default".to_string(), "jade".to_string()];
        match theme_cell_target(Some(1), &values, None) {
            JumpTarget::Row { tab, anchor } => {
                assert_eq!(tab, crate::cli::page_to_index("theme").unwrap());
                assert_eq!(anchor, "ui.theme.name:jade", "落点应是选中那格");
            }
            other => panic!("落点应是行锚点,实为 {other:?}"),
        }
        // 索引/取值表拿不到 → 退配置值。
        match theme_cell_target(None, &[], Some("mono")) {
            JumpTarget::Row { anchor, .. } => assert_eq!(anchor, "ui.theme.name:mono"),
            other => panic!("落点应是行锚点,实为 {other:?}"),
        }
        // 都拿不到 → 空串键,请求挂起。
        match theme_cell_target(None, &[], None) {
            JumpTarget::Row { anchor, .. } => assert_eq!(anchor, "ui.theme.name:"),
            other => panic!("落点应是行锚点,实为 {other:?}"),
        }
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
        assert_eq!(fit_size(&png(960, 128, 200), 240, 120), (240, 32));
        assert_eq!(fit_size(&png(200, 50, 100), 240, 120), (200, 50));
        assert_eq!(fit_size(&png(200, 600, 100), 240, 120), (40, 120));
        assert_eq!(natural_size(&png(960, 128, 200)), (480, 64));
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
    }

    #[test]
    fn clamp_scale_stays_in_server_range() {
        assert_eq!(clamp_scale(0.0), 1.0, "还没画过按 1");
        assert_eq!(clamp_scale(1.5), 1.5);
        assert_eq!(clamp_scale(8.0), 4.0);
        assert_eq!(clamp_scale(0.25), 0.5);
    }

    #[test]
    fn mock_replies_are_inline_images() {
        let items = json!({ "items": [{ "name": "default" }, { "name": "no-such-theme" }] });
        let v = crate::rpc::mock_reply("theme.renderPreviews", &items);
        let out = parse_batch(&v, 2);
        assert!(matches!(out[0], Receipt::Ready(_)));
        assert_eq!(out[1], Receipt::Failed);
        let one = crate::rpc::mock_reply("theme.renderPreview", &json!({ "package": "/x.wtheme" }));
        assert!(matches!(parse_receipt(&one), Receipt::Ready(_)));
    }

    fn gallery_with(list: &[&str]) -> ThemeGallery {
        let g = ThemeGallery::new(Rc::new(NetTasks::default()));
        g.set_entries(
            list.iter()
                .map(|id| GalleryEntry {
                    id: id.to_string(),
                    display: id.to_string(),
                    builtin: true,
                    ..Default::default()
                })
                .collect(),
        );
        g
    }

    /// 在途时再标脏：不另发一批，只记一笔；这批回来后先合并、再重新防抖。
    #[test]
    fn marking_dirty_while_in_flight_queues_exactly_one_rerun() {
        let g = gallery_with(&["default", "jade"]);
        g.start_round(1.0);
        let Phase::Fetch(first) = g.phase.get() else {
            panic!("应有一批在途：{:?}", g.phase.get());
        };
        g.request(1.0);
        g.request(1.5);
        assert_eq!(g.phase.get(), Phase::Fetch(first), "在途时不另发");
        assert_eq!(g.pending.get(), Some(1.5), "只记最后一笔");

        let all = Rc::new(ids(&["default", "jade"]));
        g.chunk_done(
            all,
            0,
            1.0,
            false,
            Some(Ok(vec![Receipt::Ready(raw("a")), Receipt::Failed])),
        );
        assert_eq!(
            ready(&g.thumbs.get(), "default").as_deref(),
            Some("a"),
            "结果先并进去"
        );
        assert!(matches!(g.phase.get(), Phase::Debounce(_)), "随后重新防抖");
        assert_eq!(g.pending.get(), None);
    }

    /// 防抖中再标脏只是重新计时，不碰服务端。
    #[test]
    fn marking_dirty_during_debounce_restarts_the_timer() {
        let g = gallery_with(&["default"]);
        g.request(1.0);
        let Phase::Debounce(a) = g.phase.get() else {
            panic!()
        };
        g.request(1.0);
        let Phase::Debounce(b) = g.phase.get() else {
            panic!()
        };
        assert_ne!(a, b);
    }

    /// 多于一批时逐批接着取；全部取完回到空闲。
    #[test]
    fn rounds_walk_the_list_in_chunks_of_eight() {
        let names: Vec<String> = (0..10).map(|i| format!("t{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let g = gallery_with(&refs);
        g.start_round(1.0);
        let all = Rc::new(names.clone());
        let oks = |n: usize| Some(Ok((0..n).map(|_| Receipt::Failed).collect()));
        g.chunk_done(all.clone(), 0, 1.0, false, oks(CHUNK));
        assert!(matches!(g.phase.get(), Phase::Fetch(_)), "还有第二批");
        g.chunk_done(all, CHUNK, 1.0, false, oks(2));
        assert_eq!(g.phase.get(), Phase::Idle);
        assert_eq!(g.thumbs.get().len(), 10);
    }

    /// 整批出错就不再接着取后面的批，剩下没图的格直接出占位。
    #[test]
    fn broken_batch_stops_the_round() {
        let names: Vec<String> = (0..10).map(|i| format!("t{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let g = gallery_with(&refs);
        g.start_round(1.0);
        g.chunk_done(
            Rc::new(names),
            0,
            1.0,
            false,
            Some(Err("连接 core 失败".into())),
        );
        assert_eq!(g.phase.get(), Phase::Idle);
        assert!(g.thumbs.get().values().all(|t| *t == Thumb::Failed));
        assert_eq!(g.thumbs.get().len(), 10);
    }

    /// 「忙」的格隔一会儿重试，至多三次，之后改占位。
    #[test]
    fn busy_items_retry_at_most_three_times() {
        let g = gallery_with(&["default"]);
        let all = Rc::new(ids(&["default"]));
        for n in 1..=MAX_RETRIES {
            g.start_round(1.0);
            g.chunk_done(all.clone(), 0, 1.0, false, Some(Ok(vec![Receipt::Retry])));
            assert_eq!(g.retries.get(), n);
            assert_eq!(g.thumbs.get()["default"], Thumb::Pending);
        }
        g.start_round(1.0);
        g.chunk_done(all, 0, 1.0, false, Some(Ok(vec![Receipt::Retry])));
        assert_eq!(g.retries.get(), MAX_RETRIES);
        assert_eq!(g.thumbs.get()["default"], Thumb::Failed);
        g.invalidate();
        assert_eq!(g.retries.get(), 0, "外部标脏清零重试计数");
    }

    /// 点一格只改下拉的索引信号：不发 RPC、不动配置镜像。
    #[test]
    fn clicking_a_cell_only_moves_the_dropdown_index() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let st = AppState::empty();
        st.theme_value_sig.set(ids(&["default", "jade", "mono"]));
        let idx = signal(0usize);
        let before = st.current_config.get_path("ui.theme.name").cloned();
        let ctx = test_ctx(&st, idx, None);
        crate::rpc::take_mock_calls();
        ctx.select("jade");
        assert_eq!(idx.get(), 1);
        assert_eq!(ctx.selected().as_deref(), Some("jade"));
        assert!(crate::rpc::take_mock_calls().is_empty(), "不落盘、不发 RPC");
        assert_eq!(st.current_config.get_path("ui.theme.name").cloned(), before);
    }

    fn test_ctx(st: &AppState, index: Signal<usize>, export: Option<Vis>) -> CellCtx {
        CellCtx {
            index,
            values: st.theme_value_sig,
            thumbs: st.theme_gallery.thumbs,
            feedback: st.feedback,
            del_id: st.theme_delete_id,
            del_msg: st.theme_delete_msg,
            del_visible: st.theme_delete_visible,
            info: st.theme_gallery.info,
            info_visible: st.theme_gallery.info_visible,
            export,
            jump: st.jump,
        }
    }

    fn mono() -> GalleryEntry {
        GalleryEntry {
            id: "mono".into(),
            display: "墨".into(),
            version: "1.2".into(),
            author: "某人".into(),
            dir: "/u/themes/mono".into(),
            base: "_base".into(),
            skin: true,
            ..Default::default()
        }
    }

    fn menu_labels(export: Option<Vis>) -> Vec<String> {
        let st = AppState::empty();
        let ctx = test_ctx(&st, signal(0), export);
        cell_menu(&ctx, &mono())
            .into_iter()
            .map(|m| m.label)
            .collect()
    }

    #[test]
    fn menu_offers_info_and_delete_for_user_theme() {
        let l = menu_labels(None);
        assert!(l.iter().any(|s| s == "主题信息…"));
        let st = AppState::empty();
        let ctx = test_ctx(&st, signal(0), None);
        let builtin = GalleryEntry {
            builtin: true,
            ..mono()
        };
        let l: Vec<String> = cell_menu(&ctx, &builtin)
            .into_iter()
            .map(|m| m.label)
            .collect();
        assert!(!l.iter().any(|s| s == "删除主题…"), "内置主题不能删");
    }

    #[test]
    fn info_rows_list_source_dir_base_and_skin() {
        let rows = info_rows(&mono());
        let get = |k: &str| rows.iter().find(|(l, _)| *l == k).map(|(_, v)| v.as_str());
        assert_eq!(get("来源"), Some("用户导入"));
        assert_eq!(get("目录"), Some("/u/themes/mono"));
        assert_eq!(get("继承自"), Some("_base"));
        assert_eq!(get("异形皮肤"), Some("有"));
        assert_eq!(get("皮肤动画"), Some("无"));
        let builtin = info_rows(&GalleryEntry {
            id: "default".into(),
            display: "默认".into(),
            builtin: true,
            ..Default::default()
        });
        assert!(builtin.iter().any(|(k, v)| *k == "来源" && v == "内置"));
        assert!(
            !builtin.iter().any(|(k, _)| *k == "目录" || *k == "版本"),
            "空值不列"
        );
    }

    /// 删除选中的那个主题时，确认框说明会改选默认主题。
    #[test]
    fn deleting_the_selected_theme_says_it_falls_back() {
        let st = AppState::empty();
        st.theme_value_sig.set(ids(&["default", "mono"]));
        let ctx = test_ctx(&st, signal(1), None);
        ctx.ask_delete(&mono());
        assert!(st.theme_delete_visible.get());
        assert!(st.theme_delete_msg.get().contains("默认主题"));
        ctx.index.set(0);
        ctx.ask_delete(&mono());
        assert!(!st.theme_delete_msg.get().contains("默认主题"));
    }

    /// 点格子上的 ✕：弹删除确认，且不顺带把这一格选中（✕ 叠在可点的格子里）。
    #[test]
    fn clicking_the_delete_icon_asks_and_does_not_select() {
        use windui::core::Tree;
        use windui::event::{MouseButton, PointerEvent, PointerKind};
        use windui::geometry::{Point, Size};
        let st = AppState::empty();
        st.theme_value_sig.set(ids(&["default", "mono"]));
        let idx = signal(0usize);
        let ctx = test_ctx(&st, idx, None);
        let mut tree = Tree::new();
        let root = Element::col()
            .width(300)
            .child(cell(&ctx, mono()))
            .build(&mut tree);
        tree.root = Some(root);
        tree.layout_root(Size::new(300, 600), &mut windui::text::NullTextEngine);
        // ✕ 是格子里唯一 18×18 的节点。
        let mut icon = None;
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if let Some(n) = tree.get(id) {
                if n.bounds.w == 18 && n.bounds.h == 18 {
                    icon = Some(id);
                }
                stack.extend(n.children.iter().copied());
            }
        }
        let r = tree.abs_bounds(icon.expect("非内置主题的格子该有 ✕"));
        let at = Point::new(r.x + r.w / 2, r.y + r.h / 2);
        let (mut hover, mut capture) = (None, None);
        for kind in [PointerKind::Down, PointerKind::Up] {
            tree.dispatch_pointer(
                PointerEvent::single(kind, at, MouseButton::Left),
                &mut hover,
                &mut capture,
            );
        }
        assert!(st.theme_delete_visible.get(), "该弹删除确认");
        assert_eq!(st.theme_delete_id.get(), "mono");
        assert_eq!(idx.get(), 0, "点 ✕ 不该选中这一格");
    }

    /// 选中与否格子几何完全一致：外环恒占位、角标不进排版流。
    #[test]
    fn selection_does_not_move_the_cell() {
        use windui::core::Tree;
        use windui::geometry::Size;
        let st = AppState::empty();
        st.theme_value_sig.set(ids(&["default", "mono"]));
        let bounds = |selected: usize| {
            let ctx = test_ctx(&st, signal(selected), None);
            let mut tree = Tree::new();
            let root = Element::col()
                .width(300)
                .child(cell(&ctx, mono()))
                .build(&mut tree);
            tree.root = Some(root);
            tree.layout_root(Size::new(300, 600), &mut windui::text::NullTextEngine);
            let mut out = Vec::new();
            let mut stack = vec![root];
            while let Some(id) = stack.pop() {
                if let Some(n) = tree.get(id) {
                    out.push(n.bounds);
                    stack.extend(n.children.iter().copied());
                }
            }
            out
        };
        // 唯一的差别是选中格多出的「使用中」角标（不可见时零尺寸），其余每个节点原样不动。
        let shown = |v: Vec<Rect>| -> Vec<Rect> { v.into_iter().filter(|r| r.w > 0).collect() };
        let (plain, picked) = (shown(bounds(0)), shown(bounds(1)));
        assert_eq!(picked.len(), plain.len() + 1, "{plain:?}\n{picked:?}");
        assert!(
            plain.iter().all(|r| picked.contains(r)),
            "{plain:?}\n{picked:?}"
        );
    }

    #[test]
    fn export_menu_follows_theme_transfer_feature() {
        let has = |l: &[String]| l.iter().any(|s| s == "导出主题包…");
        assert!(has(&menu_labels(Some(Rc::new(|| true)))));
        assert!(
            !has(&menu_labels(Some(Rc::new(|| false)))),
            "用户藏了就不出"
        );
        assert!(!has(&menu_labels(None)), "定制版藏了就不建");
        assert!(menu_labels(None).iter().any(|s| s == "删除主题…"));
    }

    /// 本页不可见（或画廊被藏）时哨兵不发请求；切过来才发。
    #[test]
    fn no_request_until_the_page_is_shown() {
        use windui::core::Tree;
        use windui::geometry::Size;
        let st = AppState::empty();
        st.theme_gallery.set_entries(vec![GalleryEntry {
            id: "default".into(),
            display: "默认".into(),
            builtin: true,
            ..Default::default()
        }]);
        let shown = Rc::new(Cell::new(true));
        let s = shown.clone();
        let card = build_selector(&st, "ui.theme.name", Rc::new(move || s.get()));
        let mut tree = Tree::new();
        let root = Element::col()
            .width(800)
            .height(600)
            .child(card)
            .build(&mut tree);
        tree.root = Some(root);
        let layout =
            |t: &mut Tree| t.layout_root(Size::new(800, 600), &mut windui::text::NullTextEngine);

        st.tab.set(0);
        layout(&mut tree);
        assert_eq!(st.theme_gallery.phase.get(), Phase::Idle, "别的页不取");

        st.tab.set(crate::cli::page_to_index("theme").unwrap());
        shown.set(false);
        layout(&mut tree);
        assert_eq!(st.theme_gallery.phase.get(), Phase::Idle, "画廊被藏不取");

        shown.set(true);
        layout(&mut tree);
        assert!(matches!(st.theme_gallery.phase.get(), Phase::Debounce(_)));
    }

    /// 「当前主题」链接的跳转落点：整窗真树 + 真布局。滚到底后点链接（发请求、跑帧让
    /// 锚点认领、滚动落地），断言画廊行锚点回到视口内、且落点符合定位算法预期
    /// （向上最小滚动 + 24 边距 → 行顶对齐视口顶 + 24）。
    #[test]
    fn current_theme_link_lands_on_gallery_row() {
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
        // 真机窗口尺寸 + mock 主题列表（3 主题）：真机见到的画廊行高达数百 px。
        // 填列表让画廊真渲染网格，「滚到底」才构得出「目标行在视口外」。
        let (w, h) = (crate::MAIN_W, crate::MAIN_H);
        let root = crate::shell::build(&app)
            .width(w)
            .height(h)
            .build(&mut tree);
        tree.root = Some(root);
        {
            let mut m = app.borrow_mut();
            m.refresh_theme_names();
        }
        let st = app.borrow();
        st.tab.set(crate::cli::page_to_index("theme").unwrap());
        let layout = |t: &mut windui::core::Tree| {
            t.layout_root(Size::new(w, h), &mut windui::text::NullTextEngine);
        };
        layout(&mut tree);
        let aid = crate::search::anchor::anchor_node_id(&tree, "ui.theme.name")
            .expect("画廊行锚点应已挂载并跑过帧");
        // 页面滚动容器 = 锚点向上第一个 Scroll 先祖。
        let mut cur = tree.get(aid).and_then(|n| n.parent);
        let mut scroller = None;
        while let Some(i) = cur {
            let n = tree.get(i).expect("父链完整");
            if matches!(n.layout, windui::core::Layout::Scroll) {
                scroller = Some(i);
                break;
            }
            cur = n.parent;
        }
        let scroller = scroller.expect("页面应有滚动容器");
        // 滚到底：跳转前目标行完全在视口上方。
        tree.set_scroll_y(scroller, i32::MAX);
        layout(&mut tree);
        let before = tree.abs_bounds(aid);
        let vb = {
            let n = tree.get(scroller).expect("滚动容器在树上");
            let abs = tree.abs_bounds(scroller);
            windui::geometry::Rect::new(
                abs.x + n.padding.left,
                abs.y + n.padding.top,
                n.scroll_view().w,
                n.scroll_view().h,
            )
        };
        let fully_visible = |r: windui::geometry::Rect| r.y >= vb.y && r.bottom() <= vb.bottom();
        assert!(
            !fully_visible(before),
            "前置条件：滚到底后画廊行不应完整可见（行 {before:?} 视口 {vb:?}）"
        );
        // 点「当前主题」链接 = 发跳转请求(现算目标 = 选中格)。
        let target = theme_cell_target(
            st.fields
                .select_indices
                .borrow()
                .get("ui.theme.name")
                .map(|s| s.get()),
            &st.theme_value_sig.get(),
            st.current_config.get_str("ui.theme.name"),
        );
        crate::search::jump::request_quiet(st.tab, st.jump, target.clone());
        // 置位帧 → 认领帧 → 滚动落地帧。
        for _ in 0..3 {
            layout(&mut tree);
        }
        // 断言口径 = **选中格**:锚点键是 target 里现算出的那把。
        let crate::search::index::JumpTarget::Row {
            anchor: cell_key, ..
        } = &target
        else {
            panic!("落点应是行锚点");
        };
        let cell = crate::search::anchor::anchor_node_id(&tree, cell_key)
            .expect("选中格锚点应已挂载并跑过帧");
        let after = tree.abs_bounds(cell);
        assert!(
            fully_visible(after),
            "跳转后选中格应完整落在视口内;格 {after:?} vs 视口 {vb:?}"
        );
    }

    /// 「当前主题」跳转 × 主题列表加载竞态：列表未到（画廊行矮）就点链接，锚点认领
    /// 滚到矮行；列表到达后行变高，跟随窗口（FOLLOW_MS 内 content_h 一变就重新对准）
    /// 必须把落点修正到高行的正确位置——真机「打开设置马上点链接」正是这个时序。
    #[test]
    fn jump_before_theme_list_arrives_still_lands_on_row() {
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
        // 模拟真实场景:配置里已有当前主题(列表未到时现算目标要用它退让)。
        app.borrow_mut()
            .current_config
            .set_path("ui.theme.name", serde_json::json!("default"));
        let st = app.borrow();
        st.tab.set(crate::cli::page_to_index("theme").unwrap());
        let layout = |t: &mut windui::core::Tree| {
            t.layout_root(Size::new(w, h), &mut windui::text::NullTextEngine);
        };
        layout(&mut tree);
        let aid = crate::search::anchor::anchor_node_id(&tree, "ui.theme.name")
            .expect("画廊行锚点应已挂载并跑过帧");
        let scroller = {
            let mut cur = tree.get(aid).and_then(|n| n.parent);
            let mut found = None;
            while let Some(i) = cur {
                let n = tree.get(i).expect("父链完整");
                if matches!(n.layout, windui::core::Layout::Scroll) {
                    found = Some(i);
                    break;
                }
                cur = n.parent;
            }
            found.expect("页面应有滚动容器")
        };
        // 列表还空:滚到底。
        tree.set_scroll_y(scroller, i32::MAX);
        layout(&mut tree);
        // 列表未到就点链接。
        let target = theme_cell_target(
            st.fields
                .select_indices
                .borrow()
                .get("ui.theme.name")
                .map(|s| s.get()),
            &st.theme_value_sig.get(),
            st.current_config.get_str("ui.theme.name"),
        );
        crate::search::jump::request_quiet(st.tab, st.jump, target.clone());
        for _ in 0..3 {
            layout(&mut tree);
        }
        // 列表此刻到达：行变高。跟随窗口应当重新对准。
        drop(st);
        app.borrow_mut().refresh_theme_names();
        // 重排帧 + 跟随重滚帧 + 落地帧。
        for _ in 0..3 {
            layout(&mut tree);
        }
        let crate::search::index::JumpTarget::Row {
            anchor: cell_key, ..
        } = &target
        else {
            panic!("落点应是行锚点");
        };
        let cell = crate::search::anchor::anchor_node_id(&tree, cell_key)
            .expect("选中格锚点应已挂载并跑过帧");
        let b = tree.abs_bounds(cell);
        let n = tree.get(scroller).expect("滚动容器在树上");
        let abs = tree.abs_bounds(scroller);
        let vb = windui::geometry::Rect::new(
            abs.x + n.padding.left,
            abs.y + n.padding.top,
            n.scroll_view().w,
            n.scroll_view().h,
        );
        assert!(
            b.y >= vb.y - 1 && b.bottom() <= vb.bottom() + 1,
            "列表到达后选中格应完整落在视口内;格 {b:?} vs 视口 {vb:?}"
        );
    }
}
