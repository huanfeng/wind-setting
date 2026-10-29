//! 「注释切换与显示」对话框里切换键那一行的提示。
//!
//! 对话框本身是清单的分组对话框（`dialog_title` 宿主 `ui.comment.state`）。
//!
//! 切换键那一行下方另挂提示：选 Alt 时说明 Windows 上的全局钩子；
//! 切换键（功能键或 Ctrl 那一侧）已被中英切换、
//! 选词、会话动作或引导键占用时标出冲突（那些功能优先）。

use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::Value;
use wind_config::config::{BoundAction, KeysConfig, SessionAction};
use windui::prelude::*;

use crate::state::AppState;

/// 「注释切换与显示」分组对话框的宿主（`dialog_title`）。
pub const HOST_KEY: &str = "ui.comment.state";
pub const SWITCH_KEY: &str = "ui.comment.switch_key";
/// 对话框尺寸：宽到每行说明一行读完，高到放下全部行与一条提示不出滚动条。
pub const DIALOG_W: i32 = 620;
pub const DIALOG_H: i32 = 420;
const ALT_NOTE: &str = "Windows 上选 Alt 会安装全局键盘钩子，只在候选窗出现时接管 Alt";

/// 切换键是不是 Alt 系。
fn is_alt(switch_key: &str) -> bool {
    matches!(switch_key, "alt" | "lalt" | "ralt")
}

/// 某项的当前值：页面上的取值器优先（改了即时反映），没建那一行时退回配置。
fn current_reader(state: &AppState) -> Rc<dyn Fn(&str) -> Value> {
    let reg = state.fields.clone();
    let cfg = state.current_config.clone();
    Rc::new(move |k: &str| {
        reg.getters
            .borrow()
            .get(k)
            .map(|g| g())
            .or_else(|| cfg.get_path(k).cloned())
            .unwrap_or(Value::Null)
    })
}

/// 切换键是不是功能键（`f1`..`f12`）。
fn is_function_key(switch_key: &str) -> bool {
    switch_key
        .strip_prefix('f')
        .and_then(|n| n.parse::<u32>().ok())
        .is_some_and(|n| (1..=12).contains(&n))
}

/// 切换键在按键表里的键名：`(键名, 显示名)`。Alt 系不收进这几张表，不会撞。
fn ctrl_sides(switch_key: &str) -> Vec<(String, String)> {
    let side = |k: &str, n: &str| (k.to_string(), n.to_string());
    match switch_key {
        "ctrl" => vec![side("lctrl", "左 Ctrl"), side("rctrl", "右 Ctrl")],
        "lctrl" => vec![side("lctrl", "左 Ctrl")],
        "rctrl" => vec![side("rctrl", "右 Ctrl")],
        k if is_function_key(k) => vec![side(k, &k.to_uppercase())],
        _ => Vec::new(),
    }
}

/// 键名是不是这一侧的 Ctrl。会话表的键先剥掉 `shift+`（同 core `session_key_to_vk`）。
fn is_side(key: &str, side: &str) -> bool {
    let k = key.trim().to_ascii_lowercase();
    let k = k.strip_prefix("shift+").map(str::trim).unwrap_or(&k);
    k == side || k == side.replace("ctrl", "control")
}

/// 别的功能对 Ctrl 单击的占用。**只查全局表**：方案级的会话动作 / 引导键不查（设置页这里
/// 不知道用户在用哪个方案）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CtrlUses {
    /// `keys.toggle_mode_keys`。
    pub toggle_mode_keys: Vec<String>,
    /// 有效的会话态按键表：键组展开 ⊕ `keys.session_actions`（同 core
    /// `KeysConfig::effective_session_actions`）。
    pub session: BTreeMap<String, String>,
    /// `keys.key_actions`。
    pub key_actions: BTreeMap<String, String>,
}

impl CtrlUses {
    pub fn from_keys(keys: &KeysConfig) -> Self {
        Self {
            toggle_mode_keys: keys.toggle_mode_keys.clone(),
            session: keys.effective_session_actions(),
            key_actions: keys.key_actions.clone(),
        }
    }
}

/// 冲突提示：切换键的 Ctrl 侧被别的功能占用时给一句话，否则 None。动作是 `none`（或不认识）
/// 的绑定不算占用，与 core 编译热键表时跳过它们一致。
pub fn conflict_note(switch_key: &str, uses: &CtrlUses) -> Option<String> {
    let owners_of = |side: &str| {
        let mut o: Vec<&str> = Vec::new();
        let mut push = |x| {
            if !o.contains(&x) {
                o.push(x);
            }
        };
        if uses.toggle_mode_keys.iter().any(|k| is_side(k, side)) {
            push("中英切换");
        }
        for (k, v) in &uses.session {
            let a = SessionAction::parse(v);
            if a.is_enabled() && is_side(k, side) {
                push(if a.candidate_ordinal().is_some() {
                    "选词"
                } else {
                    "会话动作"
                });
            }
        }
        if uses
            .key_actions
            .iter()
            .any(|(k, v)| BoundAction::parse(v).is_enabled() && is_side(k, side))
        {
            push("引导键");
        }
        o
    };
    let sides = ctrl_sides(switch_key);
    let found: Vec<(&str, Vec<&str>)> = sides
        .iter()
        .map(|(side, name)| (name.as_str(), owners_of(side)))
        .filter(|(_, o)| !o.is_empty())
        .collect();
    let parts: Vec<String> = match found.as_slice() {
        [] => return None,
        [(_, a), (_, b)] if a == b => vec![format!("左右 Ctrl 已用作{}", a.join("、"))],
        _ => found
            .iter()
            .map(|(n, o)| format!("{n} 已用作{}", o.join("、")))
            .collect(),
    };
    Some(format!(
        "{}，那些功能优先，切换键此时不生效",
        parts.join("；")
    ))
}

/// 切换键行下方的提示，没有为空串。注释关着或切换方式是「不使用」时什么都不提示；
/// 选 Alt 说明钩子；选 Ctrl 时提示撞键。
pub fn switch_key_note(enabled: bool, switch_key: &str, uses: &CtrlUses) -> String {
    if !enabled || switch_key == "none" {
        return String::new();
    }
    if is_alt(switch_key) {
        return ALT_NOTE.to_string();
    }
    conflict_note(switch_key, uses).unwrap_or_default()
}

/// 拼 [`CtrlUses`] 要的几张全局表（取不到的项不放，按 core 出厂值）。
const KEYS_READ: [&str; 7] = [
    "toggle_mode_keys",
    "session_actions",
    "key_actions",
    "select_key_groups",
    "page_keys",
    "highlight_keys",
    "select_char_keys",
];

fn uses_from_reader(read: &dyn Fn(&str) -> Value) -> CtrlUses {
    let mut obj = serde_json::Map::new();
    for k in KEYS_READ {
        let v = read(&format!("keys.{k}"));
        if !v.is_null() {
            obj.insert(k.to_string(), v);
        }
    }
    serde_json::from_value::<KeysConfig>(Value::Object(obj))
        .map(|k| CtrlUses::from_keys(&k))
        .unwrap_or_default()
}

/// 切换键那一行（`row`）连同下方提示，值变了即时刷新。
pub fn with_switch_key_note(state: &AppState, row: Element) -> Element {
    let read = current_reader(state);
    let compute = move || {
        let s = |k: &str| read(k).as_str().unwrap_or("").to_string();
        let enabled = read("ui.comment.enabled").as_bool().unwrap_or(true);
        let uses = uses_from_reader(&*read);
        switch_key_note(enabled, &s(SWITCH_KEY), &uses)
    };
    let compute = Rc::new(compute);
    let text = signal(compute());
    let watch = {
        let (c1, c2) = (compute.clone(), compute.clone());
        crate::pages::dict::reactive::reactive_effect(
            move || {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                c1().hash(&mut h);
                h.finish()
            },
            move || text.set(c2()),
        )
    };
    let note = Element::label_signal(text)
        .font_size(12.0)
        .fg_role(Role::Warning)
        .width_match()
        .max_lines(2)
        .padding_xy(0, 4)
        .visible_when(move || !text.get().is_empty());
    Element::col()
        .width_match()
        .child(row)
        .child(note)
        .child(watch)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 清单两项就位，切换方式是普通下拉。
    #[test]
    fn switch_rows_are_in_manifest() {
        let m = crate::manifest::Manifest::from_embedded();
        let item = m.get("ui.comment.switch_mode").expect("清单有切换方式");
        assert_eq!(item.typ, "select");
        assert!(item.options.iter().any(|o| o.value == "hold"));
        assert!(m.get(SWITCH_KEY).is_some());
    }

    /// 搜索落点：切换几项开「注释切换与显示」框，注释词库是卡片上的一行。
    #[test]
    fn search_targets_follow_the_new_rows() {
        use crate::search::index::{JumpTarget, build_index};
        let idx = build_index(&crate::manifest::Manifest::from_embedded());
        let target = |id: &str| {
            idx.iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("索引里没有 {id}"))
                .target
                .clone()
        };
        for k in [
            SWITCH_KEY,
            "ui.comment.switch_mode",
            "ui.candidate.comment_above",
        ] {
            match target(k) {
                JumpTarget::RowInDialog { dialog, .. } => assert_eq!(dialog, "ui.comment.state"),
                other => panic!("{k}: {other:?}"),
            }
        }
        assert!(matches!(target("ui.comment_dicts"), JumpTarget::Row { .. }));
    }

    fn uses(toggle: &[&str], groups: &[&str], session: &[(&str, &str)]) -> CtrlUses {
        let keys: KeysConfig = serde_json::from_value(serde_json::json!({
            "toggle_mode_keys": toggle,
            "select_key_groups": groups,
            "session_actions": session.iter().cloned().collect::<BTreeMap<_, _>>(),
        }))
        .unwrap();
        CtrlUses::from_keys(&keys)
    }

    const TAIL: &str = "，那些功能优先，切换键此时不生效";

    #[test]
    fn left_ctrl_toggle_conflicts_only_on_its_side() {
        let u = uses(&["lshift", "lctrl"], &[], &[]);
        assert_eq!(
            conflict_note("ctrl", &u),
            Some(format!("左 Ctrl 已用作中英切换{TAIL}"))
        );
        assert!(conflict_note("rctrl", &u).is_none(), "右侧不撞");
        assert_eq!(switch_key_note(true, "none", &u), "", "切换键为无时不提示");
        assert_eq!(switch_key_note(false, "lctrl", &u), "", "注释关着不提示");
    }

    #[test]
    fn lrctrl_select_group_hits_both_sides() {
        let u = uses(
            &[],
            &["semicolon_quote", "lrctrl"],
            &[("rctrl", "page_next")],
        );
        assert_eq!(
            conflict_note("ctrl", &u),
            Some(format!("左 Ctrl 已用作选词；右 Ctrl 已用作会话动作{TAIL}")),
            "显式会话表覆盖键组展开"
        );
        let u = uses(&[], &["lrctrl"], &[]);
        assert_eq!(
            conflict_note("ctrl", &u),
            Some(format!("左右 Ctrl 已用作选词{TAIL}"))
        );
    }

    #[test]
    fn disabled_session_binding_does_not_conflict_and_shift_prefix_does() {
        let u = uses(&[], &["lrctrl"], &[("lctrl", "none")]);
        assert_eq!(
            conflict_note("lctrl", &u),
            None,
            "lctrl = none 压掉了键组展开"
        );
        let u = uses(&[], &[], &[("shift+lctrl", "page_next")]);
        assert_eq!(
            conflict_note("lctrl", &u),
            Some(format!("左 Ctrl 已用作会话动作{TAIL}"))
        );
    }

    #[test]
    fn hold_with_ctrl_has_no_extra_note() {
        let factory = uses(&["lshift", "rshift"], &["semicolon_quote"], &[]);
        assert_eq!(switch_key_note(true, "ctrl", &factory), "");
        let busy = uses(&["lctrl"], &[], &[]);
        assert_eq!(
            switch_key_note(true, "lctrl", &busy),
            format!("左 Ctrl 已用作中英切换{TAIL}")
        );
        assert_eq!(switch_key_note(true, "lalt", &busy), ALT_NOTE);
    }

    #[test]
    fn function_key_conflicts_with_session_binding() {
        let factory = uses(&["lshift", "rshift"], &["semicolon_quote"], &[]);
        assert_eq!(switch_key_note(true, "f1", &factory), "");
        let busy = uses(&[], &[], &[("f1", "page_next")]);
        assert_eq!(
            conflict_note("f1", &busy),
            Some(format!("F1 已用作会话动作{TAIL}"))
        );
        let busy = uses(&[], &[], &[("f5", "page_next")]);
        assert_eq!(
            conflict_note("f5", &busy),
            Some(format!("F5 已用作会话动作{TAIL}"))
        );
        assert!(conflict_note("f1", &busy).is_none());
    }

    #[test]
    fn factory_keys_do_not_conflict_and_alt_shows_hook_note() {
        let factory = uses(&["lshift", "rshift"], &["semicolon_quote"], &[]);
        assert_eq!(switch_key_note(true, "ctrl", &factory), "");
        let busy = uses(&["lctrl", "rctrl"], &["lrctrl"], &[("lctrl", "page_next")]);
        for k in ["alt", "lalt", "ralt"] {
            assert_eq!(
                switch_key_note(true, k, &busy),
                ALT_NOTE,
                "Alt 不撞，只说明钩子"
            );
        }
        assert_eq!(switch_key_note(false, "alt", &busy), "");
    }

    /// 读不到的表按 core 出厂值：出厂不撞。
    #[test]
    fn missing_tables_fall_back_to_factory() {
        let u = uses_from_reader(&|_| Value::Null);
        assert_eq!(conflict_note("ctrl", &u), None);
        assert!(u.session.contains_key("semicolon"), "出厂选词键组照样展开");
    }
}
