use super::*;

#[test]
fn phase_maps_to_chinese_and_keeps_unknown() {
    assert_eq!(phase_text("not_installed"), "未安装同步组件");
    assert_eq!(phase_text("disabled"), "未启用");
    assert_eq!(phase_text("not_configured"), "未配置存储后端");
    assert_eq!(phase_text("needs_setup"), "需要设置口令或导入密钥");
    assert_eq!(phase_text("idle"), "空闲，已是最新");
    assert_eq!(phase_text("syncing"), "同步中…");
    assert_eq!(phase_text("error"), "出错");
    assert_eq!(phase_text("halted"), "已停止，需要处理");
    assert_eq!(
        phase_text("needs_passphrase"),
        "状态：needs_passphrase",
        "自拟阶段名不再认"
    );
    assert_eq!(phase_text("weird"), "状态：weird", "不认识的阶段原样带出");
    assert_eq!(phase_text(""), "状态未知");
}

#[test]
fn status_parses_and_summarises() {
    let s = StatusView::parse(&json!({
        "installed": true, "enabled": true, "phase": "idle", "last_ok": 0,
        "last_error": "", "pending": 3, "conflicts": 2, "has_secret": true
    }));
    assert!(s.installed && s.has_secret);
    assert_eq!(s.last_error, None, "空串错误不显示");
    assert_eq!(s.pending_text(), "3 项改动待上传");
    assert!(s.conflict_text().contains("2 个冲突副本"));
    assert!(s.conflict_text().contains(".conflict-"));
    assert!(
        !s.conflict_text().contains('\n'),
        "缺 conflict_paths 时只显示数量"
    );
    let p = StatusView::parse(&json!({
        "conflicts": 10,
        "conflict_paths": ["themes/a.conflict-d-1.json", "config.conflict-d-1.toml"]
    }));
    let t = p.conflict_text();
    assert!(t.contains("themes/a.conflict-d-1.json") && t.contains("10 个冲突副本"));
    assert!(t.contains("另有 8 个"));
    assert!(s.last_ok_text().starts_with("1970-01-01"));
    let none = StatusView::parse(&json!({}));
    assert_eq!(none.last_ok_text(), "尚未同步过");
    assert_eq!(none.conflict_text(), "");
    assert_eq!(none.pending_text(), "没有待上传的改动");
}

#[test]
fn passphrase_mismatch_and_empty_are_rejected() {
    assert_eq!(check_passphrase("abc", "abd"), Err("两次输入的口令不一致"));
    assert_eq!(check_passphrase("", ""), Err("口令不能为空"));
    assert_eq!(check_passphrase("abc", "abc"), Ok(()));
}

#[test]
fn secrets_never_enter_settings_and_empty_is_not_sent() {
    let f = Form {
        enabled: true,
        wd_url: "https://x/dav".into(),
        wd_user: "u".into(),
        ..Form::default()
    };
    let text = f.to_settings().to_string();
    assert!(!text.contains("password") && !text.contains("secret"));
    assert!(
        secret_commands(false, "", "zzz").is_empty(),
        "留空不覆盖已保存的"
    );
    let c = secret_commands(false, "pw", "zzz");
    assert_eq!(c, vec![json!({ "password": "pw" })]);
    let c = secret_commands(true, "pw", "sk");
    assert_eq!(c, vec![json!({ "password": "sk" })]);
}

#[test]
fn secret_placeholder_reveals_only_presence() {
    assert!(secret_placeholder(true).starts_with("已保存"));
    assert_eq!(secret_placeholder(false), "未设置");
}

#[test]
fn reset_requires_arming_passphrase_and_is_single_use() {
    let g = ResetGuard::default();
    assert!(g.take("n", "n").is_err(), "没点「重置远端…」就不给确认参数");
    g.arm();
    assert!(g.take("", "").is_err(), "空口令不让提交");
    assert!(g.take("a", "b").is_err(), "两次不一致不让提交");
    assert_eq!(
        g.take("new", "new"),
        Ok(json!({ "confirm": true, "passphrase": "new" })),
        "口令被拒不消耗确认"
    );
    assert!(g.take("new", "new").is_err(), "确认只用一次");
    g.arm();
    g.disarm();
    assert!(g.take("new", "new").is_err(), "取消后收回");
}

#[test]
fn connection_text_follows_remote_state() {
    assert!(
        connection_text(&json!({"ok": true, "remote_initialized": false})).contains("以本机为准")
    );
    assert!(
        connection_text(&json!({"remote_initialized": true, "joined": false})).contains("加入")
    );
    assert!(
        connection_text(&json!({"remote_initialized": true, "joined": true})).contains("已加入")
    );
}

#[test]
fn form_round_trips_and_omits_blank_backend_when_disabled() {
    let blank = Form::default();
    assert!(blank.to_settings().get("backend").is_none());
    let s3 = Form {
        enabled: true,
        s3: true,
        s3_endpoint: " https://s3.example.com ".into(),
        s3_bucket: "b".into(),
        s3_access_key_id: "AK".into(),
        s3_path_style: true,
        interval_min: 30,
        ..Form::default()
    };
    let v = s3.to_settings();
    assert_eq!(v["backend"]["type"], "s3");
    assert_eq!(v["backend"]["endpoint"], "https://s3.example.com");
    let back = Form::from_settings(&json!({ "settings": v }));
    assert_eq!(back.s3_endpoint, "https://s3.example.com");
    assert!(v.get("scope").is_none(), "同步范围已删除");
    assert_eq!(back.interval_min, 30);
    assert!(back.s3 && back.s3_path_style && back.enabled);
}

#[test]
fn interval_is_clamped() {
    assert_eq!(clamp_interval(0.0), 1);
    assert_eq!(clamp_interval(99999.0), 1440);
    assert_eq!(clamp_interval(14.6), 15);
    assert_eq!(clamp_interval(f64::NAN), 15);
}

#[test]
fn outcome_texts_and_exported_key() {
    let ok = outcome("export_key", &Ok(json!({ "key": "QUJD" })));
    assert!(ok.ok);
    assert_eq!(ok.key.as_deref(), Some("QUJD"));
    let empty = outcome("export_key", &Ok(json!({})));
    assert!(!empty.ok);
    let err = outcome("test_connection", &Err("401".into()));
    assert_eq!(err.toast, "测试连接失败：401");
    assert!(!err.ok);
    assert!(
        outcome("test_connection", &Ok(json!({})))
            .toast
            .starts_with("连接成功")
    );
}

const MASS_ERR: &str = "本机有 25/25 个文件看似被删除，为防误删已暂停。确认无误请点「继续同步」";

#[test]
fn error_kinds_are_classified_by_phase_and_text() {
    assert_eq!(classify_error("halted", MASS_ERR), ErrorKind::MassDelete);
    assert_eq!(
        classify_error("error", "a.toml 正在被编辑，本轮暂不同步"),
        ErrorKind::Notice
    );
    assert_eq!(classify_error("error", "401"), ErrorKind::Danger);
    assert_eq!(classify_error("halted", "别的原因"), ErrorKind::Danger);
    assert_eq!(classify_error("idle", ""), ErrorKind::None);
}

#[test]
fn save_sets_settings_before_secret() {
    let steps = save_steps(
        json!({ "enabled": true }),
        vec![("set_secret", json!({ "password": "p" }))],
    );
    assert_eq!(steps[0].0, "sync.setSettings");
    assert_eq!(steps[1].0, "sync.command");
    assert_eq!(steps[1].1["op"], "set_secret");
    assert_eq!(steps.len(), 2);
}

#[test]
fn location_change_is_detected_only_for_locating_fields() {
    let old = Form {
        wd_url: "https://a/dav".into(),
        wd_user: "u".into(),
        ..Form::default()
    };
    let mut new = old.clone();
    new.interval_min = 30;
    assert!(!backend_location_changed(&old, &new), "间隔不算换位置");
    new.wd_url = "https://b/dav".into();
    assert!(backend_location_changed(&old, &new));
    let first = Form::default();
    assert!(!backend_location_changed(&first, &new), "首次配置无旧密码");
    let mut s3 = old.clone();
    s3.s3 = true;
    s3.s3_bucket = "b".into();
    assert!(backend_location_changed(&old, &s3), "换后端类型");
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

fn dev_in(id: &str, group: &str, gname: &str, platform: &str, ts: i64) -> DeviceView {
    DeviceView {
        group: group.into(),
        group_name: gname.into(),
        platform: platform.into(),
        ..dev(id, false, true, ts)
    }
}

fn local_ts(y: i32, m: u32, d: u32, h: u32) -> i64 {
    use chrono::TimeZone;
    chrono::Local
        .with_ymd_and_hms(y, m, d, h, 30, 0)
        .unwrap()
        .timestamp()
}

#[test]
fn platform_names_are_mapped() {
    assert_eq!(platform_text("windows"), "Windows");
    assert_eq!(platform_text("macos"), "macOS");
    assert_eq!(platform_text("linux"), "Linux");
    assert_eq!(platform_text("android"), "Android");
    assert_eq!(platform_text("haiku"), "haiku");
    assert_eq!(platform_text(""), "未知平台");
}

#[test]
fn last_seen_is_today_yesterday_or_date() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 24).unwrap();
    assert_eq!(
        last_seen_text(local_ts(2026, 11, 24, 9), today),
        "今天 09:30"
    );
    assert_eq!(
        last_seen_text(local_ts(2026, 11, 23, 22), today),
        "昨天 22:30"
    );
    assert_eq!(
        last_seen_text(local_ts(2026, 11, 20, 8), today),
        "2026-11-20"
    );
    assert_eq!(last_seen_text(0, today), "最后在线未知");
}

#[test]
fn self_device_has_no_remove_button() {
    assert!(!dev("a", true, true, 1).removable());
    assert!(dev("b", false, true, 1).removable());
    assert!(dev("c", false, false, 1).removable(), "读不出的也能清理");
}

#[test]
fn unreadable_device_shows_unknown() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 24).unwrap();
    let d = dev("x", false, false, local_ts(2026, 11, 24, 9));
    assert_eq!(d.title(), "未知设备（无法解密）");
    assert_eq!(d.detail(today), "今天 09:30", "不显示平台与版本");
    let ok = dev("y", false, true, local_ts(2026, 11, 24, 9));
    assert_eq!(ok.detail(today), "macOS · v0.132.0 · 今天 09:30");
}

#[test]
fn devices_parse_self_first_then_recent() {
    let v = json!({ "devices": [
        { "id": "b", "name": "B", "last_seen": 10, "readable": true },
        { "id": "a", "name": "A", "last_seen": 5, "is_self": true, "readable": true },
        { "id": "c", "name": "", "last_seen": 20, "readable": false },
        { "name": "no-id" }
    ]});
    let ids: Vec<_> = parse_devices(&v).into_iter().map(|d| d.id).collect();
    assert_eq!(ids, ["a", "c", "b"]);
}

#[test]
fn device_name_round_trips_in_settings() {
    let f = Form {
        device_name: " 书房 ".into(),
        ..Form::default()
    };
    let v = f.to_settings();
    assert_eq!(v["device_name"], "书房");
    assert_eq!(
        Form::from_settings(&json!({ "settings": v })).device_name,
        "书房"
    );
}

#[test]
fn failure_texts_split_notice_from_error() {
    let (t, notice) = failure_text("立即同步", "正在同步，请稍后再试");
    assert!(notice);
    assert_eq!(t, "正在同步，请稍后再试");
    let (t, notice) = failure_text("保存", "拒绝：调用方权限不足");
    assert!(!notice);
    assert!(t.contains("拒绝：调用方权限不足") && t.contains("请从清风输入法设置程序操作"));
    assert!(outcome("sync_now", &Err("正在同步，请稍后再试".into())).notice);
    assert!(!outcome("sync_now", &Err("x".into())).notice);
}

#[test]
fn status_parses_group_fields_and_configured() {
    let s = StatusView::parse(&json!({
        "installed": true, "enabled": true, "joined": true, "device": "desktop-7f3k",
        "group": "g-ab", "group_name": "家里", "needs_identity": true
    }));
    assert_eq!(s.device, "desktop-7f3k");
    assert_eq!((s.group.as_str(), s.group_name.as_str()), ("g-ab", "家里"));
    assert!(s.needs_identity && s.configured());
    let none = StatusView::parse(&json!({}));
    assert!(!none.needs_identity && !none.configured());
    let not_joined = StatusView::parse(&json!({ "installed": true, "enabled": true }));
    assert!(!not_joined.configured(), "已装但没加入远端不算已配置");
    let not_installed = StatusView::parse(&json!({ "enabled": true, "joined": true }));
    assert!(!not_installed.configured());
    let disabled = StatusView::parse(&json!({ "installed": true, "joined": true }));
    assert!(!disabled.configured(), "停用后密钥还在，也回到未配置");
}

#[test]
fn settings_have_no_scope() {
    let v = Form::default().to_settings();
    assert!(v.get("scope").is_none());
}

#[test]
fn parse_devices_reads_group_fields() {
    let d = parse_devices(&json!({ "devices": [
        { "id": "a", "group": "g1", "group_name": "家里" },
        { "id": "b" }
    ]}));
    let a = d.iter().find(|x| x.id == "a").unwrap();
    assert_eq!((a.group.as_str(), a.group_name.as_str()), ("g1", "家里"));
    let b = d.iter().find(|x| x.id == "b").unwrap();
    assert_eq!((b.group.as_str(), b.group_name.as_str()), ("", ""));
}

#[test]
fn groups_put_mine_first_self_first_and_others_by_recent() {
    let mut me = dev_in("me", "g1", "", "windows", 100);
    me.is_self = true;
    let devs = vec![
        dev_in("old", "g2", "", "linux", 10),
        dev_in("m2", "g1", "", "android", 500),
        me,
        dev_in("new", "g3", "公司", "macos", 900),
        dev_in("old2", "g2", "旧组", "linux", 20),
    ];
    let gs = group_devices(&devs, "g1");
    let ids: Vec<&str> = gs.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, ["g1", "g3", "g2"], "本组在前，其余按最近活跃");
    assert!(gs[0].is_mine && !gs[1].is_mine);
    let mine: Vec<&str> = gs[0].devices.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(mine, ["me", "m2"], "本机在组内最前");
    assert_eq!(gs[0].name, "g1", "没有组名用组号");
    assert_eq!(gs[1].name, "公司");
    assert_eq!(gs[2].name, "旧组", "取组内第一个非空组名");
    assert_eq!(member_count(&gs, "g2"), 2);
    assert_eq!(member_count(&gs, "nope"), 0);
}

#[test]
fn passphrase_has_five_groups_from_alphabet() {
    let mut n = 0u32;
    let p = random_passphrase(|| {
        n += 1;
        n - 1
    });
    assert_eq!(p, "2345-6789-abcd-efgh-jkmn");
    assert_eq!(p.len(), 24);
    assert_eq!(p.matches('-').count(), 4);
    let q = random_passphrase(|| u32::MAX);
    assert!(
        q.chars()
            .all(|c| c == '-' || "23456789abcdefghjkmnpqrstuvwxyz".contains(c))
    );
    assert_eq!(q.len(), 24);
}

#[test]
fn identity_options_restore_self_when_remote_has_my_id() {
    let devs = vec![
        dev_in("me", "g1", "", "windows", 5),
        dev_in("o", "g2", "", "windows", 9),
    ];
    let (opts, def) = identity_options_for(PlatformClass::Desktop, "me", &devs);
    assert_eq!(
        opts,
        [
            IdentityChoice::Restore("me".into()),
            IdentityChoice::NewDevice
        ]
    );
    assert_eq!(def, IdentityChoice::Restore("me".into()));
}

#[test]
fn identity_options_otherwise_list_same_class_only() {
    let mut unreadable = dev_in("u", "g2", "", "windows", 1);
    unreadable.readable = false;
    let devs = vec![
        dev_in("w1", "g1", "家里", "windows", 5),
        dev_in("w2", "g2", "", "linux", 9),
        dev_in("ph", "g3", "", "android", 99),
        unreadable,
    ];
    let (opts, def) = identity_options_for(PlatformClass::Desktop, "mine", &devs);
    assert_eq!(
        opts,
        [
            IdentityChoice::Restore("w2".into()),
            IdentityChoice::Restore("w1".into()),
            IdentityChoice::Join("g2".into()),
            IdentityChoice::Join("g1".into()),
            IdentityChoice::NewGroup,
        ],
        "不列手机与不可读设备；按最近活跃"
    );
    assert_eq!(def, IdentityChoice::NewGroup);
    let (m, _) = identity_options_for(PlatformClass::Mobile, "mine", &devs);
    assert_eq!(
        m,
        [
            IdentityChoice::Restore("ph".into()),
            IdentityChoice::Join("g3".into()),
            IdentityChoice::NewGroup
        ]
    );
    let (empty, d) = identity_options_for(PlatformClass::Desktop, "mine", &[]);
    assert_eq!(empty, [IdentityChoice::NewGroup]);
    assert_eq!(d, IdentityChoice::NewGroup);
}

#[test]
fn identity_options_skip_empty_group_and_unreadable_self() {
    let mut me = dev_in("me", "g1", "", "windows", 5);
    me.readable = false;
    let devs = vec![me, dev_in("w", "", "", "windows", 9)];
    let (opts, def) = identity_options_for(PlatformClass::Desktop, "me", &devs);
    assert_eq!(
        opts,
        [
            IdentityChoice::Restore("w".into()),
            IdentityChoice::NewGroup
        ],
        "我的号不可读时走其他设备分支；空组号不列 Join"
    );
    assert_eq!(def, IdentityChoice::NewGroup);
}

#[test]
fn identity_commands_per_choice() {
    assert_eq!(
        identity_commands(&IdentityChoice::Restore("x".into())),
        vec![("claim_device", json!({ "dev": "x" }))]
    );
    assert_eq!(
        identity_commands(&IdentityChoice::NewDevice),
        vec![("confirm_identity", json!({}))]
    );
    assert_eq!(
        identity_commands(&IdentityChoice::Join("g".into())),
        vec![("join_group", json!({ "group": "g", "mode": "theirs" }))]
    );
    assert_eq!(
        identity_commands(&IdentityChoice::NewGroup),
        vec![("confirm_identity", json!({}))]
    );
}

#[test]
fn sync_now_goes_through_core_not_command() {
    assert_eq!(sync_now_steps(), vec![("sync.now", json!({}))]);
}

#[test]
fn sync_now_success_is_left_to_core_toast() {
    let o = outcome("sync_now", &Ok(json!({ "outcome": "requested" })));
    assert!(o.ok && o.toast.is_empty(), "结果由 core 弹，页内不另报");
    assert!(!outcome("sync_now", &Err("连不上".into())).toast.is_empty());
}

fn webdav_form() -> Form {
    Form {
        enabled: true,
        wd_url: "https://dav.example.com/dav/".into(),
        wd_user: "me".into(),
        ..Form::default()
    }
}

#[test]
fn disable_only_turns_off_and_keeps_backend() {
    let steps = disable_steps(&webdav_form());
    assert_eq!(steps.len(), 1, "不删远端，不发其他命令");
    assert_eq!(steps[0].0, "sync.setSettings");
    let s = &steps[0].1["settings"];
    assert_eq!(s["enabled"], false);
    assert_eq!(s["backend"]["url"], "https://dav.example.com/dav/");
}

#[test]
fn storage_summary_per_backend() {
    assert_eq!(
        storage_summary(&webdav_form()),
        "WebDAV · https://dav.example.com/dav/ · 用户 me"
    );
    let s3 = Form {
        s3: true,
        s3_endpoint: "https://s3.example.com".into(),
        s3_bucket: "wind".into(),
        s3_prefix: "a/".into(),
        ..Form::default()
    };
    assert_eq!(
        storage_summary(&s3),
        "S3 · https://s3.example.com · wind/a/"
    );
    assert_eq!(storage_summary(&Form::default()), "未设置");
}

#[test]
fn storage_change_in_place_sends_no_unlock() {
    let old = webdav_form();
    let unlock = unlock_command(false, "口令", "");
    let steps = storage_change_steps(&old, &old, vec![json!({ "password": "p" })], unlock).unwrap();
    let ops: Vec<_> = steps.iter().map(|(m, v)| (*m, v["op"].clone())).collect();
    assert_eq!(
        ops,
        vec![
            ("sync.setSettings", Value::Null),
            ("sync.command", json!("set_secret"))
        ]
    );
}

#[test]
fn storage_move_needs_password_and_unlock() {
    let old = webdav_form();
    let new = Form {
        wd_url: "https://other/dav/".into(),
        ..old.clone()
    };
    let pw = || vec![json!({ "password": "p" })];
    assert!(
        storage_change_steps(&old, &new, pw(), None).is_err(),
        "要重填口令或密钥"
    );
    assert!(
        storage_change_steps(&old, &new, vec![], unlock_command(false, "x", "")).is_err(),
        "位置变了已存密码会被清掉"
    );
    let steps = storage_change_steps(&old, &new, pw(), unlock_command(false, "x", "")).unwrap();
    assert_eq!(steps[0].0, "sync.setSettings", "先换位置再写密码");
    assert_eq!(steps[1].1["op"], "set_secret");
    assert_eq!(
        steps[2].1,
        json!({ "op": "set_passphrase", "args": { "passphrase": "x" } })
    );
    let steps = storage_change_steps(&old, &new, pw(), unlock_command(true, "x", " k ")).unwrap();
    assert_eq!(
        steps[2].1,
        json!({ "op": "import_key", "args": { "key": "k" } })
    );
    assert_eq!(unlock_command(true, "x", "  "), None);
    assert_eq!(unlock_command(false, "", "k"), None);
}

#[test]
fn interval_saves_only_when_changed() {
    let saved = webdav_form();
    assert_eq!(interval_change(&saved, 15.0), None);
    let f = interval_change(&saved, 30.4).unwrap();
    assert_eq!(f.interval_min, 30);
    assert_eq!(f.wd_url, saved.wd_url, "其余设置原样");
}

#[test]
fn duplicate_device_error_is_detected() {
    let s = StatusView::parse(&json!({
        "phase": "error",
        "last_error": "另一台设备在使用相同设备号（desktop-7f3k），请给本机改号"
    }));
    assert!(s.duplicate_device());
    assert!(
        !StatusView::parse(&json!({ "phase": "error", "last_error": "401" })).duplicate_device()
    );
}

#[test]
fn conflict_summary_counts_paths_too() {
    let s = StatusView::parse(&json!({ "conflicts": 0, "conflict_paths": ["a", "b"] }));
    assert_eq!(s.conflict_summary(), "2 个冲突副本");
    assert_eq!(StatusView::parse(&json!({})).conflict_summary(), "");
}

#[test]
fn group_line_names_group_and_counts_members() {
    let devs = vec![
        dev_in("a", "g1", "家里", "windows", 3),
        dev_in("b", "g1", "", "macos", 2),
        dev_in("c", "g2", "公司", "windows", 1),
    ];
    assert_eq!(group_line("g1", "家里", &devs), "家里 · 2 台设备");
    assert_eq!(
        group_line("g2", "", &devs),
        "g2 · 1 台设备",
        "没有组名用组号"
    );
    assert_eq!(group_line("g1", "家里", &[]), "家里", "名册没到不写台数");
    assert_eq!(group_line("", "", &[]), "");
}
