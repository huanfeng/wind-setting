//! `windinput://` 协议解析 + 注册。
//! 格式:`windinput://import/<kind>?url=<https>&name=<可选>`,kind∈theme/schema/dict/extdict。

/// 协议导入请求(解析结果)。
pub struct ProtocolRequest {
    pub kind: String,
    pub url: String,
    pub name: String,
}

const KINDS: &[&str] = &["theme", "schema", "dict", "extdict"];

/// 解析 + 校验协议 URL。非法返回 Err(原因)。
pub fn parse_protocol_url(raw: &str) -> Result<ProtocolRequest, String> {
    let raw = raw.trim();
    let rest = raw
        .strip_prefix("windinput://")
        .or_else(|| raw.strip_prefix("WINDINPUT://"))
        .ok_or("不支持的协议(应为 windinput://)")?;
    // rest = "import/<kind>?<query>"
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, q),
        None => (rest, ""),
    };
    let mut segs = path.splitn(2, '/');
    let action = segs.next().unwrap_or("");
    if !action.eq_ignore_ascii_case("import") {
        return Err(format!("不支持的操作: {action}"));
    }
    let kind = segs
        .next()
        .unwrap_or("")
        .trim_matches('/')
        .to_ascii_lowercase();
    if !KINDS.contains(&kind.as_str()) {
        return Err(format!("不支持的导入类型: {kind}"));
    }
    let (mut url, mut name) = (String::new(), String::new());
    for kv in query.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            let val = percent_decode(v);
            match k {
                "url" => url = val,
                "name" => name = val,
                _ => {}
            }
        }
    }
    if url.is_empty() {
        return Err("缺少 url 参数".into());
    }
    if !url.to_ascii_lowercase().starts_with("https://") {
        return Err("url 参数必须是 https 链接".into());
    }
    Ok(ProtocolRequest { kind, url, name })
}

/// 最小百分号解码(`%XX` → 字节),非法序列原样保留。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ─────────────────────── 协议注册 ───────────────────────

/// 协议根键（HKCU 下的用户级关联，不需管理员权限）。
#[cfg(windows)]
const PROTOCOL_KEY: &str = r"Software\Classes\windinput";
/// 命令行子键。
#[cfg(windows)]
const COMMAND_KEY: &str = r"Software\Classes\windinput\shell\open\command";

/// `windinput://` 协议当前归谁。
///
/// 非 Windows 上 `Ours` / `Other` 不会被构造：协议由应用包（`.app` 的 Info.plist / Linux
/// 的 `.desktop`）声明，[`current_registration`] 恒报 `Unregistered`。页面仍按三态写一套
/// 分支（跨平台共用），故只在这里对非 Windows 放行 dead_code，不动枚举本身。
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum Registration {
    /// 注册表里没有这个协议 —— 一键导入链接点了不会有反应。
    Unregistered,
    /// 已注册且命令行正是**本 exe**。
    Ours,
    /// 已注册但指向别处，携带注册表里的原始命令行。
    ///
    /// 最常见的来源不是"别的软件抢了"，而是**另一份清风输入法**：用户装过安装版又跑便携版、
    /// 或便携目录被移动过。所以文案要把命令行原样摆出来让用户自己判断，不能一律说成"被占用"。
    Other(String),
}

/// 本程序应当写入的命令行。`%1` 是 shell 传入的完整 URL。
#[cfg(any(windows, test))]
pub fn command_for_exe(exe: &str) -> String {
    format!("\"{exe}\" \"%1\"")
}

/// 纯判定：注册表里读到的命令行 vs 本程序应有的命令行。抽出以便单测。
///
/// 比较忽略大小写 —— Windows 路径不区分大小写，`C:\Program Files\…` 与
/// `c:\program files\…` 是同一个文件，按字节比会把自己的注册误判成他人的，
/// 于是每次启动都"自愈"一遍、界面上还一直显示"被其它程序注册"。
#[cfg(any(windows, test))]
pub fn classify(current: Option<&str>, want: &str) -> Registration {
    match current {
        None => Registration::Unregistered,
        // 空值等同未注册：键在但命令行为空，点链接同样毫无反应。
        Some(cur) if cur.trim().is_empty() => Registration::Unregistered,
        Some(cur) if cur.eq_ignore_ascii_case(want) => Registration::Ours,
        Some(cur) => Registration::Other(cur.to_string()),
    }
}

/// 从注册表命令行里取出 exe 路径：`"C:\a\b.exe" "%1"` → `C:\a\b.exe`。
///
/// 兼容没加引号的历史值（早期版本或手工改过的）：那种情况取第一个空白前的片段。
/// 路径含空格又没引号的值本身就是坏的（shell 也唤不起来），无需迁就。
#[cfg(any(windows, test))]
pub fn exe_path_in_command(cmd: &str) -> Option<String> {
    let s = cmd.trim();
    let p = match s.strip_prefix('"') {
        Some(rest) => rest.split_once('"').map(|(p, _)| p)?,
        None => s.split_whitespace().next()?,
    };
    (!p.is_empty()).then(|| p.to_string())
}

/// 他人的注册是否只是「残留」——命令行指向的 exe 已经不在了。
///
/// 这条区分决定了启动自愈能不能静默收回关联：指向**已删除**的路径，
/// 十有八九是本程序自己的旧安装（升级换目录、便携包被移走），收回无争议；
/// 指向**真实存在**的程序，那就是另一个还活着的处理器，静默抢过来是越权 ——
/// 用户会发现自己装的另一个工具突然不响应链接了，且毫无提示。
/// 后者留给高级页展示 + 用户确认后覆盖。
///
/// `exe_exists` 注入以便单测（真实实现传 `|p| Path::new(p).exists()`）。
#[cfg(any(windows, test))]
pub fn is_stale_registration(cmd: &str, exe_exists: impl Fn(&str) -> bool) -> bool {
    match exe_path_in_command(cmd) {
        Some(p) => !exe_exists(&p),
        // 解析不出路径的畸形值：留着也唤不起任何程序，按残留处理。
        None => true,
    }
}

/// 启动时是否自愈注册。
///
/// - **便携版恒不自愈**：便携版的立身之本是不动宿主机，往 HKCU 写协议关联属于留痕；
///   而且便携包常在 U 盘上，路径每次插拔都可能变，自愈只会反复改写宿主机注册表。
///   便携版要用一键导入，走高级页手动注册。
/// - **用户手动取消注册后也不自愈**：否则下次启动又给写回去，「取消注册」形同虚设。
pub fn should_self_heal_on_startup(portable: bool, self_heal_allowed: bool) -> bool {
    !portable && self_heal_allowed
}

/// 读取当前注册状态。
#[cfg(windows)]
pub fn current_registration() -> Registration {
    use winreg::RegKey;
    use winreg::enums::*;

    let Ok(exe) = std::env::current_exe() else {
        return Registration::Unregistered;
    };
    let want = command_for_exe(&exe.to_string_lossy());
    let cur = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(COMMAND_KEY)
        .ok()
        .and_then(|k| k.get_value::<String, _>("").ok());
    classify(cur.as_deref(), &want)
}

/// 注册（或修复）协议关联，指向本 exe。已被他人注册时**直接覆盖** ——
/// 调用方负责在覆盖前征得用户同意（见高级页「桌面集成」卡片）。
#[cfg(windows)]
pub fn register() -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::*;

    let exe = std::env::current_exe().map_err(|e| format!("无法定位自身路径：{e}"))?;
    let want = command_for_exe(&exe.to_string_lossy());
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (root, _) = hkcu
        .create_subkey(PROTOCOL_KEY)
        .map_err(|e| format!("创建注册表项失败：{e}"))?;
    root.set_value("", &"URL:清风输入法协议")
        .map_err(|e| format!("写入协议描述失败：{e}"))?;
    // 这个空值不是可有可无的占位：shell 靠 `URL Protocol` 值的**存在**认定该键是
    // 协议处理器，缺了它链接点下去不会唤起任何程序。
    root.set_value("URL Protocol", &"")
        .map_err(|e| format!("写入协议标记失败：{e}"))?;
    let (cmd, _) = hkcu
        .create_subkey(COMMAND_KEY)
        .map_err(|e| format!("创建命令行项失败：{e}"))?;
    cmd.set_value("", &want)
        .map_err(|e| format!("写入命令行失败：{e}"))?;
    tracing::info!("windinput:// 协议已注册：{want}");
    Ok(())
}

/// 解除协议关联（删掉整棵 `Software\Classes\windinput`）。
///
/// 键本就不存在时视作成功 —— 用户要的是"最终没有关联"这个结果，
/// 报错反而让人以为没删干净。
#[cfg(windows)]
pub fn unregister() -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::*;

    match RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(PROTOCOL_KEY) {
        Ok(()) => {
            tracing::info!("windinput:// 协议注册已解除");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("删除注册表项失败：{e}")),
    }
}

/// 启动时自愈 `windinput://` 协议注册(仅 Windows)。
/// `HKCU\Software\Classes\windinput\shell\open\command = "<exe>" "%1"`。
///
/// 调用前须先过 [`should_self_heal_on_startup`] 的门控。已是本程序则直接跳过，
/// 不做任何写入 —— 绝大多数启动都走这条路，不该每次都碰注册表。
#[cfg(windows)]
pub fn self_heal_registration() {
    match current_registration() {
        Registration::Ours => {}
        Registration::Other(cur) => {
            // 只收回**残留**的关联（目标 exe 已不存在，多半是自己的旧安装路径）。
            // 指向一个还活着的程序时保持原状：静默抢占会让高级页的「当前由谁注册」
            // 永远显示"本程序"——那个状态展示和覆盖前的确认也就都成了摆设。
            if is_stale_registration(&cur, |p| std::path::Path::new(p).exists()) {
                tracing::info!("windinput:// 协议原指向已不存在的 {cur}，自愈收回");
                if let Err(e) = register() {
                    tracing::warn!("windinput:// 协议自愈失败：{e}");
                }
            } else {
                tracing::info!("windinput:// 协议由 {cur} 处理且该程序仍在，启动时不接管");
            }
        }
        Registration::Unregistered => {
            if let Err(e) = register() {
                tracing::warn!("windinput:// 协议注册失败：{e}");
            }
        }
    }
}

/// 非 Windows:协议随 .app 的 Info.plist 注册,无运行时代码。
#[cfg(not(windows))]
pub fn self_heal_registration() {}

/// 非 Windows:协议由 .app 的 Info.plist 声明，运行时无从查证，一律报未注册。
#[cfg(not(windows))]
pub fn current_registration() -> Registration {
    Registration::Unregistered
}

#[cfg(not(windows))]
pub fn register() -> Result<(), String> {
    Err("本平台的协议关联由应用包声明，无需手动注册".to_string())
}

#[cfg(not(windows))]
pub fn unregister() -> Result<(), String> {
    Err("本平台的协议关联由应用包声明，无法在此解除".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_theme() {
        let r = parse_protocol_url(
            "windinput://import/theme?url=https%3A%2F%2Fsite.com%2Fa.yaml&name=Dark",
        )
        .unwrap();
        assert_eq!(r.kind, "theme");
        assert_eq!(r.url, "https://site.com/a.yaml");
        assert_eq!(r.name, "Dark");
    }

    #[test]
    fn parse_theme_import_https() {
        let req = parse_protocol_url(
            "windinput://import/theme?url=https://example.com/theme.toml&name=Blue",
        )
        .unwrap();
        assert_eq!(req.kind, "theme");
        assert_eq!(req.url, "https://example.com/theme.toml");
        assert_eq!(req.name, "Blue");
    }

    #[test]
    fn reject_protocol_import_http_url() {
        assert!(
            parse_protocol_url("windinput://import/theme?url=http://example.com/theme.toml")
                .is_err()
        );
    }

    /// 路径大小写不同仍是同一个文件。按字节比会把自己的注册误判成他人的，
    /// 表现为界面常显"被其它程序注册"、每次启动还多写一次注册表。
    #[test]
    fn classify_ignores_path_case() {
        let want = command_for_exe(r"C:\Program Files\WindInput\wind_setting.exe");
        let cur = command_for_exe(r"c:\program files\windinput\WIND_SETTING.EXE");
        assert_eq!(classify(Some(&cur), &want), Registration::Ours);
    }

    #[test]
    fn classify_covers_three_states() {
        let want = command_for_exe(r"C:\app\wind_setting.exe");
        assert_eq!(classify(None, &want), Registration::Unregistered);
        assert_eq!(classify(Some(&want), &want), Registration::Ours);
        let foreign = command_for_exe(r"D:\other\thing.exe");
        assert_eq!(
            classify(Some(&foreign), &want),
            Registration::Other(foreign.clone())
        );
    }

    /// 键在但命令行为空，点链接同样没反应 —— 归到「未注册」，界面才会给出「注册」按钮。
    #[test]
    fn classify_treats_blank_command_as_unregistered() {
        let want = command_for_exe(r"C:\app\wind_setting.exe");
        assert_eq!(classify(Some(""), &want), Registration::Unregistered);
        assert_eq!(classify(Some("   "), &want), Registration::Unregistered);
    }

    /// 命令行必须带引号包裹的 `%1`：路径含空格（`C:\Program Files\…`）时不加引号
    /// 会被 shell 拆成两个参数，URL 也就永远传不进来。
    #[test]
    fn command_quotes_exe_and_url_placeholder() {
        assert_eq!(
            command_for_exe(r"C:\Program Files\WindInput\wind_setting.exe"),
            r#""C:\Program Files\WindInput\wind_setting.exe" "%1""#
        );
    }

    #[test]
    fn exe_path_extracted_from_quoted_and_bare_commands() {
        assert_eq!(
            exe_path_in_command(r#""C:\Program Files\WindInput\wind_setting.exe" "%1""#).as_deref(),
            Some(r"C:\Program Files\WindInput\wind_setting.exe")
        );
        // 无引号的历史值仍要能取出路径，否则会被当成畸形值静默收回别人的关联。
        assert_eq!(
            exe_path_in_command(r#"C:\app\wind_setting.exe "%1""#).as_deref(),
            Some(r"C:\app\wind_setting.exe")
        );
        assert_eq!(exe_path_in_command("   ").as_deref(), None);
        assert_eq!(exe_path_in_command(r#""" "%1""#).as_deref(), None);
    }

    /// 指向**还活着**的程序绝不能静默收回 —— 否则高级页的状态展示与覆盖确认
    /// 全都失去意义：每次启动都抢回来，用户永远只看得到"由本程序处理"。
    #[test]
    fn live_foreign_registration_is_not_stale() {
        let cmd = r#""C:\other\tool.exe" "%1""#;
        assert!(
            !is_stale_registration(cmd, |_| true),
            "目标存在时必须保持原状，交给用户决定"
        );
        assert!(
            is_stale_registration(cmd, |_| false),
            "目标已不存在（自己的旧安装路径）才自动收回"
        );
    }

    /// 解析不出路径的畸形值：留着也唤不起任何程序，按残留处理。
    #[test]
    fn malformed_command_counts_as_stale() {
        assert!(is_stale_registration("", |_| true));
        assert!(is_stale_registration(r#""" "%1""#, |_| true));
    }

    /// 便携版恒不自愈；非便携版尊重用户是否取消过注册。
    #[test]
    fn self_heal_gate_covers_four_combinations() {
        assert!(should_self_heal_on_startup(false, true), "安装版默认自愈");
        assert!(
            !should_self_heal_on_startup(false, false),
            "用户取消注册后不得再自愈，否则取消形同虚设"
        );
        assert!(
            !should_self_heal_on_startup(true, true),
            "便携版不碰宿主机注册表"
        );
        assert!(!should_self_heal_on_startup(true, false));
    }

    #[test]
    fn rejects() {
        assert!(parse_protocol_url("http://import/theme?url=https://x").is_err()); // 错 scheme
        assert!(parse_protocol_url("windinput://import/unknown?url=https://x").is_err()); // 未知 kind
        assert!(parse_protocol_url("windinput://import/theme").is_err()); // 缺 url
        assert!(parse_protocol_url("windinput://import/theme?url=http://x/a.yaml").is_err());
        // 非 https
    }
}
