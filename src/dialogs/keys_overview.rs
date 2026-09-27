//! 方案按键总览（**只读**）。
//!
//! 回答一个此前没有任何地方能回答的问题：**这个键现在到底干什么**。一个键的绑定可能来自
//! 全局表、方案表，或 `page_keys` 这类组名的折算，分散在三处编辑入口里；用户要么记不住
//! 自己配过哪些，要么不知道两层撞了之后谁赢。
//!
//! # 为什么只读
//!
//! 集中视图若可写回，就得决定写哪一层——那是第二个真相源，会重蹈 core 仓 `trigger_keys`
//! 五处并存的覆辙。判据（「集中视图写回的是不是同一批字段」）见 core 仓
//! `docs/design/key-resolver-unification.md` §4.3。本表只展示并指路，编辑仍回各自入口。
//!
//! # 数据从哪来
//!
//! 整份由 core 在 `schema.getConfig` 里作为只读旁路字段 `keysOverview` 给出，本模块不做
//! 任何仲裁。设置页读得到两层原始表，却算不出全局那份的折算结果（`minus_equal` 这类组名
//! 的展开规则住在 core 的 `effective_session_actions`）——在这边再写一份展开表就是两处
//! 慢慢漂移，而跨仓契约没有编译期约束。

use serde_json::Value;
use windui::prelude::*;

use super::key_binding::{
    BindingBody, KeyShape, SCHEMA_COMBO_HINT, Verb, command_verb_label, key_display, key_shape,
    session_verb_label,
};

/// 总览的一行。
#[derive(Clone)]
struct Row {
    key: String,
    /// 已翻成中文的动作名；core 给的空动作在 [`KeysOverview::action_label`] 里换成占位符。
    action: String,
    /// 来源层文案（「本方案」/「全局」）。
    from: String,
    /// 是否属会话态那张表。只用于排序（会话态排在后面），不另起分组标题——两张表的键名
    /// 值域不重叠，排在一起已经读得出来，多一层标题只是把列表撑长。
    session: bool,
    /// core 判定这一行**不生效**（`ineffective`）。眼下只有方案层 lead 表的组合键：core 只从
    /// 全局 `keys.key_actions` 编组合键热键表，方案层那条按不出来。
    ///
    /// ★ 这样的行**不遮盖**同名全局条目：同一个键会出两行（全局那条在前、生效；方案那条
    /// 不生效）。本表按 `Vec` 逐行存、稳定排序，不按键去重——按键建索引会把其中一行吞掉，
    /// 而吞掉的若是全局那条，用户看到的就是「这个键不生效」，与事实相反。
    ineffective: bool,
}

/// 不生效行的提示文案。组合键与方案按键编辑器的行提示同文案（同一个常量）；
/// 将来 core 若对别的形态也标 `ineffective`，退回中性的「不生效」，不说错原因。
fn ineffective_hint(key: &str) -> &'static str {
    if key_shape(key) == KeyShape::Combo {
        SCHEMA_COMBO_HINT
    } else {
        "不生效"
    }
}

/// 方案按键总览的工作态。
#[derive(Clone)]
pub struct KeysOverview {
    rows: Signal<Vec<Row>>,
}

// 手写 Default：`Signal` 没有 `Default`（无「默认信号」这一说），与
// `SessionActionFields` 同样处置。
impl Default for KeysOverview {
    fn default() -> Self {
        Self {
            rows: signal(Vec::new()),
        }
    }
}

impl KeysOverview {
    /// 从 `getConfig` 的 `keysOverview` 旁路字段播种。
    ///
    /// `lead_verbs` 是按键功能表的动词中文名（由 `schema_manager` 现算）；会话态那张用
    /// [`session_verb_label`]。
    ///
    /// ★★ **必须传全局那份**（`build_global_key_action_verbs`），不是方案级那份。本表逐行
    /// 标了「本方案 / 全局」，即它**同时展示两层**；而单向 `switch_schema:` 只在全局表里
    /// 合法、方案级那份刻意不列它。传方案级表的表现是「切换到本方案按键」配出来的那条
    /// 在总览里翻不出中文名，显示成原始的 `switch_schema:wubi86`。
    /// **两张表各查各的**：同一个动词字符串在两张表里不会同时出现，混查会把
    /// 「找不到就显示原值」这条兜底变成「显示了另一张表的中文名」。
    pub fn seed(&self, cfg: &Value, lead_verbs: &[Verb]) {
        let mut rows: Vec<Row> = Vec::new();
        let Some(arr) = cfg.get("keysOverview").and_then(|v| v.as_array()) else {
            // 旧版 core 没有这个字段 —— 给空表而不是报错：总览是增量能力，缺了不该
            // 挡住方案设置页本身。
            self.rows.set(Vec::new());
            return;
        };
        for it in arr {
            let key = it.get("key").and_then(|v| v.as_str()).unwrap_or("");
            if key.is_empty() {
                continue;
            }
            let session = it.get("table").and_then(|v| v.as_str()) == Some("session");
            let action = it.get("action").and_then(|v| v.as_str()).unwrap_or("");
            let from_schema = it.get("from").and_then(|v| v.as_str()) == Some("schema");
            // 旧版 core 没有这个字段 → 按生效处理（那时它也没有区分的能力）。
            let ineffective = it
                .get("ineffective")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            rows.push(Row {
                key: key.to_string(),
                action: Self::action_label(action, session, lead_verbs),
                from: if from_schema { "本方案" } else { "全局" }.to_string(),
                session,
                ineffective,
            });
        }
        // 会话态排在后面，各自内部按键名 —— core 那边已按键名有序，这里只做分组。
        // ★ `sort_by` 是稳定排序：同键两行（全局生效 + 方案不生效）保持 core 给的先后。
        rows.sort_by(|a, b| a.session.cmp(&b.session).then_with(|| a.key.cmp(&b.key)));
        self.rows.set(rows);
    }

    /// 动词 → 中文名。查不到就显示原值。
    ///
    /// ★ 兜底是**显示原值**而不是「未知」：手写配置里可能有本版本设置页还不认识的动词，
    /// 显示「未知」会让用户以为配错了，而它在 core 里跑得好好的。
    fn action_label(action: &str, session: bool, lead_verbs: &[Verb]) -> String {
        if action.is_empty() {
            return "—".to_string();
        }
        if session {
            if let Some(label) = session_verb_label(action) {
                return label;
            }
        } else if let Some(label) = command_verb_label(action) {
            // 命令不在功能表里（表达式是用户写的，没法预先列举），单独翻。
            return label;
        } else if let Some(v) = lead_verbs.iter().find(|v| v.value == action) {
            return v.label.clone();
        }
        action.to_string()
    }

    /// 摘要（「未设置」/「N 项」/「N 项，其中 M 项不生效」）。
    ///
    /// 不生效的行也计入 N（它们确实是用户配过的条目），但要单独点出来——摘要是用户
    /// 不点开总览时唯一看得到的东西。
    pub fn summary(&self) -> String {
        let rows = self.rows.get();
        let n = rows.len();
        let dead = rows.iter().filter(|r| r.ineffective).count();
        if n == 0 {
            "未设置".to_string()
        } else if dead > 0 {
            format!("{n} 项，其中 {dead} 项不生效")
        } else {
            format!("{n} 项")
        }
    }

    /// 对话框主体。整表只读，故没有「添加」按钮，底部只有壳自带的「关闭」。
    ///
    /// **不加内边距**：与两个编辑器共用 [`super::key_binding::build_binding_dialog`]，
    /// 由它统一加。
    pub fn build_body(&self) -> BindingBody {
        let list = Element::host_signal(self.rows, move |row: Row| {
            // 不生效的行整行灰显，并在动作后挂提示：它仍要列出来（用户确实配了它），
            // 但不能读起来像一条在工作的绑定。
            let text_role = if row.ineffective {
                Role::TextMuted
            } else {
                Role::Text
            };
            let mut line = Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .padding_xy(0, 3)
                .child(
                    // 键名转义：配置里存的是 `rbracket` / `ctrl+shift+r` 这种内部写法，
                    // 直接摆出来等于让用户读内部词表。与两个编辑器共用同一个显示函数——
                    // 各写一份的结果是同一个键在总览和编辑页显示成两个名字。
                    Element::label(key_display(&row.key))
                        .font_size(12.5)
                        .fg_role(text_role)
                        .width(150)
                        .max_lines(1),
                )
                .child(
                    Element::label(&row.action)
                        .font_size(12.5)
                        .fg_role(text_role)
                        .weight(1.0)
                        .max_lines(1),
                );
            if row.ineffective {
                line = line.child(
                    Element::label(ineffective_hint(&row.key))
                        .font_size(11.0)
                        .fg_role(Role::Warning)
                        .max_lines(1),
                );
            }
            line.child(
                Element::label(&row.from)
                    .font_size(12.0)
                    .fg_role(Role::TextMuted)
                    .width(56)
                    .max_lines(1),
            )
        });
        let hint = Element::label(
            "本页只展示不修改。编辑请回「本方案-按键自定义」「本方案-输入中按键自定义」，或全局按键设置。",
        )
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .max_lines(2);
        BindingBody::new(
            Element::col()
                .width_match()
                .spacing(6)
                .child(hint)
                .child(list),
            Vec::new(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lead_verbs() -> Vec<Verb> {
        vec![Verb {
            value: "temp_pinyin".to_string(),
            label: "临时拼音".to_string(),
        }]
    }

    /// 两张表各查各的动词名：同一个字符串不该跨表命中。
    #[test]
    fn labels_are_looked_up_per_table() {
        let o = KeysOverview::default();
        o.seed(
            &json!({ "keysOverview": [
                { "key": "z", "table": "lead", "action": "temp_pinyin", "from": "global" },
                { "key": "minus", "table": "session", "action": "page_prev", "from": "schema" },
            ]}),
            &lead_verbs(),
        );
        let rows = o.rows.get();
        assert_eq!(rows.len(), 2);
        let lead = rows.iter().find(|r| r.key == "z").unwrap();
        assert_eq!(lead.action, "临时拼音");
        assert_eq!(lead.from, "全局");
        let sess = rows.iter().find(|r| r.key == "minus").unwrap();
        assert_eq!(sess.action, "上一页");
        assert_eq!(sess.from, "本方案");
    }

    /// 认不出的动词显示**原值**，不是「未知」——它在 core 里可能跑得好好的。
    #[test]
    fn unknown_verb_falls_back_to_raw_value() {
        let o = KeysOverview::default();
        o.seed(
            &json!({ "keysOverview": [
                { "key": "tab", "table": "session", "action": "some_future_verb", "from": "global" },
            ]}),
            &[],
        );
        assert_eq!(o.rows.get()[0].action, "some_future_verb");
    }

    /// 总览要认得出 core 值域内的**全部**序号档，**包括选择器不给配的那些**。
    ///
    /// 此前只认 2、3，第 7 候选显示成原值。现在还多一层：以词定字的选择器只给到第 4 字
    /// （第 5 字以上按下去恒定走 overflow，见 `key_binding::session_verb_categories`），
    /// 但 core 认 1..=9、用户手写的 `select_char:7` 照样执行——总览不认就会落到「显示原
    /// 值」兜底上，出现一行 `select_char:7`。
    ///
    /// ★ 写入侧收窄时最容易顺手把读取侧一起收了（两者共用过同一张表），故**两个前缀**都
    /// 钉在这里，其中 `select_char:7` 正是「选择器给不出、总览必须显示得出」的那一档。
    #[test]
    fn ordinal_verbs_are_named() {
        let o = KeysOverview::default();
        o.seed(
            &json!({ "keysOverview": [
                { "key": "tab", "table": "session", "action": "select_candidate:7", "from": "global" },
                { "key": "comma", "table": "session", "action": "select_char:7", "from": "global" },
            ]}),
            &[],
        );
        // 按内容查而不是按下标：`seed` 末尾对行排过序（先 session 后 key），下标断言会
        // 随「换个键名」这种无关改动而红。
        let labels: Vec<String> = o.rows.get().iter().map(|r| r.action.clone()).collect();
        assert!(
            labels.contains(&"选中第 7 个候选".to_string()),
            "{labels:?}"
        );
        assert!(
            labels.contains(&"以词定字：取第 7 字".to_string()),
            "{labels:?}"
        );
    }

    /// ★ 全局层独有的动词（单向 `switch_schema:`）必须翻得出中文名。
    ///
    /// 它是「切换到本方案按键」的落点，几乎每个配过方案直达键的用户都有一条。用方案级
    /// 动词表来查（那份刻意不含单向切换）就会落到「显示原值」兜底上，总览里出现一行
    /// `switch_schema:wubi86`——曾经就是这么显示的。
    #[test]
    fn global_only_verbs_are_named() {
        let o = KeysOverview::default();
        o.seed(
            &json!({ "keysOverview": [
                { "key": "ctrl+shift+r", "table": "lead", "action": "switch_schema:wubi86", "from": "global" },
            ]}),
            &[Verb {
                value: "switch_schema:wubi86".to_string(),
                label: "切到方案（单向） - 五笔".to_string(),
            }],
        );
        assert_eq!(o.rows.get()[0].action, "切到方案（单向） - 五笔");
    }

    /// 命令直通两张表都能绑，都要翻成「运行命令：<表达式>」——它不在任何功能表里
    /// （表达式是用户写的），只查表的话会落到「显示原值」兜底上。
    /// `commit_highlighted` 是会话态新动词，同样要有中文名。
    #[test]
    fn command_and_commit_highlighted_are_named() {
        let o = KeysOverview::default();
        o.seed(
            &json!({ "keysOverview": [
                { "key": "ctrl+alt+c", "table": "lead", "action": "command:proc.run(\"charmap.exe\")", "from": "global" },
                { "key": "tab", "table": "session", "action": "command:web.search(\"x\")", "from": "schema" },
                { "key": "semicolon", "table": "session", "action": "commit_highlighted", "from": "global" },
            ]}),
            &lead_verbs(),
        );
        let label = |k: &str| {
            o.rows
                .get()
                .iter()
                .find(|r| r.key == k)
                .map(|r| r.action.clone())
                .unwrap_or_default()
        };
        assert_eq!(label("ctrl+alt+c"), "运行命令：proc.run(\"charmap.exe\")");
        assert_eq!(label("tab"), "运行命令：web.search(\"x\")");
        assert_eq!(label("semicolon"), "上屏当前高亮候选（同空格）");
    }

    /// ★★ 同键两行（全局生效 + 方案层组合键不生效）都要留下，且保持 core 给的先后：
    /// 按键去重 / 建索引会吞掉其中一行——吞掉全局那条的话，用户看到的是「这个键不生效」，
    /// 与事实相反。
    #[test]
    fn same_key_effective_and_ineffective_rows_both_survive() {
        let o = KeysOverview::default();
        o.seed(
            &json!({ "keysOverview": [
                { "key": "ctrl+shift+r", "table": "lead", "action": "temp_pinyin", "from": "global", "ineffective": false },
                { "key": "ctrl+shift+r", "table": "lead", "action": "command:open(\"x\")", "from": "schema", "ineffective": true },
                { "key": "ctrl+alt+q", "table": "lead", "action": "temp_pinyin", "from": "schema", "ineffective": true },
            ]}),
            &lead_verbs(),
        );
        let rows = o.rows.get();
        let same: Vec<&Row> = rows.iter().filter(|r| r.key == "ctrl+shift+r").collect();
        assert_eq!(same.len(), 2, "同键两行都要在");
        assert_eq!(
            (same[0].from.as_str(), same[0].ineffective),
            ("全局", false),
            "全局生效那条在前"
        );
        assert_eq!(
            (same[1].from.as_str(), same[1].ineffective),
            ("本方案", true)
        );
        assert_eq!(same[1].action, "运行命令：open(\"x\")");
        let only = rows
            .iter()
            .find(|r| r.key == "ctrl+alt+q")
            .expect("仅方案层那条");
        assert!(only.ineffective);
        assert_eq!(ineffective_hint(&only.key), SCHEMA_COMBO_HINT);
        assert_eq!(o.summary(), "3 项，其中 2 项不生效");
    }

    /// 旧版 core 不给 `ineffective` 字段：一律按生效处理，摘要不出「不生效」。
    #[test]
    fn missing_ineffective_field_means_effective() {
        let o = KeysOverview::default();
        o.seed(
            &json!({ "keysOverview": [
                { "key": "ctrl+shift+r", "table": "lead", "action": "temp_pinyin", "from": "schema" },
            ]}),
            &lead_verbs(),
        );
        assert!(!o.rows.get()[0].ineffective);
        assert_eq!(o.summary(), "1 项");
    }

    /// 不生效提示：组合键与方案按键编辑器同文案；别的形态（core 将来可能标）说中性的
    /// 「不生效」，不把原因说错。
    #[test]
    fn ineffective_hint_names_the_reason_only_for_combos() {
        assert_eq!(ineffective_hint("ctrl+shift+r"), SCHEMA_COMBO_HINT);
        assert_eq!(ineffective_hint("rshift"), "不生效");
    }

    /// 键名要转义成人能读的形式，而不是配置里的内部写法。
    #[test]
    fn key_names_are_escaped_for_display() {
        let o = KeysOverview::default();
        o.seed(
            &json!({ "keysOverview": [
                { "key": "rbracket", "table": "session", "action": "page_next", "from": "global" },
            ]}),
            &[],
        );
        assert_eq!(key_display(&o.rows.get()[0].key), "]");
    }

    /// 显式 none（core 给空动作）显示成占位符而不是空白行。
    #[test]
    fn disabled_key_shows_placeholder() {
        let o = KeysOverview::default();
        o.seed(
            &json!({ "keysOverview": [
                { "key": "minus", "table": "session", "action": "none", "from": "schema" },
            ]}),
            &[],
        );
        assert_eq!(o.rows.get()[0].action, "输入中禁用该键");
    }

    /// 旧版 core 没有该字段时给空表，不 panic、不挡住设置页。
    #[test]
    fn missing_field_yields_empty_table() {
        let o = KeysOverview::default();
        o.seed(&json!({}), &[]);
        assert!(o.rows.get().is_empty());
        assert_eq!(o.summary(), "未设置");
    }
}
