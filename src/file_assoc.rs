//! 本程序各类包文件的关联（HKCU，用户级，不需管理员权限）：方案包 `.wpkg` 与主题包
//! `.wtheme`。
//!
//! 与 [`crate::protocol`] 的 `windinput://` 注册同一套规则与门控：便携版恒不注册、
//! 用户取消后不自愈、指向已消失 exe 的残留才在启动时静默收回。三态判定
//! （[`Registration`]）、命令行构造与残留判断直接复用 protocol 的纯函数 ——
//! 它们本就与「协议还是扩展名」无关。
//!
//! 差异只在注册表形状：扩展名关联是**两级**间接（扩展名 → ProgId → 打开命令），
//! 而协议是一级。于是"归谁"要看两跳（见 [`classify_assoc`]），解除时也只动扩展名键
//! 与**本程序的** ProgId 树 —— 别人的 ProgId 可能同时服务其它扩展名，删它是越权。
//!
//! ## 每个扩展名一棵自己的 ProgId 树
//!
//! 不让两个扩展名共用一个 ProgId：ProgId 是「类型」的载体，资源管理器的类型名与图标
//! 都挂在它下面。共用的话，`.wpkg` 和 `.wtheme` 在文件列表里会长得一模一样、类型列也
//! 是同一行字，用户分不出手上是方案还是主题——而这正是给主题包单开一个后缀的初衷。
//!
//! Linux 上不走注册表：`registration_of` / `register_one` / `unregister_one` 转给
//! [`crate::xdg_assoc`]（XDG 的 `.desktop` + MIME 类型 + `mimeapps.list`）。

use crate::protocol::Registration;
#[cfg(any(windows, test))]
use crate::protocol::classify;

/// 一种文件关联的全部身份信息。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Assoc {
    /// 带点的扩展名，如 `.wpkg`。
    pub ext: &'static str,
    /// 本程序为该扩展名建的 ProgId。**每个扩展名一个**，见模块头。
    pub progid: &'static str,
    /// 资源管理器「类型」列显示的名字。
    pub type_name: &'static str,
    /// 设置页那行的短名（「双击导入{短名}」）。与 `type_name` 分开：那个要在资源管理器
    /// 里独立成立（得带品牌名），这个长在一句话里，带上就啰嗦了。
    pub short_name: &'static str,
    /// `DefaultIcon` 用的资源 ID（由 build.rs 显式钉死）。
    pub icon_id: i32,
}

/// 方案包。
pub const PACKAGE: Assoc = Assoc {
    ext: ".wpkg",
    progid: "WindInput.Package",
    type_name: "清风输入法方案包",
    short_name: "方案包",
    icon_id: 2,
};

/// 主题包。
///
/// 图标暂与方案包共用一个资源（build.rs 里只钉了主图标 1 与方案包 2）：两种包在
/// 资源管理器里的**类型名**已经分开，图标要等有了主题包的素材再单独钉一个 ID。
/// 指向不存在的资源 ID 会让图标直接空掉，比暂时共用更糟。
pub const THEME: Assoc = Assoc {
    ext: ".wtheme",
    progid: "WindInput.Theme",
    type_name: "清风输入法主题包",
    short_name: "主题包",
    icon_id: 2,
};

/// 本程序登记的全部关联。注册/解除/自愈都按这张表来，新增一种包只改这里。
pub const ALL: [Assoc; 2] = [PACKAGE, THEME];

impl Assoc {
    /// 扩展名键（HKCU 下的用户级关联）。
    #[cfg(windows)]
    fn ext_key(&self) -> String {
        format!(r"Software\Classes\{}", self.ext)
    }
    /// ProgId 根键。
    #[cfg(windows)]
    fn progid_key(&self) -> String {
        format!(r"Software\Classes\{}", self.progid)
    }
    /// 打开命令子键。
    #[cfg(windows)]
    fn command_key(&self) -> String {
        format!(r"Software\Classes\{}\shell\open\command", self.progid)
    }
    /// 关联图标子键。缺它时资源管理器回落到打开命令那个 exe 的**主图标**——
    /// 于是包文件和设置程序长得一模一样，看图标分不出哪个是数据文件。
    #[cfg(windows)]
    fn icon_key(&self) -> String {
        format!(r"Software\Classes\{}\DefaultIcon", self.progid)
    }
}

/// 纯判定：两跳读到的值 → 三态。抽出以便单测。
///
/// - `.wpkg` 没指向任何 ProgId（键缺失/空值）= 未关联，双击不会打开任何程序；
/// - 指向本 ProgId 时，归属由打开命令决定（复用 [`classify`]，含旧安装路径的
///   残留识别与大小写不敏感比较）；
/// - 指向别的 ProgId 时是他人关联。展示优先给那个 ProgId 的打开命令 ——
///   用户凭它分辨"别的软件"还是"自己另一份安装"；命令读不到时给 ProgId 名，
///   这种关联本身就是死的（双击唤不起程序），自愈会按残留收回。
#[cfg(any(windows, test))]
pub fn classify_assoc(
    ext_progid: Option<&str>,
    progid_cmd: Option<&str>,
    want: &str,
    ours: &str,
) -> Registration {
    match ext_progid {
        None => Registration::Unregistered,
        Some(p) if p.trim().is_empty() => Registration::Unregistered,
        Some(p) if p.eq_ignore_ascii_case(ours) => classify(progid_cmd, want),
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
#[cfg(any(windows, test))]
pub fn icon_value_for_exe(exe: &str, icon_id: i32) -> String {
    format!("\"{exe}\",-{icon_id}")
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

/// 读取**某一个**扩展名的关联状态（两跳现读注册表，不缓存）。
#[cfg(windows)]
pub fn registration_of(a: &Assoc) -> Registration {
    let Ok(exe) = std::env::current_exe() else {
        return Registration::Unregistered;
    };
    let want = crate::protocol::command_for_exe(&exe.to_string_lossy());
    let ext_progid = read_default(&a.ext_key());
    let cmd = ext_progid
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .and_then(|p| read_default(&format!(r"Software\Classes\{p}\shell\open\command")));
    classify_assoc(ext_progid.as_deref(), cmd.as_deref(), &want, a.progid)
}

/// 通知 Shell「文件关联变了」。
///
/// **写完注册表不等于生效**：资源管理器缓存着关联与图标，不广播这条通知的话，
/// 用户重新注册、甚至跑 `ie4uinit -ClearIconCache` 都看不到变化（那个只清图标缓存，
/// 不重读关联）——只能等重启或重新登录。微软对写关联的程序明确要求发这条。
///
/// 只声明用得到的这一个导出，不为一次调用引入 `windows-sys`：shell32 是系统必备库。
#[cfg(windows)]
pub fn notify_shell_assoc_changed() {
    const SHCNE_ASSOCCHANGED: i32 = 0x0800_0000;
    const SHCNF_IDLIST: u32 = 0x0000;
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHChangeNotify(
            event_id: i32,
            flags: u32,
            item1: *const std::ffi::c_void,
            item2: *const std::ffi::c_void,
        );
    }
    // 安全性：四个参数都按文档取值，两个 item 传 NULL 表示「全局关联变更」，
    // 无内存所有权转移。
    unsafe {
        SHChangeNotify(
            SHCNE_ASSOCCHANGED,
            SHCNF_IDLIST,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
}

/// 注册（或修复）`.wpkg` 关联，指向本 exe。已被他人关联时**直接覆盖** `.wpkg` 的
/// 指向 —— 调用方负责在覆盖前征得用户同意（见高级页「系统集成」卡片）。
/// 他人的 ProgId 树不动：覆盖指向已足够，删树是越权。
#[cfg(windows)]
pub fn register_one(a: &Assoc) -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::*;

    let exe = std::env::current_exe().map_err(|e| format!("无法定位自身路径：{e}"))?;
    let want = crate::protocol::command_for_exe(&exe.to_string_lossy());
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (root, _) = hkcu
        .create_subkey(a.progid_key())
        .map_err(|e| format!("创建 ProgId 项失败：{e}"))?;
    root.set_value("", &a.type_name)
        .map_err(|e| format!("写入类型描述失败：{e}"))?;
    let (cmd, _) = hkcu
        .create_subkey(a.command_key())
        .map_err(|e| format!("创建命令行项失败：{e}"))?;
    cmd.set_value("", &want)
        .map_err(|e| format!("写入命令行失败：{e}"))?;
    let (icon, _) = hkcu
        .create_subkey(a.icon_key())
        .map_err(|e| format!("创建图标项失败：{e}"))?;
    icon.set_value("", &icon_value_for_exe(&exe.to_string_lossy(), a.icon_id))
        .map_err(|e| format!("写入关联图标失败：{e}"))?;
    let (ext, _) = hkcu
        .create_subkey(a.ext_key())
        .map_err(|e| format!("创建扩展名项失败：{e}"))?;
    ext.set_value("", &a.progid)
        .map_err(|e| format!("写入扩展名指向失败：{e}"))?;
    tracing::info!("{} 文件关联已注册：{want}", a.ext);
    Ok(())
}

/// 解除 `.wpkg` 关联：删 `.wpkg` 键 + 本程序的 ProgId 树。
///
/// 键本就不存在时视作成功 —— 用户要的是"最终没有关联"这个结果。
/// 即便 `.wpkg` 当前指向他人的 ProgId 也只删这两处（用户已在确认框里同意
/// 解除关联本身），他人的 ProgId 树留着：它可能还服务别的扩展名。
#[cfg(windows)]
pub fn unregister_one(a: &Assoc) -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::*;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for key in [a.ext_key(), a.progid_key()] {
        match hkcu.delete_subkey_all(&key) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("删除注册表项失败：{e}")),
        }
    }
    tracing::info!("{} 文件关联已解除", a.ext);
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
    // 逐个判、逐个修：两个扩展名的归属可以不同（比如别的软件只抢了 .wpkg），
    // 按聚合状态一刀切会把「该接管的那个」和「该让出的那个」混为一谈。
    let mut changed = false;
    for a in &ALL {
        match registration_of(a) {
            Registration::Ours => {}
            Registration::Other(cur) => {
                if crate::protocol::is_stale_registration(&cur, |p| {
                    std::path::Path::new(p).exists()
                }) {
                    tracing::info!("{} 关联原指向已不存在的 {cur}，自愈收回", a.ext);
                    match register_one(a) {
                        Ok(()) => changed = true,
                        Err(e) => tracing::warn!("{} 关联自愈失败：{e}", a.ext),
                    }
                } else {
                    tracing::info!("{} 关联由 {cur} 处理且该程序仍在，启动时不接管", a.ext);
                }
            }
            Registration::Unregistered => match register_one(a) {
                Ok(()) => changed = true,
                Err(e) => tracing::warn!("{} 关联注册失败：{e}", a.ext),
            },
        }
    }
    // 只在真动过注册表时才广播，免得每次启动都让资源管理器白刷一遍。
    if changed {
        notify_shell_assoc_changed();
    }
}

/// 某个扩展名关联的事实（每次现查）。
#[cfg(not(target_os = "linux"))]
pub fn facts_of(a: &Assoc) -> crate::protocol::Facts {
    crate::protocol::Facts::from_registration(registration_of(a))
}

// ── Linux：XDG 关联（语义见 crate::xdg_assoc）。两个扩展名各对一个 MIME 类型，
//    共用一个 windinput-import.desktop，但注册/取消各管各的类型。

#[cfg(target_os = "linux")]
pub fn facts_of(a: &Assoc) -> crate::protocol::Facts {
    crate::xdg_assoc::facts(&crate::xdg_assoc::target_for_ext(a.ext))
}

#[cfg(target_os = "linux")]
pub fn registration_of(a: &Assoc) -> Registration {
    facts_of(a).reg
}

#[cfg(target_os = "linux")]
pub fn register_one(a: &Assoc) -> Result<(), String> {
    crate::xdg_assoc::register(&crate::xdg_assoc::target_for_ext(a.ext))
}

#[cfg(target_os = "linux")]
pub fn unregister_one(a: &Assoc) -> Result<(), String> {
    crate::xdg_assoc::unregister(&crate::xdg_assoc::target_for_ext(a.ext))
}

#[cfg(target_os = "linux")]
pub fn self_heal_registration() {
    crate::xdg_assoc::self_heal(&[crate::xdg_assoc::PACKAGE, crate::xdg_assoc::THEME]);
}

/// Linux：`.desktop` 改动后已由 update-desktop-database 刷新，这里没有别的缓存要通知。
#[cfg(target_os = "linux")]
pub fn notify_shell_assoc_changed() {}

/// macOS：文件关联随 .app 的 Info.plist 声明，无运行时代码。
#[cfg(not(any(windows, target_os = "linux")))]
pub fn self_heal_registration() {}

/// macOS：关联由 .app 的 Info.plist 声明，运行时无从查证，一律报未注册。
#[cfg(not(any(windows, target_os = "linux")))]
pub fn registration_of(_a: &Assoc) -> Registration {
    Registration::Unregistered
}

// ★ 下面三个存根一个都不能少。本模块的 Windows 侧函数全部 `#[cfg(windows)]`，而
//   `state.rs` 的 register_file_assoc / unregister_file_assoc 是**无条件**调用它们的
//   （设置界面那两个按钮在哪个平台都在），少一个 macOS 就直接编译不过 —— 且这个错
//   只有 macOS runner 报得出来，Windows 与 Linux 上的 check 全绿。
//   同一套存根 `protocol.rs` 早就备齐了（它的 register / unregister / current_registration
//   都有 not(windows) 版），本模块是照着它写的，补的时候漏了这三个。

#[cfg(not(any(windows, target_os = "linux")))]
pub fn register_one(_a: &Assoc) -> Result<(), String> {
    Err("本平台的文件关联由应用包声明，无需手动注册".to_string())
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn unregister_one(_a: &Assoc) -> Result<(), String> {
    Err("本平台的文件关联由应用包声明，无法在此解除".to_string())
}

/// macOS：没有需要通知的外壳关联缓存，空实现。
///
/// ⚠️ 它只在 register_one / unregister_one 返回 `Ok` 之后才会被调到，而本平台那两个恒
///    返回 `Err` —— 所以这个函数实际不会执行，存在的意义只是让调用处编译得过。
#[cfg(not(any(windows, target_os = "linux")))]
pub fn notify_shell_assoc_changed() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::command_for_exe;

    /// 两个扩展名各有一棵自己的 ProgId 树：共用一个的话，资源管理器里两种包的类型名
    /// 和图标都会一样，用户分不出手上是方案还是主题——那正是给主题单开后缀的初衷。
    #[test]
    fn each_extension_has_its_own_progid() {
        assert_ne!(PACKAGE.progid, THEME.progid);
        assert_ne!(PACKAGE.ext, THEME.ext);
        assert_ne!(PACKAGE.type_name, THEME.type_name);
        // 设置页每个扩展名一行，短名是那行唯一的区分点。
        assert!(!PACKAGE.short_name.is_empty() && !THEME.short_name.is_empty());
        // 表里不能有重复项，否则注册会互相覆盖。
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(a.ext, b.ext, "扩展名重复");
                assert_ne!(a.progid, b.progid, "ProgId 重复");
                assert_ne!(a.short_name, b.short_name, "设置页那行的短名重复");
            }
        }
    }

    /// `.wpkg` 键缺失或空值都是未关联 —— 双击不会打开任何程序，界面要给「注册」按钮。
    #[test]
    fn missing_or_blank_ext_progid_is_unregistered() {
        let want = command_for_exe(r"C:\app\wind_setting.exe");
        assert_eq!(
            classify_assoc(None, None, &want, PACKAGE.progid),
            Registration::Unregistered
        );
        assert_eq!(
            classify_assoc(Some(""), None, &want, PACKAGE.progid),
            Registration::Unregistered
        );
        assert_eq!(
            classify_assoc(Some("  "), None, &want, PACKAGE.progid),
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
            classify_assoc(
                Some(PACKAGE.progid),
                Some(&same_case_diff),
                &want,
                PACKAGE.progid
            ),
            Registration::Ours
        );
        // ProgId 名大小写不同也算本程序的（注册表键名不区分大小写）。
        assert_eq!(
            classify_assoc(
                Some("windinput.package"),
                Some(&want),
                &want,
                PACKAGE.progid
            ),
            Registration::Ours
        );
        // ProgId 是本程序的但命令指向旧安装路径 → 他人（残留），自愈据此收回。
        let old = command_for_exe(r"D:\old\wind_setting.exe");
        assert_eq!(
            classify_assoc(Some(PACKAGE.progid), Some(&old), &want, PACKAGE.progid),
            Registration::Other(old.clone())
        );
        // ProgId 在但命令为空 → 未关联（双击没反应，界面给「注册」）。
        assert_eq!(
            classify_assoc(Some(PACKAGE.progid), None, &want, PACKAGE.progid),
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
            classify_assoc(
                Some("SomeTool.Package"),
                Some(&their),
                &want,
                PACKAGE.progid
            ),
            Registration::Other(their.clone())
        );
        let no_cmd = classify_assoc(Some("SomeTool.Package"), None, &want, PACKAGE.progid);
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
        let v = icon_value_for_exe(r"C:\Program Files\WindInput\wind_setting.exe", 2);
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
