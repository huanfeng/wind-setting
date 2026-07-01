//! 词库管理状态 — 导航（数据域/子标签）、当前类别数据加载与刷新。
//!
//! 复刻 [`crate::dialogs::schema_manager::SchemaManagerState`] 的范式：
//! `Rc<RefCell<>>` 存原始数据 + `Signal` 驱动响应式 + `list_signal` 即时刷新。
//! 挂进 `AppState`（与 `schema_mgr` 平级）以便对话框挂根、状态跨页重建存活。
//!
//! 所有写操作走直接 RPC 即时生效（阶段 4/5 接入），不进入设置页的批量保存通道。
#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

use crate::dialogs::schema_manager::engine_label;
use crate::rpc;
use crate::state::SchemaInfo;

use super::spec::{CategorySpec, SCHEMA_SUB_CATEGORIES, WordCategory, WordRow, spec_for};

/// 分页类别每页条数（对齐旧版 webview）。
pub const PAGE_SIZE: usize = 100;

/// `list_signal` 的数据项：一行词条，或空态提示。
#[derive(Clone)]
pub enum DictListItem {
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
    /// 可选方案列表（来自 `AppState.schemas`）。
    pub schemas: Rc<RefCell<Vec<SchemaInfo>>>,

    /// 当前类别原始行。
    pub rows: Rc<RefCell<Vec<WordRow>>>,
    /// 驱动 `list_signal` 的数据信号。
    pub list_data: Signal<Vec<DictListItem>>,
    /// 搜索框文本（可搜类别，防抖后触发 reload）。
    pub search: Signal<String>,
    /// 分页偏移（分页类别）。
    pub offset: Rc<RefCell<usize>>,
    /// 分页总数（状态/翻页按钮用）。
    pub total: Signal<usize>,
    /// 状态文字（操作反馈）。
    pub status: Signal<String>,
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
            schemas: Rc::new(RefCell::new(Vec::new())),
            rows: Rc::new(RefCell::new(Vec::new())),
            list_data: signal(vec![]),
            search: signal(String::new()),
            offset: Rc::new(RefCell::new(0)),
            total: signal(0),
            status: signal(String::new()),
        }
    }

    // ── 导航解析 ─────────────────────────────────────────────

    /// 当前类别（由 domain + sub_tab 解析）。
    pub fn current_category(&self) -> WordCategory {
        if self.domain.get() == 0 {
            WordCategory::Phrase
        } else {
            SCHEMA_SUB_CATEGORIES[self.sub_tab.get().min(SCHEMA_SUB_CATEGORIES.len() - 1)]
        }
    }

    /// 当前类别描述。
    pub fn current_spec(&self) -> &'static CategorySpec {
        spec_for(self.current_category())
    }

    /// 当前作用域方案 id（短语域返回 None）。
    pub fn current_schema_id(&self) -> Option<String> {
        let d = self.domain.get();
        if d == 0 {
            return None;
        }
        self.schemas.borrow().get(d - 1).map(|s| s.id.clone())
    }

    /// 数据域下拉选项标签（首项"快捷短语"，其后各方案）。
    pub fn domain_labels(&self) -> Vec<String> {
        let mut v = vec!["快捷短语".to_string()];
        for s in self.schemas.borrow().iter() {
            let tag = engine_label(&s.kind);
            let name = if s.name.is_empty() { s.id.clone() } else { s.name.clone() };
            v.push(if tag == "未知" { name } else { format!("{name} · {tag}") });
        }
        v
    }

    /// 方案子标签的标题列表。
    pub fn sub_tab_titles(&self) -> Vec<String> {
        SCHEMA_SUB_CATEGORIES
            .iter()
            .map(|c| spec_for(*c).title.to_string())
            .collect()
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

    /// 重建 `list_data`（rows → 行项；空则空态项）。
    pub fn rebuild_list(&self) {
        let rows = self.rows.borrow();
        let mut items = Vec::with_capacity(rows.len().max(1));
        if rows.is_empty() {
            items.push(DictListItem::Empty(self.empty_hint()));
        } else {
            for (i, r) in rows.iter().enumerate() {
                items.push(DictListItem::Row(r.clone(), i));
            }
        }
        self.list_data.set(items);
    }

    fn empty_hint(&self) -> String {
        let s = self.status.get();
        if s.is_empty() { "暂无数据".to_string() } else { s }
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
                self.status.set(String::new());
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
        if self.domain.get() > schemas.len() {
            self.domain.set(0);
        }
        self.reload();
    }

    pub fn set_domain(&self, i: usize) {
        self.domain.set(i);
        self.sub_tab.set(0);
        *self.offset.borrow_mut() = 0;
        self.search.set(String::new());
        self.reload();
    }

    pub fn set_sub_tab(&self, i: usize) {
        self.sub_tab.set(i);
        *self.offset.borrow_mut() = 0;
        self.search.set(String::new());
        self.reload();
    }

    /// 设置搜索词并刷新（可搜类别；view 侧做防抖）。
    pub fn set_search(&self, q: String) {
        self.search.set(q);
        *self.offset.borrow_mut() = 0;
        self.reload();
    }

    pub fn next_page(&self) {
        if self.has_next() {
            *self.offset.borrow_mut() += PAGE_SIZE;
            self.reload();
        }
    }

    pub fn prev_page(&self) {
        if self.has_prev() {
            let cur = *self.offset.borrow();
            *self.offset.borrow_mut() = cur.saturating_sub(PAGE_SIZE);
            self.reload();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(id: &str, name: &str, kind: &str) -> SchemaInfo {
        SchemaInfo { id: id.into(), name: name.into(), kind: kind.into() }
    }

    /// 直接写 schemas/domain，不经 set_schemas（避免触发 rpc）。
    fn with_schemas(st: &DictManagerState, list: Vec<SchemaInfo>) {
        *st.schemas.borrow_mut() = list;
    }

    #[test]
    fn default_domain_is_phrase_global() {
        let st = DictManagerState::new();
        assert_eq!(st.current_category(), WordCategory::Phrase);
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
        assert_eq!(labels[2], "全拼 · 拼音");
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
        // list_data 两行
        let items = st.list_data.get();
        assert_eq!(items.len(), 2);
        assert!(matches!(items[0], DictListItem::Row(_, 0)));
    }

    #[test]
    fn apply_rows_empty_yields_empty_item() {
        let st = DictManagerState::new(); // 短语（非分页）
        st.apply_rows(&json!([]));
        assert!(st.rows.borrow().is_empty());
        let items = st.list_data.get();
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], DictListItem::Empty(_)));
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
    fn scoped_category_without_schema_shows_hint() {
        let st = DictManagerState::new();
        // 无方案，强制进入方案域（domain=1 但 schemas 空）
        st.domain.set(1);
        assert!(st.current_schema_id().is_none());
        st.reload(); // 应短路为空表 + 提示，不触发有效 rpc
        assert!(st.rows.borrow().is_empty());
        assert_eq!(st.status.get(), "暂无已启用方案");
    }
}
