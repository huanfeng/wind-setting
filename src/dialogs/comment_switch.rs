//! 「注释切换与显示」对话框里的「切换方式」下拉：切换键选 Ctrl 系时「按住」不可选。
//!
//! 对话框本身是清单的分组对话框（`dialog_title` 宿主 `ui.comment.state`）；只有这一个下拉
//! 要看另一项的值，清单里给它单独的控件类型 `select_comment_switch_mode`。
//!
//! 按住只配 Alt：Ctrl 的按下三个平台都不送到输入法（core 把 hold + Ctrl 按 session 处理）。

use std::rc::Rc;

use serde_json::Value;
use windui::prelude::*;

use crate::config::Config;
use crate::manifest::MItem;
use crate::state::AppState;

pub const SWITCH_KEY: &str = "ui.comment.switch_key";
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
}
