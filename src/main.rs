#![cfg_attr(windows, windows_subsystem = "windows")]

//! wind_setting — 清风输入法原生设置应用（基于 windui）。
//!
//! 通过 JSON-RPC over named pipe 与核心服务通信，不改动 core 接口。
//! 无边框窗口 + 自定义标题栏，7 Tab 清单驱动（manifest 分组），运行时明暗换肤。

mod add_word_window;
mod capabilities;
mod cli;
mod config;
mod dialogs;
mod icons;
mod key_conflict;
mod logger;
mod manifest;
mod mix_fields;
mod mix_members;
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
#[allow(dead_code)]
mod update;
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

    // 快捷加词小窗：独立单例、紧凑窗，复用词库加词对话框；不落入常规设置界面路径。
    if let Some(params) = cli::parse_add_word(&args) {
        add_word_window::run(params);
        return;
    }

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
        // 词库页深链（--schema/--type）：此刻方案列表尚未就绪，故只挂起，
        // 待 apply_loaded 注入方案后再定位（见 AppState::route_dict_target）。
        if let Some(t) = cli::parse_dict_target(&args) {
            st.route_dict_target(t);
        }
        // 冷启动协议 URL：解析并路由（切相关页 + 占位提示）。
        if let Some(raw) = cli::parse_protocol(&args) {
            drop(st);
            route_protocol(&raw, &app_state);
        }
    }
    if app_state.borrow().setting_config.theme_editor.auto_start {
        app_state.borrow().start_theme_editor_server(true);
    }
    if let Some(w) = app_state.borrow().setting_config_warning.clone() {
        app_state.borrow().feedback.set(Some((ToastKind::Info, w)));
    }
    let start_dark = app_state.borrow().dark.get();

    // 渲染：默认 Direct2D GPU 加速（windui d2d 后端，已默认编入）；远程会话 / 无可用
    // GPU 时由 windui 自动回退软渲染（不 panic）。命令行 --soft / --no-accel 强制软渲染。
    let soft_render = args.iter().any(|a| a == "--soft" || a == "--no-accel");
    tracing::info!(
        "渲染模式: {}",
        if soft_render {
            "软件 (tiny-skia)"
        } else {
            "Direct2D 加速"
        }
    );

    // 单实例 + 二次运行激活/切页/协议（app_id 含变体后缀，dev/release 互不干扰）。
    let app_id = format!("wind_setting{}", mode::pipe_suffix());
    let single_instance_state = app_state.clone();
    let mut app = App::new("清风输入法设置", 820, 640)
        .single_instance(app_id, move |argv| {
            if let Some(raw) = cli::parse_protocol(&argv) {
                route_protocol(&raw, &single_instance_state);
            } else if let Some(idx) = cli::parse_target(&argv) {
                let st = single_instance_state.borrow();
                st.tab.set(idx);
                // 已运行的实例通常方案列表早已就绪，深链会立即生效；若恰好撞上启动
                // 头几十毫秒数据还没回来，route_dict_target 会自动改为挂起。
                if let Some(t) = cli::parse_dict_target(&argv) {
                    st.route_dict_target(t);
                }
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

    // core 事件订阅：控制通道是请求-响应的，问不到 core 自发的状态变更（用户用快捷键
    // 或托盘切了方案等）。这里订阅单向事件管道，让界面跟着变——否则「当前方案」这类
    // 显示只在打开页面那一刻正确，之后会变成看起来权威的错误信息。
    // 监听线程内阻塞读 + 断线重连，故与升级通道一样经 channel 回 UI 线程写 Signal。
    let events_state = app_state.clone();
    let events_tx = app.channel::<rpc::CoreEvent>(move |ev| {
        events_state.borrow_mut().on_core_event(&ev);
    });
    std::thread::spawn(move || {
        rpc::listen_events(|ev| {
            // 发送失败 = 窗口已关闭，监听线程随进程退出即可。
            let _ = events_tx.send(ev);
        });
    });

    // 升级通道：下载线程不能直接写 Signal（Signal 是线程局部 arena 句柄，非 Send），
    // 故所有升级进度/结果都经此 channel 回到 UI 线程。channel 只能在 App 构建期注册，
    // 因此这里建好后把 Sender 存进 AppState 供各按钮取用。
    let update_state = app_state.clone();
    let update_tx = app.channel::<update::UpdateMsg>(move |msg| {
        update_state.borrow().on_update_msg(msg);
    });
    app_state.borrow().set_update_tx(update_tx);

    // 启动静默检查更新：三重门控（已同意联网 + 未关自动检查 + 距上次超 24h）。
    // 无更新或失败都不打扰用户，只有确实有新版本且未被「跳过此版本」屏蔽才弹对话框。
    {
        let now = chrono::Local::now().timestamp();
        let cfg = app_state.borrow().setting_config.update.clone();
        let forced = update::force_check_requested(&args);
        // 强制检查仍受 network_consent 门控 —— 调试后门不该越过联网同意
        let gate = if forced {
            cfg.network_consent
        } else {
            update::should_auto_check(cfg.network_consent, cfg.auto_check, cfg.last_check_at, now)
        };
        if forced && !cfg.network_consent {
            tracing::warn!("请求了强制检查更新，但用户尚未同意联网，已跳过");
        }
        if gate {
            if forced {
                // 刻意不写 last_check_at：调试用的强制检查不该消耗掉真实的节流窗口，
                // 否则跑一次调试就会让接下来 24h 的自动检查失效，反而更难验证。
                tracing::warn!("强制检查更新（调试入口）：已绕过 24h 节流与自动检查开关");
            } else {
                // 发起前就记时间：失败也算"查过"，免得每次开设置都重试
                app_state.borrow_mut().mark_update_checked(now);
            }
            app_state.borrow().spawn_silent_update_check();
        }
    }

    let ui = shell::build(&app_state, th);

    app.bg(theme_def::theme_for(start_dark).palette.bg)
        // `--screenshot <path>` 离屏出图（配合 WIND_RPC_MOCK=1 可无 core 核对 UI）。
        .screenshot_from_args()
        .content(ui)
        .run();
}

/// 协议 URL 路由：解析 → 切到相关页；主题导入暂存到待确认状态并弹出导入对话框，
/// 其它 kind 提示暂不支持。反馈走 feedback（toast），status 只保留连接态。
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
                st.set_pending_theme_import(req.url, req.name);
                st.theme_import_dialog_visible.set(true);
                st.feedback.set(Some((
                    ToastKind::Info,
                    "收到主题导入请求，请确认来源后导入".to_string(),
                )));
            } else {
                st.clear_pending_theme_import();
                st.theme_import_dialog_visible.set(false);
                st.feedback.set(Some((
                    ToastKind::Info,
                    format!("暂不支持通过协议导入 {}", req.kind),
                )));
            }
        }
        Err(e) => {
            let st = state.borrow();
            st.clear_pending_theme_import();
            st.theme_import_dialog_visible.set(false);
            st.feedback
                .set(Some((ToastKind::Error, format!("协议解析失败：{e}"))));
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
        assert_eq!(
            st.pending_theme_import_url.get(),
            "https://example.com/dark.toml"
        );
        assert_eq!(st.pending_theme_import_name.get(), "Dark");
        assert!(st.theme_import_dialog_visible.get());
        assert_eq!(
            st.feedback.get(),
            Some((
                ToastKind::Info,
                "收到主题导入请求，请确认来源后导入".to_string()
            ))
        );
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
        assert_eq!(
            st.feedback.get(),
            Some((ToastKind::Info, "暂不支持通过协议导入 schema".to_string()))
        );
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
        let (kind, msg) = st.feedback.get().expect("应有反馈");
        assert_eq!(kind, ToastKind::Error);
        assert!(msg.starts_with("协议解析失败："));
        assert_eq!(st.pending_theme_import_url.get(), "");
        assert_eq!(st.pending_theme_import_name.get(), "");
        assert!(!st.theme_import_dialog_visible.get());
    }
}
