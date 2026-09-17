//! 命令行参数解析:`--page` / `--tab` / 快捷 `--<page>` / `windinput://` 协议。
//! 冷启动与二次实例共用同一套解析。

use crate::pages::dict::spec::LogicalTab;

/// 规范页 id，**顺序即侧栏顺序**：方案 / 输入 / 按键 / 外观 / 统计 / 词库 / 高级 / 关于，
/// `pages::all_tabs` 必须逐格对齐。
///
/// 「词库」排在「统计」之后：侧栏里只有它是**动作项**——点它开的是独立窗，主窗停在
/// 原页不动（见 `shell::build_nav` 里的 `is_action`）。把唯一一个「点了会跳出去」的
/// 条目从配置页中间挪到末尾那组，前面五格就都是纯粹的「切页」，行为一致。
const PAGES: &[&str] = &[
    "schema", "input", "keys", "ui", "stats", "dict", "advanced", "about",
];

/// 规范页 id → tab 索引。非法 id 返回 None(不兼容旧别名)。
pub fn page_to_index(page: &str) -> Option<usize> {
    PAGES.iter().position(|p| *p == page)
}

/// 解析目标页索引:`--page <id>` / `--page=<id>` / 快捷 `--<id>` / `--tab <n>`。无匹配返回 None。
pub fn parse_target(argv: &[String]) -> Option<usize> {
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--page=") {
            return page_to_index(v);
        }
        if a == "--page" {
            return it.next().and_then(|v| page_to_index(v));
        }
        if a == "--tab" {
            return it.next().and_then(|v| v.parse::<usize>().ok());
        }
        if let Some(name) = a.strip_prefix("--")
            && let Some(i) = page_to_index(name)
        {
            return Some(i);
        }
    }
    None
}

/// 从 argv 提取协议 URL(`windinput://` 前缀,大小写不敏感)。
pub fn parse_protocol(argv: &[String]) -> Option<String> {
    argv.iter()
        .find(|a| a.to_ascii_lowercase().starts_with("windinput://"))
        .cloned()
}

/// argv 是否请求「导入配置」深链(`--page import` / `--page=import` / `--import`)。
///
/// `import` 不是真实 Tab(PAGES 里没有,`parse_target` 对它返回 None):它是
/// 「切到高级页 + 弹出导入对话框 + 读一次剪贴板」的组合动作。解析归解析,
/// 执行在 main 的导入通道里(读剪贴板要 EventCtx,只能在 UI 线程事件里做)。
pub fn parse_import_request(argv: &[String]) -> bool {
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        if a == "--import" || a == "--page=import" {
            return true;
        }
        if a == "--page" && it.next().map(|s| s.as_str()) == Some("import") {
            return true;
        }
    }
    false
}

/// 本程序登记了文件关联的扩展名(见 `file_assoc::ALL`)。
///
/// 不认 `.zip`:太泛,不抢通用压缩包的关联,也不从命令行猜。具体是方案还是主题由
/// `config_import::route_of` 按扩展名分派,真正的类型判据在包内 manifest。
const IMPORTABLE_EXTS: [&str; 2] = ["wpkg", "wtheme"];

/// argv 里的包路径(文件关联双击时 shell 按 `"<exe>" "%1"` 传入)。
///
/// 跳过 argv[0](自身 exe 路径)与 `--` 开头的旗标。
pub fn parse_import_file(argv: &[String]) -> Option<std::path::PathBuf> {
    argv.iter()
        .skip(1)
        .find(|a| {
            !a.starts_with("--")
                && std::path::Path::new(a.as_str())
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| IMPORTABLE_EXTS.iter().any(|w| e.eq_ignore_ascii_case(w)))
        })
        .map(std::path::PathBuf::from)
}

/// 词库页深链参数(`--page dict` 时可选携带 `--schema` / `--type`)。
///
/// 两维各自可选:只给 `--type` 表示"当前数据域的该类型",只给 `--schema` 表示
/// "该方案的默认子标签"。真正的定位在 [`crate::pages::dict::state::DictManagerState::apply_deep_link`],
/// 本结构只负责把 argv 归一成意图,不碰方案列表(冷启动时方案列表尚未就绪)。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DictTarget {
    /// 方案 id / 引擎族名(`pinyin`/`codetable`/`mixed`)/ `phrase`;空 = 不指定。
    pub schema: String,
    /// 目标子标签;None = 不指定(或 `--type` 值非法)。
    pub tab: Option<LogicalTab>,
    /// `--type` 的原始取值(仅当解析失败时非空,供上层提示"未知类型 X")。
    pub bad_tab: String,
}

impl DictTarget {
    /// 两维都没给 —— 等价于只写了 `--page dict`,无需走深链路径。
    pub fn is_empty(&self) -> bool {
        self.schema.is_empty() && self.tab.is_none() && self.bad_tab.is_empty()
    }

    /// 写入 `--type` 取值:非法值不静默吞掉,留原值给上层 toast 提示,
    /// 否则用户只会看到"没反应"而无从判断是拼错还是功能没生效。
    fn set_tab(&mut self, v: &str) {
        match LogicalTab::from_cli_id(v) {
            Some(tab) => {
                self.tab = Some(tab);
                self.bad_tab.clear();
            }
            None => self.bad_tab = v.to_string(),
        }
    }
}

/// 解析词库页深链。仅当目标页确为 `dict` 时返回 `Some`,否则 `None`
/// (`--schema` 在 `--page add-word` 下另有含义,见 [`parse_add_word`])。
pub fn parse_dict_target(argv: &[String]) -> Option<DictTarget> {
    if parse_target(argv) != page_to_index("dict") {
        return None;
    }
    let mut t = DictTarget::default();
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--schema=") {
            t.schema = v.to_string();
        } else if a == "--schema" {
            if let Some(v) = it.next() {
                t.schema = v.clone();
            }
        } else if let Some(v) = a.strip_prefix("--type=") {
            t.set_tab(v);
        } else if a == "--type" {
            if let Some(v) = it.next() {
                t.set_tab(v);
            }
        }
    }
    Some(t)
}

/// 加词界面参数(`--page add-word` 触发)。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct AddWordParams {
    pub text: String,
    pub code: String,
    pub schema: String,
}

/// 检测 argv 是否请求加词界面并解析 `--text/--code/--schema`(`=` 与空格两式)。
/// 触发条件:含 `--page add-word` / `--page=add-word` / `--add-word`。
pub fn parse_add_word(argv: &[String]) -> Option<AddWordParams> {
    let mut requested = false;
    let mut it = argv.iter().peekable();
    while let Some(a) = it.peek() {
        let a = a.as_str();
        if a == "--add-word" || a == "--page=add-word" {
            requested = true;
        } else if a == "--page" {
            // 向前看下一个是否 add-word
            let mut clone = it.clone();
            clone.next();
            if clone.peek().map(|s| s.as_str()) == Some("add-word") {
                requested = true;
            }
        }
        it.next();
    }
    if !requested {
        return None;
    }
    let mut p = AddWordParams::default();
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--text=") {
            p.text = v.to_string();
        } else if a == "--text" {
            if let Some(v) = it.next() {
                p.text = v.to_string();
            }
        } else if let Some(v) = a.strip_prefix("--code=") {
            p.code = v.to_string();
        } else if a == "--code" {
            if let Some(v) = it.next() {
                p.code = v.to_string();
            }
        } else if let Some(v) = a.strip_prefix("--schema=") {
            p.schema = v.to_string();
        } else if a == "--schema" {
            if let Some(v) = it.next() {
                p.schema = v.to_string();
            }
        }
    }
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn page_mapping() {
        assert_eq!(page_to_index("schema"), Some(0));
        assert_eq!(page_to_index("keys"), Some(2));
        assert_eq!(page_to_index("stats"), Some(4));
        assert_eq!(page_to_index("dict"), Some(5));
        assert_eq!(page_to_index("advanced"), Some(6));
        assert_eq!(page_to_index("about"), Some(7));
        assert_eq!(page_to_index("nope"), None);
        assert_eq!(page_to_index("general"), None); // 旧别名不再支持
    }

    #[test]
    fn parse_target_forms() {
        assert_eq!(parse_target(&a(&["exe", "--page", "input"])), Some(1));
        assert_eq!(parse_target(&a(&["exe", "--page=about"])), Some(7));
        assert_eq!(parse_target(&a(&["exe", "--about"])), Some(7));
        assert_eq!(parse_target(&a(&["exe", "--tab", "5"])), Some(5));
        assert_eq!(parse_target(&a(&["exe"])), None);
    }

    #[test]
    fn parse_protocol_extract() {
        assert_eq!(
            parse_protocol(&a(&["exe", "windinput://import/theme?url=https://x"])).as_deref(),
            Some("windinput://import/theme?url=https://x")
        );
        assert_eq!(parse_protocol(&a(&["exe", "--page", "input"])), None);
    }

    /// `import` 深链三种写法都认;不是真实页,`parse_target` 必须继续返回 None,
    /// 否则会被当成 tab 索引乱切页。
    #[test]
    fn parse_import_request_forms() {
        assert!(parse_import_request(&a(&["exe", "--page", "import"])));
        assert!(parse_import_request(&a(&["exe", "--page=import"])));
        assert!(parse_import_request(&a(&["exe", "--import"])));
        assert!(!parse_import_request(&a(&["exe", "--page", "dict"])));
        assert!(!parse_import_request(&a(&["exe"])));
        assert_eq!(parse_target(&a(&["exe", "--page", "import"])), None);
        assert_eq!(parse_target(&a(&["exe", "--import"])), None);
    }

    /// 文件关联只登记 `.wpkg`,路由也只认 `.wpkg`:.zip/.toml 不从命令行猜。
    /// 扩展名判断走 `Path::extension`,路径末尾是多字节字符也不 panic。
    #[test]
    fn parse_import_file_takes_registered_package_exts() {
        assert_eq!(
            parse_import_file(&a(&["exe", r"C:\down\pkg.wpkg"])),
            Some(std::path::PathBuf::from(r"C:\down\pkg.wpkg"))
        );
        // 大小写不敏感 + 中文文件名
        assert_eq!(
            parse_import_file(&a(&["exe", r"C:\下载\方案包.WPKG"])),
            Some(std::path::PathBuf::from(r"C:\下载\方案包.WPKG"))
        );
        // 主题包同样认（与 file_assoc::ALL 登记的那两个扩展名对齐）
        assert_eq!(
            parse_import_file(&a(&["exe", r"C:\down\水墨.wtheme"])),
            Some(std::path::PathBuf::from(r"C:\down\水墨.wtheme"))
        );
        assert_eq!(
            parse_import_file(&a(&["exe", r"C:\down\x.WTHEME"])),
            Some(std::path::PathBuf::from(r"C:\down\x.WTHEME"))
        );
        // 旗标与其它扩展名都不认
        assert_eq!(parse_import_file(&a(&["exe", "--page", "dict"])), None);
        assert_eq!(parse_import_file(&a(&["exe", r"C:\a\b.zip"])), None);
        assert_eq!(parse_import_file(&a(&["exe", r"C:\a\b.toml"])), None);
        assert_eq!(parse_import_file(&a(&["exe", r"C:\a\b.toml"])), None);
        // argv[0] 是自身路径,即便像 .wpkg 也跳过
        assert_eq!(parse_import_file(&a(&["weird.wpkg"])), None);
    }

    #[test]
    fn parse_dict_target_forms() {
        // 完整深链（空格式）
        let t = parse_dict_target(&a(&[
            "exe", "--page", "dict", "--schema", "wubi86", "--type", "shadow",
        ]))
        .unwrap();
        assert_eq!(t.schema, "wubi86");
        assert_eq!(t.tab, Some(LogicalTab::Shadow));
        assert!(t.bad_tab.is_empty());

        // `=` 式 + 快捷 --dict（与 --page dict 能力对等）
        let t2 =
            parse_dict_target(&a(&["exe", "--dict", "--schema=pinyin", "--type=freq"])).unwrap();
        assert_eq!(t2.schema, "pinyin");
        assert_eq!(t2.tab, Some(LogicalTab::Freq));

        // 两维各自可选
        let only_type =
            parse_dict_target(&a(&["exe", "--page", "dict", "--type", "shadow"])).unwrap();
        assert!(only_type.schema.is_empty());
        assert_eq!(only_type.tab, Some(LogicalTab::Shadow));

        let only_schema =
            parse_dict_target(&a(&["exe", "--page", "dict", "--schema", "wubi86"])).unwrap();
        assert_eq!(only_schema.schema, "wubi86");
        assert!(only_schema.tab.is_none());

        // 都不给 → is_empty（上层据此跳过深链）
        assert!(
            parse_dict_target(&a(&["exe", "--page", "dict"]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn parse_dict_target_only_on_dict_page() {
        // 非词库页即便带 --schema 也不返回深链：那是 add-word 分支的参数，不能抢
        assert!(parse_dict_target(&a(&["exe", "--page", "input", "--schema", "wubi86"])).is_none());
        assert!(
            parse_dict_target(&a(&["exe", "--page", "add-word", "--schema", "pinyin"])).is_none()
        );
        assert!(parse_dict_target(&a(&["exe"])).is_none());
        // --tab 5 恰好是词库页下标：数字定位也应认深链
        assert_eq!(page_to_index("dict"), Some(5));
        assert!(parse_dict_target(&a(&["exe", "--tab", "5", "--type", "shadow"])).is_some());
    }

    #[test]
    fn parse_dict_target_keeps_bad_type_for_reporting() {
        let t = parse_dict_target(&a(&["exe", "--page", "dict", "--type", "候选调整"])).unwrap();
        assert!(t.tab.is_none());
        assert_eq!(
            t.bad_tab, "候选调整",
            "非法值应留存供上层提示，而非静默丢弃"
        );
        assert!(!t.is_empty(), "有非法值时不算空，仍需走深链路径以弹出提示");
    }

    #[test]
    fn parse_add_word_forms() {
        // --page add-word + 各参数(= 与空格两式)
        let p = parse_add_word(&a(&[
            "exe",
            "--page",
            "add-word",
            "--text=你好",
            "--code=nihao",
            "--schema=pinyin",
        ]))
        .unwrap();
        assert_eq!(p.text, "你好");
        assert_eq!(p.code, "nihao");
        assert_eq!(p.schema, "pinyin");
        // 空格式
        let p2 = parse_add_word(&a(&[
            "exe",
            "--page=add-word",
            "--text",
            "好",
            "--schema",
            "wubi",
        ]))
        .unwrap();
        assert_eq!(p2.text, "好");
        assert_eq!(p2.code, "");
        assert_eq!(p2.schema, "wubi");
        // 仅 --page add-word 无预填
        let p3 = parse_add_word(&a(&["exe", "--page", "add-word"])).unwrap();
        assert_eq!(p3.text, "");
        // 非 add-word → None
        assert!(parse_add_word(&a(&["exe", "--page", "input"])).is_none());
    }
}
