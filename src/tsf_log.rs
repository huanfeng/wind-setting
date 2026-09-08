//! TSF DLL 的日志配置（`<日志根>\tsf_log_config`）。
//!
//! # 为什么它不进 config.toml
//!
//! TSF DLL 在 `DllMain(DLL_PROCESS_ATTACH)` 里就要把日志支起来——那一刻 IPC 还没连上，
//! 甚至服务进程都可能还没启动（DLL 加载在前，拉服务在后）。而 TSF 日志最有价值的恰恰
//! 是启动期与 AppContainer 宿主里 IPC 根本不通的场景。所以它读的必须是一个**自己就能
//! 打开的文件**，而不是等谁推过来的配置。
//!
//! 于是本模块直接读写那个文件，走 `SideCommitter`（见 `state::SideCommitter`：
//! 「非 config.toml 落点一律登记 SideCommitter，不加独立保存按钮」）。
//!
//! # 文件格式与保留未知键
//!
//! 每行 `key=value`，`#` 起头为注释。读端是 C++ 的 `CFileLogger::_ReadConfig`。
//!
//! ⚠️ **写回必须逐行保留自己不认识的内容。** 这个文件同时是手工排查用的开关面板：
//! `dump_hotkey`（Ctrl+Shift+F12 环形日志导出）就只在那里配，将来还会有别的。整表重写
//! 会把用户刚写的排查开关连同注释一起抹掉，而他不会想到是设置页干的。

use crate::mode::AppMode;
use std::path::PathBuf;

/// 配置文件名。与 C++ 侧 `WIND_LOG_CONFIG_NAME` 同名，两处无编译期约束。
const CONFIG_NAME: &str = "tsf_log_config";

/// 日志级别档位。`Off` 在文件里落成 `mode=none`，其余落成 `mode=<方式>` + `level=<级别>`。
///
/// ★ 界面上只有一个「级别」下拉，`关闭` 是它的一档——而文件里"关不关"和"多详细"是
/// mode / level 两个键。合并是有意的：用户心里只有一根从"不记"到"全记"的轴，
/// 让他去理解 mode=none 与 level=off 的区别没有任何收益。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Level::Off),
            "error" => Some(Level::Error),
            "warn" => Some(Level::Warn),
            "info" => Some(Level::Info),
            "debug" => Some(Level::Debug),
            "trace" => Some(Level::Trace),
            _ => None,
        }
    }
}

/// 输出方式。`None` 不在此列——"不输出"由 [`Level::Off`] 表达，见其文档。
///
/// `all`（文件 + DebugString）刻意不暴露：它只是两者叠加，多一档只会让下拉更难选。
/// 手工在文件里写 `mode=all` 仍然有效，本模块读到时归到 [`Mode::File`]（文件那一路
/// 确实开着），且只要用户不动这一项就**不会被写回覆盖**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    File,
    DebugString,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::File => "file",
            Mode::DebugString => "debugstring",
        }
    }
}

/// 一次读取的结果。两个字段都是 fieldless 枚举，整体按值传递。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    pub level: Level,
    pub mode: Mode,
}

impl Default for Setting {
    /// 出厂：不记日志。与 C++ 侧「无配置文件 ⇒ mode=none」一致。
    fn default() -> Self {
        Self {
            level: Level::Off,
            mode: Mode::File,
        }
    }
}

/// 日志根目录：`<本机状态目录>\logs`。
///
/// 与 core 的 `Config::log_dir()`、TSF 的 `CFileLogger::_BuildPaths` 同址——三者都是
/// 「便携 ⇒ `<安装根>\userdata\logs`，否则 `%LOCALAPPDATA%\WindInput[Dev]\logs`」。
/// 三处各写一份判据，任何一处漂移都会表现为「设置页改了、TSF 读的是另一个文件」。
pub fn log_dir(mode: AppMode) -> Option<PathBuf> {
    crate::user_dir::local_dir_for_mode(mode).map(|d| d.join("logs"))
}

fn config_path(mode: AppMode) -> Option<PathBuf> {
    log_dir(mode).map(|d| d.join(CONFIG_NAME))
}

/// 解析文件内容（纯函数，不碰盘）。认不出的行一律忽略，绝不因一行坏数据整份作废。
pub fn parse(text: &str) -> Setting {
    let mut level_key: Option<Level> = None;
    let mut mode_raw: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        match k.as_str() {
            "level" => level_key = Level::parse(v),
            "mode" => mode_raw = Some(v.to_ascii_lowercase()),
            _ => {}
        }
    }

    // mode 决定"记不记"，level 决定"多详细"——界面上的那一根轴由两者合成。
    // 认不出的 mode 视同 none：宁可显示成"关闭"，也不要在界面上宣称正在记录
    // 而实际上 C++ 侧因为同样认不出而没记。
    match mode_raw.as_deref() {
        Some("file") | Some("all") => Setting {
            // 文件里 mode 开着但 level 缺失/写坏时，C++ 侧的默认是 info，此处对齐。
            level: level_key.unwrap_or(Level::Info),
            mode: Mode::File,
        },
        Some("debugstring") | Some("debug_string") => Setting {
            level: level_key.unwrap_or(Level::Info),
            mode: Mode::DebugString,
        },
        _ => Setting {
            level: Level::Off,
            // 关闭时 mode 下拉回落到"文件"这个常态默认，用户重新打开日志时不必再选一次。
            mode: Mode::File,
        },
    }
}

/// 把新设置合并进原文件内容，返回新内容（纯函数，不碰盘）。
///
/// ⚠️ 逐行保留原文：只改写 `mode` / `level` 两行，其余（注释、`dump_hotkey` 等）原样
/// 留下。整表重写会把用户手工写的排查开关抹掉，而他不会想到是设置页干的。
pub fn merge(original: &str, s: Setting) -> String {
    let mode_line = if s.level == Level::Off {
        "mode=none".to_string()
    } else {
        format!("mode={}", s.mode.as_str())
    };
    // 关闭档也把 level 写下来：用户下次开启时那个档位还在，不必重选。C++ 侧
    // mode=none 时根本不看 level，写着无害。
    let level_line = format!(
        "level={}",
        if s.level == Level::Off {
            // Off 不是 C++ 侧 level 的合法取值语义（那边 off 表示"一条都不记"，
            // 与 mode=none 重复），落 info 这个中性档。
            Level::Info.as_str()
        } else {
            s.level.as_str()
        }
    );

    let mut out: Vec<String> = Vec::new();
    let (mut seen_mode, mut seen_level) = (false, false);
    for line in original.lines() {
        let trimmed = line.trim();
        let key = if trimmed.starts_with('#') {
            None
        } else {
            trimmed
                .split_once('=')
                .map(|(k, _)| k.trim().to_ascii_lowercase())
        };
        // 认识的两个键在**第一次出现的位置**就地替换，之后的重复行直接丢：留着的话
        // C++ 侧读取以最后一行为准，与本次写入的第一行矛盾——表现是「存了没生效」。
        match key.as_deref() {
            Some("mode") => {
                if !seen_mode {
                    out.push(mode_line.clone());
                    seen_mode = true;
                }
            }
            Some("level") => {
                if !seen_level {
                    out.push(level_line.clone());
                    seen_level = true;
                }
            }
            _ => out.push(line.to_string()),
        }
    }
    if !seen_mode {
        out.push(mode_line);
    }
    if !seen_level {
        out.push(level_line);
    }

    let mut text = out.join("\r\n");
    text.push_str("\r\n");
    text
}

/// 读取当前设置。文件不存在 / 读不出来 ⇒ 出厂值（关闭），与 C++ 侧一致。
pub fn read(mode: AppMode) -> Setting {
    config_path(mode)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| parse(&t))
        .unwrap_or_default()
}

/// 写回设置。目录不存在会先建。
pub fn write(mode: AppMode, s: Setting) -> Result<(), String> {
    let Some(path) = config_path(mode) else {
        return Err("无法定位日志目录".to_string());
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建日志目录失败: {e}"))?;
    }
    // 读不出来按空串处理（文件不存在是常态：出厂就没有这个文件）。
    let original = std::fs::read_to_string(&path).unwrap_or_default();
    let text = merge(&original, s);
    std::fs::write(&path, text).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_absent_file_is_off() {
        assert_eq!(
            parse(""),
            Setting {
                level: Level::Off,
                mode: Mode::File
            }
        );
    }

    #[test]
    fn parse_mode_none_is_off_regardless_of_level() {
        // C++ 侧 mode=none 时压根不看 level；界面必须显示成"关闭"，
        // 否则用户会以为正在记录 trace 而实际一条都没有。
        let s = parse("mode=none\nlevel=trace\n");
        assert_eq!(s.level, Level::Off);
    }

    #[test]
    fn parse_unknown_mode_is_off() {
        // 认不出的 mode 在 C++ 侧同样落到 none，界面须与之一致。
        assert_eq!(parse("mode=wat\nlevel=debug\n").level, Level::Off);
    }

    #[test]
    fn parse_all_maps_to_file() {
        let s = parse("mode=all\nlevel=warn\n");
        assert_eq!(s.mode, Mode::File);
        assert_eq!(s.level, Level::Warn);
    }

    #[test]
    fn parse_mode_on_without_level_defaults_info() {
        // 与 C++ 侧 _ReadConfig 的默认值对齐。
        assert_eq!(parse("mode=file\n").level, Level::Info);
    }

    #[test]
    fn parse_tolerates_comments_and_junk() {
        let s = parse("# 注释\n\n  MODE = File \nlevel=DEBUG\n乱码行\n");
        assert_eq!(s.mode, Mode::File);
        assert_eq!(s.level, Level::Debug);
    }

    /// 写回必须保留自己不认识的键——`dump_hotkey` 是手工排查开关，抹掉它
    /// 用户不会想到是设置页干的。
    #[test]
    fn merge_preserves_unknown_keys_and_comments() {
        let orig = "# 我的排查配置\nmode=none\ndump_hotkey=1\nlevel=info\n";
        let out = merge(
            orig,
            Setting {
                level: Level::Debug,
                mode: Mode::File,
            },
        );
        assert!(out.contains("dump_hotkey=1"), "未知键被抹掉了: {out}");
        assert!(out.contains("# 我的排查配置"), "注释被抹掉了: {out}");
        assert!(out.contains("mode=file"), "mode 未更新: {out}");
        assert!(out.contains("level=debug"), "level 未更新: {out}");
    }

    #[test]
    fn merge_into_empty_file_writes_both_keys() {
        let out = merge(
            "",
            Setting {
                level: Level::Warn,
                mode: Mode::DebugString,
            },
        );
        assert!(out.contains("mode=debugstring"), "{out}");
        assert!(out.contains("level=warn"), "{out}");
    }

    #[test]
    fn merge_off_writes_mode_none() {
        let out = merge(
            "mode=file\nlevel=trace\n",
            Setting {
                level: Level::Off,
                mode: Mode::File,
            },
        );
        assert!(out.contains("mode=none"), "{out}");
        // 关闭时 level 落中性档：C++ 侧不看它，但用户重新开启时不该看到 trace 还留着。
        assert!(out.contains("level=info"), "{out}");
    }

    /// 重复行必须收敛成一行：留着的话 C++ 侧以最后一行为准，
    /// 与本次写入的第一行矛盾——表现是"存了没生效"。
    #[test]
    fn merge_collapses_duplicate_keys() {
        let out = merge(
            "mode=none\nmode=file\nlevel=info\nlevel=trace\n",
            Setting {
                level: Level::Error,
                mode: Mode::File,
            },
        );
        assert_eq!(out.matches("mode=").count(), 1, "{out}");
        assert_eq!(out.matches("level=").count(), 1, "{out}");
        assert!(out.contains("mode=file"), "{out}");
        assert!(out.contains("level=error"), "{out}");
    }

    #[test]
    fn roundtrip_every_level() {
        for lv in [
            Level::Error,
            Level::Warn,
            Level::Info,
            Level::Debug,
            Level::Trace,
        ] {
            let s = Setting {
                level: lv,
                mode: Mode::File,
            };
            assert_eq!(parse(&merge("", s)), s, "级别 {lv:?} 往返不一致");
        }
        // Off 往返：写成 mode=none，读回仍是 Off（mode 回落 File，见 parse）。
        let off = Setting {
            level: Level::Off,
            mode: Mode::File,
        };
        assert_eq!(parse(&merge("", off)), off);
    }

    #[test]
    fn roundtrip_debugstring() {
        let s = Setting {
            level: Level::Trace,
            mode: Mode::DebugString,
        };
        assert_eq!(parse(&merge("", s)), s);
    }
}
