//! 文件选择框弹不出来时的统一提示。
//!
//! `PickDialog` 的 `pick_*` / `save_file` 在用户取消与对话框根本没弹出来时都返回 `None`；
//! 后者（目前只在 Linux：门户、zenity、kdialog 都不可用）要告诉用户该装什么，取消则保持静默。

use std::path::Path;

use windui::prelude::PickDialog;

/// 文件选择框返回 `None` 后调用：没弹出来时把提示交给 `notify`，用户取消则什么也不做。
///
/// 须在拿到 `None` 的同一线程、下一次弹框前调用（windui 按线程记最近一次结果）。
pub fn report_failure(notify: impl FnOnce(&'static str)) {
    if PickDialog::last_pick_failed() {
        notify(failure_msg(on_path("zenity"), on_path("kdialog")));
    }
}

/// 按本机已有的选择框程序给提示。门户回非 0 response（比如门户在但没有文件选择后端）时
/// rfd 不会退到 zenity，所以「装了 zenity 仍失败」要指向 kdialog 或门户后端，而不是再叫人装
/// zenity；kdialog 在而仍失败，只剩门户后端可查。
fn failure_msg(has_zenity: bool, has_kdialog: bool) -> &'static str {
    match (has_zenity, has_kdialog) {
        (false, false) => "无法打开文件选择框：请安装 zenity 或 kdialog",
        (true, false) => {
            "无法打开文件选择框：请安装 kdialog，或检查 xdg-desktop-portal 的文件选择后端"
        }
        (_, true) => "无法打开文件选择框：请检查 xdg-desktop-portal 的文件选择后端",
    }
}

/// `PATH` 上有没有名为 `name` 的文件。
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|dir| Path::new(&dir).join(name).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_follows_installed_pickers() {
        assert!(failure_msg(false, false).contains("请安装 zenity 或 kdialog"));
        let m = failure_msg(true, false);
        assert!(
            m.contains("安装 kdialog") && m.contains("文件选择后端"),
            "{m}"
        );
        assert!(!m.contains("zenity"), "已装 zenity 不该再叫人装：{m}");
        for z in [false, true] {
            let m = failure_msg(z, true);
            assert!(!m.contains("安装"), "kdialog 已在：{m}");
            assert!(m.contains("文件选择后端"), "{m}");
        }
    }

    /// 没弹过框时 windui 报「未失败」，不会凭空提示。
    #[test]
    fn no_report_without_a_failed_pick() {
        let mut called = false;
        report_failure(|_| called = true);
        assert!(!called);
    }
}
