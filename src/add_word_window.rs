//! 快捷加词小窗 — 独立单例、紧凑窗，复用词库加词对话框，关窗即退进程。
//!
//! 与主设置窗用不同 app_id（`wind_setting_addword{suffix}`），互不劫持；自身单例，
//! 连按只复用同一小窗重新预填。只做最小 RPC（schema.list 一次 + 出码/提交按需），
//! 不走整页的 LoadedState::fetch。

use std::rc::Rc;

use windui::prelude::*;

use crate::cli::AddWordParams;
use crate::pages::dict::dialogs::build_dict_dialogs;
use crate::pages::dict::reactive::{close_on_hidden, toast_sink};
use crate::pages::dict::state::DictManagerState;
use crate::state::load_schemas;
use crate::theme_def::theme_for;

/// 用参数预填一个 DictManagerState（加载方案 + 定位 + 预填 + 打开对话框）。
fn prepare(params: &AddWordParams) -> DictManagerState {
    let mgr = DictManagerState::new();
    mgr.set_schemas_no_reload(&load_schemas());
    mgr.open_add_word_prefilled(&params.text, &params.code, &params.schema);
    mgr
}

/// 运行加词小窗；此函数不返回（App::run 进入消息循环，关窗即退进程）。
pub fn run(params: AddWordParams) {
    let dark = std::env::args().any(|a| a == "--dark");
    let mgr = Rc::new(prepare(&params));

    // 内容 = 复用的词库对话框集合（含加/改词对话框，edit_visible 已置 true）
    //        + toast 反馈汇 + 关窗哨兵（edit_visible→false 即 request_close）。
    let root = Element::stack()
        .fill()
        .bg_role(Role::Bg)
        .child(build_dict_dialogs(&mgr))
        .child(toast_sink(mgr.feedback))
        .child(close_on_hidden(mgr.edit_visible));

    let app_id = format!("wind_setting_addword{}", crate::mode::pipe_suffix());
    // 二次启动：重新解析参数并预填，复用同一小窗（不新起进程）。
    let si_mgr = mgr.clone();
    App::new("添加用户词库", 460, 300)
        .single_instance(app_id, move |argv| {
            if let Some(p) = crate::cli::parse_add_word(&argv) {
                si_mgr.set_schemas_no_reload(&load_schemas());
                si_mgr.open_add_word_prefilled(&p.text, &p.code, &p.schema);
            }
        })
        .frameless()
        .centered()
        .theme(theme_for(dark))
        .bg(theme_for(dark).palette.bg)
        .screenshot_from_args()
        .content(root)
        .run();
}
