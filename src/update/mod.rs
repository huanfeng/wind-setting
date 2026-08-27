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

    /// 该通道的元数据地址，**按优先级排列**：靠前的先试，取不到或内容不可解析就退到下一个。
    /// 两个变体各看各的文件，互不干扰。
    ///
    /// **平台也各看各的**：Windows 的 `latest.json` 描述 `Setup.exe`，macOS 的
    /// `latest-mac.json` 描述 `.pkg`。合不成一个文件 —— 版本号、大小、sha256 都是每平台
    /// 一份，而在野的老 Windows 客户端只认顶层的那一组字段，改不动。
    ///
    /// Linux 落到 mac 分支只是形式上的：`self_update_supported()` 压根不放它进来。
    ///
    /// # 为什么要多源
    ///
    /// `dl.windinput.com` 与 `windinput.com` 挂在不同的加速配置和节点上。已经观察到
    /// 部分国内网络对前者 TLS 握手失败（`schannel: failed to receive handshake`）而
    /// 后者正常。根因在链路不在服务端，客户端改不了，但多备一条路是能做的。
    ///
    /// # 为什么 dev 通道只有一个源
    ///
    /// 文档站的元数据由 `sync_release_notes.py` 从 **GitHub Release** 组装，而 dev 版
    /// 走的是另一条发布链路（主仓 `dev.ps1` 直推 R2），压根不进 GitHub Release ——
    /// 文档站没有也不该有那两个文件。
    ///
    /// 这里不能凭"多一个源总没坏处"随手补上：文档站对未匹配路径**返回 200 加首页
    /// HTML**（静态托管的 SPA 回落）而不是 404，列进去只会让 dev 用户白等一次超时，
    /// 再拿一坨 47KB 的 HTML 去解析。
    pub fn metadata_urls(self) -> &'static [&'static str] {
        self.metadata_urls_for(cfg!(windows))
    }

    /// [`metadata_urls`] 的纯函数版，平台作参数传入。
    ///
    /// 拆出来只为一件事：`cfg!(windows)` 在**编译期**取值，在 Windows 上跑测试就
    /// 永远走不到 mac 那两个分支 —— 地址写错（漏个 `-mac`、回退链文件名不一致）
    /// 全都测不出来，得等 mac 用户装上才暴露。同 `Artifact` 把平台作参数的理由。
    ///
    /// [`metadata_urls`]: Self::metadata_urls
    fn metadata_urls_for(self, windows: bool) -> &'static [&'static str] {
        match (self, windows) {
            (Self::Stable, true) => &[
                "https://windinput.com/latest.json",
                "https://dl.windinput.com/latest.json",
            ],
            (Self::Dev, true) => &["https://dl.windinput.com/latest-dev.json"],
            (Self::Stable, false) => &[
                "https://windinput.com/latest-mac.json",
                "https://dl.windinput.com/latest-mac.json",
            ],
            (Self::Dev, false) => &["https://dl.windinput.com/latest-mac-dev.json"],
        }
    }
}

/// 元数据里安装包地址的字段名。
///
/// 两平台**故意不共用一个键**。共用的话，「mac 客户端读到了 Windows 的元数据」这类串档
/// 会静默通过，最后下到一个装不上的文件 —— 与 `channel` 字段当初要防的是同一类事故，
/// 只是维度从通道换成了平台。各用各的键，串档会在解析阶段就报「缺少下载地址」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Artifact {
    /// Windows 安装器，键 `exeUrl`。
    Exe,
    /// macOS 安装包，键 `pkgUrl`。
    Pkg,
}

impl Artifact {
    /// 本机平台对应的产物类型。
    pub fn current() -> Self {
        if cfg!(windows) { Self::Exe } else { Self::Pkg }
    }

    /// 元数据里的键名，用于出错时告诉用户到底缺了哪一个。
    pub fn key(self) -> &'static str {
        match self {
            Self::Exe => "exeUrl",
            Self::Pkg => "pkgUrl",
        }
    }
}

/// 远端元数据（`latest.json` / `latest-dev.json`）解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: String,
    pub tag: String,
    pub channel: String,
    /// 安装包地址。取自 `exeUrl`（Windows）或 `pkgUrl`（macOS），见 [`Artifact`]。
    pub download_url: String,
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
    ///
    /// `seq` 是这次检查的请求号，UI 侧只认**最新一次**请求的结果。检查是可以并存的：
    /// 网络慢时用户会关掉对话框再点一次（关闭并不取消已发出的请求），启动时的静默检查
    /// 也可能与手动检查撞在一起。没有请求号的话，先发的那次晚回来就会覆盖当前状态——
    /// 正在下载的界面被打回「发现新版本」，用户再点一次下载就起了第二个下载线程。
    CheckDone {
        result: Result<CheckResult, String>,
        silent: bool,
        seq: u64,
    },
    Progress {
        downloaded: u64,
        total: u64,
    },
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

/// 本平台是否具备自动升级通路。
///
/// Windows（`Setup.exe` + `ShellExecuteW` 提权）与 macOS（`.pkg` + `open` 交给
/// Installer.app）各有一条；其余平台没有发行产物，`install::shell_execute` 是个直接
/// 返回「当前平台不支持自动安装」的桩。
///
/// 这道门不能省：版本号是各平台共用的，没有门就会在没有升级通路的平台上照常判定
/// 「有新版」，弹窗、下载、然后在安装那步报错 —— 整条链路唯一的产出是一个装不上的文件。
///
/// 用 `cfg!` 而非 `#[cfg]`：两边都参与编译，改坏了任一平台都会当场编译失败，
/// 不会出现"条件写错位置、悄悄失效"那种只在另一个平台才暴露的问题。
pub fn self_update_supported() -> bool {
    cfg!(any(windows, target_os = "macos"))
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

/// 调试入口：本次启动是否强制发起检查，绕过 24h 节流与「自动检查」开关。
///
/// 存在的理由：正常路径下三重门控叠加「失败也记时间」，使得开发期几乎无法复现启动
/// 自动检查 —— 首次启动之后的 24 小时内，无论重启多少次都不会再请网，且没有任何
/// 外部可见迹象。手工改 `wind_setting.toml` 的 `last_check_at` 太容易误伤真实配置。
///
/// **不绕过 `network_consent`** —— 未经用户明示同意联网是产品底线，不设调试后门；
/// 开发机上在对话框里点一次同意即可持久化。
///
/// 触发方式（任一）：`--force-update-check` 参数，或 `WIND_UPDATE_FORCE=1` 环境变量。
pub fn force_check_requested(argv: &[String]) -> bool {
    if argv.iter().any(|a| a == "--force-update-check") {
        return true;
    }
    std::env::var("WIND_UPDATE_FORCE").is_ok_and(|v| v == "1")
}

/// 检查更新：拉元数据 → 判定版本 → 按需拉发布说明。
///
/// 有更新时**总是**拉发布说明（对话框必然展示它）。`notes_when_up_to_date` 控制的是
/// 「已是最新」那条路径：用户手动点「检查更新」时即便没有新版本也想看看最新版改了
/// 什么，故要拉；而静默的启动检查在无更新时根本不弹界面，那次请求没有任何消费者，
/// 省掉即可。
///
/// **不得在 UI 线程调用** —— 含两次网络请求。结果经 [`UpdateMsg::CheckDone`] 回传。
pub fn check_update(notes_when_up_to_date: bool) -> Result<CheckResult, UpdateError> {
    let channel = Channel::current();
    let info = source::fetch_release_info(channel)?;
    let mut result = decide(&info, version::local_raw())?;
    if result.has_update || notes_when_up_to_date {
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
        download_url: info.download_url.clone(),
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
            download_url: "https://dl.windinput.com/WindInput-Setup.exe".to_string(),
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

    /// 拉取发布说明的判定（`check_update` 里的那个条件，抽出来单测——它不联网）。
    fn should_fetch_notes(has_update: bool, notes_when_up_to_date: bool) -> bool {
        has_update || notes_when_up_to_date
    }

    /// 有更新时**必须**拉说明，与静默与否无关。
    ///
    /// 这里曾差点写成 `want_notes = !silent`：静默检查发现新版本会照常弹窗，那个窗
    /// 恰恰最需要说明，却会是空的。判定依据是"这条路径会不会展示说明"，不是"静不静默"。
    #[test]
    fn notes_are_fetched_whenever_an_update_will_be_shown() {
        // silent=true → notes_when_up_to_date=false
        assert!(
            should_fetch_notes(true, false),
            "静默 + 有更新：会弹窗，必须有说明"
        );
        assert!(should_fetch_notes(true, true), "手动 + 有更新");
        assert!(
            should_fetch_notes(false, true),
            "手动 + 已最新：也要展示更新记录"
        );
        assert!(
            !should_fetch_notes(false, false),
            "静默 + 已最新：不弹界面，唯一可省的一格"
        );
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
        assert!(
            !should_auto_check(true, true, now - 3600, now),
            "1 小时前查过，不应再查"
        );
        assert!(
            !should_auto_check(true, true, now - DAY + 60, now),
            "差一分钟到 24h"
        );
        assert!(
            should_auto_check(true, true, now - DAY, now),
            "满 24h 应放行"
        );
        assert!(should_auto_check(true, true, 0, now), "从未查过应放行");
    }

    /// 系统时间被调到过去时，若只用 now - last >= 间隔 判断，会把自动检查永久卡死。
    /// 调试入口的参数式。环境变量式不入测：`set_var` 在 edition 2024 是 unsafe，
    /// 且进程级环境是共享状态，并行测试下会互相踩（见 download.rs 中同类注释）。
    #[test]
    fn force_check_flag_is_recognized_only_when_present() {
        let argv = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(force_check_requested(&argv(&[
            "exe",
            "--force-update-check"
        ])));
        assert!(force_check_requested(&argv(&[
            "exe",
            "--page",
            "about",
            "--force-update-check"
        ])));
        assert!(!force_check_requested(&argv(&["exe", "--page", "about"])));
        // 不做前缀匹配：别的参数不该意外触发联网
        assert!(!force_check_requested(&argv(&[
            "exe",
            "--force-update-checkX"
        ])));
    }

    #[test]
    fn auto_check_survives_clock_rollback() {
        let now = DAY * 5;
        let last = DAY * 100; // 上次检查记录在“未来”
        assert!(should_auto_check(true, true, last, now));
    }

    /// 遍历 (通道 × 平台) 全部四种组合。**不能用 `metadata_urls()`** —— 那个读
    /// `cfg!(windows)`，在哪个平台上跑就只能测到哪一半。
    fn all_url_sets() -> Vec<(Channel, bool, &'static [&'static str])> {
        let mut v = Vec::new();
        for c in [Channel::Stable, Channel::Dev] {
            for win in [true, false] {
                v.push((c, win, c.metadata_urls_for(win)));
            }
        }
        v
    }

    #[test]
    fn channel_urls_are_distinct_and_https() {
        for win in [true, false] {
            assert_ne!(
                Channel::Stable.metadata_urls_for(win),
                Channel::Dev.metadata_urls_for(win),
                "两个通道必须指向不同文件，否则 dev 会收到 stable 包（windows={win}）"
            );
        }
        for (c, win, urls) in all_url_sets() {
            assert!(
                !urls.is_empty(),
                "{c:?}/windows={win} 一个源都没有，检查更新会直接失败"
            );
            for u in urls {
                assert!(u.starts_with("https://"), "更新源必须是 https：{u}");
            }
        }
    }

    /// 两个平台必须指向不同文件：mac 客户端读到 Windows 元数据会拿到 `.exe` 的
    /// sha256 与 size，反之亦然。`Artifact` 的键名之防是第二道，这里是第一道。
    #[test]
    fn platform_urls_are_distinct() {
        for c in [Channel::Stable, Channel::Dev] {
            assert_ne!(
                c.metadata_urls_for(true),
                c.metadata_urls_for(false),
                "{c:?} 的两个平台指向了同一组文件"
            );
        }
    }

    /// 同一组内的多个源必须互不相同 —— 同一个地址列两遍，回退就退了个寂寞。
    #[test]
    fn fallback_sources_are_not_duplicated() {
        for (c, win, urls) in all_url_sets() {
            let uniq: std::collections::HashSet<_> = urls.iter().collect();
            assert_eq!(
                uniq.len(),
                urls.len(),
                "{c:?}/windows={win} 的源列表有重复：{urls:?}"
            );
        }
    }

    /// 同一组内的各源必须指向**同名文件**。
    ///
    /// 回退是为了换一条网络路径，不是换一份内容。若主源写 `latest.json`、备源手滑写成
    /// `latest-dev.json`，主源一挂，stable 用户就**静默**拿到 dev 包装上 —— 与 `channel`
    /// 字段当初要防的是同一类事故，只是这次的漏洞开在客户端自己的常量表里。
    #[test]
    fn fallback_sources_point_at_same_filename() {
        for (c, win, urls) in all_url_sets() {
            let names: Vec<&str> = urls
                .iter()
                .map(|u| u.rsplit('/').next().unwrap_or(u))
                .collect();
            assert!(
                names.windows(2).all(|w| w[0] == w[1]),
                "{c:?}/windows={win} 的各源文件名不一致：{names:?}"
            );
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
