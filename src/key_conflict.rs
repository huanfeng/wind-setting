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

/// 触发键内部名 → 可读符号；未知键回退原名，避免提示里出现生硬的英文标识（如 backtick）。
/// 取值域见 settings_manifest.toml 的触发键 options（backtick/semicolon/quote）。
pub fn key_symbol(key: &str) -> &str {
    match key {
        "backtick" | "grave" => "`",
        "semicolon" => ";",
        "quote" | "apostrophe" => "'",
        other => other,
    }
}

/// 把冲突列表拼成一行提示文案；无冲突返回 None。键名以符号呈现（见 [`key_symbol`]）。
pub fn conflict_summary(hits: &[(String, String, String)]) -> Option<String> {
    if hits.is_empty() {
        return None;
    }
    let parts: Vec<String> = hits
        .iter()
        .map(|(key, a, b)| format!("{} 键同时被「{a}」「{b}」使用", key_symbol(key)))
        .collect();
    Some(format!("⚠ 触发键冲突：{}", parts.join("；")))
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
        assert_eq!(conflict_summary(&[]), None);
    }

    #[test]
    fn test_summary_formats_hits() {
        let hits = vec![("semicolon".to_string(), "临时拼音".to_string(), "快捷".to_string())];
        let msg = conflict_summary(&hits).unwrap();
        assert!(msg.contains(';'), "内部键名应渲染为符号");
        assert!(!msg.contains("semicolon"), "不应再出现英文键名");
        assert!(msg.contains("临时拼音"));
        assert!(msg.contains("快捷"));
    }

    #[test]
    fn test_key_symbol_maps_known_and_falls_back() {
        assert_eq!(key_symbol("backtick"), "`");
        assert_eq!(key_symbol("semicolon"), ";");
        assert_eq!(key_symbol("quote"), "'");
        assert_eq!(key_symbol("z"), "z", "未知键回退原名");
    }
}
