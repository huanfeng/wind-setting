use super::*;

fn model() -> Model {
    Model::from_view(parse_view(&super::super::mock::view()).unwrap())
}

fn page_idx(m: &Model, id: &str) -> usize {
    m.pages.iter().position(|p| p.id == id).unwrap()
}

#[test]
fn mock_view_parses_and_has_every_slot() {
    let m = model();
    assert!(m.pages.len() >= 2);
    for p in &m.pages {
        let slots: Vec<&str> = p.keys.iter().map(|k| k.slot.as_str()).collect();
        assert_eq!(slots, all_slot_names(), "面 {} 的键位序", p.id);
    }
    assert!(!m.is_dirty());
}

#[test]
fn set_key_updates_copy_and_queues_one_op() {
    let mut m = model();
    let p = page_idx(&m, "punct");
    m.set_key(p, "w", false, Some("√".into()));
    assert_eq!(m.pages[p].key("w").unwrap().base.as_deref(), Some("√"));
    assert!(m.pages[p].key("w").unwrap().modified());
    assert_eq!(
        m.pending,
        vec![json!({"op":"setKey","page":"punct","slot":"w","shift":false,"value":"√"})]
    );
}

#[test]
fn typing_into_one_key_coalesces() {
    let mut m = model();
    let p = page_idx(&m, "punct");
    for v in ["a", "ab", "abc"] {
        m.set_key(p, "q", true, Some(v.into()));
    }
    assert_eq!(m.pending.len(), 1);
    assert_eq!(m.pending[0]["value"], "abc");
    // 换一个键就是新的一条。
    m.set_key(p, "w", true, Some("x".into()));
    assert_eq!(m.pending.len(), 2);
}

#[test]
fn setting_the_same_value_is_a_no_op() {
    let mut m = model();
    let p = page_idx(&m, "punct");
    let cur = m.pages[p].key("q").unwrap().base.clone();
    m.set_key(p, "q", false, cur);
    assert!(!m.is_dirty(), "选中键回填输入框不该产生操作");
}

#[test]
fn empty_string_means_empty_slot() {
    let mut m = model();
    let p = page_idx(&m, "punct");
    m.set_key(p, "q", false, Some(String::new()));
    assert_eq!(m.pages[p].key("q").unwrap().base, None);
    assert_eq!(m.pending[0]["value"], Value::Null);
}

#[test]
fn reset_key_restores_both_layers() {
    let mut m = model();
    let p = page_idx(&m, "punct");
    m.set_key(p, "q", false, Some("x".into()));
    m.set_key(p, "q", true, Some("y".into()));
    m.reset_key(p, "q");
    assert!(!m.pages[p].key("q").unwrap().modified());
    assert!(m.pending.iter().any(|o| o["op"] == "resetKey"));
}

#[test]
fn create_blank_and_copied_pages() {
    let mut m = model();
    let blank = m.create_page("我的", None);
    assert_eq!(m.pages[blank].id, "user1");
    assert!(m.pages[blank].user_created);
    assert!(m.pages[blank].keys.iter().all(|k| k.base.is_none()));

    let src = page_idx(&m, "punct");
    let copy = m.create_page("副本", Some(src));
    assert_eq!(m.pages[copy].id, "user2", "已占用的 id 跳过");
    assert_eq!(
        m.pages[copy].key("q").unwrap().base,
        m.pages[src].key("q").unwrap().base
    );
    assert_eq!(m.pending[1]["copyFrom"], "punct");
    assert_eq!(m.pending[0]["copyFrom"], Value::Null);
}

#[test]
fn ids_of_pages_deleted_in_this_batch_are_not_reused() {
    let mut m = model();
    let a = m.create_page("a", None);
    assert!(m.delete_page(a));
    let b = m.create_page("b", None);
    assert_ne!(m.pages[b].id, "user1");
}

#[test]
fn delete_only_user_pages_reset_only_builtin_pages() {
    let mut m = model();
    let builtin = page_idx(&m, "punct");
    assert!(!m.delete_page(builtin));
    let mine = page_idx(&m, "mine");
    assert!(!m.reset_page(mine));

    m.set_key(builtin, "q", false, Some("x".into()));
    m.rename_page(builtin, "改名");
    assert!(m.pages[builtin].modified());
    assert!(m.reset_page(builtin));
    assert!(!m.pages[builtin].modified());
    assert_eq!(m.pages[builtin].name, m.pages[builtin].default_name);

    assert!(m.delete_page(mine));
    assert!(!m.pages.iter().any(|p| p.id == "mine"));
}

#[test]
fn renaming_a_user_page_moves_its_default_name() {
    let mut m = model();
    let mine = page_idx(&m, "mine");
    m.rename_page(mine, "新名");
    assert!(!m.pages[mine].modified(), "新建面没有出厂名可回");
}

#[test]
fn captions_match_physical_keys() {
    assert_eq!(slot_caption("q"), "Q");
    assert_eq!(slot_caption("lbracket"), "[");
    assert_eq!(slot_caption("grave"), "`");
    assert_eq!(slot_caption("7"), "7");
}

#[test]
fn slot_rows_count() {
    assert_eq!(all_slot_names().len(), 47);
}

#[test]
fn slot_rows_match_core() {
    assert_eq!(SLOT_ROWS, wind_softkeyboard::KEY_ROWS);
}

/// mock 的回复与 core 真实输出逐字段同形（字段名是跨仓契约，拼错一个就静默读成默认值）。
#[test]
fn mock_shape_matches_core_view() {
    let core = serde_json::to_value(
        wind_softkeyboard::admin::view(&wind_softkeyboard::SoftKeyboardTable::builtin(), "")
            .unwrap(),
    )
    .unwrap();
    let mock = super::super::mock::view();
    let keys_of = |v: &Value| -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    let (cp, mp) = (&core[0], &mock["pages"][0]);
    assert_eq!(keys_of(cp), keys_of(mp), "面的字段");
    assert_eq!(
        keys_of(&cp["keys"][0]),
        keys_of(&mp["keys"][0]),
        "键位的字段"
    );
    // 且 core 的真实输出能被本模块解析。
    let parsed = parse_view(&serde_json::json!({ "pages": core })).unwrap();
    assert_eq!(parsed[0].keys.len(), 47);
}
