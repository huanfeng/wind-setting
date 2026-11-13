//! 「系统词库」（主仓设计 `system-dict-patch-and-candidate-adjust.md` §6.1）：中栏一个分组标题 + 一个条目，
//! 右栏两级，都走词库窗的表格管线（`state::reload` → `table_sortable_server`）。
//!
//! - 中栏：用户词库分组下方一个分组标题「系统词库」与其下的条目「词库列表」（无计数），见 `category_nav`。
//! - 一级 [`WordCategory::SysDicts`]：词库表（主词库 + 分类 / 附加 / 我放入的库），列 启用 /
//!   名称 / 分组 / 条数 / 优先级，默认按生效先后（core 给的 `order`），表头可客户端排序。启用列
//!   勾选即生效，主库与写了 `default` 的库禁用。行操作「查看」进二级、「信息」弹头部信息；
//!   双击行等同「查看」。
//! - 二级 [`WordCategory::SysEntries`]：「‹ 返回」+「系统词库 · <名>」+ 只读词条表（服务端分页、
//!   跳页、表头排序、单框搜索编码前缀或词子串），行操作「加进用户词库」复用加词对话框。
//!
//! 两级都不是子标签：不进导出，见 `state::tabs_for_kind` 的说明。
//!
//! 开关与分库开关同一套：对域内**有这张库**的每个成员方案各调一次 `schema.setDictEnabled`，
//! 任一失败就把已改的改回原值（[`DictManagerState::set_sys_cat_enabled`]）。清单取自 core
//! `sysDict.categories`，切到别的方案或收到 `schema.configChanged` 时重取；正在看的那本库
//! 不在新清单里就回一级。
//!
//! 搜索与用户词库同一个搜索框、同样输入即搜，但系统词条每搜一次 core 要整扫词库，故停手
//! [`SEARCH_DEBOUNCE`] 后才发（回车立即发），见 [`build_search_debounce`]。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use windui::prelude::*;

use super::dialogs::{dialog_body, dialog_footer, dialog_frame, dialog_header};
use super::spec::{SYS_NARROW_BELOW, WordCategory, WordField, WordRow};
use super::state::{DictManagerState, PAGE_SIZE};
use crate::rpc;

/// 中栏分组标题、右栏一级标题与面包屑首段。
pub(crate) const SYS_TITLE: &str = "系统词库";
/// 方案文件没写 `group` 的随包库归这一组。
pub(crate) const OTHER_GROUP: &str = "其他";
/// 发现目录（`dir:` 前缀）登记的库归这一组，也是它的来源名。
pub(crate) const DIR_GROUP: &str = "我放入的";
/// 系统词条搜索的防抖：停手这么久才发请求。
pub(crate) const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);

/// 词库来源（`sysDict.categories` 的 `source`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SysSource {
    Main,
    Bundled,
    Dir,
}

impl SysSource {
    pub fn label(self) -> &'static str {
        match self {
            SysSource::Main => "主词库",
            SysSource::Bundled => "随包",
            SysSource::Dir => DIR_GROUP,
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "main" => SysSource::Main,
            "dir" => SysSource::Dir,
            _ => SysSource::Bundled,
        }
    }

    fn key(self) -> &'static str {
        match self {
            SysSource::Main => "main",
            SysSource::Bundled => "bundled",
            SysSource::Dir => "dir",
        }
    }
}

/// 一本系统词库（`sysDict.categories` 的一项）。
#[derive(Clone, Debug, PartialEq)]
pub struct SysCat {
    pub id: String,
    pub label: String,
    pub description: String,
    /// 展示用的分组名（已按来源归好：主库「主词库」、放入目录的「我放入的」、没写的「其他」）。
    pub group: String,
    pub enabled: bool,
    /// 条目数；core 数不出（文件或子表不在）为 `None`。
    pub count: Option<u64>,
    pub source: SysSource,
    /// 生效先后（1 起）。
    pub order: u64,
    /// 能不能关（主库与 `default` 库不能）。
    pub togglable: bool,
}

/// `sysDict.categories` 的一项 → [`SysCat`]（缺 `id` 时 `id` 为空）。
pub(crate) fn parse_cat(it: &Value) -> SysCat {
    let s = |k: &str| it.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let id = s("id");
    let source = SysSource::parse(&s("source"));
    let group = match source {
        SysSource::Main | SysSource::Dir => source.label().to_string(),
        SysSource::Bundled => Some(s("group"))
            .filter(|g| !g.is_empty())
            .unwrap_or_else(|| OTHER_GROUP.to_string()),
    };
    SysCat {
        label: Some(s("label"))
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| id.clone()),
        description: s("description"),
        group,
        enabled: it.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        count: it.get("count").and_then(Value::as_u64),
        order: it.get("order").and_then(Value::as_u64).unwrap_or(u64::MAX),
        togglable: source != SysSource::Main
            && it.get("togglable").and_then(Value::as_bool).unwrap_or(true),
        source,
        id,
    }
}

/// `sysDict.categories` 的返回 → 清单（缺 `id` 的项丢弃），按 `order` 排。
pub(crate) fn parse_cats(v: &Value) -> Vec<SysCat> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    let mut out: Vec<SysCat> = arr
        .iter()
        .map(parse_cat)
        .filter(|c| !c.id.is_empty())
        .collect();
    out.sort_by_key(|c| c.order); // 稳定排序：旧版 core 没有 order 时保持原顺序
    out
}

/// 一级表的一行：`code` = 库 id、`text` = 名称、`position` = 生效先后、`kind` = 来源键、
/// `count` 数不出为 -1（显示「—」）。
pub(crate) fn cat_row(c: &SysCat) -> WordRow {
    WordRow {
        code: c.id.clone(),
        text: c.label.clone(),
        group: c.group.clone(),
        enabled: c.enabled,
        count: c.count.map_or(-1, |n| n as i64),
        position: c.order.min(i64::MAX as u64) as i64,
        kind: c.source.key().to_string(),
        togglable: c.togglable,
        ..Default::default()
    }
}

/// 启用格禁用时的说明。
pub(crate) fn locked_tip(row: &WordRow) -> &'static str {
    if row.kind == SysSource::Main.key() {
        "主词库总是启用"
    } else {
        "方案把它设为必选，不能关"
    }
}

fn count_text(n: Option<u64>) -> String {
    n.map_or_else(|| "—".to_string(), |n| n.to_string())
}

/// 一本库的头部信息（`sysDict.header`），「信息」对话框与词条表的子表列用。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SysHeader {
    pub label: String,
    /// (字段名, 值)：已按对话框顺序排好、空值不出。
    pub fields: Vec<(String, String)>,
    pub notes: String,
    pub path: String,
    /// 子表（名, 条数）。
    pub tables: Vec<(String, Option<u64>)>,
    /// 子表一行（「名（条数）、…」，缺失的标「缺失」）；没有子表为空。
    pub tables_text: String,
    /// 取不到头部时的说明。
    pub error: String,
}

fn fmt_mtime(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

pub(crate) fn parse_header(v: &Value) -> SysHeader {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let strs = |k: &str| -> Vec<String> {
        v.get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    };
    let count = v.get("count").and_then(Value::as_u64);
    let source = SysSource::parse(&s("source"));
    let mtime = v.get("mtimeMs").and_then(Value::as_u64).map(fmt_mtime);
    let tables: Vec<(String, Option<u64>)> = v
        .get("tables")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|t| {
                    (
                        t.get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        t.get("count").and_then(Value::as_u64),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut push = |k: &str, val: String| {
        if !val.is_empty() {
            fields.push((k.to_string(), val));
        }
    };
    push("名称", s("name"));
    push("版本", s("version"));
    push("排序", s("sort"));
    push("列", strs("columns").join(", "));
    push("引入表", strs("importTables").join(", "));
    push("条数", count_text(count));
    push("来源", source.label().to_string());
    push("修改时间", mtime.unwrap_or_default());
    let tables_text = tables
        .iter()
        .map(|(n, c)| match c {
            Some(c) => format!("{n}（{c}）"),
            None => format!("{n}（缺失）"),
        })
        .collect::<Vec<_>>()
        .join("、");
    SysHeader {
        label: s("label"),
        fields,
        tables_text,
        notes: s("notes"),
        path: s("path"),
        tables,
        error: String::new(),
    }
}

/// 系统词库的状态（Signal 为 Copy，整体可 Clone 进闭包）。表格数据本身在
/// `DictManagerState` 的通用字段里（`rows` / `table_rows` / `total` / `offset` / `sort`）。
#[derive(Clone)]
pub struct SysCatsState {
    /// 当前方案的清单（按生效先后）。中栏据此决定出不出「系统词库」。
    pub list: Signal<Vec<SysCat>>,
    /// 右栏是否换成了系统词库（点中栏「系统词库」打开，点别的节点或换数据域关上）。
    pub open: Signal<bool>,
    /// 二级：正在看的词库 id；`None` = 一级。
    pub viewing: Signal<Option<String>>,
    /// 正在看的库的头部（面包屑名称、有没有子表列）。
    pub header: Signal<SysHeader>,
    /// 「信息」对话框。
    pub info_visible: Signal<bool>,
    pub info: Signal<SysHeader>,
    pub info_title: Signal<String>,
    /// 对话框正文重建（每次打开递增）。
    info_epoch: Signal<Vec<u64>>,
    /// 词条表最近一页 core 给的说明（整库因 `columns:` 声明残缺不加载）。
    note: Rc<RefCell<Option<String>>>,
    /// 词条表最近一次按什么查询取的（`None` = 浏览）；搜索框停手后与它不同才重取。
    searched: Rc<RefCell<Option<String>>>,
    /// 清单是按哪个方案的开关取的（拼音族是活动成员，见 `lib_switch_schema`）；与当前不同时
    /// [`DictManagerState::reload`] 重取一次。
    loaded_for: Rc<RefCell<Option<String>>>,
    /// 本端最近一次写开关的时刻：之后不久收到的 `schema.setDictEnabled` 广播是自己的回声，
    /// 写完已经重取过，不再重取。
    last_write: Rc<Cell<Option<Instant>>>,
    /// 右栏窄（见 `spec::SYS_NARROW_BELOW`）：两张表换少几列的版本。由 [`build_width_probe`]
    /// 量出来写入。
    pub narrow: Rc<Cell<bool>>,
}

/// 回声窗口：本端写开关后这么久之内的同类广播当回声。
const ECHO_WINDOW: Duration = Duration::from_secs(2);

impl SysCatsState {
    pub fn new() -> Self {
        Self {
            list: signal(Vec::new()),
            open: signal(false),
            viewing: signal(None),
            header: signal(SysHeader::default()),
            info_visible: signal(false),
            info: signal(SysHeader::default()),
            info_title: signal(String::new()),
            info_epoch: signal(vec![0]),
            note: Rc::new(RefCell::new(None)),
            searched: Rc::new(RefCell::new(None)),
            loaded_for: Rc::new(RefCell::new(None)),
            last_write: Rc::new(Cell::new(None)),
            narrow: Rc::new(Cell::new(false)),
        }
    }

    /// 正在看的库有没有子表（词条表据此多一列「子表」）。
    pub(crate) fn has_tables(&self) -> bool {
        self.viewing.get().is_some() && !self.header.get().tables.is_empty()
    }

    /// 收起（点别的节点、换数据域）：下次打开回一级。
    pub(crate) fn close(&self) {
        if self.open.get() {
            self.open.set(false);
        }
        if self.viewing.get().is_some() {
            self.viewing.set(None);
        }
    }
}

impl Default for SysCatsState {
    fn default() -> Self {
        Self::new()
    }
}

impl DictManagerState {
    /// 清单还不是为当前方案取的就取一次（[`Self::reload`] 开头调用）。一级表本身就取清单，
    /// 不重复取。
    pub(crate) fn ensure_sys_cats(&self) {
        if *self.cats.loaded_for.borrow() != self.sys_cats_scope()
            && self.current_category() != WordCategory::SysDicts
        {
            self.fetch_sys_list();
        }
    }

    /// 清单按哪个方案取：方案域里读开关的那个成员（拼音族取活动成员）；全局域为 `None`。
    fn sys_cats_scope(&self) -> Option<String> {
        self.current_schema_id()
            .and_then(|_| self.lib_switch_schema())
    }

    /// 只取清单（右栏不是一级表时用）。取不到（core 忙、旧版不认）留着旧清单，`loaded_for`
    /// 不动，下次 reload 见它与当前不同会再取。正在看的库不在新清单里就回一级，还在就重取
    /// 头部（版本、子表可能变了）。返回是否收起了右栏。
    fn fetch_sys_list(&self) -> bool {
        let scope = self.sys_cats_scope();
        let list = match &scope {
            Some(id) => match fetch_cats(id) {
                Ok(l) => l,
                Err(e) => {
                    tracing::debug!("取系统词库清单失败：{e}");
                    return false;
                }
            },
            None => Vec::new(),
        };
        if self.apply_sys_list(list, scope) {
            return true;
        }
        if let Some(id) = self.cats.viewing.get() {
            if self.cats.list.get().iter().any(|c| c.id == id) {
                self.cats.header.set(self.load_sys_header(&id));
            } else {
                self.cats.viewing.set(None);
                self.reset_table_view();
            }
        }
        false
    }

    /// 记下新清单；中栏只认「有没有」，有无变化时重建。方案没有可列的系统词库时收起右栏，
    /// 返回 true——右栏该换成用户词库的表，由调用方补一次 reload（`reload` 开头经
    /// `ensure_sys_cats` 进来时它自己往下就取对了，不必再取）。
    fn apply_sys_list(&self, list: Vec<SysCat>, scope: Option<String>) -> bool {
        *self.cats.loaded_for.borrow_mut() = scope;
        let had_nav = !self.cats.list.get().is_empty();
        if list != self.cats.list.get() {
            self.cats.list.set(list);
        }
        let has_nav = !self.cats.list.get().is_empty();
        if had_nav != has_nav {
            self.bump_nav_epoch();
        }
        if !has_nav && self.cats.open.get() {
            self.cats.close();
            self.reset_table_view();
            return true;
        }
        false
    }

    /// 重取清单（开关写完、别处改了开关）；右栏开着就连表一起重取。
    pub fn refresh_sys_cats(&self) {
        match self.current_category() {
            // 一级表的 list RPC 就是清单
            WordCategory::SysDicts => self.reload(),
            _ => {
                if self.fetch_sys_list() || self.cats.open.get() {
                    self.reload();
                }
            }
        }
    }

    /// core 广播的方案配置变化：是当前域的成员（或不带 id 的整批变化）就重取清单。分库开关
    /// （`userDicts.setEnabled`）动不到它，本端刚写的开关回声也不重取。返回是否重取了。
    pub fn on_schema_config_changed(&self, id: Option<&str>, method: Option<&str>) -> bool {
        if method == Some("userDicts.setEnabled") {
            return false;
        }
        let echo = method == Some("schema.setDictEnabled")
            && self
                .cats
                .last_write
                .get()
                .is_some_and(|t| t.elapsed() < ECHO_WINDOW);
        let hit = !echo
            && match id {
                Some(id) => self.current_domain_members().iter().any(|m| m == id),
                None => self.current_schema_id().is_some(),
            };
        if hit {
            self.refresh_sys_cats();
        }
        hit
    }

    /// 打开右栏的系统词库，总是落在一级表。
    pub fn open_sys_cats(&self) {
        if self.cats.viewing.get().is_some() {
            self.cats.viewing.set(None);
        }
        if !self.cats.open.get() {
            self.cats.open.set(true);
        }
        self.reset_table_view();
        self.reload();
    }

    /// 进入一本库（二级）：取头部（面包屑与子表列），从首页、文件顺序看起。搜索词跨导航保留，
    /// 同用户词库。
    pub fn enter_sys_dict(&self, id: &str) {
        if !self.cats.list.get().iter().any(|c| c.id == id) {
            return;
        }
        self.cats.header.set(self.load_sys_header(id));
        self.cats.viewing.set(Some(id.to_string()));
        self.reset_table_view();
        self.reload();
    }

    /// 二级回一级。
    pub fn leave_sys_dict(&self) {
        if self.cats.viewing.get().is_none() {
            return;
        }
        self.cats.viewing.set(None);
        self.reset_table_view();
        self.reload();
    }

    /// 取一本库的头部；取不到时只有名字与原因。
    fn load_sys_header(&self, id: &str) -> SysHeader {
        let schema = self.sys_cats_scope().unwrap_or_default();
        match rpc::call_core("sysDict.header", &json!({ "schema": schema, "dictId": id })) {
            Ok(v) => parse_header(&v),
            Err(e) => SysHeader {
                label: self.sys_cat_label(id),
                error: format!("读不到词库头部：{e}"),
                ..Default::default()
            },
        }
    }

    fn sys_cat_label(&self, id: &str) -> String {
        self.cats
            .list
            .get()
            .into_iter()
            .find(|c| c.id == id)
            .map_or_else(|| id.to_string(), |c| c.label)
    }

    /// 面包屑第二段：头部的 label，取不到用清单里的名字。
    pub(crate) fn sys_view_title(&self) -> String {
        let h = self.cats.header.get();
        if !h.label.is_empty() {
            return h.label;
        }
        self.cats
            .viewing
            .get()
            .map(|id| self.sys_cat_label(&id))
            .unwrap_or_default()
    }

    /// 一级表 `sysDict.categories` 的参数。
    pub(crate) fn sys_list_params(&self) -> Value {
        json!({ "schema": self.sys_cats_scope() })
    }

    /// 词条表 `sysDict.entries` 的参数：当前库、页、搜索框（不分编码与词）、表头排序。
    pub(crate) fn sys_entries_params(&self) -> Value {
        let mut p = json!({
            "schema": self.sys_cats_scope(),
            "dictId": self.cats.viewing.get(),
            "offset": *self.offset.borrow(),
            "limit": PAGE_SIZE,
        });
        let q = self.search.get().trim().to_string();
        if !q.is_empty() {
            p["query"] = json!(q);
        }
        if let Some(SortKey { column, order }) = self.sort.get()
            && let Some(c) = self.current_spec().columns.get(column)
            && sys_sortable(c.field)
        {
            p["sort"] = json!(c.field.rpc_key());
            p["order"] = json!(if order == SortOrder::Asc {
                "asc"
            } else {
                "desc"
            });
        }
        p
    }

    /// 当前的表头排序 core 排不排（系统词条只排编码 / 词 / 权重）。
    pub(crate) fn sys_sort_supported(&self) -> bool {
        if self.current_category() != WordCategory::SysEntries {
            return true;
        }
        self.sort.get().is_none_or(|SortKey { column, .. }| {
            self.current_spec()
                .columns
                .get(column)
                .is_some_and(|c| sys_sortable(c.field))
        })
    }

    /// list RPC 成功、建表之前：记下词条表的说明与这次的查询（空态文字要用）。
    pub(crate) fn before_sys_rows(&self, params: &Value, v: &Value) {
        if self.current_category() != WordCategory::SysEntries {
            return;
        }
        *self.cats.note.borrow_mut() = v["note"].as_str().map(str::to_string);
        *self.cats.searched.borrow_mut() = params["query"].as_str().map(str::to_string);
    }

    /// list RPC 成功、建表之后：一级表的返回同时是清单。
    pub(crate) fn after_sys_rows(&self, v: &Value) {
        if self.current_category() == WordCategory::SysDicts
            && self.apply_sys_list(parse_cats(v), self.sys_cats_scope())
        {
            self.reload();
        }
    }

    /// 词条表空着且 core 说明了原因时的那句说明。
    pub(crate) fn sys_entries_note(&self) -> Option<String> {
        if self.current_category() != WordCategory::SysEntries || self.total.get() > 0 {
            return None;
        }
        self.cats.note.borrow().clone()
    }

    /// 搜索框停手（或回车）：框里的查询与词条表上次取的不同才重取。别的类别不管
    /// （它们输入即搜，见 `on_search_changed`）。
    pub fn flush_sys_search(&self) {
        if self.current_category() != WordCategory::SysEntries {
            return;
        }
        let q = self.search.get().trim().to_string();
        let now = (!q.is_empty()).then_some(q);
        if *self.cats.searched.borrow() != now {
            self.apply_search();
        }
    }

    /// 「信息」：弹出一本库的头部信息。
    pub fn open_sys_info(&self, id: &str) {
        let mut h = self.load_sys_header(id);
        // 方案里写的简介（表格里不占列）放在最前
        if let Some(c) = self.cats.list.get().into_iter().find(|c| c.id == id)
            && !c.description.is_empty()
        {
            h.fields.insert(0, ("简介".to_string(), c.description));
        }
        let name = if h.label.is_empty() {
            self.sys_cat_label(id)
        } else {
            h.label.clone()
        };
        self.cats.info_title.set(format!("词库信息 · {name}"));
        self.cats.info.set(h);
        let e = self.cats.info_epoch.get().first().copied().unwrap_or(0);
        self.cats.info_epoch.set(vec![e + 1]);
        self.cats.info_visible.set(true);
    }

    /// 「加进用户词库」：落到本域用户词库的主词库，打开加词对话框并预填编码与词。系统词库
    /// 的右栏保持原样（加完回来还在这一页）。
    pub fn add_sys_entry_to_user(&self, disp: usize) {
        let Some(e) = self.display_row(disp) else {
            return;
        };
        if self.current_category() != WordCategory::SysEntries || self.current_schema_id().is_none()
        {
            return;
        }
        self.prefill_add_word(&e.text, &e.code);
    }

    /// 「打开所在目录」：在文件管理器里选中「信息」里这本库的主文件。
    pub fn reveal_sys_dict(&self) {
        let path = self.cats.info.get().path;
        if path.is_empty() || rpc::is_mock() {
            return;
        }
        if let Err(e) = crate::log_tools::reveal(std::path::Path::new(&path)) {
            self.notify_err(&format!("没能打开文件夹（{e}），文件在 {path}"));
        }
    }

    /// 开关一本系统词库，对域内**有这张库**的每个成员方案都改（拼音族折叠时全拼、双拼各存一份
    /// 开关；第三方拼音方案可能没有随包细胞库，跳过不算失败）。任一成员写失败就把已改的成员
    /// 改回各自原值并提示；返回是否成功（勾选框据此回滚）。
    pub fn set_sys_cat_enabled(&self, id: &str, enabled: bool) -> bool {
        let ok = self.write_sys_cat(id, enabled);
        // 只认写成功的回声；失败（含已回滚）时别处的广播照常重取
        if ok {
            self.cats.last_write.set(Some(Instant::now()));
        }
        self.refresh_sys_cats();
        ok
    }

    /// 右栏宽窄变了：记下，正开着系统词库就换列重建。列下标随之变，排序按下标记的，
    /// 一并取消（词条表回第一页重取）。
    pub fn set_sys_narrow(&self, narrow: bool) {
        if self.cats.narrow.replace(narrow) == narrow {
            return;
        }
        let cat = self.current_category();
        if !matches!(cat, WordCategory::SysDicts | WordCategory::SysEntries) {
            return;
        }
        let sorted = self.sort.get().is_some();
        if sorted {
            self.sort.set(None);
        }
        self.bump_table_epoch();
        if sorted && cat == WordCategory::SysEntries {
            *self.offset.borrow_mut() = 0;
            self.reload();
        } else {
            self.rebuild_list();
        }
    }

    fn write_sys_cat(&self, id: &str, enabled: bool) -> bool {
        // 先按成员取清单，记下哪些成员有这张库、各自原值。
        let mut targets: Vec<(String, bool)> = Vec::new();
        for schema in self.current_domain_members() {
            match fetch_cats(&schema) {
                Ok(list) => {
                    if let Some(c) = list.iter().find(|c| c.id == id && c.togglable) {
                        targets.push((schema, c.enabled));
                    }
                }
                Err(e) => {
                    self.notify_err(&format!("切换失败：{e}"));
                    return false;
                }
            }
        }
        if targets.is_empty() {
            self.notify_err("切换失败：方案里没有可开关的这个词库");
            return false;
        }
        let call = |schema: &str, on: bool| {
            rpc::call_core(
                "schema.setDictEnabled",
                &json!({ "id": schema, "dictId": id, "enabled": on }),
            )
        };
        let mut done: Vec<&(String, bool)> = Vec::new();
        for t in &targets {
            if let Err(e) = call(&t.0, enabled) {
                for (schema, orig) in done.iter().rev() {
                    if let Err(e2) = call(schema, *orig) {
                        tracing::warn!("系统词库开关回滚失败（{schema}）：{e2}");
                    }
                }
                self.notify_err(&format!("切换失败：{e}"));
                return false;
            }
            done.push(t);
        }
        let verb = if enabled { "已启用" } else { "已停用" };
        self.notify_ok(&format!("{verb}「{}」", self.sys_cat_label(id)));
        true
    }
}

/// core 能按哪些列排系统词条。
fn sys_sortable(f: WordField) -> bool {
    matches!(f, WordField::Code | WordField::Text | WordField::Weight)
}

/// 取一个方案的系统词库清单。
fn fetch_cats(schema: &str) -> Result<Vec<SysCat>, String> {
    rpc::call_core("sysDict.categories", &json!({ "schema": schema })).map(|v| parse_cats(&v))
}

// ── 搜索防抖 ─────────────────────────────────────────────────

/// 防抖的判定：版本一变就（重新）计时，到点触发一次。
struct Debounce {
    delay: Duration,
    last: u64,
    due: Option<Instant>,
}

impl Debounce {
    /// 每帧调用；返回这一帧要不要触发。
    fn poll(&mut self, version: u64, now: Instant) -> bool {
        if version != self.last {
            self.last = version;
            self.due = Some(now + self.delay);
            return false;
        }
        if self.due.is_some_and(|d| now >= d) {
            self.due = None;
            return true;
        }
        false
    }
}

/// 防抖叶子：铺在搜索框底下（打字时的局部重绘一定画到它）。计时中在绘制时约一次到点的
/// 重绘、到点那次再要一次重排，重排前的 `on_update` 里触发——空闲时不留常驻定时器。
struct SearchDebounce {
    search: Signal<String>,
    state: RefCell<Debounce>,
    fire: Box<dyn Fn()>,
    /// 作用域见证，同 `reactive::ReactiveEffect`。
    witness: Signal<()>,
}

impl windui::core::Widget for SearchDebounce {
    fn on_update(&mut self, _ctx: &mut windui::core::EventCtx) {
        if !self.witness.is_alive() {
            return;
        }
        let fire = self
            .state
            .borrow_mut()
            .poll(self.search.version(), Instant::now());
        if fire {
            (self.fire)();
        }
    }

    fn paint(
        &self,
        bounds: Rect,
        _content: Rect,
        _focused: bool,
        _enabled: bool,
        _canvas: &mut dyn windui::render::Canvas,
        _style: &Style,
    ) {
        let Some(due) = self.state.borrow().due else {
            return;
        };
        let left = due.saturating_duration_since(Instant::now());
        if left.is_zero() {
            windui::anim::request_relayout();
        } else {
            windui::anim::request_repaint_in_after(bounds, left.as_millis() as u64 + 1);
        }
    }
}

/// 量右栏宽度的叶子：绘制时记下宽度，跨过 [`SYS_NARROW_BELOW`] 就请求一帧，下一帧的
/// `on_update` 里换表（不在绘制里改状态）。只在宽度变化时多一帧，平时什么也不做。
struct WidthProbe {
    width: Cell<i32>,
    apply: Box<dyn Fn(bool)>,
    narrow: Rc<Cell<bool>>,
    witness: Signal<()>,
}

impl WidthProbe {
    fn wants_narrow(&self) -> Option<bool> {
        let w = self.width.get();
        let narrow = w > 0 && w < SYS_NARROW_BELOW;
        (w > 0 && narrow != self.narrow.get()).then_some(narrow)
    }
}

impl windui::core::Widget for WidthProbe {
    fn on_update(&mut self, _ctx: &mut windui::core::EventCtx) {
        if !self.witness.is_alive() {
            return;
        }
        if let Some(narrow) = self.wants_narrow() {
            (self.apply)(narrow);
        }
    }

    fn paint(
        &self,
        bounds: Rect,
        _content: Rect,
        _focused: bool,
        _enabled: bool,
        _canvas: &mut dyn windui::render::Canvas,
        _style: &Style,
    ) {
        self.width.set(bounds.w);
        if self.wants_narrow().is_some() {
            windui::anim::request_relayout();
        }
    }
}

/// 右栏表格区的量宽叶子（叠在表格上，1 像素高、不画任何东西）。
pub(crate) fn build_width_probe(mgr: &DictManagerState) -> Element {
    let m = mgr.clone();
    let w = WidthProbe {
        width: Cell::new(0),
        apply: Box::new(move |n| m.set_sys_narrow(n)),
        narrow: mgr.cats.narrow.clone(),
        witness: signal(()),
    };
    Element::leaf().widget(w).reactive().width_match().height(1)
}

/// 搜索框的防抖叶子：搜索框内容停手 `delay` 后调 [`DictManagerState::flush_sys_search`]。
pub(crate) fn build_search_debounce(mgr: &DictManagerState, delay: Duration) -> Element {
    let m = mgr.clone();
    let w = SearchDebounce {
        search: mgr.search,
        state: RefCell::new(Debounce {
            delay,
            last: mgr.search.version(),
            due: None,
        }),
        fire: Box::new(move || m.flush_sys_search()),
        witness: signal(()),
    };
    // 高度取工具栏控件高：铺满父容器的话会把工具栏整行撑高
    Element::leaf()
        .widget(w)
        .reactive()
        .width_match()
        .height(windui::theme::current().metrics.control_h_sm)
}

// ── 「信息」对话框 ─────────────────────────────────────────────

/// 「信息」对话框：头部字段两列 + 子表 + 说明 + 路径与「打开所在目录」。
pub(crate) fn build_sys_info_dialog(mgr: &DictManagerState) -> Element {
    let m = mgr.clone();
    let body = Element::host_signal(mgr.cats.info_epoch, move |_| info_body(&m));
    let close = mgr.cats.info_visible;
    let footer = dialog_footer().child(Element::leaf().weight(1.0)).child(
        Element::button("关闭")
            .small()
            .outline_soft()
            .on_click(move |_| close.set(false)),
    );
    let frame = dialog_frame(460, dialog_header(mgr.cats.info_title), body, Some(footer));
    Element::dialog(mgr.cats.info_visible, frame)
}

fn info_body(mgr: &DictManagerState) -> Element {
    let h = mgr.cats.info.get();
    let small = |s: String, role: Role, lines: usize| {
        Element::label(s)
            .font_size(12.0)
            .fg_role(role)
            .max_lines(lines)
            .truncate(Truncate::End)
    };
    let line = |k: &str, v: Element| {
        Element::row()
            .width_match()
            .spacing(8)
            .child(small(k.to_string(), Role::TextMuted, 1).width(64))
            .child(v.weight(1.0))
    };
    let mut col = dialog_body().spacing(6);
    if !h.error.is_empty() {
        col = col.child(small(h.error.clone(), Role::Warning, 3).width_match());
    }
    for (k, v) in &h.fields {
        col = col.child(line(k, small(v.clone(), Role::Text, 2)));
    }
    if !h.tables_text.is_empty() {
        col = col.child(line("子表", small(h.tables_text.clone(), Role::Text, 3)));
    }
    if !h.notes.is_empty() {
        col = col.child(line("说明", small(h.notes.clone(), Role::TextMuted, 6)));
    }
    let m_dir = mgr.clone();
    col.child(line(
        "路径",
        Element::label(h.path.clone())
            .font_size(11.5)
            .fg_role(Role::TextMuted)
            .max_lines(1)
            .truncate(Truncate::Middle)
            .tooltip(h.path.clone()),
    ))
    .child(
        Element::row()
            .width_match()
            .child(Element::leaf().weight(1.0))
            .child(
                Element::button("打开所在目录")
                    .small()
                    .outline_soft()
                    .enabled(!h.path.is_empty())
                    .on_click(move |_| m_dir.reveal_sys_dict()),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SchemaInfo;

    #[test]
    fn parse_orders_rows_and_resolves_groups_by_source() {
        let v = json!([
            { "id": "cell_idiom", "label": "成语", "description": "成语俗语", "group": "文史",
              "enabled": true, "source": "bundled", "count": 48721, "order": 3, "togglable": true },
            { "id": "main", "label": "rime_frost", "group": null, "enabled": true,
              "source": "main", "count": 654116, "order": 1, "togglable": false },
            { "id": "wb_extra", "label": "", "group": null, "enabled": false,
              "source": "bundled", "count": null, "order": 2, "togglable": true },
            { "id": "dir:pinyin/ext/mine", "label": "我的", "group": "文史", "enabled": false,
              "source": "dir", "count": 3, "order": 4, "togglable": true },
            { "label": "缺 id" }
        ]);
        let l = parse_cats(&v);
        let ids: Vec<&str> = l.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            ["main", "wb_extra", "cell_idiom", "dir:pinyin/ext/mine"],
            "按 order 排，缺 id 的丢弃"
        );
        assert_eq!((l[0].source, l[0].togglable), (SysSource::Main, false));
        assert_eq!(
            (l[1].label.as_str(), l[1].group.as_str(), l[1].count),
            ("wb_extra", OTHER_GROUP, None),
            "没名字用 id，没分组归「其他」，数不出为 None"
        );
        assert_eq!(l[3].group, DIR_GROUP, "发现目录的不看 group");
        let spec = super::super::spec::spec_for(WordCategory::SysDicts);
        let cells: Vec<Vec<String>> = l
            .iter()
            .map(cat_row)
            .map(|r| spec.columns.iter().map(|c| r.cell_text(c.field)).collect())
            .collect();
        assert_eq!(
            cells,
            vec![
                vec!["是", "rime_frost", "主词库", "654116", "1"],
                vec!["否", "wb_extra", "其他", "—", "2"],
                vec!["是", "成语", "文史", "48721", "3"],
                vec!["否", "我的", "我放入的", "3", "4"],
            ]
        );
        assert!(parse_cats(&json!({"error": 1})).is_empty());
    }

    #[test]
    fn header_fields_for_the_info_dialog() {
        let h = parse_header(&json!({
            "id": "pinyin_main", "label": "rime_frost", "source": "main", "name": "rime_frost",
            "version": "2023-11-13", "sort": null, "columns": [], "importTables": ["a", "b"],
            "notes": "说明", "path": "/x/rime_frost.dict.yaml", "mtimeMs": null, "count": null,
            "tables": [ {"name": "a", "count": 3}, {"name": "b", "count": null} ]
        }));
        let keys: Vec<&str> = h.fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["名称", "版本", "引入表", "条数", "来源"], "空值不出");
        assert_eq!(h.fields[2].1, "a, b");
        assert_eq!(h.fields[3].1, "—");
        assert_eq!(h.tables_text, "a（3）、b（缺失）");
        assert_eq!((h.notes.as_str(), h.tables.len()), ("说明", 2));
    }

    // ── 状态（mock RPC）────────────────────────────────────

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

    fn calls_of(method: &str) -> Vec<Value> {
        crate::rpc::take_mock_calls()
            .into_iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p)
            .collect()
    }

    /// 拼音域（全拼 py + 双拼 `sp`，代表 py）。`bad` 打头的方案 mock 开关会失败。
    fn pinyin_state(sp: &str) -> DictManagerState {
        let st = DictManagerState::new();
        *st.schemas.borrow_mut() = vec![
            SchemaInfo {
                id: "py".into(),
                name: "全拼".into(),
                kind: "pinyin".into(),
                scheme: "full".into(),
                ..Default::default()
            },
            SchemaInfo {
                id: sp.into(),
                name: "双拼".into(),
                kind: "pinyin".into(),
                scheme: "shuangpin".into(),
                ..Default::default()
            },
        ];
        st.domain.set(super::super::state::GLOBAL_DOMAINS);
        st.sub_tab.set(0);
        st.on_nav_changed();
        st
    }

    /// 拼音域、右栏打开系统词库并进了 `id` 这本库。
    fn viewing(id: &str) -> DictManagerState {
        let st = pinyin_state("sp");
        st.open_sys_cats();
        st.enter_sys_dict(id);
        st
    }

    fn texts(st: &DictManagerState) -> Vec<String> {
        st.display_rows
            .borrow()
            .iter()
            .map(|r| r.text.clone())
            .collect()
    }

    #[test]
    fn reload_fetches_the_list_once_per_schema() {
        with_mock(|| {
            let st = pinyin_state("sp");
            let l = st.cats.list.get();
            assert_eq!(l[0].id, "pinyin_main", "主库在首行");
            crate::rpc::take_mock_calls();
            st.reload();
            assert!(calls_of("sysDict.categories").is_empty(), "同一方案不重取");
            st.domain.set(super::super::state::DOMAIN_PHRASE);
            st.on_nav_changed();
            assert!(st.cats.list.get().is_empty(), "全局域没有系统词库");
        });
    }

    /// 一级表走表格管线：类别、列、行与计数都出自 `sysDict.categories`，主库开关禁用。
    #[test]
    fn list_level_is_a_regular_table() {
        with_mock(|| {
            let st = pinyin_state("sp");
            crate::rpc::take_mock_calls();
            st.open_sys_cats();
            assert_eq!(
                calls_of("sysDict.categories"),
                vec![json!({"schema": "py"})],
                "一级表的 list RPC 就是清单，只取一次"
            );
            assert_eq!(st.current_category(), WordCategory::SysDicts);
            assert_eq!(
                (st.pane_title.get(), st.pane_subtitle.get()),
                (SYS_TITLE.to_string(), "词库列表".to_string())
            );
            let rows = st.table_rows.get();
            assert_eq!(rows.len(), 6);
            assert_eq!(rows[0], ["是", "rime_frost", "主词库", "250", "1"]);
            assert_eq!(rows[5][2], DIR_GROUP);
            assert_eq!(rows[5][3], "—", "数不出");
            assert_eq!(st.page_status.get(), "共 6 本，已启用 2 本");
            let main = st.display_row(0).unwrap();
            let ops = super::super::spec::row_ops(WordCategory::SysDicts, &main);
            assert!(!ops.set_enabled, "主库不可关");
            assert_eq!(locked_tip(&main), "主词库总是启用");
            let idiom = st.display_row(2).unwrap();
            assert!(super::super::spec::row_ops(WordCategory::SysDicts, &idiom).set_enabled);
            assert!(!st.current_spec().paged && !st.current_spec().searchable);
        });
    }

    /// 一级表点表头：客户端排序，不发请求；导航后复位。
    #[test]
    fn list_level_sorts_on_the_client() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            crate::rpc::take_mock_calls();
            st.sort.set(Some(SortKey {
                column: 3,
                order: SortOrder::Desc,
            }));
            st.on_sort_changed();
            assert!(crate::rpc::take_mock_calls().is_empty(), "客户端排序");
            let names: Vec<String> = st.table_rows.get().iter().map(|r| r[1].clone()).collect();
            assert_eq!(
                names,
                [
                    "成语",
                    "地名",
                    "网络热词",
                    "rime_frost",
                    "计算机",
                    "我的行业词"
                ],
                "条数降序（mock 主库 250 条），数不出的垫底"
            );
            st.enter_sys_dict("cell_idiom");
            assert_eq!(st.sort.get(), None, "进二级复位排序");
        });
    }

    #[test]
    fn main_dict_cannot_be_toggled() {
        with_mock(|| {
            let st = pinyin_state("sp");
            crate::rpc::take_mock_calls();
            assert!(!st.set_sys_cat_enabled("pinyin_main", false));
            assert!(calls_of("schema.setDictEnabled").is_empty());
            let (_, msg) = st.feedback.get().expect("应有提示");
            assert!(msg.starts_with("切换失败："), "{msg}");
        });
    }

    #[test]
    fn folded_domain_toggles_every_member() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            crate::rpc::take_mock_calls();
            // 启用列与右键「启用」都走 do_set_enabled → 多成员开关
            let idiom = st.display_row(2).unwrap();
            assert_eq!(idiom.code, "cell_idiom");
            st.do_set_enabled(&idiom, true);
            assert_eq!(
                calls_of("schema.setDictEnabled"),
                vec![
                    json!({"id": "py", "dictId": "cell_idiom", "enabled": true}),
                    json!({"id": "sp", "dictId": "cell_idiom", "enabled": true}),
                ]
            );
            assert!(
                st.cats
                    .list
                    .get()
                    .iter()
                    .any(|c| c.id == "cell_idiom" && c.enabled),
                "开关后重取，清单跟上"
            );
            assert_eq!(st.table_rows.get()[2][0], "是", "一级表跟上");
        });
    }

    #[test]
    fn folded_domain_rolls_back_on_partial_failure() {
        with_mock(|| {
            let st = pinyin_state("bad_sp");
            crate::rpc::take_mock_calls();
            assert!(!st.set_sys_cat_enabled("cell_idiom", true));
            assert_eq!(
                calls_of("schema.setDictEnabled"),
                vec![
                    json!({"id": "py", "dictId": "cell_idiom", "enabled": true}),
                    json!({"id": "bad_sp", "dictId": "cell_idiom", "enabled": true}),
                    json!({"id": "py", "dictId": "cell_idiom", "enabled": false}),
                ],
                "已改的成员改回去"
            );
            let (_, msg) = st.feedback.get().expect("应有提示");
            assert!(msg.starts_with("切换失败："), "{msg}");
            assert!(
                !st.cats
                    .list
                    .get()
                    .iter()
                    .any(|c| c.id == "cell_idiom" && c.enabled),
                "回滚后清单仍是关"
            );
            assert!(
                st.on_schema_config_changed(Some("py"), Some("schema.setDictEnabled")),
                "写失败不算本端回声：别处的广播照常重取"
            );
        });
    }

    /// 拼音域三个成员：全拼 py、双拼 `sp`、第三个 `third`。
    fn three_member_state(sp: &str, third: &str) -> DictManagerState {
        let st = pinyin_state(sp);
        st.schemas.borrow_mut().push(SchemaInfo {
            id: third.into(),
            name: "第三方拼音".into(),
            kind: "pinyin".into(),
            scheme: "full".into(),
            ..Default::default()
        });
        st
    }

    /// 第三方拼音方案没有这张库（mock 的 `third_py` 清单为空）：跳过它，不算失败。
    #[test]
    fn member_without_the_dict_is_skipped() {
        with_mock(|| {
            let st = three_member_state("sp", "third_py");
            crate::rpc::take_mock_calls();
            assert!(st.set_sys_cat_enabled("cell_idiom", true));
            assert_eq!(
                calls_of("schema.setDictEnabled"),
                vec![
                    json!({"id": "py", "dictId": "cell_idiom", "enabled": true}),
                    json!({"id": "sp", "dictId": "cell_idiom", "enabled": true}),
                ]
            );
        });
    }

    /// 成员原本不一致（双拼已开、全拼没开）时中途失败：各自改回原值，而不是一律写反。
    #[test]
    fn rollback_restores_each_members_original_value() {
        with_mock(|| {
            let st = three_member_state("sp", "bad_sp");
            crate::rpc::call_core(
                "schema.setDictEnabled",
                &json!({"id": "sp", "dictId": "cell_idiom", "enabled": true}),
            )
            .unwrap();
            crate::rpc::take_mock_calls();
            assert!(!st.set_sys_cat_enabled("cell_idiom", true));
            assert_eq!(
                calls_of("schema.setDictEnabled"),
                vec![
                    json!({"id": "py", "dictId": "cell_idiom", "enabled": true}),
                    json!({"id": "sp", "dictId": "cell_idiom", "enabled": true}),
                    json!({"id": "bad_sp", "dictId": "cell_idiom", "enabled": true}),
                    json!({"id": "sp", "dictId": "cell_idiom", "enabled": true}),
                    json!({"id": "py", "dictId": "cell_idiom", "enabled": false}),
                ],
                "双拼原本就开着，改回 true；全拼改回 false"
            );
        });
    }

    /// 读开关以活动方案为准：活动方案在域内换了，清单要重取。
    #[test]
    fn active_member_change_refetches() {
        with_mock(|| {
            let st = pinyin_state("sp");
            crate::rpc::take_mock_calls();
            crate::rpc::force_mock_reply("schema.active", Some(json!({"id": "sp"})));
            st.reload();
            crate::rpc::force_mock_reply("schema.active", None);
            assert_eq!(
                calls_of("sysDict.categories"),
                vec![json!({"schema": "sp"})],
                "活动方案从代表换成双拼"
            );
        });
    }

    /// 分库开关的广播不重取清单；自己刚写的开关回声也不再重取。
    #[test]
    fn lib_toggles_and_own_echoes_do_not_refetch() {
        with_mock(|| {
            let st = pinyin_state("sp");
            crate::rpc::take_mock_calls();
            assert!(!st.on_schema_config_changed(Some("py"), Some("userDicts.setEnabled")));
            assert!(st.set_sys_cat_enabled("cell_idiom", true));
            crate::rpc::take_mock_calls();
            assert!(!st.on_schema_config_changed(Some("py"), Some("schema.setDictEnabled")));
            assert!(!st.on_schema_config_changed(Some("sp"), Some("schema.setDictEnabled")));
            assert!(calls_of("sysDict.categories").is_empty());
            assert!(
                st.on_schema_config_changed(Some("py"), Some("schema.saveConfig")),
                "别的写法照常重取"
            );
        });
    }

    /// 别处改了当前域成员的开关（方案页、命令直通车）：重取清单，开着的一级表跟上；别的方案不理。
    #[test]
    fn config_changed_of_a_member_refreshes() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            crate::rpc::take_mock_calls();
            assert!(
                !st.on_schema_config_changed(Some("wubi86"), Some("schema.setDictEnabled")),
                "别的方案不理"
            );
            assert!(calls_of("sysDict.categories").is_empty());
            crate::rpc::call_core(
                "schema.setDictEnabled",
                &json!({"id": "py", "dictId": "cell_idiom", "enabled": true}),
            )
            .unwrap();
            assert!(
                st.on_schema_config_changed(Some("sp"), Some("schema.setDictEnabled")),
                "成员都算"
            );
            assert_eq!(
                calls_of("sysDict.categories").len(),
                1,
                "一级表开着：只取一次"
            );
            assert!(
                st.cats
                    .list
                    .get()
                    .iter()
                    .any(|c| c.id == "cell_idiom" && c.enabled)
            );
            assert_eq!(st.table_rows.get()[2][0], "是", "表跟上");
            assert!(
                st.on_schema_config_changed(None, Some("backup.restore")),
                "不带 id 按全部处理"
            );
        });
    }

    // ── 二级 ────────────────────────────────────────────────

    /// [查看] 进词条表：面包屑标题、首页参数、子表列；返回回一级表。
    #[test]
    fn view_enters_entries_and_back_returns() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            crate::rpc::take_mock_calls();
            st.enter_sys_dict("pinyin_main");
            let calls = crate::rpc::take_mock_calls();
            let of = |m: &str| -> Vec<Value> {
                calls
                    .iter()
                    .filter(|(x, _)| x == m)
                    .map(|(_, p)| p.clone())
                    .collect()
            };
            assert_eq!(
                of("sysDict.header"),
                vec![json!({"schema": "py", "dictId": "pinyin_main"})]
            );
            assert_eq!(
                of("sysDict.entries"),
                vec![json!({"schema": "py", "dictId": "pinyin_main", "offset": 0, "limit": 100})]
            );
            assert_eq!(st.current_category(), WordCategory::SysEntries);
            assert_eq!(
                (st.pane_title.get(), st.pane_subtitle.get()),
                ("系统词库".to_string(), "rime_frost".to_string())
            );
            let rows = st.table_rows.get();
            assert_eq!(rows.len(), 100);
            assert_eq!(
                rows[0],
                ["ma0", "词0", "1000", "cn_dicts/8105"],
                "有子表多一列"
            );
            assert_eq!(st.page_status.get(), "第 1-100 / 共 250 条");
            assert_eq!(st.page_count_text.get(), "/ 3 页");
            assert!(!st.cats.info_visible.get(), "词条视图不弹头部");

            crate::rpc::take_mock_calls();
            st.leave_sys_dict();
            assert_eq!(st.current_category(), WordCategory::SysDicts);
            assert!(st.cats.open.get(), "返回只回一级，右栏仍是系统词库");
            assert_eq!(calls_of("sysDict.categories").len(), 1, "一级表重取");
            assert_eq!(st.table_rows.get().len(), 6);

            st.enter_sys_dict("cell_idiom");
            assert_eq!(st.table_rows.get()[0].len(), 3, "没有子表不出那一列");
            crate::rpc::take_mock_calls();
            st.enter_sys_dict("nope");
            assert!(calls_of("sysDict.header").is_empty(), "不在清单里的库不进");
        });
    }

    /// [信息]：弹对话框显示头部字段，不离开一级表。
    #[test]
    fn info_opens_the_header_dialog() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            st.open_sys_info("pinyin_main");
            assert!(st.cats.info_visible.get());
            assert_eq!(st.current_category(), WordCategory::SysDicts, "不进二级");
            assert_eq!(st.cats.info_title.get(), "词库信息 · rime_frost");
            let h = st.cats.info.get();
            assert!(
                h.fields
                    .iter()
                    .any(|(k, v)| k == "版本" && v == "2026-11-11")
            );
            assert_eq!(h.tables_text, "cn_dicts/8105（100）、cn_dicts/base（150）");
            assert_eq!(h.path, "/mock/schemas/pinyin_main.dict.yaml");
            assert!(h.notes.contains("示例词库"));
            st.open_sys_info("cell_idiom");
            assert_eq!(
                st.cats.info.get().fields[0],
                ("简介".to_string(), "成语、俗语与古诗文名句".to_string()),
                "方案里的简介放在最前"
            );
        });
    }

    /// 跳页、表头排序（服务端 sort / order）、排序与分页组合；core 不排的子表列点了当作取消排序。
    #[test]
    fn page_jump_and_server_sort() {
        with_mock(|| {
            let st = viewing("pinyin_main");
            crate::rpc::take_mock_calls();
            st.page_input.set("3".into());
            st.submit_page_jump();
            let p = calls_of("sysDict.entries");
            assert_eq!(p.last().unwrap()["offset"], 200);
            assert_eq!(st.page_status.get(), "第 201-250 / 共 250 条");

            st.sort.set(Some(SortKey {
                column: 2,
                order: SortOrder::Asc,
            }));
            st.on_sort_changed();
            let p = calls_of("sysDict.entries");
            assert_eq!(
                p,
                vec![
                    json!({"schema": "py", "dictId": "pinyin_main", "offset": 0, "limit": 100,
                            "sort": "weight", "order": "asc"})
                ],
                "排序归零分页"
            );
            assert_eq!(texts(&st)[0], "词249", "权重最小的在首");
            st.next_page();
            let p = calls_of("sysDict.entries");
            assert_eq!(
                (&p[0]["offset"], &p[0]["sort"]),
                (&json!(100), &json!("weight"))
            );
            assert_eq!(texts(&st)[0], "词149");

            st.sort.set(Some(SortKey {
                column: 3,
                order: SortOrder::Asc,
            }));
            st.on_sort_changed();
            // 子表列 core 不排：当作取消排序，回文件顺序第一页（表头、本页、翻页三者一致）
            assert_eq!(st.sort.get(), None);
            assert_eq!(
                calls_of("sysDict.entries"),
                vec![json!({"schema": "py", "dictId": "pinyin_main", "offset": 0, "limit": 100})],
                "子表列：取消排序并回第一页"
            );
            assert_eq!(texts(&st)[0], "词0");
            assert_eq!(st.page_status.get(), "第 1-100 / 共 250 条");
        });
    }

    /// 单框搜索：不分编码与词（不带 mode）；打字不立即请求，停手 / 回车才搜，同一查询不重复取。
    #[test]
    fn single_box_search_is_debounced() {
        with_mock(|| {
            let st = viewing("cell_idiom");
            crate::rpc::take_mock_calls();
            st.search.set(" ma1 ".into());
            st.on_search_changed();
            assert!(calls_of("sysDict.entries").is_empty(), "每键不请求");
            st.flush_sys_search();
            assert_eq!(
                calls_of("sysDict.entries"),
                vec![
                    json!({"schema": "py", "dictId": "cell_idiom", "offset": 0, "limit": 100,
                            "query": "ma1"})
                ]
            );
            assert_eq!(st.total.get(), 111, "ma1 / ma10-19 / ma100-199");
            st.flush_sys_search();
            assert!(calls_of("sysDict.entries").is_empty(), "同一查询不重取");
            st.search.set("词24".into());
            st.flush_sys_search();
            assert_eq!(st.total.get(), 11, "词文本也搜得到");
            st.search.set("无此".into());
            st.flush_sys_search();
            assert_eq!(
                st.empty_msg.get(),
                "没有匹配「无此」的内容（清除搜索可看全部）"
            );
            st.search.set(String::new());
            st.flush_sys_search();
            let p = calls_of("sysDict.entries");
            assert!(p.last().unwrap().get("query").is_none(), "清空回到浏览");
            assert_eq!(st.total.get(), 250);
        });
    }

    fn frame(tree: &mut windui::core::Tree) {
        tree.layout_root(
            windui::geometry::Size::new(800, 600),
            &mut windui::text::NullTextEngine,
        );
    }

    /// 防抖叶子：改搜索框后当帧不搜，停手到点的那一帧搜一次。
    #[test]
    fn debounce_leaf_fires_once_after_the_delay() {
        with_mock(|| {
            let st = viewing("cell_idiom");
            let mut tree = windui::core::Tree::new();
            let root = build_search_debounce(&st, Duration::from_millis(30)).build(&mut tree);
            tree.root = Some(root);
            frame(&mut tree);
            crate::rpc::take_mock_calls();
            st.search.set("m".into());
            frame(&mut tree);
            st.search.set("ma".into());
            frame(&mut tree);
            assert!(calls_of("sysDict.entries").is_empty(), "打字中不搜");
            std::thread::sleep(Duration::from_millis(40));
            frame(&mut tree);
            let p = calls_of("sysDict.entries");
            assert_eq!(p.len(), 1);
            assert_eq!(p[0]["query"], "ma");
            frame(&mut tree);
            assert!(calls_of("sysDict.entries").is_empty(), "只触发一次");
        });
    }

    #[test]
    fn add_to_user_prefills_the_add_dialog_and_keeps_the_view() {
        with_mock(|| {
            let st = viewing("cell_idiom");
            st.next_page();
            crate::rpc::take_mock_calls();
            st.add_sys_entry_to_user(3);
            assert!(st.edit_visible.get(), "打开加词对话框");
            assert_eq!(
                (st.edit_code.get().as_str(), st.edit_text.get().as_str()),
                ("ma103", "词103")
            );
            assert!(crate::rpc::take_mock_calls().is_empty(), "不重取、不换页");
            assert_eq!(st.current_category(), WordCategory::SysEntries, "右栏不动");
            assert!(st.selected_lib().is_none(), "主词库，不是分库");
            let (save, _) = st.save_calls().unwrap();
            assert_eq!(save[0].0, "dict.add");
            assert_eq!(
                (
                    &save[0].1["schemaId"],
                    &save[0].1["code"],
                    &save[0].1["text"]
                ),
                (&json!("py"), &json!("ma103"), &json!("词103"))
            );
            assert!(save[0].1.get("name").is_none());
            st.submit_word();
            let p = calls_of("sysDict.entries");
            assert_eq!(p.last().unwrap()["offset"], 100, "加完回来还在这一页");
        });
    }

    /// 行操作与右键：一级「查看」「信息」、二级「加进用户词库」；双击同主操作。
    #[test]
    fn row_ops_and_menus() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            let ops = st.current_spec().ops;
            assert!(ops.view && ops.info && ops.set_enabled && !ops.add_to_user);
            st.enter_sys_dict("cell_idiom");
            let ops = st.current_spec().ops;
            assert!(ops.add_to_user && !ops.view && !ops.edit && !ops.remove);
        });
    }

    /// core 说明整库不加载时，空态显示这条说明而不是「暂无数据」。
    #[test]
    fn unloadable_dict_shows_the_core_note() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            crate::rpc::force_mock_reply(
                "sysDict.entries",
                Some(
                    json!({"total": 0, "rows": [], "note": "bad 的 columns: 声明缺 text 或 code 列"}),
                ),
            );
            st.enter_sys_dict("cell_idiom");
            crate::rpc::force_mock_reply("sysDict.entries", None);
            assert!(
                st.empty_msg.get().contains("columns"),
                "{}",
                st.empty_msg.get()
            );
        });
    }

    /// 停在末页之后（库变短 / 结果变少）：跳到最后一页。
    #[test]
    fn offset_past_the_end_jumps_to_the_last_page() {
        with_mock(|| {
            let st = viewing("cell_idiom");
            st.goto_page(3);
            st.search.set("ma1".into());
            crate::rpc::take_mock_calls();
            st.reload();
            let offs: Vec<Value> = calls_of("sysDict.entries")
                .into_iter()
                .map(|p| p["offset"].clone())
                .collect();
            assert_eq!(offs, [json!(200), json!(100)]);
            assert_eq!(st.page_status.get(), "第 101-111 / 共 111 条");
        });
    }

    /// 刷新清单失败：留着旧清单，不把右栏清空。
    #[test]
    fn failed_refresh_keeps_the_old_list() {
        with_mock(|| {
            let st = viewing("cell_idiom");
            let before = st.cats.list.get();
            assert!(!before.is_empty());
            crate::rpc::force_mock_error("sysDict.categories", Some("core 忙"));
            st.refresh_sys_cats();
            crate::rpc::force_mock_error("sysDict.categories", None);
            assert_eq!(st.cats.list.get(), before);
            assert_eq!(
                st.current_category(),
                WordCategory::SysEntries,
                "右栏仍开着"
            );
        });
    }

    /// 二级时收到 configChanged：库还在就重取当前页；库没了回一级。
    #[test]
    fn config_changed_while_viewing() {
        with_mock(|| {
            let st = viewing("cell_idiom");
            st.goto_page(2);
            crate::rpc::take_mock_calls();
            assert!(st.on_schema_config_changed(Some("py"), Some("schema.saveConfig")));
            let calls = crate::rpc::take_mock_calls();
            let page: Vec<&Value> = calls
                .iter()
                .filter(|(m, _)| m == "sysDict.entries")
                .map(|(_, p)| p)
                .collect();
            assert_eq!(page.len(), 1);
            assert_eq!(page[0]["offset"], 100, "停在原页");

            // 活动方案换成没有这本库的第三方方案：回一级
            st.schemas.borrow_mut().push(SchemaInfo {
                id: "third_py".into(),
                name: "第三方".into(),
                kind: "pinyin".into(),
                scheme: "full".into(),
                ..Default::default()
            });
            crate::rpc::force_mock_reply("schema.active", Some(json!({"id": "third_py"})));
            st.reload();
            crate::rpc::force_mock_reply("schema.active", None);
            assert!(st.cats.viewing.get().is_none());
        });
    }

    fn headers(st: &DictManagerState) -> Vec<&'static str> {
        st.current_spec().columns.iter().map(|c| c.header).collect()
    }

    /// 右栏窄：一级收起「分组」「优先级」、二级收起「子表」，表格换列重建；按下标记的排序取消。
    #[test]
    fn narrow_pane_drops_secondary_columns() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            assert_eq!(headers(&st), ["启用", "名称", "分组", "条数", "优先级"]);
            st.sort.set(Some(SortKey {
                column: 3,
                order: SortOrder::Desc,
            }));
            st.on_sort_changed();
            let epoch = st.table_epoch.get();
            st.set_sys_narrow(true);
            assert_eq!(headers(&st), ["", "名称", "条数"]);
            assert_ne!(st.table_epoch.get(), epoch, "换列重建");
            assert_eq!(st.sort.get(), None);
            assert_eq!(st.table_rows.get()[0].len(), 3, "行也按新列取");
            assert_eq!(st.table_rows.get()[0][1], "rime_frost", "回默认顺序");

            st.enter_sys_dict("pinyin_main");
            assert_eq!(headers(&st), ["编码", "词条", "权重"], "子表列收起");
            st.sort.set(Some(SortKey {
                column: 2,
                order: SortOrder::Asc,
            }));
            st.on_sort_changed();
            st.next_page();
            crate::rpc::take_mock_calls();
            st.set_sys_narrow(false);
            assert_eq!(headers(&st), ["编码", "词条", "权重", "子表"]);
            assert_eq!(
                calls_of("sysDict.entries"),
                vec![json!({"schema": "py", "dictId": "pinyin_main", "offset": 0, "limit": 100})],
                "取消排序回第一页"
            );
            st.set_sys_narrow(false);
            assert!(calls_of("sysDict.entries").is_empty(), "没变不重取");
        });
    }

    /// 量宽叶子：宽度跨过阈值才换，宽窄没变不动。
    #[test]
    fn width_probe_switches_only_across_the_threshold() {
        let narrow = Rc::new(Cell::new(false));
        let seen = Rc::new(Cell::new(None));
        let s2 = seen.clone();
        let n2 = narrow.clone();
        let mut p = WidthProbe {
            width: Cell::new(0),
            apply: Box::new(move |n| {
                n2.set(n);
                s2.set(Some(n));
            }),
            narrow: narrow.clone(),
            witness: signal(()),
        };
        assert_eq!(p.wants_narrow(), None, "还没量到");
        p.width.set(SYS_NARROW_BELOW + 50);
        assert_eq!(p.wants_narrow(), None);
        p.width.set(SYS_NARROW_BELOW - 1);
        assert_eq!(p.wants_narrow(), Some(true));
        windui::testing::run_with_ctx(|ctx| windui::core::Widget::on_update(&mut p, ctx));
        assert_eq!(seen.get(), Some(true));
        assert_eq!(p.wants_narrow(), None, "已换");
    }

    /// 拼音域加一个没有任何系统词库的第三方方案，并把它设成活动方案（mock 回复留着，调用方清）。
    fn switch_to_third(st: &DictManagerState) {
        st.schemas.borrow_mut().push(SchemaInfo {
            id: "third_py".into(),
            name: "第三方".into(),
            kind: "pinyin".into(),
            scheme: "full".into(),
            ..Default::default()
        });
        crate::rpc::force_mock_reply("schema.active", Some(json!({"id": "third_py"})));
    }

    /// 一级表重取到空清单：收起右栏，并把右栏换成用户词库的表（不留着空的系统词库表）。
    #[test]
    fn empty_list_closes_the_pane_and_reloads() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            switch_to_third(&st);
            crate::rpc::take_mock_calls();
            st.reload();
            let calls: Vec<String> = crate::rpc::take_mock_calls()
                .into_iter()
                .map(|(m, _)| m)
                .collect();
            crate::rpc::force_mock_reply("schema.active", None);
            assert!(!st.cats.open.get());
            assert_ne!(st.current_category(), WordCategory::SysDicts);
            let list = st.current_spec().rpc.list;
            let at = calls
                .iter()
                .rposition(|m| m == "sysDict.categories")
                .unwrap();
            assert!(
                calls[at..].iter().any(|m| m == list),
                "收起后补取用户词库：{calls:?}"
            );
        });
    }

    /// 二级时刷新清单发现库没了（右栏收起）：同样补取，不停在旧词条上。
    #[test]
    fn refresh_that_closes_the_pane_reloads() {
        with_mock(|| {
            let st = viewing("cell_idiom");
            switch_to_third(&st);
            crate::rpc::take_mock_calls();
            st.refresh_sys_cats();
            let calls: Vec<String> = crate::rpc::take_mock_calls()
                .into_iter()
                .map(|(m, _)| m)
                .collect();
            crate::rpc::force_mock_reply("schema.active", None);
            assert!(!st.cats.open.get());
            let list = st.current_spec().rpc.list;
            assert!(calls.iter().any(|m| m == list), "{calls:?}");
        });
    }

    /// 二级时重取清单、库还在：头部也重取（版本、子表可能变了）。
    #[test]
    fn refresh_while_viewing_refetches_the_header() {
        with_mock(|| {
            let st = viewing("pinyin_main");
            crate::rpc::take_mock_calls();
            st.refresh_sys_cats();
            assert_eq!(
                calls_of("sysDict.header"),
                vec![json!({"schema": "py", "dictId": "pinyin_main"})]
            );
            assert_eq!(st.cats.viewing.get().as_deref(), Some("pinyin_main"));
        });
    }
}
