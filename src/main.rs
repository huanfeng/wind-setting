#![cfg_attr(windows, windows_subsystem = "windows")]

//! wind_setting_native — 基于 windui 的原生设置应用。
//!
//! 通过 JSON-RPC over named pipe 与核心服务通信，不改动 core 接口。
//! 无边框窗口 + 自定义标题栏，9 Tab 清单驱动（manifest 分组），运行时明暗换肤。

mod config;
mod dialogs;
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

fn main() {
    let app_state = Rc::new(RefCell::new(state::AppState::load()));

    // 参数解析（最先执行，后续逻辑依赖运行时模式）：
    //   --mode dev/debug   开发/调试模式（影响管道名称与版本显示）
    //   --light / --dark   覆盖初始主题（默认随系统）
    //   --tab N            指定初始 Tab（0 起）
    {
        let args: Vec<String> = std::env::args().collect();
        mode::init(&args);
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
        // 默认 GPU(Direct2D) 加速；离屏截图/无 GPU/远程会话会自动回退软渲染。
        // 可用 `--no-accel` 关闭（对比/排障）。
        .accelerated(!std::env::args().any(|a| a == "--no-accel"))
        // 最小窗口尺寸：防止缩到操作不到标题栏按钮 / Tab。
        .min_size(560, 440)
        .theme(theme_def::theme_for(start_dark));
    let th = app.theme_handle();

    let ui = shell::build(&app_state, th);

    app.bg(theme_def::theme_for(start_dark).palette.bg)
        // `--screenshot <path>` 离屏出图（配合 WIND_RPC_MOCK=1 可无 core 核对 UI）。
        .screenshot_from_args()
        .content(ui)
        .run();
}
