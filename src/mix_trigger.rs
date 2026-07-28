use serde_json::Value;

/// 从 mix_modes 数组里取出指定 id 项当前的 trigger_keys；找不到该 id 返回 None。
pub fn mix_mode_trigger_keys(mix_modes: &[Value], id: &str) -> Option<Vec<String>> {
    mix_modes
        .iter()
        .find(|m| m.get("id").and_then(Value::as_str) == Some(id))
        .map(|m| {
            m.get("trigger_keys")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        })
}

/// 把 mix_modes 数组里指定 id 项的 trigger_keys 替换为 new_keys，其余字段/其余数组元素原样保留。
/// 找不到该 id 时返回 None（调用方不应静默新建实例）。
pub fn patch_mix_mode_trigger_keys(
    mix_modes: &[Value],
    id: &str,
    new_keys: &[String],
) -> Option<Vec<Value>> {
    if !mix_modes
        .iter()
        .any(|m| m.get("id").and_then(Value::as_str) == Some(id))
    {
        return None;
    }
    let patched = mix_modes
        .iter()
        .map(|m| {
            if m.get("id").and_then(Value::as_str) == Some(id) {
                let mut m = m.clone();
                if let Some(obj) = m.as_object_mut() {
                    obj.insert(
                        "trigger_keys".to_string(),
                        Value::Array(new_keys.iter().cloned().map(Value::String).collect()),
                    );
                }
                m
            } else {
                m.clone()
            }
        })
        .collect();
    Some(patched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_mix_modes() -> Vec<Value> {
        vec![
            json!({
                "id": "quick_mix",
                "name": "快捷",
                "short_name": "快",
                "trigger_keys": ["semicolon"],
                "members": ["quick_input", "pinyin", "english"]
            }),
            json!({
                "id": "other_mix",
                "name": "其它",
                "short_name": "其",
                "trigger_keys": ["z"],
                "members": ["pinyin"]
            }),
        ]
    }

    #[test]
    fn test_mix_mode_trigger_keys_found() {
        let arr = sample_mix_modes();
        assert_eq!(
            mix_mode_trigger_keys(&arr, "quick_mix"),
            Some(vec!["semicolon".to_string()])
        );
    }

    #[test]
    fn test_mix_mode_trigger_keys_not_found() {
        let arr = sample_mix_modes();
        assert_eq!(mix_mode_trigger_keys(&arr, "no_such_id"), None);
    }

    #[test]
    fn test_patch_replaces_only_target_entry() {
        let arr = sample_mix_modes();
        let patched =
            patch_mix_mode_trigger_keys(&arr, "quick_mix", &["backtick".to_string()]).unwrap();
        assert_eq!(patched[0]["trigger_keys"], json!(["backtick"]));
        // 其余字段原样保留
        assert_eq!(patched[0]["name"], json!("快捷"));
        assert_eq!(
            patched[0]["members"],
            json!(["quick_input", "pinyin", "english"])
        );
        // 其它数组元素完全不动
        assert_eq!(patched[1], arr[1]);
    }

    #[test]
    fn test_patch_not_found_returns_none() {
        let arr = sample_mix_modes();
        assert_eq!(
            patch_mix_mode_trigger_keys(&arr, "no_such_id", &["z".to_string()]),
            None
        );
    }

    #[test]
    fn test_patch_empty_keys_clears_trigger() {
        let arr = sample_mix_modes();
        let patched = patch_mix_mode_trigger_keys(&arr, "quick_mix", &[]).unwrap();
        assert_eq!(patched[0]["trigger_keys"], json!([]));
    }
}
