//! 词库类别描述（[`CategorySpec`]）+ 统一行模型（[`WordRow`]）+ RPC json 映射。
//!
//! 五类词库数据（短语/用户词库/临时词库/词频/候选调整）共用一套表格框架：
//! 每类由一份静态 `CategorySpec` 描述（列/可用操作/RPC 方法名/作用域），
//! 各自 RPC 返回的 json 经 [`WordRow::from_json`] 归一到同一个 [`WordRow`]。
//! 新增或调整类别只改本文件，不动视图/状态框架。

use serde_json::Value;

/// 六类词库数据（短语拆为系统短语 / 用户短语两类）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WordCategory {
    /// 系统短语（全局只读；可开关 / 恢复默认）。
    PhraseSystem,
    /// 用户短语（全局可增删改；支持导入导出）。
    PhraseUser,
    /// 用户词库（按方案·分页）。
    UserDict,
    /// 临时词库（按方案）。
    Temp,
    /// 词频（按方案·分页）。
    Freq,
    /// 候选调整 / 影子规则（按方案，只读 + 撤销）。
    Shadow,
}

/// 导航子标签的逻辑单元：短语两级 + 方案内四类。映射到 (标题, 类别, 短语系统标志)。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogicalTab {
    PhraseSystem,
    PhraseUser,
    UserDict,
    Temp,
    Freq,
    Shadow,
}

impl LogicalTab {
    pub fn title(self) -> &'static str {
        match self {
            LogicalTab::PhraseSystem => "系统短语",
            LogicalTab::PhraseUser => "用户短语",
            LogicalTab::UserDict => "用户词库",
            LogicalTab::Temp => "临时词库",
            LogicalTab::Freq => "词频",
            LogicalTab::Shadow => "候选调整",
        }
    }
    pub fn category(self) -> WordCategory {
        match self {
            LogicalTab::PhraseSystem => WordCategory::PhraseSystem,
            LogicalTab::PhraseUser => WordCategory::PhraseUser,
            LogicalTab::UserDict => WordCategory::UserDict,
            LogicalTab::Temp => WordCategory::Temp,
            LogicalTab::Freq => WordCategory::Freq,
            LogicalTab::Shadow => WordCategory::Shadow,
        }
    }
    /// 短语两级：Some(true)=系统，Some(false)=用户；非短语=None。
    pub fn phrase_system(self) -> Option<bool> {
        match self {
            LogicalTab::PhraseSystem => Some(true),
            LogicalTab::PhraseUser => Some(false),
            _ => None,
        }
    }
}

/// 统一行模型：五类 RPC 返回都归一到此，视图按 [`ColumnSpec::field`] 取字段。
///
/// 字段复用规则：
/// - `text`：dict/phrase/temp/freq 的 `text`；shadow 的 `word`。
/// - `weight`：dict/phrase。`count`：temp/freq。`position`：phrase / shadow(pin)。
/// - `enabled`：phrase。`kind`：shadow 的 `"pin"|"delete"`。`last_used`：freq。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WordRow {
    pub code: String,
    pub text: String,
    pub weight: i64,
    pub count: i64,
    pub position: i64,
    pub enabled: bool,
    pub kind: String,
    pub last_used: String,
    /// 短语专用：是否系统内置（phrase from_json 读 isSystem，其余类别恒 false）。
    pub is_system: bool,
}

impl WordRow {
    /// 按类别把单条 RPC json 映射为 `WordRow`。字段缺失回落默认值。
    pub fn from_json(cat: WordCategory, v: &Value) -> Self {
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let i = |k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
        let b = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
        match cat {
            WordCategory::PhraseSystem | WordCategory::PhraseUser => WordRow {
                code: s("code"),
                text: s("text"),
                weight: i("weight"),
                position: i("position"),
                enabled: b("enabled"),
                is_system: b("isSystem"),
                ..Default::default()
            },
            WordCategory::UserDict => WordRow {
                code: s("code"),
                text: s("text"),
                weight: i("weight"),
                enabled: true,
                ..Default::default()
            },
            WordCategory::Temp => WordRow {
                code: s("code"),
                text: s("text"),
                count: i("count"),
                ..Default::default()
            },
            WordCategory::Freq => WordRow {
                code: s("code"),
                text: s("text"),
                count: i("count"),
                last_used: fmt_last_used(v.get("lastUsed")),
                ..Default::default()
            },
            WordCategory::Shadow => WordRow {
                code: s("code"),
                // shadow 的词字段名是 word，不是 text。
                text: s("word"),
                position: i("position"),
                kind: s("type"),
                ..Default::default()
            },
        }
    }

    /// 从列表 RPC 返回解析出 (行集合, 总数)。
    /// 分页类（dict/freq）返回 `{items,total}`；其余返回裸数组，total = 长度。
    pub fn parse_list(cat: WordCategory, paged: bool, v: &Value) -> (Vec<WordRow>, usize) {
        if paged {
            let items = v
                .get("items")
                .and_then(|x| x.as_array())
                .map(|arr| arr.iter().map(|it| WordRow::from_json(cat, it)).collect())
                .unwrap_or_default();
            let total = v.get("total").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
            (items, total)
        } else {
            let items: Vec<WordRow> = v
                .as_array()
                .map(|arr| arr.iter().map(|it| WordRow::from_json(cat, it)).collect())
                .unwrap_or_default();
            let total = items.len();
            (items, total)
        }
    }

    /// 某列的展示文本（非交互列用；`Enabled`/`Kind` 由视图特殊渲染，这里给回退文本）。
    pub fn cell_text(&self, field: WordField) -> String {
        match field {
            WordField::Code => self.code.clone(),
            WordField::Text => self.text.clone(),
            WordField::Weight => self.weight.to_string(),
            WordField::Count => self.count.to_string(),
            WordField::Position => self.position.to_string(),
            WordField::Enabled => if self.enabled { "是" } else { "否" }.to_string(),
            WordField::Kind => shadow_kind_label(&self.kind).to_string(),
            WordField::LastUsed => self.last_used.clone(),
        }
    }
}

/// 词频 `lastUsed` 的展示文本。core 真实返回为 unix 秒（数字），格式化为本地时间；
/// 字符串形态（mock/旧版）原样保留；0/非法值显示 "—"，缺失为空。
fn fmt_last_used(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => {
            let secs = n.as_i64().unwrap_or(0);
            if secs <= 0 {
                return "—".to_string();
            }
            chrono::DateTime::from_timestamp(secs, 0)
                .map(|dt| {
                    dt.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_else(|| "—".to_string())
        }
        _ => String::new(),
    }
}

/// 候选调整类型的中文标签。
pub fn shadow_kind_label(kind: &str) -> &'static str {
    match kind {
        "pin" => "置顶",
        "delete" => "删除",
        _ => "—",
    }
}

/// 表格列描述。
#[derive(Clone, Copy)]
pub struct ColumnSpec {
    pub header: &'static str,
    /// 列宽权重（传给 `table` 的相对配比）。
    pub weight: f32,
    pub field: WordField,
}

/// 列取 `WordRow` 的哪个字段。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WordField {
    Code,
    Text,
    Weight,
    Count,
    Position,
    Enabled,
    Kind,
    LastUsed,
}

impl WordField {
    /// 服务端排序参数 `sortBy` 的字段名（与 list RPC 返回 item 的 json 键一致）。
    pub fn rpc_key(self) -> &'static str {
        match self {
            WordField::Code => "code",
            WordField::Text => "text",
            WordField::Weight => "weight",
            WordField::Count => "count",
            WordField::Position => "position",
            WordField::Enabled => "enabled",
            WordField::Kind => "type",
            WordField::LastUsed => "lastUsed",
        }
    }

    /// 客户端排序时是否按数值比较（其余按字符串）。
    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            WordField::Weight | WordField::Count | WordField::Position
        )
    }
}

/// 类别可用操作集。视图据此决定显示哪些工具栏/行操作按钮。
#[derive(Clone, Copy)]
pub struct OpFlags {
    pub add: bool,
    pub edit: bool,
    pub remove: bool,
    pub clear: bool,
    pub promote: bool,
    pub promote_all: bool,
    pub remove_rule: bool,
    pub set_enabled: bool,
    pub reset_default: bool,
    pub encode: bool,
    /// 是否支持导入（用户短语）。
    pub import: bool,
    /// 是否支持导出（用户短语）。
    pub export: bool,
    /// 是否支持只读查看（系统短语）。
    pub view: bool,
}

impl OpFlags {
    const NONE: OpFlags = OpFlags {
        add: false,
        edit: false,
        remove: false,
        clear: false,
        promote: false,
        promote_all: false,
        remove_rule: false,
        set_enabled: false,
        reset_default: false,
        encode: false,
        import: false,
        export: false,
        view: false,
    };
}

/// 各操作对应的 RPC 方法名（"" 表示该类别不支持）。
#[derive(Clone, Copy)]
pub struct RpcNames {
    pub list: &'static str,
    pub add: &'static str,
    pub update: &'static str,
    pub remove: &'static str,
    pub clear: &'static str,
    pub promote: &'static str,
    pub promote_all: &'static str,
    pub remove_rule: &'static str,
    pub set_enabled: &'static str,
    pub reset_default: &'static str,
    pub encode: &'static str,
}

impl RpcNames {
    const EMPTY: RpcNames = RpcNames {
        list: "",
        add: "",
        update: "",
        remove: "",
        clear: "",
        promote: "",
        promote_all: "",
        remove_rule: "",
        set_enabled: "",
        reset_default: "",
        encode: "",
    };
}

/// 一类词库的完整描述。
#[derive(Clone, Copy)]
pub struct CategorySpec {
    pub category: WordCategory,
    /// 子标签 / 面板标题。
    pub title: &'static str,
    /// 是否需要 schemaId（短语=false，其余=true）。
    pub scoped_by_schema: bool,
    /// 是否分页（dict/freq=true）。
    pub paged: bool,
    /// 是否支持服务端搜索。
    pub searchable: bool,
    /// 是否走客户端搜索过滤（非分页类：临时/候选调整/短语）。分页类走服务端。
    pub client_search: bool,
    pub columns: &'static [ColumnSpec],
    pub ops: OpFlags,
    pub rpc: RpcNames,
}

// 短语无「启用」数据列：启用开关是交互控件，移入表格尾部操作列（见 view::build_row_actions）。
const COLS_PHRASE: &[ColumnSpec] = &[
    ColumnSpec {
        header: "编码",
        weight: 1.0,
        field: WordField::Code,
    },
    ColumnSpec {
        header: "短语",
        weight: 2.2,
        field: WordField::Text,
    },
    ColumnSpec {
        header: "权重",
        weight: 0.7,
        field: WordField::Weight,
    },
];

const COLS_USERDICT: &[ColumnSpec] = &[
    ColumnSpec {
        header: "编码",
        weight: 1.0,
        field: WordField::Code,
    },
    ColumnSpec {
        header: "词条",
        weight: 2.4,
        field: WordField::Text,
    },
    ColumnSpec {
        header: "权重",
        weight: 0.7,
        field: WordField::Weight,
    },
];

const COLS_TEMP: &[ColumnSpec] = &[
    ColumnSpec {
        header: "编码",
        weight: 1.0,
        field: WordField::Code,
    },
    ColumnSpec {
        header: "词条",
        weight: 2.4,
        field: WordField::Text,
    },
    ColumnSpec {
        header: "次数",
        weight: 0.7,
        field: WordField::Count,
    },
];

const COLS_FREQ: &[ColumnSpec] = &[
    ColumnSpec {
        header: "编码",
        weight: 1.0,
        field: WordField::Code,
    },
    ColumnSpec {
        header: "词条",
        weight: 2.0,
        field: WordField::Text,
    },
    ColumnSpec {
        header: "次数",
        weight: 0.8,
        field: WordField::Count,
    },
    ColumnSpec {
        header: "最近使用",
        weight: 1.2,
        field: WordField::LastUsed,
    },
];

const COLS_SHADOW: &[ColumnSpec] = &[
    ColumnSpec {
        header: "编码",
        weight: 1.0,
        field: WordField::Code,
    },
    ColumnSpec {
        header: "词",
        weight: 2.0,
        field: WordField::Text,
    },
    ColumnSpec {
        header: "类型",
        weight: 0.8,
        field: WordField::Kind,
    },
    ColumnSpec {
        header: "位置",
        weight: 0.7,
        field: WordField::Position,
    },
];

const SPEC_PHRASE_SYSTEM: CategorySpec = CategorySpec {
    category: WordCategory::PhraseSystem,
    title: "系统短语",
    scoped_by_schema: false,
    paged: false,
    searchable: false,
    client_search: true,
    columns: COLS_PHRASE,
    ops: OpFlags {
        set_enabled: true,
        reset_default: true,
        view: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        list: "phrase.listSystem",
        set_enabled: "phrase.setEnabled",
        reset_default: "phrase.resetSystem",
        ..RpcNames::EMPTY
    },
};

const SPEC_PHRASE_USER: CategorySpec = CategorySpec {
    category: WordCategory::PhraseUser,
    title: "用户短语",
    scoped_by_schema: false,
    paged: true,
    searchable: true,
    client_search: false,
    columns: COLS_PHRASE,
    ops: OpFlags {
        add: true,
        edit: true,
        remove: true,
        clear: true,
        set_enabled: true,
        import: true,
        export: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        list: "phrase.listUser",
        add: "phrase.add",
        update: "phrase.update",
        remove: "phrase.remove",
        clear: "phrase.resetDefault",
        set_enabled: "phrase.setEnabled",
        ..RpcNames::EMPTY
    },
};

const SPEC_USERDICT: CategorySpec = CategorySpec {
    category: WordCategory::UserDict,
    title: "用户词库",
    scoped_by_schema: true,
    paged: true,
    searchable: true,
    client_search: false,
    columns: COLS_USERDICT,
    // 导入/导出不再挂在用户词库类别下：改为方案级统一「导入/导出」对话框（多类型可选），
    // 入口在子标签行最右（见 view::build_page），故此处不设 import/export。
    ops: OpFlags {
        add: true,
        edit: true,
        remove: true,
        clear: true,
        encode: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        list: "dict.listPaged",
        add: "dict.add",
        update: "dict.update",
        remove: "dict.remove",
        clear: "dict.clear",
        encode: "dict.encode",
        ..RpcNames::EMPTY
    },
};

const SPEC_TEMP: CategorySpec = CategorySpec {
    category: WordCategory::Temp,
    title: "临时词库",
    scoped_by_schema: true,
    paged: false,
    searchable: false,
    client_search: true,
    columns: COLS_TEMP,
    ops: OpFlags {
        remove: true,
        clear: true,
        promote: true,
        promote_all: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        list: "temp.list",
        remove: "temp.remove",
        clear: "temp.clear",
        promote: "temp.promote",
        promote_all: "temp.promoteAll",
        ..RpcNames::EMPTY
    },
};

const SPEC_FREQ: CategorySpec = CategorySpec {
    category: WordCategory::Freq,
    title: "词频",
    scoped_by_schema: true,
    paged: true,
    searchable: true,
    client_search: false,
    columns: COLS_FREQ,
    ops: OpFlags {
        remove: true,
        clear: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        list: "freq.listPaged",
        remove: "freq.delete",
        clear: "freq.clear",
        ..RpcNames::EMPTY
    },
};

const SPEC_SHADOW: CategorySpec = CategorySpec {
    category: WordCategory::Shadow,
    title: "候选调整",
    scoped_by_schema: true,
    paged: false,
    searchable: false,
    client_search: true,
    columns: COLS_SHADOW,
    ops: OpFlags {
        add: true,
        edit: true,
        remove_rule: true,
        clear: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        list: "shadow.list",
        add: "shadow.addRule",
        remove_rule: "shadow.removeRule",
        // clear 无单独 RPC：视图侧循环 remove_rule 实现。
        ..RpcNames::EMPTY
    },
};

/// 取某类别的静态描述。
pub fn spec_for(cat: WordCategory) -> &'static CategorySpec {
    match cat {
        WordCategory::PhraseSystem => &SPEC_PHRASE_SYSTEM,
        WordCategory::PhraseUser => &SPEC_PHRASE_USER,
        WordCategory::UserDict => &SPEC_USERDICT,
        WordCategory::Temp => &SPEC_TEMP,
        WordCategory::Freq => &SPEC_FREQ,
        WordCategory::Shadow => &SPEC_SHADOW,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn phrase_from_json_maps_all_fields() {
        let v =
            json!({ "code": "bj", "text": "北京", "position": 2, "weight": 5, "enabled": true });
        let r = WordRow::from_json(WordCategory::PhraseUser, &v);
        assert_eq!(r.code, "bj");
        assert_eq!(r.text, "北京");
        assert_eq!(r.weight, 5);
        assert_eq!(r.position, 2);
        assert!(r.enabled);
    }

    #[test]
    fn userdict_from_json_defaults_enabled_true() {
        let v = json!({ "code": "aaaa", "text": "工", "weight": 3 });
        let r = WordRow::from_json(WordCategory::UserDict, &v);
        assert_eq!(r.code, "aaaa");
        assert_eq!(r.weight, 3);
        assert!(r.enabled, "用户词库行默认 enabled=true");
    }

    #[test]
    fn shadow_reads_word_not_text_and_kind_from_type() {
        let pin =
            json!({ "code": "ab", "word": "阿", "candId": "c1", "type": "pin", "position": 1 });
        let r = WordRow::from_json(WordCategory::Shadow, &pin);
        assert_eq!(r.text, "阿", "shadow 词字段来自 word");
        assert_eq!(r.kind, "pin");
        assert_eq!(r.position, 1);

        let del = json!({ "code": "cd", "word": "屏", "candId": null, "type": "delete" });
        let r2 = WordRow::from_json(WordCategory::Shadow, &del);
        assert_eq!(r2.kind, "delete");
        assert_eq!(shadow_kind_label(&r2.kind), "删除");
    }

    #[test]
    fn freq_reads_last_used_camel_case() {
        let v = json!({ "code": "x", "text": "词", "count": 9, "lastUsed": "2026-06-30" });
        let r = WordRow::from_json(WordCategory::Freq, &v);
        assert_eq!(r.count, 9);
        assert_eq!(r.last_used, "2026-06-30");
    }

    #[test]
    fn freq_last_used_numeric_formats_local_datetime() {
        // core 真实返回 unix 秒（数字）：应格式化为 "YYYY-MM-DD HH:MM" 本地时间
        //（不断言具体值以免依赖时区，只断言形状）。
        let v = json!({ "code": "x", "text": "词", "count": 1, "lastUsed": 1_782_000_000i64 });
        let r = WordRow::from_json(WordCategory::Freq, &v);
        assert_eq!(
            r.last_used.len(),
            "2026-06-21 08:00".len(),
            "格式应为 YYYY-MM-DD HH:MM"
        );
        assert!(
            r.last_used.starts_with("20"),
            "应为格式化日期而非原始数字: {}",
            r.last_used
        );
        // 0 / 缺失：占位符 / 空。
        let z = json!({ "code": "x", "text": "词", "count": 1, "lastUsed": 0 });
        assert_eq!(WordRow::from_json(WordCategory::Freq, &z).last_used, "—");
        let none = json!({ "code": "x", "text": "词", "count": 1 });
        assert_eq!(WordRow::from_json(WordCategory::Freq, &none).last_used, "");
    }

    #[test]
    fn parse_list_paged_reads_items_and_total() {
        let v = json!({ "items": [
            { "code": "a", "text": "阿", "weight": 1 },
            { "code": "b", "text": "波", "weight": 2 }
        ], "total": 57 });
        let (rows, total) = WordRow::parse_list(WordCategory::UserDict, true, &v);
        assert_eq!(rows.len(), 2);
        assert_eq!(total, 57, "分页 total 取自 total 字段，非当前页长度");
    }

    #[test]
    fn parse_list_array_total_is_len() {
        let v = json!([
            { "code": "a", "text": "阿", "count": 1 },
            { "code": "b", "text": "波", "count": 2 },
            { "code": "c", "text": "此", "count": 3 }
        ]);
        let (rows, total) = WordRow::parse_list(WordCategory::Temp, false, &v);
        assert_eq!(rows.len(), 3);
        assert_eq!(total, 3);
    }

    #[test]
    fn spec_ops_gate_by_category() {
        // 临时词库无 add/edit，有 promote。
        let temp = spec_for(WordCategory::Temp);
        assert!(!temp.ops.add);
        assert!(!temp.ops.edit);
        assert!(temp.ops.promote);
        assert!(temp.ops.promote_all);

        // 候选调整有 remove_rule + clear + add（shadow.addRule）。
        let shadow = spec_for(WordCategory::Shadow);
        assert!(shadow.ops.remove_rule);
        assert!(shadow.ops.add);
        assert!(!shadow.ops.remove);

        // 用户词库支持加词自动出码。
        let dict = spec_for(WordCategory::UserDict);
        assert!(dict.ops.encode);
        assert_eq!(dict.rpc.encode, "dict.encode");
        assert!(dict.paged && dict.searchable && dict.scoped_by_schema);

        // 系统短语全局、不分页、可开关/恢复默认，无增删。
        let sys = spec_for(WordCategory::PhraseSystem);
        assert!(!sys.scoped_by_schema);
        assert!(sys.ops.set_enabled);
        assert!(sys.ops.reset_default);
        assert!(!sys.ops.add && !sys.ops.remove);

        // 用户短语支持增删改、导入导出、分页。
        let usr = spec_for(WordCategory::PhraseUser);
        assert!(!usr.scoped_by_schema);
        assert!(usr.ops.add && usr.ops.edit && usr.ops.remove && usr.ops.clear);
        assert!(usr.ops.import && usr.ops.export);
        assert!(usr.paged);
    }

    #[test]
    fn logical_tab_maps_title_category_and_phrase_scope() {
        assert_eq!(LogicalTab::PhraseSystem.title(), "系统短语");
        assert_eq!(LogicalTab::PhraseUser.title(), "用户短语");
        assert_eq!(LogicalTab::UserDict.category(), WordCategory::UserDict);
        assert_eq!(LogicalTab::Shadow.category(), WordCategory::Shadow);
        // 短语两级分别映射到对应 WordCategory
        assert_eq!(
            LogicalTab::PhraseSystem.category(),
            WordCategory::PhraseSystem
        );
        assert_eq!(LogicalTab::PhraseUser.category(), WordCategory::PhraseUser);
        assert_eq!(LogicalTab::PhraseSystem.phrase_system(), Some(true));
        assert_eq!(LogicalTab::PhraseUser.phrase_system(), Some(false));
        assert_eq!(LogicalTab::UserDict.phrase_system(), None);
    }

    #[test]
    fn phrase_from_json_reads_is_system() {
        let sys = json!({ "code": "bj", "text": "北京", "isSystem": true });
        assert!(WordRow::from_json(WordCategory::PhraseSystem, &sys).is_system);
        let usr = json!({ "code": "me", "text": "自定义" });
        assert!(
            !WordRow::from_json(WordCategory::PhraseUser, &usr).is_system,
            "缺省 false"
        );
    }

    #[test]
    fn shadow_spec_supports_add() {
        let s = spec_for(WordCategory::Shadow);
        assert!(s.ops.add);
        assert_eq!(s.rpc.add, "shadow.addRule");
        assert!(s.client_search, "候选调整走客户端搜索");
    }

    #[test]
    fn phrase_system_vs_user_specs() {
        let sys = spec_for(WordCategory::PhraseSystem);
        assert!(sys.ops.set_enabled);
        assert!(sys.ops.reset_default);
        assert!(!sys.ops.add && !sys.ops.remove && !sys.ops.clear);
        assert!(!sys.paged);
        assert_eq!(sys.rpc.list, "phrase.listSystem");
        assert_eq!(sys.rpc.reset_default, "phrase.resetSystem");

        let usr = spec_for(WordCategory::PhraseUser);
        assert!(usr.ops.add && usr.ops.edit && usr.ops.remove && usr.ops.clear);
        assert!(usr.ops.import && usr.ops.export);
        assert!(usr.paged);
        assert_eq!(usr.rpc.list, "phrase.listUser");
    }
}
