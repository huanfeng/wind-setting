//! 日志打包 / 清理，以及「打开目录」三个入口（设置 › 高级 › 日志）。
//!
//! 日志目录只有一个（[`crate::tsf_log::log_dir`]）：core、设置程序、TSF（`tsf_log\` 子目录）
//! 都写在这里，所以打包与清理都只管这一棵树。
//!
//! # 清理：按文件名决定截断还是删除
//!
//! 清理的典型用法是「清空 → 复现 → 打包」。正在写的日志若被删掉，写端攥着的句柄会继续
//! 往已经没有名字的文件里写（Unix 一律如此，Windows 10 起 `DeleteFile` 也是 POSIX 语义），
//! 复现期间的日志就全丢了，直到进程重启。而各端都是**追加模式**打开（core 的 file-rotate、
//! 设置程序的 tracing-appender、TSF 的 `FILE_APPEND_DATA`），截成 0 字节后新内容从头续写。
//! 规则见 [`clear_action`]，两条要点：
//!
//! - ⚠️ **core 的 `wind_input.*.log` 一个都不能删，历史的也不行。** file-rotate 只在启动时
//!   扫一次磁盘记下有哪些序号，轮转时按记忆逐个后挪并 `assert!(old_path.exists())`；
//!   删掉 `.2.log` 之后的下一次轮转就会 panic，而 core release 是 `panic = "abort"`——
//!   整个输入法服务崩掉。截断保留文件名，轮转照常。
//! - 不靠「试着独占打开」去探谁在用：探测那一瞬间的独占句柄会让恰好在轮转重开的写端
//!   打不开文件，TSF 那边会因此停摆到宿主重启。按名字认，零副作用。
//!
//! `tsf_log_config` 不是日志而是 TSF 的日志开关，清理时保留——删了它 TSF 日志就关了，
//! 用户清完去复现会发现什么都没记。

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// TSF 的日志开关文件，与 `tsf_log::CONFIG_NAME` 同名。清理时保留。
const KEEP_ON_CLEAR: &str = "tsf_log_config";

/// 日志目录下的日志文件数与总字节数（递归，不计 `tsf_log_config`）。目录不存在视为空。
pub fn summarize(log_dir: &Path) -> (usize, u64) {
    let files: Vec<_> = list_files(log_dir)
        .into_iter()
        .filter(|p| p.file_name().is_none_or(|n| n != KEEP_ON_CLEAR))
        .collect();
    let bytes = files
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    (files.len(), bytes)
}

/// 人看的大小：`1.2 MB` / `850 KB`。
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB {
        format!("{:.1} MB", b / KB / KB)
    } else {
        format!("{:.0} KB", (b / KB).ceil())
    }
}

/// 打包落点。
///
/// - Windows / macOS：桌面。`dirs` 在 Windows 上走 `FOLDERID_Desktop`，桌面被 OneDrive
///   重定向时也能拿到真实位置。
/// - Linux：桌面不一定存在（xdg-user-dirs 没配、平铺窗管），存在了也不一定看得见（GNOME
///   默认不显示桌面图标），故依次退到「下载」、主目录。打包后一律在文件管理器里选中
///   那个文件，不靠用户自己去找。
pub fn archive_dest_dir() -> Option<PathBuf> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        dirs::desktop_dir().or_else(dirs::home_dir)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        [dirs::desktop_dir(), dirs::download_dir()]
            .into_iter()
            .flatten()
            .find(|d| d.is_dir())
            .or_else(dirs::home_dir)
    }
}

/// 「打包」按钮上的字。Linux 上落点未必是桌面，不能许诺。
pub fn archive_button_label() -> &'static str {
    if cfg!(any(windows, target_os = "macos")) {
        "打包到桌面"
    } else {
        "打包"
    }
}

/// 把 `log_dir` 整棵树打成 `<dest_dir>/<应用名>-logs-<时间>.zip`，返回 zip 路径。
///
/// 读不了的文件（极少见：被别的程序独占）跳过，不让一个文件拖垮整个包；但一个都没打进去
/// 就算失败——空包发出去只会让维护者白等一轮。
pub fn pack(
    log_dir: &Path,
    dest_dir: &Path,
    app_name: &str,
    stamp: &str,
) -> Result<PathBuf, String> {
    let files = list_files(log_dir);
    if files.is_empty() {
        return Err("日志目录里没有文件".to_string());
    }
    let dest = unique_path(dest_dir, &format!("{app_name}-logs-{stamp}"), "zip");
    let out = File::create(&dest).map_err(|e| format!("无法创建 {}：{e}", dest.display()))?;
    let res = write_zip(out, log_dir, &files);
    // 写到一半失败（磁盘满等）或一个都没打进去：别在桌面上留一个打不开的半截包。
    match res {
        Ok(0) => {
            let _ = std::fs::remove_file(&dest);
            Err("日志文件都无法读取".to_string())
        }
        Ok(_) => Ok(dest),
        Err(e) => {
            let _ = std::fs::remove_file(&dest);
            Err(e)
        }
    }
}

/// 写 zip，返回实际打进去的文件数。
fn write_zip(out: File, log_dir: &Path, files: &[PathBuf]) -> Result<usize, String> {
    let mut zip = zip::ZipWriter::new(out);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        // 日志是纯文本，最快一档已能压到一成左右；高档位换不来多少体积，却让大包多卡几秒。
        .compression_level(Some(1));
    let mut packed = 0usize;
    let mut buf = Vec::new();
    for path in files {
        buf.clear();
        // 先整读再写入：读失败时 zip 里不会留下一个半截的条目。
        if let Err(e) = File::open(path).and_then(|mut f| f.read_to_end(&mut buf)) {
            tracing::warn!("打包日志：跳过 {}：{e}", path.display());
            continue;
        }
        let name = entry_name(log_dir, path);
        let opts = match mtime_of(path) {
            Some(t) => opts.last_modified_time(t),
            None => opts,
        };
        zip.start_file(name, opts)
            .and_then(|_| zip.write_all(&buf).map_err(Into::into))
            .map_err(|e| format!("写入压缩包失败：{e}"))?;
        packed += 1;
    }
    zip.finish().map_err(|e| format!("写入压缩包失败：{e}"))?;
    Ok(packed)
}

/// 后台打包的结果，经 `AppState::log_pack_tx` 回 UI 线程。
pub type PackResult = Result<PathBuf, String>;

/// 在后台线程打包。日志多时上百 MB，放在 UI 线程会让窗口卡住、被系统标成「未响应」。
pub fn spawn_pack(tx: windui::prelude::Sender<PackResult>) {
    std::thread::spawn(move || {
        let mode = crate::mode::get();
        let res = match (crate::tsf_log::log_dir(mode), archive_dest_dir()) {
            (Some(logs), Some(dest)) => {
                let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
                pack(&logs, &dest, crate::mode::app_dir_name_of(mode), &stamp)
            }
            _ => Err("无法定位日志目录或保存位置".to_string()),
        };
        let _ = tx.send(res);
    });
}

/// 打包结果回到 UI 线程后：成功就在文件管理器里选中压缩包，并提示路径。
pub fn on_pack_done(ctx: &mut windui::core::EventCtx, res: PackResult) {
    match res {
        Ok(zip) => {
            tracing::info!("日志已打包: {}", zip.display());
            if let Err(e) = reveal(&zip) {
                tracing::warn!("在文件管理器中显示压缩包失败: {e}");
            }
            ctx.toast_ok(format!("已保存到 {}", zip.display()));
        }
        Err(e) => {
            tracing::warn!("打包日志失败: {e}");
            ctx.toast_err(format!("打包失败：{e}"));
        }
    }
}

/// 文件修改时间（本地时区）→ zip 条目时间。不带的话解压出来全是 1980 年，
/// 维护者没法按时间认哪份是出事那会儿的。
fn mtime_of(path: &Path) -> Option<zip::DateTime> {
    use chrono::{Datelike, Timelike};
    let t: chrono::DateTime<chrono::Local> = std::fs::metadata(path).ok()?.modified().ok()?.into();
    zip::DateTime::from_date_and_time(
        t.year().try_into().ok()?,
        t.month() as u8,
        t.day() as u8,
        t.hour() as u8,
        t.minute() as u8,
        t.second() as u8,
    )
    .ok()
}

/// 一次清理的结果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ClearReport {
    pub removed: usize,
    /// 正在被写的，截成 0 字节。
    pub truncated: usize,
    pub failed: usize,
}

/// 清理日志目录。见模块文档「清理」一节。
pub fn clear(log_dir: &Path) -> ClearReport {
    let mut r = ClearReport::default();
    for path in list_files(log_dir) {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let action = clear_action(name);
        let res = match action {
            ClearAction::Keep => continue,
            ClearAction::Truncate => File::options()
                .write(true)
                .open(&path)
                .and_then(|f| f.set_len(0)),
            ClearAction::Remove => std::fs::remove_file(&path),
        };
        match res {
            Ok(()) if action == ClearAction::Truncate => r.truncated += 1,
            Ok(()) => r.removed += 1,
            // 列目录之后被写端自己轮转走、或被 core 回收了：目的已经达到。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => r.removed += 1,
            Err(e) => {
                tracing::warn!("清理日志：{} 失败：{e}", path.display());
                r.failed += 1;
            }
        }
    }
    r
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClearAction {
    Keep,
    Truncate,
    Remove,
}

/// 按文件名决定怎么清。理由见模块文档「清理」一节。
fn clear_action(name: &str) -> ClearAction {
    if name == KEEP_ON_CLEAR {
        ClearAction::Keep
    } else if name.starts_with("wind_input.") {
        // core 一族：历史也只截断，删了会让 file-rotate 下次轮转时崩溃。
        ClearAction::Truncate
    } else if name.ends_with(".old.log") {
        // TSF 轮转下来的旧份，没有写端。
        ClearAction::Remove
    } else if name.starts_with("wind_tsf.")
        || name == "wind_setting.1.log"
        || name == "startup_stage.log"
    {
        // 可能正在被写：TSF 的当前份（宿主可能还活着；死进程留下的空文件由 core 按时限
        // 回收）、设置程序自己这一份、core 的启动轨迹。
        ClearAction::Truncate
    } else {
        ClearAction::Remove
    }
}

/// 三个「打开」目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirKind {
    Logs,
    UserData,
    App,
}

/// 在系统文件管理器里打开对应目录。
pub fn open_dir(kind: DirKind) -> Result<(), String> {
    let mode = crate::mode::get();
    match kind {
        DirKind::Logs => {
            let dir = crate::tsf_log::log_dir(mode).ok_or("无法定位日志目录")?;
            // 三个模块都关着日志时目录可能还不存在。先建再开，比报「不存在」有用。
            let _ = std::fs::create_dir_all(&dir);
            crate::open_external::open_with_default_app(&dir)
        }
        DirKind::UserData => {
            let dir =
                crate::user_dir::user_config_dir_for_mode(mode).ok_or("无法定位用户数据目录")?;
            let _ = std::fs::create_dir_all(&dir);
            crate::open_external::open_with_default_app(&dir)
        }
        DirKind::App => {
            let exe = std::env::current_exe().map_err(|e| format!("无法定位程序目录：{e}"))?;
            // macOS 的程序目录在 .app 包里面；直接打开 Contents/MacOS 让人摸不着头脑，
            // 在 Finder 里选中整个包才是「程序在哪」的答案。
            if let Some(bundle) = outermost_app_bundle(&exe) {
                return reveal(&bundle);
            }
            let dir = exe.parent().ok_or("无法定位程序目录")?;
            crate::open_external::open_with_default_app(dir)
        }
    }
}

fn outermost_app_bundle(exe: &Path) -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    exe.ancestors()
        .filter(|p| p.extension().is_some_and(|e| e == "app"))
        .last()
        .map(Path::to_path_buf)
}

/// 在文件管理器里**选中**一个文件（而不是打开它）。Linux 没有通用的「选中」，打开所在目录。
pub fn reveal(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // explorer 要的是 `/select,"路径"` 这一整个参数；交给 std 加引号会包住 `/select,`
        // 一起，explorer 不认。
        std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = path.parent().unwrap_or(path);
        crate::open_external::open_with_default_app(dir)
    }
}

/// 递归列出目录下的所有普通文件，按路径排序（打包顺序稳定）。
fn list_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                out.push(e.path());
            }
        }
    }
    out.sort();
    out
}

/// zip 内的条目名：相对日志目录、`/` 分隔（zip 规范要求，Windows 的 `\` 解压时会变成文件名的一部分）。
fn entry_name(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// 同一秒内连点两次不覆盖上一个包。
fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|i| dir.join(format!("{stem}-{i}.{ext}")))
        .find(|p| !p.exists())
        .expect("无穷序列总能找到空位")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "wind_setting_log_tools_{name}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn pack_includes_subdirs_with_forward_slash_names() {
        let root = tmp("pack");
        let logs = root.join("logs");
        write(&logs.join("wind_input.1.log"), "core");
        write(&logs.join("tsf_log").join("wind_tsf.wps.1.log"), "tsf");
        let dest = root.join("out");
        std::fs::create_dir_all(&dest).unwrap();

        let zip_path = pack(&logs, &dest, "WindInput", "20261017-120000").unwrap();
        assert_eq!(zip_path, dest.join("WindInput-logs-20261017-120000.zip"));

        let mut z = zip::ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
        let mut names: Vec<_> = z.file_names().map(str::to_string).collect();
        names.sort();
        assert_eq!(names, ["tsf_log/wind_tsf.wps.1.log", "wind_input.1.log"]);
        let mut s = String::new();
        z.by_name("tsf_log/wind_tsf.wps.1.log")
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert_eq!(s, "tsf");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pack_twice_in_same_second_keeps_both() {
        let root = tmp("pack_twice");
        let logs = root.join("logs");
        write(&logs.join("a.log"), "x");
        let a = pack(&logs, &root, "WindInput", "s").unwrap();
        let b = pack(&logs, &root, "WindInput", "s").unwrap();
        assert_ne!(a, b);
        assert!(a.exists() && b.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pack_empty_dir_is_error_and_leaves_nothing() {
        let root = tmp("pack_empty");
        let logs = root.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        assert!(pack(&logs, &root, "WindInput", "s").is_err());
        assert!(!root.join("WindInput-logs-s.zip").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clear_keeps_tsf_switch_and_removes_history() {
        let root = tmp("clear");
        write(&root.join("tsf_log_config"), "mode=file\n");
        write(&root.join("wind_setting.3.log"), "old");
        write(
            &root.join("tsf_log").join("wind_tsf.wps.123.old.log"),
            "old",
        );

        let r = clear(&root);
        assert_eq!(r.failed, 0);
        assert_eq!(r.removed, 2);
        assert!(root.join("tsf_log_config").exists());
        assert!(!root.join("wind_setting.3.log").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 删了 core 的历史份，file-rotate 下次轮转会 panic（core release 下即整个服务 abort）。
    #[test]
    fn clear_never_deletes_core_logs_and_truncates_live_ones() {
        let root = tmp("clear_current");
        for n in [
            "wind_input.1.log",
            "wind_input.2.log",
            "startup_stage.log",
            "wind_setting.1.log",
        ] {
            write(&root.join(n), "live");
        }
        write(&root.join("wind_setting.2.log"), "old");
        write(&root.join("tsf_log").join("wind_tsf.wps.123.log"), "live");

        let r = clear(&root);
        assert_eq!(
            r,
            ClearReport {
                removed: 1,
                truncated: 5,
                failed: 0
            }
        );
        assert_eq!(
            std::fs::metadata(root.join("wind_input.2.log"))
                .unwrap()
                .len(),
            0
        );
        assert!(!root.join("wind_setting.2.log").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 写端以追加方式开着时截断，之后的写入从头续写，不会在前面留一段空洞。
    #[test]
    fn truncated_file_keeps_receiving_appends_from_the_start() {
        let root = tmp("clear_held");
        let p = root.join("tsf_log").join("wind_tsf.wps.123.log");
        write(&p, "live");
        let mut writer = File::options().append(true).open(&p).unwrap();

        clear(&root);
        writer.write_all(b"next").unwrap();
        drop(writer);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "next");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn summarize_counts_recursively_and_tolerates_missing_dir() {
        let root = tmp("summary");
        write(&root.join("a.log"), "12345");
        write(&root.join("tsf_log").join("b.log"), "123");
        write(&root.join("tsf_log_config"), "mode=file\n");
        assert_eq!(summarize(&root), (2, 8));
        assert_eq!(summarize(&root.join("nope")), (0, 0));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn human_size_rounds_up_small_files() {
        assert_eq!(human_size(1), "1 KB");
        assert_eq!(human_size(3 * 1024 * 1024 / 2), "1.5 MB");
    }
}
