use super::*;
use serde_json::json;

fn schema(id: &str, kind: &str, scheme: &str, hidden: bool) -> SchemaInfo {
    SchemaInfo {
        id: id.into(),
        name: format!("名-{id}"),
        kind: kind.into(),
        scheme: scheme.into(),
        hidden,
    }
}

fn schemas() -> Vec<SchemaInfo> {
    vec![
        schema("english", "english", "", true),
        schema("pinyin", "pinyin", "full", false),
        schema("shuangpin", "pinyin", "shuangpin", false),
        schema("sp2", "pinyin", "shuangpin", false),
        schema("wubi86", "codetable", "", false),
        schema("wubi98", "codetable", "", false),
        schema("wubi86_pinyin", "mixed", "", false),
    ]
}

fn cfg(v: Value) -> Config {
    Config::from_value(v)
}

#[test]
fn keys_map_to_cards() {
    assert_eq!(
        Card::of_key("schema.codetable.frequency.enabled"),
        Some(Card::Codetable)
    );
    assert_eq!(
        Card::of_key("schema.pinyin.sentence_count"),
        Some(Card::Pinyin)
    );
    assert_eq!(
        Card::of_key("schema.pinyin.aux_code.enabled"),
        Some(Card::Pinyin)
    );
    assert_eq!(
        Card::of_key("schema.pinyin.shuangpin.allow_full_pinyin"),
        Some(Card::Shuangpin)
    );
    assert_eq!(Card::of_key("schema.mix.enable_english"), Some(Card::Mixed));
    assert_eq!(
        Card::of_key("schema.english.commit_space"),
        Some(Card::English)
    );
    for global in [
        "schema.active",
        "schema.primary_pinyin",
        "schema.mix_modes",
        "input.fuzzy.enabled",
    ] {
        assert_eq!(Card::of_key(global), None, "{global}");
    }
}

#[test]
fn cards_bind_to_first_enabled_scheme_of_each_kind() {
    let c = cfg(json!({ "schema": { "available": ["wubi98", "sp2", "wubi86", "wubi86_pinyin"] } }));
    let s = CardSchemes::resolve(&c, &schemas());
    assert_eq!(s.id(Card::Codetable), Some("wubi98"));
    assert_eq!(s.id(Card::Shuangpin), Some("sp2"));
    assert_eq!(s.id(Card::Mixed), Some("wubi86_pinyin"));
    assert_eq!(s.id(Card::English), Some("english"));
    // 全拼没启用：退到已安装的那个，名字随方案。
    assert_eq!(s.id(Card::Pinyin), Some("pinyin"));
    assert_eq!(s.name(Card::Pinyin), Some("名-pinyin"));
}

#[test]
fn explicit_primary_schemes_win() {
    let c = cfg(json!({ "schema": {
        "available": ["wubi98", "sp2", "shuangpin"],
        "primary_codetable": "wubi86",
        "primary_pinyin": "shuangpin",
    } }));
    let s = CardSchemes::resolve(&c, &schemas());
    assert_eq!(s.id(Card::Codetable), Some("wubi86"));
    assert_eq!(s.id(Card::Shuangpin), Some("shuangpin"));
    // 主拼音方案指向全拼时，双拼卡仍按启用顺序找。
    let c = cfg(json!({ "schema": { "available": ["sp2"], "primary_pinyin": "pinyin" } }));
    assert_eq!(
        CardSchemes::resolve(&c, &schemas()).id(Card::Shuangpin),
        Some("sp2")
    );
}

#[test]
fn missing_kind_leaves_card_unbound() {
    let only = vec![schema("pinyin", "pinyin", "full", false)];
    let s = CardSchemes::resolve(&cfg(json!({})), &only);
    assert_eq!(s.id(Card::Pinyin), Some("pinyin"));
    for c in [Card::Codetable, Card::Shuangpin, Card::Mixed, Card::English] {
        assert_eq!(s.id(c), None, "{c:?}");
    }
}

#[test]
fn overlay_only_takes_keys_belonging_to_the_card() {
    let c0 = cfg(json!({ "schema": { "available": ["pinyin", "shuangpin"],
        "pinyin": { "sentence_count": 1, "shuangpin": { "allow_full_pinyin": false } } } }));
    let schemes = CardSchemes::resolve(&c0, &schemas());
    let map = |v: Value| v.as_object().unwrap().clone();
    let settings = CardSettings {
        schemes,
        effective: HashMap::from([
            (
                "pinyin".to_string(),
                map(json!({ "schema.pinyin.sentence_count": 2,
                            "schema.pinyin.shuangpin.allow_full_pinyin": false })),
            ),
            (
                "shuangpin".to_string(),
                map(json!({ "schema.pinyin.sentence_count": 5,
                            "schema.pinyin.shuangpin.allow_full_pinyin": true })),
            ),
        ]),
        ..Default::default()
    };
    let mut c = c0.clone();
    settings.overlay(&mut c);
    assert_eq!(
        c.get_u64("schema.pinyin.sentence_count"),
        Some(2),
        "拼音卡取全拼方案的值"
    );
    assert_eq!(
        c.get_bool("schema.pinyin.shuangpin.allow_full_pinyin"),
        Some(true),
        "双拼卡取双拼方案的值"
    );
}

#[test]
fn split_routes_scheme_keys_to_their_scheme() {
    let c = cfg(json!({ "schema": { "available": ["wubi86", "pinyin", "shuangpin"] } }));
    let schemes = CardSchemes::resolve(&c, &schemas());
    let item = |k: &str| ConfigItem {
        key: k.into(),
        value: json!(true),
    };
    let out = split_items(
        vec![
            item("ui.candidate.per_page"),
            item("schema.codetable.wildcard"),
            item("schema.pinyin.single_char"),
            item("schema.pinyin.shuangpin.allow_full_pinyin"),
            item("input.fuzzy.enabled"),
        ],
        &schemes,
    );
    let keys = |v: &[ConfigItem]| v.iter().map(|i| i.key.clone()).collect::<Vec<_>>();
    assert_eq!(
        keys(&out.global),
        ["ui.candidate.per_page", "input.fuzzy.enabled"]
    );
    assert_eq!(
        keys(&out.by_scheme["wubi86"]),
        ["schema.codetable.wildcard"]
    );
    assert_eq!(
        keys(&out.by_scheme["pinyin"]),
        ["schema.pinyin.single_char"]
    );
    assert_eq!(
        keys(&out.by_scheme["shuangpin"]),
        ["schema.pinyin.shuangpin.allow_full_pinyin"]
    );
    assert!(out.orphan.is_empty());
}

#[test]
fn default_patch_takes_each_cards_scheme_defaults() {
    let c0 = cfg(json!({ "schema": { "available": ["pinyin", "shuangpin"] } }));
    let schemes = CardSchemes::resolve(&c0, &schemas());
    let map = |v: Value| v.as_object().unwrap().clone();
    let settings = CardSettings {
        schemes,
        default: HashMap::from([
            (
                "pinyin".to_string(),
                map(json!({ "schema.pinyin.sentence_count": 1,
                            "schema.pinyin.shuangpin.allow_full_pinyin": false })),
            ),
            (
                "shuangpin".to_string(),
                map(json!({ "schema.pinyin.sentence_count": 3,
                            "schema.pinyin.shuangpin.allow_full_pinyin": true })),
            ),
        ]),
        ..Default::default()
    };
    let mut patch = settings.default_patch();
    patch.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        patch,
        [
            ("schema.pinyin.sentence_count".to_string(), json!(1)),
            (
                "schema.pinyin.shuangpin.allow_full_pinyin".to_string(),
                json!(true)
            ),
        ]
    );
}
