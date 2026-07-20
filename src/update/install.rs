//! 拉起安装器并让出窗口。
//!
//! wind-setting 不碰文件替换、进程终止、TSF 注册 —— 全部交给下载下来的安装包
//! （wind-installer stub + 归档）。本模块只负责三件事：判断安装形态、拼参数、启动它。
//!
//! **必须用 `ShellExecuteW` 而非 `Command::spawn`。** 安装器嵌了
//! `requestedExecutionLevel=requireAdministrator`（`wind-installer/assets/app.manifest`），
//! 从非提权进程调 `CreateProcess` 会直接失败（ERROR_ELEVATION_REQUIRED, 740）。
//! `ShellExecuteW` 会走 shell 的提权路径弹出 UAC。

use std::path::{Path, PathBuf};

use super::UpdateError;

/// 便携安装的标记文件名，对应 `config/app.toml` 的 `[app] portable_marker`。
/// 两处必须一致；改配置时记得同步这里。
const PORTABLE_MARKER: &str = "portable_mode";
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
/// `Standard`（`main.rs:114`），静默升级一个便携安装会把它变成标准安装 —— 写注册表、
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
    let portable = has_marker || no_uninstaller;
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
fn portable_signals(install_dir: &Path) -> (bool, bool) {
    (
        install_dir.join(PORTABLE_MARKER).exists(),
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
/// **完全忽略 `--dir`**（`main.rs:118` 直接走 `run_install_wizard()`），传了也没用，
/// 不如不传以免让人以为路径已被指定。
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

#[cfg(not(windows))]
fn shell_execute(_exe: &Path, _args: &str) -> Result<(), UpdateError> {
    Err(UpdateError::Install(
        "当前平台不支持自动安装".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wind_install_test_{}_{tag}", std::process::id()));
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
        std::fs::write(dir.join(PORTABLE_MARKER), b"portable=1\n").unwrap();

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
        assert!(args.contains(r#""D:\Program Files\WindInput""#), "实际: {args}");
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
        let expected = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        assert_eq!(p.install_dir, expected);
        assert_eq!(p.installer, installer);
    }
}
