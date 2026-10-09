//! 设置向导：按输入习惯（五笔/全拼/双拼）4 步快速配置。设计见 docs/design/setup-wizard.md。

use std::cell::RefCell;
use std::rc::Rc;

use windui::prelude::ToastKind;

use crate::state::{AppState, LoadedState, WizardPhase, WizardSession, new_trigger_conflicts};

pub mod apply;
pub mod model;
pub mod view;

/// 打开向导：按当前基线预填。设置页有未保存修改时先停在 Dirty 段。
pub fn open_wizard(app: &Rc<RefCell<AppState>>) {
    let st = app.borrow();
    let layouts = crate::dialogs::schema_manager::fetch_shuangpin_layouts();
    let layout = crate::rpc::call_core(
        "schema.getConfig",
        &serde_json::json!({ "id": "shuangpin" }),
    )
    .ok()
    .and_then(|v| {
        v.pointer("/engine/pinyin/shuangpin/layout")
            .and_then(|x| x.as_str())
            .map(str::to_string)
    })
    .unwrap_or_else(|| "xiaohe".into());
    let cur = model::Draft::from_current(&st.base_config, &st.schemas, &layout);
    *st.wizard.borrow_mut() = Some(WizardSession {
        draft: cur.clone(),
        cur,
        layouts,
        outcome: None,
    });
    st.wizard_phase.set(if st.is_dirty() {
        WizardPhase::Dirty
    } else {
        WizardPhase::Step(1)
    });
    st.wizard_rev.set(st.wizard_rev.get().wrapping_add(1));
    st.wizard_visible.set(true);
}

/// 启动数据就绪、或重连成功后调用。向导已开着时不动它（重开会把草稿打回预填）。
pub fn maybe_auto_open(app: &Rc<RefCell<AppState>>, connected: bool) {
    let (dismissed, open) = {
        let st = app.borrow();
        (
            st.setting_config.ui.wizard_dismissed,
            st.wizard.borrow().is_some(),
        )
    };
    if !open && should_auto_open(dismissed, connected) {
        open_wizard(app);
    }
}

/// 关闭：不碰 core，只按勾选落 `wizard_dismissed`。
pub fn close(app: &Rc<RefCell<AppState>>) {
    let dont = app.borrow().wizard_dont_show.get();
    end(app, dont);
}

/// 收尾：落标志、藏对话框、清会话。会话先清再藏——ESC 关窗的兜底监视以「可见为假而会话还在」
/// 判定为一次关闭，顺序反过来会让它再关一遍。
fn end(app: &Rc<RefCell<AppState>>, dismissed: bool) {
    let mut st = app.borrow_mut();
    st.save_wizard_dismissed(dismissed);
    st.wizard_dont_show.set(dismissed);
    *st.wizard.borrow_mut() = None;
    st.wizard_visible.set(false);
}

/// 第 4 步点「完成」：摘要为空直接收尾（视同完成，一律不再弹出），否则进二次确认。
pub fn complete(app: &Rc<RefCell<AppState>>) {
    let empty = {
        let st = app.borrow();
        let s = st.wizard.borrow();
        s.as_ref().is_none_or(|s| summary_of(&st, s).is_empty())
    };
    if empty {
        end(app, true);
    } else {
        app.borrow().wizard_phase.set(WizardPhase::Confirm);
    }
}

/// 完成：写入 → 整体重灌（Review Focus #1：只 reload() 不刷新 schema_mgr）→ 落标志。
pub fn finish(app: &Rc<RefCell<AppState>>) {
    finish_with(app, &mut apply::RealCore, LoadedState::fetch);
}

/// [`finish`] 的本体。core 调用与重灌数据可注入：mock 不落盘，测试要自己造「写入后的配置」。
pub(crate) fn finish_with(
    app: &Rc<RefCell<AppState>>,
    core: &mut dyn apply::CoreCalls,
    fetch: impl FnOnce() -> LoadedState,
) {
    let plan = {
        let st = app.borrow();
        let s = st.wizard.borrow();
        let Some(s) = s.as_ref() else { return };
        apply::Plan {
            items: s.draft.items(&st.base_config, &st.capabilities.map_keys()),
            layout: model::layout_changed(&s.cur, &s.draft).then(|| s.draft.layout.clone()),
            active: (s.draft.active != s.cur.active).then(|| s.draft.active.clone()),
        }
    };
    let outcome = apply::apply_writes(core, &plan);
    // 失败也重灌：已成功的步骤已经落盘，设置页要反映真实状态
    app.borrow_mut().apply_loaded(fetch());
    if outcome == apply::ApplyOutcome::Done {
        // 重灌只刷新 schema_mgr 的启用表与键表，不碰 active_id；它要等 core 异步广播的
        // schema.activeChanged。事件没到（或断开）时设置页的 schema.active writer 仍写旧方案，
        // 判脏、一点「保存」就把默认方案改回去。同 SchemaManagerState::set_active：成功即本地落定。
        if let Some(id) = &plan.active {
            app.borrow().schema_mgr.set_active_from_core(id);
        }
        end(app, true);
        app.borrow()
            .feedback
            .set(Some((ToastKind::Success, "设置向导：已应用".into())));
    } else {
        let st = app.borrow();
        if let Some(s) = st.wizard.borrow_mut().as_mut() {
            s.outcome = Some(outcome);
        }
        st.wizard_phase.set(WizardPhase::Result);
        st.wizard_rev.set(st.wizard_rev.get().wrapping_add(1));
    }
}

/// 第 4 步摘要。方案名取 `schema.list`，布局名取 `schema.layouts`，查不到就显示 id。
pub fn summary_of(st: &AppState, s: &WizardSession) -> Vec<model::Change> {
    let names = |id: &str| {
        st.schemas
            .iter()
            .find(|x| x.id == id)
            .map(|x| x.name.clone())
            .unwrap_or_else(|| id.to_string())
    };
    let layout = |id: &str| layout_label(&s.layouts, id);
    model::summary(&s.cur, &s.draft, &names, &layout)
}

pub fn layout_label(layouts: &[(String, String)], id: &str) -> String {
    layouts
        .iter()
        .find(|(i, _)| i == id)
        .map(|(_, n)| n.clone())
        .unwrap_or_else(|| id.to_string())
}

/// 第 3 步挡「下一步」的冲突文案：向导自己的键位判定 + 设置页触发键域判定（只算新增的）。
pub fn step3_blockers(st: &AppState, d: &model::Draft) -> Vec<String> {
    let mut out: Vec<String> = model::key_conflicts(d, &st.base_config)
        .into_iter()
        .map(|c| c.msg)
        .collect();
    if let Some(m) = new_trigger_conflicts(&st.base_config, &d.to_target(&st.base_config)) {
        out.push(m);
    }
    out
}

/// 启动后是否自动弹出向导。core 连不上时不弹：向导预填需要真实配置，拿空配置预填会误导。
pub fn should_auto_open(dismissed: bool, connected: bool) -> bool {
    !dismissed && connected
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::state::{AppState, LoadedState, WizardPhase};

    /// 造一个按 mock 数据加载好的 AppState。调用方须已持 `ENV_LOCK` 并设好 `WIND_RPC_MOCK`。
    pub(super) fn mock_app() -> Rc<RefCell<AppState>> {
        let mut st = AppState::empty();
        st.apply_loaded(LoadedState::fetch());
        Rc::new(RefCell::new(st))
    }

    pub(super) fn with_mock<R>(f: impl FnOnce() -> R) -> R {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let r = f();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        r
    }

    /// 写入全部成功的假 core：不落盘，落盘效果由测试自己造的重灌数据代替。
    pub(super) struct OkCore;
    impl apply::CoreCalls for OkCore {
        fn call(&mut self, m: &str, _p: &serde_json::Value) -> Result<serde_json::Value, String> {
            Ok(match m {
                "schema.setActive" => serde_json::json!({"ok": true}),
                _ => serde_json::json!({}),
            })
        }
    }

    /// Review Focus #1：向导写入后，设置页 writer 的状态（schema_mgr.enabled_order / key_actions /
    /// active_id、punct cells）必须跟上，否则用户接着点「保存」会把向导结果改回去。
    ///
    /// 重灌喂的是 `draft.to_target(base)`——模拟 core 真实落盘后的配置。mock 的 `config.get`
    /// 不随写入变化，直接拿它重灌等于什么都没写，测不出 writer 是否跟上。
    #[test]
    fn after_finish_settings_save_does_not_revert_wizard() {
        with_mock(|| {
            let app = mock_app();
            open_wizard(&app);
            let target = {
                let st = app.borrow();
                let mut s = st.wizard.borrow_mut();
                let s = s.as_mut().unwrap();
                s.draft = s.draft.with_preset(model::Kind::Pinyin, &st.schemas);
                assert_ne!(s.draft.active, s.cur.active, "前提：默认方案要变");
                s.draft.to_target(&st.base_config)
            };
            finish_with(&app, &mut OkCore, || {
                let mut l = LoadedState::fetch();
                l.config_value = target.0.clone();
                l
            });
            let st = app.borrow();
            assert_eq!(*st.schema_mgr.active_id.borrow(), "pinyin");
            assert!(
                !st.is_dirty(),
                "重灌后设置页不应判脏：{:?}",
                st.unsaved_items()
            );
        });
    }

    #[test]
    fn close_does_not_call_core_and_respects_checkbox() {
        with_mock(|| {
            let app = mock_app();
            open_wizard(&app);
            crate::rpc::take_mock_calls();
            app.borrow().wizard_dont_show.set(true);
            close(&app);
            let calls = crate::rpc::take_mock_calls();
            assert!(calls.is_empty(), "关闭不该调 core：{calls:?}");
            assert!(!app.borrow().wizard_visible.get());
            assert!(app.borrow().wizard.borrow().is_none());
            assert!(app.borrow().setting_config.ui.wizard_dismissed);
        });
    }

    #[test]
    fn open_when_dirty_starts_at_dirty_phase() {
        with_mock(|| {
            let app = mock_app();
            // 判脏写法同 state.rs 的关窗守卫测试：挂一个写出不同值的 writer。
            app.borrow().fields.writers.borrow_mut().push(Box::new(
                |cfg: &mut crate::config::Config| {
                    cfg.set_path("input.filter_mode", serde_json::json!("gb18030"));
                },
            ));
            open_wizard(&app);
            assert_eq!(app.borrow().wizard_phase.get(), WizardPhase::Dirty);
        });
    }

    #[test]
    fn open_when_clean_starts_at_step_one_with_current_prefill() {
        with_mock(|| {
            let app = mock_app();
            open_wizard(&app);
            let st = app.borrow();
            assert_eq!(st.wizard_phase.get(), WizardPhase::Step(1));
            assert!(st.wizard_visible.get());
            let s = st.wizard.borrow();
            let s = s.as_ref().unwrap();
            assert_eq!(s.draft, s.cur, "打开时草稿就是当前配置");
            assert!(!s.layouts.is_empty(), "mock 有 schema.layouts");
        });
    }

    /// 控制方裁定 #2：基线自带的触发键冲突与向导无关，不挡「下一步」；向导新造的才算。
    #[test]
    fn trigger_conflicts_only_count_new_ones() {
        use crate::config::Config;
        use serde_json::json;
        let base = Config::from_value(json!({
            "input": { "temp_english": { "trigger_keys": ["quote"] } },
            "keys": { "key_actions": { "quote": "mix:quick_mix" } }
        }));
        assert!(
            new_trigger_conflicts(&base, &base).is_none(),
            "基线原有的冲突不算"
        );
        let mut target = base.clone();
        target.set_path("input.temp_pinyin.trigger_keys", json!(["semicolon"]));
        target.set_path(
            "keys.key_actions",
            json!({ "quote": "mix:quick_mix", "semicolon": "mix:quick_mix" }),
        );
        assert!(
            new_trigger_conflicts(&base, &target).is_some(),
            "目标新增的冲突要报"
        );
    }

    /// 启动时 core 没起来、后来点「重试」连上的用户，也要看到首启向导。
    #[test]
    fn reconnect_auto_opens_wizard() {
        with_mock(|| {
            let app = mock_app();
            app.borrow_mut().setting_config.ui.wizard_dismissed = false;
            assert!(app.borrow().wizard.borrow().is_none());
            crate::main_reconnect(&app);
            assert!(app.borrow().wizard_visible.get());
            assert_eq!(app.borrow().wizard_phase.get(), WizardPhase::Step(1));
        });
    }

    #[test]
    fn auto_open_leaves_an_open_wizard_alone() {
        with_mock(|| {
            let app = mock_app();
            app.borrow_mut().setting_config.ui.wizard_dismissed = false;
            open_wizard(&app);
            app.borrow().wizard_phase.set(WizardPhase::Step(3));
            {
                let st = app.borrow();
                let mut s = st.wizard.borrow_mut();
                s.as_mut().unwrap().draft.slash_dun = !s.as_ref().unwrap().cur.slash_dun;
            }
            maybe_auto_open(&app, true);
            let st = app.borrow();
            assert_eq!(st.wizard_phase.get(), WizardPhase::Step(3));
            let s = st.wizard.borrow();
            let s = s.as_ref().unwrap();
            assert_ne!(s.draft, s.cur, "草稿不该被打回预填");
        });
    }

    #[test]
    fn auto_open_only_when_not_dismissed_and_connected() {
        assert!(should_auto_open(false, true));
        assert!(!should_auto_open(true, true));
        assert!(!should_auto_open(false, false));
    }
}
