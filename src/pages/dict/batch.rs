//! 多选的批量动作：工具栏「批量操作」菜单里的启用 / 停用、转正、设常用 / 生僻、恢复默认。
//! 「删除选中」与「移动到…」各有独立按钮，不在这里（见 `state.rs` / `user_libs.rs`）。
//!
//! 每个动作都是**逐条调单条的 RPC**，与行内那一枚按钮同一个写端，core 不另设批量接口；
//! 首错即停并报「已做 k/n 条」。
//!
//! 哪些行参与只按**不会变的属性**判（快捷输入的出厂 / 自建），不按启用、已修改这类状态：
//! 勾选跨页保留，别的页上的勾选行快照不随重载刷新，按状态筛会漏掉它们。这几个动作对已经
//! 处在目标态的行都是空操作，多发一次无害。

use serde_json::{Value, json};
use windui::prelude::*;

use super::spec::{WordCategory, common_label, row_ops};
use super::state::{DictManagerState, PendingAction};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchAction {
    /// 启用（`true`）/ 停用（`false`）。
    SetEnabled(bool),
    /// 临时词转正到用户词库。
    Promote,
    /// 常用字：设为常用（`true`）/ 生僻（`false`）。
    SetCommon(bool),
    /// 恢复这些条目的默认（快捷输入的出厂条目、常用字）。
    ResetEntry,
}

impl BatchAction {
    fn label(self) -> String {
        match self {
            Self::SetEnabled(true) => "启用".into(),
            Self::SetEnabled(false) => "停用".into(),
            Self::Promote => "转正到用户词库".into(),
            Self::SetCommon(c) => format!("设为{}字", common_label(c)),
            Self::ResetEntry => "恢复默认…".into(),
        }
    }

    /// 失败提示「已{verb} k/n 条后失败」里的动词。
    fn verb(self) -> &'static str {
        match self {
            Self::SetEnabled(true) => "启用",
            Self::SetEnabled(false) => "停用",
            Self::Promote => "转正",
            Self::SetCommon(_) => "设置",
            Self::ResetEntry => "恢复",
        }
    }

    fn done_msg(self, n: usize) -> String {
        match self {
            Self::SetEnabled(true) => format!("已启用 {n} 条"),
            Self::SetEnabled(false) => format!("已停用 {n} 条"),
            Self::Promote => format!("已转正 {n} 条到用户词库"),
            Self::SetCommon(c) => format!("已将 {n} 个字设为{}字", common_label(c)),
            Self::ResetEntry => format!("已恢复 {n} 条默认"),
        }
    }
}

/// 一个批量动作的调用清单：`(勾选键, (方法, 参数))`，键用来在成功后移出勾选。
type Plan = Vec<((String, String), (&'static str, Value))>;

impl DictManagerState {
    /// 当前类别有哪些批量动作（按类别能力，不看勾了什么）。
    pub(crate) fn batch_actions(&self) -> Vec<BatchAction> {
        let spec = self.current_spec();
        let ops = spec.ops;
        let mut v = Vec::new();
        if !ops.multi_select {
            return v;
        }
        if ops.set_enabled {
            v.extend([
                BatchAction::SetEnabled(true),
                BatchAction::SetEnabled(false),
            ]);
        }
        if ops.promote {
            v.push(BatchAction::Promote);
        }
        if spec.category == WordCategory::CommonChars {
            // 生僻在前：同整类操作，来批量改的多半是想把一批字关掉。
            v.extend([BatchAction::SetCommon(false), BatchAction::SetCommon(true)]);
        }
        if ops.reset_entry {
            v.push(BatchAction::ResetEntry);
        }
        v
    }

    /// 一个批量动作对勾中的行要发的调用（纯函数，便于单测）。对某行没有意义的不发：
    /// 快捷输入的自建条目没有「默认」可回到。
    pub(crate) fn batch_plan(&self, act: BatchAction) -> Plan {
        let spec = self.current_spec();
        let cat = spec.category;
        let rpc = spec.rpc;
        let schema = self.current_schema_id();
        self.selected_rows()
            .into_iter()
            .filter_map(|r| {
                let call = match (act, cat) {
                    (BatchAction::SetEnabled(on), WordCategory::QuickFormat) => {
                        self.quick_call(rpc.set_enabled, &r, &[("enabled", json!(on))])
                    }
                    (BatchAction::SetEnabled(on), _) => {
                        if !row_ops(cat, &r).set_enabled {
                            return None;
                        }
                        (
                            rpc.set_enabled,
                            json!({ "code": r.code, "text": r.text, "enabled": on }),
                        )
                    }
                    (BatchAction::Promote, _) => {
                        let mut p = json!({ "code": r.code, "text": r.text });
                        if let Some(id) = &schema {
                            p["schemaId"] = json!(id);
                        }
                        (rpc.promote, p)
                    }
                    (BatchAction::SetCommon(c), WordCategory::CommonChars) => {
                        (rpc.update, json!({ "char": r.text, "common": c }))
                    }
                    (BatchAction::ResetEntry, WordCategory::QuickFormat) => {
                        if r.user {
                            return None;
                        }
                        self.quick_call(rpc.reset_entry, &r, &[])
                    }
                    (BatchAction::ResetEntry, WordCategory::CommonChars) => {
                        (rpc.reset_entry, json!({ "char": r.text }))
                    }
                    _ => return None,
                };
                Some((self.selection_key(&r), call))
            })
            .collect()
    }

    /// 工具栏「批量操作」的菜单项；当前类别没有批量动作时为空（按钮不出现）。
    pub fn batch_menu_items(&self) -> Vec<MenuItem> {
        self.batch_actions()
            .into_iter()
            .map(|act| {
                let n = self.batch_plan(act).len();
                let m = self.clone();
                MenuItem::run(act.label(), move |_| m.request_batch(act), false)
                    .subtitle(format!("{n} 条"))
                    .enabled(n > 0)
            })
            .collect()
    }

    /// 点了某个批量动作：恢复默认要确认（丢的是用户的调整），其余直接执行——同它们的
    /// 行内版本一样是点了就生效、随时能改回来的操作。
    pub fn request_batch(&self, act: BatchAction) {
        let n = self.batch_plan(act).len();
        if n == 0 {
            return;
        }
        if act != BatchAction::ResetEntry {
            return self.run_batch(act);
        }
        *self.pending.borrow_mut() = Some(PendingAction::Batch(act));
        self.confirm_msg
            .set(format!("将选中的 {n} 条恢复默认？对它们的调整会清掉。"));
        self.confirm_visible.set(true);
    }

    pub(super) fn run_batch(&self, act: BatchAction) {
        let (keys, calls): (Vec<_>, Vec<_>) = self.batch_plan(act).into_iter().unzip();
        if act == BatchAction::Promote {
            // 转正写进主词库：中栏的主词库条目数要重取。
            self.invalidate_user_libs();
        }
        let done = self.run_calls_until_err(calls, act.verb(), &act.done_msg(keys.len()));
        // 转正后这些行已不在临时词库里；其余动作行还在，勾选留着方便接着做别的。
        if act == BatchAction::Promote {
            for (a, b) in &keys[..done] {
                self.deselect(a, b);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::dict::spec::WordRow;
    use crate::pages::dict::state::{DOMAIN_COMMON, DOMAIN_PHRASE, DOMAIN_QUICK};

    fn at(domain: usize, tab: usize, cat: WordCategory) -> DictManagerState {
        let st = DictManagerState::new();
        st.domain.set(domain);
        st.sub_tab.set(tab);
        assert_eq!(st.current_category(), cat);
        st
    }

    fn quick_row(kind: &str, id: &str, user: bool) -> WordRow {
        WordRow {
            kind: kind.into(),
            code: id.into(),
            text: format!("$Y-{id}"),
            user,
            ..Default::default()
        }
    }

    /// 快捷输入：勾选按 (kind, id) 去重——两类里同名的 id 是两条。
    #[test]
    fn quick_selection_keys_by_kind_and_id() {
        let st = at(DOMAIN_QUICK, 0, WordCategory::QuickFormat);
        st.toggle_selected(&quick_row("date", "a", false));
        st.toggle_selected(&quick_row("time", "a", false));
        assert_eq!(st.selected_count.get(), 2);
    }

    /// 快捷输入：启停发给全部勾选行，按 (kind, id)；恢复默认与删除选中各自只挑出厂 / 自建。
    #[test]
    fn quick_batch_routes_by_row_origin() {
        let st = at(DOMAIN_QUICK, 0, WordCategory::QuickFormat);
        st.toggle_selected(&quick_row("date", "f", false));
        st.toggle_selected(&quick_row("date", "u", true));

        let off = st.batch_plan(BatchAction::SetEnabled(false));
        assert_eq!(off.len(), 2);
        assert_eq!(off[0].1.0, "quick.setEnabled");
        assert_eq!(off[0].1.1["kind"], "date");
        assert_eq!(off[0].1.1["enabled"], false);

        let reset = st.batch_plan(BatchAction::ResetEntry);
        assert_eq!(reset.len(), 1);
        assert_eq!(reset[0].1.0, "quick.resetEntry");
        assert_eq!(reset[0].1.1["id"], "f");

        let del = st.selected_delete_calls();
        assert_eq!(del.len(), 1, "出厂条目不能删");
        assert_eq!(del[0].0, "quick.delete");
        assert_eq!(del[0].1["id"], "u");
        assert!(st.can_delete_selected());
    }

    /// 只勾了出厂条目时不出「删除选中」。
    #[test]
    fn quick_factory_only_selection_cannot_delete() {
        let st = at(DOMAIN_QUICK, 0, WordCategory::QuickFormat);
        st.toggle_selected(&quick_row("date", "f", false));
        assert!(!st.can_delete_selected());
    }

    /// 短语启停按 (code, text)；系统短语也能多选，但没有删除。
    #[test]
    fn phrase_batch_enable() {
        let st = at(DOMAIN_PHRASE, 0, WordCategory::PhraseSystem);
        st.toggle_selected(&WordRow {
            code: "dh".into(),
            text: "电话".into(),
            ..Default::default()
        });
        let on = st.batch_plan(BatchAction::SetEnabled(true));
        assert_eq!(on.len(), 1);
        assert_eq!(on[0].1.0, "phrase.setEnabled");
        assert_eq!(on[0].1.1["code"], "dh");
        assert_eq!(on[0].1.1["enabled"], true);
        assert!(!st.can_delete_selected());
        assert_eq!(
            st.batch_actions(),
            vec![
                BatchAction::SetEnabled(true),
                BatchAction::SetEnabled(false)
            ]
        );
    }

    /// 常用字：设常用 / 生僻、恢复默认都按那个字。
    #[test]
    fn common_chars_batch() {
        let st = at(DOMAIN_COMMON, 0, WordCategory::CommonChars);
        st.toggle_selected(&WordRow {
            text: "㐀".into(),
            ..Default::default()
        });
        let rare = st.batch_plan(BatchAction::SetCommon(false));
        assert_eq!(rare[0].1.0, "commonChars.set");
        assert_eq!(rare[0].1.1, json!({ "char": "㐀", "common": false }));
        let reset = st.batch_plan(BatchAction::ResetEntry);
        assert_eq!(reset[0].1, ("commonChars.reset", json!({ "char": "㐀" })));
        assert!(!st.can_delete_selected(), "常用字没有删除");
    }
}
