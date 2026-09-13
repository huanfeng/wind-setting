//! `system.dota2_compat` 的落地动作：以管理员权限拉起 core 的 `system dota2-compat` 子命令。
//!
//! **为什么不在本进程里直接写注册表**：目标值在 HKLM 下（TSF LanguageProfile 的
//! `Description`），设置程序跑在普通用户上下文，写不动。也不把别名字符串抄一份到本仓——
//! 它必须与 Dota 2 内置白名单**逐字**一致，抄第二份就是漂移源；core 那边有一条把
//! 二进制里提取的字节钉死的测试守着，这里只负责把开关意图转达过去。
//!
//! **为什么用 `ShellExecuteW` 而非 `Command::spawn`**：只有走 shell 的 `runas` 动词才会
//! 弹 UAC；`CreateProcess` 系不会提权，会直接以 `ERROR_ELEVATION_REQUIRED` 失败。
//! 与 `update::install` 同一理由。

use std::path::PathBuf;

/// core 可执行文件（与设置程序同目录，dev 变体带 `_dev` 后缀）。
fn core_exe() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    // 变体后缀复用 mode::pipe_suffix()（"" / "_dev"），不另造一份 dev 判定
    let p = dir.join(format!("wind_input{}.exe", crate::mode::pipe_suffix()));
    p.exists().then_some(p)
}

/// 把一个参数包成 Windows 命令行的一段。
///
/// **为什么必须有它**：登记名称天然带空格（`中文 (简体) - 郑码`），不加引号传过去，
/// core 那侧的 `argv` 会把它拆成三段，`--name` 只收到第一个词——于是写进注册表的是
/// 半截名字，游戏里照样不命中，而两边都不报错。
///
/// 规则按 MSVC 的 `argv` 解析反推：反斜杠只有在紧邻引号时才具转义含义，故只需在
/// 引号前、以及串尾（那里紧跟我们自己补的收尾引号）把反斜杠加倍。
fn quote_arg(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in s.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                out.push(c);
            }
            '"' => {
                // 加倍已攒下的反斜杠，再转义这个引号本身。
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push_str("\\\"");
            }
            _ => {
                backslashes = 0;
                out.push(c);
            }
        }
    }
    // 串尾的反斜杠紧邻下面这个收尾引号，同样要加倍，否则它会把收尾引号转义掉。
    for _ in 0..backslashes {
        out.push('\\');
    }
    out.push('"');
    out
}

/// 把开关状态与登记名称落到系统里。用户会看到一次 UAC；点「否」按失败处理。
///
/// 只在**值确实变了**时调用（见 `state::register_system_registrations`）——每次保存都弹
/// 一次 UAC 是不可接受的。
///
/// `name` 不在本仓校验，原样转达给 core：清洗规则（trim / 控制字符 / 长度上限）与
/// 出厂值都在 `wind_config::sanitize_dota2_alias`，抄第二份就是漂移源。名字不合法时
/// core 以非零码退出——但 `ShellExecuteW` 拿不到退出码（见下），故这条路径上用户看到的
/// 是「设置没生效」而不是一句报错。关闭时 `name` 被 core 忽略。
#[cfg(windows)]
pub fn apply(enabled: bool, name: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    let exe = core_exe().ok_or_else(|| {
        "找不到输入法主程序，无法应用 Dota 2 兼容设置（请确认输入法已正常安装）".to_string()
    })?;

    let params = if enabled {
        format!("system dota2-compat on --name {}", quote_arg(name))
    } else {
        // 关闭时不带名字：core 那侧也不看它，少传一个参数就少一条出错的路。
        "system dota2-compat off".to_string()
    };

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }
    let verb = wide(std::ffi::OsStr::new("runas"));
    let file = wide(exe.as_os_str());
    let args = wide(std::ffi::OsStr::new(&params));

    // ⚠️ 签名必须与 `update::install` 里那份**逐字一致**：同一符号在同一 crate 里
    // 声明两次且签名不同会触发 `clashing_extern_declarations`，且真正的风险是两处
    // 对返回值的解读不同。
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: *mut std::ffi::c_void,
            lp_operation: *const u16,
            lp_file: *const u16,
            lp_parameters: *const u16,
            lp_directory: *const u16,
            n_show_cmd: i32,
        ) -> *mut std::ffi::c_void;
    }

    // 返回的是历史遗留的 HINSTANCE：> 32 即成功，其余是错误码（与 update::install 同）。
    // 拿不到进程句柄 ⇒ **等不到子进程的退出码**，这里只能确认「提权请求被接受」。
    // 真正的成败由用户在下一次打开设置时从名称上看出来；失败路径（键不存在等）
    // 已在 core 侧给出人话提示。
    const SW_HIDE: i32 = 0;
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            args.as_ptr(),
            std::ptr::null(),
            SW_HIDE,
        )
    } as isize;
    match r {
        // 1223 = ERROR_CANCELLED：用户在 UAC 上点了「否」。这不是故障，但设置也确实没生效，
        // 必须报失败——否则界面显示已开启、游戏里却什么都没变。
        5 | 1223 => Err("需要管理员权限才能修改系统里登记的输入法名称，本次未生效".to_string()),
        n if n > 32 => Ok(()),
        n => Err(format!("应用 Dota 2 兼容设置失败（ShellExecute 返回 {n}）")),
    }
}

#[cfg(not(windows))]
pub fn apply(_enabled: bool, _name: &str) -> Result<(), String> {
    Err("Dota 2 兼容仅在 Windows 上可用".to_string())
}

#[cfg(test)]
mod tests {
    use super::quote_arg;

    #[test]
    fn quotes_the_name_so_spaces_survive_argv() {
        // 出厂别名带三处空格，不加引号 core 侧只会收到「中文」。
        assert_eq!(quote_arg("中文 (简体) - 郑码"), r#""中文 (简体) - 郑码""#);
        assert_eq!(quote_arg(""), r#""""#);
    }

    #[test]
    fn escapes_quotes_and_trailing_backslashes() {
        // 引号本身要转义，它前面攒下的反斜杠要加倍。
        assert_eq!(quote_arg("a\"b"), "\"a\\\"b\"");
        // 不挨着引号的反斜杠原样保留 —— 加倍它反而会改掉名字的内容。
        assert_eq!(quote_arg("a\\b"), "\"a\\b\"");
        // 串尾反斜杠紧邻我们补的收尾引号，不加倍就会把它转义掉、整条命令行从此错位。
        assert_eq!(quote_arg("a\\"), "\"a\\\\\"");
        assert_eq!(quote_arg("a\\\\"), "\"a\\\\\\\\\"");
    }
}
