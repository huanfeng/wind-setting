#![cfg_attr(windows, windows_subsystem = "windows")]

//! wind_setting — 清风输入法原生设置应用（基于 windui）。
//!
//! 通过 JSON-RPC over named pipe 与核心服务通信，不改动 core 接口。
//! 无边框窗口 + 自定义标题栏，7 Tab 清单驱动（manifest 分组），运行时明暗换肤。

mod capabilities;
mod cli;
mod config;
mod dialogs;
mod icons;
mod logger;
mod manifest;
mod mode;
mod pages;
mod protocol;
mod rpc;
#[allow(dead_code)]
mod setting_config;
mod shell;
mod state;
mod theme_def;
#[allow(dead_code)]
mod theme_editor_server;
#[allow(dead_code)]
mod theme_import;
mod widgets;

use std::cell::RefCell;
use std::rc::Rc;

use windui::prelude::*;

/// 同步拉取数据并 apply；连接不存在时 fast-fail 立即返回。重连复用此入口。
///
/// 设置操作低频，故直接在调用线程同步 fetch（无后台线程）；
/// 若重连后仍失败，`apply_loaded` 会再次把 `conn_failed` 置 true，对话框保持。
pub fn main_reconnect(app: &Rc<RefCell<state::AppState>>) {
    let data = state::LoadedState::fetch();
    app.borrow_mut().apply_loaded(data);
}

fn main() {
    // mode::init 必须最先执行：logger 的目录路径和 RPC 管道名均依赖它。
    let args: Vec<String> = std::env::args().collect();
    mode::init(&args);
    logger::init();
    protocol::self_heal_registration();

    // 快速初始化：仅读系统主题（~86µs），不阻塞任何 RPC，UI 立即可显示。
    let app_state = Rc::new(RefCell::new(state::AppState::empty()));
    {
        let st = app_state.borrow();
        if args.iter().any(|a| a == "--light") {
            st.dark.set(false);
        } else if args.iter().any(|a| a == "--dark") {
            st.dark.set(true);
        }
        // 初始页：--page <id> / --page=<id> / 快捷 --<id> / --tab <n>。
        if let Some(idx) = cli::parse_target(&args) {
            st.tab.set(idx);
        }
        // 冷启动协议 URL：解析并路由（切相关页 + 占位提示）。
        if let Some(raw) = cli::parse_protocol(&args) {
            drop(st);
            route_protocol(&raw, &app_state);
        }
    }
    let start_dark = app_state.borrow().dark.get();

    // 渲染：默认 Direct2D GPU 加速（windui d2d 后端，已默认编入）；远程会话 / 无可用
    // GPU 时由 windui 自动回退软渲染（不 panic）。命令行 --soft / --no-accel 强制软渲染。
    let soft_render = args.iter().any(|a| a == "--soft" || a == "--no-accel");
    tracing::info!(
        "渲染模式: {}",
        if soft_render { "软件 (tiny-skia)" } else { "Direct2D 加速" }
    );

    // 单实例 + 二次运行激活/切页/协议（app_id 含变体后缀，dev/release 互不干扰）。
    let app_id = format!("wind_setting{}", mode::pipe_suffix());
    let single_instance_state = app_state.clone();
    let mut app = App::new("清风输入法设置", 820, 640)
        .single_instance(app_id, move |argv| {
            if let Some(raw) = cli::parse_protocol(&argv) {
                route_protocol(&raw, &single_instance_state);
            } else if let Some(idx) = cli::parse_target(&argv) {
                single_instance_state.borrow().tab.set(idx);
            }
        })
        .frameless()
        .centered()
        .accelerated(!soft_render)
        .min_size(560, 440)
        .theme(theme_def::theme_for(start_dark));
    let th = app.theme_handle();

    // 注册后台→UI 通道：数据就绪后在 UI 线程回调，更新 Signal 刷新界面。
    let app_state_cb = app_state.clone();
    let tx = app.channel::<state::LoadedState>(move |data| {
        app_state_cb.borrow_mut().apply_loaded(data);
    });

    // 后台线程并行执行全部 RPC（fast-fail，管道不存在时几乎立即返回）。
    std::thread::spawn(move || {
        let data = state::LoadedState::fetch();
        if tx.send(data).is_err() {
            tracing::warn!("窗口已关闭，后台数据发送取消");
        }
    });

    let ui = shell::build(&app_state, th);

    app.bg(theme_def::theme_for(start_dark).palette.bg)
        // `--screenshot <path>` 离屏出图（配合 WIND_RPC_MOCK=1 可无 core 核对 UI）。
        .screenshot_from_args()
        .content(ui)
        .run();
}

/// 协议 URL 路由：解析 → 切到相关页；主题导入暂存到待确认状态，具体 UI 留后续接线。
fn route_protocol(raw: &str, state: &Rc<RefCell<state::AppState>>) {
    match protocol::parse_protocol_url(raw) {
        Ok(req) => {
            let st = state.borrow();
            let idx = if req.kind == "theme" { 3 } else { 4 };
            st.tab.set(idx);
            tracing::info!(
                "协议导入请求: kind={} url={} name={}",
                req.kind,
                req.url,
                req.name
            );
            if req.kind == "theme" {
                st.pending_theme_import_url.set(req.url);
                st.pending_theme_import_name.set(req.name);
                st.theme_import_dialog_visible.set(true);
                st.status.set("收到主题导入请求，请确认来源后导入".to_string());
            } else {
                st.pending_theme_import_url.set(String::new());
                st.pending_theme_import_name.set(String::new());
                st.theme_import_dialog_visible.set(false);
                st.status.set(format!("暂不支持通过协议导入 {}", req.kind));
            }
        }
        Err(e) => {
            let st = state.borrow();
            st.pending_theme_import_url.set(String::new());
            st.pending_theme_import_name.set(String::new());
            st.theme_import_dialog_visible.set(false);
            st.status.set(format!("协议解析失败：{e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_protocol_stores_theme_import_request() {
        let state = Rc::new(RefCell::new(state::AppState::empty()));

        route_protocol(
            "windinput://import/theme?url=https%3A%2F%2Fexample.com%2Fdark.toml&name=Dark",
            &state,
        );

        let st = state.borrow();
        assert_eq!(st.tab.get(), 3);
        assert_eq!(st.pending_theme_import_url.get(), "https://example.com/dark.toml");
        assert_eq!(st.pending_theme_import_name.get(), "Dark");
        assert!(st.theme_import_dialog_visible.get());
        assert_eq!(st.status.get(), "收到主题导入请求，请确认来源后导入");
    }

    #[test]
    fn route_protocol_reports_unsupported_non_theme_import() {
        let state = Rc::new(RefCell::new(state::AppState::empty()));

        route_protocol(
            "windinput://import/theme?url=https%3A%2F%2Fexample.com%2Fdark.toml&name=Dark",
            &state,
        );
        route_protocol(
            "windinput://import/schema?url=https%3A%2F%2Fexample.com%2Fschema.yaml&name=Schema",
            &state,
        );

        let st = state.borrow();
        assert_eq!(st.tab.get(), 4);
        assert_eq!(st.pending_theme_import_url.get(), "");
        assert_eq!(st.pending_theme_import_name.get(), "");
        assert!(!st.theme_import_dialog_visible.get());
        assert_eq!(st.status.get(), "暂不支持通过协议导入 schema");
    }

    #[test]
    fn route_protocol_reports_parse_error() {
        let state = Rc::new(RefCell::new(state::AppState::empty()));

        route_protocol(
            "windinput://import/theme?url=https%3A%2F%2Fexample.com%2Fdark.toml&name=Dark",
            &state,
        );
        route_protocol("windinput://import/theme", &state);

        let st = state.borrow();
        assert!(st.status.get().starts_with("协议解析失败："));
        assert_eq!(st.pending_theme_import_url.get(), "");
        assert_eq!(st.pending_theme_import_name.get(), "");
        assert!(!st.theme_import_dialog_visible.get());
    }
}
