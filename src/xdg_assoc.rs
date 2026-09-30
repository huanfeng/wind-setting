//! Linux 上的系统关联（XDG）：`windinput://` 协议与 `.wpkg` / `.wtheme` 包文件。
//!
//! 与 Windows 的注册表关联（[`crate::protocol`] / [`crate::file_assoc`]）同一套语义：
//! 三态归属（[`Registration`]）、便携版与「用户取消过」两道自愈门、指向已消失程序的残留
//! 才在启动时收回。落点换成 freedesktop 的三件东西：
//!
//! - **`.desktop` 文件**声明「这个程序能处理哪些类型」（`MimeType=`）与怎么启动（`Exec=`）；
//! - **MIME 包描述**（`mime/packages/*.xml`）让系统认得 `*.wpkg` / `*.wtheme` 是什么类型；
//! - **`mimeapps.list`** 的 `[Default Applications]` 记用户选的默认处理者（`xdg-mime default` 写它）。
//!
//! ## 两种安装形态
//!
//! - **系统包**（deb）：上面前两件已由包装进 `/usr/share`，关联开箱即用。本模块不碰系统级
//!   文件；这种形态下「取消注册」无从做起（那是卸载包的事），界面据 [`Facts::system_managed`]
//!   把按钮禁掉并说明。
//! - **便携 / tarball / 别的副本**：系统里没有（或指向另一份程序），注册时把两件文件以
//!   **当前可执行文件的绝对路径**写进 `$XDG_DATA_HOME`（默认 `~/.local/share`），再设默认。
//!   写入的文件带 [`USER_MARK`] 标记：取消注册只删带标记的文件、只去掉 `mimeapps.list`
//!   里指向我们 `.desktop` 的那几项，**别人的关联一个字不动**。
//!
//! ## 为什么模板用 include_str! 取包里那几份文件
//!
//! 用户级写入的内容与 deb 装的必须是同一份（名称、类型、图标），分开写两处迟早一边改了
//! 另一边没改。只有 `Exec` 行换成本程序路径、`MimeType` 行换成此刻已注册的子集。
//!
//! 外部命令经 [`Runner`] 注入，目录经 [`Xdg`] 注入：单测用假命令 + 临时目录，
//! 绝不碰真实用户的 XDG 目录。

use std::path::{Path, PathBuf};

use crate::protocol::{Facts, Registration};

/// 一条关联：一个 MIME 类型 + 负责它的 `.desktop` 文件名。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Target {
    pub mime: &'static str,
    pub desktop: &'static str,
}

const SETTING_DESKTOP: &str = "windinput-setting.desktop";
const IMPORT_DESKTOP: &str = "windinput-import.desktop";

/// `windinput://` 协议（浏览器里的一键导入链接）。
pub const PROTOCOL: Target = Target {
    mime: "x-scheme-handler/windinput",
    desktop: SETTING_DESKTOP,
};
/// 方案包 `.wpkg`。
pub const PACKAGE: Target = Target {
    mime: "application/x-windinput-package",
    desktop: IMPORT_DESKTOP,
};
/// 主题包 `.wtheme`。
pub const THEME: Target = Target {
    mime: "application/x-windinput-theme",
    desktop: IMPORT_DESKTOP,
};
/// 本程序登记的全部关联。
pub const ALL: [Target; 3] = [PROTOCOL, PACKAGE, THEME];

/// 包文件关联（[`crate::file_assoc::Assoc`]）对应的 XDG 关联。按扩展名对：两边各有一张表，
/// 靠扩展名这个两边都有的键连起来，免得给跨平台的 `Assoc` 加一个只有 Linux 读的字段。
pub fn target_for_ext(ext: &str) -> Target {
    if ext.eq_ignore_ascii_case(".wtheme") {
        THEME
    } else {
        PACKAGE
    }
}

const SETTING_TEMPLATE: &str =
    include_str!("../../WindInput/scripts/linux/pkg/windinput-setting.desktop");
const IMPORT_TEMPLATE: &str =
    include_str!("../../WindInput/scripts/linux/pkg/windinput-import.desktop");
const MIME_XML_TEMPLATE: &str =
    include_str!("../../WindInput/scripts/linux/pkg/windinput-mime.xml");
/// 包里 `Exec=` 写死的安装路径；用户级写入时换成本程序路径。
const PACKAGED_EXEC: &str = "/usr/lib/windinput/wind_setting";
/// 用户级 `.desktop` 的归属标记。取消注册只删带它的文件：同名文件可能是用户自己写的。
const USER_MARK: &str = "X-WindInput-UserInstall=true";
/// 用户级 MIME 描述的归属标记（XML 注释）。
const MIME_XML_MARK: &str = "<!-- X-WindInput-UserInstall -->";
const MIME_XML_NAME: &str = "windinput.xml";

fn template_of(desktop: &str) -> &'static str {
    if desktop == SETTING_DESKTOP {
        SETTING_TEMPLATE
    } else {
        IMPORT_TEMPLATE
    }
}

/// 该 `.desktop` 负责的全部类型（按 [`ALL`] 的顺序，写文件时顺序稳定）。
fn mimes_of(desktop: &str) -> impl Iterator<Item = &'static str> + '_ {
    ALL.iter()
        .filter(move |t| t.desktop == desktop)
        .map(|t| t.mime)
}

// ─────────────────────── 注入点 ───────────────────────

/// XDG 基目录。取自环境变量，缺省值按 XDG Base Directory 规范。
#[derive(Clone, Debug)]
pub struct Xdg {
    pub data_home: PathBuf,
    pub config_home: PathBuf,
    /// 系统级数据目录（`$XDG_DATA_DIRS`），deb 装的文件在这里。
    pub data_dirs: Vec<PathBuf>,
}

impl Xdg {
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let non_empty = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
        let data_dirs = non_empty("XDG_DATA_DIRS")
            .map(|v| std::env::split_paths(&v).collect())
            .unwrap_or_else(|| vec!["/usr/local/share".into(), "/usr/share".into()]);
        Self {
            data_home: non_empty("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share")),
            config_home: non_empty("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config")),
            data_dirs,
        }
    }

    fn user_apps(&self) -> PathBuf {
        self.data_home.join("applications")
    }

    fn user_mime(&self) -> PathBuf {
        self.data_home.join("mime")
    }

    /// 本程序会改动的 `mimeapps.list`：用户配置目录那份（`xdg-mime default` 写这里），
    /// 外加旧规范放在 `applications/` 下的那份（KDE4 时代的 xdg-mime 写那里）。
    fn user_mimeapps(&self) -> [PathBuf; 2] {
        [
            self.config_home.join("mimeapps.list"),
            self.user_apps().join("mimeapps.list"),
        ]
    }
}

/// 一次外部命令的结果。
pub struct CmdOut {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// 外部命令（`xdg-mime` / `update-mime-database` / `update-desktop-database`）。
///
/// 抽出来是为了单测：假实现记录调用、按脚本应答，不去改真实用户的关联。
/// 找不到程序时返回 `ErrorKind::NotFound` 的 `io::Error`（与 `std::process::Command` 一致）。
pub trait Runner {
    fn run(&self, prog: &str, args: &[&str], xdg: &Xdg) -> std::io::Result<CmdOut>;
}

/// 真实实现。子进程的 XDG 变量**显式按 [`Xdg`] 设**：本模块读写的目录与命令看到的目录
/// 必须是同一套，否则会出现「文件写进 A、xdg-mime 去 B 里查」的错位。
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, prog: &str, args: &[&str], xdg: &Xdg) -> std::io::Result<CmdOut> {
        let out = std::process::Command::new(prog)
            .args(args)
            .env("XDG_DATA_HOME", &xdg.data_home)
            .env("XDG_CONFIG_HOME", &xdg.config_home)
            .env(
                "XDG_DATA_DIRS",
                std::env::join_paths(&xdg.data_dirs).unwrap_or_default(),
            )
            .stdin(std::process::Stdio::null())
            .output()?;
        Ok(CmdOut {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// 一次操作的上下文：目录、命令、本程序路径。
pub struct Ctx<'a> {
    pub xdg: Xdg,
    pub runner: &'a dyn Runner,
    /// 本程序的绝对路径（写进 `Exec=`，也用来判「是不是指向本程序」）。
    pub exe: PathBuf,
}

/// 真实环境的上下文。定位不到自身路径时返回 `Err`（那样既判不了归属也写不了 `Exec`）。
fn real_ctx() -> Result<Ctx<'static>, String> {
    let exe = std::env::current_exe().map_err(|e| format!("无法定位自身路径：{e}"))?;
    Ok(Ctx {
        xdg: Xdg::from_env(),
        runner: &SystemRunner,
        exe,
    })
}

pub fn facts(t: &Target) -> Facts {
    match real_ctx() {
        Ok(ctx) => ctx.facts(t),
        Err(e) => Facts {
            reg: Registration::Unregistered,
            system_managed: false,
            removable: false,
            error: Some(e),
        },
    }
}

pub fn register(t: &Target) -> Result<(), String> {
    real_ctx()?.register(t)
}

pub fn unregister(t: &Target) -> Result<(), String> {
    real_ctx()?.unregister(t)
}

pub fn self_heal(targets: &[Target]) {
    match real_ctx() {
        Ok(ctx) => ctx.self_heal(targets),
        Err(e) => tracing::warn!("系统关联自愈跳过：{e}"),
    }
}

// ─────────────────────── .desktop 读写 ───────────────────────

/// `.desktop` 里本模块关心的几个键（只看 `[Desktop Entry]` 组）。
#[derive(Debug, Default, PartialEq, Eq)]
struct DesktopInfo {
    name: String,
    exec: String,
    mimes: Vec<String>,
    ours: bool,
}

fn parse_desktop(text: &str) -> DesktopInfo {
    let mut info = DesktopInfo::default();
    let (mut in_entry, mut name_zh) = (false, None::<String>);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "Name" => info.name = v.to_string(),
            "Name[zh_CN]" => name_zh = Some(v.to_string()),
            "Exec" => info.exec = v.to_string(),
            "MimeType" => {
                info.mimes = v
                    .split(';')
                    .filter(|m| !m.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            _ if line == USER_MARK => info.ours = true,
            _ => {}
        }
    }
    if let Some(zh) = name_zh {
        info.name = zh;
    }
    info
}

/// `Exec=` 值里的程序（第一个参数），按 desktop entry 规范去引号与转义。
///
/// 规范是两层：先按字符串值反转义（`\\` → `\`），再按 shell 式引号拆参数（引号内
/// `\"` `` \` `` `\$` `\\` 为转义）。本程序写入时总加引号（见 [`quote_exec_arg`]），
/// 读别人的值时两种写法都得认。
fn exec_program(exec: &str) -> Option<String> {
    let unescaped = exec.replace("\\\\", "\\");
    let s = unescaped.trim_start();
    let prog = if let Some(rest) = s.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        loop {
            match chars.next()? {
                '"' => break,
                '\\' => out.push(chars.next()?),
                c => out.push(c),
            }
        }
        out
    } else {
        s.split_whitespace().next()?.to_string()
    };
    (!prog.is_empty()).then_some(prog)
}

/// 把路径写成 `Exec=` 里的一个参数：总加双引号，引号内的 `"` `` ` `` `$` `\` 前加反斜杠，
/// `%` 写成 `%%`（否则会被当成字段码），最后再按字符串值规则把 `\` 写成 `\\`。
fn quote_exec_arg(path: &str) -> String {
    let mut q = String::from("\"");
    for c in path.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                q.push('\\');
                q.push(c);
            }
            '%' => q.push_str("%%"),
            c => q.push(c),
        }
    }
    q.push('"');
    q.replace('\\', "\\\\")
}

/// 用户级 `.desktop` 的内容：包里那份模板，`Exec` 换成本程序、`MimeType` 换成 `mimes`，
/// 去掉模板头上讲系统安装的注释，补上归属标记。
fn render_desktop(template: &str, exe: &Path, mimes: &[&str]) -> String {
    let mut out = String::from(
        "# 由清风输入法设置程序为当前用户写入（高级 → 系统集成）；在那里取消注册会删除本文件。\n",
    );
    for line in template.lines() {
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("Exec=") {
            let args = rest.strip_prefix(PACKAGED_EXEC).unwrap_or(rest);
            out.push_str("Exec=");
            out.push_str(&quote_exec_arg(&exe.to_string_lossy()));
            out.push_str(args);
        } else if line.starts_with("MimeType=") {
            out.push_str("MimeType=");
            for m in mimes {
                out.push_str(m);
                out.push(';');
            }
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out.push_str(USER_MARK);
    out.push('\n');
    out
}

fn render_mime_xml() -> String {
    // 标记放在 XML 声明之后：声明必须是文件第一行。
    match MIME_XML_TEMPLATE.split_once('\n') {
        Some((decl, rest)) => format!("{decl}\n{MIME_XML_MARK}\n{rest}"),
        None => format!("{MIME_XML_TEMPLATE}\n{MIME_XML_MARK}\n"),
    }
}

// ─────────────────────── mimeapps.list 编辑 ───────────────────────

/// 从 `mimeapps.list` 里去掉「`mime` 由 `desktop` 处理」的记录，别的一个字不动。
///
/// 只看 `[Default Applications]` 与 `[Added Associations]` 两组（`xdg-mime default` /
/// `gio mime` 写这两组）；值是 `;` 分隔的列表时只去掉我们那一项，列表空了才删整行。
/// `[Removed Associations]` 不碰：那是用户对别的程序的屏蔽意愿。
/// 没有可删的返回 `None`，调用方据此不去重写文件。
fn strip_mimeapps(text: &str, mime: &str, desktop: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut in_target_group = false;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let trimmed = body.trim();
        if trimmed.starts_with('[') {
            in_target_group =
                trimmed == "[Default Applications]" || trimmed == "[Added Associations]";
            out.push_str(line);
            continue;
        }
        if in_target_group
            && let Some((k, v)) = body.split_once('=')
            && k.trim() == mime
        {
            let apps: Vec<&str> = v
                .split(';')
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .collect();
            if apps.contains(&desktop) {
                changed = true;
                let kept: Vec<&str> = apps.into_iter().filter(|a| *a != desktop).collect();
                if !kept.is_empty() {
                    out.push_str(&format!("{}={};", k.trim(), kept.join(";")));
                    out.push_str(&line[body.len()..]);
                }
                continue;
            }
        }
        out.push_str(line);
    }
    changed.then_some(out)
}

/// 该文件里是否有「`mime` 由 `desktop` 处理」的记录。
fn mimeapps_mentions(text: &str, mime: &str, desktop: &str) -> bool {
    strip_mimeapps(text, mime, desktop).is_some()
}

// ─────────────────────── 判定与操作 ───────────────────────

fn is_not_found(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
}

fn read_opt(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok()
}

impl Ctx<'_> {
    /// 按 XDG 查找顺序（用户目录先于系统目录）找 `.desktop`，返回路径与内容。
    fn find_desktop(&self, name: &str) -> Option<(PathBuf, DesktopInfo)> {
        std::iter::once(self.xdg.data_home.clone())
            .chain(self.xdg.data_dirs.iter().cloned())
            .map(|d| d.join("applications").join(name))
            .find_map(|p| read_opt(&p).map(|t| (p, parse_desktop(&t))))
    }

    /// 系统目录里的同名 `.desktop`（deb 装的那份）。
    fn system_desktop(&self, name: &str) -> Option<DesktopInfo> {
        self.xdg
            .data_dirs
            .iter()
            .map(|d| d.join("applications").join(name))
            .find_map(|p| read_opt(&p).map(|t| parse_desktop(&t)))
    }

    fn user_desktop_path(&self, name: &str) -> PathBuf {
        self.xdg.user_apps().join(name)
    }

    /// 用户目录里**本程序写的**那份 `.desktop`（没有标记的同名文件视作用户自己的，不认）。
    fn our_user_desktop(&self, name: &str) -> Option<DesktopInfo> {
        read_opt(&self.user_desktop_path(name))
            .map(|t| parse_desktop(&t))
            .filter(|i| i.ours)
    }

    /// `Exec` 指的是不是本程序。两边都规范化再比：`/usr/lib/../lib/…` 与符号链接
    /// 指的是同一个文件；规范化失败（文件不在）时退回按原样比。
    fn exec_is_us(&self, exec: &str) -> bool {
        let Some(prog) = exec_program(exec) else {
            return false;
        };
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        canon(Path::new(&prog)) == canon(&self.exe)
    }

    /// 系统包是否已经提供了**指向本程序**的这个 `.desktop`（即本程序就是包装的那份）。
    fn system_provides_us(&self, desktop: &str) -> bool {
        self.system_desktop(desktop)
            .is_some_and(|i| self.exec_is_us(&i.exec))
    }

    /// 现查一条关联的状态（`xdg-mime query default`，不缓存）。
    pub fn facts(&self, t: &Target) -> Facts {
        let removable = self.has_user_traces(t);
        let default = match self
            .runner
            .run("xdg-mime", &["query", "default", t.mime], &self.xdg)
        {
            Ok(out) => out.stdout.trim().to_string(),
            Err(e) => {
                let why = if is_not_found(&e) {
                    "未找到 xdg-mime（请安装 xdg-utils）".to_string()
                } else {
                    format!("运行 xdg-mime 失败：{e}")
                };
                return Facts {
                    reg: Registration::Unregistered,
                    system_managed: false,
                    removable,
                    error: Some(why),
                };
            }
        };
        let (reg, system_managed) = self.classify(t, &default);
        Facts {
            reg,
            system_managed,
            removable,
            error: None,
        }
    }

    /// 默认处理者（`.desktop` 名）→ 三态，外加「是否由系统包提供」。
    ///
    /// `Other` 带的是给人看的描述：应用名 + 文件名 + 它启动的程序。最常见的「他人」其实是
    /// 另一份清风输入法（装过 deb 又跑便携版），所以程序路径必须摆出来，用户才分得清。
    ///
    /// 默认处理者的 `.desktop` 已经不在（`mimeinfo.cache` 没跟着刷新时 xdg-mime 仍会报出这个
    /// 名字）按**未注册**算：链接点下去谁也唤不起，而报成「别人占着」会让「注册」先弹一个
    /// 「是否覆盖」——覆盖一个不存在的东西，没什么可确认的。
    fn classify(&self, t: &Target, default: &str) -> (Registration, bool) {
        if default.is_empty() {
            return (Registration::Unregistered, false);
        }
        let Some((path, info)) = self.find_desktop(default) else {
            return (Registration::Unregistered, false);
        };
        if default == t.desktop && self.exec_is_us(&info.exec) {
            let system = !path.starts_with(&self.xdg.data_home);
            return (Registration::Ours, system);
        }
        let prog = exec_program(&info.exec).unwrap_or_default();
        let name = if info.name.is_empty() {
            default.to_string()
        } else {
            info.name
        };
        (
            Registration::Other(format!("{name}（{default}：{prog}）")),
            false,
        )
    }

    /// 本程序为这条关联在用户目录留下的东西：带标记且声明了该类型的 `.desktop`，
    /// 或 `mimeapps.list` 里指向我们的默认/关联项。取消注册能撤的就是这些。
    fn has_user_traces(&self, t: &Target) -> bool {
        let file = self
            .our_user_desktop(t.desktop)
            .is_some_and(|i| i.mimes.iter().any(|m| m == t.mime));
        file || self
            .xdg
            .user_mimeapps()
            .iter()
            .filter_map(|p| read_opt(p))
            .any(|text| mimeapps_mentions(&text, t.mime, t.desktop))
    }

    /// 为当前用户注册（或修复）一条关联。已被他人占用时**直接覆盖默认** ——
    /// 调用方负责先征得用户同意（高级页「系统集成」的确认框）。
    pub fn register(&self, t: &Target) -> Result<(), String> {
        // 先确认 xdg-mime 在：否则文件写了一半、默认却设不上，留下一份半成品。
        self.runner
            .run("xdg-mime", &["--version"], &self.xdg)
            .map_err(|e| {
                if is_not_found(&e) {
                    "未找到 xdg-mime（请安装 xdg-utils），无法设置默认程序".to_string()
                } else {
                    format!("运行 xdg-mime 失败：{e}")
                }
            })?;
        if self.system_provides_us(t.desktop) {
            // 系统包里那份就是本程序：用户目录里的同名文件只会遮住它，删掉（只删我们写的）。
            if self.our_user_desktop(t.desktop).is_some() {
                remove_file(&self.user_desktop_path(t.desktop))?;
                self.refresh_desktop_db();
            }
        } else {
            self.install_user_desktop(t)?;
            if t.desktop == IMPORT_DESKTOP && !self.system_knows_mime_types() {
                self.install_user_mime_xml()?;
            }
        }
        let out = self
            .runner
            .run("xdg-mime", &["default", t.desktop, t.mime], &self.xdg)
            .map_err(|e| format!("运行 xdg-mime 失败：{e}"))?;
        if !out.ok {
            return Err(format!("xdg-mime 设置默认程序失败：{}", out.stderr.trim()));
        }
        // 写完回读一遍：桌面环境专属的 `<桌面>-mimeapps.list` 优先级更高，里面若另有指定，
        // 我们写的那行不生效——与其报「已注册」让用户点了链接没反应，不如当场说清楚。
        match self.facts(t).reg {
            Registration::Ours => {
                tracing::info!("{} 已注册为由 {} 处理", t.mime, t.desktop);
                Ok(())
            }
            Registration::Other(who) => Err(format!(
                "已写入当前用户的默认程序，但系统仍报告由 {who} 处理（可能被桌面环境专属的 mimeapps.list 覆盖）"
            )),
            Registration::Unregistered => {
                Err("已写入当前用户的默认程序，但系统仍查不到处理程序".to_string())
            }
        }
    }

    /// 写（或改写）用户级 `.desktop`：`Exec` 指向本程序，`MimeType` = 已登记的 ∪ 本条。
    fn install_user_desktop(&self, t: &Target) -> Result<(), String> {
        let mut mimes: Vec<&str> = match self.our_user_desktop(t.desktop) {
            Some(i) => mimes_of(t.desktop)
                .filter(|m| i.mimes.iter().any(|x| x == m))
                .collect(),
            None => Vec::new(),
        };
        if !mimes.contains(&t.mime) {
            mimes.push(t.mime);
        }
        // 按 ALL 的顺序排，文件内容与登记顺序无关
        mimes.sort_by_key(|m| ALL.iter().position(|t| t.mime == *m));
        let path = self.user_desktop_path(t.desktop);
        if path.exists() && self.our_user_desktop(t.desktop).is_none() {
            return Err(format!(
                "{} 已存在且不是本程序写的，为免覆盖你自己的配置，未做改动",
                path.display()
            ));
        }
        write_file(
            &path,
            &render_desktop(template_of(t.desktop), &self.exe, &mimes),
        )?;
        self.refresh_desktop_db();
        Ok(())
    }

    /// 系统目录里有没有描述我们包类型的 MIME 文件（deb 装的那份）。
    fn system_knows_mime_types(&self) -> bool {
        self.xdg
            .data_dirs
            .iter()
            .any(|d| d.join("mime/packages").join(MIME_XML_NAME).exists())
    }

    fn user_mime_xml_path(&self) -> PathBuf {
        self.xdg.user_mime().join("packages").join(MIME_XML_NAME)
    }

    /// 写用户级 MIME 描述并重建用户 MIME 库。
    ///
    /// 重建工具缺失是**硬错误**：没有它，系统不认识 `*.wpkg`，会按 zip 处理，
    /// 默认程序设得再对双击也到不了本程序——报成功就是骗人。
    fn install_user_mime_xml(&self) -> Result<(), String> {
        let path = self.user_mime_xml_path();
        match read_opt(&path) {
            Some(t) if !t.contains(MIME_XML_MARK) => {
                // 用户自己放的同名文件：不覆盖，按它已描述了我们的类型处理。
                tracing::info!("{} 不是本程序写的，保留原样", path.display());
                return Ok(());
            }
            Some(t) if t == render_mime_xml() => {}
            _ => write_file(&path, &render_mime_xml())?,
        }
        self.update_mime_db()
    }

    fn update_mime_db(&self) -> Result<(), String> {
        let dir = self.xdg.user_mime();
        match self.runner.run(
            "update-mime-database",
            &[&dir.to_string_lossy()],
            &self.xdg,
        ) {
            Ok(o) if o.ok => Ok(()),
            Ok(o) => Err(format!("update-mime-database 失败：{}", o.stderr.trim())),
            Err(e) if is_not_found(&e) => Err(
                "未找到 update-mime-database（请安装 shared-mime-info），系统暂时认不出 .wpkg / .wtheme"
                    .to_string(),
            ),
            Err(e) => Err(format!("运行 update-mime-database 失败：{e}")),
        }
    }

    /// 刷新用户级 `mimeinfo.cache`。失败**不算错**：默认程序记在 `mimeapps.list`，
    /// 查询默认处理者不依赖这份缓存；它只影响「打开方式」列表里有没有本程序。
    fn refresh_desktop_db(&self) {
        let dir = self.xdg.user_apps();
        match self.runner.run(
            "update-desktop-database",
            &[&dir.to_string_lossy()],
            &self.xdg,
        ) {
            Ok(o) if o.ok => {}
            Ok(o) => tracing::warn!("update-desktop-database 失败：{}", o.stderr.trim()),
            Err(e) if is_not_found(&e) => {
                tracing::info!("未找到 update-desktop-database（desktop-file-utils），跳过缓存刷新")
            }
            Err(e) => tracing::warn!("运行 update-desktop-database 失败：{e}"),
        }
    }

    /// 取消当前用户的这条关联：去掉 `mimeapps.list` 里指向我们的项、从我们写的 `.desktop`
    /// 里拿掉该类型（空了就删文件），没有文件类型要描述了再删我们写的 MIME 描述。
    ///
    /// 系统包提供的关联无从解除（那是 `/usr/share` 里的文件），直接报错说明 ——
    /// 界面上这种状态本就不给点「取消注册」。别人的关联一律不动。
    pub fn unregister(&self, t: &Target) -> Result<(), String> {
        let f = self.facts(t);
        if f.system_managed {
            return Err(
                "该关联由系统安装的 windinput 软件包提供，无法按用户解除；如需解除请卸载软件包"
                    .to_string(),
            );
        }
        for p in self.xdg.user_mimeapps() {
            if let Some(text) = read_opt(&p)
                && let Some(new) = strip_mimeapps(&text, t.mime, t.desktop)
            {
                write_file(&p, &new)?;
            }
        }
        if let Some(info) = self.our_user_desktop(t.desktop) {
            let rest: Vec<&str> = mimes_of(t.desktop)
                .filter(|m| *m != t.mime && info.mimes.iter().any(|x| x == m))
                .collect();
            let path = self.user_desktop_path(t.desktop);
            if rest.is_empty() {
                remove_file(&path)?;
            } else {
                write_file(
                    &path,
                    &render_desktop(template_of(t.desktop), &self.exe, &rest),
                )?;
            }
            self.refresh_desktop_db();
        }
        if t.desktop == IMPORT_DESKTOP && self.our_user_desktop(IMPORT_DESKTOP).is_none() {
            let xml = self.user_mime_xml_path();
            if read_opt(&xml).is_some_and(|t| t.contains(MIME_XML_MARK)) {
                remove_file(&xml)?;
                // 删完描述不重建的话，旧的类型定义还留在用户 MIME 库里
                if let Err(e) = self.update_mime_db() {
                    tracing::warn!("{e}");
                }
            }
        }
        tracing::info!("{} 的当前用户关联已解除", t.mime);
        Ok(())
    }

    /// 启动自愈：只修**本程序写过、但指向的程序已不在**的用户级 `.desktop`。
    ///
    /// 与 Windows 同一原则——只收残留，不抢活着的：
    /// - 从没注册过（没有我们的用户级文件）→ 不动。deb 安装开箱即用，便携版要自己点注册；
    /// - 我们的文件指向另一份**还在的**清风输入法 → 不动，那是用户的另一份安装；
    /// - 指向已消失的路径（便携目录搬走了、tarball 换了版本目录）→ 改指本程序；
    ///   若本程序就是系统包装的那份，直接删掉用户级文件，让系统那份生效。
    ///
    /// 不碰 `mimeapps.list`：默认程序记的是 `.desktop` 文件名，文件修好了默认自然就通了。
    pub fn self_heal(&self, targets: &[Target]) {
        let mut desktops: Vec<&str> = targets.iter().map(|t| t.desktop).collect();
        desktops.dedup();
        for d in desktops {
            let Some(info) = self.our_user_desktop(d) else {
                continue;
            };
            if self.exec_is_us(&info.exec) {
                continue;
            }
            let prog = exec_program(&info.exec).unwrap_or_default();
            if !prog.is_empty() && Path::new(&prog).exists() {
                tracing::info!("{d} 由 {prog} 处理且该程序仍在，启动时不接管");
                continue;
            }
            let path = self.user_desktop_path(d);
            let result = if self.system_provides_us(d) {
                tracing::info!("{d} 原指向已不存在的 {prog}，系统包已提供本程序，删除用户级文件");
                remove_file(&path)
            } else {
                tracing::info!("{d} 原指向已不存在的 {prog}，自愈改指本程序");
                let mimes: Vec<&str> = mimes_of(d)
                    .filter(|m| info.mimes.iter().any(|x| x == m))
                    .collect();
                write_file(&path, &render_desktop(template_of(d), &self.exe, &mimes))
            };
            match result {
                Ok(()) => self.refresh_desktop_db(),
                Err(e) => tracing::warn!("{d} 自愈失败：{e}"),
            }
        }
    }
}

fn write_file(p: &Path, content: &str) -> Result<(), String> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建 {} 失败：{e}", dir.display()))?;
    }
    std::fs::write(p, content).map_err(|e| format!("写入 {} 失败：{e}", p.display()))
}

fn remove_file(p: &Path) -> Result<(), String> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if is_not_found(&e) => Ok(()),
        Err(e) => Err(format!("删除 {} 失败：{e}", p.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// 假命令：`xdg-mime query default` / `default` 按内存表应答并记账；
    /// `update-*` 可配成「找不到程序」。
    #[derive(Default)]
    struct FakeRunner {
        defaults: RefCell<HashMap<String, String>>,
        calls: RefCell<Vec<String>>,
        missing: Vec<&'static str>,
    }

    impl Runner for FakeRunner {
        fn run(&self, prog: &str, args: &[&str], _xdg: &Xdg) -> std::io::Result<CmdOut> {
            self.calls
                .borrow_mut()
                .push(format!("{prog} {}", args.join(" ")));
            if self.missing.contains(&prog) {
                return Err(std::io::Error::new(std::io::ErrorKind::NotFound, prog));
            }
            let ok = |stdout: String| {
                Ok(CmdOut {
                    ok: true,
                    stdout,
                    stderr: String::new(),
                })
            };
            match (prog, args) {
                ("xdg-mime", ["query", "default", mime]) => ok(self
                    .defaults
                    .borrow()
                    .get(*mime)
                    .map(|d| format!("{d}\n"))
                    .unwrap_or_default()),
                ("xdg-mime", ["default", desktop, mime]) => {
                    self.defaults
                        .borrow_mut()
                        .insert(mime.to_string(), desktop.to_string());
                    ok(String::new())
                }
                _ => ok(String::new()),
            }
        }
    }

    impl FakeRunner {
        fn set_default(&self, mime: &str, desktop: &str) {
            self.defaults
                .borrow_mut()
                .insert(mime.into(), desktop.into());
        }
        fn called(&self, prefix: &str) -> bool {
            self.calls.borrow().iter().any(|c| c.starts_with(prefix))
        }
    }

    /// 一次性的隔离根目录：用户目录、系统目录、「程序」都在里面，测完删掉。
    struct Sandbox {
        root: PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "wind_setting_xdg_{tag}_{}_{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("bin")).unwrap();
            Self { root }
        }
        fn xdg(&self) -> Xdg {
            Xdg {
                data_home: self.root.join("home/.local/share"),
                config_home: self.root.join("home/.config"),
                data_dirs: vec![self.root.join("usr/share")],
            }
        }
        /// 造一个真实存在的「程序」文件（判残留看的是存在性）。
        fn exe(&self, name: &str) -> PathBuf {
            let p = self.root.join("bin").join(name);
            std::fs::write(&p, "").unwrap();
            p
        }
        fn ctx<'a>(&self, runner: &'a FakeRunner, exe: &Path) -> Ctx<'a> {
            Ctx {
                xdg: self.xdg(),
                runner,
                exe: exe.to_path_buf(),
            }
        }
        /// 模拟 deb：系统目录里放包里那几份原样文件，`Exec` 指向 `exe`。
        fn install_system(&self, exe: &Path) {
            let apps = self.root.join("usr/share/applications");
            for (name, tpl) in [
                (SETTING_DESKTOP, SETTING_TEMPLATE),
                (IMPORT_DESKTOP, IMPORT_TEMPLATE),
            ] {
                let text = tpl.replace(PACKAGED_EXEC, &exe.to_string_lossy());
                write_file(&apps.join(name), &text).unwrap();
            }
            write_file(
                &self
                    .root
                    .join("usr/share/mime/packages")
                    .join(MIME_XML_NAME),
                MIME_XML_TEMPLATE,
            )
            .unwrap();
        }
        fn read(&self, rel: &str) -> Option<String> {
            read_opt(&self.root.join(rel))
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    const USER_SETTING: &str = "home/.local/share/applications/windinput-setting.desktop";
    const USER_IMPORT: &str = "home/.local/share/applications/windinput-import.desktop";
    const USER_XML: &str = "home/.local/share/mime/packages/windinput.xml";
    const MIMEAPPS: &str = "home/.config/mimeapps.list";

    /// 模板就是包里那几份文件：它们必须还长着本模块要替换的那几行，
    /// 否则用户级写入会悄悄写出一份 `Exec` 指向 /usr/lib 的文件。
    #[test]
    fn packaged_templates_have_the_lines_we_rewrite() {
        for (tpl, mimes) in [
            (SETTING_TEMPLATE, vec![PROTOCOL.mime]),
            (IMPORT_TEMPLATE, vec![PACKAGE.mime, THEME.mime]),
        ] {
            let i = parse_desktop(tpl);
            assert_eq!(
                exec_program(&i.exec).as_deref(),
                Some(PACKAGED_EXEC),
                "{tpl}"
            );
            assert_eq!(i.mimes, mimes);
            assert!(!i.ours, "包里的文件不能带用户级标记");
        }
        for t in ALL {
            assert!(
                MIME_XML_TEMPLATE.contains(t.mime) || t.mime.starts_with("x-scheme-handler/"),
                "MIME 描述里缺 {}",
                t.mime
            );
        }
    }

    #[test]
    fn exec_program_handles_quotes_and_escapes() {
        assert_eq!(
            exec_program("/usr/lib/windinput/wind_setting %u").as_deref(),
            Some("/usr/lib/windinput/wind_setting")
        );
        assert_eq!(
            exec_program(r#""/opt/My Apps/wind_setting" %f"#).as_deref(),
            Some("/opt/My Apps/wind_setting")
        );
        assert_eq!(exec_program("firefox %u").as_deref(), Some("firefox"));
        assert_eq!(exec_program("").as_deref(), None);
        assert_eq!(exec_program(r#""" %u"#).as_deref(), None);
    }

    /// 写进去的 `Exec` 读回来必须还是同一个路径——含空格、`$`、`%`、反斜杠、引号的路径都算。
    /// 往返不成立，自己刚写的注册就会被判成「指向其它程序」。
    #[test]
    fn exec_quoting_round_trips_awkward_paths() {
        for p in [
            "/home/u/清风/wind_setting",
            "/opt/My Apps/wind_setting",
            "/tmp/a$b/`c`/wind_setting",
            r"/tmp/back\slash/wind_setting",
            "/tmp/q\"uote/wind_setting",
        ] {
            let text = render_desktop(IMPORT_TEMPLATE, Path::new(p), &[PACKAGE.mime]);
            let i = parse_desktop(&text);
            assert_eq!(exec_program(&i.exec).as_deref(), Some(p), "{text}");
        }
        // `%` 不能裸写，否则被当成字段码
        assert!(quote_exec_arg("/tmp/100%/x").contains("100%%"));
    }

    #[test]
    fn rendered_desktop_keeps_field_code_and_marks_ownership() {
        let text = render_desktop(
            SETTING_TEMPLATE,
            Path::new("/p/wind_setting"),
            &[PROTOCOL.mime],
        );
        assert!(text.contains("Exec=\"/p/wind_setting\" %u\n"), "{text}");
        assert!(
            text.contains("MimeType=x-scheme-handler/windinput;\n"),
            "{text}"
        );
        assert!(text.contains(USER_MARK));
        assert!(
            !text.contains(PACKAGED_EXEC),
            "模板里的系统路径必须全换掉：{text}"
        );
        let i = parse_desktop(&text);
        assert!(i.ours);
        assert_eq!(i.name, "清风输入法设置");
    }

    /// 只去掉「该类型 → 我们」这一项：别的类型、别人的程序、列表里的其它项、别的组都原样保留。
    #[test]
    fn strip_mimeapps_only_removes_our_entry() {
        let text = "[Default Applications]\n\
                    text/html=firefox.desktop\n\
                    x-scheme-handler/windinput=windinput-setting.desktop\n\
                    application/x-windinput-package=other.desktop;windinput-import.desktop;\n\
                    \n\
                    [Added Associations]\n\
                    x-scheme-handler/windinput=windinput-setting.desktop;\n\
                    [Removed Associations]\n\
                    x-scheme-handler/windinput=windinput-setting.desktop;\n";
        let out = strip_mimeapps(text, PROTOCOL.mime, PROTOCOL.desktop).unwrap();
        assert_eq!(
            out,
            "[Default Applications]\n\
             text/html=firefox.desktop\n\
             application/x-windinput-package=other.desktop;windinput-import.desktop;\n\
             \n\
             [Added Associations]\n\
             [Removed Associations]\n\
             x-scheme-handler/windinput=windinput-setting.desktop;\n"
        );
        let out = strip_mimeapps(text, PACKAGE.mime, PACKAGE.desktop).unwrap();
        assert!(
            out.contains("application/x-windinput-package=other.desktop;\n"),
            "{out}"
        );
        // 没有我们的项 → None，调用方不重写文件
        assert!(
            strip_mimeapps(
                "[Default Applications]\ntext/html=a.desktop\n",
                PROTOCOL.mime,
                PROTOCOL.desktop
            )
            .is_none()
        );
        // 别人的程序处理我们的类型：不是我们的项，不动
        assert!(
            strip_mimeapps(
                "[Default Applications]\nx-scheme-handler/windinput=evil.desktop\n",
                PROTOCOL.mime,
                PROTOCOL.desktop
            )
            .is_none()
        );
    }

    #[test]
    fn nothing_registered_reads_as_unregistered() {
        let sb = Sandbox::new("none");
        let r = FakeRunner::default();
        let exe = sb.exe("wind_setting");
        let f = sb.ctx(&r, &exe).facts(&PROTOCOL);
        assert_eq!(f.reg, Registration::Unregistered);
        assert!(!f.system_managed && !f.removable && f.error.is_none());
    }

    /// 缺 xdg-mime 时状态查询要说出原因，而不是静默显示「未注册」让人去点一个注定失败的按钮。
    #[test]
    fn missing_xdg_mime_is_reported() {
        let sb = Sandbox::new("noxdg");
        let r = FakeRunner {
            missing: vec!["xdg-mime"],
            ..Default::default()
        };
        let exe = sb.exe("wind_setting");
        let ctx = sb.ctx(&r, &exe);
        assert!(ctx.facts(&PROTOCOL).error.unwrap().contains("xdg-utils"));
        assert!(ctx.register(&PROTOCOL).unwrap_err().contains("xdg-utils"));
        assert!(
            sb.read(USER_SETTING).is_none(),
            "设不上默认就别留半成品文件"
        );
    }

    /// deb 形态：系统那份指向本程序 → Ours + 系统提供；取消注册无从做起，报错而不去碰系统文件。
    #[test]
    fn system_package_pointing_at_us_is_system_managed() {
        let sb = Sandbox::new("deb");
        let r = FakeRunner::default();
        let exe = sb.exe("wind_setting");
        sb.install_system(&exe);
        r.set_default(PROTOCOL.mime, PROTOCOL.desktop);
        let ctx = sb.ctx(&r, &exe);
        let f = ctx.facts(&PROTOCOL);
        assert_eq!(f.reg, Registration::Ours);
        assert!(f.system_managed);
        assert!(!f.removable);
        assert!(ctx.unregister(&PROTOCOL).is_err());
        assert!(
            sb.read("usr/share/applications/windinput-setting.desktop")
                .is_some(),
            "系统文件不许动"
        );
    }

    /// deb 形态下注册（比如默认被别的程序抢了）：只改默认，不往用户目录写会遮住系统那份的文件。
    #[test]
    fn register_with_system_package_only_sets_default() {
        let sb = Sandbox::new("debreg");
        let r = FakeRunner::default();
        let exe = sb.exe("wind_setting");
        sb.install_system(&exe);
        r.set_default(PACKAGE.mime, "ark.desktop");
        let ctx = sb.ctx(&r, &exe);
        ctx.register(&PACKAGE).unwrap();
        assert!(
            r.called("xdg-mime default windinput-import.desktop application/x-windinput-package")
        );
        assert!(sb.read(USER_IMPORT).is_none());
        assert!(sb.read(USER_XML).is_none(), "系统已认得类型，不重复写");
        assert_eq!(ctx.facts(&PACKAGE).reg, Registration::Ours);
    }

    /// 便携形态走完一圈：注册写用户级文件（Exec=本程序）+ 设默认；取消后文件与 mimeapps 项都清干净，
    /// 别人的关联原样保留。
    #[test]
    fn portable_register_then_unregister_round_trip() {
        let sb = Sandbox::new("portable");
        let r = FakeRunner::default();
        let exe = sb.exe("wind_setting");
        write_file(
            &sb.root.join(MIMEAPPS),
            "[Default Applications]\ntext/html=firefox.desktop\n",
        )
        .unwrap();
        let ctx = sb.ctx(&r, &exe);

        ctx.register(&PROTOCOL).unwrap();
        let text = sb.read(USER_SETTING).expect("写了用户级 .desktop");
        assert!(ctx.exec_is_us(&parse_desktop(&text).exec), "{text}");
        let f = ctx.facts(&PROTOCOL);
        assert_eq!(f.reg, Registration::Ours);
        assert!(!f.system_managed);
        // 假 xdg-mime 不写文件：这里手工补上真 xdg-mime 会写的那行，验证取消时能去掉
        let apps = sb.read(MIMEAPPS).unwrap();
        write_file(
            &sb.root.join(MIMEAPPS),
            &apps.replace(
                "text/html=firefox.desktop\n",
                "text/html=firefox.desktop\nx-scheme-handler/windinput=windinput-setting.desktop\n",
            ),
        )
        .unwrap();
        assert!(ctx.facts(&PROTOCOL).removable);

        ctx.unregister(&PROTOCOL).unwrap();
        assert!(sb.read(USER_SETTING).is_none());
        assert_eq!(
            sb.read(MIMEAPPS).unwrap(),
            "[Default Applications]\ntext/html=firefox.desktop\n",
            "别人的关联一个字不动"
        );
        assert!(r.called("update-desktop-database"));
    }

    /// 两种包共用一个 import.desktop：注册一个只声明一个类型，取消其一不能把另一个也带走；
    /// MIME 描述在最后一个包类型取消时才删。
    #[test]
    fn package_and_theme_share_desktop_but_register_independently() {
        let sb = Sandbox::new("share");
        let r = FakeRunner::default();
        let exe = sb.exe("wind_setting");
        let ctx = sb.ctx(&r, &exe);

        ctx.register(&PACKAGE).unwrap();
        assert_eq!(
            parse_desktop(&sb.read(USER_IMPORT).unwrap()).mimes,
            vec![PACKAGE.mime]
        );
        assert!(sb.read(USER_XML).unwrap().contains(MIME_XML_MARK));
        assert!(r.called("update-mime-database"));
        ctx.register(&THEME).unwrap();
        assert_eq!(
            parse_desktop(&sb.read(USER_IMPORT).unwrap()).mimes,
            vec![PACKAGE.mime, THEME.mime]
        );

        ctx.unregister(&PACKAGE).unwrap();
        assert_eq!(
            parse_desktop(&sb.read(USER_IMPORT).unwrap()).mimes,
            vec![THEME.mime]
        );
        assert!(sb.read(USER_XML).is_some(), "主题包还在用，类型描述不能删");
        ctx.unregister(&THEME).unwrap();
        assert!(sb.read(USER_IMPORT).is_none());
        assert!(sb.read(USER_XML).is_none());
    }

    /// 没有 update-mime-database 时系统认不出 `.wpkg`：报错，不能报成功。
    /// update-desktop-database 缺失则不影响（默认程序不靠那份缓存）。
    #[test]
    fn missing_mime_db_tool_fails_file_registration_only() {
        let sb = Sandbox::new("nomimedb");
        let r = FakeRunner {
            missing: vec!["update-mime-database", "update-desktop-database"],
            ..Default::default()
        };
        let exe = sb.exe("wind_setting");
        let ctx = sb.ctx(&r, &exe);
        assert!(
            ctx.register(&PACKAGE)
                .unwrap_err()
                .contains("shared-mime-info")
        );
        ctx.register(&PROTOCOL).expect("协议不需要 MIME 库");
    }

    /// 同名但不是我们写的用户级文件（用户自己的定制）：注册不覆盖、取消不删除。
    #[test]
    fn foreign_user_file_with_our_name_is_left_alone() {
        let sb = Sandbox::new("foreignfile");
        let r = FakeRunner::default();
        let exe = sb.exe("wind_setting");
        let mine = "[Desktop Entry]\nName=我自己的\nExec=/usr/bin/foo %u\nMimeType=x-scheme-handler/windinput;\n";
        write_file(&sb.root.join(USER_SETTING), mine).unwrap();
        let ctx = sb.ctx(&r, &exe);
        assert!(ctx.register(&PROTOCOL).is_err());
        ctx.unregister(&PROTOCOL).unwrap();
        assert_eq!(sb.read(USER_SETTING).as_deref(), Some(mine));
    }

    /// 默认处理者是别的程序：Other 带应用名、文件名与程序，用户凭此判断；
    /// 同名 `.desktop` 但指向另一份清风输入法也是 Other（那正是「另一份安装」）；
    /// 默认处理者的文件已不在则是未注册。
    #[test]
    fn foreign_defaults_are_described() {
        let sb = Sandbox::new("other");
        let r = FakeRunner::default();
        let exe = sb.exe("wind_setting");
        let other = sb.exe("old_wind_setting");
        sb.install_system(&other);
        write_file(
            &sb.root.join("usr/share/applications/ark.desktop"),
            "[Desktop Entry]\nName=Ark\nName[zh_CN]=方舟\nExec=ark %f\n",
        )
        .unwrap();
        r.set_default(PACKAGE.mime, "ark.desktop");
        r.set_default(PROTOCOL.mime, PROTOCOL.desktop);
        r.set_default(THEME.mime, "gone.desktop");
        let ctx = sb.ctx(&r, &exe);
        let Registration::Other(d) = ctx.facts(&PACKAGE).reg else {
            panic!()
        };
        assert!(d.contains("方舟") && d.contains("ark.desktop"), "{d}");
        let Registration::Other(d) = ctx.facts(&PROTOCOL).reg else {
            panic!()
        };
        assert!(d.contains(&other.to_string_lossy().into_owned()), "{d}");
        assert_eq!(
            ctx.facts(&THEME).reg,
            Registration::Unregistered,
            "默认处理者的文件已不在：谁也唤不起，按未注册算，注册时不必确认覆盖"
        );
    }

    /// 我们写的用户级文件指向已消失的程序（便携目录搬走了）→ 自愈改指本程序；
    /// 指向还在的另一份 → 不接管；从没注册过 → 什么都不写。
    #[test]
    fn self_heal_repairs_only_stale_user_files() {
        let sb = Sandbox::new("heal");
        let r = FakeRunner::default();
        let exe = sb.exe("wind_setting");
        let ctx = sb.ctx(&r, &exe);

        ctx.self_heal(&ALL);
        assert!(sb.read(USER_SETTING).is_none(), "没注册过不自动注册");

        let gone = sb.root.join("moved/wind_setting");
        write_file(
            &sb.root.join(USER_SETTING),
            &render_desktop(SETTING_TEMPLATE, &gone, &[PROTOCOL.mime]),
        )
        .unwrap();
        ctx.self_heal(&[PROTOCOL]);
        assert!(ctx.exec_is_us(&parse_desktop(&sb.read(USER_SETTING).unwrap()).exec));

        let alive = sb.exe("another_wind_setting");
        let theirs = render_desktop(IMPORT_TEMPLATE, &alive, &[THEME.mime]);
        write_file(&sb.root.join(USER_IMPORT), &theirs).unwrap();
        ctx.self_heal(&[PACKAGE, THEME]);
        assert_eq!(sb.read(USER_IMPORT).as_deref(), Some(theirs.as_str()));
    }

    /// 装了 deb 之后，旧便携版留下的用户级文件会遮住系统那份：本程序就是 deb 那份时，
    /// 自愈删掉残留文件而不是把它改写成 /usr/lib 路径的副本。
    #[test]
    fn self_heal_prefers_system_package_over_rewriting() {
        let sb = Sandbox::new("healdeb");
        let r = FakeRunner::default();
        let exe = sb.exe("wind_setting");
        sb.install_system(&exe);
        write_file(
            &sb.root.join(USER_SETTING),
            &render_desktop(SETTING_TEMPLATE, &sb.root.join("gone/ws"), &[PROTOCOL.mime]),
        )
        .unwrap();
        sb.ctx(&r, &exe).self_heal(&[PROTOCOL]);
        assert!(sb.read(USER_SETTING).is_none());
    }

    #[test]
    fn ext_maps_to_its_own_target() {
        assert_eq!(target_for_ext(".wpkg"), PACKAGE);
        assert_eq!(target_for_ext(".WTHEME"), THEME);
        for a in &crate::file_assoc::ALL {
            let t = target_for_ext(a.ext);
            assert!(
                MIME_XML_TEMPLATE.contains(&format!("<glob pattern=\"*{}\"/>", a.ext))
                    && MIME_XML_TEMPLATE
                        .split("<mime-type ")
                        .any(|blk| blk.contains(t.mime) && blk.contains(a.ext)),
                "{} 在 MIME 描述里没有对应到 {}",
                a.ext,
                t.mime
            );
        }
    }
}
