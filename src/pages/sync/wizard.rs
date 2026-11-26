//! 「开始配置…」向导与「确认本机身份」对话框（设计见 `sync-device-groups.md` §4.5、§7）。
//!
//! 四步：存储 → 测试连接 → 同步密钥 → 本机身份，之后是完成页。每步的命令在后台线程依次发，
//! 全部成功才前进，失败就地显示；关掉向导不回滚已完成的步骤（还没确认身份的话，
//! 状态卡会给「确认身份…」，打开的对话框与第 4 步同一份内容）。
//!
//! - 第 2 步：先存设置（`enabled: true`），再 `set_secret`、`test_connection`（顺序见
//!   [`save_steps`]），远端是否已初始化决定第 3 步是「生成同步密钥」还是「粘贴同步密钥」。
//! - 第 3 步：生成同步密钥即建库，core 自动确认身份；显示密钥、勾选「我已保存同步密钥」后直接完成。
//!   粘贴同步密钥加入之后必须确认身份，core 在那之前不同步。
//!
//! 外框照首启设置向导（`src/wizard/view.rs`）：定宽定高，每步一个滚动容器按步骤显隐。

use serde_json::{Value, json};
use windui::prelude::*;

use super::dialogs::{btn, btn2, field, key_input, muted, new_key_block, storage_fields};
use super::*;

const DIALOG_W: i32 = 600;
const DIALOG_H: i32 = 560;

// ───────────────────────────── 纯逻辑 ─────────────────────────────

/// 向导所在的步骤。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WizStep {
    Storage,
    Test,
    Key,
    Identity,
    Done,
}

impl WizStep {
    /// 标题行的「n / 4」；完成页不编号。
    pub fn number(self) -> Option<usize> {
        match self {
            WizStep::Storage => Some(1),
            WizStep::Test => Some(2),
            WizStep::Key => Some(3),
            WizStep::Identity => Some(4),
            WizStep::Done => None,
        }
    }
}

/// 向导、确认身份对话框与更换存储对话框发出的命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizOp {
    /// 保存存储设置并测试连接。
    Test,
    /// 远端为空：生成同步密钥并建库。
    CreateKey,
    /// 远端已有：粘贴同步密钥加入。
    ImportKey,
    /// 更换存储：保存并测试新位置。
    StorageSave,
    /// 更换存储：在新位置建库。
    StorageCreate,
    /// 更换存储：粘贴同步密钥加入新位置的同步库。
    StorageJoin,
    /// 建库时报远端已有同步库：测一次连接，看本机密钥能不能解开它（上次建库其实成功了）。
    /// `storage` 区分更换存储对话框与向导。
    Recover { storage: bool },
    /// 本机密钥能解开：取回同步密钥，补给用户确认保存。
    RecoverExport { storage: bool },
    /// 读设备名册（身份选项）。
    Devices,
    /// 按所选身份发命令。
    Confirm,
}

impl WizOp {
    /// 回执会带回新的同步密钥（或为取回密钥而发）：在途时不许关框。
    pub(super) fn yields_key(self) -> bool {
        matches!(
            self,
            WizOp::CreateKey
                | WizOp::StorageCreate
                | WizOp::Recover { .. }
                | WizOp::RecoverExport { .. }
        )
    }
}

/// 当前步骤成功后去哪一步。`joined` 是测试连接时本机已加入（重新开启同步）；`mode` 是第 3 步的
/// 做法：生成同步密钥为 `"init"`，粘贴加入为 `"import"`；`needs_identity` 取自随后的状态。
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
        WizStep::Test => WizStep::Key,
        WizStep::Key if mode == "init" => WizStep::Done,
        WizStep::Key => WizStep::Identity,
        WizStep::Identity | WizStep::Done => WizStep::Done,
    }
}

/// 「上一步」的去处：同步密钥生效之后就回不去了。
pub(crate) fn back_step(step: WizStep) -> Option<WizStep> {
    match step {
        WizStep::Test | WizStep::Key => Some(WizStep::Storage),
        _ => None,
    }
}

/// 完成页的一句话，按从哪一步结束区分。
pub(crate) fn done_text(from: WizStep, mode: &str) -> &'static str {
    match (from, mode) {
        (WizStep::Key, "init") => {
            "同步已开启。本机是第一台设备，其他设备加入时粘贴刚才的同步密钥即可。"
        }
        (WizStep::Test, _) => "本机此前已加入，同步已重新开启。",
        (WizStep::Identity, _) => "已确认本机身份，同步会在后台开始。",
        _ => "已加入同步库，同步会在后台开始。",
    }
}

/// 第 1 步的校验：要有存储地址；换了存储位置（core 会清掉旧密码）、或此前没有存储参数又没存过机密时要填密码。
/// 已有存储参数时可留空：core 有已存密码就继续，没有则测试连接报「尚未保存存储凭据」。
pub(crate) fn check_wizard_storage(
    saved: &Form,
    new: &Form,
    has_secret: bool,
    secret: &str,
) -> Result<(), &'static str> {
    if new.backend_blank() {
        return Err("请填写存储地址");
    }
    let need = backend_location_changed(saved, new) || (saved.backend_blank() && !has_secret);
    if secret.is_empty() && need {
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

/// 第 3 步：远端为空时生成同步密钥（不看输入框），已有时粘贴同步密钥加入；之后读一次状态。
pub(crate) fn wizard_key_steps(
    remote_initialized: bool,
    key: &str,
) -> Result<Vec<Step>, &'static str> {
    if remote_initialized {
        import_key_steps(key)
    } else {
        Ok(create_key_steps())
    }
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
        self.clear_new_key();
        self.wiz_step.set(WizStep::Storage);
        self.wiz_test_ok.set(false);
        self.wiz_test_text.set(String::new());
        self.wiz_can_retry.set(false);
        *self.wiz_sent.borrow_mut() = None;
        self.wiz_remote_init.set(false);
        self.wiz_joined.set(false);
        self.wiz_needs_identity.set(false);
        self.wiz_err.set(String::new());
        self.wiz_done.set(String::new());
        self.reset_identity();
        self.wizard.set(true);
    }

    /// 关向导（取消、×、ESC、完成）：已完成的步骤不回滚，输入的机密清掉。
    /// 新生成的同步密钥还没勾「我已保存」时不关。
    pub(super) fn close_wizard(&self) {
        if self.key_unacked() {
            return;
        }
        self.wizard.set(false);
        self.clear_secrets();
        self.clear_new_key();
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
            // 已生成同步密钥：勾了「我已保存」才完成。
            WizStep::Key if !self.new_key.get().is_empty() => {
                if self.key_ack.get() {
                    self.go(WizStep::Done, WizStep::Key, "init");
                }
            }
            WizStep::Key => {
                let remote = self.wiz_remote_init.get();
                match wizard_key_steps(remote, &self.import_key.get()) {
                    Err(e) => self.wiz_err.set(e.to_string()),
                    Ok(steps) => {
                        self.wiz_err.set(String::new());
                        let op = if remote {
                            WizOp::ImportKey
                        } else {
                            WizOp::CreateKey
                        };
                        self.spawn_wizard(op, steps);
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

    /// 「上一步」：同步密钥已生成就回不去了。
    pub(super) fn wizard_back(&self) {
        if !self.new_key.get().is_empty() {
            return;
        }
        if let Some(b) = back_step(self.wiz_step.get()) {
            self.import_key.set(String::new());
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

    /// 后台依次发若干步，回执全部带回（测试连接要看结果、建库要拿密钥）。
    pub(super) fn spawn_wizard(&self, op: WizOp, steps: Vec<Step>) {
        if self.busy.get() {
            return;
        }
        if self.inline_rpc.get() {
            self.busy.set(true);
            self.key_pending.set(op.yields_key());
            self.on_wizard(op, run_all(steps));
            return;
        }
        let Some(tx) = self.tx.borrow().clone() else {
            return;
        };
        self.busy.set(true);
        self.key_pending.set(op.yields_key());
        std::thread::spawn(move || {
            let _ = tx.send(SyncMsg::Wizard {
                op,
                result: run_all(steps),
            });
        });
    }

    /// 向导 / 确认身份 / 更换存储的回执。框已关才回来的结果只刷新页面，不再动对话框状态。
    pub(super) fn on_wizard(&self, op: WizOp, result: Result<Vec<Value>, (usize, String)>) {
        self.busy.set(false);
        self.key_pending.set(false);
        if op == WizOp::StorageSave {
            self.adopt_sent_storage(&result);
        }
        let open = match op {
            WizOp::Test | WizOp::CreateKey | WizOp::ImportKey => self.wizard.get(),
            WizOp::Recover { storage } | WizOp::RecoverExport { storage } => {
                if storage {
                    self.storage_dialog.get()
                } else {
                    self.wizard.get()
                }
            }
            WizOp::Devices | WizOp::Confirm => self.wizard.get() || self.identity_dialog.get(),
            WizOp::StorageSave | WizOp::StorageCreate | WizOp::StorageJoin => {
                self.storage_dialog.get()
            }
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

    /// 更换存储的保存回执：设置已写下（全部成功，或失败在设置之后）就把发出的表单当作已保存值，
    /// 免得随后的间隔保存带着旧存储设置。
    fn adopt_sent_storage(&self, result: &Result<Vec<Value>, (usize, String)>) {
        let written = !matches!(result, Err((0, _)));
        let sent = self.storage_sent.borrow_mut().take();
        if let Some(f) = sent.filter(|_| written) {
            self.storage_summary.set(storage_summary(&f));
            *self.saved.borrow_mut() = f;
        }
    }

    fn wizard_failed(&self, op: WizOp, at: usize, e: &str) {
        let label = match op {
            WizOp::Test | WizOp::Recover { .. } => "测试连接",
            WizOp::CreateKey | WizOp::StorageCreate => "生成同步密钥",
            WizOp::ImportKey | WizOp::StorageJoin => "加入同步库",
            WizOp::RecoverExport { .. } => "读取同步密钥",
            WizOp::StorageSave => "保存存储设置",
            WizOp::Devices => "读取设备列表",
            WizOp::Confirm => "确认身份",
        };
        let text = key_error_text(label, e);
        match op {
            WizOp::Test => {
                let secret_step = self.wiz_sent.borrow().as_ref().and_then(|(_, i)| *i);
                self.wiz_test_ok.set(false);
                self.wiz_can_retry.set(can_retry_test(secret_step, at));
                self.wiz_err.set(text);
            }
            // 远端已有同步库：可能是上次建库其实成功了而回执丢了，先核对本机密钥。
            WizOp::CreateKey | WizOp::StorageCreate if e.contains(REMOTE_TAKEN) => {
                let storage = op == WizOp::StorageCreate;
                self.spawn_wizard(
                    WizOp::Recover { storage },
                    vec![(
                        "sync.command",
                        json!({ "op": "test_connection", "args": {} }),
                    )],
                );
            }
            // 远端其实是空的：改成生成形态。
            WizOp::ImportKey | WizOp::StorageJoin if e.contains(REMOTE_EMPTY) => {
                self.key_form(op == WizOp::StorageJoin, false, e);
            }
            // 本机密钥解不开或读不到：只能粘贴加入。
            WizOp::RecoverExport { storage } => self.key_form(storage, true, REMOTE_TAKEN_MSG),
            WizOp::CreateKey | WizOp::ImportKey | WizOp::Recover { storage: false } => {
                self.wiz_err.set(text)
            }
            // 测试连接是最后一步：失败在它上面时设置与密码已经存下。
            WizOp::StorageSave if at > 0 => self
                .storage_err
                .set(format!("设置已保存，但连接新位置失败：{e}")),
            WizOp::StorageSave
            | WizOp::StorageCreate
            | WizOp::StorageJoin
            | WizOp::Recover { storage: true } => self.storage_err.set(text),
            WizOp::Devices | WizOp::Confirm => self.id_err.set(text),
        }
    }

    /// 切到粘贴（`join`）或生成形态，并说明原因；粘贴框清空。
    fn key_form(&self, storage: bool, join: bool, why: &str) {
        self.import_key.set(String::new());
        if storage {
            self.storage_stage.set(if join {
                StorageStage::Join
            } else {
                StorageStage::Create
            });
            self.storage_err.set(why.to_string());
        } else {
            self.wiz_remote_init.set(join);
            self.wiz_err.set(why.to_string());
        }
    }

    /// 显示刚生成（或取回）的同步密钥，等用户勾选已保存。
    fn show_new_key(&self, storage: bool, key: String) {
        self.key_ack.set(false);
        self.new_key.set(key);
        if storage {
            self.storage_err.set(String::new());
            self.storage_stage.set(StorageStage::Created);
        } else {
            self.wiz_err.set(String::new());
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
            WizOp::CreateKey => match rs.first().and_then(key_from_reply) {
                Some(k) => self.show_new_key(false, k),
                // 库已建好但没拿到密钥：直接完成，可在「安全」里查看。
                None => self.go(WizStep::Done, WizStep::Key, "init"),
            },
            WizOp::Recover { storage } => {
                let joined = rs
                    .first()
                    .and_then(|v| v.get("joined"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if joined {
                    self.spawn_wizard(
                        WizOp::RecoverExport { storage },
                        vec![("sync.command", json!({ "op": "export_key", "args": {} }))],
                    );
                } else {
                    self.key_form(storage, true, REMOTE_TAKEN_MSG);
                }
            }
            WizOp::RecoverExport { storage } => match rs.first().and_then(key_from_reply) {
                Some(k) => self.show_new_key(storage, k),
                None => self.key_form(storage, true, REMOTE_TAKEN_MSG),
            },
            WizOp::ImportKey => {
                self.apply_status(&status);
                self.clear_secrets();
                let needs = StatusView::parse(&status).needs_identity;
                let next = next_step(WizStep::Key, false, "import", needs);
                self.go(next, WizStep::Key, "import");
            }
            WizOp::StorageSave => {
                let reply = rs.last().cloned().unwrap_or(Value::Null);
                match storage_stage_after_test(&reply) {
                    Some(stage) => self.storage_stage.set(stage),
                    None => self.close_storage_dialog(),
                }
            }
            WizOp::StorageCreate => match rs.first().and_then(key_from_reply) {
                Some(k) => self.show_new_key(true, k),
                None => self.close_storage_dialog(),
            },
            WizOp::StorageJoin => {
                self.apply_status(&status);
                self.close_storage_dialog();
                if StatusView::parse(&status).needs_identity {
                    self.open_identity_dialog();
                }
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

    /// 截图核对用：`WIND_RPC_MOCK_SYNC_WIZ=<步骤>` 直接把向导或对话框开到某一步。只在 mock 下由
    /// `load_mock_now` 调用（真实构建读不到这个变量），命令当场执行，不走后台线程。
    /// 向导：`storage` `test` `key` `key_created` `identity` `identity_restore` `identity_join` `done`；
    /// 对话框：`identity_dialog` `key_view` `reset` `reset_done` `storage_dialog` `storage_next` `storage_created`。
    /// 远端为空与否由 `WIND_RPC_MOCK_SYNC=fresh` 决定。
    pub(super) fn mock_wizard_jump(&self, to: &str) {
        self.inline_rpc.set(true);
        let mock_cmd = |op: &str, args: Value| {
            rpc::call_core("sync.command", &json!({ "op": op, "args": args }))
                .ok()
                .and_then(|v| key_from_reply(&v))
                .unwrap_or_default()
        };
        match to {
            "identity_dialog" => self.open_identity_dialog(),
            "key_view" => {
                self.export_dialog.set(true);
                self.exported_key.set(mock_cmd("export_key", json!({})));
            }
            "reset" | "reset_done" => {
                self.open_reset_dialog();
                if to == "reset_done" {
                    self.reset_guard.take();
                    self.new_key
                        .set(mock_cmd("reset_remote", json!({ "confirm": true })));
                }
            }
            "storage_dialog" => self.open_storage_dialog(),
            "storage_next" | "storage_created" => {
                self.open_storage_dialog();
                self.wd_password.set("mock-password".into());
                self.save_storage();
                if to == "storage_created" {
                    self.create_storage_library();
                }
            }
            _ => self.mock_wizard_steps(to),
        }
        self.inline_rpc.set(false);
    }

    fn mock_wizard_steps(&self, to: &str) {
        self.open_wizard();
        self.wd_password.set("mock-password".into());
        if to != "storage" {
            self.wizard_next();
        }
        if !matches!(to, "storage" | "test") {
            self.wizard_next();
        }
        if matches!(to, "key" | "storage" | "test") {
            return;
        }
        // 远端已有时贴一串密钥加入，为空时生成。
        self.import_key.set(MOCK_KEY.into());
        self.wizard_next();
        match to {
            "done" => {
                self.key_ack.set(true);
                self.wizard_next();
            }
            // 展开「恢复为已有设备 / 加入已有组」的子列表（列表形态才有）。
            "identity_restore" | "identity_join" => {
                self.id_mode
                    .set(if to == "identity_restore" { 0 } else { 1 });
            }
            _ => {}
        }
    }
}

/// mock 截图里粘贴的同步密钥（与 `rpc` mock 的格式相同：52 个 base32 字符，每 4 个一组）。
const MOCK_KEY: &str = "4cra-v4aq-os7p-s5zw-tw6e-6a5a-mtfw-rhkv-cruv-cppg-6wam-jbo5-4rta";

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
        .child(page(key_body(ui), WizStep::Key))
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

/// 第 3 步：远端为空 → 「生成同步密钥」，生成后显示密钥并要求勾选已保存；远端已有 → 粘贴同步密钥。
fn key_body(ui: &SyncUi) -> Element {
    let (remote, key, busy) = (ui.wiz_remote_init, ui.new_key, ui.busy);
    let u = ui.clone();
    let create = Element::col()
        .width_match()
        .spacing(12)
        .child(head(
            "生成同步密钥",
            "同步数据在本机用同步密钥加密后才上传，存储服务看不到内容。",
        ))
        .child(
            Element::col()
                .width_match()
                .spacing(12)
                .child(muted(
                    "存储里还没有同步库。生成同步密钥后，本机就是第一台设备；其他设备加入时粘贴这串密钥。",
                ))
                .child(
                    Element::row().child(
                        btn("生成同步密钥")
                            .enabled_when(move || !busy.get())
                            .on_click(move |_| u.wizard_next()),
                    ),
                )
                .visible_when(move || key.get().is_empty()),
        )
        .child(new_key_block(ui).visible_when(move || !key.get().is_empty()))
        .visible_when(move || !remote.get());
    let join = Element::col()
        .width_match()
        .spacing(12)
        .child(head(
            "加入同步库",
            "存储里已有同步库。粘贴在其他设备上看到的同步密钥（「安全」→「同步密钥」→「查看…」）。",
        ))
        .child(field("同步密钥", key_input(ui.import_key)))
        .visible_when(move || remote.get());
    body()
        .child(create)
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
                "沿用这台设备原来的设备号和同步组，用组内设置替换本机",
            ))
            .child(option("作为新设备", 1, &new_cap));
    }
    if !v.devices.is_empty() {
        col = col.child(
            option(
                "恢复为已有设备",
                0,
                "沿用它的设备号和同步组，用组内设置替换本机",
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
    let (remote, key, ack) = (ui.wiz_remote_init, ui.new_key, ui.key_ack);
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
                .visible_when(move || step.get() != WizStep::Done && key.get().is_empty()),
        )
        .child(Element::leaf().weight(1.0))
        .child(
            btn2("上一步")
                .enabled_when(move || !busy.get())
                .on_click(move |_| u_back.wizard_back())
                .visible_when(move || back_step(step.get()).is_some() && key.get().is_empty()),
        )
        .child(Element::switcher(primary, move |text: String| {
            let u = u_next.clone();
            btn(&text)
                .enabled_when(move || {
                    !busy.get()
                        && match step.get() {
                            WizStep::Test => ok.get(),
                            // 远端为空：用正文的「生成同步密钥」，生成后勾了「我已保存」才能下一步。
                            WizStep::Key if !remote.get() => !key.get().is_empty() && ack.get(),
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
