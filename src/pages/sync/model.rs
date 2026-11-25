//! 同步页的纯逻辑（可测）：状态快照、表单、设备与分组、口令与身份选项、命令回执。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::{Value, json};

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
    /// 已装、已启用且已加入远端：此后才显示完整的同步页，否则只给「开始配置」。
    /// 要看 `enabled`：停用后本机密钥仍在，`joined` 不会变。
    pub fn configured(&self) -> bool {
        self.installed && self.enabled && self.joined
    }

    /// 撞号：另一台设备在用本机的设备号，需要给本机改号。
    pub fn duplicate_device(&self) -> bool {
        self.last_error
            .as_deref()
            .is_some_and(|e| e.contains(DUPLICATE_MARK))
    }

    /// 状态卡「冲突」行的值；没有冲突返回空串。
    pub fn conflict_summary(&self) -> String {
        let n = self.conflicts.max(self.conflict_paths.len() as u64);
        if n == 0 {
            String::new()
        } else {
            format!("{n} 个冲突副本")
        }
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
/// 撞号错误里的标记（`wind-sync` 的 `error.rs`）。
const DUPLICATE_MARK: &str = "相同设备号";
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

/// 「立即同步」走 core 的 `sync.now`：结果由 core 弹提示并经状态事件回到界面。
pub(crate) fn sync_now_steps() -> Vec<Step> {
    vec![("sync.now", json!({}))]
}

/// 「停用同步」：只关掉启用开关，存储设置、密钥与远端数据都保留。
pub(crate) fn disable_steps(saved: &Form) -> Vec<Step> {
    let off = Form {
        enabled: false,
        ..saved.clone()
    };
    vec![("sync.setSettings", json!({ "settings": off.to_settings() }))]
}

/// 存储卡的摘要：类型与地址。
pub(crate) fn storage_summary(f: &Form) -> String {
    if f.backend_blank() {
        "未设置".to_string()
    } else if f.s3 {
        format!(
            "S3 · {} · {}{}",
            f.s3_endpoint.trim(),
            f.s3_bucket.trim(),
            match f.s3_prefix.trim() {
                "" => String::new(),
                p => format!("/{p}"),
            }
        )
    } else {
        format!("WebDAV · {} · 用户 {}", f.wd_url.trim(), f.wd_user.trim())
    }
}

/// 换存储后解开数据用的命令：口令或导出的密钥；没填返回 None。
pub(crate) fn unlock_command(
    use_key: bool,
    passphrase: &str,
    key: &str,
) -> Option<(&'static str, Value)> {
    if use_key {
        let key = key.trim();
        (!key.is_empty()).then(|| ("import_key", json!({ "key": key })))
    } else {
        (!passphrase.is_empty()).then(|| ("set_passphrase", json!({ "passphrase": passphrase })))
    }
}

/// 「更换存储」的执行序列。位置变了要重填存储密码（core 会清掉旧的），并用口令或密钥
/// 重新解开新位置的数据；位置没变只更新设置与填了的密码。
pub(crate) fn storage_change_steps(
    saved: &Form,
    new: &Form,
    secrets: Vec<Value>,
    unlock: Option<(&'static str, Value)>,
) -> Result<Vec<Step>, &'static str> {
    let moved = backend_location_changed(saved, new);
    if moved && secrets.is_empty() {
        return Err("更换了存储位置，请填写新位置的密码");
    }
    if moved && unlock.is_none() {
        return Err("更换了存储位置，请填写同步口令或粘贴导出的密钥");
    }
    let mut cmds: Vec<(&'static str, Value)> =
        secrets.into_iter().map(|a| ("set_secret", a)).collect();
    if moved {
        cmds.extend(unlock);
    }
    Ok(save_steps(new.to_settings(), cmds))
}

/// 同步间隔改了才要保存：返回带新间隔的设置，其余原样。
pub(crate) fn interval_change(saved: &Form, v: f64) -> Option<Form> {
    let n = clamp_interval(v);
    (n != saved.interval_min).then(|| Form {
        interval_min: n,
        ..saved.clone()
    })
}

/// 状态行的值：等确认身份或撞号时 core 不同步，不能照阶段写「已是最新」。
pub(crate) fn status_value(s: &StatusView) -> String {
    if s.needs_identity {
        "需要确认本机身份".to_string()
    } else if s.duplicate_device() {
        "需要给本机改号".to_string()
    } else if s.error_kind() == ErrorKind::Notice {
        "稍后自动重试".to_string()
    } else {
        phase_text(&s.phase)
    }
}

/// 「立即同步」可点：确认身份前与撞号时 core 会拒绝。
pub(crate) fn can_sync_now(s: &StatusView) -> bool {
    !s.needs_identity && !s.duplicate_device()
}

/// 同步间隔这次该怎么办。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum IntervalStep {
    Nothing,
    /// 有写入在途：等它结束再发，免得两次 setSettings 乱序、或带着旧存储设置盖掉新的。
    Defer,
    Send(Form),
}

pub(crate) fn interval_step(saved: &Form, v: f64, busy: bool) -> IntervalStep {
    match interval_change(saved, v) {
        None => IntervalStep::Nothing,
        Some(_) if busy => IntervalStep::Defer,
        Some(f) => IntervalStep::Send(f),
    }
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

/// 本机卡「所在组」的值：组名（没有就用组号）· N 台设备；名册还没到时不写台数。
pub(crate) fn group_line(group: &str, group_name: &str, devs: &[DeviceView]) -> String {
    let name = if group_name.is_empty() {
        group
    } else {
        group_name
    };
    match member_count(&group_devices(devs, group), group) {
        0 => name.to_string(),
        n => format!("{name} · {n} 台设备"),
    }
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

/// 生产用的随机口令；系统随机数不可用时返回 None（不退回可预测的种子），由界面报错。
pub(crate) fn random_passphrase_os() -> Option<String> {
    let mut ok = true;
    let p = random_passphrase(|| {
        let mut b = [0u8; 4];
        ok &= getrandom::getrandom(&mut b).is_ok();
        u32::from_le_bytes(b)
    });
    ok.then_some(p)
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
            .filter(|g| !g.id.is_empty())
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
        // core 的 join_group 自己处理未确认的身份；先 confirm 会建出单人组，加入失败就卡住。
        IdentityChoice::Join(g) => vec![("join_group", json!({ "group": g, "mode": "theirs" }))],
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
        "storage" => "保存存储设置",
        "disable" => "停用同步",
        "interval" => "保存同步间隔",
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
                // 结果由 core 弹提示，页内不另报。
                "sync_now" | "interval" => String::new(),
                "storage" => "已保存存储设置".to_string(),
                "disable" => "已停用同步".to_string(),
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

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
