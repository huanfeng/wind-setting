use super::*;
use serde_json::json;

fn dev_in(id: &str, group: &str, gname: &str, platform: &str, ts: i64) -> DeviceView {
    DeviceView {
        id: id.into(),
        name: format!("n-{id}"),
        platform: platform.into(),
        app_version: "0.132.0".into(),
        last_seen: ts,
        is_self: false,
        readable: true,
        group: group.into(),
        group_name: gname.into(),
    }
}

fn today() -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2026, 11, 25).unwrap()
}

fn local_ts(d: u32, h: u32) -> i64 {
    use chrono::TimeZone;
    chrono::Local
        .with_ymd_and_hms(2026, 11, d, h, 30, 0)
        .unwrap()
        .timestamp()
}

// ───────────────────────────── 步骤 ─────────────────────────────

#[test]
fn steps_are_numbered_one_to_four() {
    assert_eq!(WizStep::Storage.number(), Some(1));
    assert_eq!(WizStep::Test.number(), Some(2));
    assert_eq!(WizStep::Key.number(), Some(3));
    assert_eq!(WizStep::Identity.number(), Some(4));
    assert_eq!(WizStep::Done.number(), None);
}

#[test]
fn created_key_skips_identity_and_import_asks_it() {
    assert_eq!(next_step(WizStep::Key, false, "init", false), WizStep::Done);
    assert_eq!(
        next_step(WizStep::Key, false, "import", false),
        WizStep::Identity
    );
}

#[test]
fn test_step_goes_to_key_unless_already_joined() {
    assert_eq!(next_step(WizStep::Storage, false, "", false), WizStep::Test);
    assert_eq!(next_step(WizStep::Test, false, "", false), WizStep::Key);
    assert_eq!(next_step(WizStep::Test, true, "", false), WizStep::Done);
    assert_eq!(next_step(WizStep::Test, true, "", true), WizStep::Identity);
    assert_eq!(
        next_step(WizStep::Identity, false, "", false),
        WizStep::Done
    );
}

#[test]
fn back_only_returns_to_storage_before_unlocking() {
    assert_eq!(back_step(WizStep::Storage), None);
    assert_eq!(back_step(WizStep::Test), Some(WizStep::Storage));
    assert_eq!(back_step(WizStep::Key), Some(WizStep::Storage));
    assert_eq!(back_step(WizStep::Identity), None, "同步密钥已生效，回不去");
    assert_eq!(back_step(WizStep::Done), None);
}

#[test]
fn done_text_per_route() {
    assert!(done_text(WizStep::Key, "init").contains("第一台"));
    assert!(done_text(WizStep::Identity, "").contains("已确认"));
    assert!(done_text(WizStep::Test, "").contains("重新开启"));
}

// ───────────────────────────── 命令 ─────────────────────────────

fn webdav() -> Form {
    Form {
        wd_url: "https://dav/".into(),
        wd_user: "u".into(),
        ..Form::default()
    }
}

#[test]
fn storage_check_needs_location_and_secret() {
    let blank = Form::default();
    assert!(check_wizard_storage(&blank, &blank, false, "pw").is_err());
    assert!(check_wizard_storage(&blank, &webdav(), false, "").is_err());
    assert_eq!(check_wizard_storage(&blank, &webdav(), false, "pw"), Ok(()));
    assert_eq!(
        check_wizard_storage(&webdav(), &webdav(), true, ""),
        Ok(()),
        "已存密码且位置没变可以留空"
    );
    let moved = Form {
        wd_url: "https://other/".into(),
        ..webdav()
    };
    assert!(
        check_wizard_storage(&webdav(), &moved, true, "").is_err(),
        "换位置 core 会清掉旧密码"
    );
    assert_eq!(
        check_wizard_storage(&webdav(), &webdav(), false, ""),
        Ok(()),
        "已有存储参数可留空：core 有已存密码就继续，没有则测试连接报错"
    );
}

#[test]
fn test_steps_save_enabled_settings_first_then_secret_test_status() {
    let steps = wizard_test_steps(&webdav(), vec![json!({ "password": "hunter2" })]);
    let names: Vec<String> = steps
        .iter()
        .map(|(m, p)| match p.get("op").and_then(Value::as_str) {
            Some(op) => format!("{m}:{op}"),
            None => m.to_string(),
        })
        .collect();
    assert_eq!(
        names,
        [
            "sync.setSettings",
            "sync.command:set_secret",
            "sync.command:test_connection",
            "sync.status"
        ]
    );
    assert_eq!(steps[0].1["settings"]["enabled"], json!(true));
    assert!(
        !steps[0].1.to_string().contains("hunter2"),
        "机密不进 sync.setSettings"
    );
}

#[test]
fn test_steps_without_new_secret_skip_set_secret() {
    let steps = wizard_test_steps(&webdav(), Vec::new());
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[1].1["op"], "test_connection");
}

#[test]
fn fresh_remote_creates_key_existing_remote_imports() {
    assert_eq!(wizard_key_steps(false, ""), Ok(create_key_steps()));
    assert_eq!(
        wizard_key_steps(false, "zzzz"),
        Ok(create_key_steps()),
        "远端为空时不看输入框"
    );
    assert_eq!(wizard_key_steps(true, " "), Err("请粘贴同步密钥"));
    let k = wizard_key_steps(true, "abcd-efgh").unwrap();
    assert_eq!(
        k[0].1,
        json!({ "op": "import_key", "args": { "key": "abcd-efgh" } })
    );
    assert!(
        k.iter().all(|(m, _)| *m != "sync.setSettings"),
        "同步密钥不经 sync.setSettings"
    );
}

#[test]
fn identity_steps_wrap_commands() {
    assert_eq!(
        identity_steps(&IdentityChoice::Join("g".into())),
        vec![(
            "sync.command",
            json!({ "op": "join_group", "args": { "group": "g", "mode": "theirs" } })
        )]
    );
}

// ───────────────────────────── 身份选项 ─────────────────────────────

#[test]
fn identity_view_when_my_id_exists() {
    let devs = vec![
        dev_in("me", "g1", "家里", "windows", local_ts(25, 9)),
        dev_in("o", "g2", "", "windows", 9),
    ];
    let (opts, def) = identity_options_for(PlatformClass::Desktop, "me", &devs);
    let v = identity_view(&opts, &def, "me", &devs, today());
    assert_eq!(v.mine, "恢复为 me（上次活跃：今天 09:30）");
    assert_eq!(v.default_mode, 0);
    assert_eq!(
        identity_pick(&v, 0, 0, 0),
        Some(IdentityChoice::Restore("me".into()))
    );
    assert_eq!(identity_pick(&v, 1, 0, 0), Some(IdentityChoice::NewDevice));
}

#[test]
fn identity_view_lists_devices_and_groups_otherwise() {
    let devs = vec![
        dev_in("w1", "g1", "家里", "windows", local_ts(24, 8)),
        dev_in("w2", "g1", "家里", "linux", local_ts(25, 9)),
        dev_in("w3", "g2", "", "windows", local_ts(20, 9)),
    ];
    let (opts, def) = identity_options_for(PlatformClass::Desktop, "mine", &devs);
    let v = identity_view(&opts, &def, "mine", &devs, today());
    assert_eq!(v.mine, "");
    assert_eq!(v.default_mode, 2, "默认新建独立组");
    let ids: Vec<&str> = v.devices.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["w2", "w1", "w3"]);
    assert!(v.devices[0].1.contains("n-w2") && v.devices[0].1.contains("w2"));
    assert_eq!(
        v.groups,
        vec![
            ("g1".to_string(), "家里 · 2 台".to_string()),
            ("g2".to_string(), "g2 · 1 台".to_string())
        ]
    );
    assert_eq!(
        identity_pick(&v, 0, 1, 0),
        Some(IdentityChoice::Restore("w1".into()))
    );
    assert_eq!(
        identity_pick(&v, 1, 0, 1),
        Some(IdentityChoice::Join("g2".into()))
    );
    assert_eq!(identity_pick(&v, 2, 0, 0), Some(IdentityChoice::NewGroup));
    assert_eq!(identity_pick(&v, 0, 9, 0), None, "下标越界不猜");
}

#[test]
fn identity_view_with_empty_roster_only_offers_new_group() {
    let (opts, def) = identity_options_for(PlatformClass::Desktop, "mine", &[]);
    let v = identity_view(&opts, &def, "mine", &[], today());
    assert!(v.devices.is_empty() && v.groups.is_empty());
    assert_eq!(
        identity_pick(&v, v.default_mode, 0, 0),
        Some(IdentityChoice::NewGroup)
    );
}

// ───────────────────────────── 界面状态 ─────────────────────────────

#[test]
fn storage_step_rejects_missing_password_without_advancing() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wd_url.set("https://dav/".into());
    ui.wd_user.set("u".into());
    ui.wizard_next();
    assert_eq!(ui.wiz_step.get(), WizStep::Storage);
    assert!(!ui.wiz_err.get().is_empty());
}

#[test]
fn storage_step_advances_and_clears_secret_on_dispatch() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wd_url.set("https://dav/".into());
    ui.wd_user.set("u".into());
    ui.wd_password.set("hunter2".into());
    ui.wizard_next();
    assert_eq!(ui.wiz_step.get(), WizStep::Test);
    assert_eq!(ui.wd_password.get(), "");
}

fn test_reply(remote_initialized: bool, joined: bool) -> Result<Vec<Value>, (usize, String)> {
    Ok(vec![
        json!({ "ok": true }),
        json!({ "ok": true, "remote_initialized": remote_initialized, "joined": joined }),
        json!({ "installed": true, "device": "me" }),
    ])
}

#[test]
fn test_failure_stays_with_inline_error() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Test);
    ui.on_wizard(WizOp::Test, Err((2, "401 Unauthorized".into())));
    assert!(!ui.wiz_test_ok.get());
    assert_eq!(ui.wiz_err.get(), "测试连接失败：401 Unauthorized");
    ui.wizard_next();
    assert_eq!(ui.wiz_step.get(), WizStep::Test, "失败不前进");
}

#[test]
fn test_result_decides_key_form() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Test);
    ui.on_wizard(WizOp::Test, test_reply(false, false));
    assert!(ui.wiz_test_ok.get() && !ui.wiz_remote_init.get());
    ui.wizard_next();
    assert_eq!(ui.wiz_step.get(), WizStep::Key);
}

const KEY: &str = "abcd-efgh-ijkl-mnop-qrst-uvwx-yz23-4567-abcd-efgh-ijkl-mnop-qrst";

#[test]
fn created_key_must_be_acked_before_finishing() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Key);
    ui.on_wizard(
        WizOp::CreateKey,
        Ok(vec![json!({ "ok": true, "key": KEY }), json!({})]),
    );
    assert_eq!(ui.wiz_step.get(), WizStep::Key, "先显示密钥，不直接完成");
    assert_eq!(ui.new_key.get(), KEY);
    assert!(!ui.key_ack.get());
    ui.wizard_next();
    assert_eq!(ui.wiz_step.get(), WizStep::Key, "没勾「我已保存」不能前进");
    ui.close_wizard();
    assert!(ui.wizard.get(), "没勾「我已保存」不能关");
    ui.wizard_back();
    assert_eq!(ui.wiz_step.get(), WizStep::Key, "密钥已生成，回不去");
    ui.key_ack.set(true);
    ui.wizard_next();
    assert_eq!(ui.wiz_step.get(), WizStep::Done);
    assert!(ui.wiz_done.get().contains("第一台"));
    ui.close_wizard();
    assert!(!ui.wizard.get());
    assert_eq!(ui.new_key.get(), "", "关向导即清掉密钥");
    assert!(!ui.key_ack.get());
}

#[test]
fn reopening_wizard_forgets_previous_key() {
    let ui = SyncUi::new();
    ui.new_key.set(KEY.into());
    ui.key_ack.set(true);
    ui.open_wizard();
    assert_eq!(ui.new_key.get(), "");
    assert!(!ui.key_ack.get());
}

#[test]
fn create_key_failure_keeps_create_form() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Key);
    ui.on_wizard(WizOp::CreateKey, Err((1, "x".into())));
    assert!(!ui.wiz_remote_init.get(), "不切粘贴形态");
    assert_eq!(ui.wiz_err.get(), "生成同步密钥失败：x");
    ui.on_wizard(WizOp::CreateKey, Err((0, "core 无响应".into())));
    assert!(!ui.wiz_remote_init.get());
}

#[test]
fn create_key_reply_alone_shows_key() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Key);
    ui.on_wizard(
        WizOp::CreateKey,
        Ok(vec![json!({ "ok": true, "key": KEY })]),
    );
    assert_eq!(ui.new_key.get(), KEY);
}

const TAKEN: &str = "远端已有同步库，请粘贴同步密钥加入";

#[test]
fn already_exists_but_local_key_verifies_shows_exported_key() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Key);
    ui.on_wizard(WizOp::CreateKey, Err((0, TAKEN.into())));
    assert!(!ui.wiz_remote_init.get(), "先核对本机密钥，不急着切粘贴");
    ui.on_wizard(
        WizOp::Recover { storage: false },
        Ok(vec![
            json!({ "ok": true, "remote_initialized": true, "joined": true }),
        ]),
    );
    ui.on_wizard(
        WizOp::RecoverExport { storage: false },
        Ok(vec![json!({ "key": KEY })]),
    );
    assert_eq!(ui.new_key.get(), KEY, "上次建库其实成功了：把密钥补给用户");
    assert!(!ui.key_ack.get());
    assert!(!ui.wiz_remote_init.get());
    assert_eq!(ui.wiz_err.get(), "");
}

#[test]
fn already_exists_and_not_ours_switches_to_paste() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Key);
    ui.on_wizard(WizOp::CreateKey, Err((0, TAKEN.into())));
    ui.on_wizard(
        WizOp::Recover { storage: false },
        Ok(vec![
            json!({ "ok": true, "remote_initialized": true, "joined": false }),
        ]),
    );
    assert!(ui.wiz_remote_init.get(), "改成粘贴形态");
    assert_eq!(ui.wiz_err.get(), TAKEN);
    assert_eq!(ui.new_key.get(), "");
}

#[test]
fn import_on_empty_remote_switches_to_create() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Key);
    ui.wiz_remote_init.set(true);
    ui.on_wizard(WizOp::ImportKey, Err((0, "远端还没有同步库".into())));
    assert!(!ui.wiz_remote_init.get(), "改成生成形态");
    assert_eq!(ui.wiz_err.get(), "远端还没有同步库");
}

#[test]
fn imported_key_goes_to_identity_and_is_cleared() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Key);
    ui.wiz_remote_init.set(true);
    ui.import_key.set(KEY.into());
    ui.on_wizard(
        WizOp::ImportKey,
        Ok(vec![
            json!({ "ok": true }),
            json!({ "installed": true, "needs_identity": true }),
        ]),
    );
    assert_eq!(ui.wiz_step.get(), WizStep::Identity);
    assert_eq!(ui.import_key.get(), "", "用过即清");
}

#[test]
fn wrong_key_says_so_and_keeps_input() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Key);
    ui.wiz_remote_init.set(true);
    ui.import_key.set("bad".into());
    ui.on_wizard(WizOp::ImportKey, Err((0, "同步密钥不对".into())));
    assert_eq!(ui.wiz_step.get(), WizStep::Key);
    assert_eq!(ui.wiz_err.get(), "同步密钥不对");
    assert_eq!(ui.import_key.get(), "bad", "失败不清，免得重贴");
}

#[test]
fn empty_paste_is_refused_without_sending() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Key);
    ui.wiz_remote_init.set(true);
    ui.wizard_next();
    assert_eq!(ui.wiz_err.get(), "请粘贴同步密钥");
    assert!(!ui.busy.get());
}

#[test]
fn retry_allowed_only_when_password_already_stored() {
    assert!(can_retry_test(None, 0), "没发新密码：重试照旧");
    assert!(!can_retry_test(Some(1), 0), "设置都没存上");
    assert!(!can_retry_test(Some(1), 1), "密码没存上，重试也没用");
    assert!(can_retry_test(Some(1), 2), "密码已存，测试失败可重试");
}

#[test]
fn test_retry_follows_failed_step() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wd_url.set("https://dav/".into());
    ui.wd_user.set("u".into());
    ui.wd_password.set("pw".into());
    ui.wizard_next();
    ui.on_wizard(WizOp::Test, Err((1, "写入凭据失败".into())));
    assert!(!ui.wiz_can_retry.get());
    ui.on_wizard(WizOp::Test, Err((2, "超时".into())));
    assert!(ui.wiz_can_retry.get());
    ui.on_wizard(WizOp::Test, test_reply(true, false));
    assert!(!ui.wiz_can_retry.get(), "通过后不再出重试");
}

#[test]
fn identity_view_takes_default_from_options() {
    let devs = vec![
        dev_in("w1", "g1", "家里", "windows", 5),
        dev_in("w2", "g2", "", "windows", 9),
    ];
    let (opts, _) = identity_options_for(PlatformClass::Desktop, "mine", &devs);
    let v = identity_view(
        &opts,
        &IdentityChoice::Join("g1".into()),
        "mine",
        &devs,
        today(),
    );
    assert_eq!((v.default_mode, v.default_sub), (1, 1));
    let v = identity_view(
        &opts,
        &IdentityChoice::Restore("w1".into()),
        "mine",
        &devs,
        today(),
    );
    assert_eq!((v.default_mode, v.default_sub), (0, 1));
}

#[test]
fn devices_reply_builds_options_with_default() {
    let ui = SyncUi::new();
    ui.device.set("desktop-7f3k".into());
    ui.open_identity_dialog();
    assert!(ui.identity_dialog.get() && !ui.id_loaded.get());
    ui.on_wizard(
        WizOp::Devices,
        Ok(vec![json!({ "devices": [
            { "id": "desktop-7f3k", "name": "台式机", "platform": "windows", "last_seen": 1,
              "readable": true, "group": "g", "group_name": "家里" }
        ] })]),
    );
    assert!(ui.id_loaded.get());
    assert!(ui.id_view.get().mine.starts_with("恢复为 desktop-7f3k"));
    assert_eq!(ui.id_mode.get(), 0);
}

#[test]
fn confirming_identity_closes_dialog_or_finishes_wizard() {
    let ui = SyncUi::new();
    ui.open_identity_dialog();
    ui.on_wizard(WizOp::Confirm, Ok(vec![json!({ "ok": true })]));
    assert!(!ui.identity_dialog.get());

    ui.open_wizard();
    ui.wiz_step.set(WizStep::Identity);
    ui.on_wizard(WizOp::Confirm, Err((0, "网络断了".into())));
    assert_eq!(ui.wiz_step.get(), WizStep::Identity);
    assert_eq!(ui.id_err.get(), "确认身份失败：网络断了");
    ui.on_wizard(WizOp::Confirm, Ok(vec![json!({ "ok": true })]));
    assert_eq!(ui.wiz_step.get(), WizStep::Done);
}

#[test]
fn late_wizard_reply_after_close_is_ignored() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Test);
    ui.close_wizard();
    ui.on_wizard(WizOp::Test, test_reply(true, false));
    assert!(!ui.wiz_test_ok.get());
    assert!(!ui.busy.get());
}

#[test]
fn switching_wizard_key_form_clears_pasted_key() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Key);
    ui.wiz_remote_init.set(true);
    ui.import_key.set("pasted".into());
    ui.on_wizard(WizOp::ImportKey, Err((0, "远端还没有同步库".into())));
    assert_eq!(ui.import_key.get(), "");
}

// ───────────────────────────── 产出密钥的命令在途 ─────────────────────────────

#[test]
fn key_yielding_ops_are_the_ones_that_bring_back_a_key() {
    for op in [
        WizOp::CreateKey,
        WizOp::StorageCreate,
        WizOp::Recover { storage: false },
        WizOp::RecoverExport { storage: true },
    ] {
        assert!(op.yields_key(), "{op:?}");
    }
    for op in [
        WizOp::Test,
        WizOp::ImportKey,
        WizOp::StorageSave,
        WizOp::StorageJoin,
    ] {
        assert!(!op.yields_key(), "{op:?}");
    }
}

/// 生成密钥 / 建库在途时关框，回执里的新密钥会被丢掉、用户从没见过：在途期间不许关，回执一到就解除。
#[test]
fn dialogs_stay_open_while_a_key_is_on_its_way() {
    let ui = SyncUi::new();
    ui.wizard.set(true);
    ui.storage_dialog.set(true);
    ui.busy.set(true);
    ui.key_pending.set(true);
    ui.close_wizard();
    ui.close_storage_dialog();
    assert!(ui.wizard.get() && ui.storage_dialog.get(), "建库在途不关");
    ui.on_wizard(WizOp::CreateKey, Err((0, "网络错误".into())));
    assert!(!ui.key_pending.get(), "回执一到就解除");
    ui.close_wizard();
    ui.close_storage_dialog();
    assert!(!ui.wizard.get() && !ui.storage_dialog.get());
}
