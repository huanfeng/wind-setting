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

/// 确保某方案在 `special_modes` 里有条目（供 `special:<id>` 绑定指向），并**清空**其
/// `trigger_keys`。
///
/// 五c 收编后引导键的落点是全局 `keys.key_actions`，条目本身只承载实例定义
/// （`show_all_on_enter` / 注释模板 / 显示名等）。旧 `trigger_keys` 字段一并清掉——
/// core 侧 `normalize` 反正会忽略它，但留在 config.toml 里会让人以为它还管用。
///
/// `need_entry=false` 且原本无条目时不新建：没配过的方案不该在 config.toml 里凭空
/// 多出一段。新条目必须写 `id`——`hotkey.rs` 编译期对 `id` 为空的条目直接 `continue`，
/// 只写 `schema` 的话该方案后续配的直达热键会静默不注册。
pub fn ensure_special_mode_entry(
    modes: &[Value],
    schema_id: &str,
    display_name: &str,
    need_entry: bool,
) -> Vec<Value> {
    if modes.iter().any(|m| matches(m, schema_id)) {
        return modes
            .iter()
            .map(|m| {
                if matches(m, schema_id) {
                    let mut m = m.clone();
                    if let Some(obj) = m.as_object_mut() {
                        obj.remove("trigger_keys");
                    }
                    m
                } else {
                    m.clone()
                }
            })
            .collect();
    }
    let mut out = modes.to_vec();
    if need_entry {
        out.push(serde_json::json!({
            "id": schema_id,
            "name": display_name,
            "schema": schema_id,
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

    /// 维护条目**不得动其它字段**。`show_all_on_enter` / `candidate_layout` 只能手写
    /// config.toml，用结构体重建条目会把它们静默抹掉——用户改一次引导键就丢配置。
    #[test]
    fn patch_preserves_hand_written_fields() {
        let out = ensure_special_mode_entry(&sample(), "qsym", "快符", true);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["show_all_on_enter"], json!(true), "手写字段丢了");
        assert_eq!(out[0]["candidate_layout"], json!("vertical"));
        assert_eq!(out[0]["name"], json!("快符"));
    }

    /// 旧的 `trigger_keys` 字段要清掉：五c 之后它没有任何消费者
    /// （core 的 `normalize` 会忽略），留着只会让人以为它还管用。
    #[test]
    fn legacy_trigger_keys_field_is_stripped() {
        let out = ensure_special_mode_entry(&sample(), "qsym", "快符", true);
        assert!(
            out[0].get("trigger_keys").is_none(),
            "旧字段应清除，实际 {out:?}"
        );
    }

    /// 首次配置：special_modes 出厂是空数组，必须能新建条目
    /// （`special:<id>` 得指向一个真实存在的实例定义）。
    #[test]
    fn creates_entry_when_absent() {
        let out = ensure_special_mode_entry(&[], "kf", "快符", true);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["schema"], json!("kf"));
        // id 必须写：hotkey 编译期对 id 为空的条目直接跳过，否则该方案的直达热键
        // 会静默不注册。
        assert_eq!(out[0]["id"], json!("kf"), "缺 id 会让直达热键静默失效");
        assert_eq!(out[0]["name"], json!("快符"));
    }

    /// 没配过 + 无需条目 = 什么也不做，不该在 config.toml 里凭空多出一段。
    #[test]
    fn no_entry_created_when_not_needed() {
        let out = ensure_special_mode_entry(&[], "kf", "快符", false);
        assert!(out.is_empty(), "不需要时不该新建条目：{out:?}");
    }

    /// 已有条目在清空引导键后**保留**——它可能还带着直达热键与其它手写配置。
    #[test]
    fn clearing_keys_keeps_existing_entry() {
        let out = ensure_special_mode_entry(&sample(), "qsym", "快符", false);
        assert_eq!(out.len(), 1, "清空引导键不该删掉整个条目");
        assert_eq!(out[0]["show_all_on_enter"], json!(true));
    }

    /// 其余条目不受影响（多个特殊方案共存）。
    #[test]
    fn other_entries_untouched() {
        let mut modes = sample();
        modes.push(json!({ "id": "rare", "schema": "rare", "show_all_on_enter": false }));
        let out = ensure_special_mode_entry(&modes, "qsym", "快符", true);
        assert_eq!(out.len(), 2);
        let rare = out.iter().find(|m| m["schema"] == json!("rare")).unwrap();
        assert_eq!(rare["show_all_on_enter"], json!(false));
    }
}
