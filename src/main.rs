#![cfg_attr(windows, windows_subsystem = "windows")]

//! wind_setting_native — 基于 windui 的原生设置应用。
//!
//! 通过 JSON-RPC over named pipe 与核心服务通信，不改动 core 接口。
//! 无边框窗口 + 自定义标题栏，9 Tab 清单驱动（manifest 分组），运行时明暗换肤。

mod capabilities;
mod config;
mod dialogs;
mod logger;
mod manifest;
mod mode;
mod pages;
mod rpc;
mod shell;
mod state;
mod theme_def;
mod widgets;

use std::cell::RefCell;
use std::rc::Rc;

use windui::prelude::*;

/// 启动一条后台线程拉取数据并在 UI 线程 apply。重连复用此入口。
///
/// 重连直接同步 fetch（设置操作低频，连接不存在时 fast-fail 几乎立即返回）；
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

    // 快速初始化：仅读系统主题（~86µs），不阻塞任何 RPC，UI 立即可显示。
    let app_state = Rc::new(RefCell::new(state::AppState::empty()));
    {
        let st = app_state.borrow();
        if args.iter().any(|a| a == "--light") {
            st.dark.set(false);
        } else if args.iter().any(|a| a == "--dark") {
            st.dark.set(true);
        }
        if let Some(n) = args
            .iter()
            .skip_while(|a| *a != "--tab")
            .nth(1)
            .and_then(|s| s.parse::<usize>().ok())
        {
            st.tab.set(n);
        }
    }
    let start_dark = app_state.borrow().dark.get();

    let mut app = App::new("清风输入法设置", 820, 640)
        .frameless()
        .centered()
        .accelerated(!std::env::args().any(|a| a == "--no-accel"))
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
