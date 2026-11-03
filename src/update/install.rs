//! 拉起安装器并让出窗口。
//!
//! wind-setting 不碰文件替换、进程终止、TSF 注册 —— 全部交给下载下来的安装包
//! （wind-installer stub + 归档）。本模块只负责三件事：判断安装形态、拼参数、启动它。
//!
//! **必须用 `ShellExecuteW` 而非 `Command::spawn`。** 安装器嵌了
//! `requestedExecutionLevel=requireAdministrator`（`wind-installer/assets/app.manifest`），
//! 从非提权进程调 `CreateProcess` 会直接失败（ERROR_ELEVATION_REQUIRED, 740）。
//! `ShellExecuteW` 会走 shell 的提权路径弹出 UAC。
//!
//! # 拿不到、也不需要安装器的退出码
//!
//! `ShellExecuteW` 返回的是历史遗留的 `HINSTANCE`（>32 即成功），**没有进程句柄**，
//! 无从 `WaitForSingleObject`。这不是疏漏而是必然：安装器要终止本进程才能替换文件，
//! 「等它退出」与「被它杀掉」自相矛盾。所以本模块只判断「有没有启动起来」。
//!
//! 因此 wind-installer 的 `3010`（`ERROR_SUCCESS_REBOOT_REQUIRED`，装好了但有文件
//! 被占用需重启清理）**不经过这里**——那个退出码是给会 `Wait` 的调用方（批量部署
//! 脚本等）用的。走升级这条路时，该情形由安装器自己的完成页提示用户，见下。
//!
//! # 安装器不一定会自动关窗
//!
//! `--quiet` 装完通常停留约 2 秒自动退出，但**有两种情况它会一直等用户亲手关闭**：
//! 安装失败，以及有旧文件被占用清不掉（完成页显示重启提示）。后者在升级 TSF 输入法
//! 时很常见——旧 `wind_tsf.dll` 仍被各宿主进程加载着。
//!
//! 对本模块无影响（我们启动完就自退），但若日后要加「安装结束后重新拉起设置窗口」
//! 之类的逻辑，不能假设安装器一定会在固定时间内消失。

use std::path::{Path, PathBuf};

use super::UpdateError;

/// 标准安装才会释放的卸载器。
const UNINSTALLER: &str = "uninstall.exe";

/// 一次安装的执行计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
    /// 已下载并校验通过的安装包。
    pub installer: PathBuf,
    /// 当前安装目录（由自身 exe 位置推导）。
    pub install_dir: PathBuf,
    /// 是否便携/非标准安装。
    pub portable: bool,
}

impl InstallPlan {
    /// 是否走无人值守安装（安装器的 `--quiet`）。便携安装除外 —— 见 [`plan`] 的说明。
    ///
    /// 刻意不叫 `silent`：安装器的 `--silent` 是**完全无界面**，而升级要的是
    /// 「跳过配置页但显示进度、装完自动退出」，对应的是 `--quiet`。
    pub fn unattended(&self) -> bool {
        !self.portable
    }
}

/// 依当前进程位置推导安装计划。
///
/// **便携安装不走静默升级。** wind-installer 的静默分支把 `InstallMode` 硬编码为
/// `Standard`（`main.rs` 的 `run_install`），静默升级一个便携安装会把它变成标准安装 —— 写注册表、
/// 注册 COM/TSF、建快捷方式，用户的安装形态被悄悄改掉。故此处改为拉起完整向导，
/// 由用户自行确认路径与模式。
pub fn plan(installer: &Path) -> Result<InstallPlan, UpdateError> {
    let exe = std::env::current_exe()
        .map_err(|e| UpdateError::Install(format!("无法定位自身路径：{e}")))?;
    let install_dir = exe
        .parent()
        .ok_or_else(|| UpdateError::Install("无法推导安装目录".to_string()))?
        .to_path_buf();

    let (has_marker, no_uninstaller) = portable_signals(&install_dir);
    // 便携形态只有 Windows 有。macOS 的分发只有 .pkg 一种，而 Windows 那套「身边没有
    // uninstall.exe 即判便携」的信号在 mac 上必然命中 —— 可执行文件在 .app 壳里，身边
    // 永远没有卸载器。判定结果对 mac 的安装通路不起作用（pkg 不看这个字段），但会让
    // 下面那行诊断日志一律谎报「便携安装」，排查时反而误导。
    let portable = cfg!(windows) && (has_marker || no_uninstaller);
    // 无条件记录判定依据：升级出问题时，「到底判成了哪种形态、凭什么」是第一现场，
    // 只在便携分支打日志会让标准分支的误判无迹可寻。
    tracing::info!(
        "安装形态判定：dir={} portable_marker={} uninstaller_missing={} → portable={}",
        install_dir.display(),
        has_marker,
        no_uninstaller,
        portable
    );

    Ok(InstallPlan {
        installer: installer.to_path_buf(),
        portable,
        install_dir,
    })
}

/// 便携判定的两个信号，分开返回以便诊断日志逐个呈现。
///
/// 标记检测复用 [`crate::mode::has_portable_marker_in`]（认新旧两个文件名），
/// 不再在此另存一份文件名常量 —— 曾经的两份副本正是「配置路径判成非便携、
/// 升级流程判成便携」这类分歧的温床。
fn portable_signals(install_dir: &Path) -> (bool, bool) {
    (
        crate::mode::has_portable_marker_in(install_dir),
        !install_dir.join(UNINSTALLER).exists(),
    )
}

/// 判定是否为便携/非标准安装。
///
/// 取两个信号的**或**：标记文件存在，或卸载器缺失（便携模式不释放 `uninstall.exe`）。
/// 之所以不只看标记文件：`WritePortableMarker` 是非致命步骤，写失败只警告，
/// 会产出既无标记又无卸载器的「幽灵便携安装」。Receipt 也帮不上忙 —— 它既不含
/// `mode` 也不含 `install_dir`，且便携模式压根不写 receipt。
fn is_portable_install(install_dir: &Path) -> bool {
    let (marker, no_uninstaller) = portable_signals(install_dir);
    marker || no_uninstaller
}

/// 拼安装器命令行参数。
///
/// 便携/非标准安装返回空串（拉起完整向导）—— 注意 wind-installer 的非静默分支
/// **完全忽略 `--dir`**（`main.rs` 的 `run_install` 直接走 `run_install_wizard()`），
/// 传了也没用，不如不传以免让人以为路径已被指定。
pub fn build_args(plan: &InstallPlan) -> String {
    if !plan.unattended() {
        return String::new();
    }
    // `--quiet` 而非 `--silent`：升级要让用户看到安装进度，只是不再询问路径与模式。
    // 必须显式传 --dir：安装器拿不到目录就会用默认值，
    // 对装在非默认路径的用户造成双份安装。
    format!("--quiet --dir \"{}\"", plan.install_dir.display())
}

/// 启动安装器。成功返回后调用方应立即关闭窗口让出安装目录。
pub fn launch(plan: &InstallPlan) -> Result<(), UpdateError> {
    if !plan.installer.exists() {
        return Err(UpdateError::Install(format!(
            "安装包不存在：{}",
            plan.installer.display()
        )));
    }
    let args = build_args(plan);
    // 记录**实际**传出去的命令行：排查「为何没有静默」时，这一行能直接区分
    // 「wind-setting 没传」与「传了但安装器没收到/没生效」。
    tracing::info!(
        "启动安装器：unattended={} exe={} args=[{}]",
        plan.unattended(),
        plan.installer.display(),
        args
    );
    let r = shell_execute(&plan.installer, &args);
    match &r {
        Ok(()) => tracing::info!("安装器已启动，窗口即将关闭"),
        Err(e) => tracing::warn!("安装器启动失败：{}", e.user_message()),
    }
    r
}

#[cfg(windows)]
fn shell_execute(exe: &Path, args: &str) -> Result<(), UpdateError> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void,
            lp_operation: *const u16,
            lp_file: *const u16,
            lp_parameters: *const u16,
            lp_directory: *const u16,
            n_show_cmd: i32,
        ) -> *mut c_void;
    }

    const SW_SHOWNORMAL: i32 = 1;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    // verb 用 "open" 而非 "runas"：安装器 manifest 已声明 requireAdministrator，
    // shell 会自动提权；显式 runas 在已提权时反而多一次无谓的确认。
    let verb = wide(std::ffi::OsStr::new("open"));
    let file = wide(exe.as_os_str());
    let params = wide(std::ffi::OsStr::new(args));
    let params_ptr = if args.is_empty() {
        std::ptr::null()
    } else {
        params.as_ptr()
    };

    let ret = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params_ptr,
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;

    // ShellExecuteW 的返回值 > 32 才算成功（这是它的历史约定，非句柄语义）
    if ret > 32 {
        Ok(())
    } else {
        Err(UpdateError::Install(match ret {
            // 用户在 UAC 弹窗点了「否」——是主动取消，不该当成故障文案
            1223 => "已取消提权，未开始安装".to_string(),
            2 | 3 => "找不到安装程序".to_string(),
            code => format!("错误码 {code}"),
        }))
    }
}

/// macOS：把 `.pkg` 交给系统的 Installer.app。
///
/// **不自己 `installer -pkg -target /`** —— 那需要 root，而 wind_setting 是普通用户进程。
/// `open` 拉起 Installer.app，由它自行向用户索要管理员授权，与用户双击安装包的路径完全
/// 一致，也就不必在设置程序里自建一套提权。
///
/// `args` 一概忽略：`.pkg` 没有 `--quiet` 这类开关，安装一律走向导。故 [`build_args`]
/// 那套 Windows 参数在 mac 上算出什么都无所谓，不必为它加平台分支（那样反倒会让它那组
/// 纯函数单测在 mac 上集体失效）。
///
/// 与 Windows 一样拿不到、也不需要退出码：pkg 的 postinstall 会杀掉正在运行的设置 app
/// 和输入法进程，「等它结束」与「被它杀掉」自相矛盾。
///
/// 未公证的包由我们自己下载、不带 quarantine 扩展属性，`open` 不会被 Gatekeeper 拦；
/// 但**若将来改成让浏览器下载再打开，就会被拦**，那条路要另配公证。
#[cfg(target_os = "macos")]
fn shell_execute(pkg: &Path, _args: &str) -> Result<(), UpdateError> {
    std::process::Command::new("/usr/bin/open")
        .arg(pkg)
        .spawn()
        .map(|_| ())
        .map_err(|e| UpdateError::Install(format!("拉起系统安装器失败：{e}")))
}

/// Linux 不走 `shell_execute`：安装要等 pkexec 授权与 apt 跑完才知道成败，由
/// [`install_deb`] 在后台线程里同步执行。这里只是让非 Windows/mac 平台能编译过。
#[cfg(not(any(windows, target_os = "macos")))]
fn shell_execute(_exe: &Path, _args: &str) -> Result<(), UpdateError> {
    Err(UpdateError::Install("当前平台不支持该安装方式".to_string()))
}

/// Linux 安装结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebInstall {
    /// `apt-get` 已装完。
    Installed,
    /// 没有 pkexec，已把 `.deb` 交给桌面的软件安装器，成败未知。
    HandedOff,
}

/// 拼 `pkexec apt-get install` 的命令行（不含 `pkexec` 自身）。
///
/// - 用 `apt-get install <绝对路径>` 而非 `dpkg -i`：前者会顺带解决依赖。路径必须是绝对的，
///   apt 靠「含 `/`」区分本地文件与包名。
/// - `env DEBIAN_FRONTEND=noninteractive`：pkexec 没有终端，不能让 debconf 等输入。
/// - `-y`：同意安装的是用户在对话框里点「立即安装」那一下，apt 不必再问。
pub fn apt_args(deb: &Path) -> Vec<std::ffi::OsString> {
    use std::ffi::OsString;
    vec![
        OsString::from("env"),
        OsString::from("DEBIAN_FRONTEND=noninteractive"),
        OsString::from("apt-get"),
        OsString::from("install"),
        OsString::from("-y"),
        deb.as_os_str().to_owned(),
    ]
}

/// 在 PATH 里找可执行文件。
fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
}

/// 取 stderr 的最后几行作错误说明（apt 的真正原因总在末尾）。
fn tail_lines(bytes: &[u8], n: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// 安装 `.deb`。**阻塞**，必须在后台线程调用。
///
/// 有 `pkexec`：弹 polkit 授权框后以 root 跑 `apt-get install`，等它结束并据退出码报成败。
/// 无 `pkexec`：退到 `xdg-open`，交给桌面的软件安装器（GNOME 软件 / Discover / 深度商店等），
/// 此时只能说「已交出」。两者都没有则报错并给出手动命令。
///
/// 与 Windows/mac 不同，这里**不需要让出窗口**：postinst 只结束输入法服务，不碰设置程序，
/// Linux 上替换正在运行的二进制也不会出事。
pub fn install_deb(deb: &Path) -> Result<DebInstall, UpdateError> {
    if !deb.is_absolute() || !deb.is_file() {
        return Err(UpdateError::Install(format!(
            "安装包不存在：{}",
            deb.display()
        )));
    }
    let manual = format!("sudo apt install {}", deb.display());

    if let Some(pkexec) = which("pkexec") {
        tracing::info!("pkexec 安装：{}", deb.display());
        let out = std::process::Command::new(pkexec)
            .args(apt_args(deb))
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| UpdateError::Install(format!("无法启动 pkexec：{e}")))?;
        return match out.status.code() {
            Some(0) => Ok(DebInstall::Installed),
            // polkit：126 = 授权对话框被取消 / 认证失败，127 = 无法授权
            Some(126) | Some(127) => Err(UpdateError::Install(format!(
                "未获得管理员授权，已取消安装。也可在终端手动执行：{manual}"
            ))),
            _ => {
                let detail = tail_lines(&out.stderr, 4);
                Err(UpdateError::Install(format!(
                    "apt 安装失败：{detail}\n可在终端手动执行：{manual}"
                )))
            }
        };
    }

    if let Some(xdg) = which("xdg-open") {
        tracing::info!("无 pkexec，交给 xdg-open：{}", deb.display());
        return std::process::Command::new(xdg)
            .arg(deb)
            .spawn()
            .map(|_| DebInstall::HandedOff)
            .map_err(|e| UpdateError::Install(format!("拉起系统安装器失败：{e}")));
    }

    Err(UpdateError::Install(format!(
        "找不到 pkexec 或 xdg-open，请在终端手动执行：{manual}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("wind_install_test_{}_{tag}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn plan_for(dir: &Path) -> InstallPlan {
        InstallPlan {
            installer: dir.join("Setup.exe"),
            install_dir: dir.to_path_buf(),
            portable: is_portable_install(dir),
        }
    }

    #[test]
    fn standard_install_is_detected_by_uninstaller_presence() {
        let dir = temp_dir("std");
        std::fs::write(dir.join(UNINSTALLER), b"x").unwrap();

        assert!(!is_portable_install(&dir));
        assert!(plan_for(&dir).unattended(), "标准安装应走无人值守升级");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn portable_marker_forces_interactive_wizard() {
        let dir = temp_dir("marker");
        std::fs::write(dir.join(UNINSTALLER), b"x").unwrap();
        std::fs::write(dir.join(crate::mode::PORTABLE_MARKER_NAME), b"portable=1\n").unwrap();

        assert!(is_portable_install(&dir));
        assert!(
            !plan_for(&dir).unattended(),
            "便携安装无人值守升级会被转成标准安装，必须走向导"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 「幽灵便携安装」：标记写失败（非致命步骤）导致既无标记也无卸载器。
    /// 只看标记文件会误判成标准安装并静默升级，故必须两个信号取或。
    #[test]
    fn missing_uninstaller_alone_also_forces_wizard() {
        let dir = temp_dir("ghost");
        // 既无 uninstall.exe 也无 portable_mode
        assert!(is_portable_install(&dir));
        assert!(!plan_for(&dir).unattended());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unattended_args_carry_quoted_install_dir() {
        let plan = InstallPlan {
            installer: PathBuf::from(r"C:\Temp\Setup.exe"),
            install_dir: PathBuf::from(r"D:\Program Files\WindInput"),
            portable: false,
        };
        let args = build_args(&plan);
        // --quiet 而非 --silent：升级要显示进度界面，不是完全无界面
        assert!(args.starts_with("--quiet "), "实际: {args}");
        // 路径含空格，必须带引号，否则 clap 会把它拆成两个参数
        assert!(
            args.contains(r#""D:\Program Files\WindInput""#),
            "实际: {args}"
        );
    }

    /// 不传 --dir 会让静默安装落到 InstallConfig::default() 的默认目录，
    /// 对装在非默认路径的用户造成双份安装。
    #[test]
    fn unattended_args_always_specify_dir() {
        let plan = InstallPlan {
            installer: PathBuf::from("Setup.exe"),
            install_dir: PathBuf::from(r"C:\Custom\Path"),
            portable: false,
        };
        assert!(build_args(&plan).contains("--dir"));
    }

    #[test]
    fn portable_args_are_empty() {
        let plan = InstallPlan {
            installer: PathBuf::from("Setup.exe"),
            install_dir: PathBuf::from(r"C:\Portable"),
            portable: true,
        };
        assert_eq!(
            build_args(&plan),
            "",
            "非静默分支忽略 --dir，传了只会让人误以为路径已指定"
        );
    }

    #[test]
    fn launch_rejects_missing_installer() {
        let plan = InstallPlan {
            installer: PathBuf::from(r"C:\definitely\not\here\Setup.exe"),
            install_dir: PathBuf::from(r"C:\x"),
            portable: false,
        };
        let err = launch(&plan).unwrap_err();
        assert!(err.user_message().contains("不存在"));
    }

    #[test]
    fn plan_uses_own_exe_directory() {
        let installer = std::env::temp_dir().join("Setup.exe");
        let p = plan(&installer).unwrap();
        let expected = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        assert_eq!(p.install_dir, expected);
        assert_eq!(p.installer, installer);
    }

    #[test]
    fn apt_args_use_absolute_path_and_noninteractive() {
        let a = apt_args(Path::new("/home/u/.cache/windinput/update/x.deb"));
        let a: Vec<String> = a.iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert_eq!(
            a,
            [
                "env",
                "DEBIAN_FRONTEND=noninteractive",
                "apt-get",
                "install",
                "-y",
                "/home/u/.cache/windinput/update/x.deb"
            ]
        );
    }

    #[test]
    fn install_deb_rejects_relative_or_missing_path() {
        assert!(install_deb(Path::new("x.deb")).is_err());
        assert!(install_deb(Path::new("/nonexistent/x.deb")).is_err());
    }

    #[test]
    fn tail_lines_keeps_last_non_empty_lines() {
        let t = tail_lines(b"a\n\nb\nc\nd\ne\n", 2);
        assert_eq!(t, "d\ne");
        assert_eq!(tail_lines(b"", 3), "");
    }
}
