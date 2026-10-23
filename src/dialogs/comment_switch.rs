//! 「注释切换与显示」对话框里的「切换方式」下拉：切换键选 Ctrl 系时「按住」不可选。
//!
//! 对话框本身是清单的分组对话框（`dialog_title` 宿主 `ui.comment.state`）；只有这一个下拉
//! 要看另一项的值，清单里给它单独的控件类型 `select_comment_switch_mode`。
//!
//! 切换键那一行下方另挂两条提示：选 Alt 时说明 Windows 上的全局钩子；Ctrl 那一侧已被中英切换、
//! 选词、会话动作或引导键占用时标出冲突（那些功能优先）。
//!
//! 按住只配 Alt：Ctrl 的按下三个平台都不送到输入法（core 把 hold + Ctrl 按 session 处理）。

use std::rc::Rc;

use serde_json::Value;
use windui::prelude::*;

use crate::config::Config;
use crate::manifest::MItem;
use crate::state::AppState;

/// 「注释切换与显示」分组对话框的宿主（`dialog_title`）。
pub const HOST_KEY: &str = "ui.comment.state";
pub const SWITCH_KEY: &str = "ui.comment.switch_key";
const MODE_KEY: &str = "ui.comment.switch_mode";
/// 对话框尺寸：宽到每行说明一行读完，高到放下全部行与一条提示不出滚动条。
pub const DIALOG_W: i32 = 620;
pub const DIALOG_H: i32 = 420;
const ALT_NOTE: &str = "Windows 上选 Alt 会安装全局键盘钩子，只在候选窗出现时接管 Alt";
const HOLD: &str = "hold";
const HOLD_NOTE: &str = "Ctrl 的按下传不到输入法，选 Alt 系切换键才可用";

/// 这个切换键能不能配「按住」。
pub fn hold_allowed(switch_key: &str) -> bool {
    matches!(switch_key, "alt" | "lalt" | "ralt")
}

/// 用户在下拉里改选后实际留下的序号：切换键不配「按住」时选它退回原值。
pub fn accept_mode(prev: usize, picked: usize, hold: Option<usize>, hold_ok: bool) -> usize {
    if !hold_ok && Some(picked) == hold {
        prev
    } else {
        picked
    }
}

/// 下拉项：切换键不配「按住」时那一项带「仅 Alt」徽章与说明。
pub fn mode_items(labels: &[String], hold: Option<usize>, hold_ok: bool) -> Vec<DropdownItem> {
    labels
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let it = DropdownItem::new(l.clone());
            if !hold_ok && Some(i) == hold {
                it.subtitle(HOLD_NOTE).badge("仅 Alt", Intent::Warning)
            } else {
                it
            }
        })
        .collect()
}

pub fn build_switch_mode_select(
    state: &AppState,
    key: &str,
    item: &MItem,
    cfg: &Config,
) -> Element {
    let (values, labels): (Vec<String>, Vec<String>) = item
        .options
        .iter()
        .map(|o| (o.value.clone(), o.label.clone()))
        .unzip();
    let values = Rc::new(values);
    let def = state
        .capabilities
        .default_for(key)
        .as_str()
        .unwrap_or("")
        .to_string();
    let index_of = {
        let values = values.clone();
        move |v: Option<&str>| -> usize {
            let v = v.unwrap_or(&def);
            values
                .iter()
                .position(|x| x == v)
                .or_else(|| values.iter().position(|x| *x == def))
                .unwrap_or(0)
        }
    };
    let idx = index_of(cfg.get_str(key));
    // `value` 是写回的值，`shown` 是下拉绑的选中；两者只在用户选了不可选的「按住」时分开一瞬。
    let value = signal(idx);
    let shown = signal(idx);
    let hold = values.iter().position(|v| v == HOLD);

    let kw = key.to_string();
    let vw = values.clone();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            if let Some(v) = vw.get(value.get()) {
                c.set_path(&kw, Value::String(v.clone()));
            }
        }));
    let kl = key.to_string();
    state.fields.loaders.borrow_mut().insert(
        key.to_string(),
        Box::new(move |c: &Config| {
            let i = index_of(c.get_str(&kl));
            value.set(i);
            shown.set(i);
        }),
    );
    let vg = values.clone();
    state.fields.getters.borrow_mut().insert(
        key.to_string(),
        Box::new(move || {
            vg.get(value.get())
                .map(|v| Value::String(v.clone()))
                .unwrap_or(Value::Null)
        }),
    );

    // 切换键的当前值从它自己那一行的取值器读（同一个框里、先建）；读不到按可配处理。
    let reg = state.fields.clone();
    let hold_ok = move || {
        reg.getters
            .borrow()
            .get(SWITCH_KEY)
            .map(|g| g())
            .and_then(|v| v.as_str().map(hold_allowed))
            .unwrap_or(true)
    };
    let hold_ok = Rc::new(hold_ok);
    let items = signal(mode_items(&labels, hold, hold_ok()));
    let relabel = {
        let ok = hold_ok.clone();
        let ok2 = hold_ok.clone();
        crate::pages::dict::reactive::reactive_effect(
            move || ok() as u64,
            move || items.set(mode_items(&labels, hold, ok2())),
        )
    };
    let guard = crate::pages::dict::reactive::reactive_effect(
        move || shown.version(),
        move || {
            let (prev, picked) = (value.get(), shown.get());
            if prev == picked {
                return;
            }
            let kept = accept_mode(prev, picked, hold, hold_ok());
            if kept != picked {
                shown.set(kept);
            }
            if kept != prev {
                value.set(kept);
            }
        },
    );
    let dd = Element::dropdown_items_signal(items, shown);
    let dd = match item.width {
        Some(w) => dd.width(w as i32),
        None => dd.min_width(crate::manifest::CTRL_W),
    };
    Element::row()
        .cross(Align::Center)
        .child(relabel)
        .child(guard)
        .child(dd)
}

/// 切换键覆盖的 Ctrl 侧：`(键名, 显示名)`。Alt 系不收进这几张表，不会撞。
fn ctrl_sides(switch_key: &str) -> &'static [(&'static str, &'static str)] {
    const L: (&str, &str) = ("lctrl", "左 Ctrl");
    const R: (&str, &str) = ("rctrl", "右 Ctrl");
    match switch_key {
        "ctrl" => &[L, R],
        "lctrl" => &[L],
        "rctrl" => &[R],
        _ => &[],
    }
}

fn is_side(key: &str, side: &str) -> bool {
    let k = key.trim().to_ascii_lowercase();
    k == side || k == side.replace("ctrl", "control")
}

/// 别的功能对 Ctrl 单击的占用（取自配置）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CtrlUses {
    /// `keys.toggle_mode_keys`。
    pub toggle_mode_keys: Vec<String>,
    /// `keys.select_key_groups`（`lrctrl` 两侧都占）。
    pub select_key_groups: Vec<String>,
    /// `keys.session_actions` 的键。
    pub session_keys: Vec<String>,
    /// `keys.key_actions` 的键。
    pub key_action_keys: Vec<String>,
}

/// 冲突提示：切换键的 Ctrl 侧被别的功能占用时给一句话，否则 None。切换方式是「不使用」时不提示。
pub fn conflict_note(switch_key: &str, mode: &str, uses: &CtrlUses) -> Option<String> {
    if mode == "off" {
        return None;
    }
    let owners_of = |side: &str| {
        let mut o: Vec<&str> = Vec::new();
        if uses.toggle_mode_keys.iter().any(|k| is_side(k, side)) {
            o.push("中英切换");
        }
        if uses.select_key_groups.iter().any(|g| g == "lrctrl") {
            o.push("选词");
        }
        if uses.session_keys.iter().any(|k| is_side(k, side)) {
            o.push("会话动作");
        }
        if uses.key_action_keys.iter().any(|k| is_side(k, side)) {
            o.push("引导键");
        }
        o
    };
    let found: Vec<(&str, Vec<&str>)> = ctrl_sides(switch_key)
        .iter()
        .map(|(side, name)| (*name, owners_of(side)))
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

/// 切换键行下方的提示：选 Alt 时说明钩子，Ctrl 侧撞键时说明冲突；都没有为空串。
pub fn switch_key_note(switch_key: &str, mode: &str, uses: &CtrlUses) -> String {
    if hold_allowed(switch_key) {
        return ALT_NOTE.to_string();
    }
    conflict_note(switch_key, mode, uses).unwrap_or_default()
}

fn str_list(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect()
}

fn map_keys(v: &Value) -> Vec<String> {
    v.as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// 切换键那一行（`row`）连同下方提示。各键的当前值优先取页面上的取值器（改了即时刷新），
/// 没建那一页时退回配置。
pub fn with_switch_key_note(state: &AppState, row: Element) -> Element {
    let reg = state.fields.clone();
    let cfg = state.current_config.clone();
    let read = move |k: &str| -> Value {
        reg.getters
            .borrow()
            .get(k)
            .map(|g| g())
            .or_else(|| cfg.get_path(k).cloned())
            .unwrap_or(Value::Null)
    };
    let compute = move || {
        let s = |k: &str| read(k).as_str().unwrap_or("").to_string();
        let uses = CtrlUses {
            toggle_mode_keys: str_list(&read("keys.toggle_mode_keys")),
            select_key_groups: str_list(&read("keys.select_key_groups")),
            session_keys: map_keys(&read("keys.session_actions")),
            key_action_keys: map_keys(&read("keys.key_actions")),
        };
        switch_key_note(&s(SWITCH_KEY), &s(MODE_KEY), &uses)
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

    #[test]
    fn hold_only_pairs_with_alt() {
        for k in ["alt", "lalt", "ralt"] {
            assert!(hold_allowed(k), "{k}");
        }
        for k in ["ctrl", "lctrl", "rctrl", ""] {
            assert!(!hold_allowed(k), "{k}");
        }
    }

    #[test]
    fn picking_hold_with_ctrl_keeps_previous_mode() {
        let hold = Some(3);
        assert_eq!(accept_mode(1, 3, hold, false), 1);
        assert_eq!(accept_mode(1, 3, hold, true), 3);
        assert_eq!(accept_mode(1, 2, hold, false), 2, "别的档照常可选");
    }

    #[test]
    fn hold_item_is_marked_only_when_not_allowed() {
        let labels: Vec<String> = ["不使用", "临时", "切换", "按住"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let marked = mode_items(&labels, Some(3), false);
        assert!(marked[3].badge.is_some() && marked[3].subtitle.is_some());
        assert!(marked[..3].iter().all(|i| i.badge.is_none()));
        assert!(
            mode_items(&labels, Some(3), true)
                .iter()
                .all(|i| i.badge.is_none())
        );
    }

    /// 清单两项就位；按出厂值建起来，写回的就是出厂值（ctrl / session）。
    #[test]
    fn switch_rows_build_with_factory_values() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let data = crate::state::LoadedState::fetch();
        let mut st = AppState::empty();
        st.apply_loaded(data);
        let m = st.manifest.clone();
        let item = m
            .get("ui.comment.switch_mode")
            .expect("清单有切换方式")
            .clone();
        assert_eq!(item.typ, "select_comment_switch_mode");
        assert!(m.get(SWITCH_KEY).is_some());
        let cfg = st.current_config.clone();
        crate::manifest::field(&st, SWITCH_KEY);
        build_switch_mode_select(&st, "ui.comment.switch_mode", &item, &cfg);
        let get = |k: &str| st.fields.getters.borrow()[k]();
        assert_eq!(get(SWITCH_KEY), Value::String("ctrl".into()));
        assert_eq!(
            get("ui.comment.switch_mode"),
            Value::String("session".into())
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
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

    fn uses(toggle: &[&str], groups: &[&str], session: &[&str]) -> CtrlUses {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect();
        CtrlUses {
            toggle_mode_keys: v(toggle),
            select_key_groups: v(groups),
            session_keys: v(session),
            key_action_keys: Vec::new(),
        }
    }

    #[test]
    fn left_ctrl_toggle_conflicts_only_on_its_side() {
        let u = uses(&["lshift", "lctrl"], &[], &[]);
        assert_eq!(
            conflict_note("ctrl", "session", &u).as_deref(),
            Some("左 Ctrl 已用作中英切换，那些功能优先，切换键此时不生效")
        );
        assert!(conflict_note("rctrl", "session", &u).is_none(), "右侧不撞");
        assert!(
            conflict_note("lctrl", "off", &u).is_none(),
            "不使用时不提示"
        );
    }

    #[test]
    fn lrctrl_select_group_hits_both_sides() {
        let u = uses(&[], &["semicolon_quote", "lrctrl"], &["rcontrol"]);
        assert_eq!(
            conflict_note("ctrl", "persist", &u).as_deref(),
            Some(
                "左 Ctrl 已用作选词；右 Ctrl 已用作选词、会话动作，那些功能优先，切换键此时不生效"
            )
        );
        let u = uses(&[], &["lrctrl"], &[]);
        assert_eq!(
            conflict_note("ctrl", "session", &u).as_deref(),
            Some("左右 Ctrl 已用作选词，那些功能优先，切换键此时不生效")
        );
    }

    #[test]
    fn factory_keys_do_not_conflict_and_alt_shows_hook_note() {
        let factory = uses(&["lshift", "rshift"], &["semicolon_quote"], &[]);
        assert_eq!(switch_key_note("ctrl", "session", &factory), "");
        let busy = uses(&["lctrl", "rctrl"], &["lrctrl"], &["lctrl"]);
        for k in ["alt", "lalt", "ralt"] {
            assert_eq!(
                switch_key_note(k, "session", &busy),
                ALT_NOTE,
                "Alt 不撞，只说明钩子"
            );
        }
    }
}
