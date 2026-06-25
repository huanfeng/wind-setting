#![cfg_attr(windows, windows_subsystem = "windows")]

//! wind_setting_native — 基于 windui 的原生设置应用。
//!
//! 通过 JSON-RPC over named pipe 与核心服务通信，不改动 core 接口。
//! 无边框窗口 + 自定义标题栏，8 Tab 清单驱动，运行时明暗换肤。

mod config;
mod dialogs;
mod manifest;
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
    let start_dark = app_state.borrow().dark.get();

    let mut app = App::new("清风输入法设置", 820, 640)
        .frameless()
        .centered()
        .theme(theme_def::theme_for(start_dark));
    let th = app.theme_handle();

    let ui = shell::build(&app_state, th);

    app.bg(theme_def::theme_for(start_dark).palette.bg)
        // `--screenshot <path>` 离屏出图（配合 WIND_RPC_MOCK=1 可无 core 核对 UI）。
        .screenshot_from_args()
        .content(ui)
        .run();
}
