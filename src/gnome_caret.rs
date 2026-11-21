//! GNOME Wayland 的候选窗定位扩展（随包的 `windinput-caret`）：检测状态、一键启用。
//!
//! GNOME 没有 input-method-v2，候选窗要靠 Shell 扩展补窗口原点才跟得准光标；扩展随 deb 装在
//! `/usr/share/gnome-shell/extensions/`，但**默认不启用**（系统包碰不到用户的 dconf）。
//! 这里替用户做那一步，只在 GNOME + Wayland 会话里出现（X11 会话不需要）。
//!
//! 判据都用与语言无关的接口：`gnome-extensions list [--enabled]` 的输出是 uuid，
//! 扩展是否真在跑看会话总线上 `org.windinput.Caret` 有没有 owner。**不解析**
//! `gnome-extensions info`——它的字段名随界面语言本地化。
//!
//! 外部命令经 [`Sh`] 注入，单测用假实现，不碰真实用户的 GNOME 配置。

use std::path::Path;

/// ESM 版（Shell 45–51）。
pub const UUID_ESM: &str = "windinput-caret@windinput";
/// 旧格式版（Shell 42–44）。
pub const UUID_LEGACY: &str = "windinput-caret-legacy@windinput";

const BUS_NAME: &str = "org.windinput.Caret";

/// 随包扩展支持的 Shell 主版本范围。
const SUPPORTED: std::ops::RangeInclusive<u32> = 42..=51;

pub struct CmdOut {
    pub ok: bool,
    pub stdout: String,
}

/// 外部命令执行器。找不到程序时返回 `ErrorKind::NotFound`。
pub trait Sh {
    fn run(&self, prog: &str, args: &[&str]) -> std::io::Result<CmdOut>;
}

pub struct SystemSh;

impl Sh for SystemSh {
    fn run(&self, prog: &str, args: &[&str]) -> std::io::Result<CmdOut> {
        let out = std::process::Command::new(prog)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()?;
        Ok(CmdOut {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        })
    }
}

/// 此会话是否需要这个扩展：`XDG_CURRENT_DESKTOP` 按冒号分段有一段与 `GNOME` 全等
/// （大小写不敏感，如 `ubuntu:GNOME`），且是 Wayland 会话。与 addon 的判据一致。
pub fn applies(desktop: &str, session_type: &str) -> bool {
    desktop.split(':').any(|s| s.eq_ignore_ascii_case("GNOME"))
        && session_type.eq_ignore_ascii_case("wayland")
}

/// 读当前进程环境判 [`applies`]。
pub fn applies_here() -> bool {
    applies(
        &std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        &std::env::var("XDG_SESSION_TYPE").unwrap_or_default(),
    )
}

/// 从 `gnome-shell --version`（`GNOME Shell 42.9`）取主版本。
pub fn parse_shell_major(out: &str) -> Option<u32> {
    let ver = out.split_whitespace().last()?;
    ver.split('.').next()?.parse().ok()
}

/// 该 Shell 版本该用哪个扩展；超出随包支持范围返回 `None`。
pub fn uuid_for(major: u32) -> Option<&'static str> {
    if !SUPPORTED.contains(&major) {
        None
    } else if major >= 45 {
        Some(UUID_ESM)
    } else {
        Some(UUID_LEGACY)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// 扩展在跑，候选窗定位已生效。
    Active,
    /// 已在启用列表里，但 Shell 还没加载（刚启用，要注销重登）。
    PendingRelogin,
    /// 已安装、未启用；可一键启用。
    Disabled,
    /// 包里没带扩展（便携版 / 非 deb 安装）。
    NotInstalled,
    /// Shell 版本不在随包扩展支持范围。
    Unsupported(String),
    /// 查询不到（缺 `gnome-extensions` 等）。
    Unknown(String),
}

impl Status {
    pub fn text(&self) -> String {
        match self {
            Status::Active => "已启用，候选窗会跟随光标".into(),
            Status::PendingRelogin => "已启用，注销并重新登录后生效".into(),
            Status::Disabled => {
                "未启用 —— 候选窗在未最大化的窗口里会偏离光标，点右侧「启用」".into()
            }
            Status::NotInstalled => {
                "未找到随包的 GNOME 扩展（仅 deb 安装包附带），候选窗在未最大化的窗口里可能偏离光标"
                    .into()
            }
            Status::Unsupported(v) => {
                format!("当前 GNOME Shell {v} 不在随包扩展支持的 42–51 范围内，候选窗可能偏离光标")
            }
            Status::Unknown(e) => format!("无法查询扩展状态：{e}"),
        }
    }

    pub fn can_enable(&self) -> bool {
        matches!(self, Status::Disabled)
    }
}

/// 扩展目录里有没有该 uuid（系统级或用户级）。
fn installed(uuid: &str, dirs: &[&Path]) -> bool {
    dirs.iter()
        .any(|d| d.join(uuid).join("metadata.json").is_file())
}

fn lines(out: &str) -> impl Iterator<Item = &str> {
    out.lines().map(str::trim).filter(|l| !l.is_empty())
}

/// 现读状态。`dirs` 是扩展搜索目录（生产传系统与用户的 `gnome-shell/extensions`）。
pub fn status(sh: &dyn Sh, dirs: &[&Path]) -> Status {
    let ver = match sh.run("gnome-shell", &["--version"]) {
        Ok(o) if o.ok => o.stdout,
        Ok(_) => return Status::Unknown("gnome-shell --version 失败".into()),
        Err(e) => return Status::Unknown(format!("找不到 gnome-shell（{e}）")),
    };
    let Some(major) = parse_shell_major(&ver) else {
        return Status::Unknown(format!("无法识别 GNOME Shell 版本「{}」", ver.trim()));
    };
    let Some(uuid) = uuid_for(major) else {
        return Status::Unsupported(major.to_string());
    };
    if !installed(uuid, dirs) {
        return Status::NotInstalled;
    }
    // 扩展真在跑：总线名有 owner。比读启用列表更接近「到底生没生效」。
    let owned = sh.run(
        "gdbus",
        &[
            "call",
            "--session",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            "org.freedesktop.DBus.NameHasOwner",
            BUS_NAME,
        ],
    );
    if matches!(&owned, Ok(o) if o.ok && o.stdout.contains("true")) {
        return Status::Active;
    }
    match sh.run("gnome-extensions", &["list", "--enabled"]) {
        Ok(o) if o.ok => {
            if lines(&o.stdout).any(|l| l == uuid) {
                Status::PendingRelogin
            } else {
                Status::Disabled
            }
        }
        Ok(_) => Status::Unknown("gnome-extensions list 失败".into()),
        Err(e) => Status::Unknown(format!("找不到 gnome-extensions（{e}）")),
    }
}

/// 解析 `gsettings get org.gnome.shell enabled-extensions` 的输出：
/// `['a@x', 'b@y']` 或空列表的 `@as []`。
fn parse_list(s: &str) -> Vec<String> {
    let s = s.trim();
    let s = s.strip_prefix("@as").unwrap_or(s).trim();
    let inner = s.trim_start_matches('[').trim_end_matches(']');
    inner
        .split(',')
        .map(|t| t.trim().trim_matches('\'').to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

fn format_list(items: &[String]) -> String {
    let body: Vec<String> = items.iter().map(|i| format!("'{i}'")).collect();
    format!("[{}]", body.join(", "))
}

/// 启用扩展。刚装好 Shell 还没扫描过它时，`gnome-extensions enable` 会报「不存在」，
/// 退到把 uuid 追加进 `enabled-extensions`，重登后 Shell 扫描目录就会加载它。
/// 已在列表里的不重复写；别的扩展一个不动。
pub fn enable(sh: &dyn Sh, uuid: &str) -> Result<(), String> {
    if let Ok(CmdOut { ok: true, .. }) = sh.run("gnome-extensions", &["enable", uuid]) {
        return Ok(());
    }
    let cur = sh
        .run(
            "gsettings",
            &["get", "org.gnome.shell", "enabled-extensions"],
        )
        .map_err(|e| format!("找不到 gsettings（{e}）"))?;
    if !cur.ok {
        return Err("读取 enabled-extensions 失败".into());
    }
    let mut items = parse_list(&cur.stdout);
    if !items.iter().any(|i| i == uuid) {
        items.push(uuid.to_string());
    }
    let set = sh
        .run(
            "gsettings",
            &[
                "set",
                "org.gnome.shell",
                "enabled-extensions",
                &format_list(&items),
            ],
        )
        .map_err(|e| format!("写入 enabled-extensions 失败（{e}）"))?;
    if set.ok {
        Ok(())
    } else {
        Err("写入 enabled-extensions 失败".into())
    }
}

/// 按当前 Shell 版本选出要启用的 uuid 并启用。
pub fn enable_for_current_shell(sh: &dyn Sh) -> Result<(), String> {
    let ver = sh
        .run("gnome-shell", &["--version"])
        .map_err(|e| format!("找不到 gnome-shell（{e}）"))?;
    let major = parse_shell_major(&ver.stdout).ok_or("无法识别 GNOME Shell 版本")?;
    let uuid = uuid_for(major).ok_or("当前 GNOME Shell 版本不在支持范围")?;
    enable(sh, uuid)
}

/// 生产用的扩展搜索目录：系统级 + 用户级。
pub fn default_dirs() -> Vec<std::path::PathBuf> {
    let mut v = vec![std::path::PathBuf::from(
        "/usr/share/gnome-shell/extensions",
    )];
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        });
    if let Some(h) = home {
        v.push(h.join("gnome-shell/extensions"));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// 按「程序 + 首个子命令」应答的假执行器，同时记录所有调用。
    struct Fake {
        replies: HashMap<String, std::io::Result<(bool, String)>>,
        calls: RefCell<Vec<String>>,
    }

    impl Fake {
        fn new(rs: Vec<(&str, Option<(bool, &str)>)>) -> Self {
            Self {
                replies: rs
                    .into_iter()
                    .map(|(k, v)| {
                        (
                            k.to_string(),
                            v.map(|(ok, s)| (ok, s.to_string()))
                                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound)),
                        )
                    })
                    .collect(),
                calls: RefCell::new(vec![]),
            }
        }
    }

    impl Sh for Fake {
        fn run(&self, prog: &str, args: &[&str]) -> std::io::Result<CmdOut> {
            self.calls
                .borrow_mut()
                .push(format!("{prog} {}", args.join(" ")));
            let key = match prog {
                "gnome-extensions" | "gsettings" => format!("{prog} {}", args[0]),
                _ => prog.to_string(),
            };
            match self.replies.get(&key) {
                Some(Ok((ok, s))) => Ok(CmdOut {
                    ok: *ok,
                    stdout: s.clone(),
                }),
                Some(Err(e)) => Err(std::io::Error::from(e.kind())),
                None => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
            }
        }
    }

    /// 进程内唯一的临时目录，Drop 时删除。
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "wind-gnome-caret-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn ext_dir(uuid: &str) -> TempDir {
        let d = TempDir::new();
        let p = d.path().join(uuid);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("metadata.json"), "{}").unwrap();
        d
    }

    #[test]
    fn applies_only_on_gnome_wayland() {
        assert!(applies("ubuntu:GNOME", "wayland"));
        assert!(applies("GNOME", "Wayland"));
        assert!(!applies("ubuntu:GNOME", "x11"), "X11 会话不需要扩展");
        assert!(!applies("KDE", "wayland"));
        assert!(!applies("X-GNOME-Foo", "wayland"), "必须整段等于 GNOME");
        assert!(!applies("", ""));
    }

    #[test]
    fn picks_uuid_by_shell_version() {
        assert_eq!(parse_shell_major("GNOME Shell 42.9\n"), Some(42));
        assert_eq!(parse_shell_major("GNOME Shell 48.rc"), Some(48));
        assert_eq!(uuid_for(42), Some(UUID_LEGACY));
        assert_eq!(uuid_for(44), Some(UUID_LEGACY));
        assert_eq!(uuid_for(45), Some(UUID_ESM));
        assert_eq!(uuid_for(51), Some(UUID_ESM));
        assert_eq!(uuid_for(41), None);
        assert_eq!(uuid_for(52), None);
    }

    #[test]
    fn list_roundtrip_keeps_other_extensions() {
        assert_eq!(parse_list("@as []"), Vec::<String>::new());
        assert_eq!(parse_list("['a@x', 'b@y']"), vec!["a@x", "b@y"]);
        assert_eq!(format_list(&["a@x".into(), "b@y".into()]), "['a@x', 'b@y']");
        assert_eq!(format_list(&[]), "[]");
    }

    #[test]
    fn status_active_when_bus_name_owned() {
        let d = ext_dir(UUID_ESM);
        let sh = Fake::new(vec![
            ("gnome-shell", Some((true, "GNOME Shell 46.0"))),
            ("gdbus", Some((true, "(true,)"))),
        ]);
        assert_eq!(status(&sh, &[d.path()]), Status::Active);
    }

    #[test]
    fn status_pending_when_enabled_but_not_running() {
        let d = ext_dir(UUID_ESM);
        let sh = Fake::new(vec![
            ("gnome-shell", Some((true, "GNOME Shell 46.0"))),
            ("gdbus", Some((true, "(false,)"))),
            (
                "gnome-extensions list",
                Some((true, "foo@bar\nwindinput-caret@windinput\n")),
            ),
        ]);
        assert_eq!(status(&sh, &[d.path()]), Status::PendingRelogin);
    }

    #[test]
    fn status_disabled_and_enable_offered() {
        let d = ext_dir(UUID_LEGACY);
        let sh = Fake::new(vec![
            ("gnome-shell", Some((true, "GNOME Shell 42.9"))),
            ("gdbus", Some((true, "(false,)"))),
            ("gnome-extensions list", Some((true, "foo@bar\n"))),
        ]);
        let s = status(&sh, &[d.path()]);
        assert_eq!(s, Status::Disabled);
        assert!(s.can_enable());
    }

    #[test]
    fn status_not_installed_and_unsupported() {
        let empty = TempDir::new();
        let sh = Fake::new(vec![("gnome-shell", Some((true, "GNOME Shell 46.0")))]);
        assert_eq!(status(&sh, &[empty.path()]), Status::NotInstalled);
        let sh = Fake::new(vec![("gnome-shell", Some((true, "GNOME Shell 52.1")))]);
        assert!(matches!(
            status(&sh, &[empty.path()]),
            Status::Unsupported(_)
        ));
        assert!(!Status::NotInstalled.can_enable());
    }

    #[test]
    fn status_unknown_without_tools() {
        let d = ext_dir(UUID_ESM);
        let sh = Fake::new(vec![]);
        assert!(matches!(status(&sh, &[d.path()]), Status::Unknown(_)));
    }

    #[test]
    fn enable_prefers_gnome_extensions() {
        let sh = Fake::new(vec![("gnome-extensions enable", Some((true, "")))]);
        assert!(enable(&sh, UUID_ESM).is_ok());
        assert_eq!(sh.calls.borrow().len(), 1, "成功就不碰 gsettings");
    }

    #[test]
    fn enable_falls_back_to_gsettings_append() {
        // 刚装好：Shell 还不认识它，enable 失败。
        let sh = Fake::new(vec![
            ("gnome-extensions enable", Some((false, ""))),
            ("gsettings get", Some((true, "['foo@bar']\n"))),
            ("gsettings set", Some((true, ""))),
        ]);
        assert!(enable(&sh, UUID_ESM).is_ok());
        let calls = sh.calls.borrow();
        assert_eq!(
            calls.last().unwrap(),
            "gsettings set org.gnome.shell enabled-extensions ['foo@bar', 'windinput-caret@windinput']"
        );
    }

    #[test]
    fn enable_fallback_does_not_duplicate() {
        let sh = Fake::new(vec![
            ("gnome-extensions enable", Some((false, ""))),
            (
                "gsettings get",
                Some((true, "['windinput-caret@windinput']")),
            ),
            ("gsettings set", Some((true, ""))),
        ]);
        assert!(enable(&sh, UUID_ESM).is_ok());
        assert_eq!(
            sh.calls.borrow().last().unwrap(),
            "gsettings set org.gnome.shell enabled-extensions ['windinput-caret@windinput']"
        );
    }

    #[test]
    fn enable_reports_failure() {
        let sh = Fake::new(vec![
            ("gnome-extensions enable", Some((false, ""))),
            ("gsettings get", Some((true, "@as []"))),
            ("gsettings set", Some((false, ""))),
        ]);
        assert!(enable(&sh, UUID_ESM).is_err());
    }
}
