//! 外部编辑（设计 `user-dict-libraries-and-external-edit.md` §4）：「编辑文件」、编辑中的模态
//! 对话框、「打开数据目录」与「刷新」时的重读文件。
//!
//! 直接改 `data/` 下的真源文件：core 在编辑期间暂停写回，「完成并加载」时按文件同步。编辑
//! 期间词库窗弹着模态对话框，界面里的其它操作都挡住，直到完成或关窗。
//!
//! 会话与 core 对齐：每次重载列表顺带取一次 `data.editSessions`（core 侧只读内存），重开窗口、
//! 两小时租约到期都会自然反映出来。结束时有问题的那个由本端保留，直到再次打开或关掉。
//! 同时有多个（重开窗口时 core 那边留着几个）就一个一个处理。

use serde_json::{Value, json};

use super::spec::WordCategory;
use super::state::DictManagerState;
use crate::rpc;

/// 会话的一条：一个正在外部编辑、或刚结束但有问题的数据文件。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditBar {
    /// 相对数据目录、`/` 分隔（core 返回的 `file`）。
    pub file: String,
    /// 结束时发现的问题；`None` = 还在编辑。
    pub problem: Option<String>,
    /// 绝对路径（`data.beginEdit` 给的）；从会话列表恢复的没有，为空。
    pub path: String,
}

/// 当前类别「编辑文件」要发给 `data.beginEdit` 的参数；本类没有这个入口时为 `None`。
///
/// 用户词与候选调整只给方案 id 和类别，由 core 换算数据方案目录（拼音族折叠等）——这里不复刻
/// 那份映射。
pub(crate) fn edit_target(cat: WordCategory, schema: Option<&str>) -> Option<Value> {
    let kind = match cat {
        WordCategory::UserDict => "words",
        WordCategory::Shadow => "shadow",
        WordCategory::Temp => "temp",
        WordCategory::Freq => "freq",
        WordCategory::PhraseUser => return Some(json!({ "kind": "phrases" })),
        WordCategory::QuickFormat => return Some(json!({ "kind": "quick_format" })),
        WordCategory::CommonChars => return Some(json!({ "kind": "common_chars" })),
        _ => return None,
    };
    Some(json!({ "schemaId": schema?, "kind": kind }))
}

/// 一个字符分类的「编辑文件」参数（字符分类一类一个文件）。
pub(crate) fn charset_edit_target(key: &str) -> Value {
    json!({ "kind": "charsets", "key": key })
}

/// 对话框里按文件补的一句提醒；没有为 `None`。
pub(crate) fn edit_note(file: &str) -> Option<&'static str> {
    let name = file.rsplit('/').next().unwrap_or(file);
    if name == "temp.dict.yaml" || name == "freq.table.yaml" {
        Some(
            "这是程序自动学习的数据。编辑期间继续打字，新学到的内容会在完成后合并进来；\
             删掉的词如果期间又用到，会带着新的次数重新出现。",
        )
    } else if file.starts_with("charsets/") {
        Some("这里只记你对这个分类的改动，出厂内容不在文件里。出厂分类的范围不能改。")
    } else {
        None
    }
}

/// `data.endEdit` 的 `issues` → 一句话；没有问题为 `None`。
pub(crate) fn issues_text(issues: &[Value]) -> Option<String> {
    let parts: Vec<String> = issues
        .iter()
        .filter_map(|it| {
            Some(match it.get("kind")?.as_str()? {
                "bad_lines" => format!(
                    "{} 行格式不对，已跳过",
                    it.get("lines").and_then(Value::as_u64).unwrap_or(0)
                ),
                "isolated" => "文件格式损坏，未载入".to_string(),
                // 字符分类：解析错误、改了出厂范围等，core 给出原话。
                "invalid" => it.get("message")?.as_str()?.to_string(),
                "rebuilt" => format!(
                    "文件损坏，已另存为 {} 并恢复",
                    it.get("kept_as").and_then(Value::as_str).unwrap_or("")
                ),
                _ => return None,
            })
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join("；"))
}

/// core 的会话列表 → 会话：会话按 core 的顺序各一条「编辑中」，本端留着的问题条若不在
/// 会话里则保留（它们只活在本端）。
pub(crate) fn merge_sessions(bars: &[EditBar], files: &[String]) -> Vec<EditBar> {
    let editing = files.iter().map(|f| EditBar {
        file: f.clone(),
        problem: None,
        path: bars
            .iter()
            .find(|b| &b.file == f)
            .map(|b| b.path.clone())
            .unwrap_or_default(),
    });
    let problems = bars
        .iter()
        .filter(|b| b.problem.is_some() && !files.contains(&b.file))
        .cloned();
    editing.chain(problems).collect()
}

/// 用系统默认程序打开。mock 模式下路径是虚构的，不去真的拉起程序。
pub(super) fn open_path(path: &str) -> Result<(), String> {
    if rpc::is_mock() {
        return Ok(());
    }
    crate::open_external::open_with_default_app(std::path::Path::new(path))
}

impl DictManagerState {
    /// 当前类别的「编辑文件」参数（见 [`edit_target`]）。
    pub fn edit_file_target(&self) -> Option<Value> {
        if let (Some(id), Some(name)) = (self.current_schema_id(), self.selected_lib()) {
            return Some(super::user_libs::lib_edit_target(&id, &name));
        }
        edit_target(self.current_category(), self.current_schema_id().as_deref())
    }

    /// 「编辑文件」：开始编辑当前类别的数据文件，并用系统默认程序打开它。
    pub fn do_edit_file(&self) {
        if let Some(p) = self.edit_file_target() {
            self.begin_and_open(&p);
        }
    }

    /// 「再次打开」：同一文件再开始一次（core 那边是同一会话，或已结束则新开）并打开。
    pub fn reopen_edit(&self, file: &str) {
        self.begin_and_open(&json!({ "path": file }));
    }

    pub(super) fn begin_and_open(&self, params: &Value) {
        let v = match rpc::call_core("data.beginEdit", params) {
            Ok(v) => v,
            Err(e) => {
                self.notify_err(&format!("打不开文件：{e}"));
                return;
            }
        };
        let file = v.get("file").and_then(Value::as_str).unwrap_or_default();
        let path = v.get("path").and_then(Value::as_str).unwrap_or_default();
        self.set_bar(EditBar {
            file: file.to_string(),
            problem: None,
            path: path.to_string(),
        });
        if let Err(e) = open_path(path) {
            self.notify_err(&format!("没能打开编辑器（{e}），文件在 {path}"));
        }
    }

    /// 「完成并加载」：结束编辑，载入改动；有问题时会话留着显示问题。
    pub fn finish_edit(&self, file: &str) {
        let v = match rpc::call_core("data.endEdit", &json!({ "path": file })) {
            Ok(v) => v,
            Err(e) => {
                self.notify_err(&format!("加载失败：{e}"));
                // 会话可能已经不在了（租约到期）：与 core 对齐，过期的那个随之消失。
                self.refresh_edit_bars();
                return;
            }
        };
        let issues = v
            .get("issues")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let path = self
            .edit_bars
            .get()
            .into_iter()
            .find(|b| b.file == file)
            .map(|b| b.path)
            .unwrap_or_default();
        match issues_text(&issues) {
            Some(problem) => self.set_bar(EditBar {
                file: file.to_string(),
                problem: Some(problem),
                path,
            }),
            None => {
                self.drop_bar(file);
                if v.get("changed").and_then(Value::as_bool) == Some(true) {
                    self.notify_ok("已载入修改");
                } else {
                    self.notify_info("文件没有变化");
                }
            }
        }
        // 分库文件改完，条目数与解析问题都可能变了。
        self.reload_with_libs();
    }

    /// 关掉一条有问题的会话（会话在 core 那边已经结束）。
    pub fn dismiss_edit(&self, file: &str) {
        self.drop_bar(file);
    }

    /// 词库窗关闭：仍在编辑的文件逐个结束（设计：设置退出即结束；core 另有两小时租约兜底）。
    pub fn end_all_edits(&self) {
        for bar in self.edit_bars.get() {
            if bar.problem.is_none()
                && let Err(e) = rpc::call_core("data.endEdit", &json!({ "path": bar.file }))
            {
                tracing::warn!("关窗时结束外部编辑 {} 失败：{e}", bar.file);
            }
        }
        self.publish_bars(Vec::new());
    }

    /// 与 core 的会话对齐（重载列表时顺带做；取不到就保持原样）。
    pub(crate) fn refresh_edit_bars(&self) {
        let Ok(v) = rpc::call_core("data.editSessions", &json!({})) else {
            return;
        };
        let files: Vec<String> = v
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.get("file").and_then(Value::as_str).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let next = merge_sessions(&self.edit_bars.get(), &files);
        if next != self.edit_bars.get() {
            self.publish_bars(next);
        }
    }

    /// 「打开数据目录」：方案域打开该方案的数据目录，全局域打开数据目录本身。
    pub fn open_data_dir(&self) {
        self.open_data_dir_at(self.domain.get());
    }

    /// 能不能「删除此方案的用户数据」：数据目录在、且 core 说可以单独删（与别的方案共用
    /// 数据——拼音族、混输方案的主方案——不行）。全局域、取不到、旧版 core 都为 false。
    pub fn domain_purgeable(&self, domain: usize) -> bool {
        let Some(id) = self.schema_id_at(domain) else {
            return false;
        };
        let Ok(v) = rpc::call_core("data.locate", &json!({ "schemaId": id })) else {
            return false;
        };
        let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        flag("exists") && flag("purgeable")
    }

    /// 打开某个数据域的数据目录：方案域是该方案的目录，全局域是数据目录本身。
    pub fn open_data_dir_at(&self, domain: usize) {
        let params = match self.schema_id_at(domain) {
            Some(id) => json!({ "schemaId": id }),
            None => json!({}),
        };
        let path = match rpc::call_core("data.locate", &params) {
            Ok(v) => v
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            Err(e) => {
                self.notify_err(&format!("找不到数据目录：{e}"));
                return;
            }
        };
        if let Err(e) = open_path(&path) {
            self.notify_err(&format!("没能打开文件夹（{e}），目录在 {path}"));
        }
    }

    /// 「刷新」：让 core 重读数据文件（文件没变只比指纹），再重取分库与列表。有文件变了才提示。
    pub fn refresh_all(&self) {
        match rpc::call_core("dict.reload", &json!({})) {
            Ok(v) => {
                let n = v
                    .get("reloaded")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                if n > 0 {
                    self.notify_ok(&format!("已重新加载，{n} 项有变化"));
                }
            }
            Err(e) => self.notify_err(&format!("重新加载失败：{e}")),
        }
        self.reload_with_libs();
    }

    /// 同名文件的那个替换掉，否则追加。
    fn set_bar(&self, bar: EditBar) {
        let mut bars = self.edit_bars.get();
        match bars.iter_mut().find(|b| b.file == bar.file) {
            Some(b) => *b = bar,
            None => bars.push(bar),
        }
        self.publish_bars(bars);
    }

    fn drop_bar(&self, file: &str) {
        let mut bars = self.edit_bars.get();
        bars.retain(|b| b.file != file);
        self.publish_bars(bars);
    }

    /// 写会话列表，并让对话框跟上：有会话就弹着，先显示还在编辑的，再显示有问题的。
    pub(crate) fn publish_bars(&self, bars: Vec<EditBar>) {
        let first: Vec<EditBar> = bars
            .iter()
            .find(|b| b.problem.is_none())
            .or_else(|| bars.first())
            .cloned()
            .into_iter()
            .collect();
        if self.edit_current.get() != first {
            self.edit_current.set(first);
        }
        let show = !bars.is_empty();
        self.edit_bars.set(bars);
        if self.edit_dialog.get() != show {
            self.edit_dialog.set(show);
        }
    }

    /// 对话框被 ESC / 关窗关掉（不是点按钮）：编辑中的当作「完成并加载」，有问题的当作
    /// 「关闭」。不这样的话会话还开着、对话框却没了，界面上再也找不到「完成并加载」。
    pub fn on_edit_dialog_closed(&self) {
        if self.edit_dialog.get() {
            return;
        }
        let Some(bar) = self.edit_current.get().into_iter().next() else {
            return;
        };
        match bar.problem {
            None => self.finish_edit(&bar.file),
            Some(_) => self.dismiss_edit(&bar.file),
        }
    }

    /// 「打开所在文件夹」：编辑中那个文件所在的目录。从 core 恢复的会话没有绝对路径，按相对
    /// 路径问一次 core。
    pub fn open_edit_folder(&self, bar: &EditBar) {
        let path = if bar.path.is_empty() {
            rpc::call_core("data.locate", &json!({ "path": bar.file }))
                .ok()
                .and_then(|v| v.get("path").and_then(Value::as_str).map(String::from))
                .unwrap_or_default()
        } else {
            bar.path.clone()
        };
        let dir = std::path::Path::new(&path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        if dir.is_empty() {
            return self.open_data_dir();
        }
        if let Err(e) = open_path(&dir) {
            self.notify_err(&format!("没能打开文件夹（{e}），目录在 {dir}"));
        }
    }

    /// 字符分类的「编辑文件」（一类一个文件；出厂分类还没有个人调整时 core 先建一个空的）。
    pub fn edit_charset_file(&self, key: &str) {
        self.begin_and_open(&charset_edit_target(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SchemaInfo;

    fn editing(file: &str) -> EditBar {
        EditBar {
            file: file.into(),
            problem: None,
            ..Default::default()
        }
    }

    fn with_mock<R>(f: impl FnOnce() -> R) -> R {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        crate::rpc::take_mock_calls();
        let r = f();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        r
    }

    /// 方案域停在 `kind` 方案的第 `sub` 个子标签上。
    fn state_at(kind: &str, sub: usize) -> DictManagerState {
        let st = DictManagerState::new();
        *st.schemas.borrow_mut() = vec![SchemaInfo {
            id: "s1".into(),
            name: "方案".into(),
            kind: kind.into(),
            ..Default::default()
        }];
        st.domain.set(super::super::state::GLOBAL_DOMAINS);
        st.sub_tab.set(sub);
        st
    }

    #[test]
    fn edit_target_covers_editable_categories() {
        use WordCategory as C;
        assert_eq!(
            edit_target(C::UserDict, Some("wubi86")),
            Some(json!({"schemaId": "wubi86", "kind": "words"}))
        );
        assert_eq!(
            edit_target(C::Shadow, Some("pinyin_simp")),
            Some(json!({"schemaId": "pinyin_simp", "kind": "shadow"}))
        );
        assert_eq!(
            edit_target(C::PhraseUser, None),
            Some(json!({"kind": "phrases"}))
        );
        assert_eq!(
            edit_target(C::QuickFormat, None),
            Some(json!({"kind": "quick_format"}))
        );
        assert_eq!(
            edit_target(C::CommonChars, None),
            Some(json!({"kind": "common_chars"}))
        );
        for c in [
            C::PhraseSystem,
            C::Charsets,
            C::MailSuffix,
            C::UrlHistory,
            C::QuickHistory,
        ] {
            assert_eq!(edit_target(c, Some("wubi86")), None, "{c:?}");
        }
        assert_eq!(edit_target(C::UserDict, None), None, "没有方案就没有入口");
        assert_eq!(
            edit_target(C::Temp, Some("wubi86")),
            Some(json!({"schemaId": "wubi86", "kind": "temp"}))
        );
        assert_eq!(
            edit_target(C::Freq, Some("wubi86")),
            Some(json!({"schemaId": "wubi86", "kind": "freq"}))
        );
    }

    /// 混输方案只有候选调整一个子标签，照样能编辑。
    #[test]
    fn mixed_schema_can_edit_its_shadow_file() {
        let st = state_at("mixed", 0);
        assert_eq!(st.current_category(), WordCategory::Shadow);
        assert_eq!(
            st.edit_file_target(),
            Some(json!({"schemaId": "s1", "kind": "shadow"}))
        );
    }

    /// 对话框被 ESC / 关窗关掉：编辑中的当作完成并加载（endEdit），有问题的当作关闭。
    #[test]
    fn closing_the_dialog_finishes_or_dismisses() {
        with_mock(|| {
            let st = state_at("codetable", 0);
            st.do_edit_file();
            crate::rpc::take_mock_calls();
            st.edit_dialog.set(false); // 模态遮罩的 ESC 就是这么做的
            st.on_edit_dialog_closed();
            let ends: Vec<Value> = crate::rpc::take_mock_calls()
                .into_iter()
                .filter(|(m, _)| m == "data.endEdit")
                .map(|(_, p)| p)
                .collect();
            assert_eq!(ends, vec![json!({"path": "s1/user.dict.yaml"})]);
            assert!(st.edit_bars.get().is_empty());

            // 有问题的：关掉就是丢掉那条，不再调 core
            st.publish_bars(vec![EditBar {
                file: "bad/user.dict.yaml".into(),
                problem: Some("2 行格式不对，已跳过".into()),
                ..Default::default()
            }]);
            st.edit_dialog.set(false);
            st.on_edit_dialog_closed();
            assert!(st.edit_bars.get().is_empty());
            assert!(
                !crate::rpc::take_mock_calls()
                    .iter()
                    .any(|(m, _)| m == "data.endEdit")
            );
        });
    }

    /// 还在编辑的排在有问题的前面显示：新开的不被旧问题挡住。
    #[test]
    fn editing_session_is_shown_before_problems() {
        let st = DictManagerState::new();
        st.publish_bars(vec![
            EditBar {
                file: "a.yaml".into(),
                problem: Some("坏".into()),
                ..Default::default()
            },
            editing("b.yaml"),
        ]);
        assert_eq!(st.edit_current.get()[0].file, "b.yaml");
    }

    /// 学习数据与字符分类在对话框里多一句提醒，其余没有。
    #[test]
    fn edit_note_for_learned_and_charsets() {
        assert!(edit_note("pinyin/temp.dict.yaml").unwrap().contains("合并"));
        assert!(edit_note("wb/freq.table.yaml").is_some());
        assert!(edit_note("charsets/rare.yaml").unwrap().contains("出厂"));
        assert_eq!(edit_note("wb/user.dict.yaml"), None);
        assert_eq!(edit_note("phrases.dict.yaml"), None);
    }

    /// 有会话就弹对话框、显示第一个；都结束了就收起。
    #[test]
    fn dialog_follows_sessions() {
        with_mock(|| {
            let st = state_at("codetable", 1);
            st.do_edit_file();
            assert!(st.edit_dialog.get());
            assert_eq!(st.edit_current.get()[0].file, "s1/temp.dict.yaml");
            assert_eq!(
                st.edit_current.get()[0].path,
                "/mock/data/s1/temp.dict.yaml"
            );
            st.finish_edit("s1/temp.dict.yaml");
            assert!(!st.edit_dialog.get());
            assert!(st.edit_current.get().is_empty());
        });
    }

    /// 字符分类：一类一个文件，按 key 开会话。
    #[test]
    fn charset_file_edit_targets_the_class() {
        with_mock(|| {
            let st = DictManagerState::new();
            crate::rpc::take_mock_calls();
            st.edit_charset_file("rare");
            let begins: Vec<Value> = crate::rpc::take_mock_calls()
                .into_iter()
                .filter(|(m, _)| m == "data.beginEdit")
                .map(|(_, p)| p)
                .collect();
            assert_eq!(begins, vec![json!({"kind": "charsets", "key": "rare"})]);
            assert_eq!(st.edit_current.get()[0].file, "charsets/rare.yaml");
        });
    }

    #[test]
    fn issues_text_by_kind() {
        assert_eq!(issues_text(&[]), None);
        assert_eq!(
            issues_text(&[
                json!({"kind": "invalid", "message": "出厂字符分类的范围不能改，已忽略"})
            ])
            .as_deref(),
            Some("出厂字符分类的范围不能改，已忽略")
        );
        assert_eq!(
            issues_text(&[json!({"path": "a", "kind": "bad_lines", "lines": 3})]).as_deref(),
            Some("3 行格式不对，已跳过")
        );
        assert_eq!(
            issues_text(&[json!({"path": "a", "kind": "isolated"})]).as_deref(),
            Some("文件格式损坏，未载入")
        );
        assert_eq!(
            issues_text(&[json!({"path": "a", "kind": "rebuilt", "kept_as": "a.corrupt"})])
                .as_deref(),
            Some("文件损坏，已另存为 a.corrupt 并恢复")
        );
        assert_eq!(
            issues_text(&[json!({"path": "a", "kind": "unknown"})]),
            None,
            "不认识的问题不编文案"
        );
    }

    #[test]
    fn merge_sessions_follows_core_and_keeps_local_problems() {
        let problem = EditBar {
            file: "phrases.dict.yaml".into(),
            problem: Some("文件格式损坏，未载入".into()),
            ..Default::default()
        };
        // 重开窗口：本端为空，按 core 恢复。
        assert_eq!(
            merge_sessions(&[], &["wubi86/user.dict.yaml".into()]),
            vec![editing("wubi86/user.dict.yaml")]
        );
        // 租约到期：core 没了的会话消失；问题条留着。
        assert_eq!(
            merge_sessions(&[editing("wubi86/user.dict.yaml"), problem.clone()], &[]),
            vec![problem.clone()]
        );
        // 问题条的文件又开始编辑了：变回编辑中，不重复。
        assert_eq!(
            merge_sessions(&[problem], &["phrases.dict.yaml".into()]),
            vec![editing("phrases.dict.yaml")]
        );
    }

    /// 编辑文件 → beginEdit（方案域带方案与类别）→ 出现会话；完成并加载 → endEdit → 条消失。
    #[test]
    fn edit_then_finish_round_trip() {
        with_mock(|| {
            let st = state_at("codetable", 0);
            st.do_edit_file();
            let calls = crate::rpc::take_mock_calls();
            assert_eq!(calls[0].0, "data.beginEdit");
            assert_eq!(calls[0].1, json!({"schemaId": "s1", "kind": "words"}));
            assert_eq!(
                st.edit_bars.get(),
                vec![EditBar {
                    file: "s1/user.dict.yaml".into(),
                    problem: None,
                    path: "/mock/data/s1/user.dict.yaml".into(),
                }]
            );

            st.finish_edit("s1/user.dict.yaml");
            let calls = crate::rpc::take_mock_calls();
            assert_eq!(calls[0].0, "data.endEdit");
            assert_eq!(calls[0].1, json!({"path": "s1/user.dict.yaml"}));
            assert!(st.edit_bars.get().is_empty());
            let (_, msg) = st.feedback.get().expect("应有提示");
            assert_eq!(msg, "已载入修改");
        });
    }

    /// 结束时有坏行：会话留着、显示问题，再次打开回到编辑中。
    #[test]
    fn finish_with_issues_keeps_the_bar() {
        with_mock(|| {
            let st = state_at("codetable", 0);
            st.edit_bars.set(vec![editing("bad/user.dict.yaml")]);
            st.finish_edit("bad/user.dict.yaml");
            assert_eq!(
                st.edit_bars.get(),
                vec![EditBar {
                    file: "bad/user.dict.yaml".into(),
                    problem: Some("2 行格式不对，已跳过".into()),
                    ..Default::default()
                }]
            );
            st.reopen_edit("bad/user.dict.yaml");
            let bars = st.edit_bars.get();
            assert_eq!(bars.len(), 1);
            assert_eq!(
                (bars[0].file.as_str(), &bars[0].problem),
                ("bad/user.dict.yaml", &None)
            );
            st.dismiss_edit("bad/user.dict.yaml");
            assert!(st.edit_bars.get().is_empty());
        });
    }

    /// 关窗：编辑中的逐个结束，问题条不再调 core。
    #[test]
    fn closing_ends_every_open_session() {
        with_mock(|| {
            let st = DictManagerState::new();
            st.edit_bars.set(vec![
                editing("phrases.dict.yaml"),
                EditBar {
                    file: "quick_format.toml".into(),
                    problem: Some("文件格式损坏，未载入".into()),
                    ..Default::default()
                },
                editing("wubi86/shadow.dict.yaml"),
            ]);
            st.end_all_edits();
            let ends: Vec<Value> = crate::rpc::take_mock_calls()
                .into_iter()
                .filter(|(m, _)| m == "data.endEdit")
                .map(|(_, p)| p)
                .collect();
            assert_eq!(
                ends,
                vec![
                    json!({"path": "phrases.dict.yaml"}),
                    json!({"path": "wubi86/shadow.dict.yaml"})
                ]
            );
            assert!(st.edit_bars.get().is_empty());
        });
    }

    /// 打开数据目录：方案域带方案 id，全局域不带。
    #[test]
    fn open_data_dir_scopes_to_the_current_schema() {
        with_mock(|| {
            let st = state_at("codetable", 0);
            st.open_data_dir();
            st.domain.set(0);
            st.open_data_dir();
            let params: Vec<Value> = crate::rpc::take_mock_calls()
                .into_iter()
                .filter(|(m, _)| m == "data.locate")
                .map(|(_, p)| p)
                .collect();
            assert_eq!(params, vec![json!({"schemaId": "s1"}), json!({})]);
        });
    }

    /// 刷新：先让 core 重读文件，变了几项就报几项。
    #[test]
    fn refresh_all_reloads_files_and_reports_changes() {
        with_mock(|| {
            let st = DictManagerState::new();
            st.refresh_all();
            let calls = crate::rpc::take_mock_calls();
            assert_eq!(calls[0], ("dict.reload".to_string(), json!({})));
            let (_, msg) = st.feedback.get().expect("应有提示");
            assert_eq!(msg, "已重新加载，2 项有变化");
        });
    }

    /// 编辑期间界面改同一张表，core 拒绝的原话要显示出来。
    #[test]
    fn core_refusal_during_edit_reaches_the_toast() {
        with_mock(|| {
            let st = DictManagerState::new();
            st.domain.set(0);
            st.sub_tab.set(1);
            crate::rpc::force_mock_error(
                "phrase.remove",
                Some("正在外部编辑 phrases.dict.yaml，完成后再改"),
            );
            st.do_delete_row("dz", "地址");
            crate::rpc::force_mock_error("phrase.remove", None);
            let (_, msg) = st.feedback.get().expect("应有提示");
            assert!(
                msg.contains("正在外部编辑 phrases.dict.yaml，完成后再改"),
                "{msg}"
            );
        });
    }
}
