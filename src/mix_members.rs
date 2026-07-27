//! 融合模式成员（候选来源）的读写纯逻辑。
//!
//! core 侧 `schema.mix_modes[*].members` 是候选来源的单一真相源——**有无即开关、
//! 顺序即优先级**。设置界面只暴露「开关」这一维（一组勾选），顺序留给手写配置，
//! 因此写回时必须**保序**：既不能打乱用户自定义的排列，也不能丢掉界面不认识的成员
//! （用户可能把别的方案 id 加进了这个 mix）。

use serde_json::Value;

/// 从 mix_modes 数组里取出指定 id 项当前的 members；找不到该 id 返回 None。
pub fn mix_mode_members(mix_modes: &[Value], id: &str) -> Option<Vec<String>> {
    mix_modes
        .iter()
        .find(|m| m.get("id").and_then(Value::as_str) == Some(id))
        .map(|m| {
            m.get("members")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        })
}

/// 按勾选结果重排 members，保留原有顺序与界面不认识的成员。
///
/// - `managed`：界面管理的成员全集，**按界面声明序**（新增项的插入位置由它决定）；
/// - `selected`：本次勾选的成员（`managed` 的子集）。
///
/// 规则：原 members 中「managed 且未勾选」的被删除，其余原样留在原位；「勾选了但原本
/// 不在 members 里」的按 `managed` 序插到第一个序号更大的已有成员之前（都没有则追加）。
fn reorder_members(current: &[String], managed: &[String], selected: &[String]) -> Vec<String> {
    let rank = |m: &str| managed.iter().position(|x| x == m);
    // 1. 保留：非管理成员原样，管理成员仅保留已勾选的
    let mut out: Vec<String> = current
        .iter()
        .filter(|m| match rank(m) {
            Some(_) => selected.iter().any(|s| s == *m),
            None => true, // 界面不认识的成员（自定义方案 id）一律保留
        })
        .cloned()
        .collect();
    // 2. 插入新勾选的，按 managed 序找位置
    for m in managed
        .iter()
        .filter(|m| selected.iter().any(|s| s == *m) && !current.iter().any(|c| c == *m))
    {
        let my_rank = rank(m).unwrap_or(usize::MAX);
        let at = out
            .iter()
            .position(|o| rank(o).is_some_and(|r| r > my_rank))
            .unwrap_or(out.len());
        out.insert(at, m.clone());
    }
    out
}

/// 把 mix_modes 数组里指定 id 项的 members 按勾选结果替换，其余字段/数组元素原样保留。
/// 找不到该 id 时返回 None（调用方不应静默新建实例）。
pub fn patch_mix_mode_members(
    mix_modes: &[Value],
    id: &str,
    managed: &[String],
    selected: &[String],
) -> Option<Vec<Value>> {
    let current = mix_mode_members(mix_modes, id)?;
    let next = reorder_members(&current, managed, selected);
    let patched = mix_modes
        .iter()
        .map(|m| {
            if m.get("id").and_then(Value::as_str) == Some(id) {
                let mut m = m.clone();
                if let Some(obj) = m.as_object_mut() {
                    obj.insert(
                        "members".to_string(),
                        Value::Array(next.iter().cloned().map(Value::String).collect()),
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

    const MANAGED: [&str; 6] = [
        "quick_input.calc",
        "quick_input.date",
        "quick_input.number",
        "quick_input.repeat",
        "$primary_pinyin",
        "english",
    ];

    fn managed() -> Vec<String> {
        MANAGED.iter().map(|s| s.to_string()).collect()
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn sample() -> Vec<Value> {
        vec![
            json!({
                "id": "quick_mix",
                "name": "快捷",
                "trigger_keys": ["semicolon"],
                "members": MANAGED,
            }),
            json!({ "id": "other_mix", "members": ["pinyin"] }),
        ]
    }

    #[test]
    fn reads_members() {
        assert_eq!(mix_mode_members(&sample(), "quick_mix"), Some(managed()));
        assert_eq!(mix_mode_members(&sample(), "no_such"), None);
    }

    #[test]
    fn unchecking_removes_only_that_member() {
        let out = reorder_members(
            &managed(),
            &managed(),
            &strs(&[
                "quick_input.calc",
                "quick_input.date",
                "quick_input.number",
                "quick_input.repeat",
                "english",
            ]),
        );
        assert_eq!(
            out,
            strs(&[
                "quick_input.calc",
                "quick_input.date",
                "quick_input.number",
                "quick_input.repeat",
                "english"
            ]),
            "取消拼音只应删掉它，其余保持原位"
        );
    }

    #[test]
    fn rechecking_inserts_at_declared_position() {
        let current = strs(&["quick_input.calc", "english"]);
        let out = reorder_members(
            &current,
            &managed(),
            &strs(&["quick_input.calc", "$primary_pinyin", "english"]),
        );
        assert_eq!(
            out,
            strs(&["quick_input.calc", "$primary_pinyin", "english"]),
            "重新勾选应插到 english 之前（声明序）"
        );
    }

    #[test]
    fn preserves_user_custom_order() {
        // 用户手写把拼音排到了最前
        let current = strs(&["$primary_pinyin", "quick_input.calc", "english"]);
        let out = reorder_members(&current, &managed(), &current.clone());
        assert_eq!(out, current, "全勾选时保存不应重排用户的自定义顺序");
    }

    #[test]
    fn keeps_unmanaged_members() {
        // 用户往这个 mix 里加了别的方案
        let current = strs(&["quick_input.calc", "wubi86", "english"]);
        let out = reorder_members(
            &current,
            &managed(),
            &strs(&["quick_input.calc", "english"]),
        );
        assert_eq!(
            out,
            strs(&["quick_input.calc", "wubi86", "english"]),
            "界面不认识的成员必须原样保留"
        );
    }

    #[test]
    fn appends_when_no_later_member_exists() {
        let current = strs(&["quick_input.calc"]);
        let out = reorder_members(
            &current,
            &managed(),
            &strs(&["quick_input.calc", "english"]),
        );
        assert_eq!(out, strs(&["quick_input.calc", "english"]));
    }

    #[test]
    fn inserts_into_empty_members() {
        let out = reorder_members(&[], &managed(), &strs(&["english", "quick_input.calc"]));
        assert_eq!(
            out,
            strs(&["quick_input.calc", "english"]),
            "空 members 按声明序插入"
        );
    }

    #[test]
    fn patch_touches_only_target_entry() {
        let arr = sample();
        let patched = patch_mix_mode_members(
            &arr,
            "quick_mix",
            &managed(),
            &strs(&["quick_input.calc", "english"]),
        )
        .unwrap();
        assert_eq!(
            patched[0]["members"],
            json!(["quick_input.calc", "english"])
        );
        assert_eq!(patched[0]["name"], json!("快捷"), "其余字段原样保留");
        assert_eq!(patched[0]["trigger_keys"], json!(["semicolon"]));
        assert_eq!(patched[1], arr[1], "其它数组元素完全不动");
    }

    #[test]
    fn patch_not_found_returns_none() {
        assert_eq!(
            patch_mix_mode_members(&sample(), "no_such", &managed(), &managed()),
            None
        );
    }

    #[test]
    fn patch_all_unchecked_clears_managed_only() {
        let arr = sample();
        let patched = patch_mix_mode_members(&arr, "quick_mix", &managed(), &[]).unwrap();
        assert_eq!(patched[0]["members"], json!([]));
    }
}
