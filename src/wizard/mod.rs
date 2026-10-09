//! 设置向导：按输入习惯（五笔/全拼/双拼）4 步快速配置。设计见 docs/design/setup-wizard.md。
// 后续任务接线后移除（Task 5）。
#![allow(dead_code)]

pub mod apply;
pub mod model;

/// 启动后是否自动弹出向导。core 连不上时不弹：向导预填需要真实配置，拿空配置预填会误导。
pub fn should_auto_open(dismissed: bool, connected: bool) -> bool {
    !dismissed && connected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_open_only_when_not_dismissed_and_connected() {
        assert!(should_auto_open(false, true));
        assert!(!should_auto_open(true, true));
        assert!(!should_auto_open(false, false));
    }
}
