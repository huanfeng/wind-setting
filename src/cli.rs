//! 命令行参数解析:`--page` / `--tab` / 快捷 `--<page>` / `windinput://` 协议。
//! 冷启动与二次实例共用同一套解析。

/// 规范页 id,顺序对齐 `pages::all_tabs`(方案/输入/按键/外观/词库/统计/高级/关于)。
const PAGES: &[&str] = &[
    "schema", "input", "keys", "ui", "dict", "stats", "advanced", "about",
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
}
