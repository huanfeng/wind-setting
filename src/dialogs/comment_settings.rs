//! 「候选注释」对话框：分配 / 注释词库 / 切换 三个 Tab。模板库另成一框，见
//! [`crate::dialogs::comment_templates`]；两框共用本模块的 [`CommentSettingsUi`] 草稿。
//!
//! 设计见 core 仓 `docs/design/candidate-comment-templates.md` §9。
//!
//! # 读写的配置键
//!
//! - `ui.comment.assign.global`（清单宿主）与 `ui.comment.assign.{type,schema,mode}`：总表；
//! - `ui.comment.templates`：用户模板（同 id 覆盖内置）；
//! - `ui.comment_dicts`：注释词库页（逻辑在 [`crate::dialogs::comment_dicts_dialog`]）；
//! - 切换页的几项（状态、Alt、显示）是普通清单项，经 [`crate::manifest::field`] 建。
//!
//! # 格值与写回
//!
//! 每格显示的是**用户值，否则出厂值，否则不写**；选了什么就按 [`write_choice`] 写回：
//! 与出厂值相同即删键（配置里只留差异），「跟随」写 `inherit`（横排不写本就是跟随，
//! 只在有出厂值的格上才要写），「不显示」写 `none`，竖排「同横排」删键。横排为主：竖排
//! 没写 = 同本行横排。实际落到哪个模板由 core 的 `comment.library` 按草稿算（与输入时同一条
//! 解析链），格子据此显示「跟随 · 编码提示」「同横排 · 编码提示」。分配表的界面在
//! [`crate::dialogs::comment_assign`]。

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use serde_json::{Value, json};
use wind_config::comment_lib::{CommentAssign, CommentCells, CommentTemplateDef};
use windui::prelude::*;

use crate::config::Config;
use crate::dialogs::comment_dicts_dialog::CommentDictsUi;
use crate::search::{JumpRequest, JumpTarget};
use crate::state::AppState;

/// 清单宿主：全局那一行的四格。
pub const HOST_KEY: &str = "ui.comment.assign.global";
pub(crate) const TEMPLATES_KEY: &str = "ui.comment.templates";
const TYPE_KEY: &str = "ui.comment.assign.type";
const SCHEMA_KEY: &str = "ui.comment.assign.schema";
const MODE_KEY: &str = "ui.comment.assign.mode";
const DICTS_KEY: &str = "ui.comment_dicts";

/// 切换页里的清单项（顺序即显示顺序）。
const SWITCH_KEYS: [&str; 2] = ["ui.comment.state", "ui.comment.alt_switch"];
const DISPLAY_KEYS: [&str; 3] = [
    "ui.candidate.comment_above",
    "ui.candidate.comment_max_chars_horizontal",
    "ui.candidate.comment_max_chars_vertical",
];

const TABS: [&str; 3] = ["分配", "注释词库", "切换"];
const TAB_ASSIGN: usize = 0;
const TAB_DICTS: usize = 1;
const TAB_SWITCH: usize = 2;

/// 方案类型行（常驻）：键与显示名。
pub const TYPE_ROWS: [(&str, &str); 5] = [
    ("codetable", "码表"),
    ("pinyin", "拼音"),
    ("shuangpin", "双拼"),
    ("mixed", "混输"),
    ("english", "英文"),
];

/// 固定的模式键与显示名；`mix:<id>` / `special:<方案id>` 另按配置列出。
pub const BASE_MODES: [(&str, &str); 5] = [
    ("temp_pinyin", "临时拼音"),
    ("temp_english", "临时英文"),
    ("url", "网址"),
    ("email", "Email"),
    ("unicode", "Unicode"),
];

// ───────────────────────────── 列 ─────────────────────────────

/// 总表的四格列。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Col {
    DefaultH,
    DefaultV,
    DetailH,
    DetailV,
}

impl Col {
    /// 表格列序：横排在前（横排为主）。
    pub const ALL: [Col; 4] = [Col::DefaultH, Col::DefaultV, Col::DetailH, Col::DetailV];

    pub fn horizontal(self) -> bool {
        matches!(self, Col::DefaultH | Col::DetailH)
    }

    pub fn title(self) -> &'static str {
        match self {
            Col::DefaultV => "默认·竖",
            Col::DefaultH => "默认·横",
            Col::DetailV => "详细·竖",
            Col::DetailH => "详细·横",
        }
    }

    fn get(self, c: &CommentCells) -> Option<&str> {
        match self {
            Col::DefaultV => c.default_vertical.as_deref(),
            Col::DefaultH => c.default_horizontal.as_deref(),
            Col::DetailV => c.detail_vertical.as_deref(),
            Col::DetailH => c.detail_horizontal.as_deref(),
        }
    }

    fn set(self, c: &mut CommentCells, v: Option<String>) {
        let slot = match self {
            Col::DefaultV => &mut c.default_vertical,
            Col::DefaultH => &mut c.default_horizontal,
            Col::DetailV => &mut c.detail_vertical,
            Col::DetailH => &mut c.detail_horizontal,
        };
        *slot = v;
    }
}

// ───────────────────────────── 作用范围行 ─────────────────────────────

/// 总表的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Global,
    Type(String),
    Schema(String),
    Mode(String),
}

impl Scope {
    /// 与 `comment.library` 的 `scope` 同形：`global` / `type:<键>` / `schema:<id>` / `mode:<键>`。
    pub fn key(&self) -> String {
        match self {
            Scope::Global => "global".into(),
            Scope::Type(k) => format!("type:{k}"),
            Scope::Schema(k) => format!("schema:{k}"),
            Scope::Mode(k) => format!("mode:{k}"),
        }
    }

    pub fn cells<'a>(&self, a: &'a CommentAssign) -> Option<&'a CommentCells> {
        match self {
            Scope::Global => Some(&a.global),
            Scope::Type(k) => a.by_type.get(k),
            Scope::Schema(k) => a.schema.get(k),
            Scope::Mode(k) => a.mode.get(k),
        }
    }

    fn cells_mut<'a>(&self, a: &'a mut CommentAssign) -> &'a mut CommentCells {
        match self {
            Scope::Global => &mut a.global,
            Scope::Type(k) => a.by_type.entry(k.clone()).or_default(),
            Scope::Schema(k) => a.schema.entry(k.clone()).or_default(),
            Scope::Mode(k) => a.mode.entry(k.clone()).or_default(),
        }
    }

    /// 用户自己加的行（出厂没有的方案 / 模式行）：可以整行删掉；其余行只能恢复出厂。
    pub fn user_added(&self, system: &CommentAssign) -> bool {
        matches!(self, Scope::Schema(_) | Scope::Mode(_)) && self.cells(system).is_none()
    }
}

/// 表格的行：全局、五个类型常驻；方案行、模式行取出厂分配与用户分配里出现过的。
pub fn table_rows(user: &CommentAssign, system: &CommentAssign) -> Vec<Scope> {
    let mut out = vec![Scope::Global];
    out.extend(TYPE_ROWS.iter().map(|(k, _)| Scope::Type(k.to_string())));
    let union = |a: &std::collections::BTreeMap<String, CommentCells>,
                 b: &std::collections::BTreeMap<String, CommentCells>| {
        a.keys().chain(b.keys()).cloned().collect::<BTreeSet<_>>()
    };
    out.extend(
        union(&system.schema, &user.schema)
            .into_iter()
            .map(Scope::Schema),
    );
    out.extend(union(&system.mode, &user.mode).into_iter().map(Scope::Mode));
    out
}

// ───────────────────────────── 格值 ─────────────────────────────

/// 一格的选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Template(String),
    /// 不显示（`none`）。
    Hide,
    /// 跟随下一层同排布那格（横排不写 / `inherit`）。
    Follow,
    /// 竖排同本行横排（不写）。
    SameAsHorizontal,
}

/// 原始格值 → 选项。`None`（不写）横排是跟随、竖排是同横排。
fn choice_of_raw(raw: Option<&str>, col: Col) -> Choice {
    match raw {
        None if col.horizontal() => Choice::Follow,
        None => Choice::SameAsHorizontal,
        Some("none") => Choice::Hide,
        Some("inherit") => Choice::Follow,
        Some(id) => Choice::Template(id.to_string()),
    }
}

/// 一格当前显示的选项：用户值，否则出厂值，否则不写。
pub fn shown_choice(
    user: &CommentAssign,
    system: &CommentAssign,
    scope: &Scope,
    col: Col,
) -> Choice {
    let raw = scope
        .cells(user)
        .and_then(|c| col.get(c))
        .or_else(|| scope.cells(system).and_then(|c| col.get(c)));
    choice_of_raw(raw, col)
}

/// 选中 `choice` 后写回用户分配：与出厂值相同即删键；否则模板写 id、不显示写 `none`、
/// 跟随写 `inherit`（压过出厂值，或让竖排跟随下层竖排）、同横排删键。
pub fn write_choice(
    user: &mut CommentAssign,
    system: &CommentAssign,
    scope: &Scope,
    col: Col,
    choice: &Choice,
) {
    let factory = choice_of_raw(scope.cells(system).and_then(|c| col.get(c)), col);
    let raw = if *choice == factory {
        None
    } else {
        match choice {
            Choice::Template(id) => Some(id.clone()),
            Choice::Hide => Some("none".to_string()),
            Choice::Follow => Some("inherit".to_string()),
            Choice::SameAsHorizontal => None,
        }
    };
    col.set(scope.cells_mut(user), raw);
}

/// 加一行覆盖（各格先不写）。已在表里的不重复加，返回是否新加了。
pub fn add_row(user: &mut CommentAssign, system: &CommentAssign, scope: &Scope) -> bool {
    if !matches!(scope, Scope::Schema(_) | Scope::Mode(_))
        || table_rows(user, system).contains(scope)
    {
        return false;
    }
    scope.cells_mut(user);
    true
}

/// 删掉用户自己加的一行；出厂行删不掉，返回 false。
pub fn delete_row(user: &mut CommentAssign, system: &CommentAssign, scope: &Scope) -> bool {
    if !scope.user_added(system) {
        return false;
    }
    match scope {
        Scope::Schema(k) => user.schema.remove(k).is_some(),
        Scope::Mode(k) => user.mode.remove(k).is_some(),
        _ => false,
    }
}

/// 恢复出厂：清掉这一行的用户格（行本身留着）。
pub fn restore_factory(user: &mut CommentAssign, scope: &Scope) {
    match scope {
        Scope::Global => user.global = CommentCells::default(),
        Scope::Type(k) => {
            user.by_type.remove(k);
        }
        Scope::Schema(k) => {
            user.schema.remove(k);
        }
        Scope::Mode(k) => {
            user.mode.remove(k);
        }
    }
}

/// 这一行有没有用户格（「恢复出厂」可不可点）。
pub fn has_user_cells(user: &CommentAssign, scope: &Scope) -> bool {
    scope
        .cells(user)
        .is_some_and(|c| Col::ALL.iter().any(|col| col.get(c).is_some()))
}

/// 删掉用户模板后，引用它的用户格一并删掉（否则 core 只是告警后按不写处理，界面上却显示
/// 一个不存在的模板）。
pub(super) fn drop_references(user: &mut CommentAssign, id: &str) {
    let clean = |c: &mut CommentCells| {
        for col in Col::ALL {
            if col.get(c) == Some(id) {
                col.set(c, None);
            }
        }
    };
    clean(&mut user.global);
    for m in [&mut user.by_type, &mut user.schema, &mut user.mode] {
        m.values_mut().for_each(clean);
    }
}

// ───────────────────────────── 模板库（RPC） ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Origin {
    #[default]
    Builtin,
    User,
    UserOverridesBuiltin,
}

/// 合并后的一个模板（`comment.library` 的 `templates`）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TplInfo {
    pub id: String,
    pub name: String,
    pub text: String,
    pub origin: Origin,
}

impl TplInfo {
    pub(super) fn editable(&self) -> bool {
        self.origin != Origin::Builtin
    }

    pub(super) fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

/// `comment.library` 回包。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LibraryInfo {
    pub templates: Vec<TplInfo>,
    pub system_assign: CommentAssign,
    /// `(scope 键, 列)` → `(模板 id 或 none, 所在层)`。
    pub effective: HashMap<(String, Col), (String, String)>,
}

impl LibraryInfo {
    pub(super) fn name_of(&self, id: &str) -> String {
        if id == "none" {
            return "不显示".into();
        }
        self.templates
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.display_name().to_string())
            .unwrap_or_else(|| id.to_string())
    }

    pub(super) fn text_of(&self, id: &str) -> String {
        self.templates
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.text.clone())
            .unwrap_or_default()
    }

    pub(super) fn effective_of(&self, scope: &Scope, col: Col) -> Option<&(String, String)> {
        self.effective.get(&(scope.key(), col))
    }
}

pub fn library_from_value(v: &Value) -> LibraryInfo {
    let s = |o: &Value, k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let templates = v
        .get("templates")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|t| t.get("id").and_then(Value::as_str).is_some())
                .map(|t| TplInfo {
                    id: s(t, "id"),
                    name: s(t, "name"),
                    text: s(t, "text"),
                    origin: match t.get("origin").and_then(Value::as_str) {
                        Some("user") => Origin::User,
                        Some("user_overrides_builtin") => Origin::UserOverridesBuiltin,
                        _ => Origin::Builtin,
                    },
                })
                .collect()
        })
        .unwrap_or_default();
    let system_assign = v
        .get("system_assign")
        .and_then(|a| serde_json::from_value(a.clone()).ok())
        .unwrap_or_default();
    let mut effective = HashMap::new();
    for e in v
        .get("effective")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let col = match (
            e.get("state").and_then(Value::as_str),
            e.get("vertical").and_then(Value::as_bool),
        ) {
            (Some("default"), Some(true)) => Col::DefaultV,
            (Some("default"), Some(false)) => Col::DefaultH,
            (Some("detail"), Some(true)) => Col::DetailV,
            (Some("detail"), Some(false)) => Col::DetailH,
            _ => continue,
        };
        effective.insert(
            (s(e, "scope"), col),
            (s(e, "template_id"), s(e, "from_scope")),
        );
    }
    LibraryInfo {
        templates,
        system_assign,
        effective,
    }
}

/// 草稿 → `comment.library` 的 `comment` 参数（`[ui.comment]` 里与模板、分配有关的两段）。
pub fn draft_json(templates: &[CommentTemplateDef], assign: &CommentAssign) -> Value {
    json!({ "templates": templates, "assign": assign })
}

/// 新模板 id：`base` 本身没被占用就用它，否则加 `_2`、`_3`…（id 只含 `[a-z0-9_]`）。
pub fn unique_template_id(base: &str, taken: &[&str]) -> String {
    let base: String = base
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
        .collect();
    let base = if base.is_empty() {
        "custom".into()
    } else {
        base
    };
    if !taken.contains(&base.as_str()) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}_{n}"))
        .find(|id| !taken.contains(&id.as_str()))
        .expect("序号无穷")
}

// ───────────────────────────── 下拉标签 ─────────────────────────────

/// 一格的选项列表：模板（库序）、不显示、跟随；竖排另有同横排。当前值是库里没有的模板时
/// 追加在末尾，免得一打开就被改成别的值。
///
/// - 全局行不给「跟随」：它下面没有层了。
/// - 出厂写了竖排值的格不给「同横排」：同横排即删用户键，删了落回的是出厂竖排值而不是横排。
pub(super) fn cell_options(
    lib: &LibraryInfo,
    scope: &Scope,
    col: Col,
    current: &Choice,
) -> Vec<Choice> {
    let mut out: Vec<Choice> = lib
        .templates
        .iter()
        .map(|t| Choice::Template(t.id.clone()))
        .collect();
    out.push(Choice::Hide);
    if *scope != Scope::Global {
        out.push(Choice::Follow);
    }
    let factory_v = scope
        .cells(&lib.system_assign)
        .and_then(|c| col.get(c))
        .is_some();
    if !col.horizontal() && !factory_v {
        out.push(Choice::SameAsHorizontal);
    }
    if !out.contains(current) {
        out.push(current.clone());
    }
    out
}

/// 选项文案。跟随、同横排后缀实际生效的模板名（`resolved`）。
pub(super) fn choice_label(lib: &LibraryInfo, c: &Choice, resolved: Option<&str>) -> String {
    match c {
        Choice::Template(id) if lib.templates.iter().any(|t| &t.id == id) => lib.name_of(id),
        Choice::Template(id) => format!("{id}（不存在）"),
        Choice::Hide => "不显示".into(),
        Choice::Follow => match resolved {
            Some(r) => format!("跟随 · {r}"),
            None => "跟随".into(),
        },
        Choice::SameAsHorizontal => match resolved {
            Some(r) => format!("同横排 · {r}"),
            None => "同横排".into(),
        },
    }
}

pub(super) fn scope_label(scope: &Scope, names: &Names) -> String {
    match scope {
        Scope::Global => "全局".into(),
        Scope::Type(k) => TYPE_ROWS
            .iter()
            .find(|(t, _)| t == k)
            .map(|(_, l)| l.to_string())
            .unwrap_or_else(|| k.clone()),
        Scope::Schema(id) => names.schema(id),
        Scope::Mode(k) => names.mode(k),
    }
}

/// 方案与模式的显示名（来自 `schema.list` 与配置里的 `schema.mix_modes`）。
#[derive(Debug, Clone, Default)]
pub(super) struct Names {
    /// `(id, 显示名, 是否 overlay 方案)`。
    pub(super) schemas: Vec<(String, String, bool)>,
    /// `(mix id, 显示名)`。
    pub(super) mixes: Vec<(String, String)>,
}

impl Names {
    fn load(cfg: &Config) -> Self {
        let list = crate::rpc::call_core("schema.list", &json!({ "includeHidden": true }))
            .unwrap_or(Value::Array(vec![]));
        let schemas = list
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| {
                let id = s.get("id")?.as_str()?.to_string();
                let name = s
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| !n.is_empty())
                    .unwrap_or(&id)
                    .to_string();
                let overlay = s.get("overlay").and_then(Value::as_bool).unwrap_or(false);
                Some((id, name, overlay))
            })
            .collect();
        let mixes = cfg
            .get_array("schema.mix_modes")
            .into_iter()
            .flatten()
            .filter_map(|m| {
                let id = m.get("id")?.as_str().filter(|s| !s.is_empty())?.to_string();
                let name = m
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&id)
                    .to_string();
                Some((id, name))
            })
            .collect();
        Self { schemas, mixes }
    }

    fn schema_name(&self, id: &str) -> String {
        self.schemas
            .iter()
            .find(|(s, _, _)| s == id)
            .map(|(_, n, _)| n.clone())
            .unwrap_or_else(|| id.to_string())
    }

    fn schema(&self, id: &str) -> String {
        format!("方案 · {}", self.schema_name(id))
    }

    fn mode(&self, key: &str) -> String {
        let name = if let Some((_, l)) = BASE_MODES.iter().find(|(k, _)| *k == key) {
            l.to_string()
        } else if let Some(id) = key.strip_prefix("mix:") {
            let n = self
                .mixes
                .iter()
                .find(|(m, _)| m == id)
                .map(|(_, n)| n.clone())
                .unwrap_or_else(|| id.to_string());
            format!("快捷输入 {n}")
        } else if let Some(id) = key.strip_prefix("special:") {
            format!("特殊模式 {}", self.schema_name(id))
        } else {
            key.to_string()
        };
        format!("模式 · {name}")
    }

    /// 可添加的方案行：`(行, 显示名)`，已在表里的不列；`schema.list` 里重复的 id 只列一次。
    pub(super) fn addable_schemas(&self, rows: &[Scope]) -> Vec<(Scope, String)> {
        let mut out: Vec<(Scope, String)> = Vec::new();
        for (id, n, _) in &self.schemas {
            let s = Scope::Schema(id.clone());
            if !rows.contains(&s) && out.iter().all(|(o, _)| *o != s) {
                out.push((s, n.clone()));
            }
        }
        out
    }

    /// 可添加的模式行：固定模式、各快捷输入、各 overlay 方案；已在表里的不列，重复的只列一次。
    pub(super) fn addable_modes(&self, rows: &[Scope]) -> Vec<(Scope, String)> {
        let mut keys: Vec<String> = BASE_MODES.iter().map(|(k, _)| k.to_string()).collect();
        keys.extend(self.mixes.iter().map(|(id, _)| format!("mix:{id}")));
        keys.extend(
            self.schemas
                .iter()
                .filter(|(_, _, ov)| *ov)
                .map(|(id, _, _)| format!("special:{id}")),
        );
        let mut out: Vec<(Scope, String)> = Vec::new();
        for k in keys {
            let s = Scope::Mode(k.clone());
            if rows.contains(&s) || out.iter().any(|(o, _)| *o == s) {
                continue;
            }
            let label = self.mode(&k);
            out.push((s, label.trim_start_matches("模式 · ").to_string()));
        }
        out
    }
}

// ───────────────────────────── 对话框状态 ─────────────────────────────

/// 对话框的全部可变状态。
#[derive(Clone)]
pub struct CommentSettingsUi {
    pub visible: Signal<bool>,
    pub tab: Signal<usize>,
    /// 用户模板草稿。
    pub templates: Signal<Vec<CommentTemplateDef>>,
    /// 用户分配草稿。
    pub assign: Signal<CommentAssign>,
    /// 按草稿算出的模板库与每格结果；单元素 Vec 供 `host_signal` 整块重建。
    pub(super) view: Signal<Vec<LibraryInfo>>,
    /// 模板库对话框的选中与编辑框。
    pub(super) tpl: crate::dialogs::comment_templates::TplEditor,
    /// 分配表「添加覆盖」的选择框。
    pub(super) pick: crate::dialogs::comment_assign::AddPicker,
    pub(super) names: Rc<RefCell<Names>>,
    /// 出厂分配：写回时判「与出厂相同」的基准。首次拉到就定下，之后的刷新失败也不会把它
    /// 换成空表（空表会让出厂格上的「跟随」变成删键、静默回到出厂值）。
    system: Rc<RefCell<Option<CommentAssign>>>,
}

impl CommentSettingsUi {
    pub(super) fn new() -> Self {
        Self {
            visible: signal(false),
            tab: signal(TAB_ASSIGN),
            templates: signal(Vec::new()),
            assign: signal(CommentAssign::default()),
            view: signal(vec![LibraryInfo::default()]),
            tpl: crate::dialogs::comment_templates::TplEditor::new(),
            pick: crate::dialogs::comment_assign::AddPicker::new(),
            names: Rc::new(RefCell::new(Names::default())),
            system: Rc::new(RefCell::new(None)),
        }
    }

    pub(super) fn lib(&self) -> LibraryInfo {
        self.view.with(|v| v.first().cloned().unwrap_or_default())
    }

    /// 从配置播种草稿（构建 / 重载配置时）。
    pub(super) fn seed(&self, cfg: &Config) {
        let get = |k: &str| cfg.get_path(k).cloned().unwrap_or(Value::Null);
        let templates: Vec<CommentTemplateDef> =
            serde_json::from_value(get(TEMPLATES_KEY)).unwrap_or_default();
        let assign = CommentAssign {
            global: serde_json::from_value(get(HOST_KEY)).unwrap_or_default(),
            by_type: serde_json::from_value(get(TYPE_KEY)).unwrap_or_default(),
            schema: serde_json::from_value(get(SCHEMA_KEY)).unwrap_or_default(),
            mode: serde_json::from_value(get(MODE_KEY)).unwrap_or_default(),
        };
        self.templates.set(templates);
        self.assign.set(assign);
        *self.names.borrow_mut() = Names::load(cfg);
        self.refresh();
    }

    pub(super) fn system_assign(&self) -> CommentAssign {
        self.system.borrow().clone().unwrap_or_default()
    }

    /// 按草稿向 core 重算模板库与每格结果。
    pub(super) fn refresh(&self) {
        let draft = draft_json(&self.templates.get(), &self.assign.get());
        self.apply_reply(crate::rpc::call_core(
            "comment.library",
            &json!({ "comment": draft }),
        ));
    }

    /// 拉取失败时保留上一次的结果（表格不塌、出厂基准不变），只记日志。
    pub(super) fn apply_reply(&self, reply: Result<Value, String>) {
        let v = match reply {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("comment.library 拉取失败，保留上次结果: {e}");
                return;
            }
        };
        let lib = library_from_value(&v);
        if self.system.borrow().is_none() {
            *self.system.borrow_mut() = Some(lib.system_assign.clone());
        }
        if self.tpl.sel.get().is_empty()
            && let Some(t) = lib.templates.first()
        {
            self.tpl.sel.set(t.id.clone());
        }
        self.view.set(vec![lib]);
        self.sync_tpl_rows();
    }

    pub(super) fn value_of(&self, key: &str) -> Value {
        let a = self.assign.get();
        let v = match key {
            TEMPLATES_KEY => serde_json::to_value(self.templates.get()),
            HOST_KEY => serde_json::to_value(&a.global),
            TYPE_KEY => serde_json::to_value(&a.by_type),
            SCHEMA_KEY => serde_json::to_value(&a.schema),
            MODE_KEY => serde_json::to_value(&a.mode),
            _ => return Value::Null,
        };
        v.unwrap_or(Value::Null)
    }

    pub(super) fn choose(&self, scope: &Scope, col: Col, choice: &Choice) {
        let system = self.system_assign();
        self.assign
            .update(|a| write_choice(a, &system, scope, col, choice));
        self.refresh();
    }

    pub(super) fn delete(&self, scope: &Scope) {
        let system = self.system_assign();
        let mut a = self.assign.get();
        if delete_row(&mut a, &system, scope) {
            self.assign.set(a);
            self.refresh();
        }
    }

    pub(super) fn restore(&self, scope: &Scope) {
        self.assign.update(|a| restore_factory(a, scope));
        self.refresh();
    }

    pub(super) fn add(&self, scope: &Scope) {
        let system = self.system_assign();
        let mut a = self.assign.get();
        if add_row(&mut a, &system, scope) {
            self.assign.set(a);
            self.refresh();
        }
    }
}

/// 两个对话框（本框与模板库）共用的草稿：第一次取时建好并登记五个键的读写，之后原样返回。
///
/// 共用是硬要求：两框都整键写 `ui.comment.templates`，各持一份就是两个覆盖式写入者，谁后写
/// 谁赢；分配表的选项又来自模板库的草稿。
pub(crate) fn shared_ui(state: &AppState, cfg: &Config) -> CommentSettingsUi {
    if let Some(ui) = state.fields.comment_ui.borrow().as_ref() {
        return ui.clone();
    }
    let ui = CommentSettingsUi::new();
    ui.seed(cfg);
    for key in [TEMPLATES_KEY, HOST_KEY, TYPE_KEY, SCHEMA_KEY, MODE_KEY] {
        let (uw, ug) = (ui.clone(), ui.clone());
        let kw = key.to_string();
        state
            .fields
            .writers
            .borrow_mut()
            .push(Box::new(move |c: &mut Config| {
                c.set_path(&kw, uw.value_of(&kw))
            }));
        let kg = key.to_string();
        state
            .fields
            .getters
            .borrow_mut()
            .insert(key.to_string(), Box::new(move || ug.value_of(&kg)));
    }
    // 五个键共用一份草稿，loader 只挂在宿主上：载入时按整份配置重新播种（loader 拿到的恒是
    // 整份配置），挂五份只会同一次载入播种五遍。
    let ul = ui.clone();
    state
        .fields
        .loaders
        .borrow_mut()
        .insert(HOST_KEY.to_string(), Box::new(move |c: &Config| ul.seed(c)));
    *state.fields.comment_ui.borrow_mut() = Some(ui.clone());
    ui
}

/// 建对话框，返回打开它的信号。清单宿主是 [`HOST_KEY`]。
pub fn build_dialog_button_comment(state: &AppState, cfg: &Config) -> Signal<bool> {
    let ui = shared_ui(state, cfg);
    let dicts = register_comment_dicts(state, cfg);
    let dlg = build_dialog(state, &ui, &dicts);
    state.fields.dialogs.borrow_mut().push(dlg);
    ui.visible
}

/// 注释词库页的读写登记，同原 `dialog_button_comment_dicts` 的四件套。
pub(crate) fn register_comment_dicts(state: &AppState, cfg: &Config) -> CommentDictsUi {
    let ui = CommentDictsUi::new();
    // 先拉来源再 seed：`seed` 要把内置 / 自动项合并进列表，顺序不能反（见 comment_dicts_dialog）。
    ui.refresh_sources();
    ui.seed(
        cfg.get_array(DICTS_KEY)
            .map(Vec::as_slice)
            .unwrap_or_default(),
    );
    let uw = ui.clone();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            c.set_path(DICTS_KEY, uw.to_value())
        }));
    let ul = ui.clone();
    state.fields.loaders.borrow_mut().insert(
        DICTS_KEY.to_string(),
        Box::new(move |c: &Config| {
            ul.refresh_sources();
            ul.seed(
                c.get_array(DICTS_KEY)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            );
        }),
    );
    let ug = ui.clone();
    state
        .fields
        .getters
        .borrow_mut()
        .insert(DICTS_KEY.to_string(), Box::new(move || ug.to_value()));
    ui
}

/// 搜索跳进本对话框时，按目标锚点切到它所在的 Tab（不在可见 Tab 里的行认领不了跳转）。
fn tab_for_anchor(anchor: &str) -> Option<usize> {
    use crate::search::index::in_dialog_anchor;
    if anchor == in_dialog_anchor(HOST_KEY) {
        return Some(TAB_ASSIGN);
    }
    if anchor == in_dialog_anchor(DICTS_KEY) {
        return Some(TAB_DICTS);
    }
    SWITCH_KEYS
        .iter()
        .chain(DISPLAY_KEYS.iter())
        .any(|k| anchor == in_dialog_anchor(k))
        .then_some(TAB_SWITCH)
}

/// 方案设置里「在 候选注释 中配置」的跳转目标：开本对话框、落在分配表上。
///
/// 取「Alt 键」那一项的搜索条目改锚点：宿主与总开关合并成一行，自己不进搜索索引，而
/// 同一个框里的成员条目带着正确的入口行与对话框键。
pub fn assign_jump_target(manifest: &crate::manifest::Manifest) -> Option<JumpTarget> {
    let entry = crate::search::index::build_index(manifest)
        .into_iter()
        .find(|e| e.id == SWITCH_KEYS[1])?;
    match entry.target {
        JumpTarget::RowInDialog {
            tab,
            host_anchor,
            dialog,
            ..
        } => Some(JumpTarget::RowInDialog {
            tab,
            host_anchor,
            dialog,
            anchor: crate::search::index::in_dialog_anchor(HOST_KEY),
        }),
        _ => None,
    }
}

fn build_dialog(state: &AppState, ui: &CommentSettingsUi, dicts: &CommentDictsUi) -> Element {
    use crate::dialogs::comment_dicts_dialog::{
        build_comment_dicts_pane, build_comment_dicts_subdialogs,
    };
    let jump = state.jump;
    let tab = ui.tab;
    let panes: [Element; 3] = [
        crate::search::anchor::wrap(
            crate::dialogs::comment_assign::build_assign_body(ui),
            crate::search::index::in_dialog_anchor(HOST_KEY),
            jump,
        ),
        crate::search::anchor::wrap(
            build_comment_dicts_pane(dicts),
            crate::search::index::in_dialog_anchor(DICTS_KEY),
            jump,
        ),
        build_switch_tab(state),
    ];
    let mut body = Element::col().width_match();
    for (i, pane) in panes.into_iter().enumerate() {
        body = body.child(
            Element::col()
                .width_match()
                .child(pane)
                .visible_when(move || tab.get() == i),
        );
    }
    let jump_tab = crate::pages::dict::reactive::reactive_effect(
        move || jump.version(),
        move || {
            let want = jump
                .get()
                .as_ref()
                .and_then(JumpRequest::pending_anchor)
                .and_then(tab_for_anchor);
            if let Some(i) = want {
                tab.set(i);
            }
        },
    );
    let content = Element::col()
        .width_match()
        .padding_xy(16, 2)
        .child(
            Element::row()
                .width_match()
                .padding_xy(0, 8)
                .child(Element::segmented(TABS.to_vec(), tab).weight(1.0)),
        )
        .child(Element::divider())
        .child(body)
        .child(jump_tab);
    let shell = crate::dialogs::field_dialogs::build_group_dialog_shell_dyn_sized(
        ui.visible,
        signal("候选注释".to_string()),
        content,
        vec![
            Element::link("查看文档")
                .url(crate::dialogs::field_dialogs::COMMENT_DOC_URL)
                .font_size(12.0),
        ],
        680,
        460,
    );
    Element::stack()
        .child(shell)
        .child(build_comment_dicts_subdialogs(dicts))
        .child(crate::dialogs::comment_assign::build_assign_subdialogs(ui))
}

// ───────────────────────────── 切换 ─────────────────────────────

fn build_switch_tab(state: &AppState) -> Element {
    let row = |k: &str| {
        crate::search::anchor::wrap(
            crate::manifest::field(state, k),
            crate::search::index::in_dialog_anchor(k),
            state.jump,
        )
    };
    let mut switch_rows: Vec<Element> = Vec::new();
    // 总开关的清单项在卡片上（与本对话框的入口合并成一行），这里绑同一个信号。
    if let Some(on) = state
        .fields
        .bool_signals
        .borrow()
        .get("ui.comment.enabled")
        .copied()
    {
        switch_rows.push(crate::widgets::setting_row(
            "显示候选注释",
            Some("关掉时两态都不显示，Alt 也不拦"),
            Element::switch(on),
        ));
    }
    switch_rows.extend(SWITCH_KEYS.iter().map(|k| row(k)));
    switch_rows.push(
        Element::label(
            "命令短语：输入 cozs 选「切注释」切换当前状态。\n\
             按键：在「按键」页的引导键或输入中按键里选「注释：…」；\
             「本次候选窗临时切换」只能绑在输入中按键上。",
        )
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(4)
        .padding_xy(2, 6),
    );
    let display_rows: Vec<Element> = DISPLAY_KEYS.iter().map(|k| row(k)).collect();
    Element::col()
        .width_match()
        .spacing(10)
        .padding_xy(0, 8)
        .child(crate::widgets::dialog_section("切换", switch_rows))
        .child(crate::widgets::dialog_section("显示", display_rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(
        dv: Option<&str>,
        dh: Option<&str>,
        tv: Option<&str>,
        th: Option<&str>,
    ) -> CommentCells {
        let o = |s: Option<&str>| s.map(str::to_string);
        CommentCells {
            default_vertical: o(dv),
            default_horizontal: o(dh),
            detail_vertical: o(tv),
            detail_horizontal: o(th),
        }
    }

    /// 出厂（横排为主）：全局两格、码表详细横、临时英文默认横 = none。
    fn system() -> CommentAssign {
        let mut a = CommentAssign {
            global: cells(None, Some("code_hint"), None, Some("detail_full")),
            ..Default::default()
        };
        a.by_type.insert(
            "codetable".into(),
            cells(None, None, None, Some("chaizi_pinyin")),
        );
        a.mode.insert(
            "temp_english".into(),
            cells(None, Some("none"), None, Some("en_meaning")),
        );
        a
    }

    fn mode(k: &str) -> Scope {
        Scope::Mode(k.into())
    }

    #[test]
    fn unset_vertical_is_same_as_horizontal_and_unset_horizontal_follows() {
        let sys = system();
        let user = CommentAssign::default();
        let t = Scope::Type("pinyin".into());
        assert_eq!(
            shown_choice(&user, &sys, &t, Col::DefaultV),
            Choice::SameAsHorizontal
        );
        assert_eq!(shown_choice(&user, &sys, &t, Col::DefaultH), Choice::Follow);
    }

    #[test]
    fn same_as_horizontal_removes_vertical_key() {
        let sys = system();
        let mut user = CommentAssign::default();
        user.schema.insert(
            "wubi86".into(),
            cells(Some("chaizi"), Some("pinyin"), None, None),
        );
        let s = Scope::Schema("wubi86".into());
        write_choice(
            &mut user,
            &sys,
            &s,
            Col::DefaultV,
            &Choice::SameAsHorizontal,
        );
        assert_eq!(
            user.schema["wubi86"],
            cells(None, Some("pinyin"), None, None)
        );
        assert_eq!(
            shown_choice(&user, &sys, &s, Col::DefaultV),
            Choice::SameAsHorizontal
        );
    }

    /// 每种选项写回后再读出来是同一个选项（用户行与出厂行各一遍）。
    #[test]
    fn write_choice_round_trips() {
        let sys = system();
        for scope in [
            Scope::Schema("wubi86".into()),
            Scope::Type("codetable".into()),
        ] {
            for col in Col::ALL {
                let mut choices = vec![
                    Choice::Template("pinyin".into()),
                    Choice::Hide,
                    Choice::Follow,
                ];
                if !col.horizontal() {
                    choices.push(Choice::SameAsHorizontal);
                }
                for c in choices {
                    let mut user = CommentAssign::default();
                    write_choice(&mut user, &sys, &scope, col, &c);
                    assert_eq!(
                        shown_choice(&user, &sys, &scope, col),
                        c,
                        "{} {col:?}",
                        scope.key()
                    );
                }
            }
        }
    }

    #[test]
    fn follow_removes_key_on_user_row_and_writes_inherit_on_factory_cell() {
        let sys = system();
        let mut user = CommentAssign::default();
        let s = Scope::Schema("wubi86".into());
        write_choice(
            &mut user,
            &sys,
            &s,
            Col::DetailH,
            &Choice::Template("pinyin".into()),
        );
        write_choice(&mut user, &sys, &s, Col::DetailH, &Choice::Follow);
        assert_eq!(
            user.schema["wubi86"],
            CommentCells::default(),
            "用户行：横排跟随即删键"
        );

        let t = Scope::Type("codetable".into());
        write_choice(&mut user, &sys, &t, Col::DetailH, &Choice::Follow);
        assert_eq!(
            user.by_type["codetable"].detail_horizontal.as_deref(),
            Some("inherit"),
            "出厂格：写 inherit 才压得过出厂值"
        );
        assert_eq!(shown_choice(&user, &sys, &t, Col::DetailH), Choice::Follow);
    }

    #[test]
    fn hide_writes_none_and_factory_value_removes_key() {
        let sys = system();
        let mut user = CommentAssign::default();
        write_choice(
            &mut user,
            &sys,
            &Scope::Global,
            Col::DefaultH,
            &Choice::Hide,
        );
        assert_eq!(user.global.default_horizontal.as_deref(), Some("none"));
        write_choice(
            &mut user,
            &sys,
            &Scope::Global,
            Col::DefaultH,
            &Choice::Template("code_hint".into()),
        );
        assert_eq!(user.global, CommentCells::default(), "选回出厂值即删键");
        // 出厂本就是 none 的格选不显示：不写。
        write_choice(
            &mut user,
            &sys,
            &mode("temp_english"),
            Col::DefaultH,
            &Choice::Hide,
        );
        assert!(user.mode["temp_english"].default_horizontal.is_none());
    }

    #[test]
    fn vertical_follow_on_user_row_writes_inherit() {
        let sys = system();
        let mut user = CommentAssign::default();
        let s = Scope::Schema("wubi86".into());
        write_choice(&mut user, &sys, &s, Col::DetailV, &Choice::Follow);
        assert_eq!(
            user.schema["wubi86"].detail_vertical.as_deref(),
            Some("inherit"),
            "竖排不写是同横排，跟随下层竖排要写 inherit"
        );
    }

    #[test]
    fn rows_are_fixed_plus_assigned_scopes() {
        let mut user = CommentAssign::default();
        user.schema.insert("wubi86".into(), CommentCells::default());
        let rows = table_rows(&user, &system());
        let keys: Vec<String> = rows.iter().map(Scope::key).collect();
        assert_eq!(
            keys,
            [
                "global",
                "type:codetable",
                "type:pinyin",
                "type:shuangpin",
                "type:mixed",
                "type:english",
                "schema:wubi86",
                "mode:temp_english",
            ]
        );
    }

    #[test]
    fn library_parses_and_follow_label_names_the_winner() {
        let lib = library_from_value(&json!({
            "templates": [
                { "id": "code_hint", "name": "编码提示", "text": "${code_hint}", "origin": "builtin" },
                { "id": "mine", "name": "", "text": "x", "origin": "user" }
            ],
            "system_assign": { "global": { "default_vertical": "code_hint" } },
            "effective": [
                { "scope": "type:pinyin", "state": "default", "vertical": true,
                  "template_id": "code_hint", "from_scope": "global" },
                { "scope": "mode:url", "state": "detail", "vertical": false,
                  "template_id": "none", "from_scope": "mode:url" }
            ]
        }));
        assert_eq!(lib.templates[1].origin, Origin::User);
        assert_eq!(lib.templates[1].display_name(), "mine");
        assert_eq!(
            lib.system_assign.global.default_vertical.as_deref(),
            Some("code_hint")
        );
        let s = Scope::Type("pinyin".into());
        let resolved = lib
            .effective_of(&s, Col::DefaultV)
            .map(|(id, _)| lib.name_of(id));
        assert_eq!(
            choice_label(&lib, &Choice::Follow, resolved.as_deref()),
            "跟随 · 编码提示"
        );
        let (id, from) = lib.effective_of(&mode("url"), Col::DetailH).unwrap();
        assert_eq!(
            (lib.name_of(id), from.as_str()),
            ("不显示".into(), "mode:url")
        );
    }

    #[test]
    fn options_keep_an_unknown_current_template() {
        let lib = LibraryInfo::default();
        let cur = Choice::Template("gone".into());
        let opts = cell_options(&lib, &Scope::Type("pinyin".into()), Col::DefaultV, &cur);
        assert_eq!(
            opts,
            [
                Choice::Hide,
                Choice::Follow,
                Choice::SameAsHorizontal,
                cur.clone()
            ]
        );
        assert_eq!(choice_label(&lib, &cur, None), "gone（不存在）");
    }

    #[test]
    fn following_cells_name_the_resolved_template() {
        let lib = LibraryInfo::default();
        assert_eq!(
            choice_label(&lib, &Choice::Follow, Some("编码提示")),
            "跟随 · 编码提示"
        );
        assert_eq!(
            choice_label(&lib, &Choice::SameAsHorizontal, Some("拆字")),
            "同横排 · 拆字"
        );
        assert_eq!(
            choice_label(&lib, &Choice::SameAsHorizontal, None),
            "同横排"
        );
    }

    #[test]
    fn unique_template_id_sanitizes_and_numbers() {
        assert_eq!(unique_template_id("code_hint_copy", &[]), "code_hint_copy");
        assert_eq!(
            unique_template_id("custom", &["custom", "custom_2"]),
            "custom_3"
        );
        assert_eq!(unique_template_id("中文", &[]), "custom");
    }

    #[test]
    fn deleting_a_template_drops_its_cells() {
        let mut user = CommentAssign::default();
        user.global.detail_vertical = Some("mine".into());
        user.mode.insert(
            "url".into(),
            cells(Some("mine"), Some("pinyin"), None, None),
        );
        drop_references(&mut user, "mine");
        assert!(user.global.detail_vertical.is_none());
        assert_eq!(user.mode["url"], cells(None, Some("pinyin"), None, None));
    }

    #[test]
    fn jump_anchor_selects_the_owning_tab() {
        use crate::search::index::in_dialog_anchor;
        assert_eq!(
            tab_for_anchor(&in_dialog_anchor(HOST_KEY)),
            Some(TAB_ASSIGN)
        );
        assert_eq!(
            tab_for_anchor(&in_dialog_anchor(DICTS_KEY)),
            Some(TAB_DICTS)
        );
        assert_eq!(
            tab_for_anchor(&in_dialog_anchor("ui.comment.alt_switch")),
            Some(TAB_SWITCH)
        );
        assert_eq!(tab_for_anchor("ui.candidate.layout"), None);
    }

    #[test]
    fn schema_manager_jump_lands_on_the_assign_table() {
        let m = crate::manifest::Manifest::from_embedded();
        match assign_jump_target(&m) {
            Some(JumpTarget::RowInDialog { dialog, anchor, .. }) => {
                assert_eq!(dialog, HOST_KEY);
                assert_eq!(anchor, crate::search::index::in_dialog_anchor(HOST_KEY));
            }
            other => panic!("应开候选注释对话框：{other:?}"),
        }
    }

    /// 建对话框不 panic；什么都没动时五个键写回的与读进来的相同，改一格只动对应的键。
    #[test]
    fn dialog_builds_and_writes_back_only_what_changed() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut data = crate::state::LoadedState::fetch();
        data.config_value["ui"]["comment"]["templates"] =
            json!([{ "id": "mine", "name": "我的", "text": "${pinyin}\n${chaizi}" }]);
        data.config_value["ui"]["comment"]["assign"]["schema"] =
            json!({ "wubi86": { "detail_vertical": "mine" } });
        let mut st = AppState::empty();
        st.apply_loaded(data);
        let cfg = st.current_config.clone();
        let visible = build_dialog_button_comment(&st, &cfg);
        assert!(!visible.get());
        let getters = st.fields.getters.borrow();
        for key in [TEMPLATES_KEY, HOST_KEY, TYPE_KEY, SCHEMA_KEY, MODE_KEY] {
            assert_eq!(
                Some(getters[key]()),
                cfg.get_path(key).cloned(),
                "{key} 没动却写出了不同的值"
            );
        }
        drop(getters);
        let diff = st.debug_diff_after_writers();
        let keys: Vec<&str> = diff.iter().map(|i| i.key.as_str()).collect();
        assert!(keys.is_empty(), "{keys:?}");

        // 改一格：只有方案那一层的键变。
        let lib = library_from_value(&crate::rpc::mock_reply("comment.library", &json!({})));
        let mut user: CommentAssign = CommentAssign {
            schema: serde_json::from_value(cfg.get_path(SCHEMA_KEY).cloned().unwrap()).unwrap(),
            ..Default::default()
        };
        write_choice(
            &mut user,
            &lib.system_assign,
            &Scope::Schema("wubi86".into()),
            Col::DefaultH,
            &Choice::Hide,
        );
        assert_eq!(
            serde_json::to_value(&user.schema).unwrap(),
            json!({ "wubi86": { "default_horizontal": "none", "detail_vertical": "mine" } })
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 全局行不给跟随；出厂写了竖排值的格不给同横排；横排格从不给同横排。
    #[test]
    fn options_drop_follow_on_global_and_same_as_horizontal_over_factory_vertical() {
        let mut lib = LibraryInfo::default();
        lib.system_assign
            .by_type
            .insert("codetable".into(), cells(Some("pinyin"), None, None, None));
        let g = cell_options(&lib, &Scope::Global, Col::DefaultH, &Choice::Hide);
        assert!(!g.contains(&Choice::Follow));
        assert!(!g.contains(&Choice::SameAsHorizontal));
        let t = Scope::Type("codetable".into());
        let v = cell_options(&lib, &t, Col::DefaultV, &Choice::Template("pinyin".into()));
        assert!(!v.contains(&Choice::SameAsHorizontal));
        assert!(v.contains(&Choice::Follow));
        let v2 = cell_options(&lib, &t, Col::DetailV, &Choice::SameAsHorizontal);
        assert!(
            v2.contains(&Choice::SameAsHorizontal),
            "出厂没写竖排的格照给"
        );
    }

    /// mock 的出厂分配与每格结果按横排为主：出厂写横排格，竖排同横排。core 的出厂文件翻成
    /// 横排以后这条会红，提醒去掉 mock 里的横竖对调。
    #[test]
    fn mock_library_is_horizontal_primary() {
        let lib = library_from_value(&crate::rpc::mock_reply("comment.library", &json!({})));
        let g = &lib.system_assign.global;
        assert_eq!(g.default_horizontal.as_deref(), Some("code_hint"));
        assert_eq!(g.detail_horizontal.as_deref(), Some("detail_full"));
        assert!(g.default_vertical.is_none() && g.detail_vertical.is_none());
        assert_eq!(
            lib.system_assign.by_type["pinyin"]
                .detail_horizontal
                .as_deref(),
            Some("pinyin_english")
        );
        let t = Scope::Type("codetable".into());
        let (id, from) = lib.effective_of(&t, Col::DetailV).unwrap();
        assert_eq!(
            (id.as_str(), from.as_str()),
            ("chaizi_pinyin", "type:codetable")
        );

        // 用户只写某方案的默认·横：同行默认·竖跟着它。
        let assign = json!({ "schema": { "wubi98": { "default_horizontal": "pinyin" } } });
        let lib = library_from_value(&crate::rpc::mock_reply(
            "comment.library",
            &json!({ "comment": { "assign": assign } }),
        ));
        let s = Scope::Schema("wubi98".into());
        assert_eq!(lib.effective_of(&s, Col::DefaultV).unwrap().0, "pinyin");
    }

    /// 两个对话框共用一份草稿：后建的那个不再登记第二套写回。
    #[test]
    fn both_dialogs_share_one_draft_and_one_templates_writer() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let data = crate::state::LoadedState::fetch();
        let mut st = AppState::empty();
        st.apply_loaded(data);
        let cfg = st.current_config.clone();
        build_dialog_button_comment(&st, &cfg);
        let writers = st.fields.writers.borrow().len();
        crate::dialogs::comment_templates::build_dialog_button_templates(&st, &cfg);
        assert_eq!(
            st.fields.writers.borrow().len(),
            writers,
            "模板库对话框不另登记写回"
        );
        let a = shared_ui(&st, &cfg);
        let tpl = vec![CommentTemplateDef {
            id: "mine".into(),
            name: "我的".into(),
            text: "x".into(),
        }];
        a.templates.set(tpl.clone());
        let b = st.fields.comment_ui.borrow().clone().unwrap();
        assert_eq!(b.templates.get(), tpl, "两框读到同一份草稿");
        assert_eq!(
            st.fields.getters.borrow()[TEMPLATES_KEY](),
            serde_json::to_value(&tpl).unwrap()
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 刷新失败：保留上次结果；出厂基准是播种时定下的，出厂格上选跟随仍写 inherit。
    #[test]
    fn failed_refresh_keeps_view_and_factory_baseline() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let ui = CommentSettingsUi::new();
        ui.seed(&Config::from_value(json!({})));
        let before = ui.lib();
        assert!(!before.templates.is_empty());
        ui.apply_reply(Err("core 未连接".into()));
        assert_eq!(ui.lib(), before, "失败不覆盖上次结果");

        // 即便库视图被清空，写回仍以播种时的出厂分配为基准。
        ui.view.set(vec![LibraryInfo::default()]);
        let t = Scope::Type("codetable".into());
        ui.choose(&t, Col::DetailH, &Choice::Follow);
        assert_eq!(
            ui.assign.get().by_type["codetable"]
                .detail_horizontal
                .as_deref(),
            Some("inherit")
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }
}
