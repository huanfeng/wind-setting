//! 「应用兼容」页的状态与写操作。
//!
//! **写路径没有「保存」环节**：每个动作立即发一条 `compat.*`，成功后整体重拉 `compat.list`，
//! 界面永远画核心返回的那份，不在本地攒一份「待提交」的副本。这样「核心的合并规则」只有核心
//! 一份实现，界面无从与它分叉——包括「与出厂一致的值会被当成冗余删掉」这类界面很难自己预判的事。
//!
//! 详情区在所选规则的视图**变化时重建**（而不是原地回填各控件的信号）：新建的控件以新值为基线，
//! 不会把「刚从核心读回来的值」误当成用户改动再写一遍，写回循环因此无从发生。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

use super::model::{ListResult, Problem, RuleState, RuleView, Schema, Section};
use crate::pages::dict::reactive::Feedback;
use crate::rpc;

/// 左栏的状态筛选，下标对应分段控件的项。
pub const FILTERS: [&str; 4] = ["全部", "已修改", "自定义", "已禁用"];

#[derive(Clone)]
pub struct CompatState {
    /// 字段元数据（`compat.schema`）；首次加载前为空。
    pub schema: Signal<Schema>,
    /// 是否已发起过加载——切页只拉一次，来回切换不重复请求（刷新走 [`Self::reload`]）。
    loaded: Rc<Cell<bool>>,
    /// 当前段（下标对应 [`Section::ALL`]）。
    pub section: Signal<usize>,
    /// 状态筛选（下标对应 [`FILTERS`]）。
    pub filter: Signal<usize>,
    /// 左栏搜索词（按进程名子串、不区分大小写）。
    pub search: Signal<String>,
    /// 当前段的全部规则（核心返回的原样）。
    pub rules: Signal<Vec<RuleView>>,
    pub warnings: Signal<Vec<Problem>>,
    /// 选中的进程名；空串 = 没有选中。
    pub selected: Signal<String>,
    /// 非空 = `compat.list` / `compat.schema` 失败（用户层损坏、核心不在线…），页面显示横幅。
    pub load_error: Signal<String>,
    /// 左栏列表需要重建（规则集 / 筛选 / 搜索 / 段 / 草稿变了）。
    pub list_epoch: Signal<u64>,
    /// 右栏详情需要重建（选中项变了、或选中项的视图变了）。
    pub detail_epoch: Signal<u64>,
    /// 外部触发的刷新请求（窗口重获焦点、核心广播 `compat.changed`）：页面里有个 effect 监听它。
    pub refresh_tick: Signal<u64>,
    /// 新建规则的草稿进程名；空串 = 没有草稿。草稿在第一次设置字段之前不落盘。
    pub draft: Signal<String>,
    /// 详情里是否列出「高级」字段（`advanced`）：默认只列常用项，已被设置过的高级字段
    /// 无论如何都会列出，免得用户看不到自己改过什么。
    pub show_advanced: Signal<bool>,
    /// 「已选来覆盖、但还没给出值」的字段 `(进程名, 字段键)`。
    ///
    /// 详情只列被设置过的字段，用户通过「添加覆盖项」挑字段；文本 / 数字这类字段选出来的
    /// 那一刻还没有值可写（空串、0 会被核心当成没写），所以先在界面里占一行，设置了值才落盘。
    /// 不进核心、不跨段（切段即清）。
    pending: Rc<RefCell<Vec<(String, String)>>>,
    /// 新增 / 全部还原 / 导入的对话框状态（见 `actions.rs`）。
    pub dlg: super::actions::Dialogs,
    pub feedback: Feedback,
}

impl Default for CompatState {
    fn default() -> Self {
        Self::new()
    }
}

impl CompatState {
    pub fn new() -> Self {
        Self {
            schema: signal(Schema::default()),
            loaded: Rc::new(Cell::new(false)),
            section: signal(0),
            filter: signal(0),
            search: signal(String::new()),
            rules: signal(Vec::new()),
            warnings: signal(Vec::new()),
            selected: signal(String::new()),
            load_error: signal(String::new()),
            list_epoch: signal(0),
            detail_epoch: signal(0),
            refresh_tick: signal(0),
            draft: signal(String::new()),
            show_advanced: signal(false),
            pending: Rc::new(RefCell::new(Vec::new())),
            dlg: super::actions::Dialogs::default(),
            feedback: signal(None),
        }
    }

    pub fn current_section(&self) -> Section {
        Section::from_index(self.section.get())
    }

    pub fn notify(&self, kind: ToastKind, msg: impl Into<String>) {
        self.feedback.set(Some((kind, msg.into())));
    }

    fn bump(sig: Signal<u64>) {
        sig.update(|n| *n = n.wrapping_add(1));
    }

    /// 请求一次刷新（窗口重获焦点 / 核心广播 `compat.changed` 时调用）。
    /// 只在已经加载过时才有意义：从没进过本页就不必去拉。
    pub fn request_refresh(&self) {
        if self.loaded.get() {
            Self::bump(self.refresh_tick);
        }
    }

    /// 首次进入本页时拉一次。
    pub fn ensure_loaded(&self) {
        if self.loaded.replace(true) {
            return;
        }
        self.reload();
    }

    /// 重新拉取元数据（首次）与当前段的规则集。
    ///
    /// 同步调用：`compat.*` 是本地文件读写，毫秒级，与本仓其它页一致。失败时不清空已有内容，
    /// 只挂横幅——瞬时的核心不可用不该让界面清空。
    pub fn reload(&self) {
        self.loaded.set(true);
        if self.schema.with(|s| s.fields.is_empty()) {
            match rpc::call_core("compat.schema", &json!({})).and_then(|v| Schema::parse(&v)) {
                Ok(s) => self.schema.set(s),
                Err(e) => {
                    self.load_error.set(e);
                    return;
                }
            }
        }
        let sec = self.current_section();
        match rpc::call_core("compat.list", &json!({ "section": sec.id() }))
            .and_then(|v| ListResult::parse(&v))
        {
            Ok(l) => {
                self.load_error.set(String::new());
                self.apply_list(l);
            }
            Err(e) => self.load_error.set(e),
        }
    }

    /// 应用一份新的规则集。**只有内容真的变了才动信号**：核心在我们自己写完之后也会广播
    /// `compat.changed`，收到后的那次重拉与当前内容一致，此时不能重建界面（会丢焦点）。
    fn apply_list(&self, l: ListResult) {
        let same_rules = self.rules.with(|r| *r == l.rules);
        let same_warn = self.warnings.with(|w| *w == l.warnings);
        if same_rules && same_warn {
            return;
        }
        let old_sel = self.selected_rule();
        self.rules.set(l.rules);
        self.warnings.set(l.warnings);
        self.fix_selection();
        Self::bump(self.list_epoch);
        if old_sel != self.selected_rule() {
            Self::bump(self.detail_epoch);
        }
    }

    /// 选中项保持合法：不在（筛选后的）可见集里就落到第一条；草稿名单独放行。
    fn fix_selection(&self) {
        let cur = self.selected.get();
        let visible = self.visible_rules();
        let draft = self.draft.get();
        let ok = !cur.is_empty() && (visible.iter().any(|r| r.process == cur) || cur == draft);
        if !ok {
            let next = visible
                .first()
                .map(|r| r.process.clone())
                .unwrap_or_default();
            if next != cur {
                self.selected.set(next);
            }
        }
    }

    /// 按筛选与搜索词过滤后的规则，草稿（有的话）置顶。
    pub fn visible_rules(&self) -> Vec<RuleView> {
        let filter = self.filter.get();
        let needle = self.search.get().trim().to_lowercase();
        let mut out: Vec<RuleView> = self.rules.with(|rules| {
            rules
                .iter()
                .filter(|r| match filter {
                    1 => r.state == RuleState::Modified,
                    2 => r.state == RuleState::User,
                    3 => r.state == RuleState::Disabled,
                    _ => true,
                })
                .filter(|r| needle.is_empty() || r.process.to_lowercase().contains(&needle))
                .cloned()
                .collect()
        });
        let draft = self.draft.get();
        if !draft.is_empty()
            && !out.iter().any(|r| r.process.eq_ignore_ascii_case(&draft))
            && (needle.is_empty() || draft.to_lowercase().contains(&needle))
        {
            out.insert(
                0,
                self.schema
                    .with(|s| RuleView::draft(&draft, s, self.current_section())),
            );
        }
        out
    }

    /// 当前选中的规则视图；草稿会合成一份「全部字段未设置」的视图。
    pub fn selected_rule(&self) -> Option<RuleView> {
        let cur = self.selected.get();
        if cur.is_empty() {
            return None;
        }
        if let Some(r) = self
            .rules
            .with(|rules| rules.iter().find(|r| r.process == cur).cloned())
        {
            return Some(r);
        }
        (cur == self.draft.get()).then(|| {
            self.schema
                .with(|s| RuleView::draft(&cur, s, self.current_section()))
        })
    }

    /// 选中一条规则。
    pub fn select(&self, process: &str) {
        if self.selected.get() != process {
            self.selected.set(process.to_string());
            Self::bump(self.detail_epoch);
            Self::bump(self.list_epoch);
        }
    }

    /// 切段：清掉搜索与草稿之外的一切，重拉。
    pub fn switch_section(&self) {
        self.selected.set(String::new());
        self.draft.set(String::new());
        self.pending.borrow_mut().clear();
        self.rules.set(Vec::new());
        self.reload();
        self.fix_selection();
        Self::bump(self.list_epoch);
        Self::bump(self.detail_epoch);
    }

    /// 筛选 / 搜索变了：重排左栏，选中项落到可见集里。
    pub fn refilter(&self) {
        let before = self.selected.get();
        self.fix_selection();
        Self::bump(self.list_epoch);
        if before != self.selected.get() {
            Self::bump(self.detail_epoch);
        }
    }

    // ───────────── 写操作 ─────────────
    //
    // 每个都返回是否成功；失败已经 toast 了核心给的中文报错（补丁被拒、用户层读不进来…）。
    // 成功后重拉，界面据此更新。

    fn call_write(&self, method: &str, mut params: Value) -> Result<Value, ()> {
        params["section"] = json!(self.current_section().id());
        match rpc::call_core(method, &params) {
            Ok(v) => Ok(v),
            Err(e) => {
                self.notify(ToastKind::Error, e);
                Err(())
            }
        }
    }

    /// 设置一个字段。草稿的第一次写入让它成为真正的规则。
    pub fn set_field(&self, process: &str, key: &str, value: Value) -> bool {
        self.upsert(process, json!({ key: value }))
    }

    /// 清除一个字段（回到「跟随全局」；出厂设了它时核心会写 `unset`）。
    pub fn clear_field(&self, process: &str, key: &str) -> bool {
        self.upsert(process, json!({ key: Value::Null }))
    }

    fn upsert(&self, process: &str, patch: Value) -> bool {
        if self
            .call_write(
                "compat.upsert",
                json!({ "process": process, "patch": patch }),
            )
            .is_err()
        {
            self.rebuild_after_failure();
            return false;
        }
        // 给出了值的字段不再是「待设置」；清除（null）保留待设置状态——用户刚挑出这一项，
        // 选回「跟随全局」不该让这一行凭空消失。
        if let Some(obj) = patch_keys(&patch) {
            self.pending
                .borrow_mut()
                .retain(|(p, k)| !(p.eq_ignore_ascii_case(process) && obj.contains(k)));
        }
        self.reload_after_write(true);
        true
    }

    /// 还原为出厂：删掉用户层关于这个字段的一切痕迹。
    pub fn reset_field(&self, process: &str, key: &str) -> bool {
        self.simple_write(
            "compat.resetField",
            json!({ "process": process, "key": key }),
            true,
        )
    }

    pub fn set_disabled(&self, process: &str, disabled: bool) -> bool {
        self.simple_write(
            "compat.setDisabled",
            json!({ "process": process, "disabled": disabled }),
            false,
        )
    }

    /// 还原单条：删掉用户层同名条目（用户新增的规则等于删除）。
    pub fn reset_rule(&self, process: &str) -> bool {
        let ok = self.simple_write("compat.reset", json!({ "process": process }), false);
        if ok {
            self.clear_pending_of(process);
        }
        ok
    }

    /// 丢掉某个进程的全部「待设置」占位（规则被删 / 草稿被丢时）。
    fn clear_pending_of(&self, process: &str) {
        self.pending
            .borrow_mut()
            .retain(|(p, _)| !p.eq_ignore_ascii_case(process));
    }

    fn has_pending(&self, process: &str) -> bool {
        self.pending
            .borrow()
            .iter()
            .any(|(p, _)| p.eq_ignore_ascii_case(process))
    }

    fn simple_write(&self, method: &str, params: Value, prompt_if_emptied: bool) -> bool {
        if self.call_write(method, params).is_err() {
            self.rebuild_after_failure();
            return false;
        }
        self.reload_after_write(prompt_if_emptied);
        true
    }

    /// 全部还原（核心会先备份）。返回备份文件名。
    pub fn reset_all(&self) -> Option<String> {
        let v = rpc::call_core("compat.resetAll", &json!({}))
            .map_err(|e| self.notify(ToastKind::Error, e))
            .ok()?;
        self.selected.set(String::new());
        self.draft.set(String::new());
        self.pending.borrow_mut().clear();
        self.reload_after_write(false);
        // 用户层本来就是空时规则集不变，`apply_list` 不会动任何信号；选中 / 草稿却已被清掉。
        Self::bump(self.list_epoch);
        Self::bump(self.detail_epoch);
        Some(
            v.get("backup")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        )
    }

    /// 写后重拉。`prompt_if_emptied`：选中的用户新增规则因这次写入变得一个字段都不剩
    /// （核心里整条消失）时，不直接让它凭空没了，而是保留成空白草稿并弹窗问「删除还是保留」。
    fn reload_after_write(&self, prompt_if_emptied: bool) {
        let before = (self.selected.get(), self.draft.get());
        let sel = before.0.clone();
        let existed = !sel.is_empty() && self.rules.with(|r| r.iter().any(|x| x.process == sel));
        self.reload();
        let gone = existed && !self.rules.with(|r| r.iter().any(|x| x.process == sel));
        if gone && prompt_if_emptied {
            // 草稿只有一个名字：顶掉另一条未保存的草稿时把它的占位一并清掉。
            let old = self.draft.get();
            if !old.is_empty() && old != sel {
                self.clear_pending_of(&old);
            }
            // 还有等着给值的占位时，规则并没有真的「清空」——留着草稿继续设值，不必问。
            let pending = self.has_pending(&sel);
            self.draft.set(sel.clone());
            self.selected.set(sel.clone());
            if !pending {
                self.dlg.empty_name.set(sel);
                self.dlg.empty_visible.set(true);
            }
        }
        // 草稿只在核心里确实有了这条规则之后才算转正：第一次写入被 normalize 成空操作时
        // 核心不落行，草稿要留着，否则界面停在一条既不是草稿也不存在的规则上。
        let d = self.draft.get();
        if !d.is_empty()
            && self
                .rules
                .with(|r| r.iter().any(|x| x.process.eq_ignore_ascii_case(&d)))
        {
            self.draft.set(String::new());
        }
        // 选中项的视图变了（或被删掉了）：详情必须跟上。`apply_list` 只在内容变化时才动信号，
        // 这里再确保选中项合法；选中 / 草稿有变化而规则集没变时要自己通知界面。
        self.fix_selection();
        if gone || before != (self.selected.get(), self.draft.get()) {
            Self::bump(self.list_epoch);
            Self::bump(self.detail_epoch);
        }
    }

    // ───────────── 覆盖项（详情只列被设置过的字段） ─────────────

    /// 这个字段是不是「已挑出来、等着给值」。
    pub fn is_pending(&self, process: &str, key: &str) -> bool {
        self.pending
            .borrow()
            .iter()
            .any(|(p, k)| p.eq_ignore_ascii_case(process) && k == key)
    }

    /// 在「添加覆盖项」里挑了一个字段。开关类直接写 `true`（没有比它更自然的初值，
    /// 而写「关」等于没写）；其余先占一行等用户给值。
    pub fn add_override(&self, process: &str, key: &str) {
        let section = self.current_section();
        let meta = self.schema.with(|s| s.field(section, key).cloned());
        let Some(meta) = meta else {
            return;
        };
        // 依赖别的字段的选项：被依赖的字段要一并挑出来（否则这一行出现即置灰、给不了值）。
        // 依赖没满足时，开关也不能直接写 `true`——写进去也不生效。
        let mut dep_pending = false;
        if let (Some(d), Some(rule)) = (&meta.depends_on, self.selected_rule())
            && !meta.dep_met(&rule)
        {
            dep_pending = true;
            let unset = rule
                .fields
                .get(&d.key)
                .is_none_or(|v| v.source == super::model::FieldSource::Default);
            if unset && !self.is_pending(process, &d.key) {
                self.pending
                    .borrow_mut()
                    .push((process.to_string(), d.key.clone()));
            }
        }
        if meta.kind == super::model::Kind::Bool && !dep_pending {
            self.set_field(process, key, json!(true));
            return;
        }
        if !self.is_pending(process, key) {
            self.pending
                .borrow_mut()
                .push((process.to_string(), key.to_string()));
        }
        Self::bump(self.detail_epoch);
    }

    /// 去掉一个覆盖项：待设置的直接丢，已设置的清成「跟随全局」。
    pub fn remove_override(&self, process: &str, key: &str) -> bool {
        if self.is_pending(process, key) {
            self.pending
                .borrow_mut()
                .retain(|(p, k)| !(p.eq_ignore_ascii_case(process) && k == key));
            Self::bump(self.detail_epoch);
            return true;
        }
        self.clear_field(process, key)
    }

    /// 空规则弹窗里选「删除」：草稿丢掉即可，核心里早已没有这条。
    pub fn confirm_delete_emptied(&self) {
        self.dlg.empty_visible.set(false);
        let name = self.dlg.empty_name.get();
        self.clear_pending_of(&name);
        self.drop_draft();
    }

    /// 写入失败：控件已经停在用户刚选的值上，而核心里没变。按核心的值重建详情，
    /// 界面才永远画核心返回的那份。
    fn rebuild_after_failure(&self) {
        Self::bump(self.detail_epoch);
    }

    /// 窗口（重新）打开时：没加载过就首次加载，加载过则按最新内容重拉一次。
    /// 窗口关闭期间到达的 `compat.changed` 没有页面接收，重开的窗口要自己补这一课。
    pub fn open_refresh(&self) {
        // 状态比窗口长命：上一次关窗时可能留着「待设置」占位和开着的对话框，重开不该原样冒出来。
        self.pending.borrow_mut().clear();
        self.dlg.field_visible.set(false);
        self.dlg.empty_visible.set(false);
        if self.loaded.get() {
            self.reload();
        } else {
            self.ensure_loaded();
        }
    }

    // ───────────── 草稿 ─────────────

    /// 新建一条规则的草稿：先在界面里出现，第一次设置字段时才 `compat.upsert` 落盘。
    /// 同名规则已存在则直接跳到它。返回是否新建了草稿。
    pub fn start_draft(&self, process: &str) -> Result<bool, String> {
        let name = process.trim();
        if name.is_empty() {
            return Err("进程名不能为空".into());
        }
        if name.contains('/') || name.contains('\\') {
            return Err("进程名不能含路径分隔符".into());
        }
        if name.chars().any(char::is_control) {
            return Err("进程名不能含控制字符".into());
        }
        let existing = self.rules.with(|rs| {
            rs.iter()
                .find(|r| r.process.eq_ignore_ascii_case(name))
                .map(|r| r.process.clone())
        });
        if let Some(p) = existing {
            self.search.set(String::new());
            self.filter.set(0);
            self.select(&p);
            return Ok(false);
        }
        self.search.set(String::new());
        self.filter.set(0);
        self.draft.set(name.to_string());
        self.select(name);
        Self::bump(self.list_epoch);
        Self::bump(self.detail_epoch);
        Ok(true)
    }

    /// 丢弃草稿（没设置过任何字段就离开）。
    pub fn drop_draft(&self) {
        if self.draft.get().is_empty() {
            return;
        }
        self.clear_pending_of(&self.draft.get());
        let was_selected = self.selected.get() == self.draft.get();
        self.draft.set(String::new());
        if was_selected {
            self.selected.set(String::new());
        }
        self.fix_selection();
        Self::bump(self.list_epoch);
        Self::bump(self.detail_epoch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mock 模式 + 全 crate 唯一的锁（见 `rpc::ENV_LOCK`）；测完摘掉 mock 标记。
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

    fn loaded() -> CompatState {
        let st = CompatState::new();
        st.ensure_loaded();
        st
    }

    fn calls(name: &str) -> Vec<Value> {
        crate::rpc::take_mock_calls()
            .into_iter()
            .filter(|(m, _)| m == name)
            .map(|(_, p)| p)
            .collect()
    }

    #[test]
    fn first_load_fetches_schema_and_the_current_section_and_selects_the_first_rule() {
        let _g = MockGuard::new();
        let st = loaded();
        assert!(!st.schema.with(|s| s.fields.is_empty()), "元数据应已加载");
        assert!(st.rules.with(|r| !r.is_empty()));
        assert_eq!(st.load_error.get(), "");
        let first = st.visible_rules()[0].process.clone();
        assert_eq!(st.selected.get(), first, "默认选中第一条");
        assert!(st.selected_rule().is_some());
        let all = crate::rpc::take_mock_calls();
        assert!(
            all.iter().any(|(m, _)| m == "compat.schema")
                && all
                    .iter()
                    .any(|(m, p)| m == "compat.list" && p["section"] == "apps")
        );
    }

    #[test]
    fn ensure_loaded_is_lazy_and_only_once_but_reload_always_fetches() {
        let _g = MockGuard::new();
        let st = CompatState::new();
        st.request_refresh();
        assert_eq!(st.refresh_tick.get(), 0, "没进过本页就不必响应刷新请求");
        st.ensure_loaded();
        let _ = crate::rpc::take_mock_calls();
        st.ensure_loaded();
        assert!(
            crate::rpc::take_mock_calls().is_empty(),
            "第二次进页不重复请求"
        );
        st.request_refresh();
        assert_eq!(st.refresh_tick.get(), 1, "加载过之后才响应刷新请求");
        st.reload();
        assert_eq!(calls("compat.list").len(), 1, "reload 每次都拉");
        assert!(calls("compat.schema").is_empty(), "元数据只拉一次");
    }

    /// 核心在我们自己写完之后也会广播 `compat.changed`，那次重拉内容没变，不能重建界面（会丢焦点）。
    #[test]
    fn reloading_identical_data_does_not_touch_any_epoch() {
        let _g = MockGuard::new();
        let st = loaded();
        let (l, d) = (st.list_epoch.get(), st.detail_epoch.get());
        st.reload();
        st.reload();
        assert_eq!(
            (st.list_epoch.get(), st.detail_epoch.get()),
            (l, d),
            "内容没变就不动信号"
        );
    }

    #[test]
    fn filter_and_search_narrow_the_visible_rules_and_keep_the_selection_legal() {
        let _g = MockGuard::new();
        let st = loaded();
        let all = st.visible_rules().len();
        st.filter.set(3);
        st.refilter();
        let disabled = st.visible_rules();
        assert!(!disabled.is_empty() && disabled.len() < all);
        assert!(disabled.iter().all(|r| r.state == RuleState::Disabled));
        assert!(
            disabled.iter().any(|r| r.process == st.selected.get()),
            "选中项必须落在可见集里"
        );
        st.filter.set(0);
        st.search.set("WEIXIN".into());
        st.refilter();
        let hit = st.visible_rules();
        assert_eq!(hit.len(), 1, "搜索不区分大小写");
        assert_eq!(st.selected.get(), hit[0].process);
        st.search.set("zzz-nothing".into());
        st.refilter();
        assert!(st.visible_rules().is_empty());
        assert_eq!(st.selected.get(), "", "没有可见项时不选中任何一条");
        assert!(st.selected_rule().is_none());
    }

    #[test]
    fn switching_section_reloads_that_section_and_resets_selection_and_draft() {
        let _g = MockGuard::new();
        let st = loaded();
        st.draft.set("x.exe".into());
        st.section.set(2);
        let _ = crate::rpc::take_mock_calls();
        st.switch_section();
        assert_eq!(st.draft.get(), "");
        let l = calls("compat.list");
        assert_eq!(l.len(), 1);
        assert_eq!(l[0]["section"], "commit_newline");
        assert!(
            st.rules
                .with(|r| r.iter().any(|x| x.process == "WINWORD.EXE"))
        );
    }

    // ───────────── 写操作：发出去的请求形状 ─────────────

    #[test]
    fn set_and_clear_field_send_the_expected_upsert_patches() {
        let _g = MockGuard::new();
        let st = loaded();
        let _ = crate::rpc::take_mock_calls();
        assert!(st.set_field("Weixin.exe", "first_show_mode", json!("wait")));
        assert!(st.clear_field("Weixin.exe", "first_show_mode"));
        assert!(st.set_field("Weixin.exe", "caret_use_top", json!(false)));
        let u = calls("compat.upsert");
        assert_eq!(u.len(), 3);
        assert_eq!(
            u[0],
            json!({"section": "apps", "process": "Weixin.exe", "patch": {"first_show_mode": "wait"}})
        );
        assert_eq!(
            u[1]["patch"],
            json!({"first_show_mode": null}),
            "清除 = 补丁值为 null"
        );
        assert_eq!(
            u[2]["patch"],
            json!({"caret_use_top": false}),
            "显式 false 必须原样发出去"
        );
    }

    #[test]
    fn reset_disable_and_delete_send_the_expected_requests() {
        let _g = MockGuard::new();
        let st = loaded();
        let _ = crate::rpc::take_mock_calls();
        assert!(st.reset_field("Weixin.exe", "stale_probe_guard"));
        assert!(st.set_disabled("Weixin.exe", true));
        assert!(st.reset_rule("Tabby.exe"));
        let all = crate::rpc::take_mock_calls();
        let find = |m: &str| {
            all.iter()
                .find(|(n, _)| n == m)
                .map(|(_, p)| p.clone())
                .unwrap_or(Value::Null)
        };
        assert_eq!(
            find("compat.resetField"),
            json!({"section": "apps", "process": "Weixin.exe", "key": "stale_probe_guard"})
        );
        assert_eq!(
            find("compat.setDisabled"),
            json!({"section": "apps", "process": "Weixin.exe", "disabled": true})
        );
        assert_eq!(
            find("compat.reset"),
            json!({"section": "apps", "process": "Tabby.exe"})
        );
        assert_eq!(
            all.iter().filter(|(m, _)| m == "compat.list").count(),
            3,
            "每次写成功后都重拉一次"
        );
    }

    #[test]
    fn a_failed_write_toasts_the_core_message_and_does_not_reload() {
        let _g = MockGuard::new();
        let st = loaded();
        let _ = crate::rpc::take_mock_calls();
        assert!(!st.set_field(
            crate::rpc::MOCK_COMPAT_FAIL_PROCESS,
            "first_show_mode",
            json!("bogus")
        ));
        let fb = st.feedback.get().expect("失败要有反馈");
        assert!(matches!(fb.0, ToastKind::Error));
        assert!(
            fb.1.contains("first_show_mode"),
            "toast 的是核心给的中文报错: {}",
            fb.1
        );
        assert!(calls("compat.list").is_empty(), "失败不重拉");
    }

    #[test]
    fn reset_all_returns_the_backup_name_and_clears_selection() {
        let _g = MockGuard::new();
        let st = loaded();
        let backup = st.reset_all().expect("mock 的 resetAll 成功");
        assert!(backup.starts_with("compat.toml.bak-"), "{backup}");
        assert_eq!(calls("compat.resetAll").len(), 1);
    }

    // ───────────── 草稿 ─────────────

    #[test]
    fn a_draft_shows_up_selected_and_is_not_written_until_a_field_is_set() {
        let _g = MockGuard::new();
        let st = loaded();
        let _ = crate::rpc::take_mock_calls();
        assert_eq!(st.start_draft("  New.exe ").unwrap(), true);
        assert_eq!(st.draft.get(), "New.exe", "进程名 trim");
        assert_eq!(st.selected.get(), "New.exe");
        let vis = st.visible_rules();
        assert_eq!(vis[0].process, "New.exe", "草稿置顶");
        assert!(vis[0].is_draft());
        assert!(st.selected_rule().unwrap().is_draft());
        assert!(
            crate::rpc::take_mock_calls()
                .iter()
                .all(|(m, _)| !m.starts_with("compat.upsert")),
            "还没设置字段就不落盘"
        );
        assert!(st.set_field("New.exe", "auto_pair", json!(true)));
        assert_eq!(st.draft.get(), "", "第一次设置字段后草稿转正");
        assert_eq!(calls("compat.upsert").len(), 1);
        assert_eq!(st.selected.get(), "New.exe", "转正后仍选中它");
        assert!(
            !st.selected_rule().unwrap().is_draft(),
            "详情画的是核心返回的真规则"
        );
    }

    /// 第一次写入被核心 normalize 成空操作（不落行）：草稿要留着，界面不能停在一条不存在的规则上。
    #[test]
    fn a_noop_first_write_keeps_the_draft_and_the_ui_consistent() {
        let _g = MockGuard::new();
        let st = loaded();
        let name = super::super::mock::MOCK_NOOP_PROCESS;
        st.start_draft(name).unwrap();
        assert!(st.set_field(name, "auto_pair", json!(true)));
        assert_eq!(st.draft.get(), name, "核心没建出这条规则，草稿不能转正");
        assert_eq!(st.selected.get(), name);
        assert!(st.selected_rule().unwrap().is_draft());
        assert!(st.visible_rules().iter().any(|r| r.process == name));
    }

    /// 「添加覆盖项」：开关直接写 true，其余先占一行等值；给了值就不再是待设置。
    #[test]
    fn add_override_writes_bools_at_once_and_parks_other_kinds_until_given_a_value() {
        let _g = MockGuard::new();
        let st = loaded();
        st.select("Weixin.exe");
        let _ = crate::rpc::take_mock_calls();

        st.add_override("Weixin.exe", "host_render");
        let w = calls("compat.upsert");
        assert_eq!(w.len(), 1);
        assert_eq!(w[0]["patch"]["host_render"], json!(true));

        st.add_override("Weixin.exe", "initial_mode");
        assert!(
            calls("compat.upsert").is_empty(),
            "非开关字段挑出来时没有值可写"
        );
        assert!(st.is_pending("weixin.EXE", "initial_mode"));

        st.set_field("Weixin.exe", "initial_mode", json!("english"));
        assert!(!st.is_pending("Weixin.exe", "initial_mode"), "给了值就转正");
    }

    /// 待设置的字段被「移除」只是丢掉占位，不发请求；选回「跟随全局」也不让占位消失。
    #[test]
    fn removing_or_clearing_a_pending_field_behaves() {
        let _g = MockGuard::new();
        let st = loaded();
        st.select("Weixin.exe");
        st.add_override("Weixin.exe", "initial_mode");
        let _ = crate::rpc::take_mock_calls();
        st.clear_field("Weixin.exe", "initial_mode");
        assert!(st.is_pending("Weixin.exe", "initial_mode"));
        let _ = crate::rpc::take_mock_calls();
        assert!(st.remove_override("Weixin.exe", "initial_mode"));
        assert!(calls("compat.upsert").is_empty());
        assert!(!st.is_pending("Weixin.exe", "initial_mode"));
    }

    /// 用户新增的规则被去掉最后一项：不直接凭空消失，保留成空白草稿并请用户决定。
    #[test]
    fn emptying_a_user_rule_keeps_a_blank_draft_and_asks_before_it_is_gone() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft("Brand.exe").unwrap();
        assert!(st.set_field("Brand.exe", "composition_start_pair_guard", json!(true)));
        assert_eq!(st.draft.get(), "", "写入后规则转正");
        assert!(!st.dlg.empty_visible.get());

        assert!(st.reset_field("Brand.exe", "composition_start_pair_guard"));
        assert!(st.dlg.empty_visible.get(), "规则被清空要先问一句");
        assert_eq!(st.dlg.empty_name.get(), "Brand.exe");
        assert_eq!(
            st.draft.get(),
            "Brand.exe",
            "保留成可继续添加覆盖项的空白规则"
        );
        assert_eq!(st.selected.get(), "Brand.exe");
        assert!(st.selected_rule().unwrap().is_draft());

        st.confirm_delete_emptied();
        assert!(!st.dlg.empty_visible.get());
        assert_eq!(st.draft.get(), "", "选删除就丢掉");
        assert_ne!(st.selected.get(), "Brand.exe");
    }

    /// 去掉最后一项的另一条路（界面上新增规则的按钮叫「移除」，走 upsert null）同样要问。
    #[test]
    fn removing_the_last_override_via_the_remove_button_also_asks() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft("Brand.exe").unwrap();
        st.set_field("Brand.exe", "host_render", json!(true));
        assert!(st.remove_override("Brand.exe", "host_render"));
        assert!(st.dlg.empty_visible.get());
        assert_eq!(st.draft.get(), "Brand.exe");
    }

    /// 还剩别的项时去掉一项不该问；整条删除 / 还原是明确意图，也不问。
    #[test]
    fn no_prompt_while_other_overrides_remain_or_on_an_explicit_delete() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft("Brand.exe").unwrap();
        st.set_field("Brand.exe", "host_render", json!(true));
        st.set_field("Brand.exe", "initial_mode", json!("english"));
        assert!(st.remove_override("Brand.exe", "host_render"));
        assert!(!st.dlg.empty_visible.get(), "还剩一项，不问");
        assert!(
            st.rules
                .with(|r| r.iter().any(|x| x.process == "Brand.exe"))
        );

        assert!(st.reset_rule("Brand.exe"));
        assert!(!st.dlg.empty_visible.get(), "明确删除不问");
        assert!(
            !st.rules
                .with(|r| r.iter().any(|x| x.process == "Brand.exe"))
        );
    }

    /// 还有等着给值的占位时并没有真的「清空」：留草稿继续设值，不问。
    #[test]
    fn a_pending_placeholder_means_the_rule_is_not_empty() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft("Brand.exe").unwrap();
        st.set_field("Brand.exe", "host_render", json!(true));
        st.add_override("Brand.exe", "initial_mode");
        st.remove_override("Brand.exe", "host_render");
        assert!(!st.dlg.empty_visible.get());
        assert_eq!(st.draft.get(), "Brand.exe");
        assert!(st.is_pending("Brand.exe", "initial_mode"));
    }

    /// 占位不能漏给同名的下一条草稿。
    #[test]
    fn deleting_or_dropping_a_rule_clears_its_pending_placeholders() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft("Brand.exe").unwrap();
        st.add_override("Brand.exe", "initial_mode");
        st.drop_draft();
        assert!(!st.is_pending("Brand.exe", "initial_mode"));

        st.start_draft("Brand.exe").unwrap();
        st.set_field("Brand.exe", "host_render", json!(true));
        st.add_override("Brand.exe", "initial_mode");
        st.reset_rule("Brand.exe");
        assert!(!st.is_pending("Brand.exe", "initial_mode"));
    }

    /// 挑依赖别的字段的选项：被依赖的一并挑出来；开关也不直接写 true（写了也不生效）。
    #[test]
    fn picking_a_dependent_field_also_picks_what_it_depends_on() {
        let _g = MockGuard::new();
        let st = loaded();
        st.select("Weixin.exe");
        let _ = crate::rpc::take_mock_calls();
        st.add_override("Weixin.exe", "candidate_x");
        assert!(st.is_pending("Weixin.exe", "candidate_x"));
        assert!(
            st.is_pending("Weixin.exe", "candidate_position_mode"),
            "被依赖的定位方式要一并出现"
        );
        assert!(calls("compat.upsert").is_empty());
    }

    /// 重开窗口不带着上次的占位和对话框。
    #[test]
    fn reopening_the_window_resets_placeholders_and_dialogs() {
        let _g = MockGuard::new();
        let st = loaded();
        st.select("Weixin.exe");
        st.add_override("Weixin.exe", "initial_mode");
        st.dlg.field_visible.set(true);
        st.open_refresh();
        assert!(!st.is_pending("Weixin.exe", "initial_mode"));
        assert!(!st.dlg.field_visible.get());
    }

    #[test]
    fn a_failed_write_rebuilds_the_detail_from_core_values() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft(crate::rpc::MOCK_COMPAT_FAIL_PROCESS)
            .unwrap();
        let d = st.detail_epoch.get();
        assert!(!st.set_field(
            crate::rpc::MOCK_COMPAT_FAIL_PROCESS,
            "auto_pair",
            json!(true)
        ));
        assert!(st.detail_epoch.get() > d, "写失败要按核心的值重建控件");
    }

    /// 用户层本来是空时 resetAll 规则集不变，选中 / 草稿却被清了，界面必须被通知。
    #[test]
    fn reset_all_always_notifies_the_ui_even_when_the_rule_set_is_unchanged() {
        let _g = MockGuard::new();
        let st = loaded();
        let (l, d) = (st.list_epoch.get(), st.detail_epoch.get());
        st.reset_all().unwrap();
        assert!(st.list_epoch.get() > l && st.detail_epoch.get() > d);
    }

    #[test]
    fn open_refresh_loads_first_then_refetches_on_every_reopen() {
        let _g = MockGuard::new();
        let st = CompatState::new();
        st.open_refresh();
        assert_eq!(calls("compat.list").len(), 1, "首次打开加载");
        st.open_refresh();
        assert_eq!(calls("compat.list").len(), 1, "再次打开重拉一次");
    }

    #[test]
    fn starting_a_draft_for_an_existing_process_jumps_to_it_instead() {
        let _g = MockGuard::new();
        let st = loaded();
        st.filter.set(3);
        st.search.set("zzz".into());
        assert_eq!(
            st.start_draft("weixin.EXE").unwrap(),
            false,
            "已存在（不区分大小写）不新建"
        );
        assert_eq!(st.draft.get(), "");
        assert_eq!(st.selected.get(), "Weixin.exe");
        assert_eq!(
            (st.filter.get(), st.search.get().as_str()),
            (0, ""),
            "清掉筛选与搜索，保证能看到它"
        );
    }

    #[test]
    fn draft_names_are_validated_like_the_core_does() {
        let _g = MockGuard::new();
        let st = loaded();
        for bad in ["", "   ", "a/b.exe", "a\\b.exe", "a\u{7}b.exe"] {
            assert!(st.start_draft(bad).is_err(), "{bad:?} 应被拒绝");
        }
        assert_eq!(st.draft.get(), "");
    }

    #[test]
    fn dropping_a_draft_restores_a_legal_selection() {
        let _g = MockGuard::new();
        let st = loaded();
        let first = st.visible_rules()[0].process.clone();
        st.start_draft("New.exe").unwrap();
        st.drop_draft();
        assert_eq!(st.draft.get(), "");
        assert_ne!(st.selected.get(), "New.exe");
        assert!(st.visible_rules().iter().all(|r| r.process != "New.exe"));
        assert!(
            !st.selected.get().is_empty(),
            "落回一条真规则（{first} 之类）"
        );
    }

    #[test]
    fn a_load_failure_keeps_existing_content_and_raises_the_banner_until_it_recovers() {
        let _g = MockGuard::new();
        let st = loaded();
        let before = st.rules.get();
        crate::rpc::force_mock_error("compat.list", Some("用户层 compat.toml 有内容无法读入"));
        st.reload();
        assert!(
            st.load_error.get().contains("无法读入"),
            "横幅要带核心给的原因: {}",
            st.load_error.get()
        );
        assert_eq!(st.rules.get(), before, "瞬时失败不清空已有内容");
        crate::rpc::force_mock_error("compat.list", None);
        st.reload();
        assert_eq!(st.load_error.get(), "", "重拉成功后横幅自动消失");
    }

    #[test]
    fn a_schema_failure_raises_the_banner_and_is_retried_on_the_next_reload() {
        let _g = MockGuard::new();
        let st = CompatState::new();
        crate::rpc::force_mock_error("compat.schema", Some("核心不在线"));
        st.ensure_loaded();
        assert_eq!(st.load_error.get(), "核心不在线");
        assert!(
            st.rules.with(|r| r.is_empty()),
            "元数据没拿到就不往下拉规则"
        );
        crate::rpc::force_mock_error("compat.schema", None);
        st.reload();
        assert_eq!(st.load_error.get(), "");
        assert!(st.rules.with(|r| !r.is_empty()), "恢复后元数据与规则都到位");
    }
}

/// 补丁里被设置了「非 null 值」的键（清除不算「给了值」）。
fn patch_keys(patch: &Value) -> Option<Vec<String>> {
    Some(
        patch
            .as_object()?
            .iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, _)| k.clone())
            .collect(),
    )
}
