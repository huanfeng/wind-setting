// 为 wind_setting 注入版本号（全平台），并为 .exe 嵌入 Windows 资源：
// 图标 / 版本信息 / manifest（DPI 感知，仅 Windows 目标）。
// 参考 core 的 wind_input/apps/service/build.rs。
//
// 版本号取自 git（最近 tag + 短 hash），非 git 环境回退 CARGO_PKG_VERSION。
// 下方 git_version / git / parse_xyz 与本项目无耦合，WindInput 可直接照搬
// （仅产品名、图标路径不同）。
//
// ⚠️ `WIND_SETTING_VERSION` 的注入**必须在平台门控之外**：应用代码（src/shell.rs /
// src/pages/mod.rs / src/update/version.rs）用的是 `env!`，编译期缺它直接 E0080 编译失败，
// 不是"少个版本号"而是整个 crate 编不过。此前 main() 开头对非 Windows 目标一律早退，
// 连带把这行也跳过了 —— macOS 上 `scripts/mac/dev.sh 1` 因此始终构建不出设置程序。

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // HEAD 变动（提交 / 切分支）时重新注入版本。打 tag 属低频操作，必要时 cargo clean。
    println!("cargo:rerun-if-changed=.git/HEAD");

    // 版本优先级: WIND_APP_VERSION (WindInput 打包脚本/CI 从 docs/VERSION 注入) > git tag > CARGO_PKG_VERSION。
    // 打包时与 wind_input / wind_tsf / 安装包统一走 docs/VERSION; 独立开发时回退 git。
    println!("cargo:rerun-if-env-changed=WIND_APP_VERSION");
    let (ver_str, ver_u64) = match std::env::var("WIND_APP_VERSION") {
        Ok(v) if !v.trim().is_empty() => {
            let v = v.trim().to_string();
            let (maj, min, pat) = parse_xyz(&v);
            (v, (maj << 48) | (min << 32) | (pat << 16))
        }
        _ => git_version(),
    };

    // 导出给应用代码（标题栏版本号显示）：形如 X.Y.Z 开头的加 v 前缀；
    // 纯 hash（无 tag，可能以数字开头，如 "55bb398.dirty"）原样。
    let is_semver = {
        let mut segs = ver_str.split(['.', '+']);
        let num = |s: Option<&str>| {
            s.is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        };
        num(segs.next()) && num(segs.next())
    };
    let display_ver = if is_semver {
        format!("v{ver_str}")
    } else {
        ver_str.clone()
    };
    println!("cargo:rustc-env=WIND_SETTING_VERSION={display_ver}");

    // ── 以下仅 Windows 目标：图标 / 版本资源 / manifest 嵌入 ──
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest_dir.join("res").join("wind_setting.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    // `.wpkg` 关联图标。单独一张而不是复用主图标：资源管理器里要一眼分出
    // 「这是数据文件」还是「这是程序」，同一张图做不到（见 file_assoc 的 DefaultIcon）。
    let wpkg_icon = manifest_dir.join("res").join("wpkg.ico");
    println!("cargo:rerun-if-changed={}", wpkg_icon.display());

    // manifest：asInvoker + PerMonitorV2 DPI 感知 + win10 + 长路径（对齐 core）。
    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <assemblyIdentity type="win32" name="com.windinput.setting" version="1.0.0.0"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <asmv3:application>
    <asmv3:windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2, PerMonitor</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </asmv3:windowsSettings>
  </asmv3:application>
</assembly>
"#;

    let mut res = winresource::WindowsResource::new();
    // 主图标恒为资源 ID 1：exe 自身在资源管理器里显示的是**最小 ID** 的那个图标，
    // 顺序写反会让程序图标变成文件图标。
    res.set_icon_with_id(icon.to_str().expect("icon 路径含非 UTF-8"), "1");
    // ID 2 供 `.wpkg` 关联的 DefaultIcon 引用（注册表写 `<exe>,-2`，负号 = 按资源 ID
    // 而非位置序号取；位置序号会随资源表顺序变动而错位）。
    res.set_icon_with_id(wpkg_icon.to_str().expect("wpkg icon 路径含非 UTF-8"), "2");
    res.set("ProductName", "清风输入法");
    res.set("CompanyName", "清风输入法");
    res.set("FileDescription", "清风输入法设置");
    res.set("InternalName", "wind_setting");
    res.set("OriginalFilename", "wind_setting.exe");
    res.set("LegalCopyright", "Copyright © 清风输入法");
    res.set("ProductVersion", &ver_str);
    res.set("FileVersion", &ver_str);
    res.set_version_info(winresource::VersionInfo::FILEVERSION, ver_u64);
    res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, ver_u64);
    res.set_manifest(manifest);

    if let Err(e) = res.compile() {
        // 不致命：缺资源不影响功能，仅图标/版本元数据缺失。
        println!("cargo:warning=Windows 资源嵌入失败: {e}");
    }
}

/// 从 git 生成 (字符串版本, 数值版本 u64)。
/// 字符串：tag 上=`X.Y.Z`；tag 后有提交=`X.Y.Z+<hash>`；无 tag=`<hash>`；工作区脏追加 `.dirty`。
/// 数值（FIXEDFILEINFO 4×u16）：最近 tag 的 X.Y.Z + 第 4 段=tag 后提交数（无 tag → 0.0.0.0）。
/// 非 git 环境（无 git / 非仓库）回退 CARGO_PKG_VERSION。
fn git_version() -> (String, u64) {
    let short = match git(&["rev-parse", "--short", "HEAD"]) {
        Some(s) if !s.is_empty() => s,
        _ => return fallback(),
    };
    let dirty = git(&["status", "--porcelain"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    let (base, num) = match git(&["describe", "--tags", "--abbrev=0"]) {
        Some(tag) if !tag.is_empty() => {
            let count = git(&["rev-list", &format!("{tag}..HEAD"), "--count"])
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let (maj, min, pat) = parse_xyz(tag.trim_start_matches('v'));
            let num = (maj << 48) | (min << 32) | (pat << 16) | (count & 0xffff);
            let base = if count == 0 {
                format!("{maj}.{min}.{pat}")
            } else {
                format!("{maj}.{min}.{pat}+{short}")
            };
            (base, num)
        }
        _ => (short, 0u64),
    };

    let version = if dirty { format!("{base}.dirty") } else { base };
    (version, num)
}

/// 非 git 环境回退：CARGO_PKG_VERSION 文本 + 其 X.Y.Z 数值。
fn fallback() -> (String, u64) {
    let v = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let (maj, min, pat) = parse_xyz(&v);
    (v, (maj << 48) | (min << 32) | (pat << 16))
}

/// 运行 git 命令取 trim 后的 stdout；失败 / 无 git 返回 None。
/// CWD 为 build.rs 所在 package 目录（cargo 保证），即仓库内。
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 取开头 X.Y.Z 三段（忽略 `-alpha` / `-test` 等 prerelease 后缀）。
fn parse_xyz(s: &str) -> (u64, u64, u64) {
    let core = s.split('-').next().unwrap_or(s);
    let mut it = core
        .split('.')
        .map(|p| p.trim().parse::<u64>().unwrap_or(0));
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}
