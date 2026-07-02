//! 词库管理状态 — 导航（数据域/子标签）、当前类别数据加载与刷新。
//!
//! 复刻 [`crate::dialogs::schema_manager::SchemaManagerState`] 的范式：
//! `Rc<RefCell<>>` 存原始数据 + `Signal` 驱动响应式 + `list_signal` 即时刷新。
//! 挂进 `AppState`（与 `schema_mgr` 平级）以便对话框挂根、状态跨页重建存活。
//!
//! 所有写操作走直接 RPC 即时生效，不进入设置页的批量保存通道。

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

use crate::dialogs::schema_manager::engine_label;
use crate::rpc;
use crate::state::SchemaInfo;

use super::spec::{CategorySpec, LogicalTab, WordCategory, WordRow, spec_for};

/// 分页类别每页条数（对齐旧版 webview）。
pub const PAGE_SIZE: usize = 100;

/// 折叠后的一个数据域（拼音族聚合为一项）。
#[derive(Clone)]
pub struct DomainEntry {
    pub label: String,
    /// engineType（"pinyin"/"codetable"/"mixed"/其它）。
    pub kind: String,
    /// 代表方案 id（拼音族取全拼优先；短语域不用此字段）。
    pub rep_schema_id: String,
}

/// `list_signal` 的数据项：表头、一行词条，或空态提示。
#[derive(Clone)]
pub enum DictListItem {
    /// 列表头（列名随当前类别 spec 变化）。
    Header,
    /// 一行词条 + 页内序号（0 基）。
    Row(WordRow, usize),
    /// 空态提示文本（无数据/未连接/无方案）。
    Empty(String),
}

/// 词库管理状态（Signal 为 Copy，可直接 Clone 进闭包）。
#[derive(Clone)]
pub struct DictManagerState {
    /// 数据域：0 = 快捷短语（全局）；>=1 = `schemas[idx-1]`。
    pub domain: Signal<usize>,
    /// 方案域下的子标签：0=用户词库 1=临时词库 2=词频 3=候选调整。
    pub sub_tab: Signal<usize>,
    /// 数据域下拉的响应式选项（首项"快捷短语"，其后各方案；set_schemas 后填充）。
    pub domain_items: Signal<Vec<String>>,
    /// 可选方案列表（来自 `AppState.schemas`）。
    pub schemas: Rc<RefCell<Vec<SchemaInfo>>>,

    /// 当前类别原始行。
    pub rows: Rc<RefCell<Vec<WordRow>>>,
    /// 驱动 `list_signal` 的数据信号。
    pub list_data: Signal<Vec<DictListItem>>,
    /// 搜索框文本（可搜类别，由搜索框输入变化经响应式 effect 触发，见 [`Self::apply_search`]）。
    pub search: Signal<String>,
    /// 分页偏移（分页类别）。
    pub offset: Rc<RefCell<usize>>,
    /// 分页总数（状态/翻页按钮用）。
    pub total: Signal<usize>,
    /// 分页/计数状态文字（如"第 1-100 / 共 250 条"）。
    pub page_status: Signal<String>,
    /// 状态文字（操作反馈）。
    pub status: Signal<String>,
    /// toast 反馈通道：写操作只写此信号，view 侧 toast_sink 在 on_update 里 ctx.toast_*。
    /// (是否成功, 文案)。
    pub feedback: Signal<Option<(bool, String)>>,

    /// 确认对话框可见标志（清空/删除/恢复默认等破坏性操作）。
    pub confirm_visible: Signal<bool>,
    /// 确认对话框正文。
    pub confirm_msg: Signal<String>,
    /// 待确认的操作（确认后执行）。
    pending: Rc<RefCell<Option<PendingAction>>>,

    // ── 加/改词对话框（用户词库 / 短语）──
    pub edit_visible: Signal<bool>,
    pub edit_title: Signal<String>,
    /// 编码/缩写字段的标签（随类别）。
    pub edit_l_code: Signal<String>,
    /// 词条/短语字段的标签（随类别）。
    pub edit_l_text: Signal<String>,
    pub edit_code: Signal<String>,
    pub edit_text: Signal<String>,
    pub edit_weight: Signal<String>,
    /// 编辑模式的原始键 (code, text)；None = 新增模式。
    editing_orig: Rc<RefCell<Option<(String, String)>>>,

    // ── 系统短语只读查看对话框 ──
    pub view_visible: Signal<bool>,
    /// 查看对话框：缩写（独立于 edit_*，避免交叉污染）。
    pub view_code: Signal<String>,
    /// 查看对话框：内容。
    pub view_text: Signal<String>,
    /// 查看对话框：权重。
    pub view_weight: Signal<String>,

    // ── 候选调整手动添加/编辑对话框 ──
    pub shadow_add_visible: Signal<bool>,
    /// 对话框标题（"添加候选调整" / "编辑候选调整"）。
    pub shadow_title: Signal<String>,
    pub shadow_code: Signal<String>,
    pub shadow_word: Signal<String>,
    /// 0 = 固定位置(pin)，1 = 隐藏(hide)。
    pub shadow_type: Signal<usize>,
    pub shadow_pos: Signal<String>,
    /// 候选调整编辑模式的原始键 (code, word)；None = 新增模式。
    shadow_orig: Rc<RefCell<Option<(String, String)>>>,
}

/// 待确认的破坏性操作。
#[derive(Clone)]
enum PendingAction {
    /// 删除一行（code, text/word）。
    DeleteRow(String, String),
    /// 清空当前类别。
    Clear,
    /// 恢复默认（短语）。
    ResetDefault,
}

/// 当前数据域的类型（短语域或方案 kind）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DomainKind {
    Phrase,
    Pinyin,
    Codetable,
    Mixed,
    Other,
}

impl Default for DictManagerState {
    fn default() -> Self {
        Self::new()
    }
}

impl DictManagerState {
    pub fn new() -> Self {
        Self {
            domain: signal(0),
            sub_tab: signal(0),
            domain_items: signal(vec!["快捷短语".to_string()]),
            schemas: Rc::new(RefCell::new(Vec::new())),
            rows: Rc::new(RefCell::new(Vec::new())),
            list_data: signal(vec![]),
            search: signal(String::new()),
            offset: Rc::new(RefCell::new(0)),
            total: signal(0),
            page_status: signal(String::new()),
            status: signal(String::new()),
            feedback: signal(None),
            confirm_visible: signal(false),
            confirm_msg: signal(String::new()),
            pending: Rc::new(RefCell::new(None)),
            edit_visible: signal(false),
            edit_title: signal(String::new()),
            edit_l_code: signal("编码".to_string()),
            edit_l_text: signal("词条".to_string()),
            edit_code: signal(String::new()),
            edit_text: signal(String::new()),
            edit_weight: signal("0".to_string()),
            editing_orig: Rc::new(RefCell::new(None)),
            shadow_orig: Rc::new(RefCell::new(None)),
            view_visible: signal(false),
            view_code: signal(String::new()),
            view_text: signal(String::new()),
            view_weight: signal(String::new()),
            shadow_add_visible: signal(false),
            shadow_title: signal("添加候选调整".to_string()),
            shadow_code: signal(String::new()),
            shadow_word: signal(String::new()),
            shadow_type: signal(0),
            shadow_pos: signal("0".into()),
        }
    }

    /// 成功反馈（toast 绿）。
    pub fn notify_ok(&self, msg: &str) {
        self.feedback.set(Some((true, msg.to_string())));
    }
    /// 失败反馈（toast 红）。
    pub fn notify_err(&self, msg: &str) {
        self.feedback.set(Some((false, msg.to_string())));
    }

    // ── 域折叠 ───────────────────────────────────────────────

    /// 从 schemas 构建折叠后的方案域列表（不含短语域）。
    /// 拼音族（engineType=="pinyin"）聚合为单一"拼音"项；代表取非 shuangpin 优先（全拼/未标注）。
    /// 只在当前代表还是双拼时才升级为全拼，使代表确定为第一个全拼/非双拼。
    pub fn folded_domains(&self) -> Vec<DomainEntry> {
        let schemas = self.schemas.borrow();
        let mut out: Vec<DomainEntry> = Vec::new();
        let mut pinyin_seen = false;
        let mut pinyin_rep_is_shuangpin = false; // 当前代表是否还是双拼
        for s in schemas.iter() {
            if s.kind == "pinyin" {
                if pinyin_seen {
                    // 已有拼音域：只在当前代表是双拼且新方案非双拼时，才升级代表
                    if pinyin_rep_is_shuangpin && s.scheme != "shuangpin" {
                        if let Some(py) = out.iter_mut().find(|d| d.kind == "pinyin") {
                            py.rep_schema_id = s.id.clone();
                            pinyin_rep_is_shuangpin = false; // 代表已升级为非双拼
                        }
                    }
                    continue;
                }
                pinyin_seen = true;
                pinyin_rep_is_shuangpin = s.scheme == "shuangpin";
                out.push(DomainEntry {
                    label: "拼音".to_string(),
                    kind: "pinyin".to_string(),
                    rep_schema_id: s.id.clone(),
                });
            } else {
                let tag = engine_label(&s.kind);
                let name = if s.name.is_empty() { s.id.clone() } else { s.name.clone() };
                out.push(DomainEntry {
                    label: if tag == "未知" { name } else { format!("{name} · {tag}") },
                    kind: s.kind.clone(),
                    rep_schema_id: s.id.clone(),
                });
            }
        }
        out
    }

    // ── 导航解析 ─────────────────────────────────────────────

    /// 当前数据域类型。
    pub fn domain_kind(&self) -> DomainKind {
        let d = self.domain.get();
        if d == 0 {
            return DomainKind::Phrase;
        }
        match self.folded_domains().get(d - 1).map(|e| e.kind.as_str()) {
            Some("pinyin") => DomainKind::Pinyin,
            Some("codetable") => DomainKind::Codetable,
            Some("mixed") => DomainKind::Mixed,
            _ => DomainKind::Other,
        }
    }

    /// 当前域的子标签序列（按 kind 生成）。
    pub fn tabs_for_domain(&self) -> Vec<LogicalTab> {
        use LogicalTab::*;
        match self.domain_kind() {
            DomainKind::Phrase => vec![PhraseSystem, PhraseUser],
            DomainKind::Pinyin => vec![UserDict, Temp, Freq],
            DomainKind::Mixed => vec![Shadow],
            // 码表与未知：全四类（未知从宽，不隐藏数据）。
            DomainKind::Codetable | DomainKind::Other => vec![UserDict, Temp, Freq, Shadow],
        }
    }

    /// 当前选中的逻辑子标签（sub_tab 越界钳到末项）。
    pub fn current_tab(&self) -> LogicalTab {
        let tabs = self.tabs_for_domain();
        let i = self.sub_tab.get().min(tabs.len().saturating_sub(1));
        tabs[i]
    }

    /// 当前类别（由 domain + sub_tab 解析）。
    pub fn current_category(&self) -> WordCategory {
        self.current_tab().category()
    }

    /// 当前类别描述。
    pub fn current_spec(&self) -> &'static CategorySpec {
        spec_for(self.current_category())
    }

    /// 当前作用域方案 id（短语域返回 None；拼音域返回代表方案 id）。
    pub fn current_schema_id(&self) -> Option<String> {
        let d = self.domain.get();
        if d == 0 {
            return None;
        }
        self.folded_domains().get(d - 1).map(|e| e.rep_schema_id.clone())
    }

    /// 数据域下拉选项标签（首项"快捷短语"，其后折叠后各域）。
    pub fn domain_labels(&self) -> Vec<String> {
        let mut v = vec!["快捷短语".to_string()];
        for e in self.folded_domains() {
            v.push(e.label);
        }
        v
    }

    /// 方案子标签的标题列表。
    pub fn sub_tab_titles(&self) -> Vec<String> {
        self.tabs_for_domain().iter().map(|t| t.title().to_string()).collect()
    }

    // ── 分页守卫 ─────────────────────────────────────────────

    pub fn has_prev(&self) -> bool {
        *self.offset.borrow() >= PAGE_SIZE
    }

    pub fn has_next(&self) -> bool {
        *self.offset.borrow() + PAGE_SIZE < self.total.get()
    }

    // ── 数据加载 ─────────────────────────────────────────────

    /// 构建当前类别 list RPC 的参数（纯函数，便于单测）。
    pub fn build_list_params(&self) -> Value {
        let spec = self.current_spec();
        let mut p = serde_json::Map::new();
        if let Some(id) = self.current_schema_id() {
            p.insert("schemaId".into(), json!(id));
        }
        if spec.searchable {
            let q = self.search.get();
            if !q.is_empty() {
                // dict.listPaged 认 prefix/query，freq.listPaged 认 prefix：都带上。
                p.insert("prefix".into(), json!(q));
                p.insert("query".into(), json!(q));
            }
        }
        if spec.paged {
            p.insert("offset".into(), json!(*self.offset.borrow()));
            p.insert("limit".into(), json!(PAGE_SIZE));
        }
        Value::Object(p)
    }

    /// 把 list RPC 返回应用到 rows/total 并重建列表（纯函数，便于单测）。
    pub fn apply_rows(&self, v: &Value) {
        let spec = self.current_spec();
        let (rows, total) = WordRow::parse_list(spec.category, spec.paged, v);
        *self.rows.borrow_mut() = rows;
        self.total.set(total);
        self.rebuild_list();
    }

    /// 应用短语系统/用户过滤 + 客户端搜索过滤后的可见行（纯函数）。
    /// 分页类别不在此过滤（服务端已按 prefix 返回）。
    pub fn visible_rows(&self) -> Vec<WordRow> {
        let rows = self.rows.borrow();
        let phrase_sys = self.current_tab().phrase_system();
        let client_search = self.current_spec().client_search;
        let q = self.search.get();
        let q = q.trim().to_lowercase();
        rows.iter()
            .filter(|r| match phrase_sys {
                Some(sys) => r.is_system == sys,
                None => true,
            })
            .filter(|r| {
                if !client_search || q.is_empty() {
                    return true;
                }
                r.code.to_lowercase().contains(&q) || r.text.to_lowercase().contains(&q)
            })
            .cloned()
            .collect()
    }

    /// 重建 `list_data`（表头 + rows → 行项；空则空态项）并刷新分页状态文字。
    pub fn rebuild_list(&self) {
        let rows = self.visible_rows();
        let total = if self.current_spec().paged { self.total.get() } else { rows.len() };
        let spec = self.current_spec();

        // 分页/计数状态文字
        let page_status = if spec.paged {
            let off = *self.offset.borrow();
            let start = if rows.is_empty() { 0 } else { off + 1 };
            let end = off + rows.len();
            format!("第 {start}-{end} / 共 {total} 条")
        } else {
            format!("共 {total} 条")
        };
        self.page_status.set(page_status);

        let mut items = Vec::with_capacity(rows.len() + 2);
        items.push(DictListItem::Header);
        if rows.is_empty() {
            items.push(DictListItem::Empty(self.empty_hint()));
        } else {
            for (i, r) in rows.iter().enumerate() {
                items.push(DictListItem::Row(r.clone(), i));
            }
        }
        self.list_data.set(items);
    }

    /// 搜索框内容变化：分页类走服务端（归零分页+重载），非分页类走客户端重过滤。
    pub fn on_search_changed(&self) {
        if self.current_spec().paged {
            self.apply_search();
        } else {
            self.rebuild_list();
        }
    }

    fn empty_hint(&self) -> String {
        if self.current_spec().scoped_by_schema && self.current_schema_id().is_none() {
            "暂无已启用方案".to_string()
        } else {
            "暂无数据".to_string()
        }
    }

    /// 拉取当前类别数据（走真实/mock RPC），刷新列表。
    pub fn reload(&self) {
        let spec = self.current_spec();
        // 按方案类别但无方案：空表 + 提示。
        if spec.scoped_by_schema && self.current_schema_id().is_none() {
            self.rows.borrow_mut().clear();
            self.total.set(0);
            self.status.set("暂无已启用方案".into());
            self.rebuild_list();
            return;
        }
        let params = self.build_list_params();
        match rpc::call_core(spec.rpc.list, &params) {
            Ok(v) => {
                // 分页越界回收：末页删空后 offset 超出有效范围时回退到末页再取一次。
                if spec.paged {
                    let total = v.get("total").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                    let off = *self.offset.borrow();
                    if off > 0 && total <= off {
                        let max_off =
                            if total == 0 { 0 } else { (total - 1) / PAGE_SIZE * PAGE_SIZE };
                        *self.offset.borrow_mut() = max_off;
                        self.reload();
                        return;
                    }
                }
                // 成功不动 status：保留调用方（写操作）设置的反馈文字。
                self.apply_rows(&v);
            }
            Err(e) => {
                self.rows.borrow_mut().clear();
                self.total.set(0);
                self.status.set(format!("加载失败: {e}"));
                self.rebuild_list();
            }
        }
    }

    // ── 导航切换（切换后归零分页/搜索并刷新）─────────────────

    /// 注入方案列表（`AppState.apply_loaded` 后调用）。domain 越界则收敛回短语域。
    pub fn set_schemas(&self, schemas: &[SchemaInfo]) {
        *self.schemas.borrow_mut() = schemas.to_vec();
        self.domain_items.set(self.domain_labels());
        if self.domain.get() > self.folded_domains().len() {
            self.domain.set(0);
        }
        self.reload();
    }

    /// 越界时重置 sub_tab 为 0（回到首个子标签），而非钳到末项。
    /// 纯状态修正，便于单测（切域后子标签数量可能变少）。
    pub fn reset_sub_tab_if_out_of_range(&self) {
        let len = self.tabs_for_domain().len();
        if self.sub_tab.get() >= len {
            self.sub_tab.set(0);
        }
    }

    /// 导航（数据域/子标签）变化后：归零分页、清空搜索、重载。
    /// 由 view 侧的响应式 effect 在 domain/sub_tab 信号变化时调用。
    pub fn on_nav_changed(&self) {
        self.reset_sub_tab_if_out_of_range();
        *self.offset.borrow_mut() = 0;
        self.search.set(String::new());
        self.status.set(String::new());
        self.reload();
    }

    /// 归零分页并重载当前搜索框内容，供输入即搜与清除调用。
    pub fn apply_search(&self) {
        *self.offset.borrow_mut() = 0;
        self.reload();
    }

    pub fn next_page(&self) {
        if self.has_next() {
            *self.offset.borrow_mut() += PAGE_SIZE;
            self.reload();
        }
    }

    // ── 写操作（直接 RPC 即时生效 + 刷新）─────────────────────

    /// 作用域基参数（按方案类别带 schemaId，短语为空）。
    fn scoped_params(&self) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        if let Some(id) = self.current_schema_id() {
            m.insert("schemaId".into(), json!(id));
        }
        m
    }

    /// 统一写入：调 RPC，成功刷新并置反馈，失败置错误（纯 method/params，副作用集中于此）。
    fn write(&self, method: &str, params: Value, ok_msg: &str) {
        if method.is_empty() {
            self.status.set("该操作不可用".into());
            return;
        }
        match rpc::call_core(method, &params) {
            Ok(_) => {
                self.reload();
                self.notify_ok(ok_msg);
            }
            Err(e) => {
                // 失败也刷新：把乐观改动（如启用开关翻转）回退到服务端真实状态。
                self.notify_err(&format!("操作失败: {e}"));
                self.reload();
            }
        }
    }

    /// 删除一行要调用的 (方法, 参数)。候选调整走 removeRule（word 键），其余走 remove（text 键）。
    /// 纯函数，便于单测路由/键名。
    pub fn delete_call(&self, code: &str, text: &str) -> (&'static str, Value) {
        let spec = self.current_spec();
        let mut p = self.scoped_params();
        p.insert("code".into(), json!(code));
        if spec.ops.remove_rule {
            p.insert("word".into(), json!(text));
            (spec.rpc.remove_rule, Value::Object(p))
        } else {
            p.insert("text".into(), json!(text));
            (spec.rpc.remove, Value::Object(p))
        }
    }

    /// 删除/撤销一行（候选调整为撤销规则，其余为删除词条）。
    pub fn do_delete_row(&self, code: &str, text: &str) {
        let (method, params) = self.delete_call(code, text);
        let msg = if self.current_spec().ops.remove_rule { "已撤销" } else { "已删除" };
        self.write(method, params, msg);
    }

    /// 临时词转正到用户词库。
    pub fn do_promote(&self, code: &str, text: &str) {
        let spec = self.current_spec();
        let mut p = self.scoped_params();
        p.insert("code".into(), json!(code));
        p.insert("text".into(), json!(text));
        self.write(spec.rpc.promote, Value::Object(p), "已转正到用户词库");
    }

    /// 临时词全部转正。
    pub fn do_promote_all(&self) {
        let spec = self.current_spec();
        self.write(spec.rpc.promote_all, Value::Object(self.scoped_params()), "已全部转正");
    }

    /// 短语启用/停用开关。
    pub fn do_set_enabled(&self, code: &str, text: &str, enabled: bool) {
        let spec = self.current_spec();
        let mut p = self.scoped_params();
        p.insert("code".into(), json!(code));
        p.insert("text".into(), json!(text));
        p.insert("enabled".into(), json!(enabled));
        let msg = if enabled { "已启用" } else { "已停用" };
        self.write(spec.rpc.set_enabled, Value::Object(p), msg);
    }

    /// 清空当前类别：有 clear RPC 走之；候选调整无 clear，循环撤销所有规则。
    pub fn do_clear(&self) {
        let spec = self.current_spec();
        if !spec.rpc.clear.is_empty() {
            self.write(spec.rpc.clear, Value::Object(self.scoped_params()), "已清空");
        } else if spec.ops.remove_rule {
            let rows = self.rows.borrow().clone();
            let total = rows.len();
            let mut n = 0usize;
            for r in &rows {
                let mut p = self.scoped_params();
                p.insert("code".into(), json!(r.code));
                p.insert("word".into(), json!(r.text));
                if rpc::call_core(spec.rpc.remove_rule, &Value::Object(p)).is_ok() {
                    n += 1;
                }
            }
            self.reload();
            if n == total {
                self.notify_ok(&format!("已撤销 {n} 条规则"));
            } else {
                self.notify_ok(&format!("已撤销 {n} 条，{} 条失败", total - n));
            }
        } else {
            self.notify_err("该类别不支持清空");
        }
    }

    /// 恢复默认短语。
    pub fn do_reset_default(&self) {
        let spec = self.current_spec();
        self.write(spec.rpc.reset_default, json!({}), "已恢复默认短语");
    }

    // ── 破坏性操作确认 ───────────────────────────────────────

    pub fn request_delete_row(&self, code: &str, text: &str) {
        *self.pending.borrow_mut() = Some(PendingAction::DeleteRow(code.into(), text.into()));
        self.confirm_msg.set(format!("确定删除「{text}」？此操作不可恢复。"));
        self.confirm_visible.set(true);
    }

    pub fn request_clear(&self) {
        *self.pending.borrow_mut() = Some(PendingAction::Clear);
        let n = self.total.get();
        self.confirm_msg.set(format!("确定清空当前 {n} 条数据？此操作不可恢复。"));
        self.confirm_visible.set(true);
    }

    pub fn request_reset_default(&self) {
        *self.pending.borrow_mut() = Some(PendingAction::ResetDefault);
        self.confirm_msg.set("确定恢复为默认短语？将丢弃自定义短语的改动。".into());
        self.confirm_visible.set(true);
    }

    /// 执行待确认操作。
    pub fn confirm_pending(&self) {
        let act = self.pending.borrow_mut().take();
        self.confirm_visible.set(false);
        match act {
            Some(PendingAction::DeleteRow(c, t)) => self.do_delete_row(&c, &t),
            Some(PendingAction::Clear) => self.do_clear(),
            Some(PendingAction::ResetDefault) => self.do_reset_default(),
            None => {}
        }
    }

    pub fn cancel_confirm(&self) {
        *self.pending.borrow_mut() = None;
        self.confirm_visible.set(false);
    }

    // ── 加/改词对话框（用户词库 / 短语）─────────────────────

    /// 打开"添加"对话框（清空字段，按类别设标签与默认权重）。
    pub fn open_add(&self) {
        let (title, lc, lt, w) = match self.current_category() {
            WordCategory::UserDict => ("添加词条", "编码", "词条", "0"),
            WordCategory::PhraseUser => ("添加短语", "缩写", "短语", "1000"),
            _ => return,
        };
        *self.editing_orig.borrow_mut() = None;
        self.edit_code.set(String::new());
        self.edit_text.set(String::new());
        self.edit_weight.set(w.into());
        self.edit_title.set(title.into());
        self.edit_l_code.set(lc.into());
        self.edit_l_text.set(lt.into());
        self.edit_visible.set(true);
    }

    /// 打开"编辑"对话框（回填字段，记录原始键）。
    pub fn open_edit(&self, code: &str, text: &str, weight: i64) {
        let (title, lc, lt) = match self.current_category() {
            WordCategory::UserDict => ("编辑词条", "编码", "词条"),
            WordCategory::PhraseUser => ("编辑短语", "缩写", "短语"),
            _ => return,
        };
        *self.editing_orig.borrow_mut() = Some((code.into(), text.into()));
        self.edit_code.set(code.into());
        self.edit_text.set(text.into());
        self.edit_weight.set(weight.to_string());
        self.edit_title.set(title.into());
        self.edit_l_code.set(lc.into());
        self.edit_l_text.set(lt.into());
        self.edit_visible.set(true);
    }

    pub fn cancel_edit(&self) {
        self.edit_visible.set(false);
    }

    /// 打开系统短语只读查看对话框（写入独立 view_* 信号，不污染 edit_*）。
    pub fn open_view(&self, code: &str, text: &str, weight: i64) {
        self.view_code.set(code.into());
        self.view_text.set(text.into());
        self.view_weight.set(weight.to_string());
        self.view_visible.set(true);
    }

    pub fn cancel_view(&self) {
        self.view_visible.set(false);
    }

    /// 由"出码"按钮触发：用户词库下按词条反查编码填入编码框。
    pub fn encode_current(&self) {
        if self.current_category() != WordCategory::UserDict {
            return;
        }
        let text = self.edit_text.get();
        if text.trim().is_empty() {
            self.status.set("请先填写词条".into());
            return;
        }
        let mut p = self.scoped_params();
        p.insert("text".into(), json!(text));
        match rpc::call_core("dict.encode", &Value::Object(p)) {
            Ok(v) => {
                if let Some(code) = v.as_str() {
                    self.edit_code.set(code.to_string());
                }
            }
            Err(e) => self.notify_err(&format!("出码失败: {e}")),
        }
    }

    /// 计算"保存"要执行的 RPC 调用序列（纯函数，便于单测）。
    /// 用户词库：改键走删旧+加新，仅权重变走 update；短语：走 add / update。
    #[allow(clippy::type_complexity)]
    pub fn save_calls(&self) -> Result<(Vec<(&'static str, Value)>, &'static str), String> {
        let spec = self.current_spec();
        let cat = spec.category;
        let code = self.edit_code.get();
        let text = self.edit_text.get();
        let weight: i64 = match self.edit_weight.get().trim().parse() {
            Ok(w) => w,
            Err(_) => return Err("权重需为整数".into()),
        };
        let orig = self.editing_orig.borrow().clone();
        match cat {
            WordCategory::UserDict => {
                if text.trim().is_empty() {
                    return Err("词条不能为空".into());
                }
                if code.trim().is_empty() {
                    return Err("编码不能为空".into());
                }
                match orig {
                    // 仅权重变更 → update（upsert 改权重）
                    Some((oc, ot)) if oc == code && ot == text => {
                        let mut p = self.scoped_params();
                        p.insert("code".into(), json!(code));
                        p.insert("text".into(), json!(text));
                        p.insert("weight".into(), json!(weight));
                        Ok((vec![(spec.rpc.update, Value::Object(p))], "已更新"))
                    }
                    // 编码/词条变更 → 删旧 + 加新
                    Some((oc, ot)) => {
                        let mut rem = self.scoped_params();
                        rem.insert("code".into(), json!(oc));
                        rem.insert("text".into(), json!(ot));
                        let mut add = self.scoped_params();
                        add.insert("code".into(), json!(code));
                        add.insert("text".into(), json!(text));
                        add.insert("weight".into(), json!(weight));
                        // 先加新再删旧：add 失败则旧词条完好（避免删成功/加失败丢词）；
                        // 最坏 add 成功、remove 失败仅留一条重复，可再删。
                        Ok((
                            vec![
                                (spec.rpc.add, Value::Object(add)),
                                (spec.rpc.remove, Value::Object(rem)),
                            ],
                            "已更新",
                        ))
                    }
                    None => {
                        let mut p = self.scoped_params();
                        p.insert("code".into(), json!(code));
                        p.insert("text".into(), json!(text));
                        p.insert("weight".into(), json!(weight));
                        Ok((vec![(spec.rpc.add, Value::Object(p))], "已添加"))
                    }
                }
            }
            WordCategory::PhraseUser => {
                if code.trim().is_empty() {
                    return Err("缩写不能为空".into());
                }
                if text.trim().is_empty() {
                    return Err("短语不能为空".into());
                }
                match orig {
                    Some((oc, ot)) => {
                        let p = json!({
                            "code": oc, "text": ot,
                            "newCode": code, "newText": text, "weight": weight
                        });
                        Ok((vec![(spec.rpc.update, p)], "已更新"))
                    }
                    None => {
                        let p = json!({ "code": code, "text": text, "position": 0, "weight": weight });
                        Ok((vec![(spec.rpc.add, p)], "已添加"))
                    }
                }
            }
            _ => Err("该类别不支持编辑".into()),
        }
    }

    /// 顺序执行一组写调用；全部成功才刷新并置反馈，任一失败即停并报错。
    fn run_calls(&self, calls: Vec<(&str, Value)>, ok_msg: &str) {
        for (m, p) in &calls {
            if let Err(e) = rpc::call_core(m, p) {
                self.notify_err(&format!("操作失败: {e}"));
                return;
            }
        }
        self.reload();
        self.notify_ok(ok_msg);
    }

    /// 提交加/改词对话框。
    pub fn submit_word(&self) {
        match self.save_calls() {
            Ok((calls, ok)) => {
                self.edit_visible.set(false);
                self.run_calls(calls, ok);
            }
            Err(e) => self.notify_err(&e),
        }
    }

    pub fn prev_page(&self) {
        if self.has_prev() {
            let cur = *self.offset.borrow();
            *self.offset.borrow_mut() = cur.saturating_sub(PAGE_SIZE);
            self.reload();
        }
    }

    // ── 候选调整手动添加 ─────────────────────────────────────

    /// 打开候选调整添加对话框（清空字段）。
    pub fn open_shadow_add(&self) {
        self.shadow_title.set("添加候选调整".into());
        self.shadow_code.set(String::new());
        self.shadow_word.set(String::new());
        self.shadow_type.set(0);
        self.shadow_pos.set("0".into());
        *self.shadow_orig.borrow_mut() = None;
        self.shadow_add_visible.set(true);
    }

    /// 打开候选调整编辑对话框（预填该行现值，记录原始键）。
    /// kind 为 "delete" 映射到隐藏(1)，其余（"pin"）映射到固定位置(0)。
    /// 提交走 `submit_shadow` → `shadow_save_calls`：键不变 addRule upsert 覆盖，
    /// 改了 code/word 则加新 + 删旧原子替换。
    pub fn open_shadow_edit(&self, code: &str, word: &str, kind: &str, position: i64) {
        self.shadow_title.set("编辑候选调整".into());
        self.shadow_code.set(code.into());
        self.shadow_word.set(word.into());
        self.shadow_type.set(if kind == "delete" { 1 } else { 0 });
        self.shadow_pos.set(position.to_string());
        *self.shadow_orig.borrow_mut() = Some((code.into(), word.into()));
        self.shadow_add_visible.set(true);
    }

    pub fn cancel_shadow_add(&self) {
        self.shadow_add_visible.set(false);
    }

    /// 导出调用（用户短语）。返回 RPC 方法名（内容由 view 层拿到后写文件）。
    pub fn export_method(&self) -> &'static str {
        "phrase.export"
    }

    /// 导入调用参数（内容来自文件）。
    pub fn import_params(&self, content: &str) -> Value {
        json!({ "content": content })
    }

    /// 导出用户短语到文件（native save 对话框）。
    pub fn do_export_phrases(&self) {
        let path = match PickDialog::new()
            .title("导出短语")
            .file_name("phrases.wdict.yaml")
            .filter("WindDict", &["yaml"])
            .save_file()
        {
            Some(p) => p,
            None => return, // 用户取消
        };
        match rpc::call_core(self.export_method(), &json!({})) {
            Ok(v) => {
                let content = v.get("content").and_then(|c| c.as_str()).unwrap_or("");
                match std::fs::write(&path, content) {
                    Ok(_) => self.notify_ok("已导出"),
                    Err(e) => self.notify_err(&format!("写文件失败: {e}")),
                }
            }
            Err(e) => self.notify_err(&format!("导出失败: {e}")),
        }
    }

    /// 从文件导入用户短语（native open 对话框 + 合并）。
    pub fn do_import_phrases(&self) {
        let path = match PickDialog::new()
            .title("导入短语")
            .filter("WindDict", &["yaml"])
            .pick_file()
        {
            Some(p) => p,
            None => return,
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                self.notify_err(&format!("读文件失败: {e}"));
                return;
            }
        };
        match rpc::call_core("phrase.import", &self.import_params(&content)) {
            Ok(v) => {
                let n = v.get("imported").and_then(|x| x.as_u64()).unwrap_or(0);
                let m = v.get("skipped").and_then(|x| x.as_u64()).unwrap_or(0);
                self.reload();
                self.notify_ok(&format!("已导入 {n}（跳过 {m}）"));
            }
            Err(e) => self.notify_err(&format!("导入失败: {e}")),
        }
    }

    /// 构建候选调整添加的 (方法, 参数)（纯函数，便于单测）。
    pub fn shadow_add_call(&self) -> Result<(&'static str, Value), String> {
        let code = self.shadow_code.get();
        let word = self.shadow_word.get();
        if code.trim().is_empty() {
            return Err("编码不能为空".into());
        }
        if word.trim().is_empty() {
            return Err("词条不能为空".into());
        }
        let mut p = self.scoped_params();
        p.insert("code".into(), json!(code));
        p.insert("word".into(), json!(word));
        if self.shadow_type.get() == 0 {
            let pos: i64 = self.shadow_pos.get().trim().parse().map_err(|_| "位置需为整数")?;
            p.insert("type".into(), json!("pin"));
            p.insert("position".into(), json!(pos));
        } else {
            p.insert("type".into(), json!("hide"));
        }
        Ok(("shadow.addRule", Value::Object(p)))
    }

    /// 候选调整保存调用序列（纯函数，便于单测）。
    /// 新增/键不变编辑：单 addRule（同 code+word upsert 覆盖）；
    /// 编辑改了 code/word：先加新再删旧（加失败则旧规则完好，避免删成功/加失败丢规则）。
    pub fn shadow_save_calls(&self) -> Result<(Vec<(&'static str, Value)>, &'static str), String> {
        let (m, p) = self.shadow_add_call()?;
        let orig = self.shadow_orig.borrow().clone();
        let mut calls = vec![(m, p)];
        let msg = match orig {
            None => "已添加规则",
            Some((oc, ow)) => {
                if oc != self.shadow_code.get() || ow != self.shadow_word.get() {
                    let mut rem = self.scoped_params();
                    rem.insert("code".into(), json!(oc));
                    rem.insert("word".into(), json!(ow));
                    calls.push(("shadow.removeRule", Value::Object(rem)));
                }
                "已更新规则"
            }
        };
        Ok((calls, msg))
    }

    /// 提交候选调整添加/编辑。
    pub fn submit_shadow(&self) {
        match self.shadow_save_calls() {
            Ok((calls, msg)) => {
                self.shadow_add_visible.set(false);
                self.run_calls(calls, msg);
            }
            Err(e) => self.notify_err(&e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(id: &str, name: &str, kind: &str) -> SchemaInfo {
        SchemaInfo { id: id.into(), name: name.into(), kind: kind.into(), ..Default::default() }
    }

    fn schema_s(id: &str, name: &str, kind: &str, scheme: &str) -> SchemaInfo {
        SchemaInfo { id: id.into(), name: name.into(), kind: kind.into(), scheme: scheme.into() }
    }

    /// 直接写 schemas/domain，不经 set_schemas（避免触发 rpc）。
    fn with_schemas(st: &DictManagerState, list: Vec<SchemaInfo>) {
        *st.schemas.borrow_mut() = list;
    }

    #[test]
    fn default_domain_is_phrase_global() {
        let st = DictManagerState::new();
        assert_eq!(st.current_category(), WordCategory::PhraseSystem);
        assert!(st.current_schema_id().is_none());
        assert!(!st.current_spec().scoped_by_schema);
    }

    #[test]
    fn navigation_resolves_category_and_schema() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable"), schema("py", "全拼", "pinyin")]);

        // domain 1 = 第一个方案，子标签 0 = 用户词库
        st.domain.set(1);
        st.sub_tab.set(0);
        assert_eq!(st.current_category(), WordCategory::UserDict);
        assert_eq!(st.current_schema_id().as_deref(), Some("wb"));

        // 子标签 3 = 候选调整
        st.sub_tab.set(3);
        assert_eq!(st.current_category(), WordCategory::Shadow);

        // domain 2 = 第二个方案
        st.domain.set(2);
        assert_eq!(st.current_schema_id().as_deref(), Some("py"));
    }

    #[test]
    fn domain_labels_prefix_phrase_then_schemas() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔86", "codetable"), schema("py", "全拼", "pinyin")]);
        let labels = st.domain_labels();
        assert_eq!(labels[0], "快捷短语");
        assert_eq!(labels[1], "五笔86 · 码表");
        // 拼音方案折叠后统一显示"拼音"（不再拼接各方案的 name · 拼音）
        assert_eq!(labels[2], "拼音");
    }

    #[test]
    fn build_params_userdict_has_schema_and_paging() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(0); // 用户词库
        let p = st.build_list_params();
        assert_eq!(p["schemaId"], json!("wb"));
        assert_eq!(p["offset"], json!(0));
        assert_eq!(p["limit"], json!(PAGE_SIZE));
        assert!(p.get("prefix").is_none(), "空搜索不带 prefix");
    }

    #[test]
    fn build_params_userdict_with_search() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(0);
        st.search.set("工".into());
        let p = st.build_list_params();
        assert_eq!(p["prefix"], json!("工"));
        assert_eq!(p["query"], json!("工"));
    }

    #[test]
    fn build_params_phrase_is_bare() {
        let st = DictManagerState::new(); // domain 0 = 短语
        let p = st.build_list_params();
        assert!(p.get("schemaId").is_none(), "短语全局无 schemaId");
        assert!(p.get("offset").is_none(), "短语不分页");
    }

    #[test]
    fn apply_rows_paged_reads_total_and_builds_list() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(0); // 用户词库（分页）
        let v = json!({ "items": [
            { "code": "aaaa", "text": "工", "weight": 0 },
            { "code": "ggkg", "text": "王中", "weight": 5 }
        ], "total": 120 });
        st.apply_rows(&v);
        assert_eq!(st.rows.borrow().len(), 2);
        assert_eq!(st.total.get(), 120);
        // list_data = 表头 + 两行
        let items = st.list_data.get();
        assert_eq!(items.len(), 3);
        assert!(matches!(items[0], DictListItem::Header));
        assert!(matches!(items[1], DictListItem::Row(_, 0)));
        // 分页状态文字
        assert_eq!(st.page_status.get(), "第 1-2 / 共 120 条");
    }

    #[test]
    fn apply_rows_empty_yields_empty_item() {
        let st = DictManagerState::new(); // 短语（非分页）
        st.apply_rows(&json!([]));
        assert!(st.rows.borrow().is_empty());
        // 表头 + 空态项
        let items = st.list_data.get();
        assert_eq!(items.len(), 2);
        assert!(matches!(items[0], DictListItem::Header));
        assert!(matches!(items[1], DictListItem::Empty(_)));
        assert_eq!(st.page_status.get(), "共 0 条");
    }

    #[test]
    fn pagination_guards() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(0);
        st.total.set(250);
        *st.offset.borrow_mut() = 0;
        assert!(!st.has_prev());
        assert!(st.has_next());
        *st.offset.borrow_mut() = 100;
        assert!(st.has_prev());
        assert!(st.has_next());
        *st.offset.borrow_mut() = 200;
        assert!(st.has_prev());
        assert!(!st.has_next(), "200+100 不小于 250");
    }

    #[test]
    fn delete_call_dict_uses_remove_with_text_and_schema() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(0); // 用户词库
        let (method, params) = st.delete_call("aaaa", "工");
        assert_eq!(method, "dict.remove");
        assert_eq!(params["schemaId"], json!("wb"));
        assert_eq!(params["code"], json!("aaaa"));
        assert_eq!(params["text"], json!("工"));
        assert!(params.get("word").is_none());
    }

    #[test]
    fn delete_call_shadow_uses_remove_rule_with_word() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(3); // 候选调整
        let (method, params) = st.delete_call("wm", "我们");
        assert_eq!(method, "shadow.removeRule");
        assert_eq!(params["word"], json!("我们"), "shadow 用 word 键");
        assert!(params.get("text").is_none());
    }

    #[test]
    fn delete_call_phrase_has_no_schema() {
        let st = DictManagerState::new(); // domain 0 = 短语
        st.sub_tab.set(1); // 用户短语（有 remove RPC）
        let (method, params) = st.delete_call("bj", "北京");
        assert_eq!(method, "phrase.remove");
        assert!(params.get("schemaId").is_none(), "短语全局无 schemaId");
        assert_eq!(params["text"], json!("北京"));
    }

    #[test]
    fn request_delete_sets_pending_and_shows_dialog() {
        let st = DictManagerState::new();
        st.request_delete_row("bj", "北京");
        assert!(st.confirm_visible.get());
        assert!(st.confirm_msg.get().contains("北京"));
        assert!(matches!(*st.pending.borrow(), Some(PendingAction::DeleteRow(_, _))));
        // 取消清空待办
        st.cancel_confirm();
        assert!(!st.confirm_visible.get());
        assert!(st.pending.borrow().is_none());
    }

    #[test]
    fn request_clear_and_reset_set_pending() {
        let st = DictManagerState::new();
        st.total.set(42);
        st.request_clear();
        assert!(st.confirm_msg.get().contains("42"));
        assert!(matches!(*st.pending.borrow(), Some(PendingAction::Clear)));

        st.request_reset_default();
        assert!(matches!(*st.pending.borrow(), Some(PendingAction::ResetDefault)));
    }

    #[test]
    fn save_calls_dict_add() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(0); // 用户词库
        st.edit_code.set("aaaa".into());
        st.edit_text.set("工".into());
        st.edit_weight.set("5".into());
        // 新增（editing_orig = None）
        let (calls, msg) = st.save_calls().unwrap();
        assert_eq!(msg, "已添加");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "dict.add");
        assert_eq!(calls[0].1["schemaId"], json!("wb"));
        assert_eq!(calls[0].1["weight"], json!(5));
    }

    #[test]
    fn save_calls_dict_edit_weight_only_uses_update() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(0);
        st.open_edit("aaaa", "工", 0);
        st.edit_weight.set("9".into()); // 仅改权重
        let (calls, _) = st.save_calls().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "dict.update");
        assert_eq!(calls[0].1["weight"], json!(9));
    }

    #[test]
    fn save_calls_dict_edit_changed_key_removes_then_adds() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(0);
        st.open_edit("aaaa", "工", 0);
        st.edit_text.set("工人".into()); // 词条变更
        let (calls, _) = st.save_calls().unwrap();
        assert_eq!(calls.len(), 2);
        // 先加新、再删旧（避免删成功/加失败丢词）
        assert_eq!(calls[0].0, "dict.add");
        assert_eq!(calls[0].1["text"], json!("工人")); // 加新
        assert_eq!(calls[1].0, "dict.remove");
        assert_eq!(calls[1].1["text"], json!("工")); // 删旧
    }

    #[test]
    fn save_calls_phrase_add_and_edit() {
        let st = DictManagerState::new(); // domain 0 = 短语
        st.sub_tab.set(1); // 用户短语（支持 add/edit）
        st.edit_code.set("bj".into());
        st.edit_text.set("北京".into());
        st.edit_weight.set("1".into());
        let (calls, _) = st.save_calls().unwrap();
        assert_eq!(calls[0].0, "phrase.add");
        assert!(calls[0].1.get("schemaId").is_none());

        st.open_edit("bj", "北京", 1);
        st.edit_text.set("北京市".into());
        let (calls2, _) = st.save_calls().unwrap();
        assert_eq!(calls2[0].0, "phrase.update");
        assert_eq!(calls2[0].1["newText"], json!("北京市"));
        assert_eq!(calls2[0].1["text"], json!("北京")); // 原键
    }

    #[test]
    fn save_calls_validates_empty() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(0);
        st.edit_code.set(String::new());
        st.edit_text.set(String::new());
        assert!(st.save_calls().is_err(), "空词条应校验失败");
    }

    #[test]
    fn save_calls_rejects_non_numeric_weight() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(0);
        st.edit_code.set("aaaa".into());
        st.edit_text.set("工".into());
        st.edit_weight.set("abc".into()); // 非数字
        assert!(st.save_calls().is_err(), "非数字权重应校验失败");
    }

    #[test]
    fn scoped_category_without_schema_shows_hint() {
        let st = DictManagerState::new();
        // 无方案，强制进入方案域（domain=1 但 schemas 空）
        st.domain.set(1);
        assert!(st.current_schema_id().is_none());
        st.reload(); // 应短路为空表 + 提示，不触发有效 rpc
        assert!(st.rows.borrow().is_empty());
        assert_eq!(st.status.get(), "暂无已启用方案");
    }

    #[test]
    fn tabs_matrix_by_kind() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![
            schema("wb", "五笔", "codetable"),
            schema("py", "全拼", "pinyin"),
            schema("mx", "混输", "mixed"),
        ]);
        // 短语域：系统 + 用户
        st.domain.set(0);
        assert_eq!(st.tabs_for_domain(), vec![LogicalTab::PhraseSystem, LogicalTab::PhraseUser]);
        // 码表：四类含候选调整
        st.domain.set(1);
        assert_eq!(st.tabs_for_domain(),
            vec![LogicalTab::UserDict, LogicalTab::Temp, LogicalTab::Freq, LogicalTab::Shadow]);
        // 拼音：三类无候选调整
        st.domain.set(2);
        assert_eq!(st.tabs_for_domain(),
            vec![LogicalTab::UserDict, LogicalTab::Temp, LogicalTab::Freq]);
        // 混输：仅候选调整
        st.domain.set(3);
        assert_eq!(st.tabs_for_domain(), vec![LogicalTab::Shadow]);
    }

    #[test]
    fn current_category_follows_dynamic_tabs() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("mx", "混输", "mixed")]);
        st.domain.set(1); // 混输
        st.sub_tab.set(0);
        assert_eq!(st.current_category(), WordCategory::Shadow);
        // 短语用户页
        st.domain.set(0);
        st.sub_tab.set(1);
        assert_eq!(st.current_tab(), LogicalTab::PhraseUser);
        assert_eq!(st.current_category(), WordCategory::PhraseUser);
    }

    #[test]
    fn phrase_two_level_filter_by_is_system() {
        let st = DictManagerState::new(); // domain 0 = 短语
        *st.rows.borrow_mut() = vec![
            WordRow { code: "bj".into(), text: "北京".into(), is_system: true, ..Default::default() },
            WordRow { code: "me".into(), text: "我的".into(), is_system: false, ..Default::default() },
        ];
        st.sub_tab.set(0); // 系统短语
        let vis = st.visible_rows();
        assert_eq!(vis.len(), 1);
        assert_eq!(vis[0].text, "北京");
        st.sub_tab.set(1); // 用户短语
        assert_eq!(st.visible_rows()[0].text, "我的");
    }

    #[test]
    fn client_search_filters_temp_rows() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(1); // 临时词库（client_search=true）
        *st.rows.borrow_mut() = vec![
            WordRow { code: "nihao".into(), text: "你好".into(), ..Default::default() },
            WordRow { code: "women".into(), text: "我们".into(), ..Default::default() },
        ];
        st.search.set("women".into());
        let vis = st.visible_rows();
        assert_eq!(vis.len(), 1);
        assert_eq!(vis[0].text, "我们");
        // 匹配词条也命中
        st.search.set("你".into());
        assert_eq!(st.visible_rows()[0].code, "nihao");
    }

    #[test]
    fn notify_sets_feedback_signal() {
        let st = DictManagerState::new();
        st.notify_ok("已添加");
        assert_eq!(st.feedback.get(), Some((true, "已添加".to_string())));
        st.notify_err("失败了");
        assert_eq!(st.feedback.get(), Some((false, "失败了".to_string())));
    }

    #[test]
    fn shadow_add_call_pin_carries_position() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(3); // 候选调整
        st.shadow_code.set("wm".into());
        st.shadow_word.set("我们".into());
        st.shadow_type.set(0); // 固定位置
        st.shadow_pos.set("2".into());
        let (m, p) = st.shadow_add_call().unwrap();
        assert_eq!(m, "shadow.addRule");
        assert_eq!(p["schemaId"], json!("wb"));
        assert_eq!(p["code"], json!("wm"));
        assert_eq!(p["word"], json!("我们"));
        assert_eq!(p["type"], json!("pin"));
        assert_eq!(p["position"], json!(2));
    }

    #[test]
    fn shadow_add_call_hide_omits_position() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(3);
        st.shadow_code.set("nh".into());
        st.shadow_word.set("你好".into());
        st.shadow_type.set(1); // 隐藏
        let (m, p) = st.shadow_add_call().unwrap();
        assert_eq!(m, "shadow.addRule");
        assert_eq!(p["type"], json!("hide"));
        assert!(p.get("position").is_none(), "隐藏不带 position");
    }

    #[test]
    fn shadow_add_call_validates_empty() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(3);
        st.shadow_code.set(String::new());
        assert!(st.shadow_add_call().is_err());
    }

    #[test]
    fn shadow_save_calls_add_mode_single_call() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(3); // 候选调整
        st.open_shadow_add();
        st.shadow_code.set("wm".into());
        st.shadow_word.set("我们".into());
        st.shadow_type.set(0);
        st.shadow_pos.set("2".into());
        let (calls, msg) = st.shadow_save_calls().unwrap();
        assert_eq!(msg, "已添加规则");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shadow.addRule");
        assert_eq!(calls[0].1["code"], json!("wm"));
    }

    #[test]
    fn shadow_save_calls_edit_same_key_single_upsert() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(3);
        st.open_shadow_edit("wm", "我们", "pin", 2);
        st.shadow_pos.set("5".into()); // 仅改位置，键不变
        let (calls, msg) = st.shadow_save_calls().unwrap();
        assert_eq!(msg, "已更新规则");
        assert_eq!(calls.len(), 1, "键不变时 addRule upsert 覆盖即可");
        assert_eq!(calls[0].0, "shadow.addRule");
        assert_eq!(calls[0].1["position"], json!(5));
    }

    #[test]
    fn shadow_save_calls_edit_changed_key_adds_then_removes_old() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(3);
        st.open_shadow_edit("wm", "我们", "pin", 2);
        st.shadow_word.set("我门".into()); // 改词条 → 键变更
        let (calls, msg) = st.shadow_save_calls().unwrap();
        assert_eq!(msg, "已更新规则");
        assert_eq!(calls.len(), 2, "键变更需替换：加新 + 删旧");
        // 先加新、再删旧（加失败则旧规则完好）
        assert_eq!(calls[0].0, "shadow.addRule");
        assert_eq!(calls[0].1["word"], json!("我门"));
        assert_eq!(calls[1].0, "shadow.removeRule");
        assert_eq!(calls[1].1["code"], json!("wm"));
        assert_eq!(calls[1].1["word"], json!("我们"));
        assert_eq!(calls[1].1["schemaId"], json!("wb"));
    }

    #[test]
    fn open_shadow_edit_prefills_signals_and_sets_visible() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(3); // 候选调整

        // pin 类型 → 固定位置(0)
        st.open_shadow_edit("ab", "阿", "pin", 2);
        assert!(st.shadow_add_visible.get(), "编辑后 shadow_add_visible=true");
        assert_eq!(st.shadow_code.get(), "ab");
        assert_eq!(st.shadow_word.get(), "阿");
        assert_eq!(st.shadow_type.get(), 0, "pin → 固定位置(0)");
        assert_eq!(st.shadow_pos.get(), "2");
        assert_eq!(st.shadow_title.get(), "编辑候选调整");

        // delete 类型 → 隐藏(1)
        st.shadow_add_visible.set(false);
        st.open_shadow_edit("cd", "屏", "delete", 0);
        assert!(st.shadow_add_visible.get());
        assert_eq!(st.shadow_type.get(), 1, "delete → 隐藏(1)");
        assert_eq!(st.shadow_word.get(), "屏");
        assert_eq!(st.shadow_title.get(), "编辑候选调整");

        // 添加后标题恢复
        st.open_shadow_add();
        assert_eq!(st.shadow_title.get(), "添加候选调整");
        assert_eq!(st.shadow_code.get(), "", "open_shadow_add 清空字段");
    }

    #[test]
    fn open_view_sets_visible_and_signals() {
        let st = DictManagerState::new();
        st.open_view("bj", "北京", 1000);
        assert!(st.view_visible.get(), "open_view 后 view_visible=true");
        // 写入独立 view_* 信号，不污染 edit_*
        assert_eq!(st.view_code.get(), "bj");
        assert_eq!(st.view_text.get(), "北京");
        assert_eq!(st.view_weight.get(), "1000");
        // edit_* 信号不受影响
        assert_eq!(st.edit_code.get(), "", "open_view 不写 edit_code");
        assert_eq!(st.edit_text.get(), "", "open_view 不写 edit_text");
        // editing_orig 不写，仍为 None
        assert!(st.editing_orig.borrow().is_none(), "查看不设 editing_orig");
        st.cancel_view();
        assert!(!st.view_visible.get(), "cancel_view 后 view_visible=false");
    }

    #[test]
    fn open_add_phrase_user_weight_1000() {
        let st = DictManagerState::new();
        st.sub_tab.set(1); // 用户短语
        assert_eq!(st.current_category(), WordCategory::PhraseUser);
        st.open_add();
        assert_eq!(st.edit_weight.get(), "1000");
    }

    #[test]
    fn phrase_user_list_params_paged() {
        let st = DictManagerState::new();
        st.domain.set(0);
        st.sub_tab.set(1); // 用户短语（分页）
        let p = st.build_list_params();
        assert_eq!(p["offset"], serde_json::json!(0));
        assert_eq!(p["limit"], serde_json::json!(PAGE_SIZE));
        assert!(p.get("schemaId").is_none(), "短语全局无 schemaId");
    }

    #[test]
    fn phrase_system_list_params_bare() {
        let st = DictManagerState::new();
        st.domain.set(0);
        st.sub_tab.set(0); // 系统短语（不分页）
        let p = st.build_list_params();
        assert!(p.get("offset").is_none(), "系统短语不分页");
    }

    #[test]
    fn nav_change_clamps_out_of_range_sub_tab() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("mx", "混输", "mixed"), schema("wb", "五笔", "codetable")]);
        // 先在码表选到候选调整(3)，切到混输(仅1项) → 应重置到 0
        st.domain.set(2);
        st.sub_tab.set(3);
        st.domain.set(1); // 混输
        st.reset_sub_tab_if_out_of_range(); // 见实现：抽出纯收敛逻辑便于测试
        assert_eq!(st.sub_tab.get(), 0);
    }

    #[test]
    fn pinyin_family_folds_into_one_domain() {
        let st = DictManagerState::new();
        // 两个拼音方案(全拼+双拼) + 一个码表
        with_schemas(&st, vec![
            schema_s("pinyin_simp", "全拼·简", "pinyin", "full"),
            schema_s("double_pinyin", "自然码双拼", "pinyin", "shuangpin"),
            schema("wubi86", "五笔86", "codetable"),
        ]);
        let labels = st.domain_labels();
        // 域：快捷短语 + 拼音(折叠) + 五笔86 = 3 项（而非 4）
        assert_eq!(labels, vec!["快捷短语".to_string(), "拼音".to_string(), "五笔86 · 码表".to_string()]);
        // 选"拼音"域，current_schema_id 返回代表(全拼优先)
        st.domain.set(1);
        st.sub_tab.set(0);
        assert_eq!(st.current_schema_id().as_deref(), Some("pinyin_simp"));
    }

    #[test]
    fn shuangpin_only_folds_with_shuangpin_rep() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema_s("double_pinyin", "自然码双拼", "pinyin", "shuangpin")]);
        st.domain.set(1);
        // 无全拼时代表取该双拼
        assert_eq!(st.current_schema_id().as_deref(), Some("double_pinyin"));
    }

    #[test]
    fn shuangpin_before_full_upgrades_rep() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![
            schema_s("double_pinyin", "自然码双拼", "pinyin", "shuangpin"),
            schema_s("pinyin_simp", "全拼·简", "pinyin", "full"),
        ]);
        st.domain.set(1);
        // 双拼在前、全拼在后 → 代表应升级为全拼
        assert_eq!(st.current_schema_id().as_deref(), Some("pinyin_simp"));
        // 仍折叠为一个"拼音"域
        assert_eq!(st.domain_labels(), vec!["快捷短语".to_string(), "拼音".to_string()]);
    }
}
