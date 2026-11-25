//! 「高级 → 数据同步」卡（设计见 `WindInput/docs/design/data-sync.md` §3.3、§5、§10 S5）。
//!
//! 同步设置是设备本地的，**不进 config.toml**，也不走底栏「应用设置」：本卡的读写全部经
//! core 的 `sync.*` RPC，改动由卡内按钮立即提交。
//!
//! - `sync.getSettings` / `sync.setSettings`：后端、范围、间隔、启用开关（不含任何机密）；
//! - `sync.status` + 事件 `sync.statusChanged`：阶段、上次成功、待传数、冲突数、错误；
//! - `sync.command{op, args}`：`sync_now` / `test_connection` / `set_secret` / `set_passphrase` /
//!   `import_key` / `export_key` / `reset_remote`，经 core 转给同步进程，**同步等回执**。
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

/// 搜索锚点（`search::index::HANDWRITTEN` 的 `sync:page` 条目指向这里）。
pub const ANCHOR: &str = "sync:page";
/// 分区名（二级导航 + 卡片标题）。
pub const SECTION: &str = "数据同步";

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

#[derive(Clone)]
pub struct SyncUi {
    installed: Signal<bool>,
    /// 已有机密（凭据 / 密钥）。
    has_secret: Signal<bool>,
    phase: Signal<String>,
    last_ok: Signal<String>,
    pending: Signal<String>,
    conflicts: Signal<String>,
    error: Signal<String>,
    /// 自动重试类提示（非故障）。
    notice: Signal<String>,
    /// 大批量删除护栏的说明；非空即显示确认入口。
    mass_text: Signal<String>,
    mass_dialog: Signal<bool>,
    mass_guard: MassDeleteGuard,
    /// 最近一次从 core 读到 / 成功保存的表单，用来判断存储位置是否变了。
    saved: Rc<RefCell<Form>>,
    /// 有命令在途：按钮置灰，避免重复点。
    busy: Signal<bool>,

    enabled: Signal<bool>,
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
    reset_dialog: Signal<bool>,
    reset_ack: Signal<bool>,
    reset_guard: ResetGuard,

    device_name: Signal<String>,
    devices: Signal<Vec<DeviceView>>,
    /// 设备区的读取失败说明；空 = 无。
    devices_err: Signal<String>,
    remove_dialog: Signal<bool>,
    /// 确认框里显示的目标设备名。
    remove_name: Signal<String>,
    remove_guard: RemoveGuard,
    devices_loaded: Rc<Cell<bool>>,

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
        SyncUi {
            installed: signal(true),
            has_secret: signal(false),
            phase: signal("读取中…".to_string()),
            last_ok: signal(String::new()),
            pending: signal(String::new()),
            conflicts: signal(String::new()),
            error: signal(String::new()),
            notice: signal(String::new()),
            mass_text: signal(String::new()),
            mass_dialog: signal(false),
            mass_guard: MassDeleteGuard::default(),
            saved: Rc::new(RefCell::new(Form::default())),
            busy: signal(false),
            enabled: signal(f.enabled),
            backend: signal(0),
            wd_url: signal(String::new()),
            wd_user: signal(String::new()),
            wd_password: signal(String::new()),
            s3_endpoint: signal(String::new()),
            s3_region: signal(String::new()),
            s3_bucket: signal(String::new()),
            s3_prefix: signal(String::new()),
            s3_access_key_id: signal(String::new()),
            s3_secret: signal(String::new()),
            s3_path_style: signal(false),
            interval: signal(f.interval_min as f64),
            pass_dialog: signal(false),
            pass1: signal(String::new()),
            pass2: signal(String::new()),
            pass_err: signal(String::new()),
            old_pass: signal(String::new()),
            joined: signal(false),
            reset_pass1: signal(String::new()),
            reset_pass2: signal(String::new()),
            reset_err: signal(String::new()),
            import_key: signal(String::new()),
            exported_key: signal(String::new()),
            reset_dialog: signal(false),
            reset_ack: signal(false),
            reset_guard: ResetGuard::default(),
            device_name: signal(String::new()),
            devices: signal(Vec::new()),
            devices_err: signal(String::new()),
            remove_dialog: signal(false),
            remove_name: signal(String::new()),
            remove_guard: RemoveGuard::default(),
            devices_loaded: Rc::new(Cell::new(false)),
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
        self.refresh();
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
        if s.installed && s.joined && !self.devices_loaded.replace(true) {
            self.refresh_devices();
        }
        let kind = s.error_kind();
        self.phase.set(if kind == ErrorKind::Notice {
            "稍后自动重试".to_string()
        } else {
            phase_text(&s.phase)
        });
        self.last_ok.set(s.last_ok_text());
        self.pending.set(s.pending_text());
        self.conflicts.set(s.conflict_text());
        let text = s.error_text();
        let pick = |k: ErrorKind, t: String| if kind == k { t } else { String::new() };
        self.error.set(pick(ErrorKind::Danger, text.clone()));
        self.notice.set(pick(
            ErrorKind::Notice,
            format!("{text}。会自动重试，无需处理"),
        ));
        self.mass_text.set(pick(ErrorKind::MassDelete, text));
    }

    fn apply_form(&self, f: &Form) {
        *self.saved.borrow_mut() = f.clone();
        self.enabled.set(f.enabled);
        self.backend.set(usize::from(f.s3));
        self.wd_url.set(f.wd_url.clone());
        self.wd_user.set(f.wd_user.clone());
        self.s3_endpoint.set(f.s3_endpoint.clone());
        self.s3_region.set(f.s3_region.clone());
        self.s3_bucket.set(f.s3_bucket.clone());
        self.s3_prefix.set(f.s3_prefix.clone());
        self.s3_access_key_id.set(f.s3_access_key_id.clone());
        self.s3_path_style.set(f.s3_path_style);
        self.interval.set(f.interval_min as f64);
        self.device_name.set(f.device_name.clone());
    }

    fn form(&self) -> Form {
        Form {
            enabled: self.enabled.get(),
            s3: self.backend.get() == 1,
            wd_url: self.wd_url.get(),
            wd_user: self.wd_user.get(),
            s3_endpoint: self.s3_endpoint.get(),
            s3_region: self.s3_region.get(),
            s3_bucket: self.s3_bucket.get(),
            s3_prefix: self.s3_prefix.get(),
            s3_access_key_id: self.s3_access_key_id.get(),
            s3_path_style: self.s3_path_style.get(),
            interval_min: clamp_interval(self.interval.get()),
            device_name: self.device_name.get(),
        }
    }

    /// 后台回到 UI 线程（channel 回调）。
    pub fn on_msg(&self, ctx: &mut EventCtx, msg: SyncMsg) {
        match msg {
            SyncMsg::Settings(Ok(v)) => {
                self.installed
                    .set(v.get("installed").and_then(Value::as_bool).unwrap_or(true));
                self.apply_form(&Form::from_settings(&v));
            }
            SyncMsg::Status(Ok(v)) => self.apply_status(&v),
            SyncMsg::Devices(Ok(v)) => {
                self.devices_err.set(String::new());
                self.devices.set(parse_devices(&v));
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
                let o = outcome(&op, &result);
                if o.ok {
                    ctx.toast_ok(o.toast);
                } else if o.notice {
                    ctx.toast_with(o.toast, ToastKind::Info, 3000);
                } else {
                    ctx.toast_err(o.toast);
                }
                if let Some(k) = o.key {
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
                        "import_key" => self.import_key.set(String::new()),
                        "remove_device" => self.refresh_devices(),
                        "reset_remote" => {
                            self.reset_pass1.set(String::new());
                            self.reset_pass2.set(String::new());
                            self.reset_dialog.set(false);
                        }
                        _ => {}
                    }
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

    /// 同步提交设置（只是写一个本地文件，很快）。
    fn save_settings(&self) -> Result<(), String> {
        check_device_name(&self.device_name.get())?;
        rpc::call_core(
            "sync.setSettings",
            &json!({ "settings": self.form().to_settings() }),
        )
        .map(|_| ())
    }

    /// 「保存」：设置 + 本次填了的机密。`test` 为真则随后测试连接。
    fn save(&self, ctx: &mut EventCtx, test: bool) {
        if let Err(e) = check_device_name(&self.device_name.get()) {
            ctx.toast_err(e);
            return;
        }
        let f = self.form();
        let moved = backend_location_changed(&self.saved.borrow(), &f);
        let secrets = secret_commands(f.s3, &self.wd_password.get(), &self.s3_secret.get());
        // 机密一提交就从界面清掉，不留在信号里。
        self.wd_password.set(String::new());
        self.s3_secret.set(String::new());
        let cmds: Vec<(&'static str, Value)> = if test {
            // 测试只带临时密码，不落盘：没填就用已保存的。
            let args = secrets.into_iter().next().unwrap_or_else(|| json!({}));
            vec![("test_connection", args)]
        } else {
            secrets.into_iter().map(|a| ("set_secret", a)).collect()
        };
        if cmds.is_empty() {
            // 没有凭据要写：只提交设置（很快，同步做）。
            if let Err(e) = self.save_settings() {
                let (t, notice) = failure_text("保存", &e);
                if notice {
                    ctx.toast_with(t, ToastKind::Info, 3000);
                } else {
                    ctx.toast_err(t);
                }
                return;
            }
            *self.saved.borrow_mut() = f;
            if moved {
                ctx.toast_with("已更换存储位置，请重新填写密码", ToastKind::Info, 5000);
            } else {
                ctx.toast_ok("已保存同步设置");
            }
            self.refresh();
        } else {
            // 先 setSettings 再写密码：位置变化时 core 会清掉已存密码，顺序反了新密码就丢了。
            *self.saved.borrow_mut() = f.clone();
            let steps = save_steps(f.to_settings(), cmds);
            self.run_steps(if test { "test_connection" } else { "save" }, steps);
        }
    }

    /// 点「确认这些删除并继续同步」：先弹确认框并上闸，真正发命令要在框里再点一次。
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

/// 页面级状态（spike 里临时放在这里）。
#[derive(Clone)]
struct PageUi {
    configured: Signal<bool>,
    wiz: Signal<bool>,
    /// 0 存储 / 1 口令 / 2 内容
    step: Signal<usize>,
    /// 向导是「更换存储」（标题与提示不同）
    rewire: Signal<bool>,
    /// 0 未测 / 1 远端为空 / 2 远端已有数据
    remote: Signal<usize>,
    use_key: Signal<bool>,
    rename: Signal<bool>,
    export: Signal<bool>,
    creds: Signal<bool>,
    conflicts: Signal<bool>,
    disable: Signal<bool>,
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;
