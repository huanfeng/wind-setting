//! 在线升级：检查 → 下载 → 校验 → 拉起安装器 → 自身退出。
//!
//! 本模块**不依赖 windui**，进度经 [`UpdateMsg`] 通道上报，便于脱离 UI 单测。
//!
//! 两条硬约束：
//! 1. `windui::Signal` 是线程局部 arena 句柄，**非 Send**。下载线程不得直接写 Signal，
//!    必须经 `main()` 构建期注册的 `App::channel::<UpdateMsg>()` 回到 UI 线程。
//! 2. `rpc::call_core` 硬编码 5s 超时，升级链路全程不得经过它。
//!
//! 设计与分期见 `docs/online-update-plan.md`。

pub mod download;
pub mod install;
pub mod notes;
pub mod source;
pub mod version;

use std::path::PathBuf;

/// 更新通道。由二进制变体（`mode::AppMode`）决定，**用户不可切换** ——
/// Dev 与 Release 是并行安装的独立产品，不存在互相升级/降级。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Dev,
}

impl Channel {
    /// 由当前运行变体推导。
    pub fn current() -> Self {
        match crate::mode::get() {
            crate::mode::AppMode::Release => Self::Stable,
            crate::mode::AppMode::Dev => Self::Dev,
        }
    }

    /// 元数据里的 `channel` 字段值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Dev => "dev",
        }
    }

    /// 该通道的元数据地址。两个变体各看各的文件，互不干扰。
    pub fn metadata_url(self) -> &'static str {
        match self {
            Self::Stable => "https://dl.windinput.com/latest.json",
            Self::Dev => "https://dl.windinput.com/latest-dev.json",
        }
    }
}

/// 远端元数据（`latest.json` / `latest-dev.json`）解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: String,
    pub tag: String,
    pub channel: String,
    pub exe_url: String,
    /// 安装包 sha256，64 位小写 hex。**必填** —— 缺失即拒绝升级。
    pub sha256: String,
    /// 安装包字节数。**必填** —— 用于下载前展示体积与下载后校验。
    pub size: u64,
    pub release_notes_url: String,
    pub published_at: String,
    /// 低于此版本需引导全新安装；空表示无限制。
    pub min_upgradable_from: String,
}

/// 一次检查的结论，供 UI 直接渲染。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub has_update: bool,
    pub local_version: String,
    pub remote_version: String,
    /// 已清洗的发布说明；拉取失败时为空串（不阻断主流程）。
    pub release_notes: String,
    pub download_url: String,
    pub sha256: String,
    pub size: u64,
    /// 本地版本无法解析（无 tag 的开发构建），已跳过升级判定。
    pub local_unparsable: bool,
}

impl CheckResult {
    /// 体积的人类可读形式，用于下载前告知用户流量开销。
    pub fn size_display(&self) -> String {
        format_size(self.size)
    }
}

/// 字节数格式化为 MB/KB。用于「更新包 48.2 MB」这类展示。
pub fn format_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= MIB {
        format!("{:.1} MB", b / MIB)
    } else if b >= KIB {
        format!("{:.0} KB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// 升级对话框的阶段。
///
/// `Consent` 是首次联网同意页 —— 输入法产品的隐私敏感性决定了不能默认联网，
/// 必须先取得明示同意（旧 Go 版同此约定，属产品级要求而非技术选择）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePhase {
    /// 尚未同意联网，展示说明与「允许联网并检查」。
    Consent,
    Checking,
    /// 有新版本，展示版本号、体积与发布说明。
    HasUpdate,
    /// 已是最新，或本地为无 tag 的开发构建。
    NoUpdate,
    Downloading,
    /// 已下载并校验通过，待安装。
    Ready,
    Error,
}

/// 后台线程 → UI 线程的消息。**必须 Send**，故只放 String/PathBuf/数值，不夹带 Signal。
#[derive(Debug, Clone)]
pub enum UpdateMsg {
    /// `silent = true` 表示启动时的后台自动检查：无更新或出错都不打扰用户，
    /// 只有确实发现新版本（且未被「跳过此版本」屏蔽）才弹出对话框。
    /// 用消息携带而非存进 AppState，避免与用户手动点击的检查互相串状态。
    CheckDone {
        result: Result<CheckResult, String>,
        silent: bool,
    },
    Progress { downloaded: u64, total: u64 },
    DownloadDone(PathBuf),
    DownloadFailed(String),
    /// 用户取消。与失败分开，UI 才能退回「可重试」而不是弹错误。
    DownloadCancelled,
}

/// 升级链路的错误分类。用户可见文案一律中文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    Network(String),
    InvalidMetadata(String),
    /// 元数据的 channel 与自身变体不符 —— 多半是 CDN 缓存串档，拒绝安装。
    ChannelMismatch {
        expected: String,
        got: String,
    },
    Download(String),
    Verify(String),
    Install(String),
    /// 用户主动取消。**不是故障** —— UI 不应按错误样式展示。
    Cancelled,
}

impl UpdateError {
    pub fn user_message(&self) -> String {
        match self {
            Self::Network(s) => format!("检查更新失败：{s}"),
            Self::InvalidMetadata(s) => format!("更新信息无效：{s}"),
            Self::ChannelMismatch { expected, got } => {
                format!("更新来源不匹配（期望 {expected}，实际 {got}），已取消升级")
            }
            Self::Download(s) => format!("下载失败：{s}"),
            Self::Verify(s) => format!("校验失败：{s}"),
            Self::Install(s) => format!("启动安装程序失败：{s}"),
            Self::Cancelled => "已取消下载".to_string(),
        }
    }

    /// 是否为故障。取消是用户意图，不该弹错误提示。
    pub fn is_failure(&self) -> bool {
        !matches!(self, Self::Cancelled)
    }
}

/// 启动时是否应发起自动检查。
///
/// 三重门控，缺一不可：
/// 1. 用户已明示同意联网 —— 输入法产品不得默认联网
/// 2. 用户未关闭自动检查
/// 3. 距上次检查已超过 `MIN_CHECK_INTERVAL_SECS`，避免频繁开关设置窗口就反复请网
pub fn should_auto_check(
    network_consent: bool,
    auto_check: bool,
    last_check_at: i64,
    now: i64,
) -> bool {
    /// 自动检查的最小间隔：24 小时。
    const MIN_CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;

    if !network_consent || !auto_check {
        return false;
    }
    // 时钟回拨（now < last）时也放行：否则一次错误的系统时间会把自动检查永久卡死
    now.saturating_sub(last_check_at) >= MIN_CHECK_INTERVAL_SECS || now < last_check_at
}

/// 检查更新：拉元数据 → 判定版本 → 有更新时才拉发布说明。
///
/// **不得在 UI 线程调用** —— 含两次网络请求。结果经 [`UpdateMsg::CheckDone`] 回传。
pub fn check_update() -> Result<CheckResult, UpdateError> {
    let channel = Channel::current();
    let info = source::fetch_release_info(channel)?;
    let mut result = decide(&info, version::local_raw())?;
    if result.has_update {
        result.release_notes = source::fetch_release_notes(&info.release_notes_url);
    }
    Ok(result)
}

/// 版本判定（纯函数，不联网）。发布说明留空，由调用方按需填充。
fn decide(info: &ReleaseInfo, local_raw: &str) -> Result<CheckResult, UpdateError> {
    // parse_latest_json 已校验过，此处再解析一次纯属防御，避免 unwrap。
    let remote = version::parse(&info.version)
        .ok_or_else(|| UpdateError::InvalidMetadata(format!("版本号无法解析：{}", info.version)))?;

    let verdict = version::verdict(local_raw, &remote);
    let (has_update, local_unparsable) = match verdict {
        version::Verdict::Upgrade { .. } => (true, false),
        version::Verdict::UpToDate => (false, false),
        // 无 tag 的开发构建：跳过升级提示，而不是把它当成 0.0.0 每次都提示
        version::Verdict::LocalUnparsable => (false, true),
    };

    Ok(CheckResult {
        has_update,
        local_version: local_raw.to_string(),
        remote_version: info.version.clone(),
        release_notes: String::new(),
        download_url: info.exe_url.clone(),
        sha256: info.sha256.clone(),
        size: info.size,
        local_unparsable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(version: &str) -> ReleaseInfo {
        ReleaseInfo {
            version: version.to_string(),
            tag: format!("v{version}"),
            channel: "stable".to_string(),
            exe_url: "https://dl.windinput.com/WindInput-Setup.exe".to_string(),
            sha256: "a".repeat(64),
            size: 48_234_496,
            release_notes_url: "https://dl.windinput.com/notes.md".to_string(),
            published_at: String::new(),
            min_upgradable_from: String::new(),
        }
    }

    #[test]
    fn decide_reports_upgrade_with_asset_details() {
        let r = decide(&info("0.107.0"), "v0.106.0").unwrap();
        assert!(r.has_update);
        assert_eq!(r.remote_version, "0.107.0");
        assert_eq!(r.size, 48_234_496);
        assert_eq!(r.size_display(), "46.0 MB");
        assert!(!r.local_unparsable);
        assert!(
            r.release_notes.is_empty(),
            "decide 不联网，说明应留空由调用方填"
        );
    }

    #[test]
    fn decide_reports_up_to_date() {
        let r = decide(&info("0.107.0"), "v0.107.0").unwrap();
        assert!(!r.has_update);
        assert!(!r.local_unparsable);
    }

    /// 开发构建（无 tag）不提示升级，且要能让 UI 区分"已最新"与"跳过检查"。
    #[test]
    fn decide_flags_unparsable_local_build() {
        let r = decide(&info("0.107.0"), "a1b2c3d").unwrap();
        assert!(!r.has_update);
        assert!(r.local_unparsable);
    }

    #[test]
    fn decide_rejects_unparsable_remote_version() {
        let err = decide(&info("不是版本"), "v0.106.0").unwrap_err();
        assert!(matches!(err, UpdateError::InvalidMetadata(_)));
    }

    #[test]
    fn dev_prerelease_upgrade_is_detected() {
        let r = decide(&info("0.107.0-dev.4"), "v0.107.0-dev.3").unwrap();
        assert!(r.has_update);
    }

    const DAY: i64 = 24 * 60 * 60;

    #[test]
    fn auto_check_requires_explicit_network_consent() {
        // 未同意联网时，即便开着自动检查也不得发起请求
        assert!(!should_auto_check(false, true, 0, DAY * 10));
    }

    #[test]
    fn auto_check_respects_user_switch() {
        assert!(!should_auto_check(true, false, 0, DAY * 10));
    }

    #[test]
    fn auto_check_throttles_within_24h() {
        let now = DAY * 10;
        assert!(!should_auto_check(true, true, now - 3600, now), "1 小时前查过，不应再查");
        assert!(!should_auto_check(true, true, now - DAY + 60, now), "差一分钟到 24h");
        assert!(should_auto_check(true, true, now - DAY, now), "满 24h 应放行");
        assert!(should_auto_check(true, true, 0, now), "从未查过应放行");
    }

    /// 系统时间被调到过去时，若只用 now - last >= 间隔 判断，会把自动检查永久卡死。
    #[test]
    fn auto_check_survives_clock_rollback() {
        let now = DAY * 5;
        let last = DAY * 100; // 上次检查记录在“未来”
        assert!(should_auto_check(true, true, last, now));
    }

    #[test]
    fn channel_urls_are_distinct_and_https() {
        assert_ne!(
            Channel::Stable.metadata_url(),
            Channel::Dev.metadata_url(),
            "两个通道必须指向不同文件，否则 dev 会收到 stable 包"
        );
        for c in [Channel::Stable, Channel::Dev] {
            assert!(c.metadata_url().starts_with("https://"));
        }
    }

    #[test]
    fn channel_str_matches_metadata_contract() {
        assert_eq!(Channel::Stable.as_str(), "stable");
        assert_eq!(Channel::Dev.as_str(), "dev");
    }

    #[test]
    fn format_size_covers_each_magnitude() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2 KB");
        assert_eq!(format_size(48 * 1024 * 1024), "48.0 MB");
        assert_eq!(format_size(50_540_544), "48.2 MB");
    }

    #[test]
    fn channel_mismatch_message_names_both_sides() {
        let e = UpdateError::ChannelMismatch {
            expected: "dev".into(),
            got: "stable".into(),
        };
        let msg = e.user_message();
        assert!(msg.contains("dev") && msg.contains("stable"));
    }
}
