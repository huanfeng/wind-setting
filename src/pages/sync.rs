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

/// 搜索锚点（`search::index::HANDWRITTEN` 的 `sync:page` 条目指向这里）。
pub const ANCHOR: &str = "sync:page";
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
    /// 本机设备号。
    pub device: String,
    /// 本机所在组号与组名（组名可空）。
    pub group: String,
    pub group_name: String,
    /// 需要用户确认本机身份（首次加入远端后）。
    pub needs_identity: bool,
}

impl StatusView {
    /// 已装同步组件且已加入远端：此后才显示完整的同步页，否则只给「开始配置」。
    pub fn configured(&self) -> bool {
        self.installed && self.joined
    }

    pub fn parse(v: &Value) -> Self {
        let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        let s = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
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
            device: s("device"),
            group: s("group"),
            group_name: s("group_name"),
            needs_identity: b("needs_identity"),
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
    pub interval_min: u32,
    /// 本机在「我的设备」里显示的名称；空 = 用计算机名。设备本地，不随同步传播。
    pub device_name: String,
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
            interval_min: 15,
            device_name: String::new(),
        }
    }
}

/// 状态区最多列出几条冲突路径（其余折成「另有 N 个」）。
const CONFLICT_SHOWN: usize = 8;

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
        f.device_name = str_of(s, "device_name");
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
        let mut out = json!({
            "enabled": self.enabled,
            "interval_min": self.interval_min,
            "device_name": self.device_name.trim(),
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

/// 设备名称上限（字符数，与 core 对齐）。
pub(crate) const DEVICE_NAME_MAX: usize = 64;

/// 校验并规整本机设备名：去首尾空白，超过 [`DEVICE_NAME_MAX`] 个字拒绝（不静默截断）。
pub(crate) fn check_device_name(name: &str) -> Result<String, String> {
    let n = name.trim();
    if n.chars().count() > DEVICE_NAME_MAX {
        Err(format!("设备名称不能超过 {DEVICE_NAME_MAX} 个字"))
    } else {
        Ok(n.to_string())
    }
}

/// 平台的中文显示名；不认识的原样带出。
pub(crate) fn platform_text(p: &str) -> String {
    match p {
        "windows" => "Windows".to_string(),
        "macos" => "macOS".to_string(),
        "linux" => "Linux".to_string(),
        "android" => "Android".to_string(),
        "ios" => "iOS".to_string(),
        "" => "未知平台".to_string(),
        other => other.to_string(),
    }
}

/// 「最后在线」文案：今天 / 昨天带时分，更早显示日期；`ts <= 0` 视为未知。
pub(crate) fn last_seen_text(ts: i64, today: chrono::NaiveDate) -> String {
    let Some(t) = chrono::DateTime::from_timestamp(ts, 0).filter(|_| ts > 0) else {
        return "最后在线未知".to_string();
    };
    let t = t.with_timezone(&chrono::Local);
    let days = (today - t.date_naive()).num_days();
    match days {
        0 => format!("今天 {}", t.format("%H:%M")),
        1 => format!("昨天 {}", t.format("%H:%M")),
        _ => t.format("%Y-%m-%d").to_string(),
    }
}

/// `sync.command{op:"list_devices"}` 的一项。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DeviceView {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub app_version: String,
    pub last_seen: i64,
    pub is_self: bool,
    /// false = 设备信息解不开（别的密钥写的或已损坏），名称为空。
    pub readable: bool,
    /// 所在组号与组名（旧数据可空）。
    pub group: String,
    pub group_name: String,
}

impl DeviceView {
    pub fn title(&self) -> String {
        if self.readable && !self.name.trim().is_empty() {
            self.name.clone()
        } else {
            "未知设备（无法解密）".to_string()
        }
    }

    /// 副标题：平台 · 版本 · 最后在线；不可读的设备没有平台与版本。
    pub fn detail(&self, today: chrono::NaiveDate) -> String {
        let mut parts = Vec::new();
        if self.readable {
            parts.push(platform_text(&self.platform));
            if !self.app_version.is_empty() {
                parts.push(format!("v{}", self.app_version.trim_start_matches('v')));
            }
        }
        parts.push(last_seen_text(self.last_seen, today));
        parts.join(" · ")
    }

    /// 只有其他设备能移除。
    pub fn removable(&self) -> bool {
        !self.is_self
    }
}

/// 解析 `list_devices` 应答；本机排最前，其余按最后在线从新到旧。
pub(crate) fn parse_devices(v: &Value) -> Vec<DeviceView> {
    let str_of = |o: &Value, k: &str| {
        o.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut out: Vec<DeviceView> = v
        .get("devices")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|d| DeviceView {
                    id: str_of(d, "id"),
                    name: str_of(d, "name"),
                    platform: str_of(d, "platform"),
                    app_version: str_of(d, "app_version"),
                    last_seen: d.get("last_seen").and_then(Value::as_i64).unwrap_or(0),
                    is_self: d.get("is_self").and_then(Value::as_bool).unwrap_or(false),
                    readable: d.get("readable").and_then(Value::as_bool).unwrap_or(true),
                    group: str_of(d, "group"),
                    group_name: str_of(d, "group_name"),
                })
                .filter(|d| !d.id.is_empty())
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| {
        b.is_self
            .cmp(&a.is_self)
            .then(b.last_seen.cmp(&a.last_seen))
    });
    out
}

/// 设备列表里的一个组。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroupView {
    pub id: String,
    /// 组内第一个非空组名；没有就用组号。
    pub name: String,
    pub devices: Vec<DeviceView>,
    pub is_mine: bool,
}

/// 按组归并设备：本组在前，其余组按组内最近活跃从新到旧；组内本机在前，其余同样按最近活跃。
pub(crate) fn group_devices(devs: &[DeviceView], my_group: &str) -> Vec<GroupView> {
    let mut groups: Vec<GroupView> = Vec::new();
    for d in devs {
        let i = match groups.iter().position(|g| g.id == d.group) {
            Some(i) => i,
            None => {
                groups.push(GroupView {
                    id: d.group.clone(),
                    name: String::new(),
                    devices: Vec::new(),
                    is_mine: d.group == my_group,
                });
                groups.len() - 1
            }
        };
        let g = &mut groups[i];
        if g.name.is_empty() && !d.group_name.is_empty() {
            g.name = d.group_name.clone();
        }
        g.devices.push(d.clone());
    }
    let newest = |g: &GroupView| g.devices.iter().map(|d| d.last_seen).max().unwrap_or(0);
    for g in &mut groups {
        if g.name.is_empty() {
            g.name = g.id.clone();
        }
        g.devices.sort_by(|a, b| {
            b.is_self
                .cmp(&a.is_self)
                .then(b.last_seen.cmp(&a.last_seen))
        });
    }
    groups.sort_by(|a, b| b.is_mine.cmp(&a.is_mine).then(newest(b).cmp(&newest(a))));
    groups
}

/// 某组的设备数；不存在的组为 0。
pub(crate) fn member_count(groups: &[GroupView], group: &str) -> usize {
    groups
        .iter()
        .find(|g| g.id == group)
        .map_or(0, |g| g.devices.len())
}

const PASSPHRASE_ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";

/// 随机口令：5 组 × 4 字符，用去掉易混字符的字母表，`-` 连接。
pub(crate) fn random_passphrase(mut rng: impl FnMut() -> u32) -> String {
    let mut out = String::with_capacity(24);
    for i in 0..20 {
        if i > 0 && i % 4 == 0 {
            out.push('-');
        }
        let k = rng() as usize % PASSPHRASE_ALPHABET.len();
        out.push(PASSPHRASE_ALPHABET[k] as char);
    }
    out
}

/// 生产用的随机源；系统随机数不可用时退回时间戳（口令仍由用户可见、可改）。
pub(crate) fn random_passphrase_os() -> String {
    random_passphrase(|| {
        let mut b = [0u8; 4];
        if getrandom::getrandom(&mut b).is_err() {
            return std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
        }
        u32::from_le_bytes(b)
    })
}

/// 确认本机身份时可选的做法。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IdentityChoice {
    /// 恢复为远端已有的某台设备（设备号）。
    Restore(String),
    NewDevice,
    /// 加入已有的组（组号）。
    Join(String),
    NewGroup,
}

/// 设备类别：桌面（windows/macos/linux）与移动（android/ios）分开，
/// 因为配置与主题只在同类设备间同步，恢复/加入到另一类的设备没有意义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlatformClass {
    Desktop,
    Mobile,
    Other,
}

pub(crate) fn platform_class(p: &str) -> PlatformClass {
    match p {
        "windows" | "macos" | "linux" => PlatformClass::Desktop,
        "android" | "ios" => PlatformClass::Mobile,
        _ => PlatformClass::Other,
    }
}

/// 身份步骤的选项与默认项（以本机平台类别过滤）。
pub(crate) fn identity_options(
    my_id: &str,
    devs: &[DeviceView],
) -> (Vec<IdentityChoice>, IdentityChoice) {
    identity_options_for(platform_class(std::env::consts::OS), my_id, devs)
}

/// 远端已有本机设备号（同一台重装）→ 恢复或另作新设备，默认恢复；
/// 否则列出同类设备可恢复、各组可加入、或新建组，默认新建组。
pub(crate) fn identity_options_for(
    class: PlatformClass,
    my_id: &str,
    devs: &[DeviceView],
) -> (Vec<IdentityChoice>, IdentityChoice) {
    if devs.iter().any(|d| d.readable && d.id == my_id) {
        let restore = IdentityChoice::Restore(my_id.to_string());
        return (vec![restore.clone(), IdentityChoice::NewDevice], restore);
    }
    let mut same: Vec<DeviceView> = devs
        .iter()
        .filter(|d| d.readable && platform_class(&d.platform) == class)
        .cloned()
        .collect();
    same.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
    let mut opts: Vec<IdentityChoice> = same
        .iter()
        .map(|d| IdentityChoice::Restore(d.id.clone()))
        .collect();
    opts.extend(
        group_devices(&same, "")
            .into_iter()
            .map(|g| IdentityChoice::Join(g.id)),
    );
    opts.push(IdentityChoice::NewGroup);
    (opts, IdentityChoice::NewGroup)
}

/// 选项对应的 `sync.command` 序列；撞号时 core 自动加后缀。
pub(crate) fn identity_commands(c: &IdentityChoice) -> Vec<(&'static str, Value)> {
    match c {
        IdentityChoice::Restore(id) => vec![("claim_device", json!({ "dev": id }))],
        IdentityChoice::NewDevice | IdentityChoice::NewGroup => {
            vec![("confirm_identity", json!({}))]
        }
        IdentityChoice::Join(g) => vec![
            ("confirm_identity", json!({})),
            ("join_group", json!({ "group": g, "mode": "theirs" })),
        ],
    }
}

/// 移除设备的二次确认闸：点行内「移除」时记下目标并弹框，框里确认才放行；取过即收回。
#[derive(Clone, Default)]
pub(crate) struct RemoveGuard(Rc<RefCell<Option<String>>>);

impl RemoveGuard {
    pub fn arm(&self, dev: &str) {
        *self.0.borrow_mut() = Some(dev.to_string());
    }
    pub fn disarm(&self) {
        self.0.borrow_mut().take();
    }
    pub fn take(&self) -> Option<String> {
        self.0.borrow_mut().take()
    }
}

/// 移除前的确认文案。
pub(crate) const REMOVE_DEVICE_WARN: &str = "移除后该设备的同步记录会从存储中删除。若该设备仍在使用，下次同步时会重新出现；要彻底阻止，请重置远端并更换口令。";

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
        "remove_device" => "移除设备",
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
                "remove_device" => "已移除该设备".to_string(),
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

    // ── 我的设备 ──
    let joined = ui.joined;
    let ready = move || gate.get() && joined.get();
    let (u_name, u_ref) = (ui.clone(), ui.clone());
    let (devices, devices_err) = (ui.devices, ui.devices_err);
    let u_list = ui.clone();
    let devices_card = section(
        "我的设备",
        vec![
            setting_row(
                "本机名称",
                Some("其他设备看到的本机名称；随同步设置保存在本机"),
                Element::row()
                    .cross(Align::Center)
                    .spacing(6)
                    .child(
                        Element::text_input(ui.device_name, "留空使用计算机名")
                            .width(220)
                            .enabled_when(move || gate.get()),
                    )
                    .child(
                        Element::button("保存名称")
                            .small()
                            .enabled_when(can)
                            .on_click(move |ctx| match u_name.save_settings() {
                                Ok(()) => {
                                    ctx.toast_ok("已保存设备名称");
                                    u_name.refresh_devices();
                                }
                                Err(e) => ctx.toast_err(format!("保存失败：{e}")),
                            }),
                    ),
            ),
            setting_row(
                "设备列表",
                Some("已加入同步的设备；移除只清理存储里的记录"),
                Element::button("刷新")
                    .small()
                    .outline_soft()
                    .enabled_when(ready)
                    .on_click(move |_| u_ref.refresh_devices()),
            ),
            note(
                "未加入同步（需先设置口令或导入密钥）时无法查看设备列表。",
                Role::TextMuted,
            )
            .padding_xy(16, 8)
            .visible_when(move || !ready()),
            Element::label_signal(devices_err)
                .font_size(12.5)
                .fg_role(Role::Danger)
                .width_match()
                .max_lines(3)
                .padding_xy(16, 8)
                .visible_when(move || !devices_err.get().is_empty()),
            Element::switcher(devices, move |list: Vec<DeviceView>| {
                let today = chrono::Local::now().date_naive();
                let mut col = Element::col().width_match();
                for d in list {
                    let control = if d.removable() {
                        let (u, d2) = (u_list.clone(), d.clone());
                        Element::button("移除")
                            .small()
                            .danger()
                            .enabled_when(can)
                            .on_click(move |_| u.request_remove(&d2))
                    } else {
                        Element::label("本机")
                            .font_size(12.5)
                            .fg_role(Role::AccentText)
                    };
                    col = col.child(setting_row(&d.title(), Some(&d.detail(today)), control));
                }
                col
            })
            .visible_when(ready),
        ],
    );

    // ── 范围 ──
    let u = ui.clone();
    let scope_card = section(
        "同步范围",
        vec![
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
        .child(devices_card)
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
    state
        .fields
        .dialogs
        .borrow_mut()
        .push(remove_device_dialog(ui));
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

fn remove_device_dialog(ui: &SyncUi) -> Element {
    let visible = ui.remove_dialog;
    let body = Element::col()
        .width_match()
        .padding_xy(18, 14)
        .spacing(8)
        .child(
            Element::label_signal(ui.remove_name.map(|n| format!("移除设备：{n}")))
                .font_size(13.0)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(2),
        )
        .child(
            Element::label(REMOVE_DEVICE_WARN)
                .font_size(12.5)
                .fg_role(Role::Warning)
                .width_match()
                .max_lines(6),
        );
    let (u, busy, g) = (ui.clone(), ui.busy, ui.remove_guard.clone());
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
            Element::button("移除")
                .small()
                .danger()
                .enabled_when(move || !busy.get())
                .on_click(move |_| u.confirm_remove()),
        );
    Element::dialog(visible, dialog_frame("移除设备", body, footer))
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

// ───────────────────────────── 同步页（spike，待定稿） ─────────────────────────────

use crate::widgets::{CardItem, Vis, section_items};

/// 截图用：`WIND_SPIKE_SYNC=unset` 强制显示未设置态；`WIND_SPIKE_DIALOG` 启动即开某个对话框。
fn spike_env(k: &str) -> String {
    std::env::var(k).unwrap_or_default()
}

fn muted(text: &str) -> Element {
    Element::label(text)
        .font_size(12.5)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(5)
}

fn tone(text: &str, role: Role) -> Element {
    Element::label(text)
        .font_size(12.5)
        .fg_role(role)
        .width_match()
        .max_lines(5)
}

fn field(label: &str, control: Element) -> Element {
    Element::col()
        .width_match()
        .spacing(4)
        .child(Element::label(label).font_size(12.5).fg_role(Role::Text))
        .child(control)
}

fn btn(text: &str) -> Element {
    Element::button(text).small()
}

fn btn2(text: &str) -> Element {
    Element::button(text).small().outline_soft()
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

fn step_header(step: Signal<usize>) -> Element {
    let names = ["连接存储", "加密口令", "同步内容"];
    let mut row = Element::row().width_match().cross(Align::Center).spacing(8);
    for (i, n) in names.iter().enumerate() {
        if i > 0 {
            row = row.child(Element::leaf().width(24).height(1).bg_role(Role::Divider));
        }
        let cur = move || step.get() == i;
        let done = move || step.get() > i;
        let dot = |role: Role, txt: String| {
            Element::stack()
                .size(20, 20)
                .corner(10.0)
                .bg_role(role)
                .child(
                    Element::label(txt)
                        .font_size(11.5)
                        .font_weight(600)
                        .fg_role(Role::OnAccent)
                        .align(Align::Center),
                )
        };
        row = row
            .child(dot(Role::Accent, format!("{}", i + 1)).visible_when(move || cur() || done()))
            .child(
                dot(Role::TextSubtle, format!("{}", i + 1)).visible_when(move || !cur() && !done()),
            )
            .child(
                Element::label(*n)
                    .font_size(12.5)
                    .font_weight(600)
                    .fg_role(Role::Text)
                    .visible_when(cur),
            )
            .child(
                Element::label(*n)
                    .font_size(12.5)
                    .fg_role(Role::TextMuted)
                    .visible_when(move || !cur()),
            );
    }
    row
}

fn wizard_dialog(ui: &SyncUi, p: &PageUi) -> Element {
    let (step, remote, use_key, rewire) = (p.step, p.remote, p.use_key, p.rewire);
    let is_wd = {
        let b = ui.backend;
        move || b.get() == 0
    };
    let is_s3 = {
        let b = ui.backend;
        move || b.get() == 1
    };
    let inp = |s: Signal<String>, ph: &str| Element::text_input(s, ph).width_match();
    let pwd = |s: Signal<String>, ph: &str| Element::text_input(s, ph).password().width_match();
    let two = |a: Element, b: Element| {
        Element::row()
            .width_match()
            .spacing(12)
            .child(a.weight(1.0))
            .child(b.weight(1.0))
    };

    // ── 第 1 步：存储 ──
    let s1 = Element::col()
        .width_match()
        .spacing(12)
        .child(
            tone(
                "更换存储位置相当于重新设置：本机会离开当前同步库，按下面的新位置重新连接和加入。原存储里的数据不会被删除。",
                Role::Warning,
            )
            .visible_when(move || rewire.get()),
        )
        .child(field(
            "存储类型",
            Element::segmented(vec!["WebDAV", "S3 兼容存储"], ui.backend),
        ))
        .child(
            Element::col()
                .width_match()
                .spacing(12)
                .child(field("WebDAV 地址", inp(ui.wd_url, "https://dav.example.com/dav/")))
                .child(muted("坚果云等服务请填写其「第三方应用」里给出的地址，并使用应用专用密码。"))
                .child(two(
                    field("用户名", inp(ui.wd_user, "")),
                    field("密码", pwd(ui.wd_password, "")),
                ))
                .visible_when(is_wd),
        )
        .child(
            Element::col()
                .width_match()
                .spacing(12)
                .child(field("Endpoint", inp(ui.s3_endpoint, "https://s3.example.com")))
                .child(two(
                    field("Bucket", inp(ui.s3_bucket, "")),
                    field("Region", inp(ui.s3_region, "auto")),
                ))
                .child(two(
                    field("Access Key ID", inp(ui.s3_access_key_id, "")),
                    field("Secret", pwd(ui.s3_secret, "")),
                ))
                .child(two(
                    field("路径前缀（可留空）", inp(ui.s3_prefix, "windinput/")),
                    field(
                        "寻址方式",
                        Element::checkbox("Path-style（MinIO 等自建服务）", ui.s3_path_style),
                    ),
                ))
                .visible_when(is_s3),
        );

    // ── 第 2 步：口令 ──
    let s2_new = Element::col()
        .width_match()
        .spacing(12)
        .child(
            Element::row()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::badge_intent("连接成功", Intent::Success))
                .child(muted("这是一个新的同步库，本机将作为第一台设备。")),
        )
        .child(muted(
            "设置一个口令。同步数据在本机加密后才上传，存储服务看不到内容；其他设备加入时需要输入同一个口令。",
        ))
        .child(field("口令", pwd(ui.pass1, "至少 8 个字符")))
        .child(field("再输入一次", pwd(ui.pass2, "")))
        .child(tone(
            "请牢记口令。忘记口令且没有备份密钥时，存储里的数据无法解开。设置完成后可在「存储与安全」里导出密钥备份。",
            Role::Warning,
        ))
        .visible_when(move || remote.get() != 2);
    let s2_join = Element::col()
        .width_match()
        .spacing(12)
        .child(
            Element::row()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::badge_intent("连接成功", Intent::Success))
                .child(muted("存储里已有同步数据，可以把本机加入进去。")),
        )
        .child(
            Element::col()
                .width_match()
                .spacing(12)
                .child(field("口令", pwd(ui.pass1, "创建同步库时设置的口令")))
                .child(
                    Element::link("没有口令？改用导出的密钥加入")
                        .font_size(12.5)
                        .on_click(move |_| use_key.set(true)),
                )
                .visible_when(move || !use_key.get()),
        )
        .child(
            Element::col()
                .width_match()
                .spacing(12)
                .child(field(
                    "密钥",
                    pwd(ui.import_key, "粘贴在其他设备上导出的密钥"),
                ))
                .child(
                    Element::link("改用口令加入")
                        .font_size(12.5)
                        .on_click(move |_| use_key.set(false)),
                )
                .visible_when(move || use_key.get()),
        )
        .child(muted(
            "加入后，本机与存储里的数据会合并；不会删除任何一边已有的词。",
        ))
        .visible_when(move || remote.get() == 2);
    let s2 = Element::col().width_match().child(s2_new).child(s2_join);

    // ── 第 3 步：内容 ──
    let s3 = Element::col()
        .width_match()
        .spacing(12)
        .child(field(
            "本机名称",
            inp(ui.device_name, "其他设备上看到的名字"),
        ))
        .child(field(
            "自动同步间隔（分钟）",
            Element::number_input(ui.interval, 1.0, 1440.0, 1.0),
        ))
        .child(muted("本机文件有改动时会很快同步，不必等到间隔。"));

    let body = Element::col()
        .width_match()
        .padding_xy(20, 16)
        .spacing(16)
        .child(step_header(step))
        .child(Element::divider())
        .child(s1.visible_when(move || step.get() == 0))
        .child(s2.visible_when(move || step.get() == 1))
        .child(s3.visible_when(move || step.get() == 2));

    let wiz = p.wiz;
    let cfg = p.configured;
    let footer = Element::row()
        .spacing(8)
        .child(btn2("取消").on_click(move |_| wiz.set(false)))
        .child(
            btn2("上一步")
                .visible_when(move || step.get() > 0)
                .on_click(move |_| step.set(step.get().saturating_sub(1))),
        )
        .child(
            btn("测试连接并继续")
                .visible_when(move || step.get() == 0)
                .on_click(move |_| {
                    step.set(1);
                }),
        )
        .child(
            btn("下一步")
                .visible_when(move || step.get() == 1)
                .on_click(move |_| step.set(2)),
        )
        .child(
            btn("完成并开始同步")
                .visible_when(move || step.get() == 2)
                .on_click(move |_| {
                    wiz.set(false);
                    cfg.set(true);
                }),
        );
    let title = rewire.map(|r| {
        if *r {
            "更换存储".to_string()
        } else {
            "设置数据同步".to_string()
        }
    });
    let frame = Element::col()
        .width(560)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label_signal(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(20, 14),
        )
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .child(Element::leaf().weight(1.0))
                .child(footer),
        );
    Element::dialog(wiz, frame)
}

fn simple_dialog(visible: Signal<bool>, title: &str, body: Element, ok: Element) -> Element {
    let footer = Element::row()
        .spacing(8)
        .child(btn2("取消").on_click(move |_| visible.set(false)))
        .child(ok);
    Element::dialog(
        visible,
        dialog_frame(title, body.padding_xy(18, 14), footer),
    )
}

fn spike_dialogs(ui: &SyncUi, p: &PageUi) -> Vec<Element> {
    let rename = p.rename;
    let rename_d = simple_dialog(
        rename,
        "本机名称",
        Element::col()
            .width_match()
            .spacing(8)
            .child(muted(
                "其他设备的「我的设备」里显示这个名字。只保存在本机，不随同步传播。",
            ))
            .child(Element::text_input(ui.device_name, "留空使用计算机名").width_match()),
        btn("保存").on_click(move |_| rename.set(false)),
    );
    let export = p.export;
    let key = ui.exported_key;
    let export_d = Element::dialog(
        export,
        dialog_frame(
            "导出密钥",
            Element::col()
                .width_match()
                .padding_xy(18, 14)
                .spacing(10)
                .child(muted(
                    "这串密钥可以直接解开同步数据，作用等同口令。可用来在新设备上免输口令加入，或在忘记口令时找回数据。",
                ))
                .child(
                    Element::row()
                        .width_match()
                        .cross(Align::Center)
                        .spacing(6)
                        .child(Element::text_input(key, "").read_only().weight(1.0))
                        .child(btn2("复制").on_click(move |ctx| {
                            ctx.clipboard_set(&key.get());
                            ctx.toast_ok("已复制密钥");
                        })),
                )
                .child(tone(
                    "请离线保存（如密码管理器），不要发到聊天或网盘。任何拿到它的人都能读取你的同步数据。",
                    Role::Warning,
                )),
            btn("完成").on_click(move |_| export.set(false)),
        ),
    );
    let creds = p.creds;
    let creds_d = simple_dialog(
        creds,
        "更新存储密码",
        Element::col()
            .width_match()
            .spacing(8)
            .child(muted(
                "存储服务那边改了密码或 Secret 时，在这里填入新的。保存前会先测试连接。",
            ))
            .child(
                Element::text_input(ui.wd_password, "新密码")
                    .password()
                    .width_match(),
            ),
        btn("测试并保存").on_click(move |_| creds.set(false)),
    );
    let conflicts = p.conflicts;
    let paths = [
        "themes/dark.conflict-laptop-2026-11-23.json",
        "data/phrases.dict.yaml.conflict-laptop-2026-11-22",
    ];
    let mut list = Element::col().width_match().spacing(6);
    for x in paths {
        list = list.child(
            Element::label(x)
                .font_size(12.5)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(10, 6)
                .bg_role(Role::SurfaceAlt)
                .corner(6.0),
        );
    }
    let conflicts_d = Element::dialog(
        conflicts,
        dialog_frame(
            "冲突副本",
            Element::col()
                .width_match()
                .padding_xy(18, 14)
                .spacing(10)
                .child(muted(
                    "两台设备同时改了同一个文件。本机版本保留原名，另一台的版本另存为下面的副本。请比对后保留需要的内容，并删除副本。",
                ))
                .child(list),
            Element::row()
                .spacing(8)
                .child(btn2("打开用户目录"))
                .child(btn("关闭").on_click(move |_| conflicts.set(false))),
        ),
    );
    let disable = p.disable;
    let disable_d = simple_dialog(
        disable,
        "停用同步",
        Element::col().width_match().spacing(8).child(muted(
            "本机停止同步。存储设置、口令和存储里的数据都会保留，之后可以随时重新启用。",
        )),
        btn("停用").on_click(move |_| disable.set(false)),
    );
    vec![
        wizard_dialog(ui, p),
        rename_d,
        export_d,
        creds_d,
        conflicts_d,
        disable_d,
    ]
}

/// 同步页的卡片：`(二级导航名, 卡片)`。
pub fn build_page(
    _app: &Rc<RefCell<AppState>>,
    state: &AppState,
) -> Vec<(Option<&'static str>, Element)> {
    let ui = state.sync.clone();
    ui.load_once();
    let unset = spike_env("WIND_SPIKE_SYNC") == "unset";
    let p = PageUi {
        configured: signal(!unset),
        wiz: signal(false),
        step: signal(0),
        rewire: signal(false),
        remote: signal(0),
        use_key: signal(false),
        rename: signal(false),
        export: signal(false),
        creds: signal(false),
        conflicts: signal(false),
        disable: signal(false),
    };
    match spike_env("WIND_SPIKE_DIALOG").as_str() {
        "wizard1" => {
            p.wiz.set(true);
            ui.wd_url.set(String::new());
            ui.wd_user.set(String::new());
            ui.device_name.set("书房台式机".into());
        }
        "wizard1s3" => {
            p.wiz.set(true);
            ui.backend.set(1);
        }
        "wizard2new" => {
            p.wiz.set(true);
            p.step.set(1);
            p.remote.set(1);
        }
        "wizard2join" => {
            p.wiz.set(true);
            p.step.set(1);
            p.remote.set(2);
        }
        "wizard2key" => {
            p.wiz.set(true);
            p.step.set(1);
            p.remote.set(2);
            p.use_key.set(true);
        }
        "wizard3" => {
            p.wiz.set(true);
            p.step.set(2);
            ui.device_name.set("书房台式机".into());
        }
        "rewire" => {
            p.wiz.set(true);
            p.rewire.set(true);
        }
        "rename" => p.rename.set(true),
        "export" => {
            ui.exported_key
                .set("bW9jay1kYXRhLWVuY3J5cHRpb24ta2V5LTAxMjM0NTY3ODk=".into());
            p.export.set(true);
        }
        "creds" => p.creds.set(true),
        "conflicts" => p.conflicts.set(true),
        "disable" => p.disable.set(true),
        _ => {}
    }
    for d in spike_dialogs(&ui, &p) {
        state.fields.dialogs.borrow_mut().push(d);
    }

    let cfg = p.configured;
    let busy = ui.busy;
    let can = move || !busy.get();

    // ── 未设置：引导卡 ──
    let (wiz, step, rewire) = (p.wiz, p.step, p.rewire);
    let hero = section_items(
        "数据同步",
        vec![CardItem::Bare(
            Element::col()
                .width_match()
                .padding_xy(20, 18)
                .spacing(10)
                .child(
                    Element::label("把词库、短语、方案和设置同步到你的其他设备")
                        .font_size(15.0)
                        .font_weight(600)
                        .fg_role(Role::Text)
                        .width_match(),
                )
                .child(muted(
                    "使用你自己的 WebDAV（如坚果云、Nextcloud）或 S3 兼容存储（如 Cloudflare R2、MinIO）。数据在本机加密后才上传，存储服务看不到内容。",
                ))
                .child(muted("已经在其他设备上设置过？用同一个存储和口令即可加入。"))
                .child(
                    Element::row().padding_xy(0, 6).child(btn("开始设置…").on_click(move |_| {
                        rewire.set(false);
                        step.set(0);
                        wiz.set(true);
                    })),
                ),
        )],
    )
    .visible_when(move || !cfg.get());

    // ── 已设置：状态 ──
    let u = ui.clone();
    let phase = ui.phase;
    let last_ok = ui.last_ok;
    let status_desc = last_ok.map(|t| format!("上次成功 {t}"));
    let conflicts_on: Vis = Rc::new({
        let c = ui.conflicts;
        move || !c.get().is_empty()
    });
    let err_on: Vis = Rc::new({
        let e = ui.error;
        move || !e.get().is_empty()
    });
    let mass_on: Vis = Rc::new({
        let m = ui.mass_text;
        move || !m.get().is_empty()
    });
    let pc = p.conflicts;
    let u_mass = ui.clone();
    let status = section_items(
        "同步状态",
        vec![
            CardItem::Row(
                Element::row()
                    .width_match()
                    .cross(Align::Center)
                    .spacing(12)
                    .padding_xy(0, 10)
                    .child(
                        Element::col()
                            .weight(1.0)
                            .spacing(3)
                            .child(
                                Element::label_signal(phase)
                                    .font_size(14.0)
                                    .font_weight(600)
                                    .fg_role(Role::Text),
                            )
                            .child(
                                Element::label_signal(status_desc)
                                    .font_size(12.0)
                                    .fg_role(Role::TextMuted),
                            ),
                    )
                    .child(
                        btn("立即同步")
                            .enabled_when(can)
                            .on_click(move |_| u.run("sync_now", vec![("sync_now", json!({}))])),
                    ),
            ),
            CardItem::RowWhen(
                setting_row(
                    "有 2 个冲突副本",
                    Some("两台设备同时改了同一个文件，需要你来决定保留哪份"),
                    btn2("查看…").on_click(move |_| pc.set(true)),
                ),
                conflicts_on,
            ),
            CardItem::RowWhen(
                Element::label_signal(ui.error)
                    .font_size(12.5)
                    .fg_role(Role::Danger)
                    .width_match()
                    .max_lines(4),
                err_on,
            ),
            CardItem::RowWhen(
                setting_row_desc_signal(
                    "同步已暂停",
                    ui.mass_text,
                    Element::button("确认删除…")
                        .small()
                        .danger()
                        .on_click(move |_| u_mass.request_mass_delete()),
                ),
                mass_on,
            ),
        ],
    )
    .visible_when(move || cfg.get());

    // ── 我的设备 ──
    let (rename, u_list) = (p.rename, ui.clone());
    let devices = section_items(
        "我的设备",
        vec![CardItem::Bare(Element::switcher(
            ui.devices,
            move |list: Vec<DeviceView>| {
                let today = chrono::Local::now().date_naive();
                let mut col = Element::col().width_match();
                for (i, d) in list.into_iter().enumerate() {
                    if i > 0 {
                        col = col.child(crate::widgets::row_divider());
                    }
                    let title = Element::row()
                        .cross(Align::Center)
                        .spacing(8)
                        .child(
                            Element::label(d.title())
                                .font_size(13.5)
                                .fg_role(Role::Text),
                        )
                        .child(Element::badge("本机").visible_when({
                            let s = d.is_self;
                            move || s
                        }));
                    let left = Element::col().weight(1.0).spacing(3).child(title).child(
                        Element::label(d.detail(today))
                            .font_size(12.0)
                            .fg_role(Role::TextMuted),
                    );
                    let control = if d.is_self {
                        btn2("改名…").on_click(move |_| rename.set(true))
                    } else {
                        let (u, d2) = (u_list.clone(), d.clone());
                        btn2("移除…").on_click(move |_| u.request_remove(&d2))
                    };
                    col = col.child(crate::widgets::card_pad(
                        Element::row()
                            .width_match()
                            .cross(Align::Center)
                            .padding_xy(0, 10)
                            .child(left)
                            .child(control),
                    ));
                }
                col
            },
        ))],
    )
    .visible_when(move || cfg.get());

    // ── 同步选项（改了即存） ──
    let options = section(
        "同步选项",
        vec![setting_row(
            "自动同步间隔",
            Some("分钟；本机文件有改动时会很快同步"),
            Element::number_input(ui.interval, 1.0, 1440.0, 1.0),
        )],
    )
    .visible_when(move || cfg.get());

    // ── 存储与安全 ──
    let (wd_url, wd_user) = (ui.wd_url, ui.wd_user);
    let _ = wd_user;
    let where_ = wd_url.map(|u| format!("WebDAV · {u} · 用户 me"));
    let (wiz2, step2, rewire2) = (p.wiz, p.step, p.rewire);
    let (creds, export, disable) = (p.creds, p.export, p.disable);
    let (u_pass, u_exp, u_reset) = (ui.clone(), ui.clone(), ui.clone());
    let security = section_items(
        "存储与安全",
        vec![
            CardItem::Row(setting_row_desc_signal(
                "存储位置",
                where_,
                btn2("更换…").on_click(move |_| {
                    rewire2.set(true);
                    step2.set(0);
                    wiz2.set(true);
                }),
            )),
            CardItem::Row(setting_row(
                "存储密码",
                Some("已保存在本机（加密存放）"),
                btn2("更新…").on_click(move |_| creds.set(true)),
            )),
            CardItem::Row(setting_row(
                "同步口令",
                Some("其他设备加入时需要输入；修改不影响已导出的密钥"),
                btn2("修改…").on_click(move |_| {
                    u_pass.pass_err.set(String::new());
                    u_pass.pass_dialog.set(true);
                }),
            )),
            CardItem::Row(setting_row(
                "备份密钥",
                Some("可替代口令加入新设备，也能在忘记口令时找回数据"),
                btn2("导出…").on_click(move |_| {
                    u_exp
                        .exported_key
                        .set("bW9jay1kYXRhLWVuY3J5cHRpb24ta2V5LTAxMjM0NTY3ODk=".into());
                    export.set(true);
                }),
            )),
            CardItem::Heading("停用与重置".into()),
            CardItem::Row(setting_row(
                "停用同步",
                Some("本机停止同步，保留设置与存储里的数据"),
                btn2("停用…").on_click(move |_| disable.set(true)),
            )),
            CardItem::Row(setting_row(
                "重置同步库",
                Some("清空存储里的全部同步数据，以本机为准重新开始；其他设备需重新加入"),
                Element::button("重置…")
                    .small()
                    .danger()
                    .on_click(move |_| {
                        u_reset.reset_ack.set(false);
                        u_reset.reset_err.set(String::new());
                        u_reset.reset_guard.arm();
                        u_reset.reset_dialog.set(true);
                    }),
            )),
        ],
    )
    .visible_when(move || cfg.get());

    ui_dialogs(&ui, state);

    vec![
        (None, hero),
        (Some("同步状态"), status),
        (Some("我的设备"), devices),
        (Some("同步选项"), options),
        (Some("存储与安全"), security),
    ]
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
    fn device_name_over_64_chars_is_rejected() {
        assert_eq!(check_device_name("  我的电脑 ").unwrap(), "我的电脑");
        assert_eq!(check_device_name("").unwrap(), "");
        assert!(check_device_name(&"字".repeat(64)).is_ok());
        assert!(check_device_name(&"字".repeat(65)).is_err());
        let ui = SyncUi::new();
        ui.device_name.set("a".repeat(65));
        assert!(ui.save_settings().is_err(), "超长不发 RPC");
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
            "installed": true, "joined": true, "device": "desktop-7f3k",
            "group": "g-ab", "group_name": "家里", "needs_identity": true
        }));
        assert_eq!(s.device, "desktop-7f3k");
        assert_eq!((s.group.as_str(), s.group_name.as_str()), ("g-ab", "家里"));
        assert!(s.needs_identity && s.configured());
        let none = StatusView::parse(&json!({}));
        assert!(!none.needs_identity && !none.configured());
        let not_joined = StatusView::parse(&json!({ "installed": true }));
        assert!(!not_joined.configured(), "已装但没加入远端不算已配置");
        let not_installed = StatusView::parse(&json!({ "joined": true }));
        assert!(!not_installed.configured());
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
            vec![
                ("confirm_identity", json!({})),
                ("join_group", json!({ "group": "g", "mode": "theirs" }))
            ]
        );
        assert_eq!(
            identity_commands(&IdentityChoice::NewGroup),
            vec![("confirm_identity", json!({}))]
        );
    }
}
