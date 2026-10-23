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
//! 与出厂值相同即删键（配置里只留差异），「跟随」在有出厂值的格上写 `inherit`，
//! 「不显示」写 `none`，横排「同竖排」删键。实际落到哪个模板由 core 的 `comment.library`
//! 按草稿算（与输入时同一条解析链），「跟随」格据此显示「跟随 · 编码提示」。

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
    DefaultV,
    DefaultH,
    DetailV,
    DetailH,
}

impl Col {
    pub const ALL: [Col; 4] = [Col::DefaultV, Col::DefaultH, Col::DetailV, Col::DetailH];

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

    /// 方案行、模式行可以移除；全局与类型行常驻。
    fn removable(&self) -> bool {
        matches!(self, Scope::Schema(_) | Scope::Mode(_))
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
    /// 跟随下一层（竖排不写 / `inherit`）。
    Follow,
    /// 横排同竖排（不写）。
    SameAsVertical,
}

/// 原始格值 → 选项。`None`（不写）竖排是跟随、横排是同竖排。
fn choice_of_raw(raw: Option<&str>, col: Col) -> Choice {
    match raw {
        None if col.horizontal() => Choice::SameAsVertical,
        None => Choice::Follow,
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
/// 跟随写 `inherit`（压过出厂值，或让横排跟随下层横排）、同竖排删键。
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
            Choice::SameAsVertical => None,
        }
    };
    col.set(scope.cells_mut(user), raw);
}

/// 移除一行：用户自己加的行整行删掉；出厂行删不掉，只能把各格设成跟随（有出厂值的格写
/// `inherit`）。返回这一行是否还在表里。
pub fn remove_row(user: &mut CommentAssign, system: &CommentAssign, scope: &Scope) -> bool {
    if scope.cells(system).is_none() {
        match scope {
            Scope::Schema(k) => {
                user.schema.remove(k);
            }
            Scope::Mode(k) => {
                user.mode.remove(k);
            }
            _ => {}
        }
        return false;
    }
    // 「像这一行不存在」：竖排跟随；横排不写即同竖排（竖排跟随了它也就跟随），出厂写了横排
    // 的才要 `inherit` 压掉。
    let factory = scope.cells(system);
    for col in Col::ALL {
        let choice = if col.horizontal() && factory.and_then(|c| col.get(c)).is_none() {
            Choice::SameAsVertical
        } else {
            Choice::Follow
        };
        write_choice(user, system, scope, col, &choice);
    }
    true
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
    fn name_of(&self, id: &str) -> String {
        if id == "none" {
            return "不显示".into();
        }
        self.templates
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.display_name().to_string())
            .unwrap_or_else(|| id.to_string())
    }

    fn text_of(&self, id: &str) -> String {
        self.templates
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.text.clone())
            .unwrap_or_default()
    }

    fn effective_of(&self, scope: &Scope, col: Col) -> Option<&(String, String)> {
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

/// 一格的选项列表：模板（库序）、不显示、跟随；横排另有同竖排。当前值是库里没有的模板时
/// 追加在末尾，免得一打开就被改成别的值。
///
/// - 全局行不给「跟随」：它下面没有层了。
/// - 出厂写了横排值的格不给「同竖排」：同竖排即删用户键，删了落回的是出厂横排值而不是竖排。
fn cell_options(lib: &LibraryInfo, scope: &Scope, col: Col, current: &Choice) -> Vec<Choice> {
    let mut out: Vec<Choice> = lib
        .templates
        .iter()
        .map(|t| Choice::Template(t.id.clone()))
        .collect();
    out.push(Choice::Hide);
    if *scope != Scope::Global {
        out.push(Choice::Follow);
    }
    let factory_h = scope
        .cells(&lib.system_assign)
        .and_then(|c| col.get(c))
        .is_some();
    if col.horizontal() && !factory_h {
        out.push(Choice::SameAsVertical);
    }
    if !out.contains(current) {
        out.push(current.clone());
    }
    out
}

/// 选项文案。跟随后缀实际生效的模板名（`resolved`）。
fn choice_label(lib: &LibraryInfo, c: &Choice, resolved: Option<&str>) -> String {
    match c {
        Choice::Template(id) if lib.templates.iter().any(|t| &t.id == id) => lib.name_of(id),
        Choice::Template(id) => format!("{id}（不存在）"),
        Choice::Hide => "不显示".into(),
        Choice::Follow => match resolved {
            Some(r) => format!("跟随 · {r}"),
            None => "跟随".into(),
        },
        // 同竖排不带后缀：落点就是左边那一格，格子窄，省下的宽度留给模板名。
        Choice::SameAsVertical => "同竖排".into(),
    }
}

fn scope_label(scope: &Scope, names: &Names) -> String {
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
struct Names {
    /// `(id, 显示名, 是否 overlay 方案)`。
    schemas: Vec<(String, String, bool)>,
    /// `(mix id, 显示名)`。
    mixes: Vec<(String, String)>,
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

    /// 可添加的方案行：`(id, 显示名)`，已在表里的不列。
    fn addable_schemas(&self, rows: &[Scope]) -> Vec<(String, String)> {
        self.schemas
            .iter()
            .filter(|(id, _, _)| !rows.contains(&Scope::Schema(id.clone())))
            .map(|(id, n, _)| (id.clone(), n.clone()))
            .collect()
    }

    /// 可添加的模式行：固定模式、各快捷输入、各 overlay 方案。
    fn addable_modes(&self, rows: &[Scope]) -> Vec<(String, String)> {
        let mut keys: Vec<String> = BASE_MODES.iter().map(|(k, _)| k.to_string()).collect();
        keys.extend(self.mixes.iter().map(|(id, _)| format!("mix:{id}")));
        keys.extend(
            self.schemas
                .iter()
                .filter(|(_, _, ov)| *ov)
                .map(|(id, _, _)| format!("special:{id}")),
        );
        keys.into_iter()
            .filter(|k| !rows.contains(&Scope::Mode(k.clone())))
            .map(|k| {
                let label = self.mode(&k);
                let label = label.trim_start_matches("模式 · ").to_string();
                (k, label)
            })
            .collect()
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
    /// 预览的格（表格里最近改过的那一格）。
    preview_cell: Signal<(Scope, Col)>,
    preview_text: Signal<String>,
    /// 模板库对话框的选中与编辑框。
    pub(super) tpl: crate::dialogs::comment_templates::TplEditor,
    names: Rc<RefCell<Names>>,
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
            preview_cell: signal((Scope::Global, Col::DefaultV)),
            preview_text: signal(String::new()),
            tpl: crate::dialogs::comment_templates::TplEditor::new(),
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

    fn system_assign(&self) -> CommentAssign {
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
        self.update_preview();
    }

    /// 预览框跟着「最近改过的格」实际落到的模板走。
    pub(super) fn update_preview(&self) {
        use crate::dialogs::tooltip_sections_dialog::escape_template;
        let lib = self.lib();
        let (scope, col) = self.preview_cell.get();
        let text = lib
            .effective_of(&scope, col)
            .map(|(id, _)| lib.text_of(id))
            .unwrap_or_default();
        let shown = escape_template(&text);
        if self.preview_text.get() != shown {
            self.preview_text.set(shown);
        }
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

    fn choose(&self, scope: &Scope, col: Col, choice: &Choice) {
        let system = self.system_assign();
        self.assign
            .update(|a| write_choice(a, &system, scope, col, choice));
        self.preview_cell.set((scope.clone(), col));
        self.refresh();
    }

    fn remove(&self, scope: &Scope) {
        let system = self.system_assign();
        self.assign.update(|a| {
            remove_row(a, &system, scope);
        });
        self.refresh();
    }

    fn add_row(&self, scope: Scope) {
        self.assign.update(|a| {
            scope.cells_mut(a);
        });
        self.refresh();
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
            build_assign_tab(ui),
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

// ───────────────────────────── 分配 ─────────────────────────────

fn build_assign_tab(ui: &CommentSettingsUi) -> Element {
    let u = ui.clone();
    let table = Element::host_signal(ui.view, move |lib: LibraryInfo| build_table(&u, &lib));
    let preview = {
        use crate::dialogs::template_assist::{PreviewSource, TemplateField, preview};
        let cell = ui.preview_cell;
        let names = ui.names.clone();
        let caption = signal(String::new());
        let ucap = ui.clone();
        let cap_effect = crate::pages::dict::reactive::reactive_effect(
            move || cell.version().wrapping_add(ucap.view.version()),
            move || {
                let (scope, col) = cell.get();
                caption.set(format!(
                    "预览：{} · {}",
                    scope_label(&scope, &names.borrow()),
                    col.title()
                ));
            },
        );
        let (scope, col) = cell.get();
        caption.set(format!(
            "预览：{} · {}",
            scope_label(&scope, &ui.names.borrow()),
            col.title()
        ));
        Element::col()
            .width_match()
            .spacing(4)
            .padding_xy(0, 6)
            .child(
                Element::label_signal(caption)
                    .font_size(12.0)
                    .fg_role(Role::TextMuted),
            )
            .child(preview(
                ui.visible,
                vec![PreviewSource {
                    field: TemplateField::new(ui.preview_text, true),
                    scene: "comment",
                    each: None,
                    caption: "",
                    bold: false,
                }],
                || 0,
            ))
            .child(cap_effect)
    };
    Element::col()
        .width_match()
        .padding_xy(0, 6)
        .child(
            Element::label(
                "每格选模板、「不显示」或「跟随」。跟随的格按 模式 → 方案 → 方案类型 → 全局 \
                 往下找，灰色斜体后缀是它实际落到的模板。横排的「同竖排」即沿用本行竖排那格。",
            )
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(3)
            .padding_xy(2, 4),
        )
        .child(table)
        .child(preview)
}

const SCOPE_W: f32 = 0.8;

fn build_table(ui: &CommentSettingsUi, lib: &LibraryInfo) -> Element {
    let user = ui.assign.get();
    let rows = table_rows(&user, &lib.system_assign);
    let names = ui.names.borrow().clone();
    let mut columns = vec![("作用范围".to_string(), SCOPE_W)];
    columns.extend(Col::ALL.iter().map(|c| (c.title().to_string(), 1.0)));
    let body: Vec<Vec<Element>> = rows
        .iter()
        .map(|scope| {
            let mut cells = vec![scope_cell(ui, lib, scope, &names)];
            for col in Col::ALL {
                cells.push(cell_dropdown(ui, lib, &user, scope, col));
            }
            cells
        })
        .collect();
    let table = Element::table_custom(columns, body).height(252);

    let add_schema = add_dropdown(
        ui,
        "＋ 添加方案覆盖…",
        names.addable_schemas(&rows),
        Scope::Schema,
    );
    let add_mode = add_dropdown(
        ui,
        "＋ 添加模式覆盖…",
        names.addable_modes(&rows),
        Scope::Mode,
    );
    Element::col().width_match().child(table).child(
        Element::row()
            .width_match()
            .spacing(8)
            .padding_xy(0, 8)
            .child(add_schema.width(200))
            .child(add_mode.width(200)),
    )
}

fn scope_cell(ui: &CommentSettingsUi, lib: &LibraryInfo, scope: &Scope, names: &Names) -> Element {
    let mut row = Element::row()
        .cross(Align::Center)
        .spacing(4)
        .padding_xy(8, 4)
        .child(
            Element::label(scope_label(scope, names))
                .font_size(12.5)
                .fg_role(Role::Text)
                .max_lines(1)
                .weight(1.0),
        );
    if scope.removable() {
        let factory = scope.cells(&lib.system_assign).is_some();
        let (text, tip) = if factory {
            ("清空", "出厂分配的行删不掉：各格改回跟随")
        } else {
            ("移除", "删掉这一行覆盖")
        };
        let (u, s) = (ui.clone(), scope.clone());
        row = row.child(
            Element::button(text)
                .small()
                .outline_soft()
                .tooltip(tip)
                .on_click(move |_| u.remove(&s)),
        );
    }
    row
}

fn cell_dropdown(
    ui: &CommentSettingsUi,
    lib: &LibraryInfo,
    user: &CommentAssign,
    scope: &Scope,
    col: Col,
) -> Element {
    let current = shown_choice(user, &lib.system_assign, scope, col);
    let options = cell_options(lib, scope, col, &current);
    let resolved = lib.effective_of(scope, col).map(|(id, _)| lib.name_of(id));
    let items: Vec<windui::ui::select::DropdownItem> = options
        .iter()
        .map(|c| windui::ui::select::DropdownItem::new(choice_label(lib, c, resolved.as_deref())))
        .collect();
    let idx = options.iter().position(|c| *c == current).unwrap_or(0);
    let sel = signal(idx);
    let following = matches!(current, Choice::Follow | Choice::SameAsVertical);
    let mut dd = Element::dropdown_items(items, sel)
        .weight(1.0)
        .font_size(12.0);
    if following {
        dd = dd.italic().opacity(0.6);
    }
    let (u, s) = (ui.clone(), scope.clone());
    let effect = crate::pages::dict::reactive::reactive_effect(
        move || sel.version(),
        move || {
            if let Some(c) = options.get(sel.get()) {
                u.choose(&s, col, c);
            }
        },
    );
    Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(2, 3)
        .child(dd)
        .child(effect)
}

/// 「＋ 添加…」下拉：首项是占位标题，选中其余任一项即加一行。
fn add_dropdown(
    ui: &CommentSettingsUi,
    title: &str,
    options: Vec<(String, String)>,
    make: fn(String) -> Scope,
) -> Element {
    let mut labels = vec![title.to_string()];
    labels.extend(options.iter().map(|(_, l)| l.clone()));
    let sel = signal(0usize);
    let u = ui.clone();
    let effect = crate::pages::dict::reactive::reactive_effect(
        move || sel.version(),
        move || {
            let i = sel.get();
            if let Some((k, _)) = i.checked_sub(1).and_then(|j| options.get(j)) {
                u.add_row(make(k.clone()));
            }
        },
    );
    Element::row()
        .child(Element::dropdown(labels, sel).width_match())
        .child(effect)
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

    /// 出厂：全局两格、码表详细竖、临时英文默认竖 = none。
    fn system() -> CommentAssign {
        let mut a = CommentAssign {
            global: cells(Some("code_hint"), None, Some("detail_full"), None),
            ..Default::default()
        };
        a.by_type.insert(
            "codetable".into(),
            cells(None, None, Some("chaizi_pinyin"), None),
        );
        a.mode.insert(
            "temp_english".into(),
            cells(Some("none"), None, Some("en_meaning"), None),
        );
        a
    }

    fn mode(k: &str) -> Scope {
        Scope::Mode(k.into())
    }

    #[test]
    fn same_as_vertical_removes_horizontal_key() {
        let sys = system();
        let mut user = CommentAssign::default();
        user.schema.insert(
            "wubi86".into(),
            cells(Some("pinyin"), Some("chaizi"), None, None),
        );
        let s = Scope::Schema("wubi86".into());
        write_choice(&mut user, &sys, &s, Col::DefaultH, &Choice::SameAsVertical);
        assert_eq!(
            user.schema["wubi86"],
            cells(Some("pinyin"), None, None, None)
        );
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
            Col::DetailV,
            &Choice::Template("pinyin".into()),
        );
        write_choice(&mut user, &sys, &s, Col::DetailV, &Choice::Follow);
        assert_eq!(
            user.schema["wubi86"],
            CommentCells::default(),
            "用户行：删键"
        );

        let t = Scope::Type("codetable".into());
        write_choice(&mut user, &sys, &t, Col::DetailV, &Choice::Follow);
        assert_eq!(
            user.by_type["codetable"].detail_vertical.as_deref(),
            Some("inherit"),
            "出厂格：写 inherit 才压得过出厂值"
        );
        assert_eq!(shown_choice(&user, &sys, &t, Col::DetailV), Choice::Follow);
    }

    #[test]
    fn hide_writes_none_and_factory_value_removes_key() {
        let sys = system();
        let mut user = CommentAssign::default();
        write_choice(
            &mut user,
            &sys,
            &Scope::Global,
            Col::DefaultV,
            &Choice::Hide,
        );
        assert_eq!(user.global.default_vertical.as_deref(), Some("none"));
        write_choice(
            &mut user,
            &sys,
            &Scope::Global,
            Col::DefaultV,
            &Choice::Template("code_hint".into()),
        );
        assert_eq!(user.global, CommentCells::default(), "选回出厂值即删键");
        // 出厂本就是 none 的格选不显示：不写。
        write_choice(
            &mut user,
            &sys,
            &mode("temp_english"),
            Col::DefaultV,
            &Choice::Hide,
        );
        assert!(user.mode["temp_english"].default_vertical.is_none());
    }

    #[test]
    fn horizontal_follow_on_user_row_writes_inherit() {
        let sys = system();
        let mut user = CommentAssign::default();
        let s = Scope::Schema("wubi86".into());
        write_choice(&mut user, &sys, &s, Col::DetailH, &Choice::Follow);
        assert_eq!(
            user.schema["wubi86"].detail_horizontal.as_deref(),
            Some("inherit"),
            "横排不写是同竖排，跟随下层横排要写 inherit"
        );
    }

    #[test]
    fn remove_row_deletes_user_rows_and_resets_factory_rows() {
        let sys = system();
        let mut user = CommentAssign::default();
        user.mode
            .insert("url".into(), cells(Some("pinyin"), None, None, None));
        user.mode.insert(
            "temp_english".into(),
            cells(Some("pinyin"), None, None, None),
        );
        assert!(!remove_row(&mut user, &sys, &mode("url")));
        assert!(!user.mode.contains_key("url"));
        assert!(remove_row(&mut user, &sys, &mode("temp_english")));
        assert_eq!(
            user.mode["temp_english"],
            cells(Some("inherit"), None, Some("inherit"), None)
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
        let opts = cell_options(&lib, &Scope::Type("pinyin".into()), Col::DefaultH, &cur);
        assert_eq!(
            opts,
            [
                Choice::Hide,
                Choice::Follow,
                Choice::SameAsVertical,
                cur.clone()
            ]
        );
        assert_eq!(choice_label(&lib, &cur, None), "gone（不存在）");
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
            Col::DefaultV,
            &Choice::Hide,
        );
        assert_eq!(
            serde_json::to_value(&user.schema).unwrap(),
            json!({ "wubi86": { "default_vertical": "none", "detail_vertical": "mine" } })
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 全局行不给跟随；出厂写了横排值的格不给同竖排。
    #[test]
    fn options_drop_follow_on_global_and_same_as_vertical_over_factory_horizontal() {
        let mut lib = LibraryInfo::default();
        lib.system_assign
            .by_type
            .insert("codetable".into(), cells(None, Some("pinyin"), None, None));
        let g = cell_options(&lib, &Scope::Global, Col::DefaultV, &Choice::Hide);
        assert!(!g.contains(&Choice::Follow));
        let t = Scope::Type("codetable".into());
        let h = cell_options(&lib, &t, Col::DefaultH, &Choice::Template("pinyin".into()));
        assert!(!h.contains(&Choice::SameAsVertical));
        assert!(h.contains(&Choice::Follow));
        let h2 = cell_options(&lib, &t, Col::DetailH, &Choice::SameAsVertical);
        assert!(h2.contains(&Choice::SameAsVertical), "出厂没写横排的格照给");
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
        ui.choose(&t, Col::DetailV, &Choice::Follow);
        assert_eq!(
            ui.assign.get().by_type["codetable"]
                .detail_vertical
                .as_deref(),
            Some("inherit")
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }
}
