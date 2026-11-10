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
/// 图区固定高度：加载与换图时布局不随之涨缩，进页不抖。160 与主题画廊导入预览的
/// `IMPORT_PREVIEW_MAX_H`（theme_gallery.rs）一致。
const AREA_H: i32 = 160;
/// 草稿里影响候选窗外观的段（`ui.<段>`）；工具栏、状态提示等改了不重取。
pub const DRAFT_SECTIONS: &[&str] = &["theme", "candidate", "font", "comment"];

/// `ui.theme.style` 是不是「跟随系统」：同 core `ThemeStyle::from_config`，不是 light / dark 都算。
pub fn follows_system(style: Option<&str>) -> bool {
    !matches!(
        style.map(|s| s.trim().to_lowercase()).as_deref(),
        Some("light" | "dark")
    )
}

/// 屏上显示的那一张。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Shown {
    Waiting,
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

/// 一次请求要带的参数：草稿 `ui` 子树 + 缩放。
pub fn request_params(ui: &Value, scale: f32) -> Value {
    json!({ "ui": ui, "scale": scale, "inline": true })
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

/// 一次取图要比对的全部输入：草稿那几段、缩放 ×100、主题列表版本、编辑器推送次数、活跃方案。
#[derive(Clone, Debug, PartialEq)]
struct Key {
    ui: Value,
    scale_pct: u32,
    themes_rev: u64,
    external_edits: u64,
    schema: String,
    /// 草稿明暗跟随系统时的系统切换次数；不跟随时恒 0（系统切了也不重取）。
    system_flips: u64,
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
}

impl Watcher {
    fn request(&self, key: &Key, scale: f32) {
        if let Some(t) = self.task.take() {
            self.net.cancel(t);
        }
        let (shown, error, task) = (self.shown, self.error, self.task.clone());
        let (retry_tick, retried, offline, net) = (
            self.retry_tick,
            self.retried.clone(),
            self.offline.clone(),
            self.net.clone(),
        );
        let params = request_params(&key.ui, scale);
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
                let msg = match r {
                    Outcome::Ready(raw) => {
                        retried.set(0);
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
        };
        let tick = self.retry_tick.get();
        let retrying = tick != self.sent_tick.get();
        if self.sent.borrow().as_ref() == Some(&key) && !retrying {
            return;
        }
        if !retrying {
            // 输入真变了：重试计数从头算。
            self.retried.set(0);
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

/// 「效果预览」卡片。
pub fn build_card(app: &Rc<RefCell<AppState>>, state: &AppState) -> Element {
    let shown: Signal<Shown> = signal(Shown::Waiting);
    let error: Signal<String> = signal(String::new());
    let image = Element::switcher(shown, |s: Shown| match s {
        Shown::Ready(png) => {
            // 只取自然宽度作上界；`height_match()` 填充定高图区，布局不随图片高度变。
            let (w, _) = crate::theme_gallery::natural_size(&png);
            Element::image_bytes(&png.bytes)
                .fit(Fit::Contain)
                .width_match()
                .max_width(w.max(1))
                .height_match()
        }
        Shown::Waiting => Element::stack().width_match().height_match().child(
            Element::label("预览生成中…")
                .font_size(12.0)
                .fg_role(Role::TextSubtle)
                .max_lines(1)
                .align(Align::Center),
        ),
    });
    let area = Element::stack()
        .width_match()
        .height(AREA_H)
        .padding_xy(16, 16)
        .bg_role(Role::SurfaceAlt)
        .corner(6.0)
        .child(image.height_match().align(Align::Center));
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
    let card = crate::widgets::section_items(
        SECTION_TITLE,
        vec![crate::widgets::CardItem::Bare(crate::widgets::card_pad(
            body,
        ))],
    );
    crate::search::anchor::wrap(card, ANCHOR, state.jump)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_carry_draft_ui_inline() {
        let ui = json!({ "candidate": { "font_size": 20 } });
        let p = request_params(&ui, 1.5);
        assert_eq!(p["ui"], ui);
        assert_eq!(p["scale"], 1.5);
        assert_eq!(p["inline"], true);
        assert!(p.get("name").is_none(), "主题取草稿里的 ui.theme.name");
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
            &request_params(&ui, 1.0),
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
