//! 动作表达式（cmdbar 语法）与「下拉 + 参数」两种表示之间的双向转换。
//!
//! # 谁在用
//!
//! 当前是工具栏自定义按钮的 `action`。**刻意做成独立模块而非塞进工具栏那侧**：
//! 短语（`$CC(…)` 里的动作）是同一套语法，将来那边的编辑器可以直接复用本模块的
//! 解析与合成，只换 UI 外壳。故本模块只认表达式，不认「按钮」「短语」这些概念。
//!
//! # 边界纪律：真实文本是唯一事实，转义只在这里发生
//!
//! 用户在界面里填的是**真实路径**（`D:\Download\知符\知符.exe`），存进配置的是
//! cmdbar 表达式（`proc.run("D:\\Download\\知符\\知符.exe")`）。两侧的转换收在
//! [`quote_arg`] / [`unquote_arg`] 这一对里——用户不必知道反斜杠要写两个，
//! 那正是 2026-08-26 前手写配置时最容易出错的地方。
//!
//! # ⚠️ 只保证「本模块生成的表达式能被本模块读回」
//!
//! cmdbar 的完整语法在 core 的 lexer 里（变量插值、多参数、具名参数、嵌套调用……），
//! 在这里重实现一遍就是「同一套语法两处实现」，迟早分叉。故 [`parse`] 只认本模块
//! [`ActionExpr::to_source`] 写得出的那几种形态，**其余一律落 [`ActionKind::Custom`]
//! 原样显示，绝不猜**。用户手写的复杂表达式在界面上看到的就是它自己写的那一行，
//! 保存后逐字不变。

/// 界面下拉里的动作类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    /// `proc.run("<参数>")` —— 启动程序。参数是程序路径。
    RunProgram,
    /// `open("<参数>")` —— 按系统默认方式打开：网址、文档、文件夹。
    Open,
    /// `wind.cli("<参数>")` —— 执行本输入法的命令，如 `schema switch wubi86`。
    WindCli,
    /// `key.tap("<参数>")` —— 模拟一次按键组合，如 `Ctrl+Shift+P`。
    KeyTap,
    /// 上面几种都不是：原样保存用户写的表达式。
    Custom,
}

impl ActionKind {
    /// 下拉里按此顺序排列；`Custom` 恒在最后。
    pub const ALL: [ActionKind; 5] = [
        ActionKind::RunProgram,
        ActionKind::Open,
        ActionKind::WindCli,
        ActionKind::KeyTap,
        ActionKind::Custom,
    ];

    /// 下拉显示名。
    pub fn label(self) -> &'static str {
        match self {
            ActionKind::RunProgram => "启动程序",
            ActionKind::Open => "打开网址 / 文件",
            ActionKind::WindCli => "输入法命令",
            ActionKind::KeyTap => "模拟按键",
            ActionKind::Custom => "自定义表达式",
        }
    }

    /// 该类型的参数是不是本机文件路径（决定要不要给「浏览」按钮）。
    pub fn takes_path(self) -> bool {
        matches!(self, ActionKind::RunProgram)
    }

    /// 函数名；`Custom` 无。
    fn func(self) -> Option<&'static str> {
        match self {
            ActionKind::RunProgram => Some("proc.run"),
            ActionKind::Open => Some("open"),
            ActionKind::WindCli => Some("wind.cli"),
            ActionKind::KeyTap => Some("key.tap"),
            ActionKind::Custom => None,
        }
    }
}

/// 一条动作的结构化表示：类型 + 单个参数（`Custom` 时参数即整条表达式）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionExpr {
    pub kind: ActionKind,
    /// **真实值**（未转义）：路径就是路径，界面直接显示、用户直接改。
    pub arg: String,
}

impl ActionExpr {
    /// 结构化 → cmdbar 表达式源码。
    pub fn to_source(&self) -> String {
        match self.kind.func() {
            Some(f) => format!("{f}(\"{}\")", quote_arg(&self.arg)),
            // Custom：arg 本身就是用户写的整条表达式，原样返回。
            None => self.arg.clone(),
        }
    }
}

/// 一条动作的**人话摘要**，用于列表行的说明文字。
///
/// 显示解析后的真实参数而不是表达式源码：源码里的双反斜杠是给 cmdbar 看的，
/// 摆在界面上只会让用户以为自己的路径写错了。认不出的形态没有更好的说法，
/// 原样显示它自己写的那一行。
pub fn summary(src: &str) -> String {
    let a = parse(src);
    match a.kind {
        ActionKind::Custom => a.arg,
        _ => format!("{}：{}", a.kind.label(), a.arg),
    }
}

/// 表达式 → 结构化。认不出的形态落 [`ActionKind::Custom`]（arg = 原文）。
///
/// 只认「函数名(单个双引号字符串)」这一种形态——多参数、具名参数、变量插值、嵌套调用
/// 都交给 Custom。判据见模块文档：宁可让用户看到自己写的原文，也不猜错一个参数。
pub fn parse(src: &str) -> ActionExpr {
    let s = src.trim();
    for kind in ActionKind::ALL {
        let Some(f) = kind.func() else { continue };
        // 形态：<func>("<...>")
        let Some(rest) = s.strip_prefix(f) else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('(') else {
            continue;
        };
        let Some(rest) = rest.trim_end().strip_suffix(')') else {
            continue;
        };
        let inner = rest.trim();
        let Some(inner) = inner.strip_prefix('"').and_then(|x| x.strip_suffix('"')) else {
            continue;
        };
        // 参数里若还有**未转义**的引号，说明是多参数或更复杂的形态 ⇒ 不认。
        if has_bare_quote(inner) {
            continue;
        }
        return ActionExpr {
            kind,
            arg: unquote_arg(inner),
        };
    }
    ActionExpr {
        kind: ActionKind::Custom,
        arg: s.to_string(),
    }
}

/// 真实值 → 双引号字符串字面量的内容（转义反斜杠与双引号）。
///
/// 只转这两个：cmdbar 的 `decode_escape` 里 `\\` 与 `\"` 是恒等往返的，其余字符
/// （含中文）原样写进去即可。刻意**不**转 `\n` 之类——界面里的参数是一行文本，
/// 出现真换行本身就是用户写错了，替他编码只会把错误藏起来。
pub fn quote_arg(real: &str) -> String {
    real.replace('\\', r"\\").replace('"', "\\\"")
}

/// [`quote_arg`] 的逆：字符串字面量内容 → 真实值。
///
/// 与 core `lexer::decode_escape` 的口径对齐：`\\`→`\`、`\"`→`"`、`\n`/`\t`/`\r` 转控制符，
/// **未知转义原样保留反斜杠**（`\x` 仍是 `\x`）——那条是 core 的既有行为，
/// 界面上如实显示才不会让用户以为自己写的东西被吃了。
pub fn unquote_arg(lit: &str) -> String {
    let mut out = String::with_capacity(lit.len());
    let mut chars = lit.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            // 未知转义：保留反斜杠 + 原字符（同 core）。
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// 字符串字面量内容里是否有**未被转义**的双引号（有则不是单参数形态）。
fn has_bare_quote(lit: &str) -> bool {
    let mut esc = false;
    for c in lit.chars() {
        match (esc, c) {
            (true, _) => esc = false,
            (false, '\\') => esc = true,
            (false, '"') => return true,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ 往返恒等：结构化 → 源码 → 结构化，必须回到原处。
    ///
    /// 这是本模块的核心契约——界面存下去再读回来，用户看到的得是同一件事。
    #[test]
    fn round_trip_through_source() {
        let cases = [
            (ActionKind::RunProgram, r"D:\Download\知符\知符.exe"),
            (ActionKind::Open, "https://www.zdic.net"),
            (ActionKind::WindCli, "schema switch wubi86"),
            (ActionKind::KeyTap, "Ctrl+Shift+P"),
            (ActionKind::Custom, r#"proc.shell("echo hi", cwd="D:\\")"#),
        ];
        for (kind, arg) in cases {
            let a = ActionExpr {
                kind,
                arg: arg.to_string(),
            };
            let back = parse(&a.to_source());
            assert_eq!(back, a, "往返后不一致：{kind:?} {arg:?}");
        }
    }

    /// ★★ 用户 2026-08-26 手写的那条：界面显示**真实路径**，存盘是转义后的表达式。
    ///
    /// 这正是 GUI 存在的理由——手写时要记得反斜杠写两个，那是最容易错的一步。
    #[test]
    fn windows_path_is_escaped_on_save_and_shown_raw() {
        let a = ActionExpr {
            kind: ActionKind::RunProgram,
            arg: r"D:\Download\知符\知符.exe".to_string(),
        };
        assert_eq!(a.to_source(), r#"proc.run("D:\\Download\\知符\\知符.exe")"#);
        // 反向：用户手写的那条读进界面，显示的是没有双反斜杠的真实路径。
        let parsed = parse(r#"proc.run("D:\\Download\\知符\\知符.exe")"#);
        assert_eq!(parsed.kind, ActionKind::RunProgram);
        assert_eq!(parsed.arg, r"D:\Download\知符\知符.exe");
    }

    /// 认不出的形态一律落 Custom 且**逐字保留**——绝不猜、也绝不改写。
    #[test]
    fn unrecognized_forms_fall_back_to_custom_verbatim() {
        let exotic = [
            r#"proc.run("a.exe", "b", cwd="D:\\x")"#, // 多参数 + 具名参数
            r#"key.seq("Home", "Shift+End")"#,        // 多参数
            r#"web.search("baidu", last())"#,         // 嵌套调用
            r#"type("你好")"#,                        // 本模块不认的函数
            "",                                       // 空
        ];
        for src in exotic {
            let a = parse(src);
            assert_eq!(a.kind, ActionKind::Custom, "{src:?} 不该被认成已知类型");
            assert_eq!(a.arg, src, "Custom 必须逐字保留原文");
            assert_eq!(a.to_source(), src, "Custom 存回去也必须逐字不变");
        }
    }

    /// 参数里含引号：转义后往返仍恒等，且不会被误判成多参数。
    #[test]
    fn quoted_char_in_arg_survives() {
        let a = ActionExpr {
            kind: ActionKind::Open,
            arg: r#"C:\a "b" c"#.to_string(),
        };
        let src = a.to_source();
        assert_eq!(src, r#"open("C:\\a \"b\" c")"#);
        assert_eq!(parse(&src), a);
    }

    /// 未知转义原样保留（与 core `decode_escape` 同口径）：`\x` 读回仍是 `\x`。
    ///
    /// 若这里擅自吃掉反斜杠，用户会看到自己写的东西少了一截——而 core 侧那边
    /// 其实是保留的，两边就此分叉。
    #[test]
    fn unknown_escape_is_preserved_like_core() {
        assert_eq!(unquote_arg(r"a\xb"), r"a\xb");
        assert_eq!(unquote_arg(r"E:\文档"), r"E:\文档");
    }

    /// 列表行的说明显示**真实参数**，不含表达式里的双反斜杠。
    ///
    /// 这一条钉的是「界面上不出现给机器看的转义」——用户看到 `D:\\Download` 会以为
    /// 自己的路径写错了。
    #[test]
    fn summary_shows_real_value_not_source() {
        assert_eq!(
            summary(r#"proc.run("D:\\Download\\知符\\知符.exe")"#),
            r"启动程序：D:\Download\知符\知符.exe"
        );
        assert_eq!(
            summary(r#"open("https://www.zdic.net")"#),
            "打开网址 / 文件：https://www.zdic.net"
        );
        // 认不出的形态没有更好的说法，原样显示用户写的那一行。
        let exotic = r#"key.seq("Home", "Shift+End")"#;
        assert_eq!(summary(exotic), exotic);
    }

    /// 空白容错：源码两端的空格不影响识别。
    #[test]
    fn surrounding_whitespace_is_tolerated() {
        let a = parse(r#"  proc.run( "x.exe" )  "#);
        assert_eq!(a.kind, ActionKind::RunProgram);
        assert_eq!(a.arg, "x.exe");
    }

    /// 前缀相同的函数名不得互相误认（`open` 是 `open_xxx` 的前缀）。
    #[test]
    fn similar_function_names_are_not_confused() {
        let a = parse(r#"open_other("x")"#);
        assert_eq!(a.kind, ActionKind::Custom, "open_other 不是 open");
    }
}
