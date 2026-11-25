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

#[test]
fn status_drives_status_value_and_sync_now_gate() {
    let ui = SyncUi::new();
    let base = json!({ "installed": true, "enabled": true, "joined": true, "phase": "idle" });
    ui.apply_status(&base);
    assert_eq!(ui.phase.get(), "空闲，已是最新");
    assert!(ui.can_sync.get());
    let mut v = base.clone();
    v["needs_identity"] = json!(true);
    ui.apply_status(&v);
    assert_eq!(ui.phase.get(), "需要确认本机身份");
    assert!(!ui.can_sync.get(), "确认身份前 core 拒绝立即同步");
    let mut v = base.clone();
    v["last_error"] = json!("另一台设备在使用相同设备号（d1），请给本机改号");
    ui.apply_status(&v);
    assert_eq!(ui.phase.get(), "需要给本机改号");
    assert!(!ui.can_sync.get());
}

#[test]
fn late_exported_key_is_dropped_after_close() {
    let ui = SyncUi::new();
    windui::testing::run_with_ctx(|ctx| {
        let key = || SyncMsg::Command {
            op: "export_key".into(),
            result: Ok(json!({ "key": "k" })),
        };
        ui.on_msg(ctx, key());
        assert_eq!(ui.exported_key.get(), "", "框已关，迟到的密钥不留");
        ui.export_dialog.set(true);
        ui.on_msg(ctx, key());
        assert_eq!(ui.exported_key.get(), "k");
    });
}

#[test]
fn interval_change_while_busy_is_deferred_not_sent() {
    let ui = SyncUi::new();
    ui.busy.set(true);
    ui.interval.set(30.0);
    ui.save_interval();
    assert!(ui.interval_pending.get(), "等在途命令结束再发");
    assert_eq!(ui.saved.borrow().interval_min, 15, "没发就不改已保存值");
}

#[test]
fn rename_dialog_opens_with_current_values_and_rejects_bad_id() {
    let ui = SyncUi::new();
    ui.device.set("d1".into());
    ui.saved.borrow_mut().device_name = "书房".into();
    ui.open_rename_dialog();
    assert_eq!(ui.rename_id.get(), "d1");
    assert_eq!(ui.rename_name.get(), "书房");
    ui.rename_id.set("Bad_".into());
    ui.save_rename();
    assert!(!ui.rename_err.get().is_empty());
    assert!(ui.rename_dialog.get(), "校验失败框不关");
}

#[test]
fn rename_with_nothing_changed_just_closes() {
    let ui = SyncUi::new();
    ui.device.set("d1".into());
    ui.open_rename_dialog();
    ui.save_rename();
    assert!(!ui.rename_dialog.get());
    assert_eq!(ui.rename_err.get(), "");
}

#[test]
fn group_dialog_defaults_to_new_group_when_nothing_to_join() {
    let ui = SyncUi::new();
    ui.group.set("g1".into());
    ui.group_name.set("家里".into());
    ui.open_group_dialog();
    assert_eq!(ui.group_mode.get(), 1);
    assert_eq!(ui.group_rename.get(), "家里", "重命名预填当前组名");
    ui.group_mode.set(2);
    ui.group_rename.set("  ".into());
    ui.save_group();
    assert_eq!(ui.group_err.get(), "组名不能为空");
}

#[test]
fn group_dialog_lists_other_same_class_groups() {
    let ui = SyncUi::new();
    let class = platform_class(std::env::consts::OS);
    let p = if class == PlatformClass::Mobile {
        "android"
    } else {
        "windows"
    };
    let mk = |id: &str, g: &str, n: &str| DeviceView {
        id: id.into(),
        name: id.into(),
        platform: p.into(),
        app_version: String::new(),
        last_seen: 1,
        is_self: false,
        readable: true,
        group: g.into(),
        group_name: n.into(),
    };
    ui.devices.set(vec![
        mk("a", "g1", "家里"),
        mk("b", "g2", "公司"),
        mk("c", "g2", "公司"),
    ]);
    ui.group.set("g1".into());
    ui.open_group_dialog();
    assert_eq!(ui.group_mode.get(), 0);
    assert_eq!(
        ui.group_choices.get(),
        vec![("g2".to_string(), "公司 · 2 台".to_string())]
    );
}

#[test]
fn group_mine_choice_maps_to_join_mode() {
    let ui = SyncUi::new();
    ui.group_choices
        .set(vec![("g2".to_string(), "公司 · 2 台".to_string())]);
    ui.group_mode.set(0);
    let mode_of = |ui: &SyncUi| {
        let c = group_change_commands(&ui.group_change().unwrap());
        c[0].1["mode"].as_str().unwrap().to_string()
    };
    assert_eq!(mode_of(&ui), "theirs", "默认用组内设置");
    ui.group_mine.set(1);
    assert_eq!(mode_of(&ui), "mine");
}

#[test]
fn rename_without_known_device_id_is_refused() {
    let ui = SyncUi::new();
    ui.open_rename_dialog();
    ui.rename_id.set("d2".into());
    ui.save_rename();
    assert!(ui.rename_err.get().contains("设备号"));
    assert!(ui.rename_dialog.get());
}
