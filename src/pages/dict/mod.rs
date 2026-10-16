//! 词库管理页 — 统一词条表格框架 + 五类数据（短语/用户词库/临时词库/词频/候选调整）。
//!
//! 分层：
//! - [`spec`]：类别描述（列/操作/RPC/作用域）+ 统一行模型 + json 映射。
//! - （后续）`state`：`DictManagerState` 数据加载/刷新/写操作。
//! - （后续）`view`：两级导航 + 工具栏 + 表格 + 分页。
//! - （后续）`dialogs`：加/改词、确认对话框。

pub mod category_nav;
pub mod charset_new;
pub mod dialogs;
pub mod file_edit;
pub mod reactive;
pub mod sidebar;
pub mod spec;
pub mod state;
pub mod user_libs;
pub mod view;
pub mod wordlist;

pub use dialogs::build_dict_dialogs;
