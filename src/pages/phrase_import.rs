//! 短语逐行分发格式（`wind:p1`）的**出入口两侧**：导入的预览与应用，以及从一条已有短语
//! 反向生成同格式文本（词库页右键「复制为一键导入文本」）。
//!
//! 两侧同处一个模块是为了让 [`MARKER_PREFIX`] 只有一份——它是跨仓契约，抄成两份之后
//! 漂移的症状是「复制出来的文本别人导不进去」，而失败点离改动处隔着一次分享。
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

/// 出口写死的格式版本。core 的 `SUPPORTED_VERSION` 是权威上限，但「宽容只给过去、
/// 不给未来」是对**读**的一侧说的——写的一侧该挑收得最广的那个版本，而不是最新的：
/// 拿旧版应用的人收到 `wind:p2` 会被硬拒绝，而这条短语本可以用 v1 表达。
const SHARE_VERSION: u32 = 1;

/// 一条短语 →「一键导入文本」。`None` = 这条生成不出合法文本（判据见下）。
///
/// `text` 必须是**分发域**文本，也就是设置页列表里那一串原样。core 的 `phrase.listUser`
/// 出口已过 `ui_text`（即 `wdict::escape_text_field`），而 `phrase.previewImportText`
/// 回显的也是同一串——两域同形，故此处**一个字符都不再转义**。多转一层的症状是命令
/// 短语里的反斜杠每分享一轮就翻一倍，对方那边路径静默损坏。
///
/// 生成不了就拒绝、不勉强拼一串出来，是因为「行内零解析」把全部结构压在两个位置上，
/// 任一位置被污染都会让整行在对方那边失效：
/// - code 含空白 → 拆分点跑到那个空白上，正文被截断成 code 的一部分；
/// - code 含非可打印 ASCII → core 判 `BadCode`，整行跳过；
/// - text 含换行/制表 → 一条被切成两行，后半行多半连 code 都拼不出。
///
/// 三者都是「复制时无声、导入时才炸」——而炸的时候用户早已把文本发出去了。
pub fn phrase_share_text(code: &str, text: &str) -> Option<String> {
    // `is_ascii_graphic` 已排除空白与控制符，与 core `phrase_text::is_valid_code` 同构。
    if code.is_empty() || !code.chars().all(|c| c.is_ascii_graphic()) {
        return None;
    }
    if text.is_empty() || text.contains(['\n', '\r', '\t']) {
        return None;
    }
    Some(format!("{MARKER_PREFIX}{SHARE_VERSION}\n{code} {text}\n"))
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

    // ── 出口：复制为一键导入文本 ──────────────────────────────

    /// ★ 出口必须过自己的入口。前缀常量只有一份，这条测试是它的守卫：
    /// 哪天有人把 `MARKER_PREFIX` 或 `SHARE_VERSION` 改到与 core 不一致，这里先炸，
    /// 而不是等用户把文本发出去之后由对方那边报「缺少格式标记」。
    #[test]
    fn share_text_is_recognized_by_the_importer() {
        let out = phrase_share_text("kx", "(＾▽＾)").unwrap();
        assert!(is_phrase_text(&out), "{out:?}");
        assert_eq!(out, "wind:p1\nkx (＾▽＾)\n");
    }

    /// ★★ 命令短语逐字穿过，**一个反斜杠都不许多**。
    ///
    /// 列表里那串已是分发域（core 出口过了 `escape_text_field`），出口再转一层的话
    /// 反斜杠每分享一轮翻一倍：对方 lexer 拿到的 `D:\\notes` 会变成 `D:<换行>otes`，
    /// 路径静默损坏，且失败点在他触发短语时，离导入隔着好几天。
    #[test]
    fn share_text_keeps_command_phrase_verbatim() {
        let cmd = r#"$CC("知典 {sub(reverse(last(1)), 1, 1)}", proc.run("D:\\\\Program Files\\\\知典.exe", "--query"))"#;
        let out = phrase_share_text("zd", cmd).unwrap();
        assert_eq!(out, format!("wind:p1\nzd {cmd}\n"), "正文必须逐字保留");
    }

    /// 正文里的空格不是分隔符——只认第一个空白，其余原样到行尾。
    #[test]
    fn share_text_allows_spaces_inside_the_body() {
        let out = phrase_share_text("em", "我 的 邮箱 是 a@b.com").unwrap();
        assert_eq!(out, "wind:p1\nem 我 的 邮箱 是 a@b.com\n");
    }

    /// ★ 拼不出合法文本时返回 `None`，不勉强拼一串出来。
    ///
    /// 这四种输入生成的文本在**复制时毫无异样**，只在对方导入时才失效（编码被截断、
    /// 整行判 BadCode、一条切成两行）。那时文本早已发出去，没人能把它关联回这一步。
    #[test]
    fn share_text_refuses_what_the_importer_would_drop() {
        assert_eq!(
            phrase_share_text("k x", "好"),
            None,
            "编码带空白会把正文截断"
        );
        assert_eq!(
            phrase_share_text("编码", "好"),
            None,
            "编码非可打印 ASCII，core 判 BadCode"
        );
        assert_eq!(phrase_share_text("", "好"), None, "空编码");
        assert_eq!(
            phrase_share_text("kx", ""),
            None,
            "空正文，core 判 EmptyText"
        );
        assert_eq!(
            phrase_share_text("kx", "上\n下"),
            None,
            "真换行会把一条切成两行"
        );
        assert_eq!(phrase_share_text("kx", "左\t右"), None, "真制表同理");
    }

    /// 分发域里的换行是**字面两字符** `\\n`（core 出口 `escape_text_field` 的投影），
    /// 它不该被上面那条守卫误伤——多行短语正是要靠它才能落在一行里分发。
    #[test]
    fn share_text_accepts_the_escaped_newline_form() {
        let out = phrase_share_text("dz", r"第一行\n第二行").unwrap();
        assert_eq!(out, "wind:p1\ndz 第一行\\n第二行\n");
        assert_eq!(out.lines().count(), 2, "整条短语仍占一行");
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
