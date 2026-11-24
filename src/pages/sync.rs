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

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::{Value, json};
use windui::core::EventCtx;
use windui::prelude::*;

use crate::rpc;
use crate::state::AppState;
use crate::widgets::{section, setting_row, setting_row_desc_signal};

/// 搜索锚点（`search::index::HANDWRITTEN` 的 `advanced:sync` 条目指向这里）。
pub const ANCHOR: &str = "advanced:sync";
/// 分区名（二级导航 + 卡片标题）。
pub const SECTION: &str = "数据同步";

// ───────────────────────────── 纯逻辑（可测） ─────────────────────────────

/// 阶段的中文说明。阶段名由 core（`not_installed` / `disabled` / `starting` / `error`）与同步进程
/// （`not_configured` / `needs_setup` / `idle` / `syncing` / `error` / `halted`）定义；
/// 不认识的原样带出，别吞掉。
pub(crate) fn phase_text(phase: &str) -> String {
    match phase {
        "not_installed" => "未安装同步组件".to_string(),
        "disabled" => "未启用".to_string(),
        "not_configured" => "未配置存储后端".to_string(),
        "starting" => "同步进程启动中…".to_string(),
        "needs_setup" => "需要设置口令或导入密钥".to_string(),
        "idle" => "空闲，已是最新".to_string(),
        "syncing" => "同步中…".to_string(),
        "error" => "出错".to_string(),
        "halted" => "已停止，需要处理".to_string(),
        "" => "状态未知".to_string(),
        other => format!("状态：{other}"),
    }
}

/// 状态快照（`sync.status` 的应答 / `sync.statusChanged` 的载荷）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StatusView {
    pub installed: bool,
    pub enabled: bool,
    pub phase: String,
    pub last_ok: Option<i64>,
    pub last_error: Option<String>,
    pub pending: u64,
    pub conflicts: u64,
    /// 冲突副本路径（相对用户目录，新到旧，core 最多给 50 条）；旧版 core 没有这个字段。
    pub conflict_paths: Vec<String>,
    pub has_secret: bool,
    /// 本机已加入同步库（持有有效密钥）：决定口令按钮是「设置」还是「修改」。
    pub joined: bool,
}

impl StatusView {
    pub fn parse(v: &Value) -> Self {
        let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        StatusView {
            installed: b("installed"),
            enabled: b("enabled"),
            phase: v
                .get("phase")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            last_ok: v.get("last_ok").and_then(Value::as_i64),
            last_error: v
                .get("last_error")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(String::from),
            pending: n("pending"),
            conflicts: n("conflicts"),
            conflict_paths: v
                .get("conflict_paths")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|p| p.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            has_secret: b("has_secret"),
            joined: b("joined"),
        }
    }

    /// 「上次成功」行的副标题。
    pub fn last_ok_text(&self) -> String {
        match self
            .last_ok
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        {
            Some(t) => t
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
            None => "尚未同步过".to_string(),
        }
    }

    pub fn pending_text(&self) -> String {
        if self.pending == 0 {
            "没有待上传的改动".to_string()
        } else {
            format!("{} 项改动待上传", self.pending)
        }
    }

    /// 冲突副本提示；没有冲突返回空串。冲突副本是同名文件旁的 `*.conflict-<设备>-<日期>.*`，
    /// 同步进程不替用户挑，要用户自己比对后删掉不要的那份。
    pub fn conflict_text(&self) -> String {
        if self.conflicts == 0 && self.conflict_paths.is_empty() {
            return String::new();
        }
        let n = self.conflicts.max(self.conflict_paths.len() as u64);
        let mut out = format!(
            "有 {n} 个冲突副本：两台设备改了同一个文件，另一份已另存为文件名含 .conflict- 的副本，请比对后手动保留需要的一份并删除另一份"
        );
        for p in self.conflict_paths.iter().take(CONFLICT_SHOWN) {
            out.push_str("\n  ");
            out.push_str(p);
        }
        let rest = (n as usize).saturating_sub(CONFLICT_SHOWN.min(self.conflict_paths.len()));
        if !self.conflict_paths.is_empty() && rest > 0 {
            out.push_str(&format!("\n  …另有 {rest} 个"));
        }
        out
    }

    pub fn error_text(&self) -> String {
        self.last_error.clone().unwrap_or_default()
    }

    pub fn error_kind(&self) -> ErrorKind {
        match &self.last_error {
            Some(e) => classify_error(&self.phase, e),
            None => ErrorKind::None,
        }
    }
}

/// core 同步进程里大批量删除护栏的标记（`runner.rs` 的 `MASS_MARK`）。
const MASS_MARK: &str = "为防误删已暂停";
/// 本地文件正被编辑时本轮跳过的提示后缀（`local.rs`）。
const EDITING_MARK: &str = "正在被编辑，本轮暂不同步";
/// 同步进程正忙时对命令的回绝（`runner.rs` 的 `BUSY_MSG`）。
const BUSY_MSG: &str = "正在同步，请稍后再试";
/// core 对非受信调用方的回绝（`wind-rpc` 的 `DENY_UNTRUSTED`）。
const DENY_MSG: &str = "拒绝：调用方权限不足";

/// `last_error` 在界面上的归类：故障（红）/ 自动重试的提示 / 等用户确认的大批量删除。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorKind {
    None,
    Danger,
    Notice,
    MassDelete,
}

pub(crate) fn classify_error(phase: &str, err: &str) -> ErrorKind {
    if err.is_empty() {
        ErrorKind::None
    } else if phase == "halted" && err.contains(MASS_MARK) {
        ErrorKind::MassDelete
    } else if phase == "error" && err.contains(EDITING_MARK) {
        ErrorKind::Notice
    } else {
        ErrorKind::Danger
    }
}

/// 命令 / 保存失败的文案与是否只是提示（非故障，不用红色）。
pub(crate) fn failure_text(label: &str, e: &str) -> (String, bool) {
    if e.contains(BUSY_MSG) {
        (e.to_string(), true)
    } else if e.contains(DENY_MSG) {
        (
            format!("{label}失败：{e}。请从清风输入法设置程序操作"),
            false,
        )
    } else {
        (format!("{label}失败：{e}"), false)
    }
}

/// 存储位置（后端类型与定位字段）是否变了。core 在位置变化时会清掉已存的密码。
/// 旧位置全空（首次配置）不算：本来就没有已存密码。
pub(crate) fn backend_location_changed(old: &Form, new: &Form) -> bool {
    if old.backend_blank() {
        return false;
    }
    if old.s3 != new.s3 {
        return true;
    }
    if new.s3 {
        (
            &old.s3_endpoint,
            &old.s3_region,
            &old.s3_bucket,
            &old.s3_prefix,
            &old.s3_access_key_id,
        )
            .ne(&(
                &new.s3_endpoint,
                &new.s3_region,
                &new.s3_bucket,
                &new.s3_prefix,
                &new.s3_access_key_id,
            ))
    } else {
        old.wd_url.trim() != new.wd_url.trim() || old.wd_user.trim() != new.wd_user.trim()
    }
}

/// 一步 RPC：方法名 + 参数。
pub(crate) type Step = (&'static str, Value);

/// 保存的执行序列：**先 `sync.setSettings`，再 `set_secret` / `test_connection`**。
/// core 在存储位置变化时会清掉已存密码，顺序反了新密码会被清掉。
pub(crate) fn save_steps(settings: Value, cmds: Vec<(&'static str, Value)>) -> Vec<Step> {
    let mut steps: Vec<Step> = vec![("sync.setSettings", json!({ "settings": settings }))];
    steps.extend(
        cmds.into_iter()
            .map(|(op, args)| ("sync.command", json!({ "op": op, "args": args }))),
    );
    steps
}

/// 「确认大批量删除」的二次确认闸：先 arm（点按钮弹确认框），确认框里 take 才放行；取过即收回。
#[derive(Clone, Default)]
pub(crate) struct MassDeleteGuard(Rc<Cell<bool>>);

impl MassDeleteGuard {
    pub fn arm(&self) {
        self.0.set(true);
    }
    pub fn disarm(&self) {
        self.0.set(false);
    }
    pub fn take(&self) -> bool {
        self.0.replace(false)
    }
}

/// 设置表单的纯数据形态（与信号互转），对应 `wind_sync_ipc::SyncSettings`。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Form {
    pub enabled: bool,
    /// false = WebDAV，true = S3。
    pub s3: bool,
    pub wd_url: String,
    pub wd_user: String,
    pub s3_endpoint: String,
    pub s3_region: String,
    pub s3_bucket: String,
    pub s3_prefix: String,
    pub s3_access_key_id: String,
    pub s3_path_style: bool,
    /// 配置 / 方案 / 主题 / 词库与短语。
    pub scope: [bool; 4],
    pub interval_min: u32,
}

impl Default for Form {
    fn default() -> Self {
        Form {
            enabled: false,
            s3: false,
            wd_url: String::new(),
            wd_user: String::new(),
            s3_endpoint: String::new(),
            s3_region: String::new(),
            s3_bucket: String::new(),
            s3_prefix: String::new(),
            s3_access_key_id: String::new(),
            s3_path_style: false,
            scope: [true; 4],
            interval_min: 15,
        }
    }
}

/// 状态区最多列出几条冲突路径（其余折成「另有 N 个」）。
const CONFLICT_SHOWN: usize = 8;

const SCOPE_KEYS: [&str; 4] = ["config", "schemas", "themes", "dicts"];

impl Form {
    /// 取 `sync.getSettings` 应答里的 `settings`。缺的字段按默认。
    pub fn from_settings(v: &Value) -> Form {
        let s = v.get("settings").unwrap_or(v);
        let mut f = Form {
            enabled: s.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            interval_min: s
                .get("interval_min")
                .and_then(Value::as_u64)
                .map_or(15, |n| n.min(u32::MAX as u64) as u32),
            ..Form::default()
        };
        let str_of = |o: &Value, k: &str| {
            o.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        if let Some(sc) = s.get("scope") {
            for (i, k) in SCOPE_KEYS.iter().enumerate() {
                f.scope[i] = sc.get(*k).and_then(Value::as_bool).unwrap_or(true);
            }
        }
        if let Some(b) = s.get("backend") {
            match b.get("type").and_then(Value::as_str) {
                Some("webdav") => {
                    f.wd_url = str_of(b, "url");
                    f.wd_user = str_of(b, "user");
                }
                Some("s3") => {
                    f.s3 = true;
                    f.s3_endpoint = str_of(b, "endpoint");
                    f.s3_region = str_of(b, "region");
                    f.s3_bucket = str_of(b, "bucket");
                    f.s3_prefix = str_of(b, "prefix");
                    f.s3_access_key_id = str_of(b, "access_key_id");
                    f.s3_path_style = b
                        .get("path_style")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                }
                _ => {}
            }
        }
        f
    }

    /// 选中后端的必填项是否全空：没启用时，全空的半成品不提交后端（core 校验会拒绝空地址）。
    fn backend_blank(&self) -> bool {
        if self.s3 {
            [&self.s3_endpoint, &self.s3_bucket, &self.s3_access_key_id]
                .iter()
                .all(|s| s.trim().is_empty())
        } else {
            self.wd_url.trim().is_empty() && self.wd_user.trim().is_empty()
        }
    }

    /// `sync.setSettings` 的 `settings`。**不含机密**。
    pub fn to_settings(&self) -> Value {
        let mut scope = serde_json::Map::new();
        for (i, k) in SCOPE_KEYS.iter().enumerate() {
            scope.insert((*k).to_string(), json!(self.scope[i]));
        }
        let mut out = json!({
            "enabled": self.enabled,
            "scope": scope,
            "interval_min": self.interval_min,
        });
        if self.enabled || !self.backend_blank() {
            out["backend"] = if self.s3 {
                json!({
                    "type": "s3",
                    "endpoint": self.s3_endpoint.trim(),
                    "region": self.s3_region.trim(),
                    "bucket": self.s3_bucket.trim(),
                    "prefix": self.s3_prefix.trim(),
                    "access_key_id": self.s3_access_key_id.trim(),
                    "path_style": self.s3_path_style,
                })
            } else {
                json!({ "type": "webdav", "url": self.wd_url.trim(), "user": self.wd_user.trim() })
            };
        }
        out
    }
}

/// 把输入框里的间隔数收成合法整数（1 分钟到 24 小时）。
pub(crate) fn clamp_interval(v: f64) -> u32 {
    if !v.is_finite() {
        return 15;
    }
    v.round().clamp(1.0, (24 * 60) as f64) as u32
}

/// 本次要提交的机密命令。**空的不发**（留空 = 不修改，不能把已保存的覆盖成空串）；
/// 同步进程同时只有一个后端，WebDAV 密码与 S3 secret 共用 `password` 字段，取当前选中后端的那个。
pub(crate) fn secret_commands(s3: bool, webdav_password: &str, s3_secret: &str) -> Vec<Value> {
    let value = if s3 { s3_secret } else { webdav_password };
    if value.is_empty() {
        Vec::new()
    } else {
        vec![json!({ "password": value })]
    }
}

/// 机密输入框的占位文案：只反映「本机有没有」，从不带内容。
pub(crate) fn secret_placeholder(has_secret: bool) -> &'static str {
    if has_secret {
        "已保存（留空则不修改）"
    } else {
        "未设置"
    }
}

/// 设口令的两次输入校验。
pub(crate) fn check_passphrase(a: &str, b: &str) -> Result<(), &'static str> {
    if a.is_empty() {
        Err("口令不能为空")
    } else if a != b {
        Err("两次输入的口令不一致")
    } else {
        Ok(())
    }
}

/// 「重置远端」的二次确认闸：只有先 [`arm`](Self::arm)（点「重置远端…」弹出确认）、
/// 再在确认框里 [`take`](Self::take)，才拿得到带 `confirm: true` 的命令参数；取过一次就收回。
#[derive(Clone, Default)]
pub(crate) struct ResetGuard(Rc<Cell<bool>>);

impl ResetGuard {
    pub fn arm(&self) {
        self.0.set(true);
    }
    pub fn disarm(&self) {
        self.0.set(false);
    }
    /// 须已 arm 且新口令合格；口令不合格时**不消耗**确认，留在框里让用户改。
    pub fn take(&self, pass1: &str, pass2: &str) -> Result<Value, String> {
        if !self.0.get() {
            return Err("请先点「重置远端…」".to_string());
        }
        check_passphrase(pass1, pass2).map_err(String::from)?;
        self.0.set(false);
        Ok(json!({ "confirm": true, "passphrase": pass1 }))
    }
}

/// 测试连接回执的提示。
pub(crate) fn connection_text(v: &Value) -> String {
    let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    if !b("remote_initialized") {
        "连接成功，远端尚未初始化，将以本机为准".to_string()
    } else if b("joined") {
        "连接成功，本机已加入".to_string()
    } else {
        "连接成功，远端已有同步数据，设置同一口令或导入密钥即可加入".to_string()
    }
}

/// 一条命令回执翻成界面上要做的事。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Outcome {
    pub ok: bool,
    pub toast: String,
    /// `export_key` 成功时的密钥串。
    pub key: Option<String>,
    /// 失败但只是提示（如「正在同步，请稍后再试」），不用错误样式。
    pub notice: bool,
}

pub(crate) fn outcome(op: &str, r: &Result<Value, String>) -> Outcome {
    let label = match op {
        "sync_now" => "立即同步",
        "test_connection" => "测试连接",
        "set_secret" => "保存凭据",
        "save" => "保存",
        "confirm_mass_delete" => "确认删除",
        "set_passphrase" => "设置口令",
        "import_key" => "导入密钥",
        "export_key" => "导出密钥",
        "reset_remote" => "重置远端",
        other => other,
    };
    match r {
        Err(e) => {
            let (toast, notice) = failure_text(label, e);
            Outcome {
                ok: false,
                toast,
                key: None,
                notice,
            }
        }
        Ok(v) => {
            let key = (op == "export_key")
                .then(|| {
                    v.get("key")
                        .and_then(Value::as_str)
                        .or_else(|| v.as_str())
                        .map(String::from)
                })
                .flatten();
            let toast = match op {
                "sync_now" => "已开始同步".to_string(),
                "test_connection" => connection_text(v),
                "export_key" if key.is_none() => "同步进程没有返回密钥".to_string(),
                "export_key" => "已导出密钥，请妥善保管".to_string(),
                "reset_remote" => "远端已重置，将以本机数据为准重新上传".to_string(),
                "confirm_mass_delete" => "已确认，将把这些删除同步到所有设备".to_string(),
                _ => format!("{label}成功"),
            };
            Outcome {
                ok: !(op == "export_key" && key.is_none()),
                toast,
                key,
                notice: false,
            }
        }
    }
}

// ───────────────────────────── 界面状态 ─────────────────────────────

/// 后台线程送回 UI 线程的结果。
pub enum SyncMsg {
    /// `sync.getSettings` 的应答。
    Settings(Result<Value, String>),
    /// `sync.status` 的应答。
    Status(Result<Value, String>),
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
    scope: [Signal<bool>; 4],
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
            scope: [signal(true), signal(true), signal(true), signal(true)],
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

    /// `sync.statusChanged` 事件（及 `sync.status` 应答）落到界面。
    pub fn apply_status(&self, v: &Value) {
        let s = StatusView::parse(v);
        self.installed.set(s.installed);
        self.has_secret.set(s.has_secret);
        self.joined.set(s.joined);
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
        for (s, v) in self.scope.iter().zip(f.scope) {
            s.set(v);
        }
        self.interval.set(f.interval_min as f64);
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
            scope: [
                self.scope[0].get(),
                self.scope[1].get(),
                self.scope[2].get(),
                self.scope[3].get(),
            ],
            interval_min: clamp_interval(self.interval.get()),
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
        rpc::call_core(
            "sync.setSettings",
            &json!({ "settings": self.form().to_settings() }),
        )
        .map(|_| ())
    }

    /// 「保存」：设置 + 本次填了的机密。`test` 为真则随后测试连接。
    fn save(&self, ctx: &mut EventCtx, test: bool) {
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

// ───────────────────────────── 界面 ─────────────────────────────

pub fn build_section(_app: &Rc<RefCell<AppState>>, state: &AppState) -> Element {
    let ui = state.sync.clone();
    ui.load_once();
    let gate = ui.installed;
    let busy = ui.busy;
    let can = move || gate.get() && !busy.get();

    let note = |text: &str, role: Role| {
        Element::label(text)
            .font_size(12.0)
            .fg_role(role)
            .width_match()
            .max_lines(6)
    };

    // ── 状态 ──
    let (conflicts, error) = (ui.conflicts, ui.error);
    let (notice, mass_text) = (ui.notice, ui.mass_text);
    let u_mass = ui.clone();
    let u = ui.clone();
    let sync_btn = Element::button("立即同步")
        .small()
        .enabled_when(can)
        .on_click(move |_| u.run("sync_now", vec![("sync_now", json!({}))]));
    let status_card = section(
        SECTION,
        vec![
            Element::label(
                "未安装同步组件（wind_input_sync），下面的设置暂不可用。请重新运行安装程序并选择包含数据同步。",
            )
            .font_size(12.5)
            .fg_role(Role::Warning)
            .width_match()
            .max_lines(4)
            .padding_xy(16, 8)
            .visible_when(move || !gate.get()),
            setting_row_desc_signal("同步状态", ui.phase, sync_btn),
            setting_row_desc_signal("上次成功", ui.last_ok, Element::leaf()),
            setting_row_desc_signal("待上传", ui.pending, Element::leaf()),
            Element::label_signal(ui.conflicts)
                .font_size(12.5)
                .fg_role(Role::Warning)
                .width_match()
                .max_lines(12)
                .padding_xy(16, 8)
                .visible_when(move || !conflicts.get().is_empty()),
            Element::label_signal(ui.error)
                .font_size(12.5)
                .fg_role(Role::Danger)
                .width_match()
                .max_lines(4)
                .padding_xy(16, 8)
                .visible_when(move || !error.get().is_empty()),
            Element::label_signal(ui.notice)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(4)
                .padding_xy(16, 8)
                .visible_when(move || !notice.get().is_empty()),
            Element::col()
                .width_match()
                .spacing(6)
                .padding_xy(16, 8)
                .child(
                    Element::label_signal(ui.mass_text)
                        .font_size(12.5)
                        .fg_role(Role::Warning)
                        .width_match()
                        .max_lines(4),
                )
                .child(
                    Element::button("确认这些删除并继续同步")
                        .small()
                        .danger()
                        .enabled_when(can)
                        .on_click(move |_| u_mass.request_mass_delete()),
                )
                .visible_when(move || !mass_text.get().is_empty()),
        ],
    );

    // ── 后端 ──
    let u = ui.clone();
    let on_switch = Element::switch(ui.enabled)
        .enabled_when(move || gate.get())
        .on_switch_change(move |ctx, on| {
            if let Err(e) = u.save_settings() {
                ctx.toast_err(format!("无法{}同步：{e}", if on { "启用" } else { "停用" }));
                u.enabled.set(!on);
            } else {
                u.refresh();
            }
        });
    let is_s3 = {
        let b = ui.backend;
        move || b.get() == 1
    };
    let is_wd = {
        let b = ui.backend;
        move || b.get() == 0
    };
    let input = |s: Signal<String>, ph: &str| {
        Element::text_input(s, ph)
            .width(280)
            .enabled_when(move || gate.get())
    };
    let secret_input = |s: Signal<String>| {
        let has = ui.has_secret;
        Element::switcher(has, move |h| {
            Element::text_input(s, secret_placeholder(h))
                .password()
                .width(280)
                .enabled_when(move || gate.get())
        })
    };
    let (u1, u2) = (ui.clone(), ui.clone());
    let backend_card = section(
        "同步存储",
        vec![
            setting_row(
                "启用同步",
                Some("设置保存在本机，不会随同步上传"),
                on_switch,
            ),
            setting_row(
                "存储类型",
                None,
                Element::segmented(vec!["WebDAV", "S3 兼容"], ui.backend),
            ),
            setting_row(
                "WebDAV 地址",
                None,
                input(ui.wd_url, "https://dav.example.com/dav/"),
            )
            .visible_when(is_wd),
            setting_row("用户名", None, input(ui.wd_user, "")).visible_when(is_wd),
            setting_row("密码", None, secret_input(ui.wd_password)).visible_when(is_wd),
            setting_row(
                "Endpoint",
                None,
                input(ui.s3_endpoint, "https://s3.example.com"),
            )
            .visible_when(is_s3),
            setting_row("Region", None, input(ui.s3_region, "auto")).visible_when(is_s3),
            setting_row("Bucket", None, input(ui.s3_bucket, "")).visible_when(is_s3),
            setting_row(
                "路径前缀",
                Some("可留空"),
                input(ui.s3_prefix, "windinput/"),
            )
            .visible_when(is_s3),
            setting_row("Access Key ID", None, input(ui.s3_access_key_id, "")).visible_when(is_s3),
            setting_row("Secret", None, secret_input(ui.s3_secret)).visible_when(is_s3),
            setting_row(
                "Path-style 寻址",
                Some("MinIO 等自建服务通常需要勾选"),
                Element::checkbox("使用 path-style", ui.s3_path_style)
                    .enabled_when(move || gate.get()),
            )
            .visible_when(is_s3),
            setting_row(
                "保存与测试",
                Some("密码 / Secret 只写不读：保存后输入框清空，界面不会再显示它"),
                Element::row()
                    .cross(Align::Center)
                    .spacing(6)
                    .child(
                        Element::button("保存")
                            .small()
                            .enabled_when(can)
                            .on_click(move |ctx| u1.save(ctx, false)),
                    )
                    .child(
                        Element::button("测试连接")
                            .small()
                            .outline_soft()
                            .enabled_when(can)
                            .on_click(move |ctx| u2.save(ctx, true)),
                    ),
            ),
        ],
    );

    // ── 范围 ──
    let u = ui.clone();
    let names = ["配置", "方案", "主题", "词库与短语"];
    let scope_row = names.iter().zip(ui.scope).fold(
        Element::row().cross(Align::Center).spacing(14),
        |r, (n, s)| r.child(Element::checkbox(*n, s).enabled_when(move || gate.get())),
    );
    let scope_card = section(
        "同步范围",
        vec![
            setting_row("同步内容", None, scope_row),
            setting_row(
                "同步间隔",
                Some("分钟，1 到 1440；本机文件有改动时也会很快同步"),
                Element::number_input(ui.interval, 1.0, 1440.0, 1.0)
                    .enabled_when(move || gate.get()),
            ),
            setting_row(
                "应用范围",
                None,
                Element::button("保存范围")
                    .small()
                    .enabled_when(can)
                    .on_click(move |ctx| match u.save_settings() {
                        Ok(()) => {
                            ctx.toast_ok("已保存同步范围");
                            u.refresh();
                        }
                        Err(e) => ctx.toast_err(format!("保存失败：{e}")),
                    }),
            ),
        ],
    );

    // ── 加密 ──
    let u_pass = ui.clone();
    let u_imp = ui.clone();
    let u_exp = ui.clone();
    let u_copy = ui.clone();
    let u_reset = ui.clone();
    let key_out = ui.exported_key;
    let crypto_card = section(
        "加密与密钥",
        vec![
            note(
                "同步内容在本机加密后才上传，服务端看不到文件内容与路径。口令用于在新设备上解开数据密钥。",
                Role::TextMuted,
            ),
            setting_row(
                "口令",
                Some("未加入时设置口令（远端已有数据则用同一口令加入）；已加入可修改，不影响已导出的密钥"),
                Element::switcher(ui.joined, move |joined| {
                    let u = u_pass.clone();
                    Element::button(if joined {
                        "修改口令…"
                    } else {
                        "设置口令…"
                    })
                    .small()
                    .enabled_when(can)
                    .on_click(move |_| {
                        u.pass_err.set(String::new());
                        u.pass_dialog.set(true);
                    })
                }),
            ),
            setting_row(
                "导入密钥",
                Some("在新设备上粘贴导出的密钥字符串，可免输口令"),
                Element::row()
                    .cross(Align::Center)
                    .spacing(6)
                    .child(
                        Element::text_input(ui.import_key, "粘贴 base64 密钥")
                            .password()
                            .width(200)
                            .enabled_when(move || gate.get()),
                    )
                    .child(
                        Element::button("导入")
                            .small()
                            .outline_soft()
                            .enabled_when({
                                let (c, k) = (can, ui.import_key);
                                move || c() && !k.get().trim().is_empty()
                            })
                            .on_click(move |_| {
                                let key = u_imp.import_key.get().trim().to_string();
                                u_imp.run("import_key", vec![("import_key", json!({ "key": key }))]);
                            }),
                    ),
            ),
            setting_row(
                "导出密钥",
                Some("显示本机的数据密钥，请自行备份"),
                Element::button("导出…")
                    .small()
                    .outline_soft()
                    .enabled_when(can)
                    .on_click(move |_| u_exp.run("export_key", vec![("export_key", json!({}))])),
            ),
            Element::col()
                .width_match()
                .spacing(6)
                .child(
                    Element::label(
                        "请妥善保管这串密钥。忘记口令且没有导出密钥时，数据无法恢复，只能重置远端。",
                    )
                    .font_size(12.5)
                    .fg_role(Role::Warning)
                    .width_match()
                    .max_lines(3),
                )
                .child(
                    Element::row()
                        .width_match()
                        .cross(Align::Center)
                        .spacing(6)
                        .child(
                            Element::text_input(key_out, "")
                                .read_only()
                                .weight(1.0),
                        )
                        .child(Element::button("复制").small().outline_soft().on_click(
                            move |ctx| {
                                ctx.clipboard_set(&u_copy.exported_key.get());
                                ctx.toast_ok("已复制密钥");
                            },
                        )),
                )
                .visible_when(move || !key_out.get().is_empty()),
            setting_row(
                "重置远端",
                Some("清空远端全部同步数据，以本机为准重新上传。其他设备需重新加入"),
                Element::button("重置远端…")
                    .small()
                    .danger()
                    .enabled_when(can)
                    .on_click(move |_| {
                        u_reset.reset_ack.set(false);
                        u_reset.reset_err.set(String::new());
                        u_reset.reset_guard.arm();
                        u_reset.reset_dialog.set(true);
                    }),
            ),
        ],
    );

    ui_dialogs(&ui, state);

    Element::col()
        .width_match()
        .spacing(12)
        .child(status_card)
        .child(backend_card)
        .child(scope_card)
        .child(crypto_card)
}

/// 两个对话框挂到字段注册表的 `dialogs`（同扩展程序卡），随页面生命周期。
fn ui_dialogs(ui: &SyncUi, state: &AppState) {
    state
        .fields
        .dialogs
        .borrow_mut()
        .push(passphrase_dialog(ui));
    state
        .fields
        .dialogs
        .borrow_mut()
        .push(mass_delete_dialog(ui));
    state.fields.dialogs.borrow_mut().push(reset_dialog(ui));
}

fn dialog_frame(title: &str, body: Element, footer: Element) -> Element {
    Element::col()
        .width(460)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(footer),
        )
}

fn passphrase_dialog(ui: &SyncUi) -> Element {
    let visible = ui.pass_dialog;
    let joined = ui.joined;
    let body = Element::col()
        .width_match()
        .padding_xy(18, 14)
        .spacing(8)
        .child(
            Element::label(
                "口令用于加密同步数据。请使用足够长的口令，并牢记；另可用「导出密钥」做备份。",
            )
            .font_size(12.5)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(4),
        )
        .child(
            Element::text_input(ui.old_pass, "旧口令（可选，忘记可留空）")
                .password()
                .width_match()
                .visible_when(move || joined.get()),
        )
        .child(
            Element::text_input(ui.pass1, "口令")
                .password()
                .width_match(),
        )
        .child(
            Element::text_input(ui.pass2, "再输入一次")
                .password()
                .width_match(),
        )
        .child(
            Element::label_signal(ui.pass_err)
                .font_size(12.5)
                .fg_role(Role::Danger)
                .width_match(),
        );
    let (u, busy) = (ui.clone(), ui.busy);
    let footer = Element::row()
        .spacing(8)
        .child(
            Element::button("取消")
                .small()
                .outline_soft()
                .on_click(move |_| visible.set(false)),
        )
        .child(
            Element::button("确定")
                .small()
                .enabled_when(move || !busy.get())
                .on_click(move |_| u.submit_passphrase()),
        );
    Element::dialog(visible, dialog_frame("同步口令", body, footer))
}

fn mass_delete_dialog(ui: &SyncUi) -> Element {
    let visible = ui.mass_dialog;
    let body = Element::col().width_match().padding_xy(18, 14).child(
        Element::label(
            "这些文件在本机看似被删除。确认后，这些删除会同步到所有设备，其他设备上对应的文件也会被删掉。请确认不是误操作。",
        )
        .font_size(12.5)
        .fg_role(Role::Warning)
        .width_match()
        .max_lines(5),
    );
    let (u, busy, g) = (ui.clone(), ui.busy, ui.mass_guard.clone());
    let footer = Element::row()
        .spacing(8)
        .child(
            Element::button("取消")
                .small()
                .outline_soft()
                .on_click(move |_| {
                    g.disarm();
                    visible.set(false);
                }),
        )
        .child(
            Element::button("确认删除并同步")
                .small()
                .danger()
                .enabled_when(move || !busy.get())
                .on_click(move |_| u.confirm_mass_delete()),
        );
    Element::dialog(visible, dialog_frame("确认大批量删除", body, footer))
}

fn reset_dialog(ui: &SyncUi) -> Element {
    let visible = ui.reset_dialog;
    let ack = ui.reset_ack;
    let body = Element::col()
        .width_match()
        .padding_xy(18, 14)
        .spacing(10)
        .child(
            Element::label(
                "这会删除远端存储里全部同步数据（含其他设备上传的内容），并以本机当前数据重新上传。其他设备需要重新加入同步。此操作不可撤销。",
            )
            .font_size(12.5)
            .fg_role(Role::Danger)
            .width_match()
            .max_lines(5),
        )
        .child(
            Element::text_input(ui.reset_pass1, "新口令")
                .password()
                .width_match(),
        )
        .child(
            Element::text_input(ui.reset_pass2, "再输入一次新口令")
                .password()
                .width_match(),
        )
        .child(
            Element::label_signal(ui.reset_err)
                .font_size(12.5)
                .fg_role(Role::Danger)
                .width_match(),
        )
        .child(Element::checkbox("我了解后果，确认重置远端", ack));
    let (u, guard, busy) = (ui.clone(), ui.reset_guard.clone(), ui.busy);
    let footer = Element::row()
        .spacing(8)
        .child(Element::button("取消").small().outline_soft().on_click({
            let (guard, u2) = (guard.clone(), ui.clone());
            move |_| {
                guard.disarm();
                u2.reset_pass1.set(String::new());
                u2.reset_pass2.set(String::new());
                visible.set(false);
            }
        }))
        .child(
            Element::button("重置远端")
                .small()
                .danger()
                .enabled_when(move || ack.get() && !busy.get())
                .on_click(
                    move |_| match guard.take(&u.reset_pass1.get(), &u.reset_pass2.get()) {
                        Ok(args) => {
                            u.reset_err.set(String::new());
                            u.run("reset_remote", vec![("reset_remote", args)]);
                        }
                        Err(e) => u.reset_err.set(e),
                    },
                ),
        );
    Element::dialog(visible, dialog_frame("重置远端", body, footer))
}

#[cfg(test)]
mod tests {
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
    fn mismatched_passphrase_sends_no_command() {
        let ui = SyncUi::new();
        ui.pass1.set("a".into());
        ui.pass2.set("b".into());
        ui.submit_passphrase();
        assert_eq!(ui.pass_err.get(), "两次输入的口令不一致");
        assert!(!ui.busy.get(), "被拦下就不该发命令");
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
    fn secret_inputs_are_cleared_after_save_dispatch() {
        let ui = SyncUi::new();
        ui.wd_password.set("hunter2".into());
        ui.s3_secret.set("sk".into());
        // 无 tx（单测）时命令发不出去，但提交动作本身必须已清掉输入。
        let cmds = secret_commands(false, &ui.wd_password.get(), &ui.s3_secret.get());
        assert_eq!(cmds.len(), 1);
        windui::testing::run_with_ctx(|ctx| {
            // mock 下 setSettings 返回空值即视为成功。
            unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
            ui.save(ctx, false);
        });
        assert_eq!(ui.wd_password.get(), "");
        assert_eq!(ui.s3_secret.get(), "");
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
            connection_text(&json!({"ok": true, "remote_initialized": false}))
                .contains("以本机为准")
        );
        assert!(
            connection_text(&json!({"remote_initialized": true, "joined": false})).contains("加入")
        );
        assert!(
            connection_text(&json!({"remote_initialized": true, "joined": true}))
                .contains("已加入")
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
            scope: [true, false, true, false],
            interval_min: 30,
            ..Form::default()
        };
        let v = s3.to_settings();
        assert_eq!(v["backend"]["type"], "s3");
        assert_eq!(v["backend"]["endpoint"], "https://s3.example.com");
        let back = Form::from_settings(&json!({ "settings": v }));
        assert_eq!(back.s3_endpoint, "https://s3.example.com");
        assert_eq!(back.scope, [true, false, true, false]);
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

    fn status_with(phase: &str, err: &str) -> Value {
        json!({ "installed": true, "phase": phase, "last_error": err })
    }

    const MASS_ERR: &str =
        "本机有 25/25 个文件看似被删除，为防误删已暂停。确认无误请点「继续同步」";

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
        new.scope[0] = false;
        assert!(!backend_location_changed(&old, &new), "范围/间隔不算换位置");
        new.wd_url = "https://b/dav".into();
        assert!(backend_location_changed(&old, &new));
        let first = Form::default();
        assert!(!backend_location_changed(&first, &new), "首次配置无旧密码");
        let mut s3 = old.clone();
        s3.s3 = true;
        s3.s3_bucket = "b".into();
        assert!(backend_location_changed(&old, &s3), "换后端类型");
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
}
