//! `schema.special_modes`（特殊方案的引导键）的读写辅助。
//!
//! 与 [`crate::mix_trigger`] 同构，差别只有一处：那边**找不到就返回 None、绝不新建**
//! （quick_mix 是内置实例，设置页只该改它的触发键）；这边必须能新建——`special_modes`
//! 出厂是空数组，用户第一次给快符配引导键时根本没有条目可改。
//!
//! 两边共同的要害是**整表写回时不能丢字段**：`special_modes` 的条目还带
//! `show_all_on_enter` / `candidate_layout` / 注释模板等只能手写 config.toml 的项，
//! 用结构体重建条目会把它们静默抹掉。故这里一律在原始 `Value` 上改单个字段。

use serde_json::Value;

/// 条目按 `schema` 字段定位，不是 `id`。
///
/// 设置页是**按方案**打开的（「给快符这个方案配引导键」），而 `id` 只是实例标识、
/// 用户手写时可以取任意值。用 `schema` 匹配才对得上「这个方案的引导键」这个问题。
fn matches(entry: &Value, schema_id: &str) -> bool {
    entry.get("schema").and_then(Value::as_str) == Some(schema_id)
}

/// 取某方案当前的引导键；无对应条目返回 None（与「有条目但键为空」是两件事）。
pub fn special_mode_trigger_keys(modes: &[Value], schema_id: &str) -> Option<Vec<String>> {
    modes.iter().find(|m| matches(m, schema_id)).map(|m| {
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

/// 写入某方案的引导键，返回新数组。
///
/// - 已有条目 → 只改 `trigger_keys`，**其余字段原样保留**；
/// - 无条目且 `new_keys` 非空 → 追加最小条目（`id` / `name` / `schema` / `trigger_keys`）；
/// - 无条目且 `new_keys` 为空 → 原样返回，**不新建空条目**（没配过的方案不该在
///   config.toml 里凭空多出一段）。
///
/// 新条目必须写 `id`：`hotkey.rs` 的编译期对 `id` 为空的条目直接 `continue`，
/// 只写 `schema` 的话该方案后续配的直达热键会静默不注册。
pub fn upsert_special_mode_trigger_keys(
    modes: &[Value],
    schema_id: &str,
    display_name: &str,
    new_keys: &[String],
) -> Vec<Value> {
    let keys_json = Value::Array(new_keys.iter().cloned().map(Value::String).collect());

    if modes.iter().any(|m| matches(m, schema_id)) {
        return modes
            .iter()
            .map(|m| {
                if matches(m, schema_id) {
                    let mut m = m.clone();
                    if let Some(obj) = m.as_object_mut() {
                        obj.insert("trigger_keys".to_string(), keys_json.clone());
                    }
                    m
                } else {
                    m.clone()
                }
            })
            .collect();
    }

    let mut out = modes.to_vec();
    if !new_keys.is_empty() {
        out.push(serde_json::json!({
            "id": schema_id,
            "name": display_name,
            "schema": schema_id,
            "trigger_keys": keys_json,
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Vec<Value> {
        vec![json!({
            "id": "qsym",
            "name": "快符",
            "schema": "qsym",
            "trigger_keys": ["backslash"],
            "show_all_on_enter": true,
            "candidate_layout": "vertical",
        })]
    }

    /// 改引导键**不得动其它字段**。`show_all_on_enter` / `candidate_layout` 只能手写
    /// config.toml，用结构体重建条目会把它们静默抹掉——用户改一次引导键就丢配置。
    #[test]
    fn patch_preserves_hand_written_fields() {
        let out = upsert_special_mode_trigger_keys(&sample(), "qsym", "快符", &["grave".into()]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["trigger_keys"], json!(["grave"]));
        assert_eq!(out[0]["show_all_on_enter"], json!(true), "手写字段丢了");
        assert_eq!(out[0]["candidate_layout"], json!("vertical"));
    }

    /// 首次配置：special_modes 出厂是空数组，必须能新建条目。
    #[test]
    fn creates_entry_when_absent() {
        let out = upsert_special_mode_trigger_keys(&[], "kf", "快符", &["backslash".into()]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["schema"], json!("kf"));
        assert_eq!(out[0]["trigger_keys"], json!(["backslash"]));
        // id 必须写：hotkey 编译期对 id 为空的条目直接跳过，否则该方案的直达热键
        // 会静默不注册。
        assert_eq!(out[0]["id"], json!("kf"), "缺 id 会让直达热键静默失效");
        assert_eq!(out[0]["name"], json!("快符"));
    }

    /// 没配过 + 清空选择 = 什么也不做，不该在 config.toml 里凭空多出一段。
    #[test]
    fn no_entry_created_for_empty_selection() {
        let out = upsert_special_mode_trigger_keys(&[], "kf", "快符", &[]);
        assert!(out.is_empty(), "空选择不该新建条目：{out:?}");
    }

    /// 已有条目被清空引导键时**保留条目**——它可能还带着直达热键与其它手写配置。
    #[test]
    fn clearing_keys_keeps_existing_entry() {
        let out = upsert_special_mode_trigger_keys(&sample(), "qsym", "快符", &[]);
        assert_eq!(out.len(), 1, "清空引导键不该删掉整个条目");
        assert_eq!(out[0]["trigger_keys"], json!([]));
        assert_eq!(out[0]["show_all_on_enter"], json!(true));
    }

    /// 其余条目不受影响（多个特殊方案共存）。
    #[test]
    fn other_entries_untouched() {
        let mut modes = sample();
        modes.push(json!({ "id": "rare", "schema": "rare", "trigger_keys": ["grave"] }));
        let out = upsert_special_mode_trigger_keys(&modes, "qsym", "快符", &["slash".into()]);
        assert_eq!(out.len(), 2);
        let rare = out.iter().find(|m| m["schema"] == json!("rare")).unwrap();
        assert_eq!(rare["trigger_keys"], json!(["grave"]));
    }

    /// 读取：无条目返回 None，有条目但键为空返回 Some(vec![])——两者语义不同，
    /// 前者是「没配过」，后者是「配过又清空了」。
    #[test]
    fn read_distinguishes_absent_from_empty() {
        assert_eq!(special_mode_trigger_keys(&[], "kf"), None);
        let cleared = vec![json!({ "schema": "kf", "trigger_keys": [] })];
        assert_eq!(
            special_mode_trigger_keys(&cleared, "kf"),
            Some(Vec::<String>::new())
        );
    }
}
