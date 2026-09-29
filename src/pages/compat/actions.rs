//! 「应用兼容」页的整体操作：新增规则、全部还原、导出、导入（预览 + 确认）。
//!
//! 与 `state.rs` 里的单条规则写操作分开放：这些都带对话框，状态是「对话框开着 / 关着 + 一份
//! 待确认的内容」，与规则列表的重建节奏无关。

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::core::EventCtx;
use windui::prelude::*;

use super::model::ImportPreview;
use super::state::CompatState;
use crate::rpc;

/// 导入方式，下标对应对话框里的分段控件。
pub const IMPORT_MODES: [(&str, &str); 2] = [("合并", "merge"), ("替换", "replace")];

/// 各对话框的开合与待确认内容。
#[derive(Clone)]
pub struct Dialogs {
    pub add_visible: Signal<bool>,
    pub add_name: Signal<String>,
    pub add_error: Signal<String>,

    pub reset_visible: Signal<bool>,

    pub help_visible: Signal<bool>,

    /// 「添加覆盖项」（挑字段）对话框。
    pub field_visible: Signal<bool>,
    /// 「规则已没有任何配置项」的删除 / 保留确认。
    pub empty_visible: Signal<bool>,
    pub empty_name: Signal<String>,

    pub import_visible: Signal<bool>,
    /// 0 = 合并，1 = 替换（对应 [`IMPORT_MODES`]）。
    pub import_mode: Signal<usize>,
    /// 预览摘要（多行文本）。
    pub import_summary: Signal<String>,
    /// 预览里有没有真会改动的条目：为空时「导入」没有意义。
    pub import_can_apply: Signal<bool>,
    /// 待导入的文件内容；预览与确认用同一份，避免「预览的和导进去的不是同一个」。
    import_content: Rc<RefCell<String>>,
}

impl Default for Dialogs {
    fn default() -> Self {
        Self {
            add_visible: signal(false),
            add_name: signal(String::new()),
            add_error: signal(String::new()),
            reset_visible: signal(false),
            help_visible: signal(false),
            field_visible: signal(false),
            empty_visible: signal(false),
            empty_name: signal(String::new()),
            import_visible: signal(false),
            import_mode: signal(0),
            import_summary: signal(String::new()),
            import_can_apply: signal(false),
            import_content: Rc::new(RefCell::new(String::new())),
        }
    }
}

/// 把预览渲染成给人看的几行字。
pub fn summarize(p: &ImportPreview, replace: bool) -> String {
    let (add, over, dis, same, rm) = p.counts();
    let mut lines = vec![format!(
        "新增 {add} 条 · 覆盖 {over} 条 · 禁用 {dis} 条 · 移除 {rm} 条 · 无变化 {same} 条"
    )];
    if replace {
        lines.push(
            "替换模式：会先清空现有的用户改动，再写入文件里的内容（原文件自动备份）。".into(),
        );
    }
    let named = |action: &str| -> Vec<String> {
        p.items
            .iter()
            .filter(|i| i.action == action)
            .map(|i| i.process.clone())
            .collect()
    };
    for (action, title) in [
        ("add", "新增"),
        ("override", "覆盖"),
        ("disable", "禁用"),
        ("remove", "移除"),
    ] {
        let names = named(action);
        if !names.is_empty() {
            lines.push(format!("{title}：{}", names.join("、")));
        }
    }
    if !p.fallbacks.is_empty() {
        lines.push(format!(
            "以下值不合法，会按「未设置」处理：{}",
            p.fallbacks.join("、")
        ));
    }
    if !p.ignored_keys.is_empty() {
        lines.push(format!(
            "本版本不认识的键，原样保留：{}",
            p.ignored_keys.join("、")
        ));
    }
    if !p.rejected.is_empty() {
        lines.push(format!("被拒绝的条目：{}", p.rejected.join("；")));
    }
    lines.join("\n")
}

impl CompatState {
    // ───────────── 新增 ─────────────

    pub fn open_add(&self) {
        self.dlg.add_name.set(String::new());
        self.dlg.add_error.set(String::new());
        self.dlg.add_visible.set(true);
    }

    /// 确认新增：只建草稿，第一次设置字段时才落盘。
    pub fn confirm_add(&self) {
        let name = self.dlg.add_name.get();
        match self.start_draft(&name) {
            Ok(_) => self.dlg.add_visible.set(false),
            Err(e) => self.dlg.add_error.set(e),
        }
    }

    // ───────────── 全部还原 ─────────────

    pub fn confirm_reset_all(&self) {
        self.dlg.reset_visible.set(false);
        if let Some(backup) = self.reset_all() {
            let tail = if backup.is_empty() {
                String::new()
            } else {
                format!("，原文件已备份为 {backup}")
            };
            self.notify(ToastKind::Success, format!("已还原全部规则{tail}"));
        }
    }

    // ───────────── 导出 ─────────────

    /// 导出用户层（只含用户的改动，出厂规则不导出）。
    pub fn start_export(&self, ctx: &mut EventCtx) {
        let st = self.clone();
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        ctx.request_save_file(
            PickDialog::new()
                .title("导出 · 应用兼容性与独立配置")
                .file_name(format!("compat-{stamp}.toml"))
                .filter("TOML", &["toml"]),
            move |path| {
                let Some(path) = path else { return };
                match rpc::call_core("compat.export", &json!({ "scope": "user" })) {
                    Ok(v) => {
                        let content = v.get("content").and_then(Value::as_str).unwrap_or("");
                        match std::fs::write(&path, content) {
                            Ok(()) => st.notify(ToastKind::Success, "已导出"),
                            Err(e) => st.notify(ToastKind::Error, format!("写文件失败: {e}")),
                        }
                    }
                    Err(e) => st.notify(ToastKind::Error, format!("导出失败: {e}")),
                }
            },
        );
    }

    // ───────────── 导入 ─────────────

    pub fn start_import(&self, ctx: &mut EventCtx) {
        let st = self.clone();
        ctx.request_pick_file(
            PickDialog::new()
                .title("导入 · 应用兼容性与独立配置")
                .filter("TOML", &["toml"]),
            move |path| {
                let Some(path) = path else { return };
                match std::fs::read_to_string(&path) {
                    Ok(content) => st.begin_import(content),
                    Err(e) => st.notify(ToastKind::Error, format!("读文件失败: {e}")),
                }
            },
        );
    }

    /// 拿到文件内容后：先 dry-run 出预览再开对话框。文件本身不合法时直接 toast，不开框。
    pub fn begin_import(&self, content: String) {
        *self.dlg.import_content.borrow_mut() = content;
        if self.dlg.import_mode.get() != 0 {
            self.dlg.import_mode.set(0);
        }
        if self.refresh_import_preview() {
            self.dlg.import_visible.set(true);
        }
    }

    fn import_params(&self, dry_run: bool) -> Value {
        let mode = IMPORT_MODES
            .get(self.dlg.import_mode.get())
            .map_or("merge", |m| m.1);
        json!({
            "content": *self.dlg.import_content.borrow(),
            "mode": mode,
            "dryRun": dry_run,
        })
    }

    /// 按当前方式重算预览。失败返回 false（已 toast）。
    pub fn refresh_import_preview(&self) -> bool {
        let replace = self.dlg.import_mode.get() == 1;
        match rpc::call_core("compat.import", &self.import_params(true))
            .and_then(|v| ImportPreview::parse(&v))
        {
            Ok(p) => {
                self.dlg.import_can_apply.set(p.changes() > 0);
                self.dlg.import_summary.set(summarize(&p, replace));
                true
            }
            Err(e) => {
                // 预览与将要执行的必须是同一件事：重算失败就不能再留着旧预览让人点「导入」。
                self.dlg.import_can_apply.set(false);
                self.dlg
                    .import_summary
                    .set(format!("预览失败，无法导入：{e}"));
                self.notify(ToastKind::Error, format!("无法导入: {e}"));
                false
            }
        }
    }

    pub fn confirm_import(&self) {
        match rpc::call_core("compat.import", &self.import_params(false))
            .and_then(|v| ImportPreview::parse(&v))
        {
            Ok(p) => {
                self.dlg.import_visible.set(false);
                self.dlg.import_content.borrow_mut().clear();
                self.reload();
                let tail = match p.backup.as_deref() {
                    Some(b) if !b.is_empty() => format!("，原文件已备份为 {b}"),
                    _ => String::new(),
                };
                self.notify(
                    ToastKind::Success,
                    format!("已导入 {} 条{tail}", p.changes()),
                );
            }
            Err(e) => self.notify(ToastKind::Error, format!("导入失败: {e}")),
        }
    }

    pub fn cancel_import(&self) {
        self.dlg.import_visible.set(false);
        self.dlg.import_content.borrow_mut().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockGuard(std::sync::MutexGuard<'static, ()>);
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

    fn calls(name: &str) -> Vec<Value> {
        crate::rpc::take_mock_calls()
            .into_iter()
            .filter(|(m, _)| m == name)
            .map(|(_, p)| p)
            .collect()
    }

    fn loaded() -> CompatState {
        let st = CompatState::new();
        st.ensure_loaded();
        st
    }

    #[test]
    fn add_rejects_bad_names_and_keeps_the_dialog_open() {
        let _g = MockGuard::new();
        let st = loaded();
        st.open_add();
        st.dlg.add_name.set("  ".into());
        st.confirm_add();
        assert!(st.dlg.add_visible.get(), "校验失败时对话框不关");
        assert!(!st.dlg.add_error.get().is_empty());
        st.dlg.add_name.set("a\\b.exe".into());
        st.confirm_add();
        assert!(st.dlg.add_error.get().contains("路径"));
    }

    #[test]
    fn add_creates_a_draft_without_touching_disk() {
        let _g = MockGuard::new();
        let st = loaded();
        let _ = crate::rpc::take_mock_calls();
        st.open_add();
        st.dlg.add_name.set(" Brand.exe ".into());
        st.confirm_add();
        assert!(!st.dlg.add_visible.get());
        assert_eq!(st.draft.get(), "Brand.exe", "名字两端空白被去掉");
        assert_eq!(st.selected.get(), "Brand.exe");
        assert!(
            calls("compat.upsert").is_empty(),
            "草稿在第一次设置字段前不落盘"
        );
    }

    /// 切换合并 / 替换后重算预览失败：不能留着旧预览和可点的「导入」（会按新模式执行）。
    #[test]
    fn a_failed_preview_refresh_disables_apply_instead_of_keeping_the_stale_preview() {
        let _g = MockGuard::new();
        let st = loaded();
        st.begin_import("[[apps]]\nprocess = \"Brand.exe\"\n".into());
        assert!(st.dlg.import_can_apply.get());
        *st.dlg.import_content.borrow_mut() = format!("x {}", crate::rpc::MOCK_FAIL_TEXT);
        st.dlg.import_mode.set(1);
        assert!(!st.refresh_import_preview());
        assert!(!st.dlg.import_can_apply.get());
        assert!(st.dlg.import_summary.get().contains("预览失败"));
    }

    #[test]
    fn import_previews_first_then_applies_the_same_content_with_the_chosen_mode() {
        let _g = MockGuard::new();
        let st = loaded();
        let _ = crate::rpc::take_mock_calls();
        st.begin_import("[[apps]]\nprocess = \"Brand.exe\"\n".into());
        assert!(st.dlg.import_visible.get());
        assert!(st.dlg.import_can_apply.get());
        assert!(st.dlg.import_summary.get().contains("新增 1 条"));
        let first = calls("compat.import");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0]["dryRun"], true, "打开对话框只预览");
        assert_eq!(first[0]["mode"], "merge");

        st.dlg.import_mode.set(1);
        assert!(st.refresh_import_preview());
        assert!(st.dlg.import_summary.get().contains("替换模式"));
        let _ = crate::rpc::take_mock_calls();

        st.confirm_import();
        let applied = calls("compat.import");
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0]["dryRun"], false);
        assert_eq!(applied[0]["mode"], "replace");
        assert_eq!(
            applied[0]["content"], "[[apps]]\nprocess = \"Brand.exe\"\n",
            "确认导入的必须是预览过的那份内容"
        );
        assert!(!st.dlg.import_visible.get());
    }

    #[test]
    fn reset_all_confirm_closes_and_calls_the_core() {
        let _g = MockGuard::new();
        let st = loaded();
        let _ = crate::rpc::take_mock_calls();
        st.dlg.reset_visible.set(true);
        st.confirm_reset_all();
        assert!(!st.dlg.reset_visible.get());
        assert_eq!(calls("compat.resetAll").len(), 1);
    }

    #[test]
    fn summary_lists_names_and_flags() {
        let p = ImportPreview::parse(&json!({
            "items": [
                {"section": "apps", "process": "A.exe", "action": "add"},
                {"section": "apps", "process": "B.exe", "action": "override"},
                {"section": "apps", "process": "C.exe", "action": "unchanged"}
            ],
            "ignoredKeys": ["apps.future"],
            "fallbacks": ["\"wiat\""],
            "rejected": ["坏条目"]
        }))
        .unwrap();
        let s = summarize(&p, false);
        assert!(s.contains("新增 1 条 · 覆盖 1 条"));
        assert!(s.contains("新增：A.exe") && s.contains("覆盖：B.exe"));
        assert!(!s.contains("C.exe"), "无变化的不逐条列");
        assert!(s.contains("apps.future") && s.contains("wiat") && s.contains("坏条目"));
        assert!(!s.contains("替换模式"));
        assert!(summarize(&p, true).contains("替换模式"));
    }
}
