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
        "lbracket" => "[",
        "rbracket" => "]",
        // ── 候选操作：成组键名（expand 后仍可能出现的组名） ──
        "semicolon_quote" => "; '",
        "comma_period" => ", .",
        "minus_equal" => "- =",
        "brackets" => "[ ]",
        "tab" => "Tab/Shift+Tab",
        "arrows" => "方向键 ↑↓",
        "pageupdown" => "PageUp/PageDown",
        "lrshift" => "左/右 Shift",
        "lrctrl" => "左/右 Ctrl",
        // ── 单修饰键（expand 展开后 / toggle_mode_keys 原值） ──
        "lshift" => "左Shift",
        "rshift" => "右Shift",
        "lctrl" => "左Ctrl",
        "rctrl" => "右Ctrl",
        "capslock" => "CapsLock",
        other => other,
    }
}

/// 候选操作组名展开成 canonical 物理键 token（用于候选域归一化检测）：
/// - `shift_tab`(翻页) 与 `tab`(高亮移动) 是同一对物理键（Tab/Shift+Tab），归一到 `tab`；
/// - `lrshift`/`lrctrl`(左右 Shift/Ctrl 次选) 展开成 `{lshift,rshift}`/`{lctrl,rctrl}`，
///   以便与 `keys.toggle_mode_keys`(中英切换，值为 lshift/rshift/…) 跨组匹配出冲突；
/// - 其余组名域内同名即同键，原样返回单元素。
pub fn expand_candidate_key(group: &str) -> Vec<&str> {
    match group {
        "shift_tab" => vec!["tab"],
        "lrshift" => vec!["lshift", "rshift"],
        "lrctrl" => vec!["lctrl", "rctrl"],
        other => vec![other],
    }
}

/// 组合键（功能快捷键）归一化为唯一显示形：拆分 `+`、大小写无关、修饰键按固定序
/// （Ctrl<Shift<Alt<Win）排前、主键在后。故 `shift+ctrl+e` 与 `ctrl+shift+e` 归一后相等，
/// 既用于冲突比较也直接作显示（如 `Ctrl+Shift+E`、`Ctrl+数字`）。空/`none` 由调用方先行剔除。
pub fn canonical_hotkey(s: &str) -> String {
    fn display_main(k: &str) -> String {
        match k {
            "number" => "数字".to_string(),
            "equal" => "=".to_string(),
            "minus" => "-".to_string(),
            "space" => "Space".to_string(),
            _ => {
                let mut c = k.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            }
        }
    }
    let mut mods: Vec<(u8, &str)> = Vec::new();
    let mut mains: Vec<String> = Vec::new();
    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => mods.push((0, "Ctrl")),
            "shift" => mods.push((1, "Shift")),
            "alt" => mods.push((2, "Alt")),
            "win" | "super" | "meta" | "cmd" => mods.push((3, "Win")),
            other => mains.push(display_main(other)),
        }
    }
    mods.sort_by_key(|(o, _)| *o);
    mods.dedup_by_key(|(o, _)| *o);
    let mut out: Vec<String> = mods.into_iter().map(|(_, m)| m.to_string()).collect();
    out.extend(mains);
    out.join("+")
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
        assert_eq!(
            hits,
            vec![(
                "semicolon".to_string(),
                "临时拼音".to_string(),
                "快捷".to_string()
            )]
        );
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
        let hits = vec![(
            "semicolon".to_string(),
            "临时拼音".to_string(),
            "快捷".to_string(),
        )];
        let msg = conflict_summary(&hits, "触发键").unwrap();
        assert!(msg.contains("触发键冲突"), "应冠场景标签");
        assert!(msg.contains(';'), "内部键名应渲染为符号");
        assert!(!msg.contains("semicolon"), "不应再出现英文键名");
        assert!(msg.contains("临时拼音"));
        assert!(msg.contains("快捷"));
    }

    #[test]
    fn test_expand_candidate_key() {
        // highlight_keys 的 tab 与 page_keys 的 shift_tab 是同一对物理键，归一到 tab。
        assert_eq!(expand_candidate_key("shift_tab"), vec!["tab"]);
        assert_eq!(expand_candidate_key("tab"), vec!["tab"]);
        // lrshift/lrctrl 展开成单修饰键，供与 toggle_mode_keys 跨组匹配。
        assert_eq!(expand_candidate_key("lrshift"), vec!["lshift", "rshift"]);
        assert_eq!(expand_candidate_key("lrctrl"), vec!["lctrl", "rctrl"]);
        assert_eq!(
            expand_candidate_key("comma_period"),
            vec!["comma_period"],
            "其余原样"
        );
    }

    fn expanded(name: &str, group: &str) -> (String, Vec<String>) {
        (
            name.to_string(),
            expand_candidate_key(group)
                .into_iter()
                .map(String::from)
                .collect(),
        )
    }

    #[test]
    fn test_tab_and_shift_tab_conflict_after_expand() {
        // 高亮移动(tab) 与 翻页(shift_tab) 归一后应检出冲突。
        let groups = vec![
            expanded("高亮移动键", "tab"),
            expanded("翻页键", "shift_tab"),
        ];
        let hits = find_key_conflicts(&groups);
        assert_eq!(hits.len(), 1);
        let msg = conflict_summary(&hits, "候选键").unwrap();
        assert!(msg.contains("候选键冲突"));
        assert!(msg.contains("Tab"), "归一后的 tab 应显示为 Tab/Shift+Tab");
    }

    #[test]
    fn test_lrshift_vs_toggle_mode_conflict_after_expand() {
        // 次选键 lrshift 展开成 {lshift,rshift}，与中英切换键 rshift 争用右 Shift → 冲突。
        let groups = vec![
            expanded("次选/三选键", "lrshift"),
            ("中英切换键".to_string(), vec!["rshift".to_string()]),
        ];
        let hits = find_key_conflicts(&groups);
        assert_eq!(hits.len(), 1);
        let msg = conflict_summary(&hits, "候选键").unwrap();
        assert!(msg.contains("右Shift"));
    }

    #[test]
    fn test_candidate_group_symbols() {
        assert_eq!(key_symbol("comma_period"), ", .");
        assert_eq!(key_symbol("minus_equal"), "- =");
        assert_eq!(key_symbol("brackets"), "[ ]");
        assert_eq!(key_symbol("semicolon_quote"), "; '");
    }

    #[test]
    fn test_canonical_hotkey_is_order_and_case_insensitive() {
        // 修饰键顺序/大小写无关，归一后相等 → 可检出冲突。
        assert_eq!(canonical_hotkey("ctrl+shift+e"), "Ctrl+Shift+E");
        assert_eq!(canonical_hotkey("Shift+Ctrl+E"), "Ctrl+Shift+E");
        assert_eq!(canonical_hotkey("ctrl+number"), "Ctrl+数字");
        assert_eq!(canonical_hotkey("ctrl+equal"), "Ctrl+=");
        assert_eq!(canonical_hotkey("shift+space"), "Shift+Space");
        // cmd 与 win/super/meta 同视为 Win 修饰键（与 manifest.rs 解析一致）。
        assert_eq!(canonical_hotkey("cmd+e"), canonical_hotkey("win+e"));
        // 两种写法归一后相等，find_key_conflicts 能检出。
        let a = canonical_hotkey("ctrl+shift+j");
        let b = canonical_hotkey("shift+ctrl+j");
        assert_eq!(a, b);
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
        assert_eq!(key_symbol("lbracket"), "[");
        assert_eq!(key_symbol("rbracket"), "]");
        assert_eq!(key_symbol("z"), "z", "未知键回退原名");
    }
}
