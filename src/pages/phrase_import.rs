//! 短语逐行分发格式（`wind:p1`）的导入预览与应用。
//!
//! 与配置片段**共用同一个导入对话框**——「入口多通路一条」：用户从群里复制一段文本
//! 粘进来，不该先被问「这是配置还是短语」。`ConfigImportState::validate` 按内容分派，
//! 本模块只负责短语那一支的解析、渲染与应用。
//!
//! ## 命令短语不设门槛
//!
//! 短语能调用 `proc.run` / `key.type` 这类函数——但那本来就是短语的主要用途，而且短语
//! **不会自行执行**：要触发得先打出它的编码、再从候选里选中。把常态当异常来设确认门槛，
//! 只会让每次导入都多两步无意义的点击。所以预览是一个平铺列表，逐条给出内容与落点，
//! 不分组、不勾选、不标红。
//!
//! 仍然拦的只有两样，都与「危不危险」无关：语法错的条目不导入（装进去只会在触发时
//! 失败，失败点离导入很远），路径里反斜杠写少了给一句提示（见 `wind-cmdbar::lint`）。

use serde_json::{Value, json};
use windui::prelude::*;

use crate::rpc;

/// 短语分发格式的标记前缀（**跨仓契约**，主仓 `wind-store/src/phrase_text.rs` 的
/// `MARKER_PREFIX` 是权威定义）。
///
/// 客户端做前缀判定只为省两次 RPC 往返，**真正的校验恒在 core**：判错了最多是把
/// 短语文本送进配置片段管线，由那边如实报错，不会误装任何东西。
const MARKER_PREFIX: &str = "wind:p";

/// 这段文本是否是短语分发格式。首个非空行的前缀即判据。
pub fn is_phrase_text(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .is_some_and(|l| l.starts_with(MARKER_PREFIX))
}

/// 预览列表的一行。
#[derive(Clone)]
pub struct PhraseRow {
    pub code: String,
    pub text: String,
    /// 落点：新增 / 已存在跳过 / 会遮蔽系统短语；坏行则是错误原因。
    pub note: String,
    /// 疑似笔误的提示（路径里的反斜杠写少了）。空 = 无。
    ///
    /// 与 [`Self::note`] 分开渲染而不是拼进去：它说的是「这条大概不是你想写的」，
    /// 与「会遮蔽系统短语」这类落点信息不是一回事。
    pub hint: String,
    /// 语法错误的条目 core 一律不装，这里灰掉并注明。
    pub broken: bool,
}

/// 一次预览的完整结果。
#[derive(Clone, Default)]
pub struct PhrasePreview {
    pub title: String,
    pub rows: Vec<PhraseRow>,
    /// 解析跳过的行（缺分隔、编码非法…），只回显不参与导入。
    pub problems: Vec<String>,
}

impl PhrasePreview {
    /// 有没有任何可导入的条目。全是坏行时「应用」该禁用。
    pub fn has_importable(&self) -> bool {
        self.rows.iter().any(|r| !r.broken)
    }
}

/// `phrase.previewImportText` 响应 → 预览结果。
///
/// **保持 core 给的顺序**（即源文件行序）：用户在聊天窗口里读到的是哪一行在前，
/// 预览里就该是哪一行在前，重排只会让人对不上号。
pub fn parse_phrase_preview(v: &Value) -> PhrasePreview {
    PhrasePreview {
        title: v
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string(),
        rows: v
            .get("entries")
            .and_then(|e| e.as_array())
            .map(|arr| arr.iter().map(parse_row).collect())
            .unwrap_or_default(),
        problems: v
            .get("problems")
            .and_then(|p| p.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|p| {
                        let line = p.get("line").and_then(Value::as_u64).unwrap_or(0);
                        let msg = p
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or_default();
                        format!("第 {line} 行：{msg}")
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn parse_row(e: &Value) -> PhraseRow {
    let broken = e.get("error").and_then(|x| x.as_str()).is_some();
    let note = if broken {
        format!(
            "语法错误，不会导入（{}）",
            e.get("error").and_then(|x| x.as_str()).unwrap_or_default()
        )
    } else {
        match e.get("status").and_then(|s| s.as_str()) {
            Some("existsUser") => "已存在，跳过".to_string(),
            Some("shadowsSystem") => "会遮蔽同名系统短语".to_string(),
            _ => "新增".to_string(),
        }
    };

    PhraseRow {
        code: e
            .get("code")
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string(),
        text: e
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string(),
        note,
        hint: e
            .get("hints")
            .and_then(|h| h.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|h| h.get("message").and_then(|m| m.as_str()))
                    .collect::<Vec<_>>()
                    .join("；")
            })
            .unwrap_or_default(),
        broken,
    }
}

/// 应用导入。返回给用户看的一句话结果。
///
/// **不传 `accept`**：core 缺省导入全部可导入条目。传空数组会被理解成「一条也不要」，
/// 是完全相反的意思。
pub fn apply_phrase_import(text: &str) -> Result<String, String> {
    let v = rpc::call_core("phrase.importText", &json!({ "content": text }))
        .map_err(|e| format!("导入失败: {e}"))?;
    let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let mut msg = format!("已导入 {} 条短语", n("added"));
    let skipped = n("skippedExisting");
    if skipped > 0 {
        msg.push_str(&format!("，{skipped} 条已存在未改动"));
    }
    let shadowed = n("shadowedSystem");
    if shadowed > 0 {
        msg.push_str(&format!("，{shadowed} 条遮蔽了系统短语"));
    }
    let invalid = n("skippedInvalid");
    if invalid > 0 {
        msg.push_str(&format!("，{invalid} 条语法有误未导入"));
    }
    Ok(msg)
}

/// 预览列表的一行：编码 / 内容 / 落点。
///
/// 内容列**不截断成一行**：命令类短语的正文就是用户唯一能据以判断「这条要干什么」的
/// 依据，把它省略号掉等于把预览的意义抽掉。
pub fn phrase_row_element(row: &PhraseRow) -> Element {
    let fg = if row.broken {
        Role::TextDisabled
    } else {
        Role::Text
    };
    Element::row()
        .width_match()
        .cross(Align::Start)
        .spacing(10)
        .padding_xy(10, 5)
        .child(
            Element::label(&row.code)
                .font_size(12.5)
                .fg_role(fg)
                .font_weight(600)
                .width(64)
                .max_lines(1)
                .truncate(Truncate::End),
        )
        .child(
            Element::col()
                .weight(1.0)
                .spacing(2)
                .child(
                    Element::label(&row.text)
                        .font_size(12.5)
                        .fg_role(fg)
                        .width_match()
                        .max_lines(4),
                )
                .child(
                    Element::label(&row.note)
                        .font_size(11.5)
                        .fg_role(if row.broken {
                            Role::Danger
                        } else {
                            Role::TextMuted
                        })
                        .width_match()
                        .max_lines(3),
                )
                .child(hint_line(&row.hint)),
        )
}

/// 笔误提示行。空提示时给一个零高度占位而不是条件拼装——`Element` 的 builder 链
/// 每步取走 self，中途插 `if` 要把整条链拆成可变变量，为一行提示不值得。
fn hint_line(hint: &str) -> Element {
    if hint.is_empty() {
        return Element::leaf().height(0);
    }
    Element::label(hint)
        .font_size(11.0)
        .fg_role(Role::Warning)
        .width_match()
        .max_lines(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview_json() -> Value {
        json!({
            "title": "我的直通车",
            "entries": [
                { "line": 2, "code": "kx", "text": "(＾▽＾)", "status": "new",
                  "hints": [], "error": null },
                { "line": 3, "code": "zd", "text": "$CC(\"记事本\", proc.run(\"notepad.exe\"))",
                  "status": "new", "hints": [], "error": null },
                { "line": 4, "code": "sh", "text": "$CC(\"跑\", proc.shell(\"x\"))",
                  "status": "shadowsSystem", "hints": [], "error": null },
                { "line": 5, "code": "bad", "text": "$CC(\"未闭合", "status": "new",
                  "hints": [], "error": "解析失败" }
            ],
            "problems": [ { "line": 6, "raw": "xx", "message": "缺少空格分隔" } ]
        })
    }

    #[test]
    fn detection_matches_marker_prefix() {
        assert!(is_phrase_text("wind:p1\nkx 好\n"));
        assert!(is_phrase_text("\n  wind:p1 标题\nkx 好\n"));
        assert!(!is_phrase_text("[package]\nkind = \"schema_text\"\n"));
        assert!(!is_phrase_text("input.mode = \"pinyin\"\n"));
        assert!(!is_phrase_text(""));
    }

    /// ★ 顺序即源文件行序，不重排——用户在聊天窗口读到的顺序要对得上。
    #[test]
    fn rows_keep_source_order() {
        let p = parse_phrase_preview(&preview_json());
        let codes: Vec<&str> = p.rows.iter().map(|r| r.code.as_str()).collect();
        assert_eq!(codes, vec!["kx", "zd", "sh", "bad"]);
        assert_eq!(p.title, "我的直通车");
        assert_eq!(p.problems.len(), 1);
        assert!(p.problems[0].contains("第 6 行"));
    }

    /// ★ 命令短语与纯文本一视同仁：note 只说落点，不提它调用了什么。
    #[test]
    fn command_phrase_note_is_just_status() {
        let p = parse_phrase_preview(&preview_json());
        let zd = p.rows.iter().find(|r| r.code == "zd").unwrap();
        assert_eq!(zd.note, "新增");
        assert!(!zd.broken);
        assert!(zd.hint.is_empty());
    }

    #[test]
    fn shadowing_system_phrase_is_called_out() {
        let p = parse_phrase_preview(&preview_json());
        let sh = p.rows.iter().find(|r| r.code == "sh").unwrap();
        assert!(sh.note.contains("遮蔽"), "{}", sh.note);
    }

    #[test]
    fn broken_row_is_marked() {
        let p = parse_phrase_preview(&preview_json());
        let bad = p.rows.iter().find(|r| r.code == "bad").unwrap();
        assert!(bad.broken);
        assert!(bad.note.contains("语法错误"), "{}", bad.note);
        assert!(p.has_importable(), "还有三条好的");
    }

    #[test]
    fn hint_is_carried_separately_from_note() {
        let p = parse_phrase_preview(&json!({
            "entries": [
                { "line": 2, "code": "b1", "text": "$CC(...)", "status": "new",
                  "hints": [{ "kind": "controlCharInPath", "func": "proc.run",
                              "message": "proc.run 的路径里出现了换行或制表符" }],
                  "error": null }
            ]
        }));
        let row = &p.rows[0];
        assert!(row.hint.contains("换行"), "{}", row.hint);
        assert_eq!(row.note, "新增", "提示不混进落点说明");
        assert!(!row.broken, "笔误不阻止导入");
    }

    #[test]
    fn missing_hints_field_is_empty_not_panic() {
        let p = parse_phrase_preview(&json!({
            "entries": [ { "line": 2, "code": "k", "text": "好", "status": "new" } ]
        }));
        assert_eq!(p.rows[0].hint, "");
        assert_eq!(p.rows[0].note, "新增");
    }

    #[test]
    fn all_broken_has_nothing_to_apply() {
        let p = parse_phrase_preview(&json!({
            "entries": [ { "line": 2, "code": "b", "text": "$CC(", "status": "new",
                           "hints": [], "error": "解析失败" } ]
        }));
        assert!(!p.has_importable());
    }

    #[test]
    fn empty_preview_has_nothing_to_apply() {
        let p = parse_phrase_preview(&json!({ "entries": [] }));
        assert!(!p.has_importable());
        assert!(p.rows.is_empty());
    }
}
