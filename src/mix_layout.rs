//! 融合模式候选布局（`schema.mix_modes[*].candidate_layout`）的读写纯逻辑。
//!
//! core 侧是**每实例**的三态字段（`follow` / `vertical` / `horizontal`，见
//! WindInput 的 `docs/design/mode-candidate-layout.md`）。设置界面只暴露内置
//! `quick_mix` 那一项——与触发键、候选来源两项同构，都是「整体读写 structlist +
//! 按 id 打补丁」，因为 core 侧 `mix_modes` 的定义真相源在代码里，预置文件不写它。
//!
//! 它取代了旧的 `schema.quick_input.force_vertical`：那个布尔挂在与实例无关的全局段，
//! 判定条件却是「这个 mix 实例含 quick 成员」。

use serde_json::Value;

/// core 侧 `LayoutIntent` 的默认值。`quick_mix` 出厂是 `vertical`（等价旧
/// `force_vertical = true`），但**那是实例的默认、不是类型的默认**，故此处只用作
/// 「数组里根本没有该项」时的兜底，正常路径一律读实际值。
pub const LAYOUT_FALLBACK: &str = "follow";

/// 从 mix_modes 数组里取出指定 id 项当前的 candidate_layout；找不到该 id 返回 None。
///
/// 找到了但字段缺失（老配置快照 / 手写配置没写这一项）返回 [`LAYOUT_FALLBACK`]，
/// 与 core 的 `#[serde(default)]` 语义一致。
pub fn mix_mode_layout(mix_modes: &[Value], id: &str) -> Option<String> {
    mix_modes
        .iter()
        .find(|m| m.get("id").and_then(Value::as_str) == Some(id))
        .map(|m| {
            m.get("candidate_layout")
                .and_then(Value::as_str)
                .unwrap_or(LAYOUT_FALLBACK)
                .to_string()
        })
}

/// 把 mix_modes 数组里指定 id 项的 candidate_layout 替换为 `layout`，
/// 其余字段/其余数组元素原样保留。
/// 找不到该 id 时返回 None（调用方不应静默新建实例）。
pub fn patch_mix_mode_layout(mix_modes: &[Value], id: &str, layout: &str) -> Option<Vec<Value>> {
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
                        "candidate_layout".to_string(),
                        Value::String(layout.to_string()),
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

    fn sample() -> Vec<Value> {
        vec![
            json!({
                "id": "quick_mix",
                "trigger_keys": ["semicolon"],
                "members": ["quick_input.calc"],
                "candidate_layout": "vertical",
            }),
            json!({
                "id": "my_mix",
                "trigger_keys": ["z"],
                "members": ["pinyin"],
                "candidate_layout": "horizontal",
            }),
        ]
    }

    #[test]
    fn reads_layout_of_named_instance() {
        assert_eq!(
            mix_mode_layout(&sample(), "quick_mix").as_deref(),
            Some("vertical")
        );
        assert_eq!(
            mix_mode_layout(&sample(), "my_mix").as_deref(),
            Some("horizontal")
        );
        assert_eq!(mix_mode_layout(&sample(), "nope"), None);
    }

    /// 字段缺失（老快照/手写配置）按 core 的 serde 默认解释，而不是当成「没有这个实例」。
    #[test]
    fn missing_field_reads_as_follow() {
        let arr = vec![json!({"id": "quick_mix", "members": []})];
        assert_eq!(
            mix_mode_layout(&arr, "quick_mix").as_deref(),
            Some("follow")
        );
    }

    #[test]
    fn patch_only_touches_named_instance() {
        let patched = patch_mix_mode_layout(&sample(), "quick_mix", "horizontal").expect("应命中");
        assert_eq!(
            mix_mode_layout(&patched, "quick_mix").as_deref(),
            Some("horizontal")
        );
        assert_eq!(
            mix_mode_layout(&patched, "my_mix").as_deref(),
            Some("horizontal"),
            "另一实例应原样保留"
        );
        // 同实例的其余字段不能被打补丁顺手清掉。
        let quick = patched
            .iter()
            .find(|m| m.get("id").and_then(Value::as_str) == Some("quick_mix"))
            .expect("quick_mix 应仍在");
        assert!(quick.get("members").is_some(), "members 应原样保留");
        assert!(
            quick.get("trigger_keys").is_some(),
            "trigger_keys 应原样保留"
        );
    }

    /// 字段原本缺失时补丁应新增它，而不是无声跳过。
    #[test]
    fn patch_inserts_missing_field() {
        let arr = vec![json!({"id": "quick_mix", "members": []})];
        let patched = patch_mix_mode_layout(&arr, "quick_mix", "vertical").expect("应命中");
        assert_eq!(
            mix_mode_layout(&patched, "quick_mix").as_deref(),
            Some("vertical")
        );
    }

    #[test]
    fn patch_returns_none_for_unknown_id() {
        assert!(patch_mix_mode_layout(&sample(), "nope", "vertical").is_none());
    }
}
