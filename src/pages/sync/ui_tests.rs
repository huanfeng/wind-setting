use super::*;
use serde_json::json;

#[test]
fn secret_inputs_are_cleared_after_storage_save_dispatch() {
    let ui = SyncUi::new();
    ui.wd_password.set("hunter2".into());
    ui.s3_secret.set("sk".into());
    // 单测无 tx，命令发不出去；提交动作本身必须已清掉输入。
    ui.save_storage();
    assert_eq!(ui.storage_err.get(), "");
    assert_eq!(ui.wd_password.get(), "");
    assert_eq!(ui.s3_secret.get(), "");
}

#[test]
fn secret_hint_follows_saved_backend_and_has_secret() {
    let ui = SyncUi::new();
    assert_eq!(ui.secret_hint.get(), "未设置");
    ui.apply_form(&Form {
        wd_url: "https://a/dav/".into(),
        wd_user: "u".into(),
        ..Form::default()
    });
    assert_eq!(ui.secret_hint.get(), "如已保存过密码可留空");
    ui.apply_status(&json!({ "installed": true, "has_secret": true }));
    assert!(ui.secret_hint.get().starts_with("已保存"));
}

#[test]
fn storage_move_without_password_keeps_dialog_inputs() {
    let ui = SyncUi::new();
    *ui.saved.borrow_mut() = Form {
        enabled: true,
        wd_url: "https://a/dav/".into(),
        wd_user: "u".into(),
        ..Form::default()
    };
    ui.reset_storage_form();
    ui.wd_url.set("https://b/dav/".into());
    ui.wd_user.set("u2".into());
    ui.save_storage();
    assert_eq!(ui.storage_err.get(), "更换了存储位置，请填写新位置的密码");
    assert_eq!(ui.wd_user.get(), "u2", "没发出去就不清，免得重填");
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
    assert!(ui.configured.get(), "总开关关掉不等于未配置");
    assert!(!ui.enabled.get(), "总开关跟着状态里的 enabled");
    assert!(!ui.dup_device.get());
    ui.apply_status(&json!({ "installed": true, "enabled": false, "joined": false }));
    assert!(!ui.configured.get());
}

#[test]
fn layout_texts_follow_the_spec() {
    use super::page::*;
    assert_eq!(
        NAV,
        ["向导", "状态", "本机", "设备", "内容", "存储", "安全"]
    );
    assert_eq!((BTN_MODIFY, ROW_GROUP), ("修改…", "同步组"));
    assert_eq!(
        wizard_text(false),
        "用 WebDAV 或 S3 在多台设备间同步，数据先在本机加密"
    );
    assert_eq!(wizard_text(true), "已配置；要换一个同步库可重新配置");
    assert_eq!(
        (wizard_button(false), wizard_button(true)),
        ("开始配置…", "重新配置…")
    );
}

#[test]
fn unconfigured_page_shows_only_the_wizard_card() {
    use super::page::{NAV, card_shown};
    let shown = |cfg| (0..NAV.len()).filter(|&i| card_shown(i, cfg)).count();
    assert_eq!(shown(false), 1);
    assert!(card_shown(0, false));
    assert_eq!(shown(true), NAV.len());
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
    assert!(REMOVE_DEVICE_WARN.contains("重置远端"));
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

fn joined_ui(enabled: bool) -> SyncUi {
    let ui = SyncUi::new();
    ui.apply_status(&json!({ "installed": true, "enabled": enabled, "joined": true }));
    *ui.saved.borrow_mut() = Form {
        enabled,
        ..Form::default()
    };
    ui
}

#[test]
fn master_switch_reverts_when_nothing_is_sent() {
    let ui = joined_ui(true);
    ui.enabled.set(false);
    ui.set_master(false);
    assert!(ui.enabled.get(), "没接通道：退回已保存的开");
    // windui 的 Sender 在测试里造不出来，busy 与无通道共用同一条退回分支。
    ui.busy.set(true);
    ui.enabled.set(false);
    ui.set_master(false);
    assert!(ui.enabled.get(), "有写入在途：退回已保存的开");
}

#[test]
fn master_switch_reverts_on_failed_command_and_saves_on_success() {
    let ui = joined_ui(true);
    windui::testing::run_with_ctx(|ctx| {
        ui.enabled.set(false);
        ui.on_msg(
            ctx,
            SyncMsg::Command {
                op: "disable".into(),
                result: Err("boom".into()),
            },
        );
        assert!(
            ui.enabled.get() && ui.saved.borrow().enabled,
            "失败：开关退回"
        );
        ui.on_msg(
            ctx,
            SyncMsg::Command {
                op: "disable".into(),
                result: Ok(json!({})),
            },
        );
        assert!(!ui.saved.borrow().enabled, "回执写入 enabled=false");
        assert!(!ui.enabled.get());
        ui.on_msg(
            ctx,
            SyncMsg::Command {
                op: "enable".into(),
                result: Ok(json!({})),
            },
        );
        assert!(ui.saved.borrow().enabled && ui.enabled.get());
    });
}

#[test]
fn status_does_not_overwrite_switch_while_busy() {
    let ui = joined_ui(true);
    ui.busy.set(true);
    ui.apply_status(&json!({ "installed": true, "enabled": false, "joined": true }));
    assert!(ui.enabled.get(), "写入在途时状态可能是旧的，不动开关");
    ui.busy.set(false);
    ui.apply_status(&json!({ "installed": true, "enabled": false, "joined": true }));
    assert!(!ui.enabled.get());
}

#[test]
fn unconfigured_switch_shows_off() {
    let ui = SyncUi::new();
    ui.apply_status(&json!({ "installed": true, "enabled": true, "joined": false }));
    assert!(
        !ui.configured.get() && !ui.enabled.get(),
        "未配置：开关显示为关"
    );
}

// ───────────────────────────── 同步密钥 ─────────────────────────────

const KEY: &str = "abcd-efgh-ijkl-mnop-qrst-uvwx-yz23-4567-abcd-efgh-ijkl-mnop-qrst";

fn test_ok(init: bool, joined: bool) -> Result<Vec<Value>, (usize, String)> {
    Ok(vec![
        json!({ "ok": true }),
        json!({ "ok": true }),
        json!({ "ok": true, "remote_initialized": init, "joined": joined }),
    ])
}

fn open_storage(ui: &SyncUi) {
    ui.open_storage_dialog();
    *ui.storage_sent.borrow_mut() = Some(Form {
        wd_url: "https://b/dav/".into(),
        wd_user: "u".into(),
        ..Form::default()
    });
}

#[test]
fn storage_on_empty_location_offers_create_then_shows_key() {
    let ui = SyncUi::new();
    open_storage(&ui);
    ui.on_wizard(WizOp::StorageSave, test_ok(false, false));
    assert!(ui.storage_dialog.get());
    assert_eq!(ui.storage_stage.get(), StorageStage::Create);
    assert_eq!(
        ui.saved.borrow().wd_url,
        "https://b/dav/",
        "设置已换到新位置"
    );
    ui.on_wizard(
        WizOp::StorageCreate,
        Ok(vec![json!({ "ok": true, "key": KEY })]),
    );
    assert_eq!(ui.storage_stage.get(), StorageStage::Created);
    assert_eq!(ui.new_key.get(), KEY);
    ui.close_storage_dialog();
    assert!(ui.storage_dialog.get(), "没勾「我已保存」不能关");
    ui.key_ack.set(true);
    ui.close_storage_dialog();
    assert!(!ui.storage_dialog.get());
    assert_eq!(ui.new_key.get(), "", "关框即清掉密钥");
}

#[test]
fn storage_on_existing_library_asks_key_then_identity() {
    let ui = SyncUi::new();
    open_storage(&ui);
    ui.on_wizard(WizOp::StorageSave, test_ok(true, false));
    assert_eq!(ui.storage_stage.get(), StorageStage::Join);
    ui.join_storage();
    assert_eq!(ui.storage_err.get(), "请粘贴同步密钥", "没贴不发");
    assert!(!ui.busy.get());
    ui.import_key.set("bad".into());
    ui.on_wizard(WizOp::StorageJoin, Err((0, "同步密钥不对".into())));
    assert_eq!(ui.storage_err.get(), "同步密钥不对");
    assert_eq!(ui.import_key.get(), "bad", "失败不清，免得重贴");
    assert!(ui.storage_dialog.get());
    ui.import_key.set(KEY.into());
    ui.on_wizard(
        WizOp::StorageJoin,
        Ok(vec![
            json!({ "ok": true }),
            json!({ "installed": true, "joined": true, "needs_identity": true }),
        ]),
    );
    assert!(!ui.storage_dialog.get());
    assert_eq!(ui.import_key.get(), "");
    assert!(ui.identity_dialog.get(), "加入后要确认身份");
}

#[test]
fn storage_already_joined_location_just_closes() {
    let ui = SyncUi::new();
    open_storage(&ui);
    ui.on_wizard(WizOp::StorageSave, test_ok(true, true));
    assert!(!ui.storage_dialog.get());
    assert!(!ui.identity_dialog.get());
}

#[test]
fn storage_test_failure_stays_on_form_with_error() {
    let ui = SyncUi::new();
    open_storage(&ui);
    ui.on_wizard(WizOp::StorageSave, Err((2, "超时".into())));
    assert!(ui.storage_dialog.get());
    assert_eq!(ui.storage_stage.get(), StorageStage::Form);
    assert_eq!(ui.storage_err.get(), "设置已保存，但连接新位置失败：超时");
}

#[test]
fn storage_dialog_reopens_on_form_without_key() {
    let ui = SyncUi::new();
    ui.storage_stage.set(StorageStage::Created);
    ui.new_key.set(KEY.into());
    ui.open_storage_dialog();
    assert_eq!(ui.storage_stage.get(), StorageStage::Form);
    assert_eq!(ui.new_key.get(), "");
}

#[test]
fn key_view_is_cleared_on_close() {
    let ui = SyncUi::new();
    ui.export_dialog.set(true);
    ui.exported_key.set(KEY.into());
    ui.close_export();
    assert_eq!(ui.exported_key.get(), "");
}

#[test]
fn reset_shows_new_key_and_needs_ack_to_close() {
    let ui = SyncUi::new();
    ui.open_reset_dialog();
    windui::testing::run_with_ctx(|ctx| {
        ui.on_msg(
            ctx,
            SyncMsg::Command {
                op: "reset_remote".into(),
                result: Ok(json!({ "ok": true, "key": KEY })),
            },
        );
    });
    assert!(ui.reset_dialog.get(), "重置后留在框里显示新密钥");
    assert_eq!(ui.new_key.get(), KEY);
    ui.close_reset_dialog();
    assert!(ui.reset_dialog.get(), "没勾「我已保存」不能关");
    ui.key_ack.set(true);
    ui.close_reset_dialog();
    assert!(!ui.reset_dialog.get());
    assert_eq!(ui.new_key.get(), "");
}

#[test]
fn late_reset_key_is_dropped_after_close() {
    let ui = SyncUi::new();
    windui::testing::run_with_ctx(|ctx| {
        ui.on_msg(
            ctx,
            SyncMsg::Command {
                op: "reset_remote".into(),
                result: Ok(json!({ "ok": true, "key": KEY })),
            },
        );
    });
    assert_eq!(ui.new_key.get(), "");
}

#[test]
fn storage_create_on_taken_location_recovers_own_key() {
    let ui = SyncUi::new();
    open_storage(&ui);
    ui.storage_stage.set(StorageStage::Create);
    ui.on_wizard(
        WizOp::StorageCreate,
        Err((0, "远端已有同步库，请粘贴同步密钥加入".into())),
    );
    assert_eq!(
        ui.storage_stage.get(),
        StorageStage::Create,
        "先核对本机密钥"
    );
    ui.on_wizard(
        WizOp::Recover { storage: true },
        Ok(vec![
            json!({ "ok": true, "remote_initialized": true, "joined": true }),
        ]),
    );
    ui.on_wizard(
        WizOp::RecoverExport { storage: true },
        Ok(vec![json!({ "key": KEY })]),
    );
    assert_eq!(ui.storage_stage.get(), StorageStage::Created);
    assert_eq!(ui.new_key.get(), KEY);
}

#[test]
fn storage_create_on_foreign_library_switches_to_join() {
    let ui = SyncUi::new();
    open_storage(&ui);
    ui.storage_stage.set(StorageStage::Create);
    ui.on_wizard(
        WizOp::StorageCreate,
        Err((0, "远端已有同步库，请粘贴同步密钥加入".into())),
    );
    ui.on_wizard(
        WizOp::Recover { storage: true },
        Ok(vec![
            json!({ "ok": true, "remote_initialized": true, "joined": false }),
        ]),
    );
    assert_eq!(ui.storage_stage.get(), StorageStage::Join);
}

#[test]
fn storage_join_on_empty_location_switches_to_create() {
    let ui = SyncUi::new();
    open_storage(&ui);
    ui.storage_stage.set(StorageStage::Join);
    ui.on_wizard(WizOp::StorageJoin, Err((0, "远端还没有同步库".into())));
    assert_eq!(ui.storage_stage.get(), StorageStage::Create);
}

#[test]
fn reset_dialog_stays_open_while_reset_in_flight() {
    let ui = SyncUi::new();
    ui.open_reset_dialog();
    ui.busy.set(true);
    ui.close_reset_dialog();
    assert!(ui.reset_dialog.get(), "新密钥回来前关框会把它丢掉");
}

#[test]
fn switching_storage_join_to_create_clears_pasted_key() {
    let ui = SyncUi::new();
    open_storage(&ui);
    ui.storage_stage.set(StorageStage::Join);
    ui.import_key.set("pasted".into());
    ui.on_wizard(WizOp::StorageJoin, Err((0, "远端还没有同步库".into())));
    assert_eq!(ui.import_key.get(), "");
}

// ───────────────────────────── 同步内容与清除已移除设备 ─────────────────────────────

#[test]
fn content_card_hidden_when_unconfigured() {
    use super::page::{NAV, card_shown};
    let i = NAV
        .iter()
        .position(|n| *n == "内容")
        .expect("导航里有「内容」");
    assert!(!card_shown(i, false));
    assert!(card_shown(i, true));
    assert!(
        i < NAV.iter().position(|n| *n == "存储").unwrap(),
        "内容卡在存储卡之前"
    );
}

#[test]
fn content_card_toggles_persist() {
    let ui = joined_ui(true);
    // 设置里的开关值落到界面
    ui.apply_form(&Form::from_settings(&json!({
        "settings": { "content": { "learned": false } }
    })));
    assert!(!ui.content_learned.get() && ui.content_completion.get() && ui.content_stats.get());
    // 没接通道：没发出去，开关退回已保存的值
    ui.content_learned.set(true);
    ui.set_content(|c| c.learned = true);
    assert!(!ui.content_learned.get(), "没发出去：退回已保存");
    // 回执成功：已保存值跟上开关；失败：开关退回
    windui::testing::run_with_ctx(|ctx| {
        ui.content_stats.set(false);
        ui.on_msg(
            ctx,
            SyncMsg::Command {
                op: "content".into(),
                result: Err("boom".into()),
            },
        );
        assert!(ui.content_stats.get() && ui.saved.borrow().content.stats);
        ui.content_stats.set(false);
        ui.content_sent.set(Some(ui.content_now()));
        ui.on_msg(
            ctx,
            SyncMsg::Command {
                op: "content".into(),
                result: Ok(json!({})),
            },
        );
        assert!(!ui.saved.borrow().content.stats);
        assert!(!ui.content_stats.get());
    });
}

#[test]
fn purge_row_shows_count_and_confirms() {
    let ui = joined_ui(true);
    ui.apply_status(&json!({
        "installed": true, "enabled": true, "joined": true,
        "removed_with_data": ["old-a", "old-b"]
    }));
    assert_eq!(ui.purge_text.get(), "已移除设备的数据：2 台");
    assert!(ui.take_purge_confirm().is_none(), "没点过入口不放行");
    ui.request_purge();
    assert!(ui.purge_dialog.get(), "第一次点击只弹确认框");
    assert_eq!(
        ui.take_purge_confirm(),
        Some(vec!["old-a".to_string(), "old-b".to_string()])
    );
    assert!(!ui.purge_dialog.get());
    assert!(ui.take_purge_confirm().is_none(), "确认只用一次");
    ui.request_purge();
    ui.purge_guard.disarm();
    assert!(ui.take_purge_confirm().is_none(), "取消后收回");
    assert!(PURGE_WARN.contains("无法恢复"));
}

#[test]
fn purge_row_hidden_when_none() {
    let ui = joined_ui(true);
    ui.apply_status(&json!({
        "installed": true, "enabled": true, "joined": true,
        "removed_with_data": ["old-a"]
    }));
    ui.apply_status(&json!({ "installed": true, "enabled": true, "joined": true }));
    assert!(ui.purge_text.get().is_empty(), "字段缺省：行隐藏");
}

#[test]
fn content_card_title_differs_from_nav_name() {
    use super::page::{CONTENT_TITLE, NAV};
    assert_eq!(CONTENT_TITLE, "同步内容");
    assert!(NAV.contains(&"内容"));
}

#[test]
fn content_dispatch_defers_while_busy_instead_of_reverting() {
    assert_eq!(content_dispatch(false, true), ContentDispatch::Send);
    assert_eq!(content_dispatch(true, true), ContentDispatch::Defer);
    assert_eq!(content_dispatch(false, false), ContentDispatch::Revert);
    assert_eq!(content_dispatch(true, false), ContentDispatch::Revert);
}

#[test]
fn content_toggle_in_flight_then_second_toggle_both_written() {
    let ui = joined_ui(true);
    // 第一个开关已发出（在途），用户又拨了第二个：signals 都是新值，pending 置位
    ui.content_learned.set(false);
    ui.content_sent.set(Some(Content {
        learned: false,
        ..Content::default()
    }));
    ui.content_stats.set(false);
    ui.content_pending.set(true);
    windui::testing::run_with_ctx(|ctx| {
        ui.on_msg(
            ctx,
            SyncMsg::Command {
                op: "content".into(),
                result: Ok(json!({})),
            },
        );
    });
    assert_eq!(
        ui.saved.borrow().content,
        Content {
            learned: false,
            ..Content::default()
        },
        "第一次回执只落第一个开关"
    );
    assert!(!ui.content_pending.get(), "pending 已消费");
    assert_eq!(
        ui.content_sent.get(),
        Some(Content {
            learned: false,
            completion: true,
            stats: false
        }),
        "按当前开关值重发，两项都在里面"
    );
    assert!(!ui.content_stats.get(), "在途时不把开关退回");
}

#[test]
fn content_failure_reverts_and_drops_pending() {
    let ui = joined_ui(true);
    ui.content_learned.set(false);
    ui.content_stats.set(false);
    ui.content_pending.set(true);
    ui.content_sent.set(Some(Content {
        learned: false,
        ..Content::default()
    }));
    windui::testing::run_with_ctx(|ctx| {
        ui.on_msg(
            ctx,
            SyncMsg::Command {
                op: "content".into(),
                result: Err("boom".into()),
            },
        );
    });
    assert_eq!(ui.content_now(), Content::default());
    assert!(!ui.content_pending.get());
}

#[test]
fn usage_row_follows_status() {
    let ui = SyncUi::new();
    ui.apply_status(&json!({ "installed": true, "joined": true }));
    assert_eq!(ui.usage_line.get(), "未统计");
    assert!(!ui.usage_ready.get(), "没统计过不给明细");
    ui.apply_status(&json!({
        "installed": true, "joined": true,
        "usage": { "computed_at": 1_795_000_000, "total": 2048,
                   "devices": [{ "id": "pc", "learned": 1024, "stats": 0 }] }
    }));
    assert_eq!(ui.usage_line.get(), "已用 2.0 KB");
    assert!(ui.usage_desc.get().starts_with("统计于"));
    assert!(ui.usage_ready.get());
    assert!(ui.usage_rows.get().iter().any(|(l, _)| l == "pc"));
    ui.apply_status(&json!({ "installed": true, "joined": true, "usage_busy": true }));
    assert_eq!(ui.usage_line.get(), "统计中…");
    assert!(ui.usage_busy.get());
    ui.apply_status(&json!({ "installed": true, "joined": true, "phase": "syncing" }));
    assert!(ui.syncing.get(), "同步中刷新置灰");
    ui.apply_status(&json!({ "installed": true, "joined": true, "phase": "idle" }));
    assert!(!ui.syncing.get());
}

#[test]
fn usage_rows_use_roster_names_once_loaded() {
    let ui = SyncUi::new();
    ui.apply_status(&json!({
        "installed": true, "joined": true,
        "usage": { "computed_at": 1, "total": 1,
                   "devices": [{ "id": "pc", "learned": 1, "stats": 0 },
                               { "id": "gone", "learned": 1, "stats": 0 }] }
    }));
    ui.devices.set(vec![dev("pc", true, true, 1)]);
    ui.update_usage_rows();
    let labels: Vec<String> = ui.usage_rows.get().into_iter().map(|(l, _)| l).collect();
    assert!(labels.contains(&"n-pc".to_string()), "{labels:?}");
    assert!(labels.contains(&"已移除的设备（gone）".to_string()));
}

#[test]
fn usage_dialog_opens_and_closes() {
    let ui = SyncUi::new();
    ui.open_usage_dialog();
    assert!(ui.usage_dialog.get());
    ui.usage_dialog.set(false);
    assert_eq!(super::page::ROW_USAGE, "已用空间");
}
