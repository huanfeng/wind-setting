//! 融合模式**每实例字段**（`schema.mix_modes[*].<字段>`）的读写纯逻辑。
//!
//! core 侧 `mix_modes` 是一个 structlist，设置界面只暴露内置 `quick_mix` 那一项——
//! 与触发键、候选来源两项同构，都是「整体读写 structlist + 按 id 打补丁」，因为
//! core 侧 `mix_modes` 的定义真相源在代码里，预置文件不写它。
//!
//! 目前经此读写的字段：
//! - `candidate_layout`：候选窗布局三态（见 WindInput 的
//!   `docs/design/mode-candidate-layout.md`）。取代了旧的
//!   `schema.quick_input.force_vertical`——那个布尔挂在与实例无关的全局段，判定条件
//!   却是「这个 mix 实例含 quick 成员」。
//! - `free_input`：自由（字面）输入三态。
//! - `free_input_takes_select_keys`：自由输入是否夺取二三候选键作字面。
//! - `numeric_letter_select`：数字透镜（数字/符号开头）下是否拿字母作选词键。
//!
//! 本模块原名 `mix_layout`，只为 `candidate_layout` 写死。加第二、第三个字段时若照抄，
//! 就会有三份同构的「按 id 定位 + 读字段 / 打补丁」逻辑，故泛化为按字段名存取。
//!
//! # 字段缺失一律回落到 core 的 serde 默认
//!
//! 读取器的 `fallback` 参数**必须与 core 的 `#[serde(default …)]` 一致**：用户配置里
//! 那份 `mix_modes` 常常是老版本写下的冻结快照，新字段根本不在里面。回落值取错会让
//! 界面显示的状态与实际生效值相反——而这类分叉不会有任何编译期或运行期报错。

use serde_json::Value;

/// 候选窗布局字段名。
pub const FIELD_CANDIDATE_LAYOUT: &str = "candidate_layout";
/// 自由输入字段名。
pub const FIELD_FREE_INPUT: &str = "free_input";
/// 自由输入是否夺取二三候选键的字段名。
pub const FIELD_FREE_INPUT_TAKES_SELECT_KEYS: &str = "free_input_takes_select_keys";

/// `FreeInputMode` 的 core 侧 serde 默认（`#[default] Auto`）。
pub const FREE_INPUT_FALLBACK: &str = "auto";
/// `free_input_takes_select_keys` 的 core 侧 serde 默认（`default_true`）。
pub const FREE_INPUT_TAKES_SELECT_KEYS_FALLBACK: bool = true;
/// 数字透镜下字母选词的字段名。
pub const FIELD_NUMERIC_LETTER_SELECT: &str = "numeric_letter_select";
/// `numeric_letter_select` 的 core 侧 serde 默认（`default_true`）。
pub const NUMERIC_LETTER_SELECT_FALLBACK: bool = true;

/// 定位 mix_modes 数组里 `id` 等于给定值的那一项。
fn find_mode<'a>(mix_modes: &'a [Value], id: &str) -> Option<&'a Value> {
    mix_modes
        .iter()
        .find(|m| m.get("id").and_then(Value::as_str) == Some(id))
}

/// 读字符串字段。**实例不存在**返回 None；实例在但字段缺失返回 `fallback`
/// （与 core 的 `#[serde(default)]` 语义一致，见模块文档）。
pub fn mix_mode_field_str(
    mix_modes: &[Value],
    id: &str,
    field: &str,
    fallback: &str,
) -> Option<String> {
    find_mode(mix_modes, id).map(|m| {
        m.get(field)
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_string()
    })
}

/// 读布尔字段，语义同 [`mix_mode_field_str`]。
pub fn mix_mode_field_bool(
    mix_modes: &[Value],
    id: &str,
    field: &str,
    fallback: bool,
) -> Option<bool> {
    find_mode(mix_modes, id).map(|m| m.get(field).and_then(Value::as_bool).unwrap_or(fallback))
}

/// 把指定 id 项的 `field` 替换为 `value`，其余字段与其余数组元素原样保留。
/// 找不到该 id 时返回 None（调用方不应静默新建实例）。
pub fn patch_mix_mode_field(
    mix_modes: &[Value],
    id: &str,
    field: &str,
    value: Value,
) -> Option<Vec<Value>> {
    find_mode(mix_modes, id)?;
    let patched = mix_modes
        .iter()
        .map(|m| {
            if m.get("id").and_then(Value::as_str) == Some(id) {
                let mut m = m.clone();
                if let Some(obj) = m.as_object_mut() {
                    obj.insert(field.to_string(), value.clone());
                }
                m
            } else {
                m.clone()
            }
        })
        .collect();
    Some(patched)
}

/// core 侧 `LayoutIntent` 的默认值。`quick_mix` 出厂是 `vertical`（等价旧
/// `force_vertical = true`），但**那是实例的默认、不是类型的默认**，故此处只用作
/// 「数组里根本没有该项」时的兜底，正常路径一律读实际值。
pub const LAYOUT_FALLBACK: &str = "follow";

/// 取指定 id 项当前的 `candidate_layout`（[`mix_mode_field_str`] 的具名封装）。
pub fn mix_mode_layout(mix_modes: &[Value], id: &str) -> Option<String> {
    mix_mode_field_str(mix_modes, id, FIELD_CANDIDATE_LAYOUT, LAYOUT_FALLBACK)
}

/// 给指定 id 项的 `candidate_layout` 打补丁（[`patch_mix_mode_field`] 的具名封装）。
pub fn patch_mix_mode_layout(mix_modes: &[Value], id: &str, layout: &str) -> Option<Vec<Value>> {
    patch_mix_mode_field(
        mix_modes,
        id,
        FIELD_CANDIDATE_LAYOUT,
        Value::String(layout.to_string()),
    )
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

    // ── 泛化后的按字段存取 ──

    /// 布尔字段：读、打补丁、以及**字段缺失回落 core 默认**三条一起锁。
    ///
    /// 回落值取错不会有任何报错，只会让界面显示与实际生效值相反——而用户配置里那份
    /// `mix_modes` 常常是老版本写下的冻结快照，新字段根本不在里面，这条路径是常态而非边角。
    #[test]
    fn bool_field_round_trip_and_fallback() {
        let arr = sample();
        assert_eq!(
            mix_mode_field_bool(
                &arr,
                "quick_mix",
                FIELD_FREE_INPUT_TAKES_SELECT_KEYS,
                FREE_INPUT_TAKES_SELECT_KEYS_FALLBACK
            ),
            Some(true),
            "字段缺失应回落 core 的 default_true"
        );
        let patched = patch_mix_mode_field(
            &arr,
            "quick_mix",
            FIELD_FREE_INPUT_TAKES_SELECT_KEYS,
            Value::Bool(false),
        )
        .expect("应命中 quick_mix");
        assert_eq!(
            mix_mode_field_bool(
                &patched,
                "quick_mix",
                FIELD_FREE_INPUT_TAKES_SELECT_KEYS,
                FREE_INPUT_TAKES_SELECT_KEYS_FALLBACK
            ),
            Some(false),
            "打完补丁应读回 false，而不是继续回落默认"
        );
        // 另一实例不得被顺带改写。
        assert_eq!(
            mix_mode_field_bool(
                &patched,
                "my_mix",
                FIELD_FREE_INPUT_TAKES_SELECT_KEYS,
                FREE_INPUT_TAKES_SELECT_KEYS_FALLBACK
            ),
            Some(true)
        );
    }

    /// 字符串字段（free_input）同理，并确认 fallback 是 core 的 `auto` 而非空串。
    #[test]
    fn str_field_round_trip_and_fallback() {
        let arr = sample();
        assert_eq!(
            mix_mode_field_str(&arr, "quick_mix", FIELD_FREE_INPUT, FREE_INPUT_FALLBACK).as_deref(),
            Some("auto")
        );
        let patched = patch_mix_mode_field(
            &arr,
            "quick_mix",
            FIELD_FREE_INPUT,
            Value::String("off".into()),
        )
        .expect("应命中 quick_mix");
        assert_eq!(
            mix_mode_field_str(&patched, "quick_mix", FIELD_FREE_INPUT, FREE_INPUT_FALLBACK)
                .as_deref(),
            Some("off")
        );
        // 打补丁不得清掉同实例的其它字段（布局那条已有测试，这里锁新字段的对称面）。
        assert_eq!(
            mix_mode_layout(&patched, "quick_mix").as_deref(),
            Some("vertical")
        );
    }

    /// 实例不存在与「实例在但字段缺失」是两种不同的结果，不能被压成同一个值。
    #[test]
    fn missing_instance_differs_from_missing_field() {
        let arr = sample();
        assert_eq!(
            mix_mode_field_bool(&arr, "nope", FIELD_FREE_INPUT_TAKES_SELECT_KEYS, true),
            None,
            "实例不存在应为 None"
        );
        assert_eq!(
            mix_mode_field_bool(&arr, "quick_mix", FIELD_FREE_INPUT_TAKES_SELECT_KEYS, true),
            Some(true),
            "实例在、字段缺失应为 Some(fallback)"
        );
        assert!(
            patch_mix_mode_field(&arr, "nope", FIELD_FREE_INPUT, Value::String("off".into()))
                .is_none()
        );
    }
}
