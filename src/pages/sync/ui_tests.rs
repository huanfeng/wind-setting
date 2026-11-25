use super::*;
use serde_json::json;

#[test]
fn mismatched_passphrase_sends_no_command() {
    let ui = SyncUi::new();
    ui.pass1.set("a".into());
    ui.pass2.set("b".into());
    ui.submit_passphrase();
    assert_eq!(ui.pass_err.get(), "两次输入的口令不一致");
    assert!(!ui.busy.get(), "被拦下就不该发命令");
}

#[test]
fn secret_inputs_are_cleared_after_storage_save_dispatch() {
    let ui = SyncUi::new();
    ui.wd_password.set("hunter2".into());
    ui.s3_secret.set("sk".into());
    ui.pass1.set("口令".into());
    // 单测无 tx，命令发不出去；提交动作本身必须已清掉输入。
    ui.save_storage();
    assert_eq!(ui.storage_err.get(), "");
    assert_eq!(ui.wd_password.get(), "");
    assert_eq!(ui.s3_secret.get(), "");
    assert_eq!(ui.pass1.get(), "");
}

#[test]
fn storage_move_without_unlock_keeps_dialog_inputs() {
    let ui = SyncUi::new();
    *ui.saved.borrow_mut() = Form {
        enabled: true,
        wd_url: "https://a/dav/".into(),
        wd_user: "u".into(),
        ..Form::default()
    };
    ui.reset_storage_form();
    ui.wd_url.set("https://b/dav/".into());
    ui.wd_password.set("pw".into());
    ui.save_storage();
    assert!(ui.storage_err.get().contains("口令"));
    assert_eq!(ui.wd_password.get(), "pw", "没发出去就不清，免得重填");
}

#[test]
fn status_drives_configured_identity_and_duplicate() {
    let ui = SyncUi::new();
    ui.apply_status(&json!({
        "installed": true, "enabled": true, "joined": true, "needs_identity": true,
        "phase": "error", "last_error": "另一台设备在使用相同设备号（d1），请给本机改号"
    }));
    assert!(ui.configured.get() && ui.needs_identity.get() && ui.dup_device.get());
    ui.apply_status(&json!({ "installed": true, "enabled": false, "joined": true }));
    assert!(!ui.configured.get(), "停用后回到未配置");
    assert!(!ui.dup_device.get());
}

#[test]
fn hooks_open_their_dialog_signals() {
    let ui = SyncUi::new();
    ui.open_rename_dialog();
    ui.open_group_dialog();
    ui.open_identity_dialog();
    ui.open_wizard();
    assert!(ui.rename_dialog.get() && ui.group_dialog.get());
    assert!(ui.identity_dialog.get() && ui.wizard.get());
}

#[test]
fn closing_export_forgets_the_key() {
    let ui = SyncUi::new();
    ui.open_export();
    assert!(ui.export_dialog.get());
    ui.exported_key.set("k".into());
    ui.close_export();
    assert_eq!(ui.exported_key.get(), "");
}

fn status_with(phase: &str, err: &str) -> Value {
    json!({ "installed": true, "phase": phase, "last_error": err })
}

const MASS_ERR: &str = "本机有 25/25 个文件看似被删除，为防误删已暂停。确认无误请点「继续同步」";

#[test]
fn mass_delete_status_shows_confirm_not_error() {
    let ui = SyncUi::new();
    ui.apply_status(&status_with("halted", MASS_ERR));
    assert_eq!(ui.mass_text.get(), MASS_ERR);
    assert_eq!(ui.error.get(), "", "不用红色错误样式");
    ui.apply_status(&status_with("idle", ""));
    assert_eq!(ui.mass_text.get(), "", "恢复后确认入口消失");
}

#[test]
fn editing_error_is_a_notice_with_retry_hint() {
    let ui = SyncUi::new();
    ui.apply_status(&status_with("error", "a.toml 正在被编辑，本轮暂不同步"));
    assert_eq!(ui.error.get(), "", "不走红色错误样式");
    assert!(ui.notice.get().contains("会自动重试"));
    assert_eq!(ui.phase.get(), "稍后自动重试");
    ui.apply_status(&status_with("error", "401"));
    assert_eq!(ui.error.get(), "401");
    assert_eq!(ui.notice.get(), "");
}

#[test]
fn mass_delete_needs_second_confirmation() {
    let ui = SyncUi::new();
    assert!(!ui.take_mass_confirm(), "没点过入口不放行");
    ui.request_mass_delete();
    assert!(ui.mass_dialog.get(), "第一次点击只弹确认框");
    assert!(ui.take_mass_confirm(), "框里确认才放行");
    assert!(!ui.mass_dialog.get());
    assert!(!ui.take_mass_confirm(), "确认只用一次");
    ui.request_mass_delete();
    ui.mass_guard.disarm();
    assert!(!ui.take_mass_confirm(), "取消后收回");
}

fn dev(id: &str, self_: bool, readable: bool, ts: i64) -> DeviceView {
    DeviceView {
        id: id.into(),
        name: if readable {
            format!("n-{id}")
        } else {
            String::new()
        },
        platform: "macos".into(),
        app_version: "0.132.0".into(),
        last_seen: ts,
        is_self: self_,
        readable,
        group: "g1".into(),
        group_name: String::new(),
    }
}

#[test]
fn remove_needs_second_confirmation() {
    let ui = SyncUi::new();
    assert_eq!(ui.take_remove_confirm(), None, "没点过入口不放行");
    ui.request_remove(&dev("b", false, true, 1));
    assert!(ui.remove_dialog.get(), "第一次点击只弹确认框");
    assert_eq!(ui.remove_name.get(), "n-b");
    assert_eq!(ui.take_remove_confirm().as_deref(), Some("b"));
    assert!(!ui.remove_dialog.get());
    assert_eq!(ui.take_remove_confirm(), None, "确认只用一次");
    ui.request_remove(&dev("b", false, true, 1));
    ui.remove_guard.disarm();
    assert_eq!(ui.take_remove_confirm(), None, "取消后收回");
    assert!(REMOVE_DEVICE_WARN.contains("重置远端并更换口令"));
}

#[test]
fn device_name_over_64_chars_is_rejected() {
    assert_eq!(check_device_name("  我的电脑 ").unwrap(), "我的电脑");
    assert_eq!(check_device_name("").unwrap(), "");
    assert!(check_device_name(&"字".repeat(64)).is_ok());
    assert!(check_device_name(&"字".repeat(65)).is_err());
}
