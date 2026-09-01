//! 设置项全局搜索 —— 索引 / 匹配 / 跳转目标。
//!
//! 分四层（见 `docs/settings-search-plan.md`）：
//! - **索引** [`index`]：manifest 派生 + 手写页补充 + 方案级，启动构建一次；
//! - **匹配** [`matcher`]：汉字子串 / 拼音首字母 / 配置 key 子串，打分排序；
//! - **导航** [`jump`]：跳转请求写进信号，切页当场做，滚动+闪烁由锚点认领（P2）；
//! - **反馈**：闪烁与指示条（P2/P4）。
//!
//! [`ui`] 是搜索框与结果浮层；其余三层都是纯逻辑，可完整单测。

pub mod anchor;
pub mod hint;
pub mod index;
pub mod jump;
pub mod matcher;
pub mod pinyin;
pub mod ui;

// 只重导出**真有人跨模块用**的三个。`SearchEntry` / `build_index` / `SearchHit` /
// `search` 一度也摆在这儿"留作模块出口"，但本仓是 binary crate、没有外部消费者，
// 组内又都按具体路径引用，于是它们只能靠 `#[allow(unused_imports)]` 压住警告
// ——那个 allow 掩盖的正是编译器在说"这行没用"。要用时按具体路径写即可。
pub use index::JumpTarget;
pub use jump::JumpRequest;
pub use ui::SearchState;
