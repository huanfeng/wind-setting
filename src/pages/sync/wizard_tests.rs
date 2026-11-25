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
    assert_eq!(WizStep::Passphrase.number(), Some(3));
    assert_eq!(WizStep::Identity.number(), Some(4));
    assert_eq!(WizStep::Done.number(), None);
}

#[test]
fn init_passphrase_skips_identity() {
    assert_eq!(
        next_step(WizStep::Passphrase, false, "init", false),
        WizStep::Done
    );
}

#[test]
fn joining_always_asks_identity() {
    for mode in ["join", "import"] {
        assert_eq!(
            next_step(WizStep::Passphrase, false, mode, false),
            WizStep::Identity,
            "{mode}"
        );
    }
}

#[test]
fn change_mode_asks_identity_only_when_unconfirmed() {
    assert_eq!(
        next_step(WizStep::Passphrase, false, "change", true),
        WizStep::Identity
    );
    assert_eq!(
        next_step(WizStep::Passphrase, false, "change", false),
        WizStep::Done
    );
}

#[test]
fn test_step_goes_to_passphrase_unless_already_joined() {
    assert_eq!(next_step(WizStep::Storage, false, "", false), WizStep::Test);
    assert_eq!(
        next_step(WizStep::Test, false, "", false),
        WizStep::Passphrase
    );
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
    assert_eq!(back_step(WizStep::Passphrase), Some(WizStep::Storage));
    assert_eq!(back_step(WizStep::Identity), None, "口令已生效，回不去");
    assert_eq!(back_step(WizStep::Done), None);
}

#[test]
fn done_text_per_route() {
    assert!(done_text(WizStep::Passphrase, "init").contains("第一台"));
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
fn new_remote_passphrase_is_checked_twice() {
    assert_eq!(
        wizard_unlock_steps(false, false, "a", "b", ""),
        Err("两次输入的口令不一致")
    );
    let steps = wizard_unlock_steps(false, false, "pw", "pw", "").unwrap();
    assert_eq!(
        steps,
        vec![
            (
                "sync.command",
                json!({ "op": "set_passphrase", "args": { "passphrase": "pw" } })
            ),
            ("sync.status", json!({})),
        ]
    );
}

#[test]
fn existing_remote_takes_passphrase_once_or_key() {
    assert!(wizard_unlock_steps(true, false, "", "", "").is_err());
    let p = wizard_unlock_steps(true, false, "pw", "", "").unwrap();
    assert_eq!(p[0].1["op"], "set_passphrase");
    assert!(wizard_unlock_steps(true, true, "", "", "  ").is_err());
    let k = wizard_unlock_steps(true, true, "", "", " KEY ").unwrap();
    assert_eq!(
        k[0].1,
        json!({ "op": "import_key", "args": { "key": "KEY" } })
    );
    assert!(
        p.iter()
            .chain(k.iter())
            .all(|(m, _)| *m != "sync.setSettings"),
        "口令与密钥不经 sync.setSettings"
    );
}

#[test]
fn unlock_mode_reads_reply_or_import() {
    assert_eq!(
        unlock_mode(false, &json!({ "ok": true, "mode": "init" })),
        "init"
    );
    assert_eq!(unlock_mode(false, &json!({ "ok": true })), "join");
    assert_eq!(unlock_mode(true, &json!({ "ok": true })), "import");
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
fn test_result_decides_passphrase_form() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Test);
    ui.on_wizard(WizOp::Test, test_reply(false, false));
    assert!(ui.wiz_test_ok.get() && !ui.wiz_remote_init.get());
    ui.wizard_next();
    assert_eq!(ui.wiz_step.get(), WizStep::Passphrase);
}

#[test]
fn init_passphrase_finishes_without_identity() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Passphrase);
    ui.pass1.set("pw".into());
    ui.pass2.set("pw".into());
    ui.on_wizard(
        WizOp::Unlock { import: false },
        Ok(vec![json!({ "ok": true, "mode": "init" }), json!({})]),
    );
    assert_eq!(ui.wiz_step.get(), WizStep::Done);
    assert_eq!(ui.pass1.get(), "", "口令用过即清");
}

#[test]
fn imported_key_goes_to_identity() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Passphrase);
    ui.on_wizard(
        WizOp::Unlock { import: true },
        Ok(vec![
            json!({ "ok": true }),
            json!({ "installed": true, "needs_identity": true }),
        ]),
    );
    assert_eq!(ui.wiz_step.get(), WizStep::Identity);
}

#[test]
fn unlock_failure_keeps_inputs_and_shows_error() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Passphrase);
    ui.pass1.set("pw".into());
    ui.pass2.set("pw".into());
    ui.on_wizard(
        WizOp::Unlock { import: false },
        Err((0, "存储不可写".into())),
    );
    assert_eq!(ui.wiz_step.get(), WizStep::Passphrase);
    assert_eq!(ui.wiz_err.get(), "设置口令失败：存储不可写");
    assert_eq!(ui.pass1.get(), "pw", "失败不清，免得重输");
    assert_eq!(ui.pass2.get(), "pw");
}

#[test]
fn join_passphrase_failure_says_wrong_or_unverifiable() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.wiz_step.set(WizStep::Passphrase);
    ui.wiz_remote_init.set(true);
    ui.on_wizard(WizOp::Unlock { import: false }, Err((0, "解密失败".into())));
    assert_eq!(ui.wiz_err.get(), "口令不对或无法验证：解密失败");
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
fn generated_passphrase_fills_both_inputs() {
    let ui = SyncUi::new();
    ui.open_wizard();
    ui.generate_passphrase();
    let g = ui.wiz_generated.get();
    assert_eq!(g.len(), 24);
    assert_eq!(ui.pass1.get(), g);
    assert_eq!(ui.pass2.get(), g);
    ui.close_wizard();
    assert_eq!(ui.wiz_generated.get(), "", "关向导即忘掉");
    assert_eq!(ui.pass1.get(), "");
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
