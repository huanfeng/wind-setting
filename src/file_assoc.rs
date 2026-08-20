//! `.wpkg` 方案包的文件关联（HKCU，用户级，不需管理员权限）。
//!
//! 与 [`crate::protocol`] 的 `windinput://` 注册同一套规则与门控：便携版恒不注册、
//! 用户取消后不自愈、指向已消失 exe 的残留才在启动时静默收回。三态判定
//! （[`Registration`]）、命令行构造与残留判断直接复用 protocol 的纯函数 ——
//! 它们本就与「协议还是扩展名」无关。
//!
//! 差异只在注册表形状：扩展名关联是**两级**间接（`.wpkg` → ProgId → 打开命令），
//! 而协议是一级。于是"归谁"要看两跳（见 [`classify_assoc`]），解除时也只动
//! `.wpkg` 键与**本程序的** ProgId 树 —— 别人的 ProgId 可能同时服务其它扩展名，
//! 删它是越权。

use crate::protocol::{Registration, classify};

/// 本程序的 ProgId。`.wpkg` 键的默认值指向它，真正的打开命令挂在它下面。
pub const PROGID: &str = "WindInput.Package";

/// 扩展名键（HKCU 下的用户级关联）。
#[cfg(windows)]
const EXT_KEY: &str = r"Software\Classes\.wpkg";
/// ProgId 根键。
#[cfg(windows)]
const PROGID_KEY: &str = r"Software\Classes\WindInput.Package";
/// 打开命令子键。
#[cfg(windows)]
const COMMAND_KEY: &str = r"Software\Classes\WindInput.Package\shell\open\command";
/// 关联图标子键。缺它时资源管理器回落到打开命令那个 exe 的**主图标**——
/// 于是 `.wpkg` 文件和设置程序长得一模一样，看图标分不出哪个是数据文件。
#[cfg(windows)]
const ICON_KEY: &str = r"Software\Classes\WindInput.Package\DefaultIcon";

/// 纯判定：两跳读到的值 → 三态。抽出以便单测。
///
/// - `.wpkg` 没指向任何 ProgId（键缺失/空值）= 未关联，双击不会打开任何程序；
/// - 指向本 ProgId 时，归属由打开命令决定（复用 [`classify`]，含旧安装路径的
///   残留识别与大小写不敏感比较）；
/// - 指向别的 ProgId 时是他人关联。展示优先给那个 ProgId 的打开命令 ——
///   用户凭它分辨"别的软件"还是"自己另一份安装"；命令读不到时给 ProgId 名，
///   这种关联本身就是死的（双击唤不起程序），自愈会按残留收回。
pub fn classify_assoc(
    ext_progid: Option<&str>,
    progid_cmd: Option<&str>,
    want: &str,
) -> Registration {
    match ext_progid {
        None => Registration::Unregistered,
        Some(p) if p.trim().is_empty() => Registration::Unregistered,
        Some(p) if p.eq_ignore_ascii_case(PROGID) => classify(progid_cmd, want),
        Some(p) => match progid_cmd {
            Some(cmd) if !cmd.trim().is_empty() => Registration::Other(cmd.to_string()),
            _ => Registration::Other(format!("(ProgId: {p}，无打开命令)")),
        },
    }
}

/// `DefaultIcon` 的值：`"<exe路径>,-2"`。
///
/// **负号是关键**：正数按图标在资源表里的**位置序号**取（0 开始），会随资源顺序
/// 变动而指错；负数按**资源 ID** 取，ID 由 build.rs 显式钉死（主图标 1、本图标 2）。
/// 路径含空格时整体加引号，与打开命令同一处理。
pub fn icon_value_for_exe(exe: &str) -> String {
    format!("\"{exe}\",-2")
}

/// 读 HKCU 下某键的默认值。键不存在 / 值不存在都归 `None`。
#[cfg(windows)]
fn read_default(path: &str) -> Option<String> {
    use winreg::RegKey;
    use winreg::enums::*;

    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(path)
        .ok()
        .and_then(|k| k.get_value::<String, _>("").ok())
}

/// 读取当前 `.wpkg` 关联状态（两跳现读注册表，不缓存）。
#[cfg(windows)]
pub fn current_registration() -> Registration {
    let Ok(exe) = std::env::current_exe() else {
        return Registration::Unregistered;
    };
    let want = crate::protocol::command_for_exe(&exe.to_string_lossy());
    let ext_progid = read_default(EXT_KEY);
    let cmd = ext_progid
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .and_then(|p| read_default(&format!(r"Software\Classes\{p}\shell\open\command")));
    classify_assoc(ext_progid.as_deref(), cmd.as_deref(), &want)
}

/// 注册（或修复）`.wpkg` 关联，指向本 exe。已被他人关联时**直接覆盖** `.wpkg` 的
/// 指向 —— 调用方负责在覆盖前征得用户同意（见高级页「系统集成」卡片）。
/// 他人的 ProgId 树不动：覆盖指向已足够，删树是越权。
#[cfg(windows)]
pub fn register() -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::*;

    let exe = std::env::current_exe().map_err(|e| format!("无法定位自身路径：{e}"))?;
    let want = crate::protocol::command_for_exe(&exe.to_string_lossy());
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (root, _) = hkcu
        .create_subkey(PROGID_KEY)
        .map_err(|e| format!("创建 ProgId 项失败：{e}"))?;
    root.set_value("", &"清风输入法方案包")
        .map_err(|e| format!("写入类型描述失败：{e}"))?;
    let (cmd, _) = hkcu
        .create_subkey(COMMAND_KEY)
        .map_err(|e| format!("创建命令行项失败：{e}"))?;
    cmd.set_value("", &want)
        .map_err(|e| format!("写入命令行失败：{e}"))?;
    let (icon, _) = hkcu
        .create_subkey(ICON_KEY)
        .map_err(|e| format!("创建图标项失败：{e}"))?;
    icon.set_value("", &icon_value_for_exe(&exe.to_string_lossy()))
        .map_err(|e| format!("写入关联图标失败：{e}"))?;
    let (ext, _) = hkcu
        .create_subkey(EXT_KEY)
        .map_err(|e| format!("创建扩展名项失败：{e}"))?;
    ext.set_value("", &PROGID)
        .map_err(|e| format!("写入扩展名指向失败：{e}"))?;
    tracing::info!(".wpkg 文件关联已注册：{want}");
    Ok(())
}

/// 解除 `.wpkg` 关联：删 `.wpkg` 键 + 本程序的 ProgId 树。
///
/// 键本就不存在时视作成功 —— 用户要的是"最终没有关联"这个结果。
/// 即便 `.wpkg` 当前指向他人的 ProgId 也只删这两处（用户已在确认框里同意
/// 解除关联本身），他人的 ProgId 树留着：它可能还服务别的扩展名。
#[cfg(windows)]
pub fn unregister() -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::*;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for key in [EXT_KEY, PROGID_KEY] {
        match hkcu.delete_subkey_all(key) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("删除注册表项失败：{e}")),
        }
    }
    tracing::info!(".wpkg 文件关联已解除");
    Ok(())
}

/// 启动时自愈 `.wpkg` 关联（仅 Windows）。
///
/// 调用前须先过 [`crate::protocol::should_self_heal_on_startup`] 的门控
/// （便携版恒不自愈、用户取消后不自愈，门控字段是 `file_assoc.self_heal`）。
/// 收回规则与协议一致：只收**残留**（指向的 exe 已不存在），
/// 活着的他人关联保持原状，留给高级页展示 + 用户确认后覆盖。
#[cfg(windows)]
pub fn self_heal_registration() {
    match current_registration() {
        Registration::Ours => {}
        Registration::Other(cur) => {
            if crate::protocol::is_stale_registration(&cur, |p| std::path::Path::new(p).exists()) {
                tracing::info!(".wpkg 关联原指向已不存在的 {cur}，自愈收回");
                if let Err(e) = register() {
                    tracing::warn!(".wpkg 关联自愈失败：{e}");
                }
            } else {
                tracing::info!(".wpkg 关联由 {cur} 处理且该程序仍在，启动时不接管");
            }
        }
        Registration::Unregistered => {
            if let Err(e) = register() {
                tracing::warn!(".wpkg 关联注册失败：{e}");
            }
        }
    }
}

/// 非 Windows：文件关联随 .app 的 Info.plist 声明，无运行时代码。
#[cfg(not(windows))]
pub fn self_heal_registration() {}

#[cfg(not(windows))]
pub fn current_registration() -> Registration {
    Registration::Unregistered
}

#[cfg(not(windows))]
pub fn register() -> Result<(), String> {
    Err("本平台的文件关联由应用包声明，无需手动注册".to_string())
}

#[cfg(not(windows))]
pub fn unregister() -> Result<(), String> {
    Err("本平台的文件关联由应用包声明，无法在此解除".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::command_for_exe;

    /// `.wpkg` 键缺失或空值都是未关联 —— 双击不会打开任何程序，界面要给「注册」按钮。
    #[test]
    fn missing_or_blank_ext_progid_is_unregistered() {
        let want = command_for_exe(r"C:\app\wind_setting.exe");
        assert_eq!(
            classify_assoc(None, None, &want),
            Registration::Unregistered
        );
        assert_eq!(
            classify_assoc(Some(""), None, &want),
            Registration::Unregistered
        );
        assert_eq!(
            classify_assoc(Some("  "), None, &want),
            Registration::Unregistered
        );
    }

    /// 指向本 ProgId 时归属由打开命令决定，且沿用协议的大小写不敏感比较 ——
    /// 按字节比会把自己的注册误判成他人的，每次启动都"自愈"一遍。
    #[test]
    fn own_progid_ownership_follows_command() {
        let want = command_for_exe(r"C:\Program Files\WindInput\wind_setting.exe");
        let same_case_diff = command_for_exe(r"c:\program files\windinput\WIND_SETTING.EXE");
        assert_eq!(
            classify_assoc(Some(PROGID), Some(&same_case_diff), &want),
            Registration::Ours
        );
        // ProgId 名大小写不同也算本程序的（注册表键名不区分大小写）。
        assert_eq!(
            classify_assoc(Some("windinput.package"), Some(&want), &want),
            Registration::Ours
        );
        // ProgId 是本程序的但命令指向旧安装路径 → 他人（残留），自愈据此收回。
        let old = command_for_exe(r"D:\old\wind_setting.exe");
        assert_eq!(
            classify_assoc(Some(PROGID), Some(&old), &want),
            Registration::Other(old.clone())
        );
        // ProgId 在但命令为空 → 未关联（双击没反应，界面给「注册」）。
        assert_eq!(
            classify_assoc(Some(PROGID), None, &want),
            Registration::Unregistered
        );
    }

    /// 他人 ProgId：展示优先给命令行（用户凭它分辨占用者）；命令读不到时给 ProgId 名。
    /// 占位串按命令行语法解析出的"路径"（`(ProgId:`）在文件系统里必然不存在 ——
    /// 用自愈同款的存在性判据即按残留处理，这种关联本就唤不起任何程序，收回无争议。
    #[test]
    fn foreign_progid_prefers_command_for_display() {
        let want = command_for_exe(r"C:\app\wind_setting.exe");
        let their = command_for_exe(r"C:\other\tool.exe");
        assert_eq!(
            classify_assoc(Some("SomeTool.Package"), Some(&their), &want),
            Registration::Other(their.clone())
        );
        let no_cmd = classify_assoc(Some("SomeTool.Package"), None, &want);
        let Registration::Other(desc) = &no_cmd else {
            panic!("指向他人 ProgId 必须是 Other：{no_cmd:?}");
        };
        assert!(desc.contains("SomeTool.Package"), "{desc}");
        assert!(
            crate::protocol::is_stale_registration(desc, |p| std::path::Path::new(p).exists()),
            "无打开命令的关联唤不起任何程序，应按残留处理"
        );
    }

    /// `DefaultIcon` 的值形状：引号包路径 + **负数**资源 ID。
    ///
    /// 负号钉死在这里：正数是按图标在资源表里的位置序号取，build.rs 里图标的
    /// 添加顺序一变就指到主图标去了——那正是本功能要消除的「文件和程序同一个图标」。
    #[test]
    fn icon_value_uses_negative_resource_id() {
        let v = icon_value_for_exe(r"C:\Program Files\WindInput\wind_setting.exe");
        assert_eq!(v, r#""C:\Program Files\WindInput\wind_setting.exe",-2"#);
        assert!(
            v.ends_with(",-2"),
            "必须按资源 ID 取（负号），不是位置序号：{v}"
        );
        assert!(
            v.starts_with('"') && v[1..].contains('"'),
            "含空格的路径必须整体加引号：{v}"
        );
    }
}
