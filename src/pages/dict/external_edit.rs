//! 「外部编辑」对话框：导出到文件 → 在系统编辑器里改 → 试算 → 确认加载。
//!
//! # 为什么是一个对话框，而不是工具栏上的「新建类」「从文件加载」两枚按钮
//!
//! 第一版把两端拆成两个按钮：导出后编辑器开了，回来还得点「从文件加载」再在文件选择器里
//! 找到刚才那个文件。导出和回读明明是同一件事的两半，却让用户自己记路径（2026-09-07
//! 用户试用后的反馈）。现在两端都在一个对话框里：路径显示着，导出、打开、加载各一键。
//!
//! # 领域无关的壳
//!
//! 对话框本身不认识「字符类」。它只依赖一份 [`ExternalEditDomain`]——三个 RPC 方法名加
//! 一个把返回值变成一句话的函数。core 侧的三件套语义：
//!
//! ```text
//! probe   <domain>.editFile   {key?}         → {key, name, builtin, path, exists, modified}
//! export  <domain>.exportEdit {key?}         → {path}
//! import  <domain>.importFile {path, apply}  → 领域自定的摘要（apply=false 只试算）
//! ```
//!
//! `key` 缺省 = 「新建」，它是目标下拉的**首项**（[`ExternalEditDomain::new_label`]）。
//! 其它词库要接同一个对话框，core 照这组语义实现三个方法，这里填一份
//! `ExternalEditDomain` 即可——不要再各写一套导出/回读流程。
//!
//! # 两处「覆盖提示」
//!
//! - **导出侧**：编辑文件已存在时不静默覆盖。探测报出修改时间，按钮变成「打开已有文件」
//!   与「重新导出」——上次改到一半关了窗，回来应该能接着改。
//! - **加载侧**：先 `apply=false` 试算，把「将更新「常用汉字」：+3 字 −2 字」摆出来，
//!   点「确认加载」才写。加载是整份替换该类的用户层，而用户看不见库里现在是什么。

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::core::EventCtx;
use windui::prelude::*;

use super::state::DictManagerState;
use crate::rpc;

/// 一个数据域接入「外部编辑」对话框所需的全部信息。
pub struct ExternalEditDomain {
    /// 对话框标题。
    pub title: &'static str,
    pub probe: &'static str,
    pub export: &'static str,
    pub import: &'static str,
    /// 三个方法里目标的参数名。
    pub key_param: &'static str,
    /// 下拉**首项**的「新建…」项；`None` = 本域不支持从模板新建。
    ///
    /// ★ 2026-09-18 从末项挪到首项（用户反馈：新建藏在 50 多个已有类后面，找不着）。
    /// 新建是「列表里没有我要的目标」时的出路，放在开头才够得着；放末尾等于要求用户
    /// 先把整份清单滚完才发现它。
    pub new_label: Option<&'static str>,
    /// 「选择其它文件…」的文件过滤。
    pub file_filter: (&'static str, &'static [&'static str]),
    /// 把 import 的返回值变成一句话。`applied=false` 是试算（「将更新…」），`true` 是已写
    /// （「已更新…」）。
    pub summarize: fn(&Value, bool) -> String,
}

/// 字符类。
pub const CHARSET_DOMAIN: ExternalEditDomain = ExternalEditDomain {
    title: "外部编辑 · 字符类",
    probe: "charset.editFile",
    export: "charset.exportEdit",
    import: "charset.importFile",
    key_param: "key",
    new_label: Some("新建类…"),
    file_filter: ("YAML", &["yaml", "yml"]),
    summarize: charset_import_summary,
};

/// 对话框的全部状态。挂在 [`DictManagerState`] 上的一个字段，本身可克隆（信号是句柄）。
#[derive(Clone)]
pub struct ExternalEditState {
    pub visible: Signal<bool>,
    pub title: Signal<String>,
    /// 目标下拉的文案（与 `targets` 对齐）。
    pub labels: Signal<Vec<String>>,
    pub idx: Signal<usize>,
    /// 当前对准的文件路径（探测得到，或用户「选择其它文件」）。
    pub path: Signal<String>,
    /// 一句话现状/结果。
    pub status: Signal<String>,
    /// 编辑文件已存在 ⇒ 按钮组切成「打开已有文件 / 重新导出」。
    pub exists: Signal<bool>,
    /// 试算完成、等用户确认。此态下下拉与其它动作都锁住。
    pub pending: Signal<bool>,
    /// 下拉各项对应的目标 key；`None` = 新建。
    targets: Rc<RefCell<Vec<Option<String>>>>,
    domain: Rc<RefCell<Option<&'static ExternalEditDomain>>>,
    /// 试算过、等确认的那个文件。
    pending_path: Rc<RefCell<Option<PathBuf>>>,
    /// 上一次探测时的下拉下标。目标下拉只写信号、无变更回调，靠 effect 轮询版本号；
    /// 而 `idx.set` 即使值没变也会走一遍 effect——不记这个，打开对话框会探测两次，
    /// 「确认加载」后重装下拉会把刚写好的「已更新…」状态行冲掉。
    probed_idx: Rc<Cell<Option<usize>>>,
}

impl Default for ExternalEditState {
    fn default() -> Self {
        Self::new()
    }
}

impl ExternalEditState {
    pub fn new() -> Self {
        Self {
            visible: signal(false),
            title: signal(String::new()),
            labels: signal(Vec::new()),
            idx: signal(0),
            path: signal(String::new()),
            status: signal(String::new()),
            exists: signal(false),
            pending: signal(false),
            targets: Rc::new(RefCell::new(Vec::new())),
            domain: Rc::new(RefCell::new(None)),
            pending_path: Rc::new(RefCell::new(None)),
            probed_idx: Rc::new(Cell::new(None)),
        }
    }

    fn domain(&self) -> Option<&'static ExternalEditDomain> {
        *self.domain.borrow()
    }

    /// 当前对准的目标 key；`None` = 新建（或列表为空）。
    pub fn current_key(&self) -> Option<String> {
        self.targets.borrow().get(self.idx.get()).cloned().flatten()
    }

    /// 三个方法共用的目标参数：新建时**不带** key。
    fn key_params(&self, d: &ExternalEditDomain) -> Value {
        match self.current_key() {
            Some(k) => json!({ d.key_param: k }),
            None => json!({}),
        }
    }
}

impl DictManagerState {
    /// 打开对话框。`targets` 是可选的目标 (key, 显示名) 列表——字符类传当前全部行；
    /// `preselect` 为右键/行内按钮带来的那一行，`None` 落在首项。
    pub fn xedit_open(
        &self,
        domain: &'static ExternalEditDomain,
        targets: &[(String, String)],
        preselect: Option<&str>,
    ) {
        let x = &self.xedit;
        x.title.set(domain.title.to_string());
        x.pending.set(false);
        *x.pending_path.borrow_mut() = None;
        // 先装目标再显示：下标变化会经响应式 effect 触发一次探测（见
        // `on_xedit_target_changed`），但下标没变（比如连续两次都开首项）时 effect 不响，
        // 所以这里总要探测一次。重复探测只是多一次只读 RPC。
        self.xedit_set_targets(domain, targets, preselect);
        x.visible.set(true);
        self.xedit_probe();
    }

    /// 装目标列表（**首项**按域插入「新建…」项）并选中 `want`。纯状态，不碰 RPC。
    ///
    /// 两条落点规则，⚠️ 分开写不是啰嗦——「新建」搬到首项之后，原来那句
    /// `unwrap_or(0)` 的含义从「落第一个已有类」变成了「落新建」，两种 `want` 得区别对待：
    ///
    /// | `want` | 落点 | 为什么 |
    /// |---|---|---|
    /// | `None`（工具栏入口，没指名目标） | 首项 = 「新建…」 | 无目标可言时用户多半是要建新的 |
    /// | `Some(k)` 但找不到（类刚被删/改了 key） | 第一个**真实**目标 | 指名要编辑 X 却落到新建，下一步「重新导出」导出的就是模板，用户看不出自己编辑的已经不是 X |
    ///
    /// ⚠️ 上表按「对话框入口」的语境写，但 [`Self::xedit_refresh_targets`]（加载后重建
    /// 列表）走的是同一个函数：它的 `want` 取 `current_key()`，当前选中就是「新建」且
    /// 导入结果为空时 `want` 也是 `None` ⇒ 按上表落回「新建」。行为无害（用户本来就停在
    /// 新建那一项），但别以为 `None` 只来自工具栏。
    fn xedit_set_targets(
        &self,
        domain: &'static ExternalEditDomain,
        targets: &[(String, String)],
        want: Option<&str>,
    ) {
        let x = &self.xedit;
        *x.domain.borrow_mut() = Some(domain);
        let mut keys: Vec<Option<String>> = Vec::with_capacity(targets.len() + 1);
        let mut labels: Vec<String> = Vec::with_capacity(targets.len() + 1);
        if let Some(l) = domain.new_label {
            keys.push(None);
            labels.push(l.to_string());
        }
        keys.extend(targets.iter().map(|(k, _)| Some(k.clone())));
        labels.extend(targets.iter().map(|(_, n)| n.clone()));
        let idx = match want {
            Some(p) => keys
                .iter()
                .position(|k| k.as_deref() == Some(p))
                // 找不到就退到第一个真实目标；一个都没有（空列表）时才落 0。
                .or_else(|| keys.iter().position(|k| k.is_some()))
                .unwrap_or(0),
            None => 0,
        };
        *x.targets.borrow_mut() = keys;
        x.labels.set(labels);
        x.idx.set(idx);
    }

    pub fn xedit_close(&self) {
        self.xedit.probed_idx.set(None);
        self.xedit.visible.set(false);
        self.xedit.pending.set(false);
        *self.xedit.pending_path.borrow_mut() = None;
    }

    /// 目标下拉变化 → 重新探测。挂在 `reactive_effect` 上；对话框没开、或下标其实没变
    /// （见 `probed_idx`）时不动。
    pub fn on_xedit_target_changed(&self) {
        let x = &self.xedit;
        if x.visible.get() && x.probed_idx.get() != Some(x.idx.get()) {
            self.xedit_probe();
        }
    }

    /// 探测当前目标的编辑文件：更新路径、存在与否、一句话现状。不写文件。
    fn xedit_probe(&self) {
        let x = &self.xedit;
        let Some(d) = x.domain() else { return };
        x.pending.set(false);
        *x.pending_path.borrow_mut() = None;
        x.probed_idx.set(Some(x.idx.get()));
        match rpc::call_core(d.probe, &x.key_params(d)) {
            Ok(v) => {
                let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                x.path.set(s("path"));
                let exists = v.get("exists").and_then(|b| b.as_bool()).unwrap_or(false);
                x.exists.set(exists);
                x.status.set(Self::probe_status(exists, &s("modified")));
            }
            Err(e) => {
                x.path.set(String::new());
                x.exists.set(false);
                x.status.set(format!("探测失败：{e}"));
            }
        }
    }

    /// 探测结果的一句话。纯函数便于单测。
    pub fn probe_status(exists: bool, modified: &str) -> String {
        if !exists {
            return "尚未导出。点「外部文件编辑」导出并在编辑器里打开。".into();
        }
        if modified.is_empty() {
            "已有上次导出的文件：可直接打开继续改，或「重新导出」用当前生效的内容覆盖它。".into()
        } else {
            format!(
                "已有上次导出的文件（{modified}）：可直接打开继续改，或「重新导出」用当前生效的内容覆盖它。"
            )
        }
    }

    /// 「外部文件编辑」：文件不存在就导出再打开；已存在就直接打开（不覆盖）。
    pub fn xedit_edit(&self) {
        if self.xedit.exists.get() {
            self.xedit_open_file();
        } else {
            self.xedit_export_and_open();
        }
    }

    /// 「重新导出」：用当前生效的内容覆盖已有文件，再打开。
    pub fn xedit_export_and_open(&self) {
        let x = &self.xedit;
        let Some(d) = x.domain() else { return };
        match rpc::call_core(d.export, &x.key_params(d)) {
            Ok(v) => {
                if let Some(p) = v.get("path").and_then(|p| p.as_str()) {
                    x.path.set(p.to_string());
                }
                x.exists.set(true);
                self.xedit_open_file();
            }
            Err(e) => x.status.set(format!("导出失败：{e}")),
        }
    }

    /// 用系统默认程序打开当前路径。
    fn xedit_open_file(&self) {
        let x = &self.xedit;
        let path = x.path.get();
        if path.is_empty() {
            x.status.set("还没有文件路径".into());
            return;
        }
        match crate::open_external::open_with_default_app(std::path::Path::new(&path)) {
            Ok(()) => x
                .status
                .set("已在编辑器里打开。改完保存后，回到这里点「加载外部文件」。".into()),
            Err(e) => x.status.set(format!("打不开 {path}：{e}")),
        }
    }

    /// 「加载外部文件」：对当前路径试算，把将发生的变化摆出来等确认。
    pub fn xedit_load(&self) {
        let path = self.xedit.path.get();
        if path.is_empty() {
            self.xedit.status.set("还没有文件路径".into());
            return;
        }
        self.xedit_preview(PathBuf::from(path));
    }

    /// 「选择其它文件…」：别处拿到的文件也走同一条试算 → 确认的路。
    pub fn xedit_pick_other(&self, ctx: &mut EventCtx) {
        let Some(d) = self.xedit.domain() else { return };
        let state = self.clone();
        ctx.request_pick_file(
            PickDialog::new()
                .title(d.title)
                .filter(d.file_filter.0, d.file_filter.1),
            move |path| {
                let Some(path) = path else { return };
                state.xedit.path.set(path.to_string_lossy().into_owned());
                state.xedit_preview(path);
            },
        );
    }

    fn xedit_preview(&self, path: PathBuf) {
        let x = &self.xedit;
        let Some(d) = x.domain() else { return };
        let params = json!({ "path": path.to_string_lossy(), "apply": false });
        match rpc::call_core(d.import, &params) {
            Ok(v) => {
                x.status
                    .set(format!("{}。确认写入？", (d.summarize)(&v, false)));
                *x.pending_path.borrow_mut() = Some(path);
                x.pending.set(true);
            }
            Err(e) => {
                x.pending.set(false);
                *x.pending_path.borrow_mut() = None;
                x.status.set(format!("文件有问题，没有加载：{e}"));
            }
        }
    }

    /// 试算后的「确认加载」：真正写入，刷新列表，把目标列表也刷一遍（可能新建了类）。
    pub fn xedit_confirm(&self) {
        let x = &self.xedit;
        let Some(d) = x.domain() else { return };
        let Some(path) = x.pending_path.borrow_mut().take() else {
            x.pending.set(false);
            return;
        };
        x.pending.set(false);
        let params = json!({ "path": path.to_string_lossy(), "apply": true });
        match rpc::call_core(d.import, &params) {
            Ok(v) => {
                self.reload();
                self.xedit_refresh_targets(&v);
                x.status.set((d.summarize)(&v, true));
                self.notify_ok(&(d.summarize)(&v, true));
            }
            Err(e) => x.status.set(format!("加载失败：{e}")),
        }
    }

    /// 试算后的「取消」：回到探测态。
    pub fn xedit_cancel_pending(&self) {
        self.xedit_probe();
    }

    /// 加载后按最新的行重建目标列表；选中项跟着 key 走，新建的类选中它自己。
    /// 下标可能没变（同一个类），此时 effect 不响，路径也不需要变；变了则 effect 会探测。
    fn xedit_refresh_targets(&self, imported: &Value) {
        let x = &self.xedit;
        let Some(d) = x.domain() else { return };
        let want = x.current_key().or_else(|| {
            imported
                .get("classes")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.get("key"))
                .and_then(|k| k.as_str())
                .map(str::to_string)
        });
        let rows: Vec<(String, String)> = self
            .rows
            .borrow()
            .iter()
            .map(|r| (r.code.clone(), r.text.clone()))
            .collect();
        self.xedit_set_targets(d, &rows, want.as_deref());
    }
}

/// 字符类加载结果的一句话摘要。纯函数便于单测。
///
/// 单个类时说清「更新了出厂类」还是「新建了类」——两者用户的下一步不同：前者去看那一行
/// 的变化，后者去找新行。`applied=false` 是试算，用「将」而不是「已」。
///
/// ⚠️ 别在文案里写「到列表末尾找」：列表按 `order` 升序，新建类模板给的 `order` 比
/// 出厂类都小（见 core 的 `charset_export_template`），新行落在**开头**。
pub fn charset_import_summary(v: &Value, applied: bool) -> String {
    let classes = v
        .get("classes")
        .and_then(|c| c.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let n = |c: &Value, k: &str| c.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let tense = if applied { "已" } else { "将" };
    match classes {
        [] => "文件里没有字符类".into(),
        [c] => {
            let name = c.get("name").and_then(|x| x.as_str()).unwrap_or("?");
            let what = if c.get("builtin").and_then(|x| x.as_bool()).unwrap_or(false) {
                "更新"
            } else {
                "新建"
            };
            format!(
                "{tense}{what}「{name}」：+{} 字 −{} 字，{} 个字段",
                n(c, "added"),
                n(c, "removed"),
                n(c, "fields")
            )
        }
        many => format!("{tense}加载 {} 个字符类", many.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charset_import_summary_distinguishes_update_from_create_and_tense() {
        let updated = json!({ "classes": [
            { "key": "emoji", "name": "Emoji 表情", "builtin": true, "added": 1, "removed": 2, "fields": 1 }
        ] });
        assert_eq!(
            charset_import_summary(&updated, true),
            "已更新「Emoji 表情」：+1 字 −2 字，1 个字段"
        );
        assert_eq!(
            charset_import_summary(&updated, false),
            "将更新「Emoji 表情」：+1 字 −2 字，1 个字段"
        );

        let created = json!({ "classes": [
            { "key": "mine", "name": "我的符号", "builtin": false, "added": 3, "removed": 0, "fields": 2 }
        ] });
        assert_eq!(
            charset_import_summary(&created, true),
            "已新建「我的符号」：+3 字 −0 字，2 个字段"
        );

        let many = json!({ "classes": [{}, {}, {}] });
        assert_eq!(charset_import_summary(&many, false), "将加载 3 个字符类");
        assert_eq!(
            charset_import_summary(&json!({}), true),
            "文件里没有字符类",
            "缺 classes 字段也不能 panic"
        );
    }

    /// 探测过的下标再被 `set` 一次（值没变）不算变化：effect 会因版本号跳而进来，
    /// 这里得挡住，否则「确认加载」后重装下拉会把结果状态行冲掉。
    #[test]
    fn target_change_is_judged_by_index_not_by_signal_version() {
        let st = DictManagerState::new();
        st.xedit.visible.set(true);
        st.xedit.idx.set(1);
        st.xedit.probed_idx.set(Some(1));
        // 没有 domain ⇒ 真探测会直接返回；这里只验证判据本身。
        assert_eq!(st.xedit.probed_idx.get(), Some(1));
        st.xedit.idx.set(1);
        assert_eq!(st.xedit.probed_idx.get() != Some(st.xedit.idx.get()), false);
        st.xedit.idx.set(2);
        assert!(st.xedit.probed_idx.get() != Some(st.xedit.idx.get()));
        st.xedit_close();
        assert_eq!(st.xedit.probed_idx.get(), None, "关了再开必须重新探测");
    }

    #[test]
    fn probe_status_tells_the_user_what_to_press() {
        assert!(DictManagerState::probe_status(false, "").contains("外部文件编辑"));
        let s = DictManagerState::probe_status(true, "2026-09-07 11:20");
        assert!(s.contains("2026-09-07 11:20") && s.contains("重新导出"));
        assert!(DictManagerState::probe_status(true, "").contains("重新导出"));
    }

    /// 装目标：预选那一行；**首项**是「新建类…」；新建项的 key 是 None，
    /// 参数里因此**不带** key（core 据此出模板）。走纯状态的 `xedit_set_targets`，
    /// 不经 `xedit_open`——后者会探测（RPC）。
    #[test]
    fn open_preselects_the_row_and_puts_the_new_item_first() {
        let st = DictManagerState::new();
        let targets = vec![
            ("emoji".to_string(), "Emoji 表情".to_string()),
            ("common_han".to_string(), "常用汉字".to_string()),
        ];
        st.xedit_set_targets(&CHARSET_DOMAIN, &targets, Some("common_han"));
        assert_eq!(st.xedit.idx.get(), 2);
        assert_eq!(
            st.xedit.labels.get(),
            vec!["新建类…", "Emoji 表情", "常用汉字"]
        );
        assert_eq!(st.xedit.current_key().as_deref(), Some("common_han"));

        st.xedit.idx.set(0);
        assert_eq!(st.xedit.current_key(), None, "首项是新建");
        assert_eq!(st.xedit.key_params(&CHARSET_DOMAIN), json!({}));

        st.xedit.visible.set(true);
        st.xedit_close();
        assert!(!st.xedit.visible.get());
    }

    /// ★★ 两种落点分开测——「新建」搬到首项后，`unwrap_or(0)` 的含义变了。
    ///
    /// 没指名目标（工具栏入口）落新建；指名了却找不到（类刚被删）必须落第一个**真实**
    /// 目标：落到新建的话，用户下一步点「重新导出」导出的是模板，而他以为自己在编辑
    /// 那个类——两者在对话框里长得一模一样，只有路径那一行能看出差别。
    #[test]
    fn missing_target_falls_back_to_a_real_one_not_to_new() {
        let st = DictManagerState::new();
        let targets = vec![
            ("emoji".to_string(), "Emoji 表情".to_string()),
            ("common_han".to_string(), "常用汉字".to_string()),
        ];

        st.xedit_set_targets(&CHARSET_DOMAIN, &targets, None);
        assert_eq!(st.xedit.idx.get(), 0);
        assert_eq!(st.xedit.current_key(), None, "没指名 ⇒ 对准新建");

        st.xedit_set_targets(&CHARSET_DOMAIN, &targets, Some("no_such"));
        assert_eq!(
            st.xedit.current_key().as_deref(),
            Some("emoji"),
            "指名不到 ⇒ 落第一个真实目标，不是新建"
        );

        // 一个真实目标都没有时才允许落到新建（否则没有可落之处）。
        st.xedit_set_targets(&CHARSET_DOMAIN, &[], Some("no_such"));
        assert_eq!(st.xedit.idx.get(), 0);
        assert_eq!(st.xedit.current_key(), None);
    }
}
