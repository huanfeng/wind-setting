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

use super::model::{ListResult, Problem, RuleIdent, RuleState, RuleView, Schema, Section};
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
    /// 左栏搜索词（按进程名 / 类名 / 标题子串、不区分大小写）。
    pub search: Signal<String>,
    /// 当前段的全部规则（核心返回的原样）。
    pub rules: Signal<Vec<RuleView>>,
    pub warnings: Signal<Vec<Problem>>,
    /// 选中规则的规范化身份（[`RuleView::key`]）；空串 = 没有选中。
    ///
    /// 不能存进程名：同一个进程可以有好几条规则（纯进程的、带类名 / 标题条件的）。
    pub selected: Signal<String>,
    /// 非空 = `compat.list` / `compat.schema` 失败（用户层损坏、核心不在线…），页面显示横幅。
    pub load_error: Signal<String>,
    /// 左栏列表需要重建（规则集 / 筛选 / 搜索 / 段 / 草稿变了）。
    pub list_epoch: Signal<u64>,
    /// 右栏详情需要重建（选中项变了、或选中项的视图变了）。
    pub detail_epoch: Signal<u64>,
    /// 外部触发的刷新请求（窗口重获焦点、核心广播 `compat.changed`）：页面里有个 effect 监听它。
    pub refresh_tick: Signal<u64>,
    /// 新建规则的草稿身份；`None` = 没有草稿。草稿在第一次设置字段之前不落盘。
    pub draft: Signal<Option<RuleIdent>>,
    /// 详情里是否列出「高级」字段（`advanced`）：默认只列常用项，已被设置过的高级字段
    /// 无论如何都会列出，免得用户看不到自己改过什么。
    pub show_advanced: Signal<bool>,
    /// 「已选来覆盖、但还没给出值」的字段 `(规则身份 key, 字段键)`。
    ///
    /// 详情只列被设置过的字段，用户通过「添加覆盖项」挑字段；文本 / 数字这类字段选出来的
    /// 那一刻还没有值可写（空串、0 会被核心当成没写），所以先在界面里占一行，设置了值才落盘。
    /// 不进核心、不跨段（切段即清）。
    pending: Rc<RefCell<Vec<(String, String)>>>,
    /// 核心支持窗口条件（见 [`ListResult::window_conditions`]）；不支持时界面藏起类名 / 标题输入
    /// 与「修改匹配条件」「复制为新规则」。首次加载前按支持处理（没有规则可操作）。
    pub window_ok: Signal<bool>,
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
            draft: signal(None),
            show_advanced: signal(false),
            pending: Rc::new(RefCell::new(Vec::new())),
            window_ok: signal(true),
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
                if self.window_ok.get() != l.window_conditions {
                    self.window_ok.set(l.window_conditions);
                    Self::bump(self.detail_epoch);
                }
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

    /// 选中项保持合法：不在（筛选后的）可见集里就落到第一条。草稿与其它规则一视同仁——
    /// 被筛选 / 搜索挡住的草稿不能继续占着选中，否则右栏显示一条左栏看不到的规则。
    fn fix_selection(&self) {
        let cur = self.selected.get();
        let visible = self.visible_rules();
        let ok = !cur.is_empty() && visible.iter().any(|r| r.key == cur);
        if !ok {
            let next = visible.first().map(|r| r.key.clone()).unwrap_or_default();
            if next != cur {
                self.selected.set(next);
            }
        }
    }

    /// 按筛选与搜索词过滤、按进程归组排序后的规则。
    ///
    /// - 草稿（新增的、或被清空后保留的空白规则）当成一条「自定义」规则**同样过筛选与搜索**：
    ///   此前它无条件置顶、绕过筛选，清空后的空白规则会莫名其妙排第一。
    /// - 搜索词匹配进程名（含「所有应用」这个显示名）、类名、标题。
    /// - 排序：**同一进程的规则排在一起**（纯进程那条在前，带类名 / 标题条件的跟在后面），
    ///   用户层动过（已修改 / 自定义 / 用户禁用的，含草稿）的进程组排前面，纯出厂的在后；
    ///   组与组之间、组内各条保持核心返回的顺序，草稿排在最前——刚新增的马上能看到。
    pub fn visible_rules(&self) -> Vec<RuleView> {
        let filter = self.filter.get();
        let needle = self.search.get().trim().to_lowercase();
        let mut all: Vec<RuleView> = self.rules.with(|r| r.clone());
        if let Some(d) = self.draft.get()
            && !all.iter().any(|r| r.key == d.key())
        {
            all.insert(
                0,
                self.schema
                    .with(|s| RuleView::draft(&d, s, self.current_section())),
            );
        }
        let touched = |r: &RuleView| r.has_user_entry || r.is_draft();
        let group_of = |r: &RuleView| r.process.trim().to_ascii_lowercase();
        // 每个进程组的序号（首次出现的位置）与「组里有没有用户动过的」。
        let mut groups: Vec<(String, bool)> = Vec::new();
        for r in &all {
            let g = group_of(r);
            match groups.iter_mut().find(|(name, _)| *name == g) {
                Some(e) => e.1 |= touched(r),
                None => groups.push((g, touched(r))),
            }
        }
        let mut out: Vec<RuleView> = all
            .into_iter()
            .filter(|r| match filter {
                1 => r.state == RuleState::Modified,
                2 => r.state == RuleState::User,
                3 => r.state == RuleState::Disabled,
                _ => true,
            })
            .filter(|r| {
                needle.is_empty()
                    || [
                        r.process.as_str(),
                        r.class.as_str(),
                        r.title.as_str(),
                        &r.ident().display_name(),
                    ]
                    .iter()
                    .any(|t| t.to_lowercase().contains(&needle))
            })
            .collect();
        out.sort_by_key(|r| {
            let g = group_of(r);
            let gi = groups.iter().position(|(name, _)| *name == g).unwrap_or(0);
            let gt = groups.get(gi).is_some_and(|e| e.1);
            (!gt, gi, r.ident().has_window_condition())
        });
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
            .with(|rules| rules.iter().find(|r| r.key == cur).cloned())
        {
            return Some(r);
        }
        self.draft.get().filter(|d| d.key() == cur).map(|d| {
            self.schema
                .with(|s| RuleView::draft(&d, s, self.current_section()))
        })
    }

    /// 选中一条规则（按规范化身份 [`RuleView::key`]）。
    pub fn select(&self, key: &str) {
        if self.selected.get() != key {
            self.selected.set(key.to_string());
            // 列表的选中高亮由 `selected` 信号直接驱动（`selectable`），不需要重建左栏。
            Self::bump(self.detail_epoch);
        }
    }

    /// 切段：清掉搜索与草稿之外的一切，重拉。
    pub fn switch_section(&self) {
        self.selected.set(String::new());
        self.draft.set(None);
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

    fn call_write(&self, method: &str, params: Value) -> Result<Value, ()> {
        self.call_write_quiet(method, params).map_err(|e| {
            self.notify(ToastKind::Error, e);
        })
    }

    /// 同 [`Self::call_write`]，但失败不 toast（对话框里自己显示错误）。
    fn call_write_quiet(&self, method: &str, mut params: Value) -> Result<Value, String> {
        params["section"] = json!(self.current_section().id());
        rpc::call_core(method, &params)
    }

    /// 身份参数 + 额外参数。
    fn with_ident(ident: &RuleIdent, extra: Value) -> Value {
        let mut m = ident.params();
        if let Value::Object(e) = extra {
            m.extend(e);
        }
        Value::Object(m)
    }

    /// 设置一个字段。草稿的第一次写入让它成为真正的规则。
    pub fn set_field(&self, ident: &RuleIdent, key: &str, value: Value) -> bool {
        self.upsert(ident, json!({ key: value }))
    }

    /// 清除一个字段（回到「跟随全局」；出厂设了它时核心会写 `unset`）。
    pub fn clear_field(&self, ident: &RuleIdent, key: &str) -> bool {
        self.upsert(ident, json!({ key: Value::Null }))
    }

    fn upsert(&self, ident: &RuleIdent, patch: Value) -> bool {
        if self
            .call_write(
                "compat.upsert",
                Self::with_ident(ident, json!({ "patch": patch })),
            )
            .is_err()
        {
            self.rebuild_after_failure();
            return false;
        }
        let keys: Vec<String> = patch
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        self.reload_after_write(true, Some((ident, &keys)));
        true
    }

    /// 写入之后核对这几个字段：任何一层还设着的（出厂 / 用户）就不再是「待设置」；
    /// 写完却回到「没设置」的（开关拨回关、数字回 0、下拉选回「跟随全局」）**保留成一行**——
    /// 用户是主动改成默认值的，行不该凭空消失，要去掉走「移除」。
    fn settle_pending(&self, ident: &RuleIdent, keys: &[String]) {
        // 规则整条没了（用户新增的规则、字段全落回默认）时视为全部未设置。
        let rule = self.selected_rule_of(ident);
        let rk = ident.key();
        let mut pend = self.pending.borrow_mut();
        let changed = false;
        for k in keys {
            let unset = rule.as_ref().is_none_or(|r| {
                r.fields
                    .get(k)
                    .is_none_or(|v| v.source == super::model::FieldSource::Default)
            });
            let has = pend.iter().any(|(p, key)| *p == rk && key == k);
            match (unset, has) {
                (true, false) => pend.push((rk.clone(), k.clone())),
                (false, true) => pend.retain(|(p, key)| !(*p == rk && key == k)),
                _ => {}
            }
        }
        drop(pend);
        // 占位没增减就不必重建：写入内容没变时（文本框回车提交同样的文字）重建会白丢焦点。
        if changed {
            Self::bump(self.detail_epoch);
        }
    }

    fn selected_rule_of(&self, ident: &RuleIdent) -> Option<RuleView> {
        let k = ident.key();
        self.rules
            .with(|rs| rs.iter().find(|r| r.key == k).cloned())
            .or_else(|| {
                self.draft.get().filter(|d| d.key() == k).map(|d| {
                    self.schema
                        .with(|s| RuleView::draft(&d, s, self.current_section()))
                })
            })
    }

    /// 还原为出厂：删掉用户层关于这个字段的一切痕迹。
    pub fn reset_field(&self, ident: &RuleIdent, key: &str) -> bool {
        self.simple_write(
            "compat.resetField",
            Self::with_ident(ident, json!({ "key": key })),
            true,
        )
    }

    pub fn set_disabled(&self, ident: &RuleIdent, disabled: bool) -> bool {
        self.simple_write(
            "compat.setDisabled",
            Self::with_ident(ident, json!({ "disabled": disabled })),
            false,
        )
    }

    /// 还原单条：删掉用户层同身份的条目（用户新增的规则等于删除）。
    pub fn reset_rule(&self, ident: &RuleIdent) -> bool {
        let ok = self.simple_write("compat.reset", Self::with_ident(ident, json!({})), false);
        if ok {
            self.clear_pending_of(&ident.key());
        }
        ok
    }

    /// 丢掉某条规则（规范化身份）的全部「待设置」占位（规则被删 / 草稿被丢时）。
    fn clear_pending_of(&self, key: &str) {
        self.pending.borrow_mut().retain(|(p, _)| p != key);
    }

    fn has_pending(&self, key: &str) -> bool {
        self.pending.borrow().iter().any(|(p, _)| p == key)
    }

    /// 规则换了身份（改匹配条件）：它的占位跟着走。
    fn rekey_pending(&self, from: &str, to: &str) {
        for (p, _) in self.pending.borrow_mut().iter_mut() {
            if p == from {
                *p = to.to_string();
            }
        }
    }

    fn simple_write(&self, method: &str, params: Value, prompt_if_emptied: bool) -> bool {
        if self.call_write(method, params).is_err() {
            self.rebuild_after_failure();
            return false;
        }
        self.reload_after_write(prompt_if_emptied, None);
        true
    }

    fn require_window_support(&self) -> Result<(), String> {
        if self.window_ok.get() {
            Ok(())
        } else {
            Err("输入法核心版本过旧，不支持修改匹配条件，请先升级".into())
        }
    }

    /// 改匹配条件（`compat.rename`）。只对用户新增的规则（含草稿）有意义——出厂规则只能
    /// [`Self::copy_rule`]，核心同样会拒绝。草稿还没落盘，只在界面里换身份。
    /// 失败返回核心 / 校验给的中文原因（对话框里显示），成功后选中新身份。
    pub fn rename_rule(&self, from: &RuleIdent, to: RuleIdent) -> Result<(), String> {
        self.require_window_support()?;
        to.validate(self.current_section())?;
        let (fk, tk) = (from.key(), to.key());
        let taken = |k: &str| self.rules.with(|r| r.iter().any(|x| x.key == k));
        if tk != fk && taken(&tk) {
            return Err("已有匹配条件相同的规则".into());
        }
        let is_draft = self.draft.get().is_some_and(|d| d.key() == fk) && !taken(&fk);
        if is_draft {
            self.draft.set(Some(to));
        } else {
            let mut params = Value::Object(from.params());
            params["to"] = Value::Object(to.params());
            self.call_write_quiet("compat.rename", params)?;
        }
        self.rekey_pending(&fk, &tk);
        self.selected.set(tk);
        self.reload_after_write(false, None);
        Self::bump(self.list_epoch);
        Self::bump(self.detail_epoch);
        Ok(())
    }

    /// 复制为新规则（`compat.copy`）：源规则叠加后的设置写成一条新身份的用户规则。
    /// 出厂规则想换匹配条件走这里。成功后选中新规则。
    pub fn copy_rule(&self, from: &RuleIdent, to: RuleIdent) -> Result<(), String> {
        self.require_window_support()?;
        to.validate(self.current_section())?;
        let tk = to.key();
        if self.rules.with(|r| r.iter().any(|x| x.key == tk)) {
            return Err("已有匹配条件相同的规则".into());
        }
        let mut params = Value::Object(from.params());
        params["to"] = Value::Object(to.params());
        self.call_write_quiet("compat.copy", params)?;
        self.search.set(String::new());
        self.filter.set(0);
        self.selected.set(tk);
        self.reload_after_write(false, None);
        Self::bump(self.list_epoch);
        Self::bump(self.detail_epoch);
        Ok(())
    }

    /// 全部还原（核心会先备份）。返回备份文件名。
    pub fn reset_all(&self) -> Option<String> {
        let v = rpc::call_core("compat.resetAll", &json!({}))
            .map_err(|e| self.notify(ToastKind::Error, e))
            .ok()?;
        self.selected.set(String::new());
        self.draft.set(None);
        self.pending.borrow_mut().clear();
        self.reload_after_write(false, None);
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
    fn reload_after_write(
        &self,
        prompt_if_emptied: bool,
        touched: Option<(&RuleIdent, &[String])>,
    ) {
        let before = (self.selected.get(), self.draft.get());
        let sel = before.0.clone();
        let sel_ident = self.selected_rule().map(|r| r.ident());
        let existed = !sel.is_empty() && self.rules.with(|r| r.iter().any(|x| x.key == sel));
        self.reload();
        if let Some((p, keys)) = touched {
            self.settle_pending(p, keys);
        }
        let gone = existed && !self.rules.with(|r| r.iter().any(|x| x.key == sel));
        if gone
            && prompt_if_emptied
            && let Some(ident) = sel_ident
        {
            // 草稿只有一条：顶掉另一条未保存的草稿时把它的占位一并清掉。
            if let Some(old) = self.draft.get()
                && old.key() != sel
            {
                self.clear_pending_of(&old.key());
            }
            // 还有等着给值的占位时，规则并没有真的「清空」——留着草稿继续设值，不必问。
            let pending = self.has_pending(&sel);
            self.dlg.empty_name.set(ident.describe());
            self.draft.set(Some(ident));
            self.selected.set(sel.clone());
            if !pending {
                self.dlg.empty_visible.set(true);
            }
        }
        // 草稿只在核心里确实有了这条规则之后才算转正：第一次写入被 normalize 成空操作时
        // 核心不落行，草稿要留着，否则界面停在一条既不是草稿也不存在的规则上。
        if let Some(d) = self.draft.get()
            && self.rules.with(|r| r.iter().any(|x| x.key == d.key()))
        {
            self.draft.set(None);
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
    pub fn is_pending(&self, ident: &RuleIdent, key: &str) -> bool {
        let rk = ident.key();
        self.pending
            .borrow()
            .iter()
            .any(|(p, k)| *p == rk && k == key)
    }

    /// 在「添加覆盖项」里挑了一个字段。开关类直接写 `true`（没有比它更自然的初值，
    /// 而写「关」等于没写）；其余先占一行等用户给值。
    pub fn add_override(&self, ident: &RuleIdent, key: &str) {
        let section = self.current_section();
        let meta = self.schema.with(|s| s.field(section, key).cloned());
        let Some(meta) = meta else {
            return;
        };
        let rk = ident.key();
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
            if unset && !self.is_pending(ident, &d.key) {
                self.pending.borrow_mut().push((rk.clone(), d.key.clone()));
            }
        }
        if meta.kind == super::model::Kind::Bool && !dep_pending {
            self.set_field(ident, key, json!(true));
            return;
        }
        if !self.is_pending(ident, key) {
            self.pending.borrow_mut().push((rk, key.to_string()));
        }
        Self::bump(self.detail_epoch);
    }

    /// 去掉一个覆盖项：让这一行真的没有。
    ///
    /// 占位（还没落盘）直接丢；已被某一层设置过的走「还原」（`compat.resetField`，删掉用户层的痕迹）。
    /// 占位与「已设置」可能并存（核心外部改动了它，如右键菜单），所以两件事都要做，
    /// 否则第一次点击只丢了本地占位、这一行还在。
    pub fn remove_override(&self, ident: &RuleIdent, key: &str) -> bool {
        let rk = ident.key();
        self.pending
            .borrow_mut()
            .retain(|(p, k)| !(*p == rk && k == key));
        let unset = self.selected_rule_of(ident).is_none_or(|r| {
            r.fields
                .get(key)
                .is_none_or(|v| v.source == super::model::FieldSource::Default)
        });
        if unset {
            Self::bump(self.detail_epoch);
            return true;
        }
        // 走「还原」而不是「清除」：清除会落回默认并被保留成一行（见 `settle_pending`），
        // 而「移除」要的是这一行真的没有。
        self.reset_field(ident, key)
    }

    /// 空规则弹窗里选「删除」：草稿丢掉即可，核心里早已没有这条。
    pub fn confirm_delete_emptied(&self) {
        self.dlg.empty_visible.set(false);
        if let Some(d) = self.draft.get() {
            self.clear_pending_of(&d.key());
        }
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
    /// 同身份（进程名 + 类名 + 标题，不区分大小写）的规则已存在则直接跳到它。
    /// 返回是否新建了草稿。
    pub fn start_draft(&self, ident: RuleIdent) -> Result<bool, String> {
        // 旧核心会把类名 / 标题丢掉，写成一条 `*` 或纯进程规则（`*` 套到所有应用）。
        if ident.has_window_condition() && !self.window_ok.get() {
            return Err("输入法核心版本过旧，不支持类名 / 标题条件，请先升级".into());
        }
        ident.validate(self.current_section())?;
        let key = ident.key();
        let exists = self.rules.with(|rs| rs.iter().any(|r| r.key == key));
        self.search.set(String::new());
        self.filter.set(0);
        if exists {
            self.select(&key);
            return Ok(false);
        }
        self.draft.set(Some(ident));
        self.select(&key);
        Self::bump(self.list_epoch);
        Self::bump(self.detail_epoch);
        Ok(true)
    }

    /// 丢弃草稿（没设置过任何字段就离开）。
    pub fn drop_draft(&self) {
        let Some(d) = self.draft.get() else {
            return;
        };
        let key = d.key();
        self.clear_pending_of(&key);
        let was_selected = self.selected.get() == key;
        self.draft.set(None);
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
    use crate::pages::compat::model::FieldSource;

    fn id(p: &str) -> RuleIdent {
        RuleIdent::process(p)
    }

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
        let first = st.visible_rules()[0].key.clone();
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
            disabled.iter().any(|r| r.key == st.selected.get()),
            "选中项必须落在可见集里"
        );
        st.filter.set(0);
        st.search.set("WEIXIN".into());
        st.refilter();
        let hit = st.visible_rules();
        assert_eq!(hit.len(), 1, "搜索不区分大小写");
        assert_eq!(st.selected.get(), hit[0].key);
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
        st.draft.set(Some(id("x.exe")));
        st.section.set(2);
        let _ = crate::rpc::take_mock_calls();
        st.switch_section();
        assert_eq!(st.draft.get(), None);
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
        assert!(st.set_field(&id("Weixin.exe"), "first_show_mode", json!("wait")));
        assert!(st.clear_field(&id("Weixin.exe"), "first_show_mode"));
        assert!(st.set_field(&id("Weixin.exe"), "caret_use_top", json!(false)));
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
        assert!(st.reset_field(&id("Weixin.exe"), "stale_probe_guard"));
        assert!(st.set_disabled(&id("Weixin.exe"), true));
        assert!(st.reset_rule(&id("Tabby.exe")));
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
            &id(crate::rpc::MOCK_COMPAT_FAIL_PROCESS),
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
        assert_eq!(st.start_draft(id("  New.exe ")).unwrap(), true);
        assert_eq!(st.draft.get(), Some(id("New.exe")), "进程名 trim");
        assert_eq!(st.selected.get(), id("New.exe").key());
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
        assert!(st.set_field(&id("New.exe"), "auto_pair", json!(true)));
        assert_eq!(st.draft.get(), None, "第一次设置字段后草稿转正");
        assert_eq!(calls("compat.upsert").len(), 1);
        assert_eq!(st.selected.get(), id("New.exe").key(), "转正后仍选中它");
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
        st.start_draft(id(name)).unwrap();
        assert!(st.set_field(&id(name), "auto_pair", json!(true)));
        assert_eq!(
            st.draft.get(),
            Some(id(name)),
            "核心没建出这条规则，草稿不能转正"
        );
        assert_eq!(st.selected.get(), id(name).key());
        assert!(st.selected_rule().unwrap().is_draft());
        assert!(st.visible_rules().iter().any(|r| r.process == name));
    }

    /// 「添加覆盖项」：开关直接写 true，其余先占一行等值；给了值就不再是待设置。
    #[test]
    fn add_override_writes_bools_at_once_and_parks_other_kinds_until_given_a_value() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft(id("Brand.exe")).unwrap();
        let _ = crate::rpc::take_mock_calls();

        st.add_override(&id("Brand.exe"), "host_render");
        let w = calls("compat.upsert");
        assert_eq!(w.len(), 1);
        assert_eq!(w[0]["patch"]["host_render"], json!(true));

        st.add_override(&id("Brand.exe"), "initial_mode");
        assert!(
            calls("compat.upsert").is_empty(),
            "非开关字段挑出来时没有值可写"
        );
        assert!(st.is_pending(&id("brand.EXE"), "initial_mode"));

        st.set_field(&id("Brand.exe"), "initial_mode", json!("english"));
        assert!(
            !st.is_pending(&id("Brand.exe"), "initial_mode"),
            "给了值就转正"
        );
    }

    /// 改回默认值不让这一行消失：开关拨回关、下拉选回「跟随全局」都保留成一行，
    /// 要去掉走「移除」。
    #[test]
    fn setting_a_field_back_to_its_default_keeps_the_row() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft(id("Brand.exe")).unwrap();
        st.set_field(&id("Brand.exe"), "host_render", json!(true));
        st.set_field(&id("Brand.exe"), "initial_mode", json!("english"));
        assert!(!st.is_pending(&id("Brand.exe"), "host_render"));

        st.set_field(&id("Brand.exe"), "host_render", json!(false));
        assert!(
            st.is_pending(&id("Brand.exe"), "host_render"),
            "开关拨回关，行还在"
        );
        st.clear_field(&id("Brand.exe"), "initial_mode");
        assert!(
            st.is_pending(&id("Brand.exe"), "initial_mode"),
            "选回跟随全局，行还在"
        );
        assert!(!st.dlg.empty_visible.get(), "还有行在，规则不算清空");

        // 再给值就转正，再「移除」才真的没有。
        st.set_field(&id("Brand.exe"), "host_render", json!(true));
        assert!(!st.is_pending(&id("Brand.exe"), "host_render"));
        // 「移除」才真的没有：不是占位、来源回到默认。initial_mode 仍在，规则不算清空。
        assert!(st.remove_override(&id("Brand.exe"), "host_render"));
        assert!(!st.is_pending(&id("Brand.exe"), "host_render"));
        let rule = st.selected_rule_of(&id("Brand.exe")).unwrap();
        assert_eq!(rule.fields["host_render"].source, FieldSource::Default);
        assert!(!st.dlg.empty_visible.get());
    }

    /// 三态开关的显式「关」是有意义的（核心保留），拨到关不该被当成「没设置」而变成占位。
    #[test]
    fn a_tri_state_off_is_a_real_override_not_a_placeholder() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft(id("Brand.exe")).unwrap();
        st.set_field(&id("Brand.exe"), "auto_pair", json!(false));
        assert!(!st.is_pending(&id("Brand.exe"), "auto_pair"));
        let rule = st.selected_rule_of(&id("Brand.exe")).unwrap();
        assert_eq!(rule.fields["auto_pair"].source, FieldSource::User);
    }

    /// 占位与「已设置」并存（外部改动了它）时，第一次点「移除」就要真的去掉。
    #[test]
    fn remove_works_on_the_first_click_even_if_the_field_got_set_behind_our_back() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft(id("Brand.exe")).unwrap();
        st.set_field(&id("Brand.exe"), "host_render", json!(true));
        st.set_field(&id("Brand.exe"), "initial_mode", json!("english"));
        // 制造并存：占位还挂着，而核心里它已经有值。
        st.pending
            .borrow_mut()
            .push((id("Brand.exe").key(), "initial_mode".into()));
        let _ = crate::rpc::take_mock_calls();
        assert!(st.remove_override(&id("Brand.exe"), "initial_mode"));
        assert_eq!(calls("compat.resetField").len(), 1, "已设置的要发还原请求");
        assert!(!st.is_pending(&id("Brand.exe"), "initial_mode"));
    }

    /// 待设置的字段被「移除」只是丢掉占位，不发请求；选回「跟随全局」也不让占位消失。
    #[test]
    fn removing_or_clearing_a_pending_field_behaves() {
        let _g = MockGuard::new();
        let st = loaded();
        st.select(&id("Weixin.exe").key());
        st.add_override(&id("Weixin.exe"), "initial_mode");
        let _ = crate::rpc::take_mock_calls();
        st.clear_field(&id("Weixin.exe"), "initial_mode");
        assert!(st.is_pending(&id("Weixin.exe"), "initial_mode"));
        let _ = crate::rpc::take_mock_calls();
        assert!(st.remove_override(&id("Weixin.exe"), "initial_mode"));
        assert!(calls("compat.upsert").is_empty());
        assert!(!st.is_pending(&id("Weixin.exe"), "initial_mode"));
    }

    /// 用户新增的规则被去掉最后一项：不直接凭空消失，保留成空白草稿并请用户决定。
    #[test]
    fn emptying_a_user_rule_keeps_a_blank_draft_and_asks_before_it_is_gone() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft(id("Brand.exe")).unwrap();
        assert!(st.set_field(
            &id("Brand.exe"),
            "composition_start_pair_guard",
            json!(true)
        ));
        assert_eq!(st.draft.get(), None, "写入后规则转正");
        assert!(!st.dlg.empty_visible.get());

        assert!(st.reset_field(&id("Brand.exe"), "composition_start_pair_guard"));
        assert!(st.dlg.empty_visible.get(), "规则被清空要先问一句");
        assert_eq!(st.dlg.empty_name.get(), "Brand.exe");
        assert_eq!(
            st.draft.get(),
            Some(id("Brand.exe")),
            "保留成可继续添加覆盖项的空白规则"
        );
        assert_eq!(st.selected.get(), id("Brand.exe").key());
        assert!(st.selected_rule().unwrap().is_draft());

        st.confirm_delete_emptied();
        assert!(!st.dlg.empty_visible.get());
        assert_eq!(st.draft.get(), None, "选删除就丢掉");
        assert_ne!(st.selected.get(), id("Brand.exe").key());
    }

    /// 去掉最后一项的另一条路（界面上新增规则的按钮叫「移除」，走 upsert null）同样要问。
    #[test]
    fn removing_the_last_override_via_the_remove_button_also_asks() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft(id("Brand.exe")).unwrap();
        st.set_field(&id("Brand.exe"), "host_render", json!(true));
        assert!(st.remove_override(&id("Brand.exe"), "host_render"));
        assert!(st.dlg.empty_visible.get());
        assert_eq!(st.draft.get(), Some(id("Brand.exe")));
    }

    /// 还剩别的项时去掉一项不该问；整条删除 / 还原是明确意图，也不问。
    #[test]
    fn no_prompt_while_other_overrides_remain_or_on_an_explicit_delete() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft(id("Brand.exe")).unwrap();
        st.set_field(&id("Brand.exe"), "host_render", json!(true));
        st.set_field(&id("Brand.exe"), "initial_mode", json!("english"));
        assert!(st.remove_override(&id("Brand.exe"), "host_render"));
        assert!(!st.dlg.empty_visible.get(), "还剩一项，不问");
        assert!(
            st.rules
                .with(|r| r.iter().any(|x| x.process == "Brand.exe"))
        );

        assert!(st.reset_rule(&id("Brand.exe")));
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
        st.start_draft(id("Brand.exe")).unwrap();
        st.set_field(&id("Brand.exe"), "host_render", json!(true));
        st.add_override(&id("Brand.exe"), "initial_mode");
        st.remove_override(&id("Brand.exe"), "host_render");
        assert!(!st.dlg.empty_visible.get());
        assert_eq!(st.draft.get(), Some(id("Brand.exe")));
        assert!(st.is_pending(&id("Brand.exe"), "initial_mode"));
    }

    /// 占位不能漏给同名的下一条草稿。
    #[test]
    fn deleting_or_dropping_a_rule_clears_its_pending_placeholders() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft(id("Brand.exe")).unwrap();
        st.add_override(&id("Brand.exe"), "initial_mode");
        st.drop_draft();
        assert!(!st.is_pending(&id("Brand.exe"), "initial_mode"));

        st.start_draft(id("Brand.exe")).unwrap();
        st.set_field(&id("Brand.exe"), "host_render", json!(true));
        st.add_override(&id("Brand.exe"), "initial_mode");
        st.reset_rule(&id("Brand.exe"));
        assert!(!st.is_pending(&id("Brand.exe"), "initial_mode"));
    }

    /// 挑依赖别的字段的选项：被依赖的一并挑出来；开关也不直接写 true（写了也不生效）。
    #[test]
    fn picking_a_dependent_field_also_picks_what_it_depends_on() {
        let _g = MockGuard::new();
        let st = loaded();
        st.select(&id("Weixin.exe").key());
        let _ = crate::rpc::take_mock_calls();
        st.add_override(&id("Weixin.exe"), "candidate_x");
        assert!(st.is_pending(&id("Weixin.exe"), "candidate_x"));
        assert!(
            st.is_pending(&id("Weixin.exe"), "candidate_position_mode"),
            "被依赖的定位方式要一并出现"
        );
        assert!(calls("compat.upsert").is_empty());
    }

    /// 重开窗口不带着上次的占位和对话框。
    #[test]
    fn reopening_the_window_resets_placeholders_and_dialogs() {
        let _g = MockGuard::new();
        let st = loaded();
        st.select(&id("Weixin.exe").key());
        st.add_override(&id("Weixin.exe"), "initial_mode");
        st.dlg.field_visible.set(true);
        st.open_refresh();
        assert!(!st.is_pending(&id("Weixin.exe"), "initial_mode"));
        assert!(!st.dlg.field_visible.get());
    }

    /// 同一进程的规则排在一起（纯进程那条在前、带窗口条件的在后）；用户动过的进程组排前面，
    /// 纯出厂的在后；组的先后保持核心给的顺序。
    #[test]
    fn rules_group_by_process_with_user_touched_groups_first() {
        let _g = MockGuard::new();
        let st = loaded();
        let v = st.visible_rules();
        let group = |r: &RuleView| r.process.to_ascii_lowercase();
        // 1) 同进程连续。
        let mut seen: Vec<String> = Vec::new();
        for r in &v {
            let g = group(r);
            if seen.last() != Some(&g) {
                assert!(
                    !seen.contains(&g),
                    "{g} 的规则被拆开了: {:?}",
                    v.iter().map(group).collect::<Vec<_>>()
                );
                seen.push(g);
            }
        }
        // 2) 组内纯进程那条在前。
        for g in &seen {
            let conds: Vec<bool> = v
                .iter()
                .filter(|r| group(r) == *g)
                .map(|r| r.ident().has_window_condition())
                .collect();
            let mut sorted = conds.clone();
            sorted.sort();
            assert_eq!(conds, sorted, "{g}：纯进程那条排在带窗口条件的前面");
        }
        // 3) 动过的组在前，组序与核心一致。
        let touched = |g: &str| v.iter().any(|r| group(r) == g && r.has_user_entry);
        let first_factory = seen.iter().position(|g| !touched(g)).unwrap();
        assert!(first_factory > 0, "mock 里有用户改过的规则");
        assert!(
            seen[first_factory..].iter().all(|g| !touched(g)),
            "{seen:?}"
        );
        let mut core: Vec<String> = Vec::new();
        st.rules.with(|r| {
            for x in r {
                let g = group(x);
                if !core.contains(&g) {
                    core.push(g);
                }
            }
        });
        let factory_core: Vec<String> = core.into_iter().filter(|g| !touched(g)).collect();
        assert_eq!(seen[first_factory..].to_vec(), factory_core);
        // mock 里确有同进程多条（QQ.exe 的纯进程与类名规则）以及「所有应用」的窗口规则。
        assert!(v.iter().filter(|r| group(r) == "qq.exe").count() >= 2);
        assert!(v.iter().any(|r| r.process == "*" && !r.class.is_empty()));
    }

    /// 草稿（新增的、被清空后保留的空白规则）与其它规则一样受筛选和搜索约束，
    /// 且排在「用户动过的」那组最前面，而不是无条件置顶。
    #[test]
    fn a_draft_obeys_the_filter_and_the_search_and_leads_only_its_own_group() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft(id("Brand.exe")).unwrap();
        let v = st.visible_rules();
        assert_eq!(v[0].process, "Brand.exe", "草稿在用户动过的那组最前");
        assert!(
            v.iter().skip(1).any(|r| !r.has_user_entry),
            "纯出厂的仍在后面"
        );

        st.filter.set(3);
        assert!(
            st.visible_rules().iter().all(|r| r.process != "Brand.exe"),
            "「已禁用」筛选下草稿（自定义）不出现"
        );
        st.filter.set(0);
        st.search.set("weixin".into());
        assert!(st.visible_rules().iter().all(|r| r.process != "Brand.exe"));
        st.refilter();
        assert_ne!(
            st.selected.get(),
            id("Brand.exe").key(),
            "被挡住的草稿不能继续占着选中"
        );
    }

    #[test]
    fn a_failed_write_rebuilds_the_detail_from_core_values() {
        let _g = MockGuard::new();
        let st = loaded();
        st.start_draft(id(crate::rpc::MOCK_COMPAT_FAIL_PROCESS))
            .unwrap();
        let d = st.detail_epoch.get();
        assert!(!st.set_field(
            &id(crate::rpc::MOCK_COMPAT_FAIL_PROCESS),
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
            st.start_draft(id("weixin.EXE")).unwrap(),
            false,
            "已存在（不区分大小写）不新建"
        );
        assert_eq!(st.draft.get(), None);
        assert_eq!(st.selected.get(), id("Weixin.exe").key());
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
            assert!(st.start_draft(id(bad)).is_err(), "{bad:?} 应被拒绝");
        }
        assert_eq!(st.draft.get(), None);
    }

    #[test]
    fn dropping_a_draft_restores_a_legal_selection() {
        let _g = MockGuard::new();
        let st = loaded();
        let first = st.visible_rules()[0].process.clone();
        st.start_draft(id("New.exe")).unwrap();
        st.drop_draft();
        assert_eq!(st.draft.get(), None);
        assert_ne!(st.selected.get(), id("New.exe").key());
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

    // ───────────── 窗口条件（同进程多条规则） ─────────────

    fn win(p: &str, c: &str, t: &str) -> RuleIdent {
        RuleIdent::new(p, c, t)
    }

    /// 选中项按身份存：同进程的纯进程规则与类名规则是两条，各自选中、各自写。
    #[test]
    fn same_process_rules_are_selected_and_written_by_identity() {
        let _g = MockGuard::new();
        let st = loaded();
        let qq_win = win("QQ.exe", "TXGuiFoundation", "");
        st.select(&qq_win.key());
        let r = st.selected_rule().expect("类名规则可选中");
        assert_eq!(r.class, "TXGuiFoundation");
        st.select(&id("QQ.exe").key());
        assert!(
            st.selected_rule().unwrap().class.is_empty(),
            "纯进程那条是另一条"
        );

        let _ = crate::rpc::take_mock_calls();
        assert!(st.set_field(&qq_win, "auto_pair", json!(true)));
        assert!(st.set_disabled(&win("*", "Chrome_WidgetWin_*", ""), true));
        let u = calls("compat.upsert");
        assert_eq!(
            u[0],
            json!({"section": "apps", "process": "QQ.exe", "class": "TXGuiFoundation", "patch": {"auto_pair": true}})
        );
        let _ = st;
    }

    /// 已有同进程的规则时，新增带类名的规则是一条新草稿，不会跳到那条纯进程规则上。
    #[test]
    fn a_draft_with_a_class_is_distinct_from_the_process_rule() {
        let _g = MockGuard::new();
        let st = loaded();
        let d = win("Weixin.exe", "ChatWnd", "");
        assert!(st.start_draft(d.clone()).unwrap(), "新建草稿");
        assert_eq!(st.selected.get(), d.key());
        assert_eq!(st.selected_rule().unwrap().class, "ChatWnd");
        let _ = crate::rpc::take_mock_calls();
        assert!(st.set_field(&d, "auto_pair", json!(true)));
        assert_eq!(st.draft.get(), None, "转正");
        assert_eq!(st.selected.get(), d.key());
        assert!(
            st.rules.with(|r| r.iter().any(|x| x.key == d.key())),
            "mock 落了这条"
        );
        // 大小写不同的同一身份：跳过去而不是另建。
        assert!(!st.start_draft(win("weixin.EXE", "chatwnd", "")).unwrap());
        // 只填类名 = 所有应用。
        assert!(st.start_draft(win("", "Foo", "")).unwrap());
        assert_eq!(st.draft.get().unwrap().process, "*");
    }

    #[test]
    fn identity_validation_mirrors_the_core() {
        let _g = MockGuard::new();
        let st = loaded();
        assert!(
            st.start_draft(win("a.exe", "x\u{7}", "")).is_err(),
            "控制字符"
        );
        assert!(
            st.start_draft(win("a.exe", "", &"字".repeat(257))).is_err(),
            "超长"
        );
        st.section.set(2);
        st.switch_section();
        assert!(
            st.start_draft(win("a.exe", "X", "")).is_err(),
            "附属段不支持窗口条件"
        );
        assert!(st.start_draft(id("*")).is_err(), "附属段不支持 *");
        assert!(st.start_draft(id("a.exe")).is_ok());
    }

    /// 改匹配条件：用户规则发 compat.rename 并选中新身份；草稿只在本地换身份；撞上已有的拒绝。
    #[test]
    fn rename_sends_the_old_and_new_identity_and_follows_the_selection() {
        let _g = MockGuard::new();
        let st = loaded();
        let from = win("Tabby.exe", "", "SSH*");
        st.select(&from.key());
        let _ = crate::rpc::take_mock_calls();
        let to = win("Tabby.exe", "VirtualConsoleClass", "");
        st.rename_rule(&from, to.clone()).unwrap();
        let r = calls("compat.rename");
        assert_eq!(
            r[0],
            json!({"section": "apps", "process": "Tabby.exe", "title": "SSH*",
                   "to": {"process": "Tabby.exe", "class": "VirtualConsoleClass"}})
        );
        assert_eq!(st.selected.get(), to.key());

        let err = st.rename_rule(&to, id("Weixin.exe")).unwrap_err();
        assert!(err.contains("已有"), "{err}");

        let d = win("New.exe", "A", "");
        st.start_draft(d.clone()).unwrap();
        st.add_override(&d, "initial_mode");
        let _ = crate::rpc::take_mock_calls();
        let d2 = win("New.exe", "B", "");
        st.rename_rule(&d, d2.clone()).unwrap();
        assert!(calls("compat.rename").is_empty(), "草稿不落盘");
        assert_eq!(st.draft.get(), Some(d2.clone()));
        assert_eq!(st.selected.get(), d2.key());
        assert!(st.is_pending(&d2, "initial_mode"), "占位跟着走");
    }

    /// 复制为新规则：发 compat.copy，新规则出现并被选中。
    #[test]
    fn copy_creates_a_new_rule_and_selects_it() {
        let _g = MockGuard::new();
        let st = loaded();
        let from = id("Weixin.exe");
        let to = win("Weixin.exe", "ChatWnd", "");
        let _ = crate::rpc::take_mock_calls();
        st.copy_rule(&from, to.clone()).unwrap();
        let c = calls("compat.copy");
        assert_eq!(
            c[0],
            json!({"section": "apps", "process": "Weixin.exe", "to": {"process": "Weixin.exe", "class": "ChatWnd"}})
        );
        assert_eq!(st.selected.get(), to.key());
        let r = st.selected_rule().expect("新规则在列表里");
        assert_eq!(r.state, RuleState::User);
        assert!(st.copy_rule(&from, to).is_err(), "已存在");
    }

    /// 无效值提示按身份归属：类名规则的提示不挂到同进程的纯进程规则上。
    #[test]
    fn warnings_are_attributed_by_identity() {
        let w = Problem {
            section: "apps".into(),
            process: "QQ.exe".into(),
            class: "TXGuiFoundation".into(),
            title: String::new(),
            key: "k".into(),
            message: "m".into(),
        };
        let mut r = RuleView::draft(&id("qq.exe"), &Schema::default(), Section::Apps);
        assert!(!w.is_about(&r));
        r = RuleView::draft(
            &win("QQ.EXE", "txguifoundation", ""),
            &Schema::default(),
            Section::Apps,
        );
        assert!(w.is_about(&r));
    }

    /// 搜索也查类名 / 标题，以及「所有应用」这个显示名。
    #[test]
    fn search_matches_class_title_and_the_all_apps_label() {
        let _g = MockGuard::new();
        let st = loaded();
        st.search.set("widgetwin".into());
        let v = st.visible_rules();
        assert!(
            !v.is_empty()
                && v.iter()
                    .all(|r| r.class.to_lowercase().contains("widgetwin"))
        );
        st.search.set("所有应用".into());
        assert!(st.visible_rules().iter().all(|r| r.process == "*"));
        assert!(!st.visible_rules().is_empty());
    }

    /// 旧核心（列表不带 `key`）：窗口条件相关的入口全部关掉，免得写出一条 `*` 规则。
    #[test]
    fn an_old_core_disables_every_window_condition_entry() {
        let _g = MockGuard::new();
        let st = loaded();
        assert!(st.window_ok.get(), "新核心的 mock 支持窗口条件");
        st.window_ok.set(false);
        assert!(st.start_draft(win("", "Chrome_WidgetWin_*", "")).is_err());
        assert!(st.start_draft(win("a.exe", "X", "")).is_err());
        assert!(st.start_draft(id("a.exe")).is_ok(), "纯进程规则照常");
        let tabby = win("Tabby.exe", "", "SSH*");
        assert!(st.rename_rule(&tabby, win("Tabby.exe", "X", "")).is_err());
        assert!(st.copy_rule(&id("Weixin.exe"), id("W2.exe")).is_err());
        // 对话框里残留的类名不能混进新增。
        st.open_add();
        st.dlg.add_name.set("b.exe".into());
        st.dlg.add_class.set("Leftover".into());
        st.confirm_add();
        assert_eq!(st.draft.get(), Some(id("b.exe")));
    }
}
