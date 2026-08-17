//! 融合模式成员（候选来源）的读写纯逻辑。
//!
//! core 侧 `schema.mix_modes[*].members` 是候选来源的单一真相源——**有无即开关、
//! 顺序即优先级**。设置界面两维都管：一组开关决定有无，拖拽手柄决定顺序。
//!
//! 写回时仍必须保留**界面不认识的成员**（用户可能把别的方案 id 手写进了这个 mix），
//! 且不能把它们冲到列表末尾——它们在原 members 里的相对位置也是用户的编排意图。

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

/// 某成员在界面声明序中的序号。
fn rank(managed: &[String], m: &str) -> Option<usize> {
    managed.iter().position(|x| x == m)
}

/// 界面行序：**以当前配置的顺序为准**，界面管理但配置里还没有的成员按声明序补位。
///
/// 列表打开时用户看到的就是配置里的真实优先级——这是拖拽排序能成立的前提：若界面
/// 恒按声明序展示，用户手写的顺序会在他还没动任何东西时就"看起来"被改过了。
///
/// 补位规则同旧版的重新勾选：插到第一个序号更大的已有成员之前（都没有则追加）。
pub fn managed_order(current: &[String], managed: &[String]) -> Vec<String> {
    let mut out: Vec<String> = current
        .iter()
        .filter(|m| rank(managed, m).is_some())
        .cloned()
        .collect();
    // 先收集缺席项再插入：过滤闭包借着 `out`，边遍历边插会与之打架。
    let missing: Vec<String> = managed
        .iter()
        .filter(|m| !out.iter().any(|o| o == *m))
        .cloned()
        .collect();
    for m in &missing {
        let my = rank(managed, m).unwrap_or(usize::MAX);
        let at = out
            .iter()
            .position(|o| rank(managed, o).is_some_and(|r| r > my))
            .unwrap_or(out.len());
        out.insert(at, m.clone());
    }
    out
}

/// 按界面结果重写 members：`ordered` 是**已按界面行序排好、且只含已勾选**的管理成员。
///
/// 界面不认识的成员全部保留，落点按**锚点**还原：记下它原本前面有几个管理成员，
/// 重排后仍插在新序列的同一个序号处。这条规则让"用户没动过顺序"成为恒等变换
/// （`ordered` 与原管理子序列一致时输出 == 输入），也让手写的混排编排在拖动后不失真。
///
/// 反例是"外来成员一律追加末尾"：那会让用户每保存一次就把手写的编排推走一截。
fn apply_member_order(current: &[String], managed: &[String], ordered: &[String]) -> Vec<String> {
    // (锚点 = 它前面的管理成员个数, 成员)
    let mut foreign: Vec<(usize, String)> = Vec::new();
    let mut seen = 0usize;
    for m in current {
        if rank(managed, m).is_some() {
            seen += 1;
        } else {
            foreign.push((seen, m.clone()));
        }
    }

    let mut out: Vec<String> = Vec::with_capacity(ordered.len() + foreign.len());
    let mut fi = 0;
    for (i, m) in ordered.iter().enumerate() {
        while fi < foreign.len() && foreign[fi].0 <= i {
            out.push(foreign[fi].1.clone());
            fi += 1;
        }
        out.push(m.clone());
    }
    // 锚点落在新序列末尾之后的（原本排在所有管理成员之后，或管理成员被删到不够长）。
    for (_, m) in foreign.into_iter().skip(fi) {
        out.push(m);
    }
    out
}

/// 把 mix_modes 数组里指定 id 项的 members 按界面结果替换，其余字段/数组元素原样保留。
/// `ordered` 见 [`apply_member_order`]。找不到该 id 时返回 None（调用方不应静默新建实例）。
pub fn patch_mix_mode_members(
    mix_modes: &[Value],
    id: &str,
    managed: &[String],
    ordered: &[String],
) -> Option<Vec<Value>> {
    let current = mix_mode_members(mix_modes, id)?;
    let next = apply_member_order(&current, managed, ordered);
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

    /// 声明序照搬清单的候选来源选项（＝core `default_mix_modes()` 序，由
    /// `manifest::tests::mix_members_options_match_core_default_order` 对账）。
    /// 本模块测的是"按声明序补位"的算法，顺序本身可任取，但抄真值才不会误导读者。
    const MANAGED: [&str; 6] = [
        "quick_input.calc",
        "quick_input.number",
        "quick_input.date",
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
    fn order_follows_config_not_declaration() {
        // 用户手写把拼音排到了最前：界面必须照配置展示，否则他一打开就看到被改过的顺序。
        let current = strs(&["$primary_pinyin", "quick_input.calc", "english"]);
        let order = managed_order(&current, &managed());
        let in_config: Vec<String> = order
            .iter()
            .filter(|m| current.iter().any(|c| c == *m))
            .cloned()
            .collect();
        assert_eq!(
            in_config, current,
            "配置里已有的成员必须保持配置顺序（未启用项按声明序穿插其间）"
        );
        assert_eq!(
            order.len(),
            MANAGED.len(),
            "未启用的管理成员也要出现在列表里"
        );
    }

    #[test]
    fn order_fills_missing_members_at_declared_position() {
        let current = strs(&["quick_input.calc", "english"]);
        assert_eq!(
            managed_order(&current, &managed()),
            strs(&[
                "quick_input.calc",
                "quick_input.number",
                "quick_input.date",
                "quick_input.repeat",
                "$primary_pinyin",
                "english"
            ]),
            "缺席成员按声明序补到第一个序号更大的已有成员之前"
        );
    }

    #[test]
    fn order_of_empty_config_is_declaration_order() {
        assert_eq!(managed_order(&[], &managed()), managed());
    }

    #[test]
    fn unchecking_removes_only_that_member() {
        let out = apply_member_order(
            &managed(),
            &managed(),
            &strs(&[
                "quick_input.calc",
                "quick_input.number",
                "quick_input.date",
                "quick_input.repeat",
                "english",
            ]),
        );
        assert_eq!(
            out,
            strs(&[
                "quick_input.calc",
                "quick_input.number",
                "quick_input.date",
                "quick_input.repeat",
                "english"
            ]),
            "取消拼音只应删掉它，其余保持原位"
        );
    }

    #[test]
    fn drag_result_is_written_verbatim() {
        // 界面把英文拖到首位 → 配置就该是这个顺序（顺序即优先级）。
        let out = apply_member_order(
            &managed(),
            &managed(),
            &strs(&["english", "quick_input.calc", "$primary_pinyin"]),
        );
        assert_eq!(
            out,
            strs(&["english", "quick_input.calc", "$primary_pinyin"])
        );
    }

    #[test]
    fn unchanged_order_is_identity() {
        // 没动过顺序时保存必须是恒等变换，否则每存一次配置就漂一点。
        let current = strs(&["$primary_pinyin", "wubi86", "quick_input.calc", "english"]);
        let ordered = managed_order(&current, &managed());
        let selected: Vec<String> = ordered
            .iter()
            .filter(|m| current.iter().any(|c| c == *m))
            .cloned()
            .collect();
        assert_eq!(apply_member_order(&current, &managed(), &selected), current);
    }

    #[test]
    fn keeps_unmanaged_members_at_their_anchor() {
        // 用户往这个 mix 里加了别的方案，它夹在第 1、2 个管理成员之间——
        // 重排后仍应待在"第 1 个管理成员之后"，而不是被冲到末尾。
        let current = strs(&["quick_input.calc", "wubi86", "english"]);
        let out = apply_member_order(
            &current,
            &managed(),
            &strs(&["quick_input.calc", "english"]),
        );
        assert_eq!(out, strs(&["quick_input.calc", "wubi86", "english"]));

        // 界面把两项对调后，锚点仍是"第 1 个管理成员之后"。
        let swapped = apply_member_order(
            &current,
            &managed(),
            &strs(&["english", "quick_input.calc"]),
        );
        assert_eq!(swapped, strs(&["english", "wubi86", "quick_input.calc"]));
    }

    #[test]
    fn appends_when_no_later_member_exists() {
        let current = strs(&["quick_input.calc"]);
        let out = apply_member_order(
            &current,
            &managed(),
            &strs(&["quick_input.calc", "english"]),
        );
        assert_eq!(out, strs(&["quick_input.calc", "english"]));
    }

    #[test]
    fn inserts_into_empty_members() {
        let out = apply_member_order(&[], &managed(), &strs(&["english", "quick_input.calc"]));
        assert_eq!(
            out,
            strs(&["english", "quick_input.calc"]),
            "空 members 按界面序落地"
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
