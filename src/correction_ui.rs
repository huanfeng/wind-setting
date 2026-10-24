//! 输入页「纠错与提示」一节里手写的三行（其余各项走清单的通用控件）：
//!
//! - 「拼写纠错」：总开关 + 扩展设置。对话框里 8 条内置规则排成紧凑网格（仿模糊音的
//!   对话框，见 `manifest::build_fuzzy_row`），下面是自定义规则列表（增删）。
//! - 「提示来源」：三个来源勾选挤在一行，三个配置键各自独立。
//! - 「误读表」：打开用户数据目录里的 `pinyin_misread.txt`（不存在就先建一份带说明的空表）。
//!
//! 自定义规则的冲突示例来自 core 的 `correction.conflictExample`（判据与引擎同源，本仓不
//! 依赖 wind-engine）；core 没在运行时取不到，就不显示。

use serde_json::{Value, json};
use windui::prelude::*;

use crate::config::Config;
use crate::state::AppState;

/// 内置规则：(配置键, 网格里的标签, 冲突示例)。文案照设计 §5.2「设置端文案」「冲突示例」两列。
pub(crate) const RULES: [(&str, &str, Option<&str>); 8] = [
    (
        "input.correction.rules.gn_ng",
        "ng 打成 gn（ogn→ong）",
        Some("简拼下 xign 也可是 xi+g+n"),
    ),
    (
        "input.correction.rules.on_ong",
        "漏 g：on→ong",
        Some("lone 也可是 lo+ne"),
    ),
    ("input.correction.rules.vowel_swap", "韵母字母颠倒", None),
    (
        "input.correction.rules.legacy_spelling",
        "旧式拼写 uei/iou/uen",
        Some("liou 也可是 li+ou"),
    ),
    (
        "input.correction.rules.h_misplaced",
        "h 错位（hz→zh、zih→zhi）",
        Some("h+zi 是简拼"),
    ),
    (
        "input.correction.rules.mg_ng",
        "ng 打成 mg",
        Some("m 是简拼首字母"),
    ),
    (
        "input.correction.rules.n_before_g",
        "ng 打成 nag 类",
        Some("nage 也可是 na+ge"),
    ),
    (
        "input.correction.rules.ai_ia",
        "简拼冲突类",
        Some("x'ai 喜爱 会被纠成 xia"),
    ),
];

/// 自定义规则的配置键。
pub(crate) const CUSTOM_KEY: &str = "input.correction.custom";

/// 提示来源：(配置键, 行内勾选的标签)。
pub(crate) const HINT_SOURCES: [(&str, &str); 3] = [
    ("input.correction.hint.fuzzy", "模糊音"),
    ("input.correction.hint.misread", "误读"),
    ("input.correction.hint.correction", "拼写纠错"),
];

/// 用户误读表的文件名（相对用户数据目录，与 core `MISREAD_FILE` 一致）。
const MISREAD_FILE: &str = "pinyin_misread.txt";

/// 新建用户误读表时写进去的说明。
const MISREAD_TEMPLATE: &str = "\
# 拼音误读表（用户追加）
#
# 格式：文字<Tab>错误拼音<Tab>正确拼音，拼音按音节以空格分隔、不带声调，ü 写作 v。
# 例：馄饨<Tab>hun dun<Tab>hun tun
# 第三列留空（行尾仍保留 Tab）= 这个读法不算错，不提示也不召回。
# 同一「文字 + 错误拼音」覆盖内置表。以 # 开头的行是注释。
# 改完后，下次保存任一设置或重启输入法时生效。
";

/// 规则网格每格的说明：有冲突写冲突，没有写「无已知冲突」。
pub(crate) fn rule_note(conflict: Option<&str>) -> String {
    match conflict {
        Some(c) => format!("冲突：{c}"),
        None => "无已知冲突".to_string(),
    }
}

/// 「拼写纠错」入口行的说明：开着时写出启用了几条规则，不点开也知道现在的状态。
pub(crate) fn correction_summary(enabled: bool, rules_on: usize, custom: usize) -> String {
    if !enabled {
        return "未开启。开启后，打错的拼音按纠正后的拼写也出候选".to_string();
    }
    let mut s = if rules_on == 0 {
        "已开启，但还没有勾选内置规则".to_string()
    } else {
        format!("已开启 {rules_on} 条内置规则")
    };
    if custom > 0 {
        s.push_str(&format!("，自定义 {custom} 条"));
    }
    s
}

/// 自定义规则列表里的一行。
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct CustomRule {
    pub rule: String,
    /// 冲突示例或无效原因；空 = 无冲突（或 core 未运行、查不到）。
    pub note: String,
}

/// 冲突查询的超时：纯计算的轻查询，core 不在时别让添加按钮卡住。
const CONFLICT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(300);

/// 问 core 这条规则的冲突示例。取不到（core 未运行）时 `None`：不显示示例，不阻止保存。
fn query_conflict(rule: &str) -> Option<Value> {
    crate::rpc::call_core_within(
        "correction.conflictExample",
        &json!({ "rule": rule }),
        CONFLICT_TIMEOUT,
    )
    .ok()
}

/// 行内说明：冲突示例或无效原因；取不到 core 时空串。
fn conflict_note(rule: &str) -> String {
    query_conflict(rule)
        .map(|v| note_from_reply(&v))
        .unwrap_or_default()
}

/// 本地语法检查，与 core 的 `parse_custom` 同口径：先去空白、转小写，单个 `=`，
/// 两侧为非空小写字母且不相同。返回规整后的 `错误=正确`。
pub(crate) fn normalize_rule(raw: &str) -> Result<String, String> {
    let rule: String = raw.split_whitespace().collect::<String>().to_lowercase();
    let mut parts = rule.split('=');
    let (Some(wrong), Some(correct), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err("写成「错误=正确」，只能有一个 =".to_string());
    };
    let letters = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase());
    if !letters(wrong) || !letters(correct) {
        return Err("两侧须为非空的拼音字母".to_string());
    }
    if wrong == correct {
        return Err("两侧相同".to_string());
    }
    Ok(rule)
}

/// `correction.conflictExample` 的回包 → 行内说明。
pub(crate) fn note_from_reply(v: &Value) -> String {
    if v.get("valid").and_then(Value::as_bool) == Some(false) {
        let why = v
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("格式不对");
        return format!("无效：{why}");
    }
    match v.get("example").and_then(Value::as_str) {
        Some(ex) => format!("冲突：{ex}"),
        None => String::new(),
    }
}

/// 自定义规则列表的状态：行、输入框草稿、添加被拒的原因。
#[derive(Clone)]
pub(crate) struct CustomRulesUi {
    pub rows: Signal<Vec<CustomRule>>,
    pub draft: Signal<String>,
    pub error: Signal<String>,
}

impl CustomRulesUi {
    pub fn new() -> Self {
        Self {
            rows: signal(Vec::new()),
            draft: signal(String::new()),
            error: signal(String::new()),
        }
    }

    /// 按配置值重建列表（每条现查一次冲突示例）。
    pub fn seed(&self, values: &[Value]) {
        let rows = values
            .iter()
            .filter_map(Value::as_str)
            .map(|r| CustomRule {
                rule: r.to_string(),
                note: conflict_note(r),
            })
            .collect();
        self.rows.set(rows);
    }

    /// 把草稿加进列表。空的、重复的不加；格式不对或 core 判无效的不加，原因写进 `error`、
    /// 草稿留着供修改。返回是否加了。
    pub fn add_draft(&self) -> bool {
        let draft = self.draft.get();
        if draft.trim().is_empty() {
            return false;
        }
        let rule = match normalize_rule(&draft) {
            Ok(r) => r,
            Err(why) => {
                self.error.set(format!("无效：{why}"));
                return false;
            }
        };
        if self.rows.get().iter().any(|r| r.rule == rule) {
            self.error.set("已有这条规则".to_string());
            return false;
        }
        let note = match query_conflict(&rule) {
            Some(v) if v.get("valid").and_then(Value::as_bool) == Some(false) => {
                self.error.set(note_from_reply(&v));
                return false;
            }
            Some(v) => note_from_reply(&v),
            None => String::new(),
        };
        let mut rows = self.rows.get();
        rows.push(CustomRule { rule, note });
        self.rows.set(rows);
        self.draft.set(String::new());
        self.error.set(String::new());
        true
    }

    pub fn remove(&self, rule: &str) {
        let mut rows = self.rows.get();
        rows.retain(|r| r.rule != rule);
        self.rows.set(rows);
    }

    pub fn to_value(&self) -> Value {
        Value::Array(
            self.rows
                .get()
                .into_iter()
                .map(|r| Value::String(r.rule))
                .collect(),
        )
    }
}

/// 登记自定义规则的写回 / 取值 / 恢复器（同 `comment_dicts` 的四件套）。
fn register_custom(state: &AppState, ui: &CustomRulesUi, cfg: &Config) {
    let values = |c: &Config| c.get_array(CUSTOM_KEY).cloned().unwrap_or_default();
    ui.seed(&values(cfg));
    let ui_w = ui.clone();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            c.set_path(CUSTOM_KEY, ui_w.to_value())
        }));
    let ui_g = ui.clone();
    state
        .fields
        .getters
        .borrow_mut()
        .insert(CUSTOM_KEY.to_string(), Box::new(move || ui_g.to_value()));
    let ui_l = ui.clone();
    state.fields.loaders.borrow_mut().insert(
        CUSTOM_KEY.to_string(),
        Box::new(move |c: &Config| ui_l.seed(&values(c))),
    );
}

/// 自定义规则那一块：说明、列表（每条带冲突示例与删除）、输入框 + 添加。
fn custom_rules_block(ui: &CustomRulesUi) -> Element {
    let muted = |s: &str| {
        Element::label(s)
            .font_size(11.5)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(2)
    };
    let ui_rm = ui.clone();
    let list = Element::host_signal(ui.rows, move |r: CustomRule| {
        let ui_del = ui_rm.clone();
        let rule = r.rule.clone();
        let text = Element::col()
            .weight(1.0)
            .spacing(2)
            .child(Element::label(&r.rule).font_size(12.5).fg_role(Role::Text));
        let text = if r.note.is_empty() {
            text
        } else {
            text.child(
                Element::label(&r.note)
                    .font_size(11.5)
                    .fg_role(Role::TextMuted)
                    .max_lines(2),
            )
        };
        Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(8)
            .padding_xy(0, 2)
            .child(text)
            .child(
                Element::button("删除")
                    .small()
                    .outline_soft()
                    .on_click(move |_| ui_del.remove(&rule)),
            )
    });
    let ui_add = ui.clone();
    let ui_sub = ui.clone();
    let input_row = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::text_input(ui.draft, "错误=正确，如 uen=un")
                .weight(1.0)
                .on_submit(move |_| {
                    ui_sub.add_draft();
                }),
        )
        .child(Element::button("添加").small().on_click(move |_| {
            ui_add.add_draft();
        }));
    Element::col()
        .width_match()
        .spacing(6)
        .child(crate::manifest::dialog_group_header("自定义规则"))
        .child(muted(
            "作用于单个音节内的片段替换；能切成合法音节或简拼的写法会列出冲突示例，仍可保存",
        ))
        .child(list)
        .child(input_row)
        .child({
            let err = ui.error;
            Element::label_signal(err)
                .font_size(11.5)
                .fg_role(Role::Danger)
                .width_match()
                .max_lines(2)
                .visible_when(move || !err.get().is_empty())
        })
}

/// 「拼写纠错」入口行：「拼写纠错 …… [开关][扩展设置]」，规则网格与自定义规则在对话框里。
pub(crate) fn build_correction_row(state: &AppState, cfg: &Config) -> Element {
    use crate::dialogs::field_dialogs::build_group_dialog;
    use crate::manifest::{host_off_notice, register_dialog, register_toggle};
    use crate::search::index::{CORRECTION_MASTER, in_dialog_anchor};

    let en = register_toggle(state, CORRECTION_MASTER, cfg);
    let sigs: Vec<Signal<bool>> = RULES
        .iter()
        .map(|(k, ..)| register_toggle(state, k, cfg))
        .collect();
    let cells: Vec<Element> = RULES
        .iter()
        .zip(sigs.iter())
        .map(|((_, lbl, conflict), s)| {
            Element::col()
                .width_match()
                .spacing(1)
                .child(Element::checkbox(*lbl, *s).small().fg_role(Role::Text))
                .child(
                    Element::label(rule_note(*conflict))
                        .font_size(11.0)
                        .fg_role(Role::TextMuted)
                        .padding_xy(22, 0)
                        .max_lines(2),
                )
        })
        .collect();
    let grid = crate::search::anchor::wrap(
        Element::grid(2, 10, cells).width_match(),
        in_dialog_anchor(RULES[0].0),
        state.jump,
    );

    let custom = CustomRulesUi::new();
    register_custom(state, &custom, cfg);
    let custom_block = crate::search::anchor::wrap(
        custom_rules_block(&custom),
        in_dialog_anchor(CUSTOM_KEY),
        state.jump,
    );

    // 入口行说明随开关、规则勾选与自定义条数实时变。
    let summary = signal(String::new());
    let version_sigs = sigs.clone();
    let sum_sigs = sigs.clone();
    let rows = custom.rows;
    let sync_effect = crate::pages::dict::reactive::reactive_effect(
        move || {
            version_sigs
                .iter()
                .fold(en.version().wrapping_add(rows.version()), |acc, s| {
                    acc.wrapping_add(s.version())
                })
        },
        move || {
            let on = sum_sigs.iter().filter(|s| s.get()).count();
            let text = correction_summary(en.get(), on, rows.get().len());
            if summary.get() != text {
                summary.set(text);
            }
        },
    );

    let body = Element::col()
        .width_match()
        .padding_xy(0, 8)
        .spacing(10)
        .child(host_off_notice("拼写纠错").visible_when(move || !en.get()))
        .child(crate::manifest::dialog_group_header("内置规则"))
        .child(grid)
        .child(custom_block);
    let visible = signal(false);
    state
        .fields
        .dialog_opens
        .borrow_mut()
        .insert(CORRECTION_MASTER.to_string(), visible);
    register_dialog(
        state,
        build_group_dialog(visible, "拼写纠错 · 扩展设置", vec![body], None),
    );

    let control = Element::row()
        .cross(Align::Center)
        .spacing(10)
        .child(Element::switch(en))
        .child(
            Element::button("扩展设置")
                .small()
                .outline_soft()
                .on_click(move |_| visible.set(true)),
        );
    Element::col()
        .width_match()
        .child(crate::widgets::setting_row_desc_signal(
            "拼写纠错",
            summary,
            control,
        ))
        .child(sync_effect)
}

pub(crate) fn is_hint_source(key: &str) -> bool {
    HINT_SOURCES.iter().any(|(k, _)| *k == key)
}

/// 「提示来源」那一行挂在哪个键上：第一个没被藏的来源（定制版可能藏掉其中几个）。
pub(crate) fn first_visible_hint_source(m: &crate::manifest::Manifest) -> Option<&'static str> {
    HINT_SOURCES
        .iter()
        .map(|(k, _)| *k)
        .find(|k| m.is_visible(k))
}

/// 「提示来源」一行：没被藏的来源各一个勾选，各写各的配置键。
pub(crate) fn build_hint_sources_row(state: &AppState, cfg: &Config) -> Element {
    let cells: Vec<Element> = HINT_SOURCES
        .iter()
        .filter(|(k, _)| state.manifest.is_visible(k))
        .map(|(k, lbl)| {
            let s = crate::manifest::register_toggle(state, k, cfg);
            Element::checkbox(*lbl, s).small().fg_role(Role::Text)
        })
        .collect();
    crate::widgets::setting_row_checks(
        "提示来源",
        Some("哪些候选带正确拼音；同时作用于注释变量 ${correct_pinyin}，勾选误读才会带出读错的词"),
        Element::grid(3, 6, cells),
    )
}

/// 「误读表」行的控件：打开用户误读表（没有就先建一份带说明的）。
pub(crate) fn misread_table_button(state: &AppState) -> Element {
    let feedback = state.feedback;
    Element::button("打开文件")
        .small()
        .outline_soft()
        .on_click(move |_| {
            if let Err(e) = open_misread_table() {
                feedback.set(Some((ToastKind::Error, e)));
            }
        })
}

/// 经 core 定位用户数据目录里的误读表（引擎读的就是这一份）。
fn locate_misread_table() -> Result<std::path::PathBuf, String> {
    let v = crate::rpc::call_core("data.locate", &json!({ "path": MISREAD_FILE }))
        .map_err(|e| format!("找不到用户数据目录：{e}"))?;
    v.get("path")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "找不到用户数据目录".to_string())
}

/// 定位误读表，不存在就写入说明模板，再交给系统打开。
fn open_misread_table() -> Result<(), String> {
    let path = locate_misread_table()?;
    let path = path.as_path();
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("建不了目录：{e}"))?;
        }
        std::fs::write(path, MISREAD_TEMPLATE).map_err(|e| format!("建不了误读表：{e}"))?;
    }
    crate::open_external::open_with_default_app(path)
        .map_err(|e| format!("没能打开文件（{e}），文件在 {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内置规则表与清单「纠错与提示」的规则项逐项一致：键、标签、次序，hint 即冲突说明。
    #[test]
    fn rules_match_manifest() {
        let m = crate::manifest::Manifest::from_embedded();
        let want: Vec<(String, String, Option<String>)> = m
            .order
            .iter()
            .filter(|k| k.starts_with("input.correction.rules."))
            .map(|k| {
                let it = m.get(k).unwrap();
                (k.clone(), it.label.clone(), it.hint.clone())
            })
            .collect();
        let got: Vec<(String, String, Option<String>)> = RULES
            .iter()
            .map(|(k, l, c)| (k.to_string(), l.to_string(), Some(rule_note(*c))))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn hint_sources_are_manifest_items() {
        let m = crate::manifest::Manifest::from_embedded();
        for (k, _) in HINT_SOURCES {
            let it = m.get(k).unwrap_or_else(|| panic!("清单缺 {k}"));
            assert_eq!(it.section, "纠错与提示", "{k}");
        }
        let row = m.get("local.misread_table").expect("清单缺误读表那一行");
        assert_eq!(row.typ, "open_misread_table");
    }

    #[test]
    fn summary_reports_state() {
        assert!(correction_summary(false, 3, 1).starts_with("未开启"));
        assert_eq!(correction_summary(true, 2, 0), "已开启 2 条内置规则");
        assert_eq!(
            correction_summary(true, 2, 1),
            "已开启 2 条内置规则，自定义 1 条"
        );
        assert!(correction_summary(true, 0, 0).contains("没有勾选"));
    }

    #[test]
    fn note_from_core_reply() {
        assert_eq!(
            note_from_reply(&json!({ "valid": true, "example": "tuen = tu+en" })),
            "冲突：tuen = tu+en"
        );
        assert_eq!(
            note_from_reply(&json!({ "valid": true, "example": null })),
            ""
        );
        assert_eq!(
            note_from_reply(&json!({ "valid": false, "reason": "缺少 `=`" })),
            "无效：缺少 `=`"
        );
    }

    /// 误读表经 `data.locate {path}` 定位到数据目录根下的 `pinyin_misread.txt`（core 的
    /// `classify_text_path` 认这个文件名；mock 照 core 拒收不认识的根下文件）。
    #[test]
    fn misread_table_is_located_in_data_dir() {
        with_mock(|| {
            crate::rpc::take_mock_calls();
            let p = locate_misread_table().expect("core 应认误读表");
            assert_eq!(p, std::path::Path::new("/mock/data/pinyin_misread.txt"));
            assert_eq!(
                crate::rpc::take_mock_calls(),
                vec![(
                    "data.locate".to_string(),
                    json!({ "path": "pinyin_misread.txt" })
                )]
            );
            assert!(
                crate::rpc::call_core("data.locate", &json!({ "path": "misread.txt" })).is_err(),
                "根下不认识的文件 core 会拒"
            );
        });
    }

    fn with_mock<R>(f: impl FnOnce() -> R) -> R {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let r = f();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        r
    }

    /// 本地语法检查与 core 的 `parse_custom` 同口径：单个 `=`、两侧非空小写字母且不同；先转小写。
    #[test]
    fn custom_rule_syntax_checked_locally() {
        assert_eq!(normalize_rule(" UEN = Un "), Ok("uen=un".to_string()));
        for bad in ["ogn", "=ong", "ogn=", "a=b=c", "o1=x", "x=x", "ü=v"] {
            assert!(normalize_rule(bad).is_err(), "{bad} 应判无效");
        }
    }

    /// 本地格式不对、或 core 判无效时都不收，并把原因留给界面显示。
    #[test]
    fn invalid_custom_rule_is_rejected() {
        with_mock(|| {
            let ui = CustomRulesUi::new();
            ui.draft.set("a=b=c".into());
            assert!(!ui.add_draft());
            assert!(!ui.error.get().is_empty());
            crate::rpc::force_mock_reply(
                "correction.conflictExample",
                Some(json!({ "valid": false, "reason": "生成不出可用的错误拼写" })),
            );
            ui.draft.set("dui=diu".into());
            let added = ui.add_draft();
            crate::rpc::force_mock_reply("correction.conflictExample", None);
            assert!(!added);
            assert_eq!(ui.error.get(), "无效：生成不出可用的错误拼写");
            assert!(ui.rows.get().is_empty());
            assert_eq!(ui.draft.get(), "dui=diu", "被拒时保留草稿供修改");
        });
    }

    /// 增删与写回：去空白、去重，冲突示例取自 core（mock 下 uen=un 有冲突）。
    #[test]
    fn custom_rules_add_remove_and_write_back() {
        with_mock(|| {
            let ui = CustomRulesUi::new();
            ui.seed(&[json!("ogn=ong")]);
            ui.draft.set(" UEN = un ".into());
            assert!(ui.add_draft());
            assert_eq!(ui.draft.get(), "");
            ui.draft.set("ogn=ong".into());
            assert!(!ui.add_draft(), "重复的不加");
            ui.draft.set("  ".into());
            assert!(!ui.add_draft(), "空的不加");
            let rows = ui.rows.get();
            assert_eq!(rows[1].rule, "uen=un");
            assert_eq!(rows[1].note, "冲突：tuen = tu+en");
            ui.remove("ogn=ong");
            assert_eq!(ui.to_value(), json!(["uen=un"]));
        });
    }
}
