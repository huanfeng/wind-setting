//! 「同步」页（设计见 `WindInput/docs/design/sync-device-groups.md` §7、`data-sync.md` §5）。
//!
//! 同步设置是设备本地的，**不进 config.toml**，也不走底栏「应用设置」：本页的读写全部经
//! core 的 `sync.*` RPC，改动由页内按钮或对话框立即提交。
//!
//! - `sync.getSettings` / `sync.setSettings`：后端、间隔、启用开关（不含任何机密）；
//! - `sync.status` + 事件 `sync.statusChanged`：阶段、上次成功、待传数、冲突数、错误、身份；
//! - `sync.now`：立即同步，结果由 core 弹提示；
//! - `sync.command{op, args}`：`test_connection` / `set_secret` / `set_passphrase` /
//!   `import_key` / `export_key` / `reset_remote` 等，经 core 转给同步进程，**同步等回执**。
//!
//! # 机密只写不读
//!
//! WebDAV 密码、S3 secret、口令、导入的密钥都只存在于输入框信号里，提交后立刻清空；
//! 界面上唯一的「是否已有」来源是状态里的 `has_secret`，据此把占位符写成「已保存」。
//! 唯一会回到界面的机密是用户主动点「导出密钥」得到的那串，且只放在只读框里。
//!
//! # 不在 UI 线程上等
//!
//! 命令要等同步进程回执（`set_passphrase` 跑 Argon2，`test_connection` 走网络），全部在后台线程
//! 发，结果经 `App::channel`（[`SyncUi::set_tx`]，`main` 注册）回 UI 线程。
//!
//! # 命令参数约定（与同步进程对齐）
//!
//! | op | args | 回执 |
//! |---|---|---|
//! | `set_secret` | `{password}`（WebDAV 密码或 S3 secret，同时只有一个后端；null 清除） | |
//! | `test_connection` | 可选 `{password}`（未保存的临时密码） | `{ok, remote_initialized, joined}` |
//! | `set_passphrase` | `{passphrase}`；已加入时可带 `old_passphrase` | |
//! | `import_key` | `{key}`（导出的 base64 串） | |
//! | `export_key` | | `{key}` |
//! | `reset_remote` | `{confirm: true, passphrase: <新口令>}` | |

mod dialogs;
pub(crate) mod model;
mod page;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::{Value, json};
use windui::core::EventCtx;
use windui::prelude::*;

use crate::rpc;

pub(crate) use model::*;
pub use page::build_page;

/// 搜索锚点（`search::index::HANDWRITTEN` 的 `sync` 条目指向这里）。
pub const ANCHOR: &str = "sync";

// ───────────────────────────── 界面状态 ─────────────────────────────

/// 后台线程送回 UI 线程的结果。
pub enum SyncMsg {
    /// `sync.getSettings` 的应答。
    Settings(Result<Value, String>),
    /// `sync.status` 的应答。
    Status(Result<Value, String>),
    /// `list_devices` 的回执。
    Devices(Result<Value, String>),
    /// 一条（或一串）命令的最终回执，`op` 供 [`outcome`] 选文案。
    Command {
        op: String,
        result: Result<Value, String>,
    },
}

/// 同步间隔的保存：与其他命令一样占 busy，保证 setSettings 一条接一条。
const OP_INTERVAL: &str = "interval";

#[derive(Clone)]
pub struct SyncUi {
    installed: Signal<bool>,
    /// 已有机密（凭据 / 密钥）。
    has_secret: Signal<bool>,
    /// 已装、已启用、已加入：显示完整的同步页，否则只给「开始配置」。
    configured: Signal<bool>,
    needs_identity: Signal<bool>,
    /// 撞号：另一台设备在用本机的设备号。
    dup_device: Signal<bool>,
    /// 「立即同步」可点（确认身份前、撞号时不可）。
    can_sync: Signal<bool>,
    phase: Signal<String>,
    last_ok: Signal<String>,
    pending: Signal<String>,
    /// 冲突说明全文（含路径），冲突对话框里显示。
    conflicts: Signal<String>,
    /// 「N 个冲突副本」；空 = 无冲突。
    conflict_summary: Signal<String>,
    error: Signal<String>,
    /// 自动重试类提示（非故障）。
    notice: Signal<String>,
    /// 大批量删除护栏的说明；非空即显示确认入口。
    mass_text: Signal<String>,
    mass_dialog: Signal<bool>,
    mass_guard: MassDeleteGuard,
    /// 本机设备号、所在组号与组名。
    device: Signal<String>,
    group: Signal<String>,
    group_name: Signal<String>,
    /// 本机卡「所在组」的值（组名 · N 台设备）。
    group_line: Signal<String>,
    /// 最近一次从 core 读到 / 成功保存的表单：存储摘要、改间隔、停用都以它为准。
    saved: Rc<RefCell<Form>>,
    storage_summary: Signal<String>,
    /// 间隔改了但有写入在途，等它结束再发最新值。
    interval_pending: Rc<Cell<bool>>,
    /// 在途的「更换存储」表单：成功后立即作为已保存值，免得随后的间隔保存带着旧存储设置。
    storage_sent: Rc<RefCell<Option<Form>>>,
    /// 有命令在途：按钮置灰，避免重复点。
    busy: Signal<bool>,

    // 存储表单（更换存储对话框与向导共用）。
    /// 0 = WebDAV，1 = S3。
    backend: Signal<usize>,
    wd_url: Signal<String>,
    wd_user: Signal<String>,
    wd_password: Signal<String>,
    s3_endpoint: Signal<String>,
    s3_region: Signal<String>,
    s3_bucket: Signal<String>,
    s3_prefix: Signal<String>,
    s3_access_key_id: Signal<String>,
    s3_secret: Signal<String>,
    s3_path_style: Signal<bool>,
    interval: Signal<f64>,
    device_name: Signal<String>,

    pass_dialog: Signal<bool>,
    pass1: Signal<String>,
    pass2: Signal<String>,
    pass_err: Signal<String>,
    old_pass: Signal<String>,
    joined: Signal<bool>,
    reset_pass1: Signal<String>,
    reset_pass2: Signal<String>,
    reset_err: Signal<String>,
    import_key: Signal<String>,
    exported_key: Signal<String>,
    export_dialog: Signal<bool>,
    reset_dialog: Signal<bool>,
    reset_ack: Signal<bool>,
    reset_guard: ResetGuard,

    devices: Signal<Vec<DeviceView>>,
    /// 设备区的读取失败说明；空 = 无。
    devices_err: Signal<String>,
    remove_dialog: Signal<bool>,
    /// 确认框里显示的目标设备名。
    remove_name: Signal<String>,
    remove_guard: RemoveGuard,
    devices_loaded: Rc<Cell<bool>>,

    storage_dialog: Signal<bool>,
    /// 更换存储时用导出的密钥代替口令。
    storage_use_key: Signal<bool>,
    storage_err: Signal<String>,
    conflicts_dialog: Signal<bool>,
    disable_dialog: Signal<bool>,

    /// 配置向导（Task 4 重写）。
    wizard: Signal<bool>,
    /// 0 存储 / 1 口令 / 2 内容。
    wiz_step: Signal<usize>,
    /// 0 未测 / 1 远端为空 / 2 远端已有数据。
    wiz_remote: Signal<usize>,
    wiz_use_key: Signal<bool>,
    /// 打开钩子：改号（Task 3）、更换组（Task 3）、确认身份（Task 4）的对话框挂在这些信号上。
    rename_dialog: Signal<bool>,
    group_dialog: Signal<bool>,
    identity_dialog: Signal<bool>,

    loaded: Rc<Cell<bool>>,
    tx: Rc<RefCell<Option<Sender<SyncMsg>>>>,
}

impl Default for SyncUi {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncUi {
    pub fn new() -> Self {
        let f = Form::default();
        let s = || signal(String::new());
        SyncUi {
            installed: signal(true),
            has_secret: signal(false),
            configured: signal(false),
            needs_identity: signal(false),
            dup_device: signal(false),
            can_sync: signal(true),
            phase: signal("读取中…".to_string()),
            last_ok: s(),
            pending: s(),
            conflicts: s(),
            conflict_summary: s(),
            error: s(),
            notice: s(),
            mass_text: s(),
            mass_dialog: signal(false),
            mass_guard: MassDeleteGuard::default(),
            device: s(),
            group: s(),
            group_name: s(),
            group_line: s(),
            saved: Rc::new(RefCell::new(Form::default())),
            storage_summary: signal(storage_summary(&f)),
            interval_pending: Rc::new(Cell::new(false)),
            storage_sent: Rc::new(RefCell::new(None)),
            busy: signal(false),
            backend: signal(0),
            wd_url: s(),
            wd_user: s(),
            wd_password: s(),
            s3_endpoint: s(),
            s3_region: s(),
            s3_bucket: s(),
            s3_prefix: s(),
            s3_access_key_id: s(),
            s3_secret: s(),
            s3_path_style: signal(false),
            interval: signal(f.interval_min as f64),
            device_name: s(),
            pass_dialog: signal(false),
            pass1: s(),
            pass2: s(),
            pass_err: s(),
            old_pass: s(),
            joined: signal(false),
            reset_pass1: s(),
            reset_pass2: s(),
            reset_err: s(),
            import_key: s(),
            exported_key: s(),
            export_dialog: signal(false),
            reset_dialog: signal(false),
            reset_ack: signal(false),
            reset_guard: ResetGuard::default(),
            devices: signal(Vec::new()),
            devices_err: s(),
            remove_dialog: signal(false),
            remove_name: s(),
            remove_guard: RemoveGuard::default(),
            devices_loaded: Rc::new(Cell::new(false)),
            storage_dialog: signal(false),
            storage_use_key: signal(false),
            storage_err: s(),
            conflicts_dialog: signal(false),
            disable_dialog: signal(false),
            wizard: signal(false),
            wiz_step: signal(0),
            wiz_remote: signal(0),
            wiz_use_key: signal(false),
            rename_dialog: signal(false),
            group_dialog: signal(false),
            identity_dialog: signal(false),
            loaded: Rc::new(Cell::new(false)),
            tx: Rc::new(RefCell::new(None)),
        }
    }

    /// 注入后台 → UI 的发送端（`main()` 在 App 构建期调用一次）。
    pub fn set_tx(&self, tx: Sender<SyncMsg>) {
        *self.tx.borrow_mut() = Some(tx);
    }

    /// 进页取一次设置与状态；之后靠 `sync.statusChanged` 事件保持最新。
    fn load_once(&self) {
        if self.loaded.replace(true) {
            return;
        }
        if rpc::is_mock() {
            self.load_mock_now();
        } else {
            self.refresh();
        }
    }

    /// mock 下当场取齐：截图的合成点击发生在首几帧，要落在已加载好的页面上。
    fn load_mock_now(&self) {
        if let Ok(v) = rpc::call_core("sync.getSettings", &json!({})) {
            self.apply_form(&Form::from_settings(&v));
        }
        if let Ok(v) = rpc::call_core("sync.status", &json!({})) {
            self.devices_loaded.set(true);
            self.apply_status(&v);
        }
        if let Ok(v) = rpc::call_core("sync.command", &json!({ "op": "list_devices" })) {
            self.devices.set(parse_devices(&v));
            self.update_group_line();
        }
    }

    fn refresh(&self) {
        let Some(tx) = self.tx.borrow().clone() else {
            return;
        };
        std::thread::spawn(move || {
            let _ = tx.send(SyncMsg::Settings(rpc::call_core(
                "sync.getSettings",
                &json!({}),
            )));
            let _ = tx.send(SyncMsg::Status(rpc::call_core("sync.status", &json!({}))));
        });
    }

    /// 拉一次设备名册（后台线程，不占 busy）。
    fn refresh_devices(&self) {
        let Some(tx) = self.tx.borrow().clone() else {
            return;
        };
        std::thread::spawn(move || {
            let _ = tx.send(SyncMsg::Devices(rpc::call_core(
                "sync.command",
                &json!({ "op": "list_devices", "args": {} }),
            )));
        });
    }

    /// 点行内「移除」：记下目标并弹确认框，此时不发命令。
    fn request_remove(&self, dev: &DeviceView) {
        self.remove_guard.arm(&dev.id);
        self.remove_name.set(dev.title());
        self.remove_dialog.set(true);
    }

    /// 确认框里点确认：已上闸才放行并返回目标设备 id。
    fn take_remove_confirm(&self) -> Option<String> {
        let dev = self.remove_guard.take();
        if dev.is_some() {
            self.remove_dialog.set(false);
        }
        dev
    }

    fn confirm_remove(&self) {
        if let Some(dev) = self.take_remove_confirm() {
            self.run(
                "remove_device",
                vec![("remove_device", json!({ "dev": dev, "confirm": true }))],
            );
        }
    }

    /// `sync.statusChanged` 事件（及 `sync.status` 应答）落到界面。
    pub fn apply_status(&self, v: &Value) {
        let s = StatusView::parse(v);
        self.installed.set(s.installed);
        self.has_secret.set(s.has_secret);
        self.joined.set(s.joined);
        self.configured.set(s.configured());
        self.needs_identity.set(s.needs_identity);
        self.dup_device.set(s.duplicate_device());
        self.can_sync.set(can_sync_now(&s));
        self.device.set(s.device.clone());
        self.group.set(s.group.clone());
        self.group_name.set(s.group_name.clone());
        self.update_group_line();
        if s.installed && s.joined && !self.devices_loaded.replace(true) {
            self.refresh_devices();
        }
        let kind = s.error_kind();
        self.phase.set(status_value(&s));
        self.last_ok.set(s.last_ok_text());
        self.pending.set(s.pending_text());
        self.conflicts.set(s.conflict_text());
        self.conflict_summary.set(s.conflict_summary());
        let text = s.error_text();
        let pick = |k: ErrorKind, t: String| if kind == k { t } else { String::new() };
        self.error.set(pick(ErrorKind::Danger, text.clone()));
        self.notice.set(pick(
            ErrorKind::Notice,
            format!("{text}。会自动重试，无需处理"),
        ));
        self.mass_text.set(pick(ErrorKind::MassDelete, text));
    }

    fn update_group_line(&self) {
        self.group_line.set(group_line(
            &self.group.get(),
            &self.group_name.get(),
            &self.devices.get(),
        ));
    }

    /// 已保存的设置落到界面。存储表单只在没人编辑时跟着回到已保存的样子：命令失败后的
    /// 兜底刷新不能冲掉对话框里填了一半的内容。
    fn apply_form(&self, f: &Form) {
        *self.saved.borrow_mut() = f.clone();
        self.storage_summary.set(storage_summary(f));
        if !self.storage_dialog.get() && !self.wizard.get() {
            self.reset_storage_form();
        }
        self.interval.set(f.interval_min as f64);
        self.device_name.set(f.device_name.clone());
    }

    fn reset_storage_form(&self) {
        let f = self.saved.borrow().clone();
        self.backend.set(usize::from(f.s3));
        self.wd_url.set(f.wd_url);
        self.wd_user.set(f.wd_user);
        self.s3_endpoint.set(f.s3_endpoint);
        self.s3_region.set(f.s3_region);
        self.s3_bucket.set(f.s3_bucket);
        self.s3_prefix.set(f.s3_prefix);
        self.s3_access_key_id.set(f.s3_access_key_id);
        self.s3_path_style.set(f.s3_path_style);
        self.wd_password.set(String::new());
        self.s3_secret.set(String::new());
    }

    /// 机密一提交（或对话框一关）就从界面清掉，不留在信号里。
    fn clear_secrets(&self) {
        self.wd_password.set(String::new());
        self.s3_secret.set(String::new());
        self.pass1.set(String::new());
        self.pass2.set(String::new());
        self.import_key.set(String::new());
    }

    /// 存储表单 + 已保存的其余设置。
    fn storage_form(&self) -> Form {
        Form {
            s3: self.backend.get() == 1,
            wd_url: self.wd_url.get(),
            wd_user: self.wd_user.get(),
            s3_endpoint: self.s3_endpoint.get(),
            s3_region: self.s3_region.get(),
            s3_bucket: self.s3_bucket.get(),
            s3_prefix: self.s3_prefix.get(),
            s3_access_key_id: self.s3_access_key_id.get(),
            s3_path_style: self.s3_path_style.get(),
            ..self.saved.borrow().clone()
        }
    }

    /// 后台回到 UI 线程（channel 回调）。
    pub fn on_msg(&self, ctx: &mut EventCtx, msg: SyncMsg) {
        match msg {
            // 有写入在途时读到的可能是写之前的值；命令结束后的刷新会再带来最新的。
            SyncMsg::Settings(Ok(_)) if self.busy.get() => {}
            SyncMsg::Settings(Ok(v)) => {
                self.installed
                    .set(v.get("installed").and_then(Value::as_bool).unwrap_or(true));
                self.apply_form(&Form::from_settings(&v));
            }
            SyncMsg::Status(Ok(v)) => self.apply_status(&v),
            SyncMsg::Devices(Ok(v)) => {
                self.devices_err.set(String::new());
                self.devices.set(parse_devices(&v));
                self.update_group_line();
            }
            SyncMsg::Devices(Err(e)) => {
                let (t, _) = failure_text("读取设备列表", &e);
                self.devices_err.set(t);
            }
            SyncMsg::Settings(Err(e)) | SyncMsg::Status(Err(e)) => {
                self.phase.set(format!("无法连接输入法服务：{e}"));
            }
            SyncMsg::Command { op, result } => {
                self.busy.set(false);
                let mut sent_storage = if op == "storage" {
                    self.storage_sent.borrow_mut().take()
                } else {
                    None
                };
                let o = outcome(&op, &result);
                match (o.toast.is_empty(), o.ok, o.notice) {
                    (true, ..) => {}
                    (_, true, _) => ctx.toast_ok(o.toast),
                    (_, _, true) => ctx.toast_with(o.toast, ToastKind::Info, 3000),
                    _ => ctx.toast_err(o.toast),
                }
                // 框已关才回来的密钥不留在界面上。
                if let Some(k) = o.key.filter(|_| self.export_dialog.get()) {
                    self.exported_key.set(k);
                }
                if o.ok {
                    match op.as_str() {
                        "set_passphrase" => {
                            self.pass1.set(String::new());
                            self.pass2.set(String::new());
                            self.old_pass.set(String::new());
                            self.pass_dialog.set(false);
                        }
                        "remove_device" => self.refresh_devices(),
                        "reset_remote" => {
                            self.reset_pass1.set(String::new());
                            self.reset_pass2.set(String::new());
                            self.reset_dialog.set(false);
                        }
                        "storage" => {
                            if let Some(f) = sent_storage.take() {
                                self.storage_summary.set(storage_summary(&f));
                                *self.saved.borrow_mut() = f;
                            }
                            self.storage_dialog.set(false);
                        }
                        "disable" => {
                            self.saved.borrow_mut().enabled = false;
                            self.disable_dialog.set(false);
                        }
                        _ => {}
                    }
                } else if op == "export_key" {
                    self.export_dialog.set(false);
                }
                if self.interval_pending.get() {
                    self.save_interval();
                }
                // 命令常带来状态变化（has_secret、阶段），事件之外再拉一次兜底。
                self.refresh();
            }
        }
    }

    /// 后台依次发若干条命令，遇错即停；`op` 是最终回执的名字，回执取最后一条的结果。
    fn run(&self, op: &str, cmds: Vec<(&'static str, Value)>) {
        let steps = cmds
            .into_iter()
            .map(|(name, args)| ("sync.command", json!({ "op": name, "args": args })))
            .collect();
        self.run_steps(op, steps);
    }

    /// 后台依次发若干步 RPC（方法 + 参数），遇错即停；`op` 是最终回执的名字。
    fn run_steps(&self, op: &str, steps: Vec<Step>) {
        if self.busy.get() {
            return;
        }
        let Some(tx) = self.tx.borrow().clone() else {
            return;
        };
        self.busy.set(true);
        let op = op.to_string();
        std::thread::spawn(move || {
            let mut result = Ok(Value::Null);
            for (method, params) in steps {
                result = rpc::call_core(method, &params);
                if result.is_err() {
                    break;
                }
            }
            let _ = tx.send(SyncMsg::Command { op, result });
        });
    }

    fn sync_now(&self) {
        self.run_steps("sync_now", sync_now_steps());
    }

    /// 同步间隔改了即存（成功不提示）；有写入在途就等它结束再发最新值。
    fn save_interval(&self) {
        let step = interval_step(&self.saved.borrow(), self.interval.get(), self.busy.get());
        self.interval_pending.set(step == IntervalStep::Defer);
        if let IntervalStep::Send(f) = step {
            let steps = vec![("sync.setSettings", json!({ "settings": f.to_settings() }))];
            *self.saved.borrow_mut() = f;
            self.run_steps(OP_INTERVAL, steps);
        }
    }

    fn open_storage_dialog(&self) {
        self.reset_storage_form();
        self.clear_secrets();
        self.storage_use_key.set(false);
        self.storage_err.set(String::new());
        self.storage_dialog.set(true);
    }

    fn close_storage_dialog(&self) {
        self.storage_dialog.set(false);
        self.reset_storage_form();
        self.clear_secrets();
    }

    /// 「更换存储」对话框里点保存：先校验，再按 [`storage_change_steps`] 发送；机密随即清空。
    fn save_storage(&self) {
        if self.busy.get() {
            return;
        }
        let new = self.storage_form();
        let secrets = secret_commands(new.s3, &self.wd_password.get(), &self.s3_secret.get());
        let unlock = unlock_command(
            self.storage_use_key.get(),
            &self.pass1.get(),
            &self.import_key.get(),
        );
        match storage_change_steps(&self.saved.borrow(), &new, secrets, unlock) {
            Err(e) => self.storage_err.set(e.to_string()),
            Ok(steps) => {
                self.storage_err.set(String::new());
                self.clear_secrets();
                *self.storage_sent.borrow_mut() = Some(new);
                self.run_steps("storage", steps);
            }
        }
    }

    /// 「导出密钥…」：先开框（显示「正在读取…」），密钥回来再填进去。
    fn open_export(&self) {
        self.exported_key.set(String::new());
        self.export_dialog.set(true);
        self.run("export_key", vec![("export_key", json!({}))]);
    }

    fn close_export(&self) {
        self.export_dialog.set(false);
        self.exported_key.set(String::new());
    }

    fn open_passphrase_dialog(&self) {
        self.pass_err.set(String::new());
        self.pass1.set(String::new());
        self.pass2.set(String::new());
        self.old_pass.set(String::new());
        self.pass_dialog.set(true);
    }

    fn open_reset_dialog(&self) {
        self.reset_ack.set(false);
        self.reset_err.set(String::new());
        self.reset_guard.arm();
        self.reset_dialog.set(true);
    }

    fn confirm_disable(&self) {
        self.run_steps("disable", disable_steps(&self.saved.borrow()));
    }

    /// 「开始配置…」。
    fn open_wizard(&self) {
        self.reset_storage_form();
        self.clear_secrets();
        self.wiz_step.set(0);
        self.wiz_remote.set(0);
        self.wiz_use_key.set(false);
        self.wizard.set(true);
    }

    /// 「给本机改号…」：改号对话框由 Task 3 挂到 `rename_dialog`。
    fn open_rename_dialog(&self) {
        self.rename_dialog.set(true);
    }

    /// 「更换组…」：对话框由 Task 3 挂到 `group_dialog`。
    fn open_group_dialog(&self) {
        self.group_dialog.set(true);
    }

    /// 「确认身份…」：对话框由 Task 4 挂到 `identity_dialog`。
    fn open_identity_dialog(&self) {
        self.identity_dialog.set(true);
    }

    /// 点「确认删除…」：先弹确认框并上闸，真正发命令要在框里再点一次。
    fn request_mass_delete(&self) {
        self.mass_guard.arm();
        self.mass_dialog.set(true);
    }

    /// 确认框里点确认：已上闸才放行（并收回闸、关框）。
    fn take_mass_confirm(&self) -> bool {
        let ok = self.mass_guard.take();
        if ok {
            self.mass_dialog.set(false);
        }
        ok
    }

    fn confirm_mass_delete(&self) {
        if self.take_mass_confirm() {
            self.run(
                "confirm_mass_delete",
                vec![("confirm_mass_delete", json!({}))],
            );
        }
    }

    fn submit_passphrase(&self) {
        match check_passphrase(&self.pass1.get(), &self.pass2.get()) {
            Err(e) => self.pass_err.set(e.to_string()),
            Ok(()) => {
                self.pass_err.set(String::new());
                let mut args = json!({ "passphrase": self.pass1.get() });
                let old = self.old_pass.get();
                if self.joined.get() && !old.is_empty() {
                    args["old_passphrase"] = json!(old);
                }
                self.run("set_passphrase", vec![("set_passphrase", args)]);
            }
        }
    }
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;
