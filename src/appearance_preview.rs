//! 外观页顶部的「当前效果」：草稿配置的 `ui` 子树交给 core 出一张候选窗预览
//! （`theme.renderPreview` 的 `ui` 参数，协议见 WindInput `docs/design/wind-scene-extraction.md` §7.2），
//! 改了字号、字体、每页候选数这类设置，不必保存就能看到效果。
//!
//! - **草稿**：与按键冲突检测同一取法——`current_config` 的副本跑一遍全部写回器
//!   （[`AppState::draft_config`]）。只在外观页可见时算，草稿的 `ui` 子树没变就不发请求。
//! - **防抖、只认最新**：草稿一变就取消上一个还没回来的请求、重新计时 [`DEBOUNCE`]；
//!   取消的请求回执直接丢掉（[`NetTasks::cancel`] 摘掉续体），屏上永远是最后一次的结果。
//! - **失败不崩**：服务端不在、出图失败时保留上一张图，下方显示一行简短原因；不自动重发，
//!   下一次草稿变化再请求。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use serde_json::{Value, json};
use windui::core::{EventCtx, Widget};
use windui::geometry::Rect;
use windui::prelude::*;
use windui::render::Canvas;
use windui::style::Style;

use crate::net_task::{NetTasks, TaskId};
use crate::state::AppState;
use crate::theme_gallery::{Png, Receipt};

/// 卡片标题（二级导航里也叫这个）。
pub const SECTION_TITLE: &str = "当前效果";
/// 搜索锚点。
pub const ANCHOR: &str = "ui:appearance-preview";
/// 草稿变化到发请求之间至少隔这么久。
const DEBOUNCE: Duration = Duration::from_millis(200);
/// 图区最小高度：图还没来时卡片不至于塌成一条。
const AREA_MIN_H: i32 = 72;

/// 屏上显示的那一张。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Shown {
    Waiting,
    Ready(Png),
}

/// 一次请求要带的参数：草稿 `ui` 子树 + 缩放。
pub fn request_params(ui: &Value, scale: f32) -> Value {
    json!({ "ui": ui, "scale": scale, "inline": true })
}

/// 回执 → 屏上的图或一行错误。整次 RPC 失败（服务端不在）也是错误。
fn outcome(r: Result<Value, String>) -> Result<crate::theme_gallery::RawPng, String> {
    match r.map(|v| crate::theme_gallery::parse_receipt(&v)) {
        Ok(Receipt::Ready(raw)) => Ok(raw),
        Ok(Receipt::Retry) => Err("预览忙，稍后会再试".into()),
        Ok(Receipt::Failed) => Err("暂时无法生成预览".into()),
        Err(e) => {
            tracing::debug!("外观预览取图失败：{e}");
            Err("暂时无法生成预览（输入法服务未连接）".into())
        }
    }
}

/// 页内哨兵：每次更新时看外观页是否可见、草稿是否变了，变了就重新计时取图；绘制时记下缩放。
struct Watcher {
    app: Rc<RefCell<AppState>>,
    net: Rc<NetTasks>,
    tab: Signal<usize>,
    page: usize,
    shown: Signal<Shown>,
    error: Signal<String>,
    /// 最近一次发出去的（草稿 `ui` 子树, 缩放 ×100）。
    sent: Rc<RefCell<Option<(Value, u32)>>>,
    task: Rc<Cell<Option<TaskId>>>,
    scale: Cell<f32>,
}

impl Watcher {
    fn request(&self, ui: Value, scale: f32) {
        if let Some(t) = self.task.take() {
            self.net.cancel(t);
        }
        let (shown, error, task) = (self.shown, self.error, self.task.clone());
        let params = request_params(&ui, scale);
        let id = self.net.spawn(
            move |cancel| {
                crate::theme_gallery::after_debounce(cancel, DEBOUNCE, || {
                    outcome(crate::rpc::call_core("theme.renderPreview", &params))
                })
            },
            move |r: Option<Option<Result<crate::theme_gallery::RawPng, String>>>| {
                task.set(None);
                match r.flatten() {
                    Some(Ok(raw)) => {
                        shown.set(Shown::Ready(raw.into_png(scale)));
                        if !error.with(String::is_empty) {
                            error.set(String::new());
                        }
                    }
                    // 不自动重发：错误提示一显示就会触发重绘，忘掉这一版会在服务端不在时
                    // 原地空转。下一次草稿变化自然会再请求。
                    Some(Err(e)) => error.set(e),
                    None => {}
                }
            },
        );
        self.task.set(Some(id));
    }
}

impl Widget for Watcher {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
        if self.tab.get() != self.page {
            return;
        }
        let Ok(st) = self.app.try_borrow() else {
            return;
        };
        let ui = st
            .draft_config()
            .get_path("ui")
            .cloned()
            .unwrap_or(Value::Null);
        drop(st);
        let scale = crate::theme_gallery::clamp_scale(self.scale.get());
        let key = (ui, crate::theme_gallery::scale_pct(scale));
        if self.sent.borrow().as_ref() == Some(&key) {
            return;
        }
        let ui = key.0.clone();
        *self.sent.borrow_mut() = Some(key);
        self.request(ui, scale);
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

/// 「当前效果」卡片。
pub fn build_card(app: &Rc<RefCell<AppState>>, state: &AppState) -> Element {
    let shown: Signal<Shown> = signal(Shown::Waiting);
    let error: Signal<String> = signal(String::new());
    let image = Element::switcher(shown, |s: Shown| match s {
        Shown::Ready(png) => {
            let (w, h) = crate::theme_gallery::natural_size(&png);
            Element::image_bytes(&png.bytes)
                .fit(Fit::Contain)
                .width_match()
                .max_width(w.max(1))
                .height(h.max(1))
        }
        Shown::Waiting => Element::stack().width_match().height(AREA_MIN_H).child(
            Element::label("预览生成中…")
                .font_size(12.0)
                .fg_role(Role::TextSubtle)
                .max_lines(1)
                .align(Align::Center),
        ),
    });
    let area = Element::stack()
        .width_match()
        .padding_xy(16, 16)
        .bg_role(Role::SurfaceAlt)
        .corner(6.0)
        .child(image.align(Align::Center));
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
            shown,
            error,
            sent: Rc::new(RefCell::new(None)),
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
    fn failures_become_short_messages() {
        assert!(
            outcome(Err("连接 core 失败".into()))
                .unwrap_err()
                .contains("未连接")
        );
        let busy = outcome(Ok(json!({ "ok": false, "error": "预览渲染忙" })));
        assert!(busy.unwrap_err().contains("忙"));
        assert!(outcome(Ok(json!({ "ok": false, "error": "x" }))).is_err());
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
        assert_eq!(r.unwrap(), jade.unwrap());
    }
}
