//! 对话框的交互链路：选键 → 输入框 → 工作副本 → 「应用设置」发出去的请求。
//!
//! 不渲染：effect 是框架每帧驱动的，这里直接调它们落到的方法（`on_edit` 等），断言的是
//! 数据与发出去的 RPC，而不是像素。

use serde_json::Value;

use super::*;

struct MockGuard(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
impl MockGuard {
    fn new() -> Self {
        let g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let _ = crate::rpc::take_mock_calls();
        MockGuard(g)
    }
}
impl Drop for MockGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }
}

fn applied_ops() -> Vec<Vec<Value>> {
    crate::rpc::take_mock_calls()
        .into_iter()
        .filter(|(m, _)| m == "softkeyboard.apply")
        .map(|(_, p)| p["ops"].as_array().cloned().unwrap_or_default())
        .collect()
}

fn opened() -> (EditorUi, SoftKbdData) {
    let data = SoftKbdData::default();
    let ui = EditorUi::new(data.clone());
    ui.open();
    (ui, data)
}

fn slot_index(slot: &str) -> usize {
    all_slot_names().iter().position(|s| *s == slot).unwrap()
}

/// 切到指定 id 的面（等价于用户在下拉里选它，再由 effect 同步）。
fn goto(ui: &EditorUi, data: &SoftKbdData, id: &str) {
    let i = data
        .model
        .borrow()
        .pages
        .iter()
        .position(|p| p.id == id)
        .unwrap();
    ui.page_sel.set(i);
    ui.selected.set(NONE);
    ui.sync_page();
}

#[test]
fn open_loads_from_core_and_lists_pages() {
    let _m = MockGuard::new();
    let (ui, _) = opened();
    let names = ui.page_names.get();
    assert!(names.iter().any(|n| n == "标点符号（已修改）"), "{names:?}");
    assert!(names.iter().any(|n| n == "我的符号（自建）"), "{names:?}");
    assert!(ui.visible.get());
}

#[test]
fn editing_a_key_flows_into_the_commit() {
    let _m = MockGuard::new();
    let (ui, data) = opened();
    goto(&ui, &data, "punct");
    let w = slot_index("w");
    ui.select(w);
    assert_eq!(ui.edit_base.get(), "”", "选中即回填当前值");
    assert!(!data.is_dirty(), "回填不是编辑");

    ui.edit_base.set("√".into());
    ui.on_edit(false);
    assert_eq!(ui.caps_base[w].get(), "√", "键帽当场变");
    assert!(ui.caps_mod[w].get(), "标成已修改");
    assert!(data.is_dirty());

    data.commit().unwrap();
    let ops = applied_ops();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0][0]["op"], "setKey");
    assert_eq!(ops[0][0]["slot"], "w");
    assert_eq!(ops[0][0]["value"], "√");
    assert!(!data.is_dirty(), "提交后队列清空");
}

#[test]
fn programmatic_refill_is_not_an_edit() {
    let _m = MockGuard::new();
    let (ui, data) = opened();
    goto(&ui, &data, "punct");
    ui.select(slot_index("e"));
    // effect 在回填后的下一帧才跑：模拟它此时补跑一次。
    ui.on_edit(false);
    ui.on_edit(true);
    assert!(!data.is_dirty());
}

#[test]
fn cancel_reverts_to_the_state_at_open() {
    let _m = MockGuard::new();
    let (ui, data) = opened();
    goto(&ui, &data, "punct");
    ui.select(slot_index("w"));
    ui.edit_base.set("X".into());
    ui.on_edit(false);
    ui.cancel();
    assert!(!data.is_dirty());
    assert!(!ui.visible.get());
}

#[test]
fn keyboard_pages_are_read_only() {
    let _m = MockGuard::new();
    let (ui, data) = opened();
    goto(&ui, &data, "pc");
    assert!(!ui.editable.get());
    ui.select(slot_index("q"));
    assert_eq!(ui.selected.get(), NONE, "键盘面点键不选中");
}

#[test]
fn create_rename_delete_via_the_form() {
    let _m = MockGuard::new();
    let (ui, data) = opened();
    goto(&ui, &data, "punct");

    ui.open_new_form();
    ui.form.name.set("  ".into());
    ui.submit_form();
    assert!(!ui.form.error.get().is_empty(), "空名字就地报错");

    ui.form.name.set("数学".into());
    ui.form.copy.set(true);
    ui.submit_form();
    assert!(!ui.form.visible.get());
    let cur = ui.page_sel.get();
    assert_eq!(
        data.model.borrow().pages[cur].name,
        "数学",
        "新建后切到新面"
    );
    assert!(ui.can_delete.get());

    ui.open_rename_form();
    assert_eq!(ui.form.name.get(), "数学", "改名表单带上当前名");
    ui.form.name.set("数学符号".into());
    ui.submit_form();
    assert!(ui.page_names.get()[cur].starts_with("数学符号"));

    ui.delete_page();
    assert!(
        !data
            .model
            .borrow()
            .pages
            .iter()
            .any(|p| p.name == "数学符号")
    );

    data.commit().unwrap();
    let ops = applied_ops();
    let kinds: Vec<&str> = ops[0].iter().filter_map(|o| o["op"].as_str()).collect();
    assert_eq!(kinds, ["createPage", "renamePage", "deletePage"]);
    assert_eq!(ops[0][0]["copyFrom"], "punct");
}

#[test]
fn reset_page_restores_builtin() {
    let _m = MockGuard::new();
    let (ui, data) = opened();
    goto(&ui, &data, "punct");
    assert!(ui.can_reset.get(), "mock 里标点面改过一个键");
    ui.reset_page();
    assert!(!ui.can_reset.get());
    assert!(!ui.caps_mod[slot_index("q")].get());
    assert!(ui.page_names.get().iter().any(|n| n == "标点符号"));
}

#[test]
fn failed_commit_keeps_the_queue() {
    let _m = MockGuard::new();
    let (ui, data) = opened();
    goto(&ui, &data, "punct");
    ui.select(slot_index("w"));
    ui.edit_base.set("Y".into());
    ui.on_edit(false);
    crate::rpc::force_mock_error("softkeyboard.apply", Some("boom"));
    let r = data.commit();
    crate::rpc::force_mock_error("softkeyboard.apply", None);
    assert!(r.unwrap_err().contains("boom"));
    assert!(data.is_dirty(), "失败不丢编辑，重试仍会带上");
}

#[test]
fn load_failure_is_shown_instead_of_an_empty_board() {
    let _m = MockGuard::new();
    crate::rpc::force_mock_error("softkeyboard.get", Some("连接 core 失败"));
    let (ui, _) = opened();
    crate::rpc::force_mock_error("softkeyboard.get", None);
    assert!(
        ui.page_info.get().contains("连接 core 失败"),
        "{}",
        ui.page_info.get()
    );
}

/// Esc 只把 `visible` 置假（不经任何按钮）：关窗 effect 要把它当取消处理。
#[test]
fn esc_close_reverts_like_cancel_but_confirm_keeps() {
    let _m = MockGuard::new();
    let (ui, data) = opened();
    goto(&ui, &data, "punct");
    ui.select(slot_index("w"));
    ui.edit_base.set("X".into());
    ui.on_edit(false);
    ui.visible.set(false); // Esc
    ui.on_visibility_change();
    assert!(!data.is_dirty(), "Esc 等同取消");

    ui.open();
    goto(&ui, &data, "punct");
    ui.select(slot_index("w"));
    ui.edit_base.set("X".into());
    ui.on_edit(false);
    ui.confirm();
    ui.on_visibility_change();
    assert!(data.is_dirty(), "确认后编辑留着，等「应用设置」");
}
