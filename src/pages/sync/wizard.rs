//! 「开始配置…」向导与「确认本机身份」对话框（设计见 `sync-device-groups.md` §4.5、§7）。
//!
//! 四步：存储 → 测试连接 → 口令 → 本机身份，之后是完成页。每步的命令在后台线程依次发，
//! 全部成功才前进，失败就地显示；关掉向导不回滚已完成的步骤（还没确认身份的话，
//! 状态卡会给「确认身份…」，打开的对话框与第 4 步同一份内容）。
//!
//! - 第 2 步：先存设置（`enabled: true`），再 `set_secret`、`test_connection`（顺序见
//!   [`save_steps`]），远端是否已初始化决定第 3 步是「设置口令」还是「输入口令 / 粘贴密钥」。
//! - 第 3 步：设置口令即初始化远端，core 自动确认身份，跳过第 4 步；用口令加入或导入密钥
//!   之后必须确认身份，core 在那之前不同步。
//!
//! 外框照首启设置向导（`src/wizard/view.rs`）：定宽定高，每步一个滚动容器按步骤显隐。

use serde_json::{Value, json};
use windui::prelude::*;

use super::dialogs::{btn, btn2, field, muted, storage_fields, tone};
use super::*;

const DIALOG_W: i32 = 600;
const DIALOG_H: i32 = 560;

// ───────────────────────────── 纯逻辑 ─────────────────────────────

/// 向导所在的步骤。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WizStep {
    Storage,
    Test,
    Passphrase,
    Identity,
    Done,
}

impl WizStep {
    /// 标题行的「n / 4」；完成页不编号。
    pub fn number(self) -> Option<usize> {
        match self {
            WizStep::Storage => Some(1),
            WizStep::Test => Some(2),
            WizStep::Passphrase => Some(3),
            WizStep::Identity => Some(4),
            WizStep::Done => None,
        }
    }
}

/// 向导与确认身份对话框发出的命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizOp {
    /// 保存存储设置并测试连接。
    Test,
    /// 设置 / 输入口令，或导入密钥。
    Unlock { import: bool },
    /// 读设备名册（身份选项）。
    Devices,
    /// 按所选身份发命令。
    Confirm,
}

/// 当前步骤成功后去哪一步。`joined` 是测试连接时本机已加入（重新开启同步）；`mode` 是
/// `set_passphrase` 回执的 mode，导入密钥记作 `"import"`；`needs_identity` 取自随后的状态。
pub(crate) fn next_step(step: WizStep, joined: bool, mode: &str, needs_identity: bool) -> WizStep {
    let identity_or_done = |needs: bool| {
        if needs {
            WizStep::Identity
        } else {
            WizStep::Done
        }
    };
    match step {
        WizStep::Storage => WizStep::Test,
        WizStep::Test if joined => identity_or_done(needs_identity),
        WizStep::Test => WizStep::Passphrase,
        WizStep::Passphrase => match mode {
            "init" => WizStep::Done,
            "change" => identity_or_done(needs_identity),
            _ => WizStep::Identity,
        },
        WizStep::Identity | WizStep::Done => WizStep::Done,
    }
}

/// 「上一步」的去处：口令生效之后就回不去了。
pub(crate) fn back_step(step: WizStep) -> Option<WizStep> {
    match step {
        WizStep::Test | WizStep::Passphrase => Some(WizStep::Storage),
        _ => None,
    }
}

/// 完成页的一句话，按从哪一步结束区分。
pub(crate) fn done_text(from: WizStep, mode: &str) -> &'static str {
    match (from, mode) {
        (WizStep::Passphrase, "init") => {
            "同步已开启。本机是第一台设备，其他设备加入时输入同一个口令即可。"
        }
        (WizStep::Test, _) => "本机此前已加入，同步已重新开启。",
        (WizStep::Identity, _) => "已确认本机身份，同步会在后台开始。",
        _ => "口令已确认，同步会在后台开始。",
    }
}

/// 第 1 步的校验：要有存储地址；本机没存过密码、或换了存储位置（core 会清掉旧密码）时要填密码。
pub(crate) fn check_wizard_storage(
    saved: &Form,
    new: &Form,
    has_secret: bool,
    secret: &str,
) -> Result<(), &'static str> {
    if new.backend_blank() {
        return Err("请填写存储地址");
    }
    if secret.is_empty() && (!has_secret || backend_location_changed(saved, new)) {
        return Err(if new.s3 {
            "请填写 Secret"
        } else {
            "请填写密码"
        });
    }
    Ok(())
}

/// 第 2 步：存设置（启用）→ 存密码 → 测试连接 → 读状态（看是否待确认身份）。
pub(crate) fn wizard_test_steps(form: &Form, secrets: Vec<Value>) -> Vec<Step> {
    let on = Form {
        enabled: true,
        ..form.clone()
    };
    let mut cmds: Vec<(&'static str, Value)> =
        secrets.into_iter().map(|a| ("set_secret", a)).collect();
    cmds.push(("test_connection", json!({})));
    let mut steps = save_steps(on.to_settings(), cmds);
    steps.push(("sync.status", json!({})));
    steps
}

/// 第 3 步：远端为空时两次输入的口令；远端已有时口令（输一次）或导出的密钥。之后读一次状态。
pub(crate) fn wizard_unlock_steps(
    remote_initialized: bool,
    use_key: bool,
    pass1: &str,
    pass2: &str,
    key: &str,
) -> Result<Vec<Step>, &'static str> {
    let (op, args) = if !remote_initialized {
        check_passphrase(pass1, pass2)?;
        ("set_passphrase", json!({ "passphrase": pass1 }))
    } else if use_key {
        match key.trim() {
            "" => return Err("请粘贴导出的密钥"),
            k => ("import_key", json!({ "key": k })),
        }
    } else if pass1.is_empty() {
        return Err("请输入口令");
    } else {
        ("set_passphrase", json!({ "passphrase": pass1 }))
    };
    Ok(vec![
        ("sync.command", json!({ "op": op, "args": args })),
        ("sync.status", json!({})),
    ])
}

/// 第 3 步回执的 mode：导入密钥为 `"import"`；口令回执缺 mode 时按加入处理（要确认身份）。
pub(crate) fn unlock_mode(import: bool, reply: &Value) -> String {
    if import {
        return "import".to_string();
    }
    reply
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("join")
        .to_string()
}

/// 测试连接失败后能否「重试」（不再带密码）：这次没发新密码，或 `set_secret`（第 `secret_step` 步）已成功。
pub(crate) fn can_retry_test(secret_step: Option<usize>, failed_at: usize) -> bool {
    secret_step.is_none_or(|i| failed_at > i)
}

/// 所选身份对应的 RPC 步骤。
pub(crate) fn identity_steps(c: &IdentityChoice) -> Vec<Step> {
    identity_commands(c)
        .into_iter()
        .map(|(op, args)| ("sync.command", json!({ "op": op, "args": args })))
        .collect()
}

/// 身份选项的界面形态。
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub(crate) struct IdentityView {
    /// 远端已有本机设备号时「恢复为 …（上次活跃：…）」的文案；空 = 列表形态。
    pub mine: String,
    pub my_id: String,
    /// 列表形态：可恢复的设备 `(设备号, 文案)`、可加入的组 `(组号, 「组名 · N 台」)`。
    pub devices: Vec<(String, String)>,
    pub groups: Vec<(String, String)>,
    /// 预选项：「本机」形态 0 = 恢复 / 1 = 作为新设备；列表形态 0 = 恢复 / 1 = 加入组 / 2 = 新建组。
    pub default_mode: usize,
    /// 预选项落在子列表（设备或组）里时的下标。
    pub default_sub: usize,
}

/// 把 [`identity_options`] 的结果整理成界面用的形态（文案、名册里的名字与台数）。
pub(crate) fn identity_view(
    opts: &[IdentityChoice],
    default: &IdentityChoice,
    my_id: &str,
    devs: &[DeviceView],
    today: chrono::NaiveDate,
) -> IdentityView {
    let find = |id: &str| devs.iter().find(|d| d.id == id);
    let mut v = IdentityView {
        my_id: my_id.to_string(),
        ..IdentityView::default()
    };
    if opts.contains(&IdentityChoice::NewDevice) {
        let seen = find(my_id).map_or(0, |d| d.last_seen);
        v.mine = format!(
            "恢复为 {my_id}（上次活跃：{}）",
            last_seen_text(seen, today)
        );
        v.default_mode = usize::from(*default == IdentityChoice::NewDevice);
        return v;
    }
    // 组的台数按名册里全部可读设备算（与设备卡一致），不只数同类设备：加入后同步的就是这一组。
    let readable: Vec<DeviceView> = devs.iter().filter(|d| d.readable).cloned().collect();
    let groups = group_devices(&readable, "");
    for o in opts {
        match o {
            IdentityChoice::Restore(id) => {
                if let Some(d) = find(id) {
                    let label = format!("{}（{id}） · {}", d.title(), d.detail(today));
                    v.devices.push((id.clone(), label));
                }
            }
            IdentityChoice::Join(g) => {
                let (name, n) = groups
                    .iter()
                    .find(|x| &x.id == g)
                    .map_or((g.clone(), 0), |x| (x.name.clone(), x.devices.len()));
                v.groups.push((g.clone(), format!("{name} · {n} 台")));
            }
            _ => {}
        }
    }
    let pos = |items: &[(String, String)], id: &str| items.iter().position(|(x, _)| x == id);
    (v.default_mode, v.default_sub) = match default {
        IdentityChoice::Restore(id) => (0, pos(&v.devices, id).unwrap_or(0)),
        IdentityChoice::Join(g) => (1, pos(&v.groups, g).unwrap_or(0)),
        _ => (2, 0),
    };
    v
}

/// 界面上的选择 → 身份；下标对不上时不猜。
pub(crate) fn identity_pick(
    v: &IdentityView,
    mode: usize,
    dev: usize,
    group: usize,
) -> Option<IdentityChoice> {
    if !v.mine.is_empty() {
        return match mode {
            0 => Some(IdentityChoice::Restore(v.my_id.clone())),
            1 => Some(IdentityChoice::NewDevice),
            _ => None,
        };
    }
    match mode {
        0 => v
            .devices
            .get(dev)
            .map(|(id, _)| IdentityChoice::Restore(id.clone())),
        1 => v
            .groups
            .get(group)
            .map(|(g, _)| IdentityChoice::Join(g.clone())),
        2 => Some(IdentityChoice::NewGroup),
        _ => None,
    }
}

/// 依次执行，遇错即停；成功时带回每一步的回执，失败时带回出错的是第几步。
fn run_all(steps: Vec<Step>) -> Result<Vec<Value>, (usize, String)> {
    steps
        .into_iter()
        .enumerate()
        .map(|(i, (method, params))| rpc::call_core(method, &params).map_err(|e| (i, e)))
        .collect()
}

// ───────────────────────────── 界面状态 ─────────────────────────────

impl SyncUi {
    /// 「开始配置…」：从已保存的存储设置开始，清掉上次留下的一切。
    pub(super) fn open_wizard(&self) {
        self.reset_storage_form();
        self.clear_secrets();
        self.wiz_generated.set(String::new());
        self.wiz_step.set(WizStep::Storage);
        self.wiz_test_ok.set(false);
        self.wiz_test_text.set(String::new());
        self.wiz_can_retry.set(false);
        *self.wiz_sent.borrow_mut() = None;
        self.wiz_remote_init.set(false);
        self.wiz_joined.set(false);
        self.wiz_needs_identity.set(false);
        self.wiz_use_key.set(false);
        self.wiz_err.set(String::new());
        self.wiz_done.set(String::new());
        self.reset_identity();
        self.wizard.set(true);
    }

    /// 关向导（取消、×、ESC、完成）：已完成的步骤不回滚，输入的机密清掉。
    pub(super) fn close_wizard(&self) {
        self.wizard.set(false);
        self.clear_secrets();
        self.wiz_generated.set(String::new());
        self.reset_storage_form();
    }

    /// 状态卡「确认身份…」：与向导第 4 步同一份内容。
    pub(super) fn open_identity_dialog(&self) {
        self.reset_identity();
        self.identity_dialog.set(true);
        self.load_identity();
    }

    fn reset_identity(&self) {
        self.id_view.set(IdentityView::default());
        self.id_loaded.set(false);
        self.id_mode.set(0);
        self.id_dev_pick.set(0);
        self.id_group_pick.set(0);
        self.id_err.set(String::new());
    }

    fn load_identity(&self) {
        self.id_loaded.set(false);
        self.id_err.set(String::new());
        self.spawn_wizard(
            WizOp::Devices,
            vec![("sync.command", json!({ "op": "list_devices", "args": {} }))],
        );
    }

    /// 「下一步」/「确定」/「完成」：按当前步骤校验、发命令；回执到了才前进。
    pub(super) fn wizard_next(&self) {
        if self.busy.get() {
            return;
        }
        match self.wiz_step.get() {
            WizStep::Storage => {
                let new = self.storage_form();
                let (wd, s3) = (self.wd_password.get(), self.s3_secret.get());
                let secret = if new.s3 { &s3 } else { &wd };
                let saved = self.saved.borrow().clone();
                if let Err(e) = check_wizard_storage(&saved, &new, self.has_secret.get(), secret) {
                    self.wiz_err.set(e.to_string());
                    return;
                }
                let secrets = secret_commands(new.s3, &wd, &s3);
                // wizard_test_steps 里 set_secret 紧跟在 setSettings 之后。
                let secret_step = (!secrets.is_empty()).then_some(1);
                let steps = wizard_test_steps(&new, secrets);
                *self.wiz_sent.borrow_mut() = Some((new, secret_step));
                self.clear_secrets();
                self.wiz_test_ok.set(false);
                self.wiz_test_text.set(String::new());
                self.go(WizStep::Test, WizStep::Storage, "");
                self.spawn_wizard(WizOp::Test, steps);
            }
            WizStep::Test if self.wiz_test_ok.get() => {
                let next = next_step(
                    WizStep::Test,
                    self.wiz_joined.get(),
                    "",
                    self.wiz_needs_identity.get(),
                );
                self.go(next, WizStep::Test, "");
            }
            WizStep::Test => {}
            WizStep::Passphrase => {
                let remote = self.wiz_remote_init.get();
                let use_key = remote && self.wiz_use_key.get();
                match wizard_unlock_steps(
                    remote,
                    use_key,
                    &self.pass1.get(),
                    &self.pass2.get(),
                    &self.import_key.get(),
                ) {
                    Err(e) => self.wiz_err.set(e.to_string()),
                    Ok(steps) => {
                        self.wiz_err.set(String::new());
                        self.spawn_wizard(WizOp::Unlock { import: use_key }, steps);
                    }
                }
            }
            WizStep::Identity => self.confirm_identity(),
            WizStep::Done => self.close_wizard(),
        }
    }

    /// 第 2 步「重试」：密码已存上，只重发设置与测试，不再要密码。
    fn retry_test(&self) {
        let Some((form, _)) = self.wiz_sent.borrow().clone() else {
            return;
        };
        *self.wiz_sent.borrow_mut() = Some((form.clone(), None));
        self.wiz_err.set(String::new());
        self.wiz_can_retry.set(false);
        self.spawn_wizard(WizOp::Test, wizard_test_steps(&form, Vec::new()));
    }

    fn wizard_back(&self) {
        if let Some(b) = back_step(self.wiz_step.get()) {
            self.pass1.set(String::new());
            self.pass2.set(String::new());
            self.import_key.set(String::new());
            self.wiz_generated.set(String::new());
            self.go(b, self.wiz_step.get(), "");
        }
    }

    fn go(&self, step: WizStep, from: WizStep, mode: &str) {
        self.wiz_err.set(String::new());
        if step == WizStep::Done {
            self.wiz_done.set(done_text(from, mode).to_string());
        }
        self.wiz_step.set(step);
        if step == WizStep::Identity {
            self.reset_identity();
            self.load_identity();
        }
    }

    fn confirm_identity(&self) {
        if self.busy.get() || !self.id_loaded.get() {
            return;
        }
        let pick = identity_pick(
            &self.id_view.get(),
            self.id_mode.get(),
            self.id_dev_pick.get(),
            self.id_group_pick.get(),
        );
        match pick {
            None => self.id_err.set("请选择一项".to_string()),
            Some(c) => {
                self.id_err.set(String::new());
                self.spawn_wizard(WizOp::Confirm, identity_steps(&c));
            }
        }
    }

    /// 「随机生成」：填进两个口令框并明文显示；系统随机数不可用就请用户自己输。
    pub(super) fn generate_passphrase(&self) {
        match random_passphrase_os() {
            Some(p) => {
                self.pass1.set(p.clone());
                self.pass2.set(p.clone());
                self.wiz_generated.set(p);
                self.wiz_err.set(String::new());
            }
            None => self.wiz_err.set("无法生成随机口令，请自己输入".to_string()),
        }
    }

    /// 后台依次发若干步，回执全部带回（测试连接要看结果、口令要看 mode）。
    fn spawn_wizard(&self, op: WizOp, steps: Vec<Step>) {
        if self.busy.get() {
            return;
        }
        if self.inline_rpc.get() {
            self.busy.set(true);
            self.on_wizard(op, run_all(steps));
            return;
        }
        let Some(tx) = self.tx.borrow().clone() else {
            return;
        };
        self.busy.set(true);
        std::thread::spawn(move || {
            let _ = tx.send(SyncMsg::Wizard {
                op,
                result: run_all(steps),
            });
        });
    }

    /// 向导 / 确认身份的回执。框已关才回来的结果只刷新页面，不再动向导状态。
    pub(super) fn on_wizard(&self, op: WizOp, result: Result<Vec<Value>, (usize, String)>) {
        self.busy.set(false);
        let open = match op {
            WizOp::Test | WizOp::Unlock { .. } => self.wizard.get(),
            WizOp::Devices | WizOp::Confirm => self.wizard.get() || self.identity_dialog.get(),
        };
        if open {
            match result {
                Ok(rs) => self.wizard_ok(op, &rs),
                Err((at, e)) => self.wizard_failed(op, at, &e),
            }
        }
        if self.interval_pending.get() {
            self.save_interval();
        }
        if op != WizOp::Devices {
            self.refresh_all();
        }
    }

    fn wizard_failed(&self, op: WizOp, at: usize, e: &str) {
        let label = match op {
            WizOp::Test => "测试连接",
            WizOp::Unlock { import: true } => "导入密钥",
            WizOp::Unlock { import: false } => "设置口令",
            WizOp::Devices => "读取设备列表",
            WizOp::Confirm => "确认身份",
        };
        let (mut text, notice) = failure_text(label, e);
        // 远端已有数据时口令是拿来验证的：失败多半是口令不对。
        if op == (WizOp::Unlock { import: false }) && self.wiz_remote_init.get() && !notice {
            text = format!("口令不对或无法验证：{e}");
        }
        match op {
            WizOp::Test => {
                let secret_step = self.wiz_sent.borrow().as_ref().and_then(|(_, i)| *i);
                self.wiz_test_ok.set(false);
                self.wiz_can_retry.set(can_retry_test(secret_step, at));
                self.wiz_err.set(text);
            }
            WizOp::Unlock { .. } => self.wiz_err.set(text),
            WizOp::Devices | WizOp::Confirm => self.id_err.set(text),
        }
    }

    fn wizard_ok(&self, op: WizOp, rs: &[Value]) {
        let status = rs.last().cloned().unwrap_or(Value::Null);
        match op {
            WizOp::Test => {
                let reply = rs
                    .len()
                    .checked_sub(2)
                    .and_then(|i| rs.get(i))
                    .cloned()
                    .unwrap_or(Value::Null);
                let flag = |k: &str| reply.get(k).and_then(Value::as_bool).unwrap_or(false);
                self.apply_status(&status);
                self.wiz_remote_init.set(flag("remote_initialized"));
                self.wiz_joined.set(flag("joined"));
                self.wiz_needs_identity
                    .set(StatusView::parse(&status).needs_identity);
                self.wiz_test_text.set(connection_text(&reply));
                self.wiz_err.set(String::new());
                self.wiz_can_retry.set(false);
                self.wiz_test_ok.set(true);
            }
            WizOp::Unlock { import } => {
                let mode = unlock_mode(import, rs.first().unwrap_or(&Value::Null));
                self.apply_status(&status);
                self.clear_secrets();
                self.wiz_generated.set(String::new());
                let needs = StatusView::parse(&status).needs_identity;
                let next = next_step(WizStep::Passphrase, false, &mode, needs);
                self.go(next, WizStep::Passphrase, &mode);
            }
            WizOp::Devices => {
                let devs = parse_devices(rs.first().unwrap_or(&Value::Null));
                let my = self.device.get();
                let (opts, default) = identity_options(&my, &devs);
                let today = chrono::Local::now().date_naive();
                let v = identity_view(&opts, &default, &my, &devs, today);
                self.id_mode.set(v.default_mode);
                self.id_dev_pick.set(v.default_sub);
                self.id_group_pick.set(v.default_sub);
                self.id_view.set(v);
                self.id_err.set(String::new());
                self.id_loaded.set(true);
            }
            WizOp::Confirm => {
                self.identity_dialog.set(false);
                if self.wizard.get() {
                    self.go(WizStep::Done, WizStep::Identity, "");
                }
            }
        }
    }

    /// 截图核对用：`WIND_RPC_MOCK_SYNC_WIZ=<步骤>` 直接把向导开到某一步。只在 mock 下由
    /// `load_mock_now` 调用（真实构建读不到这个变量），命令当场执行，不走后台线程。
    /// 取值：`storage` `test` `pass` `pass_random` `pass_key` `identity` `identity_restore` `identity_join` `done`
    /// `identity_dialog`。
    pub(super) fn mock_wizard_jump(&self, to: &str) {
        self.inline_rpc.set(true);
        if to == "identity_dialog" {
            self.open_identity_dialog();
        } else {
            self.open_wizard();
            self.wd_password.set("mock-password".into());
            if to != "storage" {
                self.wizard_next();
            }
            if !matches!(to, "storage" | "test") {
                self.wizard_next();
            }
            match to {
                "pass_random" => self.generate_passphrase(),
                "pass_key" => self.wiz_use_key.set(true),
                "identity" | "done" => {
                    self.pass1.set("mock-passphrase".into());
                    self.pass2.set("mock-passphrase".into());
                    self.wizard_next();
                    if to == "done" && self.wiz_step.get() == WizStep::Identity {
                        self.wizard_next();
                    }
                }
                // 展开「恢复为已有设备 / 加入已有组」的子列表（列表形态才有）。
                "identity_restore" | "identity_join" => {
                    self.import_key.set("mock-key".into());
                    self.wiz_use_key.set(true);
                    self.wizard_next();
                    self.id_mode
                        .set(if to == "identity_restore" { 0 } else { 1 });
                }
                _ => {}
            }
        }
        self.inline_rpc.set(false);
    }
}

// ───────────────────────────── 界面 ─────────────────────────────

/// 向导对话框：标题行 → 各步正文（各自滚动、按步骤显隐）→ 按钮栏。
pub(super) fn wizard_dialog(ui: &SyncUi) -> Element {
    let step = ui.wiz_step;
    let page = move |body: Element, s: WizStep| {
        Element::scroll()
            .fill()
            .child(body)
            .visible_when(move || step.get() == s)
    };
    let pages = Element::stack()
        .fill()
        .weight(1.0)
        .child(page(storage_body(ui), WizStep::Storage))
        .child(page(test_body(ui), WizStep::Test))
        .child(page(passphrase_body(ui), WizStep::Passphrase))
        .child(page(identity_step_body(ui), WizStep::Identity))
        .child(page(done_body(ui), WizStep::Done));
    let frame = Element::col()
        .width(DIALOG_W)
        .height(DIALOG_H)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(title_row(step))
        .child(Element::divider())
        .child(pages)
        .child(Element::divider())
        .child(wizard_footer(ui));
    let u = ui.clone();
    Element::dialog_closable(ui.wizard, frame, move |_| u.close_wizard())
}

fn title_row(step: Signal<WizStep>) -> Element {
    let n = step.map(|s| s.number().map_or(String::new(), |n| format!("{n} / 4")));
    Element::row()
        .width_match()
        .cross(Align::Center)
        // 右侧让出对话框自带的关闭按钮，步骤指示才不会压在 × 上。
        .padding_edges(Insets::new(18, 14, 52, 14))
        .child(
            Element::label("设置数据同步")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .weight(1.0)
                .max_lines(1),
        )
        .child(
            Element::label_signal(n)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .max_lines(1),
        )
}

fn body() -> Element {
    Element::col().width_match().spacing(14).padding_xy(20, 16)
}

/// 页头：标题 + 一句说明。
fn head(title: &str, lead: &str) -> Element {
    Element::col()
        .width_match()
        .spacing(4)
        .child(
            Element::label(title)
                .font_size(17.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(1),
        )
        .child(
            Element::label(lead)
                .font_size(13.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3),
        )
}

fn error_line(err: Signal<String>) -> Element {
    Element::label_signal(err)
        .font_size(12.5)
        .fg_role(Role::Danger)
        .width_match()
        .max_lines(4)
        .visible_when(move || !err.get().is_empty())
}

fn storage_body(ui: &SyncUi) -> Element {
    body()
        .child(head(
            "连接存储",
            "同步数据加密后存放在你自己的 WebDAV 或 S3 兼容存储里。",
        ))
        .child(storage_fields(ui))
        .child(error_line(ui.wiz_err))
}

fn test_body(ui: &SyncUi) -> Element {
    let (busy, ok, can_retry) = (ui.busy, ui.wiz_test_ok, ui.wiz_can_retry);
    let u = ui.clone();
    body()
        .child(head(
            "测试连接",
            "保存存储设置，并检查能否连上、存储里有没有同步数据。",
        ))
        .child(muted("正在连接…").visible_when(move || busy.get()))
        .child(
            Element::label_signal(ui.wiz_test_text)
                .font_size(13.0)
                .fg_role(Role::Success)
                .width_match()
                .max_lines(3)
                .visible_when(move || ok.get()),
        )
        .child(error_line(ui.wiz_err))
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(muted("连不上时点「上一步」检查地址与密码，或稍后重试。").weight(1.0))
                .child(
                    btn2("重试")
                        .on_click(move |_| u.retry_test())
                        .visible_when(move || can_retry.get()),
                )
                .visible_when(move || !busy.get() && !ok.get()),
        )
}

fn passphrase_body(ui: &SyncUi) -> Element {
    let remote = ui.wiz_remote_init;
    let use_key = ui.wiz_use_key;
    let (gen_sig, pass1) = (ui.wiz_generated, ui.pass1);
    let pwd = |s: Signal<String>, ph: &str| Element::text_input(s, ph).password().width_match();
    let u = ui.clone();
    let generated = Element::col()
        .width_match()
        .spacing(6)
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(6)
                .child(Element::text_input(gen_sig, "").read_only().weight(1.0))
                .child(btn2("复制").on_click(move |ctx| {
                    ctx.clipboard_set(&gen_sig.get());
                    ctx.toast_ok("已复制口令");
                })),
        )
        .child(tone("请保存好，其他设备加入时要用", Role::Warning))
        // 用户改了口令框，明文就不再是要用的口令，收起。
        .visible_when(move || {
            let g = gen_sig.get();
            !g.is_empty() && g == pass1.get()
        });
    let new_remote = Element::col()
        .width_match()
        .spacing(12)
        .child(head(
            "设置口令",
            "同步数据在本机用口令加密后才上传，存储服务看不到内容。",
        ))
        .child(field(
            "口令",
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(6)
                .child(Element::text_input(ui.pass1, "").password().weight(1.0))
                .child(btn2("随机生成").on_click(move |_| u.generate_passphrase())),
        ))
        .child(generated)
        .child(field("再输入一次", pwd(ui.pass2, "")))
        .child(muted(
            "忘记口令且没有导出密钥时，存储里的数据无法解开。完成后可在「安全」里导出密钥备份。",
        ))
        .visible_when(move || !remote.get());
    let join = Element::col()
        .width_match()
        .spacing(12)
        .child(head(
            "输入口令",
            "存储里已有同步数据。输入创建时设置的口令，或粘贴在其他设备上导出的密钥。",
        ))
        .child(
            Element::col()
                .width_match()
                .spacing(8)
                .child(field("口令", pwd(ui.pass1, "创建同步时设置的口令")))
                .child(
                    Element::link("改用导出的密钥")
                        .font_size(12.5)
                        .on_click(move |_| use_key.set(true)),
                )
                .visible_when(move || !use_key.get()),
        )
        .child(
            Element::col()
                .width_match()
                .spacing(8)
                .child(field(
                    "密钥",
                    pwd(ui.import_key, "粘贴在其他设备上导出的密钥"),
                ))
                .child(
                    Element::link("改用口令")
                        .font_size(12.5)
                        .on_click(move |_| use_key.set(false)),
                )
                .visible_when(move || use_key.get()),
        )
        .visible_when(move || remote.get());
    body()
        .child(new_remote)
        .child(join)
        .child(error_line(ui.wiz_err))
}

fn identity_step_body(ui: &SyncUi) -> Element {
    body()
        .child(head(
            "本机身份",
            "存储里已有设备。选择本机以什么身份加入；确认之前本机不会同步。",
        ))
        .child(identity_body(ui))
}

fn done_body(ui: &SyncUi) -> Element {
    let text = Element::col()
        .width_match()
        .spacing(4)
        .child(
            Element::label("设置完成")
                .font_size(17.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(1),
        )
        .child(
            Element::label_signal(ui.wiz_done)
                .font_size(13.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(4),
        );
    body().child(text)
}

/// 身份选项（向导第 4 步与确认身份对话框共用）。
fn identity_body(ui: &SyncUi) -> Element {
    let (view, mode, dev, group) = (ui.id_view, ui.id_mode, ui.id_dev_pick, ui.id_group_pick);
    let (loaded, busy) = (ui.id_loaded, ui.busy);
    let u = ui.clone();
    let options = Element::switcher(view, move |v: IdentityView| {
        identity_options_el(&v, mode, dev, group)
    });
    Element::col()
        .width_match()
        .spacing(12)
        .child(muted("正在读取设备列表…").visible_when(move || !loaded.get() && busy.get()))
        .child(
            Element::col()
                .width_match()
                .child(options)
                .visible_when(move || loaded.get()),
        )
        .child(error_line(ui.id_err))
        .child(
            Element::link("重新读取")
                .font_size(12.5)
                .on_click(move |_| u.load_identity())
                .visible_when(move || !loaded.get() && !busy.get()),
        )
}

fn identity_options_el(
    v: &IdentityView,
    mode: Signal<usize>,
    dev: Signal<usize>,
    group: Signal<usize>,
) -> Element {
    // 一项 = 单选 + 紧贴的说明（+ 选中时展开的子列表），项与项之间留开。
    let option = |label: &str, m: usize, cap: &str| {
        Element::col()
            .width_match()
            .spacing(2)
            .child(Element::radio(label, mode, m))
            .child(muted(cap).padding_xy(26, 0))
    };
    let sub = |items: &[(String, String)], pick: Signal<usize>, m: usize| {
        let mut c = Element::col()
            .width_match()
            .spacing(6)
            .padding_edges(Insets::new(26, 6, 0, 2));
        for (i, (_, label)) in items.iter().enumerate() {
            c = c.child(Element::radio(label.as_str(), pick, i));
        }
        c.visible_when(move || mode.get() == m)
    };
    let mut col = Element::col().width_match().spacing(12);
    if !v.mine.is_empty() {
        let new_cap = format!("本机改用新的设备号（如 {}-2），单独成组", v.my_id);
        return col
            .child(option(
                &v.mine,
                0,
                "沿用这台设备原来的设备号和所在组，用组内设置替换本机",
            ))
            .child(option("作为新设备", 1, &new_cap));
    }
    if !v.devices.is_empty() {
        col = col.child(
            option(
                "恢复为已有设备",
                0,
                "沿用它的设备号和所在组，用组内设置替换本机",
            )
            .child(sub(&v.devices, dev, 0)),
        );
    }
    if !v.groups.is_empty() {
        col = col
            .child(option("加入已有组", 1, "用组内设置替换本机").child(sub(&v.groups, group, 1)));
    }
    col.child(option("新建独立组", 2, "本机单独成组，保留本机设置"))
}

fn wizard_footer(ui: &SyncUi) -> Element {
    let (step, busy, ok, loaded) = (ui.wiz_step, ui.busy, ui.wiz_test_ok, ui.id_loaded);
    let (u_cancel, u_back, u_next) = (ui.clone(), ui.clone(), ui.clone());
    let primary = step.map(|s| {
        match s {
            WizStep::Identity => "确定",
            WizStep::Done => "完成",
            _ => "下一步",
        }
        .to_string()
    });
    Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 12)
        .spacing(8)
        .child(
            btn2("取消")
                .on_click(move |_| u_cancel.close_wizard())
                .visible_when(move || step.get() != WizStep::Done),
        )
        .child(Element::leaf().weight(1.0))
        .child(
            btn2("上一步")
                .enabled_when(move || !busy.get())
                .on_click(move |_| u_back.wizard_back())
                .visible_when(move || back_step(step.get()).is_some()),
        )
        .child(Element::switcher(primary, move |text: String| {
            let u = u_next.clone();
            btn(&text)
                .enabled_when(move || {
                    !busy.get()
                        && match step.get() {
                            WizStep::Test => ok.get(),
                            WizStep::Identity => loaded.get(),
                            _ => true,
                        }
                })
                .on_click(move |_| u.wizard_next())
        }))
}

/// 状态卡「确认身份…」打开的对话框：内容同向导第 4 步。
pub(super) fn identity_dialog(ui: &SyncUi) -> Element {
    let visible = ui.identity_dialog;
    let (busy, loaded) = (ui.busy, ui.id_loaded);
    let content = Element::col()
        .width_match()
        .padding_xy(18, 14)
        .spacing(12)
        .child(muted(
            "本机已加入同步，但还没确认身份，确认之前不会同步。选择本机以什么身份加入：",
        ))
        .child(identity_body(ui));
    let content = Element::scroll()
        .width_match()
        .max_height(420)
        .child(content);
    let u = ui.clone();
    let footer = Element::row()
        .spacing(8)
        .child(btn2("取消").on_click(move |_| visible.set(false)))
        .child(
            btn("确定")
                .enabled_when(move || !busy.get() && loaded.get())
                .on_click(move |_| u.confirm_identity()),
        );
    Element::dialog_closable(
        visible,
        super::dialogs::dialog_frame_w(520, "确认本机身份", content, footer),
        move |_| visible.set(false),
    )
}

#[cfg(test)]
#[path = "wizard_tests.rs"]
mod tests;
