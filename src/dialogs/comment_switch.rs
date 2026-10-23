//! 「注释切换与显示」对话框里的「切换方式」下拉：切换键选 Ctrl 系时「按住」不可选。
//!
//! 对话框本身是清单的分组对话框（`dialog_title` 宿主 `ui.comment.state`）；只有这一个下拉
//! 要看另一项的值，清单里给它单独的控件类型 `select_comment_switch_mode`。
//!
//! 切换键那一行下方另挂两条提示：选 Alt 时说明 Windows 上的全局钩子；Ctrl 那一侧已被中英切换、
//! 选词、会话动作或引导键占用时标出冲突（那些功能优先）。
//!
//! 按住只配 Alt：Ctrl 的按下三个平台都不送到输入法（core 把 hold + Ctrl 按 session 处理）。

use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::Value;
use wind_config::config::{BoundAction, KeysConfig, SessionAction};
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
const HOLD_AS_SESSION: &str = "按住只配 Alt，当前按「临时切换」生效";
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

    // 切换键的当前值从它自己那一行的取值器读（同一个框里、先建），取不到退回配置。
    let read = current_reader(state);
    let hold_ok = move || read(SWITCH_KEY).as_str().is_none_or(hold_allowed);
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

/// 切换键行下方的提示，没有为空串。注释关着或切换方式是「不使用」时什么都不提示；
/// 选 Alt 说明钩子；选 Ctrl 时说明「按住」按临时切换生效（存着 hold 的旧配置）与撞键。
pub fn switch_key_note(enabled: bool, switch_key: &str, mode: &str, uses: &CtrlUses) -> String {
    if !enabled || mode == "off" {
        return String::new();
    }
    if hold_allowed(switch_key) {
        return ALT_NOTE.to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    if mode == HOLD {
        parts.push(HOLD_AS_SESSION.to_string());
    }
    parts.extend(conflict_note(switch_key, uses));
    parts.join("；")
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
        switch_key_note(enabled, &s(SWITCH_KEY), &s(MODE_KEY), &uses)
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
        assert_eq!(
            switch_key_note(true, "lctrl", "off", &u),
            "",
            "不使用时不提示"
        );
        assert_eq!(
            switch_key_note(false, "lctrl", "session", &u),
            "",
            "注释关着不提示"
        );
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
    fn hold_with_ctrl_says_it_acts_as_session() {
        let factory = uses(&["lshift", "rshift"], &["semicolon_quote"], &[]);
        assert_eq!(
            switch_key_note(true, "ctrl", "hold", &factory),
            HOLD_AS_SESSION
        );
        let busy = uses(&["lctrl"], &[], &[]);
        assert_eq!(
            switch_key_note(true, "lctrl", "hold", &busy),
            format!("{HOLD_AS_SESSION}；左 Ctrl 已用作中英切换{TAIL}"),
            "与撞键提示共用一行"
        );
        assert_eq!(switch_key_note(true, "lalt", "hold", &busy), ALT_NOTE);
    }

    #[test]
    fn factory_keys_do_not_conflict_and_alt_shows_hook_note() {
        let factory = uses(&["lshift", "rshift"], &["semicolon_quote"], &[]);
        assert_eq!(switch_key_note(true, "ctrl", "session", &factory), "");
        let busy = uses(&["lctrl", "rctrl"], &["lrctrl"], &[("lctrl", "page_next")]);
        for k in ["alt", "lalt", "ralt"] {
            assert_eq!(
                switch_key_note(true, k, "session", &busy),
                ALT_NOTE,
                "Alt 不撞，只说明钩子"
            );
        }
        assert_eq!(switch_key_note(false, "alt", "session", &busy), "");
    }

    /// 读不到的表按 core 出厂值：出厂不撞。
    #[test]
    fn missing_tables_fall_back_to_factory() {
        let u = uses_from_reader(&|_| Value::Null);
        assert_eq!(conflict_note("ctrl", &u), None);
        assert!(u.session.contains_key("semicolon"), "出厂选词键组照样展开");
    }
}
