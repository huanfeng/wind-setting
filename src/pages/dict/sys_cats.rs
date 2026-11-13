//! 「系统词库」（主仓设计 `system-dict-patch-and-candidate-adjust.md` §6.1）：中栏一个节点，
//! 右栏两级。
//!
//! - 中栏：方案域里一个「系统词库」节点（无计数、无子项），节点见 `category_nav`。
//! - 右栏一级：词库表（主词库 + 分类 / 附加 / 我放入的库），列 启用 / 名称 / 分组 / 条数 /
//!   优先级 / 来源，按生效先后排（core 给的 `order`）。主库与写了 `default` 的库开关禁用。
//! - 右栏二级：点一行进入一本库：面包屑「系统词库 / <名>」+ 返回、头部信息
//!   卡片、只读词条表（编码 / 词 / 权重 / 子表，分页 + 编码前缀或词文本搜索），行操作「加进
//!   用户词库」复用加词对话框。
//!
//! 一二级都不是表格类别，不占子标签、不进导出（见 `state::tabs_for_kind` 的说明）。
//!
//! 开关拨动即生效，与分库开关同一套：对域内**有这张库**的每个成员方案各调一次
//! `schema.setDictEnabled`，任一失败就把已改的改回原值（[`DictManagerState::set_sys_cat_enabled`]）。
//! 清单取自 core `sysDict.categories`，切到别的方案或收到 `schema.configChanged` 时重取；
//! 正在看的那本库不在新清单里就回一级，否则头部与当前页一并重取。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

use super::state::DictManagerState;
use crate::rpc;

/// 中栏节点、右栏一级标题与面包屑首段。
pub(crate) const SYS_TITLE: &str = "系统词库";
/// 方案文件没写 `group` 的随包库归这一组。
pub(crate) const OTHER_GROUP: &str = "其他";
/// 发现目录（`dir:` 前缀）登记的库归这一组，也是它的来源名。
pub(crate) const DIR_GROUP: &str = "我放入的";
/// 二级词条表一页条数。
pub(crate) const ENTRY_PAGE: u64 = 100;

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
}

/// 一本系统词库（`sysDict.categories` 的一项）。
#[derive(Clone, Debug, PartialEq)]
pub struct SysCat {
    pub id: String,
    pub label: String,
    pub description: String,
    /// 展示用的分组名（已按来源归好「其他」「我放入的」；主库为空）。
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

/// `sysDict.categories` 的返回 → 清单（缺 `id` 的项丢弃），按 `order` 排。
pub(crate) fn parse_cats(v: &Value) -> Vec<SysCat> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    let mut out: Vec<SysCat> = arr
        .iter()
        .filter_map(|it| {
            let s = |k: &str| it.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let id = it
                .get("id")?
                .as_str()
                .filter(|s| !s.is_empty())?
                .to_string();
            let source = SysSource::parse(&s("source"));
            let group = match source {
                SysSource::Main => String::new(),
                SysSource::Dir => DIR_GROUP.to_string(),
                SysSource::Bundled => Some(s("group"))
                    .filter(|g| !g.is_empty())
                    .unwrap_or_else(|| OTHER_GROUP.to_string()),
            };
            Some(SysCat {
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
            })
        })
        .collect();
    out.sort_by_key(|c| c.order); // 稳定排序：旧版 core 没有 order 时保持原顺序
    out
}

/// 一级表的文本行（启用 / 名称 / 分组 / 条数 / 优先级 / 来源）。启用与名称两格另画。
pub(crate) fn cat_rows(cats: &[SysCat]) -> Vec<Vec<String>> {
    cats.iter()
        .map(|c| {
            vec![
                if c.enabled { "开" } else { "关" }.to_string(),
                c.label.clone(),
                if c.group.is_empty() {
                    "—".to_string()
                } else {
                    c.group.clone()
                },
                count_text(c.count),
                c.order.to_string(),
                c.source.label().to_string(),
            ]
        })
        .collect()
}

fn count_text(n: Option<u64>) -> String {
    n.map_or_else(|| "—".to_string(), |n| n.to_string())
}

/// 一行只读词条（`sysDict.entries` 的 rows）。
#[derive(Clone, Debug, PartialEq)]
pub struct SysEntry {
    pub code: String,
    pub text: String,
    pub weight: i64,
    pub table: String,
}

fn parse_entries(v: &Value) -> (u64, Vec<SysEntry>) {
    let total = v.get("total").and_then(Value::as_u64).unwrap_or(0);
    let rows = v
        .get("rows")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|r| {
                    let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    SysEntry {
                        code: s("code"),
                        text: s("text"),
                        weight: r.get("weight").and_then(Value::as_i64).unwrap_or(0),
                        table: s("table"),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    (total, rows)
}

/// 二级头部卡片（`sysDict.header`）。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SysHeader {
    pub label: String,
    /// (字段名, 值)：已按卡片顺序排好、空值不出。
    pub fields: Vec<(String, String)>,
    pub notes: String,
    pub path: String,
    /// 子表（名, 条数）。
    pub tables: Vec<(String, Option<u64>)>,
    /// 子表一行（「名（条数）、…」，缺失的标「缺失」）；没有子表为空。
    pub tables_text: String,
    /// 一行摘要（收起时显示）。
    pub summary: String,
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
    push("条数", count_text(count));
    push("来源", source.label().to_string());
    push("修改时间", mtime.clone().unwrap_or_default());
    let mut summary = vec![
        source.label().to_string(),
        format!("{} 条", count_text(count)),
    ];
    if !s("version").is_empty() {
        summary.push(format!("版本 {}", s("version")));
    }
    if !tables.is_empty() {
        summary.push(format!("{} 张子表", tables.len()));
    }
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
        summary: summary.join(" · "),
    }
}

/// 系统词库的状态（Signal 为 Copy，整体可 Clone 进闭包）。
#[derive(Clone)]
pub struct SysCatsState {
    /// 当前方案的清单（按生效先后）。
    pub list: Signal<Vec<SysCat>>,
    /// 右栏是否换成了系统词库（点中栏「系统词库」打开，点别的节点或换数据域关上）。
    pub open: Signal<bool>,
    /// 一级表整体重建（清单变化时递增）。
    pub epoch: Signal<Vec<u64>>,
    /// 右栏标题旁的「已启用 n / 共 m」。
    pub summary: Signal<String>,
    /// 二级：正在看的词库 id；`None` = 一级。
    pub viewing: Signal<Option<String>>,
    pub header: Signal<SysHeader>,
    /// 头部卡片展开。
    pub card_open: Signal<bool>,
    /// 当前页词条（按显示下标取行）。
    pub entries: Rc<RefCell<Vec<SysEntry>>>,
    pub entry_rows: Signal<Vec<Vec<String>>>,
    pub entry_sort: Signal<Option<SortKey>>,
    pub total: Signal<u64>,
    pub offset: Rc<Cell<u64>>,
    pub page_text: Signal<String>,
    /// 搜索框里的字与方式（0 编码前缀 / 1 词文本）。打字不发请求，回车或点「搜索」才生效。
    pub query: Signal<String>,
    pub mode: Signal<usize>,
    /// 生效中的搜索（`None` = 按文件顺序浏览）：（去首尾空白的查询, 方式）。
    searched: Rc<RefCell<Option<(String, usize)>>>,
    /// 当前页里选中的行（显示下标）；换页、换库即清。工具栏「加进用户词库」据此可用。
    pub selected: Signal<Option<usize>>,
    /// 二级词条表整表重建（换库时递增：有没有子表列随库而变）。
    pub entries_epoch: Signal<Vec<u64>>,
    /// 头部卡片重建（头部数据或展开态变化）。与词条表分开：展开卡片不重建词条表。
    pub card_epoch: Signal<Vec<u64>>,
    /// 清单是按哪个方案的开关取的（拼音族是活动成员，见 `lib_switch_schema`）；与当前不同时
    /// [`DictManagerState::reload`] 重取一次。
    loaded_for: Rc<RefCell<Option<String>>>,
    /// 本端最近一次写开关的时刻：之后不久收到的 `schema.setDictEnabled` 广播是自己的回声，
    /// 写完已经重取过，不再重取。
    last_write: Rc<Cell<Option<std::time::Instant>>>,
}

/// 回声窗口：本端写开关后这么久之内的同类广播当回声。
const ECHO_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

impl SysCatsState {
    pub fn new() -> Self {
        Self {
            list: signal(Vec::new()),
            open: signal(false),
            epoch: signal(vec![0]),
            summary: signal(String::new()),
            viewing: signal(None),
            header: signal(SysHeader::default()),
            card_open: signal(false),
            entries: Rc::new(RefCell::new(Vec::new())),
            entry_rows: signal(Vec::new()),
            entry_sort: signal(None),
            total: signal(0),
            offset: Rc::new(Cell::new(0)),
            page_text: signal(String::new()),
            query: signal(String::new()),
            mode: signal(0),
            searched: Rc::new(RefCell::new(None)),
            selected: signal(None),
            entries_epoch: signal(vec![0]),
            card_epoch: signal(vec![0]),
            loaded_for: Rc::new(RefCell::new(None)),
            last_write: Rc::new(Cell::new(None)),
        }
    }

    fn bump_entries_epoch(&self) {
        let e = self.entries_epoch.get().first().copied().unwrap_or(0);
        self.entries_epoch.set(vec![e + 1]);
    }

    fn bump_card_epoch(&self) {
        let e = self.card_epoch.get().first().copied().unwrap_or(0);
        self.card_epoch.set(vec![e + 1]);
    }

    /// 正在看的库有没有子表（词条表据此多一列「子表」）。
    pub(crate) fn has_tables(&self) -> bool {
        !self.header.get().tables.is_empty()
    }

    /// 二级词条表的文本行（编码 / 词 / 权重 [/ 子表]）。
    fn entry_text_rows(&self, rows: &[SysEntry]) -> Vec<Vec<String>> {
        let tables = self.has_tables();
        rows.iter()
            .map(|r| {
                let mut v = vec![r.code.clone(), r.text.clone(), r.weight.to_string()];
                if tables {
                    v.push(r.table.clone());
                }
                v
            })
            .collect()
    }
}

impl Default for SysCatsState {
    fn default() -> Self {
        Self::new()
    }
}

impl DictManagerState {
    /// 清单还不是为当前方案取的就取一次（[`Self::reload`] 开头调用）。
    pub(crate) fn ensure_sys_cats(&self) {
        if *self.cats.loaded_for.borrow() != self.sys_cats_scope() {
            self.refresh_sys_cats();
        }
    }

    /// 清单按哪个方案取：方案域里读开关的那个成员（拼音族取活动成员）；全局域为 `None`。
    fn sys_cats_scope(&self) -> Option<String> {
        self.current_schema_id()
            .and_then(|_| self.lib_switch_schema())
    }

    /// 重取清单；有变化时重建中栏与一级表。不是方案域、或 core 不认这个方法（旧版）时为空，
    /// 中栏随之不出「系统词库」。正在看的库还在就重取二级，不在了回一级。
    pub fn refresh_sys_cats(&self) {
        let scope = self.sys_cats_scope();
        let list = match &scope {
            Some(id) => match fetch_cats(id) {
                Ok(l) => l,
                Err(e) => {
                    // 取不到（core 忙、旧版不认）：留着旧清单，不把右栏清空；`loaded_for` 不动，
                    // 下次 reload 见它与当前不同会再取
                    tracing::debug!("取系统词库清单失败：{e}");
                    return;
                }
            },
            None => Vec::new(),
        };
        *self.cats.loaded_for.borrow_mut() = scope;
        if list.is_empty() && self.cats.open.get() {
            self.close_sys_cats();
        }
        let had_nav = !self.cats.list.get().is_empty();
        if list != self.cats.list.get() {
            let on = list.iter().filter(|c| c.enabled).count();
            self.cats
                .summary
                .set(format!("已启用 {on} / 共 {}", list.len()));
            let e = self.cats.epoch.get().first().copied().unwrap_or(0);
            self.cats.epoch.set(vec![e + 1]);
            self.cats.list.set(list);
            // 中栏只认「有没有」：开关、条数变化不必重建
            if had_nav != !self.cats.list.get().is_empty() {
                self.bump_nav_epoch();
            }
        }
        if let Some(id) = self.cats.viewing.get() {
            if self.cats.list.get().iter().any(|c| c.id == id) {
                self.load_sys_view();
            } else {
                self.cats.viewing.set(None);
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
    }

    /// 收起系统词库（点别的节点、换数据域）：下次打开回一级。
    pub(crate) fn close_sys_cats(&self) {
        if self.cats.open.get() {
            self.cats.open.set(false);
        }
        if self.cats.viewing.get().is_some() {
            self.cats.viewing.set(None);
        }
    }

    /// 进入一本库（二级）：清搜索、回首页，取头部与首页词条。
    pub fn enter_sys_dict(&self, id: &str) {
        if !self.cats.list.get().iter().any(|c| c.id == id) {
            return;
        }
        self.cats.offset.set(0);
        if !self.cats.query.get().is_empty() {
            self.cats.query.set(String::new());
        }
        if self.cats.mode.get() != 0 {
            self.cats.mode.set(0);
        }
        *self.cats.searched.borrow_mut() = None;
        self.cats.viewing.set(Some(id.to_string()));
        self.load_sys_view();
        self.cats.bump_entries_epoch();
    }

    /// 二级回一级。
    pub fn leave_sys_dict(&self) {
        self.cats.viewing.set(None);
        self.cats.entries.borrow_mut().clear();
        self.cats.entry_rows.set(Vec::new());
    }

    /// 取正在看的库的头部与当前页。
    fn load_sys_view(&self) {
        let (Some(schema), Some(id)) = (
            self.cats.loaded_for.borrow().clone(),
            self.cats.viewing.get(),
        ) else {
            return;
        };
        let had_tables = self.cats.has_tables();
        match rpc::call_core("sysDict.header", &json!({ "schema": schema, "dictId": id })) {
            Ok(v) => self.cats.header.set(parse_header(&v)),
            Err(e) => {
                let label = self.sys_cat_label(&id);
                self.cats.header.set(SysHeader {
                    label: label.clone(),
                    summary: format!("读不到词库头部：{e}"),
                    ..Default::default()
                });
            }
        }
        if had_tables != self.cats.has_tables() {
            self.cats.bump_entries_epoch();
        }
        self.load_sys_page();
    }

    fn sys_cat_label(&self, id: &str) -> String {
        self.cats
            .list
            .get()
            .into_iter()
            .find(|c| c.id == id)
            .map_or_else(|| id.to_string(), |c| c.label)
    }

    /// 面包屑第二段：头部卡片的 label，取不到用清单里的名字。
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

    /// `sysDict.entries` 的参数（当前库、搜索与页）。
    pub(crate) fn sys_entries_params(&self) -> Option<Value> {
        let schema = self.cats.loaded_for.borrow().clone()?;
        let id = self.cats.viewing.get()?;
        let mut p = json!({
            "schema": schema,
            "dictId": id,
            "offset": self.cats.offset.get(),
            "limit": ENTRY_PAGE,
        });
        if let Some((q, mode)) = self.cats.searched.borrow().as_ref() {
            p["query"] = json!(q);
            p["mode"] = json!(if *mode == 1 { "text" } else { "code" });
        }
        Some(p)
    }

    /// 取当前页词条。
    pub fn load_sys_page(&self) {
        let Some(p) = self.sys_entries_params() else {
            return;
        };
        let (total, rows, note) = match rpc::call_core("sysDict.entries", &p) {
            Ok(v) => {
                let (t, r) = parse_entries(&v);
                (t, r, v["note"].as_str().map(str::to_string))
            }
            Err(e) => {
                self.notify_err(&format!("读取词条失败：{e}"));
                (0, Vec::new(), None)
            }
        };
        let off = self.cats.offset.get();
        // 停在了末页之后（库变短了 / 搜索结果变少了）：跳到最后一页重取
        if rows.is_empty() && total > 0 && off >= total {
            self.cats.offset.set(last_page_offset(total));
            return self.load_sys_page();
        }
        let text = if let Some(n) = note.filter(|_| total == 0) {
            // 整库被加载器跳过（columns: 声明残缺）：说清为什么没有词条
            n
        } else if total == 0 {
            if self.cats.searched.borrow().is_none() {
                "没有词条".to_string()
            } else {
                "没有匹配的词条".to_string()
            }
        } else {
            format!(
                "第 {}-{} / 共 {total} 条",
                off + 1,
                (off + rows.len() as u64).max(off + 1)
            )
        };
        self.cats.page_text.set(text);
        self.cats.total.set(total);
        self.cats.entry_rows.set(self.cats.entry_text_rows(&rows));
        *self.cats.entries.borrow_mut() = rows;
        if self.cats.selected.get().is_some() {
            self.cats.selected.set(None);
        }
    }

    /// 回车或点「搜索」：按框里的字与方式搜，回首页。框是空的就回到浏览。
    pub fn submit_sys_search(&self) {
        if self.cats.viewing.get().is_none() {
            return;
        }
        let q = self.cats.query.get().trim().to_string();
        *self.cats.searched.borrow_mut() = (!q.is_empty()).then(|| (q, self.cats.mode.get()));
        self.cats.offset.set(0);
        self.load_sys_page();
    }

    /// 切换搜索方式：已在搜索就按新方式重搜（一次点击一次请求），否则只记下。
    pub fn on_sys_mode_changed(&self) {
        if self.cats.searched.borrow().is_some() {
            self.submit_sys_search();
        }
    }

    /// ×：清空搜索框并回到浏览。
    pub fn clear_sys_search(&self) {
        if !self.cats.query.get().is_empty() {
            self.cats.query.set(String::new());
        }
        if self.cats.searched.borrow_mut().take().is_some() {
            self.cats.offset.set(0);
            self.load_sys_page();
        }
    }

    /// 有没有生效中的搜索（× 是否可用）。
    pub(crate) fn sys_searching(&self) -> bool {
        self.cats.searched.borrow().is_some()
    }

    pub(crate) fn sys_has_prev(&self) -> bool {
        self.cats.offset.get() > 0
    }

    pub(crate) fn sys_has_next(&self) -> bool {
        self.cats.offset.get() + ENTRY_PAGE < self.cats.total.get()
    }

    /// 翻页：`to` 为目标偏移（夹到最后一页之内）。
    pub(crate) fn sys_goto(&self, to: u64) {
        let to = to.min(last_page_offset(self.cats.total.get()));
        if to != self.cats.offset.get() {
            self.cats.offset.set(to);
            self.load_sys_page();
        }
    }

    /// 「加进用户词库」：落到本域用户词库的主词库，打开加词对话框并预填编码与词。系统词库
    /// 的右栏保持原样（加完回来还在这一页）。
    pub fn add_sys_entry_to_user(&self, disp: usize) {
        let Some(e) = self.cats.entries.borrow().get(disp).cloned() else {
            return;
        };
        let Some(schema) = self.current_schema_id() else {
            return;
        };
        self.select_lib(None);
        self.open_add_word_prefilled(&e.text, &e.code, &schema);
    }

    /// 「打开所在目录」：在文件管理器里选中这本库的主文件。
    pub fn reveal_sys_dict(&self) {
        let path = self.cats.header.get().path;
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
        self.cats.last_write.set(Some(std::time::Instant::now()));
        self.refresh_sys_cats();
        ok
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

/// 最后一页的偏移。
fn last_page_offset(total: u64) -> u64 {
    total.saturating_sub(1) / ENTRY_PAGE * ENTRY_PAGE
}

/// 取一个方案的系统词库清单。
fn fetch_cats(schema: &str) -> Result<Vec<SysCat>, String> {
    rpc::call_core("sysDict.categories", &json!({ "schema": schema })).map(|v| parse_cats(&v))
}

// ── 右栏 ─────────────────────────────────────────────────────

/// 右栏：一级词库表与二级单库查看按 `viewing` 切换。
pub fn build_sys_cats_pane(mgr: &DictManagerState) -> Element {
    let (v1, v2) = (mgr.cats.viewing, mgr.cats.viewing);
    Element::stack()
        .fill()
        .child(build_list_level(mgr).visible_when(move || v1.get().is_none()))
        .child(build_view_level(mgr).visible_when(move || v2.get().is_some()))
}

fn pane_col() -> Element {
    Element::col()
        .fill()
        .bg_role(Role::SurfaceAlt)
        .padding_xy(20, 16)
        .spacing(12)
}

/// 一级表的列（标题, 权重）。不用 `table_sortable_server`：表按生效先后排、不该被点表头重排，
/// 那个表格每列固定留排序箭头槽、左右内边距 14，820 宽的窗里六列放不下「优先级」「我放入的」。
const CAT_COLS: [(&str, f32); 6] = [
    ("启用", 0.85),
    ("名称", 1.8),
    ("分组", 1.4),
    ("条数", 1.25),
    ("优先级", 1.15),
    ("来源", 1.35),
];

/// 一级表的一格：内边距收窄到 8，左对齐、竖直居中。
fn cat_cell(w: f32, content: Element) -> Element {
    Element::row()
        .weight(w)
        .cross(Align::Center)
        .padding_xy(8, 6)
        .child(content)
}

fn cat_text(s: String, role: Role) -> Element {
    Element::label(s)
        .font_size(12.5)
        .fg_role(role)
        .max_lines(1)
        .truncate(Truncate::End)
        .width_match()
}

/// 一级：标题 + 说明 + 词库表（表头 + 各行；点行进入二级，勾选框只管开关）。
fn build_list_level(mgr: &DictManagerState) -> Element {
    let cats = mgr.cats.clone();
    // 窄窗口下让计数先截，标题保持完整。
    let summary = Element::label_signal(cats.summary)
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .max_lines(1)
        .weight(1.0)
        .text_align(Align::End);
    let title_row = Element::row()
        .width_match()
        .height(30)
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::label(SYS_TITLE)
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .max_lines(1),
        )
        .child(summary);
    let hint = Element::label("勾选即生效；按生效先后排列，优先级只读；点一行查看词条。")
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(1)
        .truncate(Truncate::End);

    let mut header = Element::row()
        .width_match()
        .cross(Align::Center)
        .bg_role(Role::SurfaceAlt);
    for (t, w) in CAT_COLS {
        header = header.child(cat_cell(
            w,
            Element::label(t)
                .font_size(windui::theme::current().metrics.font_sm)
                .font_weight(600)
                .fg_role(Role::TextMuted)
                .max_lines(1)
                .truncate(Truncate::End)
                .width_match(),
        ));
    }
    let m = mgr.clone();
    let body = Element::host_signal(cats.epoch, move |_| build_cat_rows(&m));
    let table = Element::col()
        .fill()
        .child(header)
        .child(Element::divider())
        .child(Element::scroll().fill().weight(1.0).child(body));
    let card = crate::widgets::card_frame()
        .width_match()
        .weight(1.0)
        .padding_xy(4, 2)
        .child(table);
    pane_col().child(title_row).child(hint).child(card)
}

/// 一级表各行。
fn build_cat_rows(mgr: &DictManagerState) -> Element {
    let list = mgr.cats.list.get();
    let texts = cat_rows(&list);
    let mut col = Element::col().width_match();
    for (i, (c, t)) in list.iter().zip(texts).enumerate() {
        let (m, id) = (mgr.clone(), c.id.clone());
        let mut row = Element::row()
            .width_match()
            .cross(Align::Center)
            .clickable()
            .on_click(move |_| m.enter_sys_dict(&id))
            .child(cat_cell(CAT_COLS[0].1, cat_toggle(mgr, c)))
            .child(cat_cell(CAT_COLS[1].1, cat_name(c)));
        for (k, s) in t.into_iter().enumerate().skip(2) {
            let role = if k == 4 { Role::TextMuted } else { Role::Text };
            row = row.child(cat_cell(CAT_COLS[k].1, cat_text(s, role)));
        }
        if i % 2 == 1 {
            row = row.bg_role(Role::SurfaceAlt);
        }
        col = col.child(row).child(Element::divider());
    }
    col
}

/// 启用格：勾选框；主库与关不掉的库禁用。
fn cat_toggle(mgr: &DictManagerState, c: &SysCat) -> Element {
    let sig = signal(c.enabled);
    let cb = Element::checkbox("", sig).small();
    if !c.togglable {
        let tip = if c.source == SysSource::Main {
            "主词库总是启用"
        } else {
            "方案把它设为必选，不能关"
        };
        return cb.enabled(false).tooltip(tip);
    }
    let (m, id) = (mgr.clone(), c.id.clone());
    cb.on_toggle(move |_| {
        let next = !sig.get();
        sig.set(next);
        if !m.set_sys_cat_enabled(&id, next) {
            sig.set(!next);
        }
    })
}

/// 名称格：名称 + 说明（次要文字，悬停看全文）。
fn cat_name(c: &SysCat) -> Element {
    let mut name = Element::col().width_match().spacing(1).child(
        Element::label(c.label.clone())
            .font_size(12.5)
            .fg_role(Role::Text)
            .max_lines(1)
            .truncate(Truncate::End),
    );
    if !c.description.is_empty() {
        name = name
            .child(
                Element::label(c.description.clone())
                    .font_size(11.0)
                    .fg_role(Role::TextMuted)
                    .max_lines(1)
                    .truncate(Truncate::End),
            )
            .tooltip(c.description.clone());
    }
    name
}

/// 二级：面包屑 + 头部卡片 + 搜索 + 词条表 + 翻页。
fn build_view_level(mgr: &DictManagerState) -> Element {
    let cats = mgr.cats.clone();
    let m_back = mgr.clone();
    let m_title = mgr.clone();
    let crumb_title = signal(String::new());
    let crumb_eff = {
        let (h_v, v_v) = (cats.header, cats.viewing);
        let m = mgr.clone();
        super::reactive::reactive_effect(
            move || {
                h_v.version()
                    .wrapping_mul(1_000_003)
                    .wrapping_add(v_v.version())
            },
            move || crumb_title.set(m.sys_view_title()),
        )
    };
    crumb_title.set(m_title.sys_view_title());
    let crumb = Element::row()
        .width_match()
        .height(30)
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::button("‹ 返回")
                .small()
                .outline_soft()
                .on_click(move |_| m_back.leave_sys_dict()),
        )
        .child(
            Element::label(format!("{SYS_TITLE} /"))
                .font_size(14.0)
                .fg_role(Role::TextMuted)
                .max_lines(1),
        )
        .child(
            Element::label_signal(crumb_title)
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .max_lines(1)
                .truncate(Truncate::End)
                .weight(1.0),
        );

    let m_card = mgr.clone();
    let (h_sig, co_sig) = (cats.header, cats.card_open);
    let card_body =
        Element::host_signal(cats.card_epoch, move |_| build_header_card(&m_card)).width_match();
    // 头部数据变了或展开态变了都重画卡片：挂在两个信号上
    let m_card2 = mgr.clone();
    let card_eff = super::reactive::reactive_effect(
        move || {
            h_sig
                .version()
                .wrapping_mul(1_000_003)
                .wrapping_add(co_sig.version())
        },
        move || m_card2.cats.bump_card_epoch(),
    );

    let small_h = windui::theme::current().metrics.control_h_sm;
    let (m_mode, m_submit, m_go, m_clr, m_clr_en) = (
        mgr.clone(),
        mgr.clone(),
        mgr.clone(),
        mgr.clone(),
        mgr.clone(),
    );
    let (q_en, sel_en) = (cats.query, cats.selected);
    let (m_add, m_add_en) = (mgr.clone(), mgr.clone());
    let search = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(6)
        .child(
            Element::segmented(vec!["编码", "词"], cats.mode)
                .on_segment_change(move |_, _| m_mode.on_sys_mode_changed()),
        )
        .child(
            Element::text_input(cats.query, "编码前缀或词，回车搜索")
                .on_submit(move |_| m_submit.submit_sys_search())
                .weight(1.0)
                .max_width(260)
                .height(small_h),
        )
        .child(
            Element::button("搜索")
                .small()
                .outline_soft()
                .on_click(move |_| m_go.submit_sys_search())
                .enabled_when(move || !q_en.get().trim().is_empty()),
        )
        .child(
            Element::button("×")
                .small()
                .outline_soft()
                .tooltip("清除搜索")
                .on_click(move |_| m_clr.clear_sys_search())
                .enabled_when(move || m_clr_en.sys_searching()),
        )
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("加进用户词库…")
                .small()
                .outline_soft()
                .tooltip("把选中的词条加进用户词库（也可双击词条）")
                .on_click(move |_| {
                    if let Some(row) = sel_en.get() {
                        m_add.add_sys_entry_to_user(row);
                    }
                })
                .enabled_when(move || m_add_en.cats.selected.get().is_some()),
        );

    let m_tab = mgr.clone();
    let table = Element::host_signal(cats.entries_epoch, move |_| build_entries_table(&m_tab));
    let card = crate::widgets::card_frame()
        .width_match()
        .weight(1.0)
        .padding_xy(4, 2)
        .child(table);

    pane_col()
        .child(crumb)
        .child(card_body)
        .child(search)
        .child(card)
        .child(build_sys_pagination(mgr))
        .child(crumb_eff)
        .child(card_eff)
}

/// 头部信息卡片：收起时一行摘要，展开为两列字段 + 说明 + 路径。右上「打开所在目录」与展开开关。
fn build_header_card(mgr: &DictManagerState) -> Element {
    let h = mgr.cats.header.get();
    let open = mgr.cats.card_open.get();
    let co = mgr.cats.card_open;
    // 收起时只有摘要与「详情」：窄窗里一行放不下两枚按钮
    let head = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::label(h.summary.clone())
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .max_lines(1)
                .truncate(Truncate::End)
                .weight(1.0),
        )
        .child(
            Element::button(if open { "收起" } else { "详情" })
                .small()
                .outline_soft()
                .on_click(move |_| co.set(!co.get())),
        );
    let mut col = crate::widgets::card_frame()
        .width_match()
        .padding_xy(14, 10)
        .spacing(6)
        .child(head);
    if !open {
        return col;
    }
    let small = |s: String, role: Role, lines: usize| {
        Element::label(s)
            .font_size(12.0)
            .fg_role(role)
            .max_lines(lines)
            .truncate(Truncate::End)
    };
    // 两列：字段对半分，左列先满
    let half = h.fields.len().div_ceil(2);
    let field_col = |items: &[(String, String)]| {
        let mut c = Element::col().weight(1.0).spacing(3);
        for (k, v) in items {
            c = c.child(
                Element::row()
                    .width_match()
                    .spacing(6)
                    .child(small(k.clone(), Role::TextMuted, 1).width(56))
                    .child(small(v.clone(), Role::Text, 1).weight(1.0)),
            );
        }
        c
    };
    col = col.child(
        Element::row()
            .width_match()
            .spacing(16)
            .child(field_col(&h.fields[..half]))
            .child(field_col(&h.fields[half..])),
    );
    if !h.tables_text.is_empty() {
        col = col.child(
            Element::row()
                .width_match()
                .spacing(6)
                .child(small("子表".into(), Role::TextMuted, 1).width(56))
                .child(small(h.tables_text.clone(), Role::Text, 2).weight(1.0)),
        );
    }
    if !h.notes.is_empty() {
        col = col.child(small(h.notes.clone(), Role::TextMuted, 4).width_match());
    }
    let m_dir = mgr.clone();
    col.child(
        Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(8)
            .child(
                Element::label(h.path.clone())
                    .font_size(11.0)
                    .fg_role(Role::TextDisabled)
                    .max_lines(1)
                    .truncate(Truncate::Middle)
                    .weight(1.0)
                    .tooltip(h.path.clone()),
            )
            .child(
                Element::button("打开所在目录")
                    .small()
                    .outline_soft()
                    .enabled(!h.path.is_empty())
                    .on_click(move |_| m_dir.reveal_sys_dict()),
            ),
    )
}

/// 词条表：编码 / 词 / 权重（有子表时再加「子表」）。双击或右键「加进用户词库」——不另起
/// 按钮列：窄窗里它会把权重与子表挤成省略号。
fn build_entries_table(mgr: &DictManagerState) -> Element {
    let mut cols: Vec<(String, f32)> = vec![
        ("编码".to_string(), 1.3),
        ("词".to_string(), 2.0),
        ("权重".to_string(), 1.0),
    ];
    if mgr.cats.has_tables() {
        cols.push(("子表".to_string(), 1.4));
    }
    let sort = mgr.cats.entry_sort;
    let (m_menu, m_dbl) = (mgr.clone(), mgr.clone());
    let sel = mgr.cats.selected;
    Element::table_sortable_server(
        cols,
        mgr.cats.entry_rows,
        mgr.cats.entry_sort,
        move |_, _| {
            // 按文件顺序只读浏览，不排序
            sort.set(None);
        },
    )
    .cell_render(move |row, _, text| Some(pick_cell(sel, row, text)))
    .on_row_activate(move |_, row| m_dbl.add_sys_entry_to_user(row))
    .on_row_context_menu(move |row| sys_entry_menu(&m_menu, row))
    .fill()
}

/// 词条单元格：文字同默认格，按下即选中这一行（选中行带强调底色）。
fn pick_cell(sel: Signal<Option<usize>>, row: usize, text: &str) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .widget(RowPick { sel, row })
        .bg_when(move || {
            if sel.get() == Some(row) {
                windui::theme::current().palette.accent.scale_alpha(0.18)
            } else {
                Color::TRANSPARENT
            }
        })
        .child(
            Element::label(text.to_string())
                .font_size(windui::theme::current().metrics.font_sm)
                .max_lines(1)
                .truncate(Truncate::End)
                .weight(1.0),
        )
}

/// 按下（左右键都算）即把所在行记为选中。**不吃事件**：双击仍冒泡到行上触发「加进用户
/// 词库」，右键仍弹行菜单——表格只给了双击与右键两个行级钩子，单击选中只能挂在格上。
struct RowPick {
    sel: Signal<Option<usize>>,
    row: usize,
}

impl windui::core::Widget for RowPick {
    fn on_event(&mut self, _ctx: &mut windui::core::EventCtx, ev: &windui::event::Event) -> bool {
        if let windui::event::Event::Pointer(p) = ev
            && p.kind == windui::event::PointerKind::Down
            && self.sel.get() != Some(self.row)
        {
            self.sel.set(Some(self.row));
        }
        false
    }
}

/// 词条行右键：加进用户词库、复制词、复制编码。
pub(crate) fn sys_entry_menu(mgr: &DictManagerState, row: usize) -> Vec<MenuItem> {
    let Some(e) = mgr.cats.entries.borrow().get(row).cloned() else {
        return Vec::new();
    };
    let (m, m_t, m_c) = (mgr.clone(), mgr.clone(), mgr.clone());
    let (text, code) = (e.text.clone(), e.code.clone());
    vec![
        MenuItem::run(
            "加进用户词库…",
            move |_| m.add_sys_entry_to_user(row),
            false,
        ),
        MenuItem::separator(),
        MenuItem::run(
            "复制词",
            move |_| super::view::copy_to_clipboard(&m_t, &text),
            false,
        ),
        MenuItem::run(
            "复制编码",
            move |_| super::view::copy_to_clipboard(&m_c, &code),
            false,
        ),
    ]
}

/// 二级底栏：计数 + 首页 / 上一页 / 下一页 / 末页。
fn build_sys_pagination(mgr: &DictManagerState) -> Element {
    let (m_first, e_first) = (mgr.clone(), mgr.clone());
    let (m_prev, e_prev) = (mgr.clone(), mgr.clone());
    let (m_next, e_next) = (mgr.clone(), mgr.clone());
    let (m_last, e_last) = (mgr.clone(), mgr.clone());
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(6)
        .padding_xy(2, 4)
        .child(
            Element::label_signal(mgr.cats.page_text)
                .font_size(11.5)
                .fg_role(Role::TextMuted)
                .max_lines(1)
                .weight(1.0)
                .truncate(Truncate::End),
        )
        .child(
            Element::button("«")
                .tooltip("首页")
                .small()
                .outline_soft()
                .enabled_when(move || e_first.sys_has_prev())
                .on_click(move |_| m_first.sys_goto(0)),
        )
        .child(
            Element::button("‹ 上一页")
                .small()
                .outline_soft()
                .enabled_when(move || e_prev.sys_has_prev())
                .on_click(move |_| {
                    let o = m_prev.cats.offset.get();
                    m_prev.sys_goto(o.saturating_sub(ENTRY_PAGE));
                }),
        )
        .child(
            Element::button("下一页 ›")
                .small()
                .outline_soft()
                .enabled_when(move || e_next.sys_has_next())
                .on_click(move |_| {
                    let o = m_next.cats.offset.get();
                    m_next.sys_goto(o + ENTRY_PAGE);
                }),
        )
        .child(
            Element::button("»")
                .tooltip("末页")
                .small()
                .outline_soft()
                .enabled_when(move || e_last.sys_has_next())
                .on_click(move |_| m_last.sys_goto(u64::MAX)),
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
        assert_eq!(
            (l[0].source, l[0].togglable, l[0].group.as_str()),
            (SysSource::Main, false, "")
        );
        assert_eq!(
            (l[1].label.as_str(), l[1].group.as_str(), l[1].count),
            ("wb_extra", OTHER_GROUP, None),
            "没名字用 id，没分组归「其他」，数不出为 None"
        );
        assert_eq!(l[3].group, DIR_GROUP, "发现目录的不看 group");
        assert_eq!(
            cat_rows(&l),
            vec![
                vec!["开", "rime_frost", "—", "654116", "1", "主词库"],
                vec!["关", "wb_extra", "其他", "—", "2", "随包"],
                vec!["开", "成语", "文史", "48721", "3", "随包"],
                vec!["关", "我的", "我放入的", "3", "4", "我放入的"],
            ]
        );
        assert!(parse_cats(&json!({"error": 1})).is_empty());
    }

    #[test]
    fn header_card_fields_and_summary() {
        let h = parse_header(&json!({
            "id": "pinyin_main", "label": "rime_frost", "source": "main", "name": "rime_frost",
            "version": "2023-11-13", "sort": null, "columns": [], "importTables": ["a", "b"],
            "notes": "说明", "path": "/x/rime_frost.dict.yaml", "mtimeMs": null, "count": null,
            "tables": [ {"name": "a", "count": 3}, {"name": "b", "count": null} ]
        }));
        let keys: Vec<&str> = h.fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["名称", "版本", "条数", "来源"], "空值不出");
        assert_eq!(h.fields[2].1, "—");
        assert_eq!(h.tables_text, "a（3）、b（缺失）");
        assert_eq!(h.summary, "主词库 · — 条 · 版本 2023-11-13 · 2 张子表");
        assert_eq!(h.tables.len(), 2);
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

    #[test]
    fn reload_fetches_the_list_once_per_schema() {
        with_mock(|| {
            let st = pinyin_state("sp");
            let l = st.cats.list.get();
            assert_eq!(l[0].id, "pinyin_main", "主库在首行");
            assert!(st.cats.epoch.get()[0] > 0, "一级表跟着清单重建");
            crate::rpc::take_mock_calls();
            st.reload();
            assert!(calls_of("sysDict.categories").is_empty(), "同一方案不重取");
            st.domain.set(super::super::state::DOMAIN_PHRASE);
            st.on_nav_changed();
            assert!(st.cats.list.get().is_empty(), "全局域没有系统词库");
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
            crate::rpc::take_mock_calls();
            assert!(st.set_sys_cat_enabled("cell_idiom", true));
            assert_eq!(
                calls_of("schema.setDictEnabled"),
                vec![
                    json!({"id": "py", "dictId": "cell_idiom", "enabled": true}),
                    json!({"id": "sp", "dictId": "cell_idiom", "enabled": true}),
                ]
            );
            let idiom = st
                .cats
                .list
                .get()
                .into_iter()
                .find(|c| c.id == "cell_idiom")
                .unwrap();
            assert!(idiom.enabled, "开关后重取，清单跟上");
            assert_eq!(cat_rows(&st.cats.list.get())[2][0], "开", "一级表跟上");
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

    /// 别处改了当前域成员的开关（方案页、命令直通车）：重取清单；别的方案不理。
    #[test]
    fn config_changed_of_a_member_refreshes() {
        with_mock(|| {
            let st = pinyin_state("sp");
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
            assert_eq!(calls_of("sysDict.categories").len(), 1);
            assert!(
                st.cats
                    .list
                    .get()
                    .iter()
                    .any(|c| c.id == "cell_idiom" && c.enabled)
            );
            assert!(
                st.on_schema_config_changed(None, Some("backup.restore")),
                "不带 id 按全部处理"
            );
        });
    }

    // ── 二级 ────────────────────────────────────────────────

    #[test]
    fn entering_fetches_header_and_first_page_and_back_returns() {
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
            assert_eq!(st.cats.viewing.get().as_deref(), Some("pinyin_main"));
            assert_eq!(st.sys_view_title(), "rime_frost");
            assert!(st.cats.has_tables());
            let rows = st.cats.entry_rows.get();
            assert_eq!(rows.len(), 100);
            assert_eq!(
                rows[0],
                ["ma0", "词0", "1000", "cn_dicts/8105"],
                "有子表多一列"
            );
            assert_eq!(st.cats.page_text.get(), "第 1-100 / 共 250 条");

            st.leave_sys_dict();
            assert!(st.cats.viewing.get().is_none());
            assert!(st.cats.open.get(), "返回只回一级，右栏仍是系统词库");

            st.enter_sys_dict("cell_idiom");
            assert!(!st.cats.has_tables());
            assert_eq!(st.cats.entry_rows.get()[0].len(), 3, "没有子表不出那一列");
            crate::rpc::take_mock_calls();
            st.enter_sys_dict("nope");
            assert!(calls_of("sysDict.header").is_empty(), "不在清单里的库不进");
        });
    }

    #[test]
    fn paging_and_search_send_the_right_params() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            st.enter_sys_dict("cell_idiom");
            assert!(!st.sys_has_prev() && st.sys_has_next());
            crate::rpc::take_mock_calls();
            st.sys_goto(u64::MAX);
            assert_eq!(
                calls_of("sysDict.entries"),
                vec![json!({"schema": "py", "dictId": "cell_idiom", "offset": 200, "limit": 100})],
                "末页夹到最后一页的起点"
            );
            assert_eq!(st.cats.page_text.get(), "第 201-250 / 共 250 条");
            assert!(st.sys_has_prev() && !st.sys_has_next());

            // 打字不发请求；回车 / 点「搜索」才搜：回首页，带上 query 与方式
            st.cats.query.set(" ma1 ".into());
            assert!(calls_of("sysDict.entries").is_empty(), "每键不请求");
            st.submit_sys_search();
            assert_eq!(
                calls_of("sysDict.entries"),
                vec![
                    json!({"schema": "py", "dictId": "cell_idiom", "offset": 0, "limit": 100,
                            "query": "ma1", "mode": "code"})
                ]
            );
            assert_eq!(st.cats.total.get(), 111, "ma1 / ma10-19 / ma100-199");
            st.cats.query.set("词24".into());
            st.cats.mode.set(1);
            st.on_sys_mode_changed();
            let p = calls_of("sysDict.entries");
            assert_eq!(p.len(), 1, "已在搜索时切方式：重搜一次");
            assert_eq!(
                (&p[0]["mode"], &p[0]["query"]),
                (&json!("text"), &json!("词24"))
            );
            assert_eq!(st.cats.total.get(), 11);
            st.cats.query.set("无此".into());
            st.submit_sys_search();
            assert_eq!(st.cats.page_text.get(), "没有匹配的词条");

            // ×：清框、回到浏览
            crate::rpc::take_mock_calls();
            st.clear_sys_search();
            assert_eq!(st.cats.query.get(), "");
            assert!(!st.sys_searching());
            let p = calls_of("sysDict.entries");
            assert_eq!(p.len(), 1);
            assert!(p[0].get("query").is_none(), "回到浏览");
            assert_eq!(st.cats.total.get(), 250);
            st.clear_sys_search();
            assert!(
                calls_of("sysDict.entries").is_empty(),
                "没在搜索时 × 不再请求"
            );
            // 框里只有空白：等于回到浏览
            st.cats.query.set("  ".into());
            st.submit_sys_search();
            assert!(!st.sys_searching());
            st.cats.mode.set(0);
            crate::rpc::take_mock_calls();
            st.on_sys_mode_changed();
            assert!(
                calls_of("sysDict.entries").is_empty(),
                "没在搜索时切方式不请求"
            );

            // 换一本库：搜索清空、回编码方式
            st.enter_sys_dict("cell_place");
            assert_eq!((st.cats.query.get().as_str(), st.cats.mode.get()), ("", 0));
        });
    }

    #[test]
    fn add_to_user_prefills_the_add_dialog_and_keeps_the_view() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            st.enter_sys_dict("cell_idiom");
            st.add_sys_entry_to_user(3);
            assert!(st.edit_visible.get(), "打开加词对话框");
            assert_eq!(
                (st.edit_code.get().as_str(), st.edit_text.get().as_str()),
                ("ma3", "词3")
            );
            assert_eq!(
                st.current_category(),
                super::super::spec::WordCategory::UserDict,
                "落到本域用户词库"
            );
            assert!(st.selected_lib().is_none(), "主词库，不是分库");
            assert!(
                st.cats.open.get() && st.cats.viewing.get().is_some(),
                "右栏不动"
            );
            let (save, _) = st.save_calls().unwrap();
            assert_eq!(save[0].0, "dict.add");
            assert_eq!(save[0].1["code"], "ma3");
            assert_eq!(save[0].1["text"], "词3");

            let menu = sys_entry_menu(&st, 0);
            assert_eq!(menu[0].label, "加进用户词库…");
            assert!(sys_entry_menu(&st, 999).is_empty(), "越界不弹");
        });
    }

    /// core 说明整库不加载时，底栏显示这条说明而不是「没有词条」。
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
            assert!(st.cats.page_text.get().contains("columns"));
        });
    }

    /// 停在末页之后（库变短 / 结果变少）：跳到最后一页。
    #[test]
    fn offset_past_the_end_jumps_to_the_last_page() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            st.enter_sys_dict("cell_idiom");
            st.sys_goto(200);
            // 搜索结果只有 111 条，偏移还在 200
            *st.cats.searched.borrow_mut() = Some(("ma1".into(), 0));
            crate::rpc::take_mock_calls();
            st.load_sys_page();
            let offs: Vec<Value> = calls_of("sysDict.entries")
                .into_iter()
                .map(|p| p["offset"].clone())
                .collect();
            assert_eq!(offs, [json!(200), json!(100)]);
            assert_eq!(st.cats.offset.get(), 100);
            assert_eq!(st.cats.page_text.get(), "第 101-111 / 共 111 条");
        });
    }

    fn frame(tree: &mut windui::core::Tree) {
        tree.layout_root(
            windui::geometry::Size::new(800, 600),
            &mut windui::text::NullTextEngine,
        );
    }

    /// 单击某行即选中（工具栏「加进用户词库」据此可用），双击仍走行激活；换页清选中。
    #[test]
    fn click_selects_a_row_and_double_click_still_adds() {
        use windui::event::{MouseButton, PointerEvent, PointerKind};
        use windui::geometry::Point;
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            st.enter_sys_dict("cell_idiom");
            let mut tree = windui::core::Tree::new();
            let root = build_entries_table(&st).build(&mut tree);
            tree.root = Some(root);
            frame(&mut tree);
            frame(&mut tree);
            assert_eq!(st.cats.selected.get(), None);
            // 表头约 38px，首行之下第二行约在 y≈80
            let at = Point::new(300, 82);
            let (mut hover, mut capture) = (None, None);
            for kind in [PointerKind::Down, PointerKind::Up] {
                tree.dispatch_pointer(
                    PointerEvent::single(kind, at, MouseButton::Left),
                    &mut hover,
                    &mut capture,
                );
            }
            let row = st.cats.selected.get().expect("单击应选中一行");
            assert!(row > 0, "点的是第二行起");
            assert!(!st.edit_visible.get(), "单击不加词");
            for kind in [PointerKind::Down, PointerKind::Up] {
                let mut ev = PointerEvent::single(kind, at, MouseButton::Left);
                ev.click_count = 2;
                tree.dispatch_pointer(ev, &mut hover, &mut capture);
            }
            assert!(st.edit_visible.get(), "双击仍冒泡到行，打开加词");
            assert_eq!(st.edit_text.get(), format!("词{row}"));
            st.sys_goto(100);
            assert_eq!(st.cats.selected.get(), None, "换页清选中");
        });
    }

    /// 刷新清单失败：留着旧清单，不把右栏清空。
    #[test]
    fn failed_refresh_keeps_the_old_list() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            let before = st.cats.list.get();
            assert!(!before.is_empty());
            crate::rpc::force_mock_error("sysDict.categories", Some("core 忙"));
            st.refresh_sys_cats();
            crate::rpc::force_mock_error("sysDict.categories", None);
            assert_eq!(st.cats.list.get(), before);
            assert!(st.cats.open.get(), "右栏仍开着");
        });
    }

    /// 展开 / 收起头部卡片只重建卡片，不重建词条表。
    #[test]
    fn card_toggle_does_not_rebuild_the_entries_table() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            st.enter_sys_dict("cell_idiom");
            let mut tree = windui::core::Tree::new();
            let root = build_view_level(&st).build(&mut tree);
            tree.root = Some(root);
            frame(&mut tree);
            let e0 = st.cats.entries_epoch.get();
            let c0 = st.cats.card_epoch.get();
            st.cats.card_open.set(true);
            frame(&mut tree);
            assert_eq!(st.cats.entries_epoch.get(), e0, "词条表不重建");
            assert_ne!(st.cats.card_epoch.get(), c0, "卡片重建");
        });
    }

    /// 二级时收到 configChanged：库还在就重取头部与当前页；库没了回一级。
    #[test]
    fn config_changed_while_viewing() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.open_sys_cats();
            st.enter_sys_dict("cell_idiom");
            st.sys_goto(100);
            crate::rpc::take_mock_calls();
            assert!(st.on_schema_config_changed(Some("py"), Some("schema.saveConfig")));
            let calls = crate::rpc::take_mock_calls();
            assert!(calls.iter().any(|(m, _)| m == "sysDict.header"));
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
}
