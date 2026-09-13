//! `system.dota2_compat` 的落地动作：以管理员权限拉起 core 的 `system dota2-compat` 子命令。
//!
//! **为什么不在本进程里直接写注册表**：目标值在 HKLM 下（TSF LanguageProfile 的
//! `Description`），设置程序跑在普通用户上下文，写不动。也不把别名字符串抄一份到本仓——
//! 它必须与 Dota 2 内置白名单**逐字**一致，抄第二份就是漂移源；core 那边有一条把
//! 二进制里提取的字节钉死的测试守着，这里只负责把开关意图转达过去。
//!
//! ⚠️ **已知缺口：非法名字在本仓这一侧无声无息。** `ShellExecuteW` 拿不到子进程退出码
//! （见 `apply` 末尾），core 拒掉一个名字时我们收不到任何信号，界面显示成功、基线被推到
//! 新值，此后永远不判脏——用户看到的是「填了、应用了、游戏里没候选」，再点应用也没用。
//! 前置校验本该用 core 同一个 `wind_config::sanitize_dota2_alias` 挡在弹 UAC 之前，但
//! `wind-config` 在本仓只是 **dev-dependency**（见 Cargo.toml 那段「不引入新的路径前提」），
//! 二进制里链不到。补法有三条，都要先定架构，故暂记在此而非就地抄一份校验：
//!   ① 把 wind-config 提为正式依赖；
//!   ② 让 core 在 `config.setItems` 那条 RPC 上校验本键——那条链路有现成的报错通道；
//!   ③ 设置端改用 `Command::output()` 跑 `system dota2-compat status` 读回真实登记名
//!      （设置程序无控制台，core 的 `attach_parent_console` 会失败，管道得以保留）。
//! ⛔ 不要就地抄一份清洗规则：那是漂移源，而漂移的表现同样是「填了没反应」。
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
/// **为什么必须有它**：名单里**多数条目**带空格（如 `中文 (简体) - 搜狗拼音输入法`），
/// 不加引号传过去，core 那侧的 `argv` 会把它拆成好几段，`--name` 只收到第一个词——
/// 于是写进注册表的是半截名字，游戏里照样不命中，而两边都不报错。
/// （出厂值「拼音输入法」恰好不含空格，正是挑它的理由之一；但用户一旦换成别的条目
/// 就又需要这层引号，所以这里不能按出厂值来省。）
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
/// 拼给 core 的命令行参数，顺带把名字校验掉。
///
/// 抽成纯函数是为了**可测**：`apply` 本身会弹 UAC，测试里跑不了；判据留在它内部
/// 就只能靠肉眼守。`#[cfg_attr]` 让非 Windows 构建也编得过（那边 `apply` 是桩）。
#[cfg_attr(not(windows), allow(dead_code))]
fn build_params(enabled: bool, name: &str) -> Result<String, String> {
    if !enabled {
        // ⚠️ 关闭时连名字都不传：core 那侧同样忽略它，少一个参数少一条出错的路。
        // 也因此，用户把名字改坏之后仍然关得掉——关闭是唯一的退路，不能被名字挡住。
        return Ok("system dota2-compat off".to_string());
    }
    // 名字原样转达，清洗在 core 那侧做（规则与出厂值都在 wind_config::sanitize_dota2_alias，
    // 抄第二份就是漂移源）。本仓收不到它的拒绝结果，见模块文档那条已知缺口。
    Ok(format!("system dota2-compat on --name {}", quote_arg(name)))
}

/// `name` 原样转达给 core，由那侧清洗与校验；关闭时它被忽略。
/// 校验失败在本仓这一侧看不见，见模块文档那条已知缺口。
#[cfg(windows)]
pub fn apply(enabled: bool, name: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    // ⚠️ 顺序：先校验、再找 exe、最后才弹 UAC。校验失败时用户拿到的是一句能照做的
    // 提示，而不是一次白弹的提权。
    let params = build_params(enabled, name)?;

    let exe = core_exe().ok_or_else(|| {
        "找不到输入法主程序，无法应用 Dota 2 兼容设置（请确认输入法已正常安装）".to_string()
    })?;

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
    use super::{build_params, quote_arg};

    #[test]
    fn quotes_the_name_so_spaces_survive_argv() {
        // 名单里多数条目带空格，不加引号 core 侧只会收到第一个词。
        assert_eq!(
            quote_arg("中文 (简体) - 搜狗拼音输入法"),
            r#""中文 (简体) - 搜狗拼音输入法""#
        );
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

    #[test]
    fn the_off_path_carries_no_name_at_all() {
        // 用户把名字改坏之后必须还能关得掉 —— 关闭是唯一的退路，名字不能挡在这。
        assert_eq!(
            build_params(false, "随便什么坏名字").unwrap(),
            "system dota2-compat off"
        );
    }

    #[test]
    fn the_on_path_passes_the_name_quoted() {
        assert_eq!(
            build_params(true, "中文 (简体) - 搜狗拼音输入法").unwrap(),
            r#"system dota2-compat on --name "中文 (简体) - 搜狗拼音输入法""#
        );
    }

    /// 守护：本仓传下去的名字必须能被 core 的清洗函数原样接受，否则就是在
    /// 「界面显示 A、系统里落 B」。`wind-config` 只是 dev-dependency，故这条只在
    /// 测试里成立——生产路径上的缺口记在模块文档里。
    #[test]
    fn the_factory_alias_survives_core_validation_unchanged() {
        let d = wind_config::DEFAULT_DOTA2_ALIAS;
        assert_eq!(wind_config::sanitize_dota2_alias(d).unwrap(), d);
        assert_eq!(
            build_params(true, d).unwrap(),
            format!("system dota2-compat on --name \"{d}\"")
        );
    }
}
