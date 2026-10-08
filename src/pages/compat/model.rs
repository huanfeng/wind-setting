//! 「应用兼容」页的数据模型：核心 `compat.*` RPC 的响应解析成 Rust 类型。
//!
//! 只做**形状**，不做界面：契约在核心（`wind-config` 的 `compat_admin` / `compat_schema`），
//! 这里的类型逐字段对应它的 camelCase JSON。核心新增字段时，这里不认识的键被 serde 忽略，
//! 不会让整页解析失败；核心**改名 / 删除**已用字段则由 `mock` 的漂移守护测试拦下。

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// 三段规则。`id` 是 RPC 的 `section` 参数值，也是元数据里的 `section`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Apps,
    InitialModeScope,
    CommitNewline,
}

impl Section {
    pub const ALL: [Section; 3] = [
        Section::Apps,
        Section::InitialModeScope,
        Section::CommitNewline,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Apps => "apps",
            Self::InitialModeScope => "initial_mode_scope",
            Self::CommitNewline => "commit_newline",
        }
    }

    /// 顶部分段切换的标题。
    pub fn title(self) -> &'static str {
        match self {
            Self::Apps => "应用规则",
            Self::InitialModeScope => "窗口作用域",
            Self::CommitNewline => "上屏换行",
        }
    }

    pub fn from_index(i: usize) -> Section {
        Self::ALL.get(i).copied().unwrap_or(Section::Apps)
    }
}

/// 字段的控件类型（对应核心 `compat_schema::Kind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// 普通开关（未配 = 关）。
    Bool,
    /// 三态：跟随全局 / 开 / 关。
    TriBool,
    Enum,
    Int,
    Text,
    TextList,
}

/// 联动条件：仅当 `key` 的值等于 `value` 时本字段才有意义。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Dep {
    pub key: String,
    pub value: String,
}

/// 一个字段的元数据（`compat.schema`）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldMeta {
    pub section: String,
    pub key: String,
    pub kind: Kind,
    pub group: String,
    pub label: String,
    pub summary: String,
    pub problem: String,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub options: Vec<String>,
    /// 与 `options` 同序的中文显示名（核心登记）；旧版核心没有这个字段时为空，退回原 id。
    #[serde(default)]
    pub option_labels: Vec<String>,
    /// 三态开关三档（跟随 / 真 / 假）的显示名，与右键菜单用词一致；旧核心没有时用默认说法。
    #[serde(default)]
    pub tri_labels: Vec<String>,
    #[serde(default)]
    pub depends_on: Option<Dep>,
    #[serde(default)]
    pub protocol: bool,
    #[serde(default)]
    pub advanced: bool,
}

impl FieldMeta {
    /// 三态开关的三档显示名（下标 0 = 跟随，1 = 真，2 = 假）。
    pub fn tri_label_set(&self) -> Vec<String> {
        if self.tri_labels.len() == 3 {
            self.tri_labels.clone()
        } else {
            vec!["跟随全局".into(), "开".into(), "关".into()]
        }
    }

    /// 枚举取值的中文名；核心没给（旧版）或没登记时退回原 id。
    pub fn option_label(&self, id: &str) -> String {
        self.options
            .iter()
            .position(|o| o == id)
            .and_then(|i| self.option_labels.get(i))
            .cloned()
            .unwrap_or_else(|| id.to_string())
    }

    /// 联动条件是否满足：`dependsOn` 指向的字段当前生效值等于要求值才有意义。
    pub fn dep_met(&self, rule: &RuleView) -> bool {
        match &self.depends_on {
            None => true,
            Some(d) => rule
                .fields
                .get(&d.key)
                .is_some_and(|v| v.value.as_str() == Some(d.value.as_str())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Group {
    pub id: String,
    pub label: String,
}

/// `compat.schema` 的响应。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct Schema {
    pub groups: Vec<Group>,
    pub fields: Vec<FieldMeta>,
}

impl Schema {
    pub fn parse(v: &Value) -> Result<Schema, String> {
        serde_json::from_value(v.clone()).map_err(|e| format!("compat.schema 响应不合法: {e}"))
    }

    /// 某一段的字段，按登记顺序。
    pub fn fields_of(&self, section: Section) -> Vec<&FieldMeta> {
        self.fields
            .iter()
            .filter(|f| f.section == section.id())
            .collect()
    }

    pub fn field(&self, section: Section, key: &str) -> Option<&FieldMeta> {
        self.fields
            .iter()
            .find(|f| f.section == section.id() && f.key == key)
    }

    /// 某一段按分组归拢的字段：分组顺序取 `groups` 的顺序，组内取登记顺序，空组不出。
    /// `advanced` 字段是否出现由调用方过滤。
    pub fn groups_of(&self, section: Section) -> Vec<(&Group, Vec<&FieldMeta>)> {
        self.groups
            .iter()
            .filter_map(|g| {
                let fs: Vec<&FieldMeta> = self
                    .fields
                    .iter()
                    .filter(|f| f.section == section.id() && f.group == g.id)
                    .collect();
                (!fs.is_empty()).then_some((g, fs))
            })
            .collect()
    }
}

/// 一条规则在分层视图里的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleState {
    System,
    Modified,
    User,
    Disabled,
}

impl RuleState {
    /// 状态徽章文案。
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "系统",
            Self::Modified => "已修改",
            Self::User => "自定义",
            Self::Disabled => "已禁用",
        }
    }
}

/// 字段的值是谁定的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldSource {
    /// 系统与用户都没设，走全局 / 默认。
    Default,
    /// 出厂设了，用户没动。
    System,
    /// 用户覆盖了。
    User,
    /// 出厂设了，用户取消了（回到跟随全局）。
    Cleared,
}

/// 逐字段视图（核心 `compat_admin::FieldView`）。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldView {
    pub source: FieldSource,
    /// 生效值，按控件类型给类型化默认值（Bool = false，Int = 0，TextList = []，其余未设为 null）。
    pub value: Value,
    pub system_value: Value,
    pub user_value: Value,
}

impl FieldView {
    /// 「还原为出厂」有没有东西可还原：用户改写过或取消过。
    pub fn can_reset(&self) -> bool {
        matches!(self.source, FieldSource::User | FieldSource::Cleared)
    }
}

/// 「所有应用」：`[[apps]]` 不写进程名等于写它。
pub const ANY_PROCESS: &str = "*";

/// 窗口条件模式的最大长度（字符数），与核心 `compat_admin::MAX_PATTERN_CHARS` 一致。
pub const MAX_PATTERN_CHARS: usize = 256;

/// 规范化身份（与核心 `compat_admin::rule_key_string` 逐字一致）：进程名 trim + ASCII 小写
/// （空 = `*`），类名 / 标题 trim + 逐字符转小写（转小写后不是单个字符的保留原字符），
/// 三者以 U+001F 连接。界面认「同一条」只用它。
pub fn rule_key(process: &str, class: &str, title: &str) -> String {
    let fold = |s: &str| -> String {
        s.trim()
            .chars()
            .map(|c| {
                let mut l = c.to_lowercase();
                match (l.next(), l.next()) {
                    (Some(x), None) => x,
                    _ => c,
                }
            })
            .collect()
    };
    let p = match process.trim() {
        "" => ANY_PROCESS.to_string(),
        p => p.to_ascii_lowercase(),
    };
    format!("{p}\u{1f}{}\u{1f}{}", fold(class), fold(title))
}

/// 规则身份的原样写法：写接口按它定位（核心按规范化身份认同一条）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleIdent {
    /// 进程名；空串或 `*` = 所有应用。
    pub process: String,
    /// 顶层窗口类名模式；空 = 不限。
    pub class: String,
    /// 顶层窗口标题模式；空 = 不限。
    pub title: String,
}

impl RuleIdent {
    /// 纯进程键。
    pub fn process(p: &str) -> Self {
        Self::new(p, "", "")
    }

    /// 各项 trim；带窗口条件、进程名留空时记成 `*`（与核心列表里的写法一致）。
    pub fn new(process: &str, class: &str, title: &str) -> Self {
        let (class, title) = (class.trim().to_string(), title.trim().to_string());
        let mut process = process.trim().to_string();
        if process.is_empty() && !(class.is_empty() && title.is_empty()) {
            process = ANY_PROCESS.to_string();
        }
        Self {
            process,
            class,
            title,
        }
    }

    pub fn of(r: &RuleView) -> Self {
        Self::new(&r.process, &r.class, &r.title)
    }

    pub fn key(&self) -> String {
        rule_key(&self.process, &self.class, &self.title)
    }

    /// 写接口的身份参数：`process` 必带（旧核心只认它），`class` / `title` 有才带。
    pub fn params(&self) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert("process".into(), Value::String(self.process.clone()));
        if !self.class.is_empty() {
            m.insert("class".into(), Value::String(self.class.clone()));
        }
        if !self.title.is_empty() {
            m.insert("title".into(), Value::String(self.title.clone()));
        }
        m
    }

    pub fn has_window_condition(&self) -> bool {
        !self.class.is_empty() || !self.title.is_empty()
    }

    /// 列表 / 标题里显示的名字：`*` 显示成「所有应用」。
    pub fn display_name(&self) -> String {
        display_process(&self.process)
    }

    /// 窗口条件的一行说明（没有条件为空串），如「类名：Chrome_WidgetWin_* · 标题：*记事本」。
    pub fn condition_text(&self) -> String {
        let mut parts = Vec::new();
        if !self.class.is_empty() {
            parts.push(format!("类名：{}", self.class));
        }
        if !self.title.is_empty() {
            parts.push(format!("标题：{}", self.title));
        }
        parts.join(" · ")
    }

    /// 完整的一行描述（名字 + 条件），对话框 / 导入预览用。
    pub fn describe(&self) -> String {
        match self.condition_text() {
            c if c.is_empty() => self.display_name(),
            c => format!("{}（{c}）", self.display_name()),
        }
    }

    /// 按段校验（与核心 `compat_admin::validate_key` 同一套规则，提前在界面里拦下）。
    pub fn validate(&self, section: Section) -> Result<(), String> {
        let p = self.process.as_str();
        if p.contains('/') || p.contains('\\') {
            return Err("进程名不能含路径分隔符".into());
        }
        for (what, v) in [("进程名", p), ("类名", &self.class), ("标题", &self.title)] {
            if v.chars().any(char::is_control) {
                return Err(format!("{what}不能含控制字符"));
            }
        }
        for (what, v) in [("类名", &self.class), ("标题", &self.title)] {
            if v.chars().count() > MAX_PATTERN_CHARS {
                return Err(format!("{what}不能超过 {MAX_PATTERN_CHARS} 个字符"));
            }
        }
        if section == Section::Apps {
            if p.is_empty() {
                return Err("进程名、类名、标题至少要填一项".into());
            }
        } else {
            if self.has_window_condition() {
                return Err("这一类规则不支持类名 / 标题条件".into());
            }
            if p.is_empty() {
                return Err("进程名不能为空".into());
            }
            if p == ANY_PROCESS {
                return Err("这一类规则不支持「*」，请填具体进程名".into());
            }
        }
        Ok(())
    }
}

/// 进程名的显示：`*` / 空 = 「所有应用」。
pub fn display_process(p: &str) -> String {
    match p.trim() {
        "" | ANY_PROCESS => "所有应用".to_string(),
        p => p.to_string(),
    }
}

/// 一条规则的视图（`compat.list` 的 `rules[]`）。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleView {
    pub process: String,
    /// 顶层窗口类名模式（原样）；没有为空串。
    #[serde(default)]
    pub class: String,
    /// 顶层窗口标题模式（原样）；没有为空串。
    #[serde(default)]
    pub title: String,
    /// 规范化身份（见 [`rule_key`]）。旧核心不给时解析后按 [`rule_key`] 补上。
    #[serde(default)]
    pub key: String,
    pub state: RuleState,
    #[serde(default)]
    pub effective: Value,
    #[serde(default)]
    pub system: Option<Value>,
    #[serde(default)]
    pub overridden: Vec<String>,
    #[serde(default)]
    pub has_user_entry: bool,
    #[serde(default)]
    pub user: Option<Value>,
    #[serde(default)]
    pub unknown_keys: Vec<String>,
    #[serde(default)]
    pub fields: BTreeMap<String, FieldView>,
}

impl RuleView {
    /// 新建规则的**草稿**视图：还没落盘，所以系统层没有、用户层也没有，每个字段都是
    /// 「未设置」。第一次设置字段时才 `compat.upsert`，之后核心返回的才是真规则。
    pub fn draft(ident: &RuleIdent, schema: &Schema, section: Section) -> RuleView {
        let fields = schema
            .fields_of(section)
            .into_iter()
            .map(|m| {
                let value = match m.kind {
                    Kind::Bool => Value::Bool(false),
                    Kind::Int => Value::from(0),
                    Kind::TextList => Value::Array(Vec::new()),
                    Kind::TriBool | Kind::Enum | Kind::Text => Value::Null,
                };
                (
                    m.key.clone(),
                    FieldView {
                        source: FieldSource::Default,
                        value,
                        system_value: Value::Null,
                        user_value: Value::Null,
                    },
                )
            })
            .collect();
        RuleView {
            process: ident.process.clone(),
            class: ident.class.clone(),
            title: ident.title.clone(),
            key: ident.key(),
            state: RuleState::User,
            effective: serde_json::json!({ "process": ident.process }),
            system: None,
            overridden: Vec::new(),
            has_user_entry: false,
            user: None,
            unknown_keys: Vec::new(),
            fields,
        }
    }

    /// 是不是还没落盘的草稿。
    pub fn is_draft(&self) -> bool {
        self.state == RuleState::User && !self.has_user_entry && self.system.is_none()
    }

    /// 界面能不能给这条规则提供「删除」：只有用户新增的（系统层没有的）才谈得上删除。
    pub fn is_user_only(&self) -> bool {
        self.state == RuleState::User || (self.system.is_none() && self.has_user_entry)
    }

    /// 「还原此规则」有没有东西可还原：用户层有这一条，且它不是「用户新增」的。
    pub fn can_reset(&self) -> bool {
        self.has_user_entry && self.system.is_some()
    }

    /// 匹配条件能不能改：只有用户新增的规则（含草稿）；出厂规则只能「复制为新规则」。
    pub fn can_rename(&self) -> bool {
        self.system.is_none()
    }

    pub fn ident(&self) -> RuleIdent {
        RuleIdent::of(self)
    }
}

/// 被当成「没写」忽略的无效内容（`compat.list` 的 `warnings[]`）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Problem {
    pub section: String,
    pub process: String,
    #[serde(default)]
    pub class: String,
    #[serde(default)]
    pub title: String,
    pub key: String,
    pub message: String,
}

impl Problem {
    /// 是不是说的这条规则（按规范化身份比）。
    ///
    /// 「整条作废」类问题（核心报在 `process` 键上：身份认不出的行）不归属任何列出的规则——
    /// 那一行根本不在列表里，按 key 硬配会挂到恰好同名的别的规则上。
    pub fn is_about(&self, rule: &RuleView) -> bool {
        self.key != "process" && rule_key(&self.process, &self.class, &self.title) == rule.key
    }
}

/// `compat.list` 的响应。
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct ListResult {
    #[serde(default)]
    pub rules: Vec<RuleView>,
    #[serde(default)]
    pub warnings: Vec<Problem>,
    /// 核心支持按类名 / 标题管理规则（每条规则都带 `key`）。旧核心不给 `key`：它的写接口只认
    /// 进程名、会忽略 `class` / `title`，在它上面「新增带类名的规则」会写成一条 `*` 或纯进程规则，
    /// 所以界面要藏起窗口条件的输入与「修改匹配条件」「复制为新规则」。
    #[serde(skip)]
    pub window_conditions: bool,
}

impl ListResult {
    pub fn parse(v: &Value) -> Result<ListResult, String> {
        let mut l: ListResult = serde_json::from_value(v.clone())
            .map_err(|e| format!("compat.list 响应不合法: {e}"))?;
        l.window_conditions = l.rules.iter().all(|r| !r.key.is_empty());
        for r in &mut l.rules {
            if r.key.is_empty() {
                r.key = r.ident().key();
            }
        }
        Ok(l)
    }
}

/// 导入预览里的一条。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ImportItem {
    pub section: String,
    pub process: String,
    #[serde(default)]
    pub class: String,
    #[serde(default)]
    pub title: String,
    /// `add` / `override` / `disable` / `unchanged` / `remove`。
    pub action: String,
}

/// `compat.import` 的响应（`dryRun` 预览与真正导入同形）。
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    #[serde(default)]
    pub items: Vec<ImportItem>,
    #[serde(default)]
    pub ignored_keys: Vec<String>,
    #[serde(default)]
    pub fallbacks: Vec<String>,
    #[serde(default)]
    pub rejected: Vec<String>,
    #[serde(default)]
    pub applied: bool,
    #[serde(default)]
    pub backup: Option<String>,
}

impl ImportPreview {
    pub fn parse(v: &Value) -> Result<ImportPreview, String> {
        serde_json::from_value(v.clone()).map_err(|e| format!("compat.import 响应不合法: {e}"))
    }

    /// 按动作统计条数：`(新增, 覆盖, 禁用, 无变化, 移除)`。
    pub fn counts(&self) -> (usize, usize, usize, usize, usize) {
        let n = |a: &str| self.items.iter().filter(|i| i.action == a).count();
        (
            n("add"),
            n("override"),
            n("disable"),
            n("unchanged"),
            n("remove"),
        )
    }

    /// 真的会改动用户层的条数（不含「无变化」）。预览为 0 时确认按钮没有意义。
    pub fn changes(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.action != "unchanged")
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn section_ids_match_the_rpc_contract() {
        assert_eq!(Section::Apps.id(), "apps");
        assert_eq!(Section::InitialModeScope.id(), "initial_mode_scope");
        assert_eq!(Section::CommitNewline.id(), "commit_newline");
        assert_eq!(Section::from_index(1), Section::InitialModeScope);
        assert_eq!(Section::from_index(99), Section::Apps, "越界回落到第一段");
    }

    fn sample_schema() -> Schema {
        Schema::parse(&json!({
            "groups": [{"id": "position", "label": "候选窗定位"}, {"id": "host", "label": "宿主兼容"}, {"id": "empty", "label": "空组"}],
            "fields": [
                {"section": "apps", "key": "first_show_mode", "kind": "enum", "group": "position", "label": "首显", "summary": "s", "problem": "p",
                 "hosts": ["EXCEL.EXE"], "options": ["wait", "fast"], "dependsOn": null, "protocol": false, "advanced": false},
                {"section": "apps", "key": "candidate_x", "kind": "int", "group": "position", "label": "X", "summary": "s", "problem": "p",
                 "hosts": [], "options": [], "dependsOn": {"key": "candidate_position_mode", "value": "fixed"}, "protocol": false, "advanced": false},
                {"section": "apps", "key": "stale_probe_guard", "kind": "bool", "group": "host", "label": "陈旧", "summary": "s", "problem": "p",
                 "hosts": [], "options": [], "dependsOn": null, "protocol": false, "advanced": true},
                {"section": "commit_newline", "key": "style", "kind": "enum", "group": "position", "label": "换行", "summary": "s", "problem": "p",
                 "hosts": [], "options": ["keep"], "dependsOn": null, "protocol": false, "advanced": false}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn schema_groups_are_ordered_by_the_group_list_and_skip_empty_and_other_sections() {
        let s = sample_schema();
        let g = s.groups_of(Section::Apps);
        assert_eq!(
            g.iter().map(|(g, _)| g.id.as_str()).collect::<Vec<_>>(),
            ["position", "host"],
            "空组不出，别的段的字段不混入"
        );
        assert_eq!(
            g[0].1.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(),
            ["first_show_mode", "candidate_x"],
            "组内取登记顺序"
        );
        assert_eq!(s.groups_of(Section::CommitNewline).len(), 1);
        assert!(s.groups_of(Section::InitialModeScope).is_empty());
        assert_eq!(
            s.field(Section::Apps, "candidate_x")
                .unwrap()
                .depends_on
                .as_ref()
                .unwrap()
                .value,
            "fixed"
        );
        assert!(s.field(Section::Apps, "style").is_none(), "字段按段查找");
    }

    #[test]
    fn schema_tolerates_unknown_keys_from_a_newer_core() {
        let v = json!({"groups": [], "fields": [{"section": "apps", "key": "k", "kind": "bool", "group": "g", "label": "l",
            "summary": "s", "problem": "p", "someFutureKey": 1}], "extra": true});
        assert!(Schema::parse(&v).is_ok(), "核心新增的键不能让整页解析失败");
        assert!(Schema::parse(&json!({"groups": 1})).is_err());
    }

    #[test]
    fn rule_view_parses_the_core_shape_and_answers_the_action_questions() {
        let v = json!({"rules": [
            {"process": "Weixin.exe", "state": "modified", "effective": {}, "system": {"caret_use_top": true}, "overridden": ["first_show_mode"],
             "hasUserEntry": true, "user": {"first_show_mode": "wait"}, "unknownKeys": [],
             "fields": {"first_show_mode": {"source": "user", "value": "wait", "systemValue": null, "userValue": "wait"}}},
            {"process": "Mine.exe", "state": "user", "system": null, "hasUserEntry": true, "fields": {}},
            {"process": "QQ.exe", "state": "system", "system": {}, "hasUserEntry": false, "fields": {}}
        ], "warnings": [{"section": "apps", "process": "A.exe", "key": "k", "message": "m"}]});
        let l = ListResult::parse(&v).unwrap();
        assert_eq!(l.rules.len(), 3);
        assert_eq!(l.warnings[0].key, "k");
        let (w, m, q) = (&l.rules[0], &l.rules[1], &l.rules[2]);
        assert!(
            w.can_reset() && !w.is_user_only(),
            "系统规则被改写：可还原、不可删除"
        );
        assert!(
            !m.can_reset() && m.is_user_only(),
            "用户新增：不可还原、可删除"
        );
        assert!(
            !q.can_reset() && !q.is_user_only(),
            "未动过的系统规则：什么都不可做"
        );
        assert!(w.fields["first_show_mode"].can_reset());
        assert_eq!(w.state.label(), "已修改");
    }

    #[test]
    fn import_preview_counts_by_action() {
        let p = ImportPreview::parse(&json!({"items": [
            {"section": "apps", "process": "a", "action": "add"}, {"section": "apps", "process": "b", "action": "add"},
            {"section": "apps", "process": "c", "action": "override"}, {"section": "apps", "process": "d", "action": "unchanged"},
            {"section": "apps", "process": "e", "action": "remove"}], "ignoredKeys": ["apps.x"], "applied": false})).unwrap();
        assert_eq!(p.counts(), (2, 1, 0, 1, 1));
        assert_eq!(p.changes(), 4, "无变化不算改动");
        assert_eq!(p.ignored_keys, ["apps.x"]);
        assert!(p.backup.is_none());
        assert_eq!(ImportPreview::default().changes(), 0);
    }

    /// 界面算的身份必须与核心给的 `key` 逐字一致：界面拿它比对选中项、草稿与核心返回的规则。
    #[test]
    fn local_rule_key_matches_the_core_key_for_every_mock_rule() {
        for sec in Section::ALL {
            let l = ListResult::parse(&super::super::mock::reply(
                "compat.list",
                &json!({"section": sec.id()}),
            ))
            .unwrap();
            for r in &l.rules {
                assert!(!r.key.is_empty());
                assert_eq!(rule_key(&r.process, &r.class, &r.title), r.key, "{r:?}");
                assert_eq!(r.ident().key(), r.key);
            }
        }
        assert_eq!(rule_key(" A.EXE ", " Foo* ", ""), "a.exe\u{1f}foo*\u{1f}");
        assert_eq!(
            rule_key("", "X", ""),
            rule_key("*", "x", ""),
            "不写进程 = *"
        );
    }

    #[test]
    fn rule_ident_display_and_params() {
        let a = RuleIdent::new("", " Chrome_WidgetWin_* ", "");
        assert_eq!(a.process, "*");
        assert_eq!(a.display_name(), "所有应用");
        assert_eq!(a.condition_text(), "类名：Chrome_WidgetWin_*");
        assert_eq!(
            Value::Object(a.params()),
            json!({"process": "*", "class": "Chrome_WidgetWin_*"})
        );
        let b = RuleIdent::new("ahk.exe", "", "Ed*");
        assert_eq!(b.describe(), "ahk.exe（标题：Ed*）");
        assert_eq!(RuleIdent::process("x.exe").describe(), "x.exe");
        assert!(RuleIdent::new("", "", "").validate(Section::Apps).is_err());
        assert!(RuleIdent::process("*").validate(Section::Apps).is_ok());
    }

    #[test]
    fn old_core_without_key_gets_one_computed() {
        let l = ListResult::parse(&json!({"rules": [{"process": "A.exe", "state": "system"}]}))
            .unwrap();
        assert_eq!(l.rules[0].key, rule_key("A.exe", "", ""));
        assert!(!l.window_conditions, "旧核心不给 key ⇒ 不支持窗口条件");
        let new = ListResult::parse(&json!({"rules": [{"process": "A.exe", "key": "a.exe\u{1f}\u{1f}", "state": "system"}]})).unwrap();
        assert!(new.window_conditions);
    }

    /// 「整条作废」的提示不按 key 挂到同名规则上。
    #[test]
    fn whole_rule_problems_are_not_attributed_to_a_listed_rule() {
        let r = RuleView::draft(
            &RuleIdent::process("A.exe"),
            &Schema::default(),
            Section::Apps,
        );
        let p = |key: &str| Problem {
            section: "apps".into(),
            process: "A.exe".into(),
            class: String::new(),
            title: String::new(),
            key: key.into(),
            message: "m".into(),
        };
        assert!(p("auto_pair").is_about(&r));
        assert!(!p("process").is_about(&r));
    }
}
