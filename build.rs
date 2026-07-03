// 为 wind_setting.exe 嵌入 Windows 资源：图标 / 版本信息 / manifest（DPI 感知）。
// 参考 core 的 wind_input/apps/service/build.rs。仅 Windows 目标生效，其它目标空操作。
//
// 版本号取自 git（最近 tag + 短 hash），非 git 环境回退 CARGO_PKG_VERSION。
// 下方 git_version / git / parse_xyz 与本项目无耦合，WindInput 可直接照搬
// （仅产品名、图标路径不同）。

use std::path::PathBuf;
use std::process::Command;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest_dir.join("res").join("wind_setting.ico");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", icon.display());
    // HEAD 变动（提交 / 切分支）时重新注入版本。打 tag 属低频操作，必要时 cargo clean。
    println!("cargo:rerun-if-changed=.git/HEAD");

    let (ver_str, ver_u64) = git_version();

    // 导出给应用代码（标题栏版本号显示）：形如 X.Y.Z 开头的加 v 前缀；
    // 纯 hash（无 tag，可能以数字开头，如 "55bb398.dirty"）原样。
    let is_semver = {
        let mut segs = ver_str.split(['.', '+']);
        let num = |s: Option<&str>| s.is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
        num(segs.next()) && num(segs.next())
    };
    let display_ver = if is_semver { format!("v{ver_str}") } else { ver_str.clone() };
    println!("cargo:rustc-env=WIND_SETTING_VERSION={display_ver}");

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
    res.set_icon(icon.to_str().expect("icon 路径含非 UTF-8"));
    res.set("ProductName", "清风输入法");
    res.set("CompanyName", "清风输入法");
    res.set("FileDescription", "清风输入法设置");
    res.set("InternalName", "wind_setting");
    res.set("OriginalFilename", "wind_setting.exe");
    res.set("LegalCopyright", "Copyright © 2026 清风输入法");
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
    let mut it = core.split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0));
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}
