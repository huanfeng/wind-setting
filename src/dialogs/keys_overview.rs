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

use super::schema_key_actions::Verb;
use super::session_actions::SessionActionFields;

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
    /// `lead_verbs` 是引导键那张表的动词中文名（由 `schema_manager` 现算，与「按键功能」
    /// 对话框同一份）；会话态那张用 [`SessionActionFields::verb_options`]。
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
            rows.push(Row {
                key: key.to_string(),
                action: Self::action_label(action, session, lead_verbs),
                from: if from_schema { "本方案" } else { "全局" }.to_string(),
                session,
            });
        }
        // 会话态排在后面，各自内部按键名 —— core 那边已按键名有序，这里只做分组。
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
            if let Some((_, label)) = SessionActionFields::verb_options()
                .iter()
                .find(|(v, _)| *v == action)
            {
                return label.to_string();
            }
        } else if let Some(v) = lead_verbs.iter().find(|v| v.value == action) {
            return v.label.clone();
        }
        action.to_string()
    }

    /// 摘要（「未设置」/「N 项」）。
    pub fn summary(&self) -> String {
        let n = self.rows.get().len();
        if n == 0 {
            "未设置".to_string()
        } else {
            format!("{n} 项")
        }
    }

    /// 对话框主体。整表只读，故没有「添加」按钮，底部只有壳自带的「关闭」。
    pub fn build_body(&self) -> (Element, Vec<Element>) {
        let list = Element::host_signal(self.rows, move |row: Row| {
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .padding_xy(0, 3)
                .child(
                    Element::label(&row.key)
                        .font_size(12.5)
                        .fg_role(Role::Text)
                        .width(150)
                        .max_lines(1),
                )
                .child(
                    Element::label(&row.action)
                        .font_size(12.5)
                        .fg_role(Role::Text)
                        .weight(1.0)
                        .max_lines(1),
                )
                .child(
                    Element::label(&row.from)
                        .font_size(12.0)
                        .fg_role(Role::TextMuted)
                        .width(56)
                        .max_lines(1),
                )
        });
        let hint = Element::label(
            "只读。编辑请回「本方案的按键绑定」「本方案输入中的按键」，或全局按键设置。",
        )
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .max_lines(2);
        (
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
                { "key": "tab", "table": "session", "action": "select_candidate:7", "from": "global" },
            ]}),
            &[],
        );
        assert_eq!(o.rows.get()[0].action, "select_candidate:7");
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
