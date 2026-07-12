/// 找出多个分组两两之间重叠的触发键，返回 (键, 分组A, 分组B)，每对只报一次。
pub fn find_key_conflicts(groups: &[(String, Vec<String>)]) -> Vec<(String, String, String)> {
    let mut hits = Vec::new();
    for i in 0..groups.len() {
        for j in (i + 1)..groups.len() {
            let (label_a, keys_a) = &groups[i];
            let (label_b, keys_b) = &groups[j];
            for key in keys_a {
                if keys_b.contains(key) {
                    hits.push((key.clone(), label_a.clone(), label_b.clone()));
                }
            }
        }
    }
    hits
}

/// 触发键内部名/候选组名 → 可读符号；未知键回退原名，避免提示里出现生硬的英文标识。
/// 触发键取值见 settings_manifest.toml 触发键 options（单键名）；候选组名为成组按键
/// （semicolon_quote 等，一名代表一组物理键），取值见"候选操作" section。
pub fn key_symbol(key: &str) -> &str {
    match key {
        // ── 触发键：单键名 ──
        "backtick" | "grave" => "`",
        "semicolon" => ";",
        "quote" | "apostrophe" => "'",
        "comma" => ",",
        "period" => ".",
        "slash" => "/",
        "backslash" => "\\",
        "open_bracket" => "[",
        "close_bracket" => "]",
        // ── 候选操作：成组键名（canon 后） ──
        "semicolon_quote" => "; '",
        "comma_period" => ", .",
        "minus_equal" => "- =",
        "brackets" => "[ ]",
        "tab" => "Tab/Shift+Tab",
        "arrows" => "方向键 ↑↓",
        "pageupdown" => "PageUp/PageDown",
        "lrshift" => "左/右 Shift",
        "lrctrl" => "左/右 Ctrl",
        other => other,
    }
}

/// 候选操作组名归一：`page_keys` 的 `shift_tab` 与 `highlight_keys` 的 `tab` 是同一对
/// 物理键（Tab / Shift+Tab），须归一到同一 token 才能检出"移动高亮 vs 翻页"冲突。
/// 其余组名域内同名即同键，原样返回。
pub fn canon_candidate_key(group: &str) -> &str {
    match group {
        "shift_tab" => "tab",
        other => other,
    }
}

/// 把冲突列表拼成一行提示文案；无冲突返回 None。键名以符号呈现（见 [`key_symbol`]）。
/// `scene` 为场景标签（如"触发键"/"候选键"），冠于文案前区分冲突域。
pub fn conflict_summary(hits: &[(String, String, String)], scene: &str) -> Option<String> {
    if hits.is_empty() {
        return None;
    }
    let parts: Vec<String> = hits
        .iter()
        .map(|(key, a, b)| format!("{} 键同时被「{a}」「{b}」使用", key_symbol(key)))
        .collect();
    Some(format!("⚠ {scene}冲突：{}", parts.join("；")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_conflict() {
        let groups = vec![
            ("临时拼音".to_string(), vec!["backtick".to_string()]),
            ("临时英文".to_string(), vec!["z".to_string()]),
        ];
        assert!(find_key_conflicts(&groups).is_empty());
    }

    #[test]
    fn test_single_conflict() {
        let groups = vec![
            ("临时拼音".to_string(), vec!["semicolon".to_string()]),
            ("快捷".to_string(), vec!["semicolon".to_string()]),
        ];
        let hits = find_key_conflicts(&groups);
        assert_eq!(hits, vec![("semicolon".to_string(), "临时拼音".to_string(), "快捷".to_string())]);
    }

    #[test]
    fn test_multi_key_multi_group_conflict() {
        let groups = vec![
            ("A".to_string(), vec!["x".to_string(), "y".to_string()]),
            ("B".to_string(), vec!["x".to_string()]),
            ("C".to_string(), vec!["y".to_string(), "x".to_string()]),
        ];
        let hits = find_key_conflicts(&groups);
        // A-B: x；A-C: x,y；B-C: x
        assert_eq!(hits.len(), 4);
        assert!(hits.contains(&("x".to_string(), "A".to_string(), "B".to_string())));
        assert!(hits.contains(&("x".to_string(), "A".to_string(), "C".to_string())));
        assert!(hits.contains(&("y".to_string(), "A".to_string(), "C".to_string())));
        assert!(hits.contains(&("x".to_string(), "B".to_string(), "C".to_string())));
    }

    #[test]
    fn test_empty_groups_or_keys() {
        assert!(find_key_conflicts(&[]).is_empty());
        let groups = vec![
            ("A".to_string(), vec![]),
            ("B".to_string(), vec!["x".to_string()]),
        ];
        assert!(find_key_conflicts(&groups).is_empty());
    }

    #[test]
    fn test_summary_empty_is_none() {
        assert_eq!(conflict_summary(&[], "触发键"), None);
    }

    #[test]
    fn test_summary_formats_hits() {
        let hits = vec![("semicolon".to_string(), "临时拼音".to_string(), "快捷".to_string())];
        let msg = conflict_summary(&hits, "触发键").unwrap();
        assert!(msg.contains("触发键冲突"), "应冠场景标签");
        assert!(msg.contains(';'), "内部键名应渲染为符号");
        assert!(!msg.contains("semicolon"), "不应再出现英文键名");
        assert!(msg.contains("临时拼音"));
        assert!(msg.contains("快捷"));
    }

    #[test]
    fn test_canon_candidate_key_unifies_tab_variants() {
        // highlight_keys 的 tab 与 page_keys 的 shift_tab 是同一对物理键，须归一。
        assert_eq!(canon_candidate_key("shift_tab"), "tab");
        assert_eq!(canon_candidate_key("tab"), "tab");
        assert_eq!(canon_candidate_key("comma_period"), "comma_period", "其余原样");
    }

    #[test]
    fn test_tab_and_shift_tab_conflict_after_canon() {
        // 高亮移动(tab) 与 翻页(shift_tab) 归一后应检出冲突。
        let groups = vec![
            ("高亮移动键".to_string(), vec![canon_candidate_key("tab").to_string()]),
            ("翻页键".to_string(), vec![canon_candidate_key("shift_tab").to_string()]),
        ];
        let hits = find_key_conflicts(&groups);
        assert_eq!(hits.len(), 1);
        let msg = conflict_summary(&hits, "候选键").unwrap();
        assert!(msg.contains("候选键冲突"));
        assert!(msg.contains("Tab"), "canon 后的 tab 应显示为 Tab/Shift+Tab");
    }

    #[test]
    fn test_candidate_group_symbols() {
        assert_eq!(key_symbol("comma_period"), ", .");
        assert_eq!(key_symbol("minus_equal"), "- =");
        assert_eq!(key_symbol("brackets"), "[ ]");
        assert_eq!(key_symbol("semicolon_quote"), "; '");
    }

    #[test]
    fn test_key_symbol_maps_known_and_falls_back() {
        assert_eq!(key_symbol("backtick"), "`");
        assert_eq!(key_symbol("semicolon"), ";");
        assert_eq!(key_symbol("quote"), "'");
        assert_eq!(key_symbol("comma"), ",");
        assert_eq!(key_symbol("period"), ".");
        assert_eq!(key_symbol("slash"), "/");
        assert_eq!(key_symbol("backslash"), "\\");
        assert_eq!(key_symbol("open_bracket"), "[");
        assert_eq!(key_symbol("close_bracket"), "]");
        assert_eq!(key_symbol("z"), "z", "未知键回退原名");
    }
}
