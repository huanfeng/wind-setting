//! 设置项全局搜索 —— 索引 / 匹配 / 跳转目标。
//!
//! 分四层（见 `docs/settings-search-plan.md`）：
//! - **索引** [`index`]：manifest 派生 + 手写页补充 + 方案级，启动构建一次；
//! - **匹配** [`matcher`]：汉字子串 / 拼音首字母 / 配置 key 子串，打分排序；
//! - **导航**：跳转请求写进信号，由页面上的锚点自认领（P2）；
//! - **反馈**：闪烁与指示条（P2/P4）。
//!
//! 本模块（P0）只含前两层——纯逻辑、不碰 UI，故可完整单测。

pub mod index;
pub mod matcher;
pub mod pinyin;

// 模块门面：P1 接线搜索框时由 `shell` / `search::ui` 消费，P0 阶段尚无使用者。
#[allow(unused_imports)]
pub use index::{JumpTarget, SearchEntry, build_index};
#[allow(unused_imports)]
pub use matcher::{SearchHit, search};
