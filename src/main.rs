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
mod dict_window;
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
mod user_dir;
mod widgets;

use std::cell::RefCell;
use std::rc::Rc;

use windui::prelude::*;

/// 没有 `EventCtx` 的回调（`single_instance` 的 argv 处理）送往 UI 线程的请求。
///
/// 之所以要绕这一道：开窗请求只在副作用被采纳的时机才算数（控件事件回调、
/// `App::channel` 的 `on_message`、`on_interval`），而 `single_instance` 的回调虽然也在
/// UI 线程，却只收 argv、拿不到 ctx。经 channel 转一手，回调那侧就有 ctx 了。
pub enum UiRequest {
    /// 打开词库管理窗（实际开不开取决于 `take_dict_window_request`）。
    OpenDictWindow,
}

/// 把 `--page` / `--tab` 解出的目标下标分派到「切页」或「开词库窗」。
///
/// `dict` 已不是一个页：词库整体迁进了独立窗，主窗那一格只剩个永不显示的占位
/// （见 `pages::dict_page`），`tab.set(dict_idx)` 只会得到一屏空白。
///
/// `notify` 透传给 [`state::AppState::request_dict_window`]：冷启动传 `false`（等
/// `apply_loaded` 注入方案后再开），二次实例传 `true`（数据早已就绪，立刻推一帧）。
///
/// 冷启动与二次实例共用此入口 —— 两处各写一份 if 的话，改一边漏一边，而漏掉的那条
/// 路径的症状是「点了词库管理，设置窗跳出来却是一片空白」。
fn route_target(st: &state::AppState, idx: usize, notify: bool) {
    if Some(idx) == cli::page_to_index("dict") {
        st.request_dict_window(notify);
    } else {
        st.tab.set(idx);
    }
}

/// 同步拉取数据并 apply；连接不存在时 fast-fail 立即返回。重连复用此入口。
///
/// 设置操作低频，故直接在调用线程同步 fetch（无后台线程）；
/// 若重连后仍失败，`apply_loaded` 会再次把 `conn_failed` 置 true，对话框保持。
pub fn main_reconnect(app: &Rc<RefCell<state::AppState>>) {
    let data = state::LoadedState::fetch();
    app.borrow_mut().apply_loaded(data);
}

/// 单实例 app_id（含变体后缀，dev/release 互不干扰）。主窗口与加词小窗是两个独立单例，
/// 各占一个 id —— 加词小窗开着时点「设置」，两者互不顶替。
///
/// `claim_instance` 与 `App::single_instance` 必须用同一个 id，故收拢在此。
fn single_instance_app_id(add_word: bool) -> String {
    let suffix = mode::pipe_suffix();
    if add_word {
        format!("wind_setting_addword{suffix}")
    } else {
        format!("wind_setting{suffix}")
    }
}

fn main() {
    // mode::init 必须最先执行：logger 的目录路径和 RPC 管道名均依赖它。
    let args: Vec<String> = std::env::args().collect();
    mode::init(&args);

    // 单实例闸门：设置程序已开着时，本进程的使命就只是把 argv（深链页、协议 URL）递给
    // 首实例，然后死掉。故仲裁要赶在任何启动动作之前 —— windui 在 App::run 里也会仲裁，
    // 但那时这个注定要退场的进程已经轮转过日志、发了七八个 RPC、订阅了 core 事件通道，
    // theme_editor.auto_start 开着的话还会去抢端口。这些副作用全落在首实例头上。
    let add_word = cli::parse_add_word(&args);
    if windui::claim_instance(&single_instance_app_id(add_word.is_some()))
        == windui::InstanceRole::Handoff
    {
        return;
    }

    logger::init();
    // 协议自愈有两道门：便携版不碰宿主机注册表，用户手动取消过注册的也不再写回去。
    // 单读一次配置而不等 AppState 建好 —— 自愈要赶在窗口出现前完成，此刻 AppState
    // 还没开始拉 core 数据。判定本身是纯函数，见 protocol::should_self_heal_on_startup。
    if protocol::should_self_heal_on_startup(
        mode::is_portable(),
        setting_config::load().config.protocol.self_heal,
    ) {
        protocol::self_heal_registration();
    }

    // 快捷加词小窗：独立单例、紧凑窗，复用词库加词对话框；不落入常规设置界面路径。
    if let Some(params) = add_word {
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
        // `--page dict` 走开窗而非切页，且冷启动不立刻推帧（等 apply_loaded），见 route_target。
        if let Some(idx) = cli::parse_target(&args) {
            route_target(&st, idx, false);
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

    // 渲染：默认 `Renderer::Auto`（GPU 优先，远程会话 / 无可用 GPU 时由 windui 自动回退
    // 软渲染，不 panic）。命令行 --soft / --no-accel 强制软渲染。
    //
    // 显式写 Auto 而非省略：windui 的默认值是 `Software`（其 GPU 路径的验证仍在补齐），
    // 而本应用一直跑在 GPU 加速上——省略参数会静默降级成软渲染。
    let soft_render = args.iter().any(|a| a == "--soft" || a == "--no-accel");
    let renderer = if soft_render {
        Renderer::Software
    } else {
        Renderer::Auto
    };
    tracing::info!(
        "渲染模式: {}",
        if soft_render {
            "软件 (tiny-skia)"
        } else {
            "GPU 优先（不可用时自动回退软渲染）"
        }
    );

    // 单实例 + 二次运行激活/切页/协议。
    let app_id = single_instance_app_id(false);
    let single_instance_state = app_state.clone();
    let mut app = App::new("清风输入法设置", 820, 640)
        .single_instance(app_id, move |argv| {
            // 深链失效（点了"词库管理"却停在原页）时，这条是唯一能分辨「argv 没送到」
            // 与「送到了但解析不出页」的证据。两种成因的修法完全不同，值一条 info。
            tracing::info!("二次实例 argv: {:?}", argv.get(1..).unwrap_or(&[]));
            if let Some(raw) = cli::parse_protocol(&argv) {
                route_protocol(&raw, &single_instance_state);
            } else if let Some(idx) = cli::parse_target(&argv) {
                let st = single_instance_state.borrow();
                // 已运行的实例通常方案列表早已就绪，深链会立即生效；若恰好撞上启动
                // 头几十毫秒数据还没回来，route_dict_target 会自动改为挂起。
                if let Some(t) = cli::parse_dict_target(&argv) {
                    st.route_dict_target(t);
                }
                // notify = true：本回调没有 ctx、开不了窗（见 `UiRequest`），要经通道推一帧。
                // 词库窗若已开着，`Window::single` 会把它激活到前台，深链参数照样生效。
                route_target(&st, idx, true);
            }
        })
        .frameless()
        .centered()
        .renderer(renderer)
        .min_size(560, 440)
        .theme(theme_def::theme_for(start_dark));
    let th = app.theme_handle();

    // 注册后台→UI 通道：数据就绪后在 UI 线程回调，更新 Signal 刷新界面。
    let app_state_cb = app_state.clone();
    let tx = app.channel::<state::LoadedState>(move |ctx, data| {
        app_state_cb.borrow_mut().apply_loaded(data);
        // 冷启动 `--page dict` 的落点：方案列表刚刚注入，此刻开窗左栏才有内容、
        // `--schema` 也才解析得出域下标。channel 回调的副作用走
        // `apply_dispatch_effects`，`open_window` 在这条路上是被采纳的。
        if app_state_cb.borrow().take_dict_window_request() {
            dict_window::open(ctx, &app_state_cb);
        }
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
    let events_tx = app.channel::<rpc::CoreEvent>(move |ctx, ev| {
        events_state.borrow_mut().on_core_event(ctx, &ev);
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
    let update_tx = app.channel::<update::UpdateMsg>(move |_ctx, msg| {
        update_state.borrow().on_update_msg(msg);
    });
    app_state.borrow().set_update_tx(update_tx);

    // UI 请求通道：`single_instance` 的 argv 回调没有 ctx，开不了窗（见 `UiRequest`），
    // 由它把请求转到这里 —— channel 的 on_message 收 `&mut EventCtx`，副作用被采纳。
    let ui_state = app_state.clone();
    let ui_tx = app.channel::<UiRequest>(move |ctx, req| match req {
        UiRequest::OpenDictWindow => {
            if ui_state.borrow().take_dict_window_request() {
                dict_window::open(ctx, &ui_state);
            }
        }
    });
    app_state.borrow().set_ui_tx(ui_tx);

    // 启动静默检查更新：三重门控（已同意联网 + 未关自动检查 + 距上次超 24h）。
    // 无更新或失败都不打扰用户，只有确实有新版本且未被「跳过此版本」屏蔽才弹对话框。
    {
        let now = chrono::Local::now().timestamp();
        let cfg = app_state.borrow().setting_config.update.clone();
        let forced = update::force_check_requested(&args);
        // 强制检查仍受 network_consent 门控 —— 调试后门不该越过联网同意
        let gate = if !update::self_update_supported() {
            // 非 Windows 无升级通路：查了也只能下到一个装不上的 .exe，纯骚扰。
            // 门控放在这里而不是 should_auto_check 里边 —— 那是个纯函数，三重门控的
            // 语义与平台无关，掺进平台判断会让它的单测在非 Windows 上集体失效。
            false
        } else if forced {
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

    // 关窗守卫：有未保存改动时挡下这一次关闭，弹确认框问一句。
    //
    // `on_close_request` 的返回值被平台在 WM_CLOSE 里**同步等待**，容不下"等用户回答"
    // 这种异步决定，故这里只能"先返回 false 挡住 + 另起一条路把答案送回来"——那条路就是
    // 应用内的确认对话框，它的两个退出按钮走 `ctx.force_close()`（绕过本守卫）——
    // 判据本身（有没有改动）不会因为用户选了"直接退出"就消失，走普通关闭会被自己再拦一次。
    //
    // 自绘标题栏的 × 与 Alt+F4 都会到这里（windui 0.12 起两者同走关闭决策链）。
    let close_state = app_state.clone();
    app = app.on_close_request(move |_ctx| {
        let Ok(st) = close_state.try_borrow() else {
            // 借用不到说明正处在某个改状态的回调里，此时拦下反而更糟（用户会以为窗口卡死）。
            return true;
        };
        if !st.has_unsaved_changes() {
            // 放行的这一刻才宣告退出：词库子窗的哨兵据此自行关闭（windui 没有"关别的
            // 窗口"的 API）。置位必须在**放行分支**里 —— 下面那条是拦下、应用并没有要退出，
            // 在那里置位会让用户点了取消之后词库窗自己消失。
            st.mark_app_exiting();
            return true;
        }
        st.exit_confirm.set(true);
        false
    });

    app.bg(theme_def::theme_for(start_dark).palette.bg)
        // `--screenshot <path>` 离屏出图（配合 WIND_RPC_MOCK=1 可无 core 核对 UI）。
        .screenshot_from_args()
        .content(ui)
        .run();
}

/// 协议 URL 路由：解析 → 切到相关页；主题导入暂存到待确认状态并弹出导入对话框，
/// 其它 kind 提示暂不支持。反馈走 feedback（toast）——底栏只剩状态点，没有常驻文字位。
fn route_protocol(raw: &str, state: &Rc<RefCell<state::AppState>>) {
    match protocol::parse_protocol_url(raw) {
        Ok(req) => {
            let st = state.borrow();
            // 只有主题导入有页可切（外观页）。其余 kind 一律只弹一句"暂不支持"，**不切页**：
            // 此前它们被送到索引 4（词库），而词库现在是主窗里一格永不显示的占位，切过去
            // 就是一屏空白 —— 用户点了个导入链接，设置窗跳出来却什么都没有。
            if req.kind == "theme"
                && let Some(i) = cli::page_to_index("ui")
            {
                st.tab.set(i);
            }
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
        assert_eq!(st.tab.get(), crate::cli::page_to_index("ui").unwrap());
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
        // 不支持的 kind **不切页**：停在上一次主题请求切到的外观页，而不是被送去词库
        // 那一格永不显示的占位（切过去就是一屏空白）。
        assert_eq!(st.tab.get(), crate::cli::page_to_index("ui").unwrap());
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

    /// `--page dict` 必须**开窗**而不是切页：主窗那一格是永不显示的占位，切过去就是
    /// 一屏空白。这是本次把词库迁出主窗后最容易回退的一条。
    #[test]
    fn dict_target_requests_window_and_leaves_tab_alone() {
        let st = state::AppState::empty();
        let before = st.tab.get();

        route_target(
            &st,
            cli::page_to_index("dict").expect("dict 必在 PAGES 中"),
            false,
        );

        assert_eq!(st.tab.get(), before, "开词库窗不该动主窗的当前页");
        assert!(st.take_dict_window_request(), "应挂起一个开窗请求");
    }

    /// 开窗请求取一次即清：否则 `apply_loaded` 与 UI 通道两处都会消费到它，
    /// 一次深链开两回窗（非单例窗就是真开两个）。
    #[test]
    fn dict_window_request_is_consumed_once() {
        let st = state::AppState::empty();

        st.request_dict_window(false);

        assert!(st.take_dict_window_request());
        assert!(!st.take_dict_window_request(), "第二次取应为空");
    }

    /// 其余页照旧切页，且不该顺带挂起开窗请求。
    #[test]
    fn non_dict_target_switches_tab() {
        let st = state::AppState::empty();
        let about = cli::page_to_index("about").expect("about 必在 PAGES 中");

        route_target(&st, about, false);

        assert_eq!(st.tab.get(), about);
        assert!(!st.take_dict_window_request());
    }

    /// 退出置位默认关着 —— 否则词库窗一开出来就被哨兵关掉。
    #[test]
    fn app_closing_starts_false_and_latches() {
        let st = state::AppState::empty();
        assert!(!st.app_closing.get());

        st.mark_app_exiting();

        assert!(st.app_closing.get());
    }
}
