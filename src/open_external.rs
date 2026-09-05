//! 用系统默认程序打开一个文件（字符类「外部编辑」等）。
//!
//! 不引入 `open` / `opener` crate：Windows 上一个 `ShellExecuteW` 就够（与
//! `update/install.rs` 同源写法，那边是提权语境、错误码语义不同，不共用）；
//! macOS / Linux 走 `open` / `xdg-open`。
//!
//! ⚠️ `.yaml` 在干净的 Windows 上**没有关联程序**，`ShellExecuteW` 回 `SE_ERR_NOASSOC`
//! ——那正是最常见的情形（用户不一定装了编辑器）。此时回落到记事本，而不是报错让
//! 用户自己去找文件。

use std::path::Path;

/// 打开成功不等于用户看到了窗口（编辑器可能在后台启动），只保证已交给系统。
pub fn open_with_default_app(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        win::shell_open(path)
    }
    #[cfg(target_os = "macos")]
    {
        spawn("open", path)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        spawn("xdg-open", path)
    }
}

#[cfg(not(windows))]
fn spawn(cmd: &str, path: &Path) -> Result<(), String> {
    std::process::Command::new(cmd)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

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
    /// 文件类型没有关联程序。
    const SE_ERR_NOASSOC: isize = 31;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    pub(super) fn shell_open(path: &Path) -> Result<(), String> {
        let verb = wide(std::ffi::OsStr::new("open"));
        let file = wide(path.as_os_str());
        let ret = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        } as isize;
        // > 32 才算成功（历史约定，非句柄语义）。
        if ret > 32 {
            return Ok(());
        }
        if ret == SE_ERR_NOASSOC {
            return std::process::Command::new("notepad")
                .arg(path)
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("没有关联程序，记事本也打不开：{e}"));
        }
        Err(match ret {
            2 | 3 => "找不到文件".to_string(),
            code => format!("错误码 {code}"),
        })
    }
}
