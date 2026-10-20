//! 文件选择框弹不出来时的统一提示。
//!
//! `PickDialog` 的 `pick_*` / `save_file` 在用户取消与对话框根本没弹出来时都返回 `None`；
//! 后者（目前只在 Linux：门户、zenity、kdialog 都不可用）要告诉用户装什么，取消则保持静默。

use windui::prelude::PickDialog;

/// 对话框没弹出来时给用户的提示。
pub const PICK_FAILED_MSG: &str = "无法打开文件选择框：请安装 zenity 或 kdialog";

/// 文件选择框返回 `None` 后调用：没弹出来时把提示交给 `notify`，用户取消则什么也不做。
///
/// 须在拿到 `None` 的同一线程、下一次弹框前调用（windui 按线程记最近一次结果）。
pub fn report_failure(notify: impl FnOnce(&'static str)) {
    report_if(PickDialog::last_pick_failed(), notify);
}

fn report_if(failed: bool, notify: impl FnOnce(&'static str)) {
    if failed {
        notify(PICK_FAILED_MSG);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_is_reported_cancel_stays_silent() {
        let mut got = None;
        report_if(true, |m| got = Some(m));
        assert_eq!(got, Some(PICK_FAILED_MSG));

        let mut called = false;
        report_if(false, |_| called = true);
        assert!(!called, "用户取消不提示");
    }

    /// 没弹过框时 windui 报「未失败」，不会凭空提示。
    #[test]
    fn no_report_without_a_failed_pick() {
        let mut called = false;
        report_failure(|_| called = true);
        assert!(!called);
    }
}
