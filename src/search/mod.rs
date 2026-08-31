//! 设置项全局搜索 —— 索引 / 匹配 / 跳转目标。
//!
//! 分四层（见 `docs/settings-search-plan.md`）：
//! - **索引** [`index`]：manifest 派生 + 手写页补充 + 方案级，启动构建一次；
//! - **匹配** [`matcher`]：汉字子串 / 拼音首字母 / 配置 key 子串，打分排序；
//! - **导航** [`jump`]：跳转请求写进信号，切页当场做，滚动+闪烁由锚点认领（P2）；
//! - **反馈**：闪烁与指示条（P2/P4）。
//!
//! [`ui`] 是搜索框与结果浮层；其余三层都是纯逻辑，可完整单测。

pub mod index;
pub mod jump;
pub mod matcher;
pub mod pinyin;
pub mod ui;

pub use index::JumpTarget;
pub use jump::JumpRequest;
// 索引门面：`ui` 内部按具体路径引用，此处留作模块出口。
#[allow(unused_imports)]
pub use index::{SearchEntry, build_index};
pub use ui::SearchState;
// 匹配层门面：目前只有 `ui` 直接用（它 use 的是具体路径），留作模块出口。
#[allow(unused_imports)]
pub use matcher::{SearchHit, search};
