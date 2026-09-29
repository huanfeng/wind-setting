//! 「应用兼容」页：统一管理 `compat.toml`（出厂规则 + 用户改写），设计见
//! `WindInput/docs/design/compat-settings-ui.md` 第 11、12 节。
//!
//! 与其它设置页的根本区别：**不走 manifest / `FieldRegistry` 的「保存时统一 diff」**——
//! 每次改动立即 `compat.*` 落盘，界面上没有「应用」按钮。

pub mod actions;
pub mod dialogs;
pub mod mock;
pub mod model;
pub mod state;
mod view;

pub use state::CompatState;
pub use view::build_page;
