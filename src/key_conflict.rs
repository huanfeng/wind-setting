/// 一个参与冲突检测的按键分组。
///
/// `id` 是**稳定标识**，供分级规则（[`pair_level`]）匹配；`label` 只用于文案呈现。
/// 两者分开是必需的：规则若按 label 匹配，改一次文案就会静默失掉分级，
/// 而分级失效的表现是「本该只提示一次的 Toast 变成常驻警告条」——没人会去查代码。
/// 动态分组（每个 mix / 特殊模式实例各一组）用 `id` 前缀 + 序号，见 `state.rs` 的收集函数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyGroup {
    pub id: String,
    pub label: String,
    pub keys: Vec<String>,
}

impl KeyGroup {
    pub fn new(id: &str, label: impl Into<String>, keys: Vec<String>) -> Self {
        Self {
            id: id.to_string(),
            label: label.into(),
            keys,
        }
    }
}

/// 冲突的严重级别——决定它以什么形式呈现给用户。
///
/// 判据是**内核对这次争用的实际处置**，不是「看起来像不像问题」：
/// - `Blocking`：两个功能在同一输入状态下都想要这个键，内核只能选一个 → 另一个**静默失效**。
///   用户配了却不生效、且没有任何反馈，必须常驻警告条催他改。
/// - `Coexist`：内核有确定的分时裁决，**两个功能都能用**，只是各自的生效条件不同。
///   这不是错误配置，常驻警告条会把「可用的设计」误报成「待修的问题」；
///   但用户也未必知道分时规则，故在**刚配出这个组合时** Toast 告知一次实际行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictLevel {
    Blocking,
    Coexist,
}

/// 一条冲突记录：`key` 为归一化后的物理键/组合键 token，`a`/`b` 为两侧分组。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub key: String,
    pub a_label: String,
    pub b_label: String,
    pub level: ConflictLevel,
}

impl Conflict {
    /// 去重/变化检测用的稳定指纹（同一对分组的同一个键恒等），与文案无关。
    pub fn fingerprint(&self) -> String {
        format!("{}|{}|{}", self.key, self.a_label, self.b_label)
    }
}

/// 判定一对分组争用同一个键时的级别。**唯一的分级真相源**，新增共存规则只加这里一条。
///
/// 目前唯一的 `Coexist`：**修饰键次选（`lrshift`/`lrctrl`）× 中英切换键**。
/// 内核 2026-08-03 起把修饰键选词改到 KEY_UP 轻敲，并明确裁决为「有候选选词、
/// 无候选切换」（见 `../WindInput/wind_input/crates/wind-coordinator/src/handle_candidate.rs`
/// 的 `handle_select_key_up`）——两个功能都留着，只是分时。
/// 在此之前它确实是 Blocking（选词吃掉 KEY_DOWN，切换永远等不到 `had_pending`），
/// 本仓的文案也一直按「真冲突」写，是内核行为变了、这里跟着变。
fn pair_level(id_a: &str, id_b: &str) -> ConflictLevel {
    let pair = (id_a, id_b);
    let coexist = matches!(
        pair,
        ("select_keys", "toggle_mode") | ("toggle_mode", "select_keys")
    );
    if coexist {
        ConflictLevel::Coexist
    } else {
        ConflictLevel::Blocking
    }
}

/// 找出多个分组两两之间重叠的按键，每对只报一次，按 [`pair_level`] 分级。
pub fn find_key_conflicts(groups: &[KeyGroup]) -> Vec<Conflict> {
    let mut hits = Vec::new();
    for i in 0..groups.len() {
        for j in (i + 1)..groups.len() {
            let (a, b) = (&groups[i], &groups[j]);
            for key in &a.keys {
                if b.keys.contains(key) {
                    hits.push(Conflict {
                        key: key.clone(),
                        a_label: a.label.clone(),
                        b_label: b.label.clone(),
                        level: pair_level(&a.id, &b.id),
                    });
                }
            }
        }
    }
    hits
}

/// 触发键内部名/候选组名 → 可读符号；未知键回退原名，避免提示里出现生硬的英文标识。
/// 触发键取值见 settings_manifest.toml 触发键 options（单键名）；候选组名为成组按键
/// （semicolon_quote 等，一名代表一组物理键），取值见"候选操作" section。
pub fn key_symbol(key: &str) -> &str {
    match key {
        // ── 触发键：单键名 ──
        "backtick" | "grave" => "`",
        "semicolon" => ";",
        "quote" | "apostrophe" => "'",
        "comma" => ",",
        "period" => ".",
        "slash" => "/",
        "backslash" => "\\",
        "lbracket" => "[",
        "rbracket" => "]",
        // ── 候选操作：成组键名（expand 后仍可能出现的组名） ──
        "semicolon_quote" => "; '",
        "comma_period" => ", .",
        "minus_equal" => "- =",
        "brackets" => "[ ]",
        "tab" => "Tab/Shift+Tab",
        "arrows" => "方向键 ↑↓",
        "pageupdown" => "PageUp/PageDown",
        "lrshift" => "左/右 Shift",
        "lrctrl" => "左/右 Ctrl",
        // ── 单修饰键（expand 展开后 / toggle_mode_keys 原值） ──
        "lshift" => "左Shift",
        "rshift" => "右Shift",
        "lctrl" => "左Ctrl",
        "rctrl" => "右Ctrl",
        "capslock" => "CapsLock",
        other => other,
    }
}

/// 候选操作组名展开成 canonical 物理键 token（用于候选域归一化检测）：
/// - `shift_tab`(翻页) 与 `tab`(高亮移动) 是同一对物理键（Tab/Shift+Tab），归一到 `tab`；
/// - `lrshift`/`lrctrl`(左右 Shift/Ctrl 次选) 展开成 `{lshift,rshift}`/`{lctrl,rctrl}`，
///   以便与 `keys.toggle_mode_keys`(中英切换，值为 lshift/rshift/…) 跨组匹配出冲突；
/// - 其余组名域内同名即同键，原样返回单元素。
pub fn expand_candidate_key(group: &str) -> Vec<&str> {
    match group {
        "shift_tab" => vec!["tab"],
        "lrshift" => vec!["lshift", "rshift"],
        "lrctrl" => vec!["lctrl", "rctrl"],
        other => vec![other],
    }
}

/// 组合键（功能快捷键）归一化为唯一显示形：拆分 `+`、大小写无关、修饰键按固定序
/// （Ctrl<Shift<Alt<Win）排前、主键在后。故 `shift+ctrl+e` 与 `ctrl+shift+e` 归一后相等，
/// 既用于冲突比较也直接作显示（如 `Ctrl+Shift+E`、`Ctrl+数字`）。空/`none` 由调用方先行剔除。
pub fn canonical_hotkey(s: &str) -> String {
    fn display_main(k: &str) -> String {
        match k {
            "number" => "数字".to_string(),
            "equal" => "=".to_string(),
            "minus" => "-".to_string(),
            "space" => "Space".to_string(),
            _ => {
                let mut c = k.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            }
        }
    }
    let mut mods: Vec<(u8, &str)> = Vec::new();
    let mut mains: Vec<String> = Vec::new();
    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => mods.push((0, "Ctrl")),
            "shift" => mods.push((1, "Shift")),
            "alt" => mods.push((2, "Alt")),
            "win" | "super" | "meta" | "cmd" => mods.push((3, "Win")),
            other => mains.push(display_main(other)),
        }
    }
    mods.sort_by_key(|(o, _)| *o);
    mods.dedup_by_key(|(o, _)| *o);
    let mut out: Vec<String> = mods.into_iter().map(|(_, m)| m.to_string()).collect();
    out.extend(mains);
    out.join("+")
}

/// 组合键值 → 用于比较的归一 token 列表。空 / `none` 返回空表（未设置）。
///
/// **`ctrl+number` 这类模板会展开成十个具体键**（`Ctrl+0`…`Ctrl+9`）。
/// 置顶/删除词条用的就是模板，而热键控件的主键列表里**有 0-9**（`manifest::hotkey_keys`），
/// 于是「把打开设置配成 `Ctrl+1`」是 GUI 里点得出来的操作——不展开就检不出这层重叠，
/// 用户只会看到热键按下去没反应（内核 key_down 表按注册序 `.find()`，先入列者赢）。
pub fn expand_hotkey(raw: &str) -> Vec<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    let canon = canonical_hotkey(raw);
    match canon.strip_suffix("数字") {
        // 只认「修饰键 + 数字」整体模板；`Ctrl+Shift+数字` 与 `Ctrl+数字` 因修饰位不同不会互撞。
        Some(prefix) if prefix.ends_with('+') => (0..=9).map(|d| format!("{prefix}{d}")).collect(),
        _ => vec![canon],
    }
}

// ─────────────── 住在专用字段里的按键绑定：配置路径 → 中文名 ───────────────
//
// 这三张表的共同点是**它们与 `keys.key_actions` 编译进同一张热键表**（core
// `wind-config/src/hotkey.rs` 的 `compile`），撞了就是一方静默失效。故两处都要认它们：
// 冲突检测要同域比（`state::collect_hotkey_groups`），全局按键编辑器要在只读组里
// 让用户看见（`manifest::foreign_key_bindings`）。
//
// ★ 提到这里而不是各写一份：两处各抄一遍的话，加一个热键字段只改一处的表现是
// 「冲突检测报得出、列表里查无此项」或反过来——两种都让人怀疑是自己看错了。

/// 组合键热键字段（`[keys]` 段的「一个功能一个字段」那批）。与 `key_actions` 里的
/// **组合键**条目同域（key_down 表，匹配走 `.find()` 先注册者赢）。
///
/// `pin_candidate` / `delete_candidate` 的值是**模板**（`ctrl+number`），一条展开成
/// 11 个键，见 [`expand_hotkey`]。
pub const HOTKEY_FIELDS: &[(&str, &str)] = &[
    ("keys.switch_engine", "切换输入方案"),
    ("keys.toggle_full_width", "切换全角/半角"),
    ("keys.toggle_punct", "切换中/英文标点"),
    ("keys.toggle_s2t", "切换简入繁出"),
    ("keys.toggle_t2s", "切换繁入简出"),
    ("keys.toggle_toolbar", "显示/隐藏工具栏"),
    ("keys.softkeyboard", "显示/隐藏软键盘"),
    ("keys.open_settings", "打开设置"),
    ("keys.add_word", "快捷加词"),
    ("keys.open_add_word_dialog", "打开加词界面"),
    ("keys.activate_ime", "切换到本输入法"),
    ("keys.pin_candidate", "置顶词条"),
    ("keys.delete_candidate", "删除词条"),
];

/// 模式直达热键：与 [`HOTKEY_FIELDS`] 同域，只是落点在 `input` 段。
///
/// 特殊模式的那份**不在这里**——它已收编进 `keys.key_actions`（`special:<方案id>`），
/// 属于用户在全局表里自己配的条目，不是「别处占用」。
pub const MODE_HOTKEY_FIELDS: &[(&str, &str)] = &[("input.temp_pinyin.hotkey", "临时拼音直达热键")];

/// 中英切换键：**唯一**一个住在专用字段里的修饰键绑定，与 `key_actions` 的修饰键条目
/// 编译进同一张 key_up 表（服务端 `handle_bound_modifier_key_up` 先于
/// `is_toggle_mode_keycode`，即 `key_actions` 那条赢）。
///
/// ⚠️ 它的值域含 `capslock`，而 `key_actions` **表达不了 CapsLock**：core 的
/// `route_of_key_action` 只把 `VK_LSHIFT..=VK_RCONTROL` 判为修饰键通路，CapsLock（0x14）
/// 落引导键通路 ⇒ 配了不进热键表、静默失效。所以这个字段不能简单地并进那张表——
/// 要合并，前置条件是先补上 core 的那处值域。
pub const TOGGLE_MODE_FIELD: (&str, &str) = ("keys.toggle_mode_keys", "中英文切换");

// ───────────── 候选操作键组：与 `keys.session_actions` 同域 ─────────────

/// 「候选操作」那批键组字段：配置路径 → 中文名。**与 `keys.session_actions` 同域**
/// （core 的 `effective_session_actions` 把它们折算进那张表，显式项优先）。
///
/// ⚠️ 与 [`HOTKEY_FIELDS`] 那批**分属两张表**，别混：这四组进「自定义输入中按键」编辑器
/// 的只读组，那批进「全局自定义按键」的。判据是「它折算进哪张表」，不是「像不像按键」。
pub const CANDIDATE_FIELDS: &[(&str, &str)] = &[
    ("keys.select_key_groups", "次选/三选键"),
    ("keys.page_keys", "翻页键"),
    ("keys.highlight_keys", "高亮移动键"),
    ("keys.select_char_keys", "以词定字"),
];

/// 键组名 → 它实际占用的**会话态键名**。
///
/// # 为什么这份表非有不可
///
/// [`expand_candidate_key`] 展开的是**比较用的组名 token**（`minus_equal` 原样返回），
/// 够候选域内部两两比，但答不出「用户显式配的 `minus` 是不是落在 `minus_equal` 这一组
/// 里」——那要的是具体键。`keys_overview` 那条注释说的「别在设置页再写一份展开表」针对的
/// 是**完整仲裁**（算最终生效表，那必须由 core 算）；这里只用于高亮提示，判错的后果是
/// 少标或多标一个橙色，不是显示错误的生效结果。
///
/// ⚠️ 与 core 的四张 `*_group_binds` 是**跨仓手写清单**，无编译期约束。守门测试
/// `candidate_group_keys_are_valid_session_keys` 拿 `session_base_keys()` 交叉校验键名
/// 拼写（core 那边同一批名字要过 `session_key_name_to_vk`），拼错即红。
///
/// ★ 同名组在 core 的四张表里展开一致（`comma_period` 恒是 comma+period），故合成一张。
/// 各字段**能用哪些组名**由设置清单的 options 管（core 那边张冠李戴过一次：用选词键组的
/// 解析器解以词定字配置，`brackets` 静默失效），本表不负责那道校验。
const CANDIDATE_GROUP_KEYS: &[(&str, &[&str])] = &[
    ("semicolon_quote", &["semicolon", "quote"]),
    ("comma_period", &["comma", "period"]),
    ("minus_equal", &["minus", "equal"]),
    ("brackets", &["lbracket", "rbracket"]),
    ("pageupdown", &["pageup", "pagedown"]),
    // 翻页的 `shift_tab` 与高亮的 `tab` 是同一对物理键，展开结果相同。
    ("shift_tab", &["tab", "shift+tab"]),
    ("tab", &["tab", "shift+tab"]),
    ("arrows", &["up", "down"]),
    ("lrshift", &["lshift", "rshift"]),
    ("lrctrl", &["lctrl", "rctrl"]),
];

/// 本表认得的全部组名。**仅供守门测试**遍历校验键名拼写（：产品代码里
/// 没有「遍历全部组名」的需求，去掉这个门就是一条 dead_code 告警）。
#[cfg(test)]
pub fn candidate_group_names() -> impl Iterator<Item = &'static str> {
    CANDIDATE_GROUP_KEYS.iter().map(|(n, _)| *n)
}

/// 组名 → 会话态键名；未知组名返回空（手写配置里的生僻值不该 panic，也不该乱标橙色）。
pub fn candidate_group_keys(group: &str) -> &'static [&'static str] {
    let g = group.trim().to_lowercase();
    CANDIDATE_GROUP_KEYS
        .iter()
        .find(|(n, _)| *n == g)
        .map(|(_, ks)| *ks)
        .unwrap_or(&[])
}

/// 把 **Blocking** 冲突拼成一行警告文案；无 Blocking 冲突返回 None。
/// 键名以符号呈现（见 [`key_symbol`]）。`scene` 为场景标签（如"触发键"/"候选键"），
/// 冠于文案前区分冲突域。
///
/// `Coexist` 条目**不进这里**——它们不是待修的问题，走 [`coexist_note`] 的 Toast。
pub fn conflict_summary(hits: &[Conflict], scene: &str) -> Option<String> {
    // 同一对分组撞了多个键时合并成一条：`lrctrl` 展开后左右 Ctrl 各撞一次、
    // `Ctrl+数字` 模板展开后十个数字键各撞一次——逐条列出会把警告条刷成一屏，
    // 而用户要采取的动作是同一个（改这一对里的某一项）。
    let mut order: Vec<(String, String)> = Vec::new();
    let mut merged: std::collections::HashMap<(String, String), Vec<String>> =
        std::collections::HashMap::new();
    for c in hits.iter().filter(|c| c.level == ConflictLevel::Blocking) {
        let pair = (c.a_label.clone(), c.b_label.clone());
        if !merged.contains_key(&pair) {
            order.push(pair.clone());
        }
        merged
            .entry(pair)
            .or_default()
            .push(key_symbol(&c.key).to_string());
    }
    if order.is_empty() {
        return None;
    }
    let parts: Vec<String> = order
        .iter()
        .map(|pair| {
            let keys = &merged[pair];
            let shown = if keys.len() > 3 {
                format!("{} 等 {} 个键", keys[..3].join("、"), keys.len())
            } else {
                keys.join("、")
            };
            format!("{shown} 同时被「{}」「{}」使用", pair.0, pair.1)
        })
        .collect();
    Some(format!("⚠ {scene}冲突：{}", parts.join("；")))
}

/// 一条 `Coexist` 冲突的行为说明（Toast 文案）。非 Coexist 返回 None。
///
/// 文案只讲**实际会发生什么**，不用"冲突/警告"这类措辞——它描述的是可用的设计，
/// 说成问题只会让用户去改一个本来没坏的配置。
pub fn coexist_note(c: &Conflict) -> Option<String> {
    if c.level != ConflictLevel::Coexist {
        return None;
    }
    // 目前唯一的共存对，说明文字直接对应内核裁决；将来若有第二条，按分组 id 分支。
    Some(format!(
        "{} 兼作「{}」与「{}」：有候选时选词，无候选时切中英文",
        key_symbol(&c.key),
        c.a_label,
        c.b_label
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(id: &str, label: &str, keys: &[&str]) -> KeyGroup {
        KeyGroup::new(id, label, keys.iter().map(|s| s.to_string()).collect())
    }

    /// 候选组名展开后建组，模拟 `state.rs` 的收集口径。
    fn expanded(id: &str, label: &str, group: &str) -> KeyGroup {
        KeyGroup::new(
            id,
            label,
            expand_candidate_key(group)
                .into_iter()
                .map(String::from)
                .collect(),
        )
    }

    #[test]
    fn test_no_conflict() {
        let groups = vec![
            g("temp_pinyin", "临时拼音", &["backtick"]),
            g("temp_english", "临时英文", &["z"]),
        ];
        assert!(find_key_conflicts(&groups).is_empty());
    }

    #[test]
    fn test_single_conflict() {
        let groups = vec![
            g("temp_pinyin", "临时拼音", &["semicolon"]),
            g("mix_0", "快捷", &["semicolon"]),
        ];
        let hits = find_key_conflicts(&groups);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "semicolon");
        assert_eq!(hits[0].a_label, "临时拼音");
        assert_eq!(hits[0].b_label, "快捷");
        assert_eq!(
            hits[0].level,
            ConflictLevel::Blocking,
            "两个触发键抢同一个键，先匹配者赢、另一个永不触发 → 必须是 Blocking"
        );
    }

    #[test]
    fn test_multi_key_multi_group_conflict() {
        let groups = vec![
            g("a", "A", &["x", "y"]),
            g("b", "B", &["x"]),
            g("c", "C", &["y", "x"]),
        ];
        let hits = find_key_conflicts(&groups);
        // A-B: x；A-C: x,y；B-C: x
        assert_eq!(hits.len(), 4);
        let fps: Vec<String> = hits.iter().map(|c| c.fingerprint()).collect();
        assert!(fps.contains(&"x|A|B".to_string()));
        assert!(fps.contains(&"x|A|C".to_string()));
        assert!(fps.contains(&"y|A|C".to_string()));
        assert!(fps.contains(&"x|B|C".to_string()));
    }

    #[test]
    fn test_empty_groups_or_keys() {
        assert!(find_key_conflicts(&[]).is_empty());
        let groups = vec![g("a", "A", &[]), g("b", "B", &["x"])];
        assert!(find_key_conflicts(&groups).is_empty());
    }

    #[test]
    fn test_summary_empty_is_none() {
        assert_eq!(conflict_summary(&[], "触发键"), None);
    }

    #[test]
    fn test_summary_formats_hits() {
        let groups = vec![
            g("temp_pinyin", "临时拼音", &["semicolon"]),
            g("mix_0", "快捷", &["semicolon"]),
        ];
        let msg = conflict_summary(&find_key_conflicts(&groups), "触发键").unwrap();
        assert!(msg.contains("触发键冲突"), "应冠场景标签");
        assert!(msg.contains(';'), "内部键名应渲染为符号");
        assert!(!msg.contains("semicolon"), "不应再出现英文键名");
        assert!(msg.contains("临时拼音"));
        assert!(msg.contains("快捷"));
    }

    #[test]
    fn test_expand_candidate_key() {
        // highlight_keys 的 tab 与 page_keys 的 shift_tab 是同一对物理键，归一到 tab。
        assert_eq!(expand_candidate_key("shift_tab"), vec!["tab"]);
        assert_eq!(expand_candidate_key("tab"), vec!["tab"]);
        // lrshift/lrctrl 展开成单修饰键，供与 toggle_mode_keys 跨组匹配。
        assert_eq!(expand_candidate_key("lrshift"), vec!["lshift", "rshift"]);
        assert_eq!(expand_candidate_key("lrctrl"), vec!["lctrl", "rctrl"]);
        assert_eq!(
            expand_candidate_key("comma_period"),
            vec!["comma_period"],
            "其余原样"
        );
    }

    #[test]
    fn test_tab_and_shift_tab_conflict_after_expand() {
        // 高亮移动(tab) 与 翻页(shift_tab) 归一后应检出冲突，且是 Blocking——
        // 内核 NavKeys::classify 用 .find()，同一个键只会命中先入列的那条绑定。
        let groups = vec![
            expanded("highlight_keys", "高亮移动键", "tab"),
            expanded("page_keys", "翻页键", "shift_tab"),
        ];
        let hits = find_key_conflicts(&groups);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].level, ConflictLevel::Blocking);
        let msg = conflict_summary(&hits, "候选键").unwrap();
        assert!(msg.contains("候选键冲突"));
        assert!(msg.contains("Tab"), "归一后的 tab 应显示为 Tab/Shift+Tab");
    }

    // ── 分级：修饰键次选 × 中英切换 = 可共存（内核已定分时裁决）─────────────

    #[test]
    fn test_modifier_select_vs_toggle_is_coexist() {
        let groups = vec![
            expanded("select_keys", "次选/三选键", "lrctrl"),
            g("toggle_mode", "中英切换键", &["lctrl", "rctrl"]),
        ];
        let hits = find_key_conflicts(&groups);
        assert_eq!(hits.len(), 2, "左右 Ctrl 各撞一次");
        assert!(
            hits.iter().all(|c| c.level == ConflictLevel::Coexist),
            "内核裁决为有候选选词、无候选切换，两个功能都可用 → 不是 Blocking"
        );
        assert_eq!(
            conflict_summary(&hits, "候选键"),
            None,
            "可共存的不进常驻警告条"
        );
        let note = coexist_note(&hits[0]).expect("应给出行为说明");
        assert!(note.contains("左Ctrl"));
        assert!(note.contains("有候选时选词"));
        assert!(note.contains("无候选时切中英文"));
        assert!(
            !note.contains('⚠') && !note.contains("冲突"),
            "共存文案不应用告警措辞：{note}"
        );
    }

    #[test]
    fn test_coexist_note_rejects_blocking() {
        let groups = vec![
            expanded("select_keys", "次选/三选键", "comma_period"),
            expanded("page_keys", "翻页键", "comma_period"),
        ];
        let hits = find_key_conflicts(&groups);
        assert_eq!(
            hits[0].level,
            ConflictLevel::Blocking,
            "次选与翻页在有候选态争同一键，翻页先被处理、次选失效"
        );
        assert_eq!(coexist_note(&hits[0]), None);
        assert!(conflict_summary(&hits, "候选键").is_some(), "应进警告条");
    }

    /// 分级按 **id** 判定，与 label 文案无关——改文案不得让共存降级成 Blocking。
    #[test]
    fn test_level_keyed_on_id_not_label() {
        let groups = vec![
            expanded("select_keys", "换个说法的次选键", "lrshift"),
            g("toggle_mode", "换个说法的中英切换", &["lshift"]),
        ];
        let hits = find_key_conflicts(&groups);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].level, ConflictLevel::Coexist);
    }

    // ── 合并呈现 ────────────────────────────────────────────────

    #[test]
    fn test_summary_merges_same_pair() {
        // 同一对分组撞了两个键（左右 Shift）→ 合并成一条，不重复出现分组名。
        let groups = vec![
            expanded("select_keys", "次选/三选键", "lrshift"),
            expanded("page_keys", "翻页键", "lrshift"),
        ];
        let msg = conflict_summary(&find_key_conflicts(&groups), "候选键").unwrap();
        assert_eq!(
            msg.matches("次选/三选键").count(),
            1,
            "同一对分组只应出现一次：{msg}"
        );
        assert!(msg.contains("左Shift") && msg.contains("右Shift"));
    }

    #[test]
    fn test_summary_abbreviates_many_keys() {
        let many: Vec<String> = (0..10).map(|d| format!("Ctrl+{d}")).collect();
        let groups = vec![
            KeyGroup::new("keys.pin_candidate", "置顶词条", many.clone()),
            KeyGroup::new("keys.open_settings", "打开设置", many),
        ];
        let msg = conflict_summary(&find_key_conflicts(&groups), "快捷键").unwrap();
        assert!(msg.contains("等 10 个键"), "键太多时应缩写：{msg}");
    }

    // ── 组合键归一与模板展开 ─────────────────────────────────────

    #[test]
    fn test_canonical_hotkey_is_order_and_case_insensitive() {
        // 修饰键顺序/大小写无关，归一后相等 → 可检出冲突。
        assert_eq!(canonical_hotkey("ctrl+shift+e"), "Ctrl+Shift+E");
        assert_eq!(canonical_hotkey("Shift+Ctrl+E"), "Ctrl+Shift+E");
        assert_eq!(canonical_hotkey("ctrl+number"), "Ctrl+数字");
        assert_eq!(canonical_hotkey("ctrl+equal"), "Ctrl+=");
        assert_eq!(canonical_hotkey("shift+space"), "Shift+Space");
        // cmd 与 win/super/meta 同视为 Win 修饰键（与 manifest.rs 解析一致）。
        assert_eq!(canonical_hotkey("cmd+e"), canonical_hotkey("win+e"));
        let a = canonical_hotkey("ctrl+shift+j");
        let b = canonical_hotkey("shift+ctrl+j");
        assert_eq!(a, b);
    }

    #[test]
    fn test_expand_hotkey_unset_values() {
        assert!(expand_hotkey("").is_empty());
        assert!(expand_hotkey("  ").is_empty());
        assert!(expand_hotkey("none").is_empty(), "core 的未设置 sentinel");
        assert!(expand_hotkey("None").is_empty());
    }

    #[test]
    fn test_expand_hotkey_number_template() {
        let ctrl_num = expand_hotkey("ctrl+number");
        assert_eq!(ctrl_num.len(), 10, "模板应覆盖 Ctrl+0..Ctrl+9");
        assert!(ctrl_num.contains(&"Ctrl+0".to_string()));
        assert!(ctrl_num.contains(&"Ctrl+9".to_string()));
        // 修饰位不同的两个模板不该互撞。
        let ctrl_shift_num = expand_hotkey("ctrl+shift+number");
        assert!(ctrl_shift_num.iter().all(|k| !ctrl_num.contains(k)));
        // 普通组合键仍是单 token。
        assert_eq!(expand_hotkey("ctrl+shift+e"), vec!["Ctrl+Shift+E"]);
    }

    /// GUI 的主键列表里有 0-9，故「把某功能配成 Ctrl+1」是点得出来的操作，
    /// 而 Ctrl+数字 模板已占满整段——不展开就检不出，用户只会看到热键没反应。
    #[test]
    fn test_number_template_overlaps_specific_digit() {
        let groups = vec![
            KeyGroup::new(
                "keys.pin_candidate",
                "置顶词条",
                expand_hotkey("ctrl+number"),
            ),
            KeyGroup::new("keys.open_settings", "打开设置", expand_hotkey("ctrl+1")),
        ];
        let hits = find_key_conflicts(&groups);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "Ctrl+1");
        assert_eq!(hits[0].level, ConflictLevel::Blocking);
    }

    #[test]
    fn test_key_symbol_maps_known_and_falls_back() {
        assert_eq!(key_symbol("backtick"), "`");
        assert_eq!(key_symbol("semicolon"), ";");
        assert_eq!(key_symbol("quote"), "'");
        assert_eq!(key_symbol("comma"), ",");
        assert_eq!(key_symbol("period"), ".");
        assert_eq!(key_symbol("slash"), "/");
        assert_eq!(key_symbol("backslash"), "\\");
        assert_eq!(key_symbol("lbracket"), "[");
        assert_eq!(key_symbol("rbracket"), "]");
        assert_eq!(key_symbol("z"), "z", "未知键回退原名");
        // 组合键 token 不在符号表里，原样回退（快捷键域直接用归一形显示）。
        assert_eq!(key_symbol("Ctrl+Shift+E"), "Ctrl+Shift+E");
    }

    #[test]
    fn test_candidate_group_symbols() {
        assert_eq!(key_symbol("comma_period"), ", .");
        assert_eq!(key_symbol("minus_equal"), "- =");
        assert_eq!(key_symbol("brackets"), "[ ]");
        assert_eq!(key_symbol("semicolon_quote"), "; '");
    }
}
