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
    /// 快捷输入格式（全局；调序 / 停用 / 恢复，出厂条目不可增删）。
    QuickFormat,
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
    QuickFormat,
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
            LogicalTab::QuickFormat => "快捷输入",
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
            LogicalTab::QuickFormat => WordCategory::QuickFormat,
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

    /// 命令行 `--type` 的稳定 id。与 [`Self::title`] 分离：标题是给人看的中文、可随文案调整，
    /// id 是对外契约（用户会写死在快捷方式/脚本里），改动会破坏已有的命令行。
    pub fn cli_id(self) -> &'static str {
        match self {
            LogicalTab::PhraseSystem => "phrase-system",
            LogicalTab::PhraseUser => "phrase-user",
            LogicalTab::UserDict => "user-dict",
            LogicalTab::Temp => "temp",
            LogicalTab::Freq => "freq",
            LogicalTab::Shadow => "shadow",
            LogicalTab::QuickFormat => "quick",
        }
    }

    /// 全部逻辑子标签（`from_cli_id` 与测试穷尽用）。
    pub const ALL: &'static [LogicalTab] = &[
        LogicalTab::PhraseSystem,
        LogicalTab::PhraseUser,
        LogicalTab::UserDict,
        LogicalTab::Temp,
        LogicalTab::Freq,
        LogicalTab::Shadow,
        LogicalTab::QuickFormat,
    ];

    /// 由 `--type` 取值反查（大小写不敏感）。`dict` 是 `user-dict` 的顺手别名。
    pub fn from_cli_id(s: &str) -> Option<LogicalTab> {
        let s = s.trim().to_ascii_lowercase();
        if s == "dict" {
            return Some(LogicalTab::UserDict);
        }
        Self::ALL.iter().copied().find(|t| t.cli_id() == s)
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
    /// 候选调整专用：规则锚定的候选**稳定 id**（`phrase:{code}:{模板原文}`，读 `candId`）。
    /// 其余类别恒空。短语规则靠它跨日命中，也靠它还原出「这条规则管的是哪个短语」。
    pub cand_id: String,
    /// 短语专用：这条用户短语**遮蔽了同码同内容的系统条目**（读 `overridesSystem`）。
    ///
    /// 主程序的短语表主键只有 `(code, text)` 一把，用户建同款时该行转为用户行、系统条目
    /// 从系统短语列表隐去。此位供列表标注来源，并提示「删掉它即恢复系统默认」——
    /// 否则用户看到系统短语少了一条却不知道去哪儿找。
    pub overrides_system: bool,
    /// 快捷输入专用：该条格式的示例效果（core 用示例输入跑真实候选生成，读 `sample`）。
    /// 其余类别恒空。列表里全是 `$YC年$MC月$DC日` 这样的模板，光看认不出哪条是哪条。
    pub sample: String,
    /// 快捷输入专用：该条在**候选**里的 0-based 下标（读 `moveIndex`）。
    ///
    /// ⚠️ 与 [`Self::position`]（列表行号 `displayPos`）**不是一回事**：停用项占着行号却
    /// 不在候选里，拿行号去写移动规则，用户会看到「上移一位跳过好几条」。停用项此处为
    /// `None`，这同时表达了「不能移动，得先启用」。
    pub move_index: Option<usize>,
    /// 快捷输入专用：用户是否调整过（移动或停用，读 `adjusted`）。决定「恢复此条」能不能点。
    pub adjusted: bool,
    /// 快捷输入专用：是不是用户自己加的条目（读 `user`）。
    ///
    /// ⚠️ 与 [`Self::adjusted`] **不是一回事**，行内操作按它分流：出厂条目被调序过也是
    /// `adjusted`，但它只能停用、能「恢复默认」；用户条目反过来——能改模板、能真删，
    /// 而它没有「默认」可回到（「恢复此条」对它无意义，要禁掉）。
    pub user: bool,
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
                overrides_system: b("overridesSystem"),
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
                cand_id: s("candId"),
                position: i("position"),
                kind: s("type"),
                ..Default::default()
            },
            // 快捷输入：`code` 复用为格式 id、`kind` 为格式类别、`position` 为**列表行号**。
            // `moveIndex` 单独存 —— 两个下标的区别见 [`WordRow::move_index`]。
            WordCategory::QuickFormat => WordRow {
                code: s("id"),
                text: s("text"),
                kind: s("kind"),
                position: i("displayPos"),
                enabled: b("enabled"),
                adjusted: b("adjusted"),
                user: b("user"),
                sample: s("sample"),
                move_index: v
                    .get("moveIndex")
                    .and_then(|x| x.as_u64())
                    .map(|n| n as usize),
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

    /// 「词条」列的显示文本：有候选稳定 id 时显示**模板原文**，否则显示 `text` 本身。
    ///
    /// 候选调整里短语规则的 `word` 存的是**最后一次调整时**的候选文本。`date`/`time` 这类
    /// 求值型短语次日即过期——列表写着 `2026/07/30`、候选早已是 `07/31`，看着像条失效的
    /// 僵尸规则（其实工作正常，匹配走的是 id）；同一个 `date` 下多个格式的规则放久了也
    /// 全是旧日期，无从分辨每行管的是哪个。模板原文才是规则真正锚定的东西。
    ///
    /// **`text` 字段本身保持原始 `word` 不动**：编辑与删除规则都按它定位并回传给 core
    /// （`open_shadow_edit` / `do_remove_rule`），换成模板就定位不到了。
    pub fn display_text(&self) -> String {
        phrase_template_of(&self.cand_id).unwrap_or_else(|| self.text.clone())
    }

    /// 某列的展示文本（非交互列用；`Enabled`/`Kind` 由视图特殊渲染，这里给回退文本）。
    pub fn cell_text(&self, field: WordField) -> String {
        match field {
            WordField::Code => self.code.clone(),
            WordField::Text => self.display_text(),
            WordField::Weight => self.weight.to_string(),
            WordField::Count => self.count.to_string(),
            WordField::Position => self.position.to_string(),
            WordField::Enabled => if self.enabled { "是" } else { "否" }.to_string(),
            WordField::Kind => kind_label(&self.kind).to_string(),
            WordField::LastUsed => self.last_used.clone(),
            WordField::Sample => self.sample.clone(),
        }
    }
}

/// 从候选稳定 id 里取出模板原文：`phrase:{code}:{模板}` → `Some(模板)`。
/// 非 `phrase:` 命名空间、缺第三段或模板为空 → `None`（调用方回落到原文本）。
///
/// ⚠️ 取的是**第二个冒号之后的全部内容**（`strip_prefix` + `split_once`），不能按冒号
/// 切成三段取第三段——模板自身常含冒号：`{time("HH:mm:ss")}`、`$CC(...)` 的参数等，
/// 按段取会把模板截断成 `{time("HH`。
fn phrase_template_of(cand_id: &str) -> Option<String> {
    let rest = cand_id.strip_prefix("phrase:")?;
    let (_code, tpl) = rest.split_once(':')?;
    (!tpl.is_empty()).then(|| tpl.to_string())
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

/// 「类型」列的中文标签。
///
/// 候选调整与快捷输入共用这一列，两者取值域**不重叠**（`pin`/`delete` vs
/// `date`/`month_day`/…），故一个函数认两套，不必给 [`WordRow::cell_text`] 多传一个类别参数。
///
/// `pin` 叫「位置」而不是「置顶」：这类规则带一个 `position` 字段，置顶只是 `position = 0`
/// 的那种情形，右键菜单的「前移 / 后移」同样落成 pin 规则。列里写「置顶」会让 position 非 0
/// 的行自相矛盾。添加对话框的类型选项（`dialogs.rs`）用的也是同一套语义。
pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "pin" => "位置",
        "delete" => "删除",
        "date" => "日期",
        "month_day" => "月日",
        "year_month" => "年月",
        "number" => "数字",
        "calc" => "计算",
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
    /// 快捷输入：该条格式的示例效果（core 用示例输入跑真实候选生成得到）。
    Sample,
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
            WordField::Sample => "sample",
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
    /// 是否支持「转为用户短语」（系统短语）：原样复制为用户短语后即可编辑。
    /// 走 `phrase.add` 撞键转归属，不另设 RPC（见 `DictManagerState::claim_phrase_as_user`）。
    ///
    /// 入口在**只读查看对话框**里，不在列表操作列——系统短语行已有「启用开关 + 查看」，
    /// 默认列宽（`ACTIONS_WEIGHT`）再加一枚按钮就挤爆了。本标志因此只作为能力声明，
    /// 由 `build_view_dialog` 消费。
    pub claim_as_user: bool,
    /// 是否支持**行内调序**（上移 / 下移 / 置顶）。
    ///
    /// windui 没有拖拽重排控件，故用一对小按钮；点击时传的是
    /// [`WordRow::move_index`]（候选内下标）而不是行号。
    pub reorder: bool,
    /// 是否支持「恢复此条默认」（快捷输入：清掉这一条的调序与停用）。
    ///
    /// 与 `reset_default`（整类/整表恢复）分开：出厂条目改坏了，用户想恢复的通常是
    /// 手上这一条，而不是把整类的排序偏好全丢掉。
    pub reset_entry: bool,
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
        claim_as_user: false,
        reorder: false,
        reset_entry: false,
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
    /// 移到指定位置（快捷输入：`quick.move`，参数 kind/id/index）。
    pub move_to: &'static str,
    /// 恢复单条默认（快捷输入：`quick.resetEntry`）。
    pub reset_entry: &'static str,
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
        move_to: "",
        reset_entry: "",
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
        claim_as_user: true,
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

// 快捷输入：不显示格式 id（`date.cn` 这种对用户没有识别价值，认条目靠「示例」那一列），
// 省下的宽度给示例。id 仍可从行右键菜单里复制。
// ⚠️ 这四个权重（含 `actions_weight` 给的操作列）是**一组配比**，不是各自独立的宽度：
// 实际宽度 = 本列权重 / 总权重。动其中任何一个都会挤到其余所有列——操作列为装下
// 用户条目的五个控件从 1.5 提到 2.9 时，类别列数值没变，占比却从 12.7% 掉到 10.1%，
// 「年月」两个汉字就折行了。改任一列前先把四个数加起来算占比。
// 现行配比合计 7.3：类别 14% / 格式 25% / 示例 25% / 操作 37%。
const COLS_QUICK: &[ColumnSpec] = &[
    ColumnSpec {
        header: "类别",
        // 内容恒是两个汉字（日期/月日/年月/数字/计算），够放不折行即可。
        weight: 1.0,
        field: WordField::Kind,
    },
    ColumnSpec {
        header: "格式",
        weight: 1.8,
        field: WordField::Text,
    },
    ColumnSpec {
        header: "示例",
        weight: 1.8,
        field: WordField::Sample,
    },
];

/// 快捷输入格式表。
///
/// 全局（不按方案）：core 那张调整表的键只到格式类别，不带方案。
///
/// **增删只对用户自己的条目**：`kind` 是解析器白名单（不可自创），出厂条目的模板也不可改
/// ——想要别的写法就停用出厂那条、自己加一条。故 `add`/`edit`/`remove` 三个开关为真，
/// 但每一行能不能用要看 [`WordRow::user`]（见 `row_ops`），不是整类一刀切。
///
/// 客户端搜索：条目总数二十余条，一次全取，服务端搜索反而多一轮 RPC。
const SPEC_QUICK: CategorySpec = CategorySpec {
    category: WordCategory::QuickFormat,
    title: "快捷输入",
    scoped_by_schema: false,
    paged: false,
    searchable: false,
    client_search: true,
    columns: COLS_QUICK,
    ops: OpFlags {
        set_enabled: true,
        reorder: true,
        reset_entry: true,
        // 整表恢复（逐类调 quick.resetKind，见 `do_reset_default`）。
        reset_default: true,
        import: true,
        export: true,
        // 三个「按行才知道能不能用」的能力：出厂条目一律不可编辑/删除。
        add: true,
        edit: true,
        remove: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        list: "quick.list",
        set_enabled: "quick.setEnabled",
        move_to: "quick.move",
        reset_entry: "quick.resetEntry",
        // 整类恢复；「整表」由状态层对每个类别各调一次。
        reset_default: "quick.resetKind",
        add: "quick.add",
        // 改的是**模板**，故落在 update 上；出厂条目会被 core 拒绝。
        update: "quick.setText",
        remove: "quick.delete",
        ..RpcNames::EMPTY
    },
};

/// 这一行实际可用的操作。
///
/// ★ 存在的理由：快捷输入是本页第一个「同一类别里，两种行有不同权限」的域。
/// [`CategorySpec::ops`] 是**类别级**的能力声明（这一类有没有这个功能），到了行级还要
/// 再过一道——否则用户会看到出厂条目上亮着「删除」，点下去收一句 core 的报错。
///
/// 其余类别没有行级差异，原样返回。
pub fn row_ops(cat: WordCategory, row: &WordRow) -> OpFlags {
    let base = spec_for(cat).ops;
    match cat {
        WordCategory::QuickFormat => OpFlags {
            // 出厂条目：只能停用/调序/恢复默认
            edit: row.user,
            remove: row.user,
            // 用户条目没有「默认」可回到——「恢复此条」对它无意义
            reset_entry: base.reset_entry && !row.user && row.adjusted,
            ..base
        },
        _ => base,
    }
}

impl CategorySpec {
    /// 操作列宽度权重。默认值只够「启用开关 + 两枚小按钮」。
    ///
    /// 快捷输入要宽一档：它的**用户条目**上最多同时出现五个控件——启用开关、↑、↓、
    /// 编辑、删除。按默认宽度排，后面的按钮会被挤出可视区（列不会自动加宽，只会截断），
    /// 用户看到的是「有些行的删除按钮不见了」。
    ///
    /// 出厂条目只有四个（无编辑无删除，见 `row_ops`），但列宽是**整列**的，
    /// 必须按最宽的那种行算。
    pub fn actions_weight(&self) -> f32 {
        match self.category {
            // 与 COLS_QUICK 的三个权重是一组配比（见那里的 ⚠️）：合计 7.3，
            // 操作列占约 37%，恰好排下五个控件而不挤压类别列。
            WordCategory::QuickFormat => 2.7,
            _ => 1.5,
        }
    }
}

/// 取某类别的静态描述。
pub fn spec_for(cat: WordCategory) -> &'static CategorySpec {
    match cat {
        WordCategory::PhraseSystem => &SPEC_PHRASE_SYSTEM,
        WordCategory::PhraseUser => &SPEC_PHRASE_USER,
        WordCategory::UserDict => &SPEC_USERDICT,
        WordCategory::Temp => &SPEC_TEMP,
        WordCategory::Freq => &SPEC_FREQ,
        WordCategory::Shadow => &SPEC_SHADOW,
        WordCategory::QuickFormat => &SPEC_QUICK,
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
        // 这条夹具的 position 是 1 而非 0——pin 规则并不等同于置顶，故类型列写「位置」。
        assert_eq!(kind_label(&r.kind), "位置");

        let del = json!({ "code": "cd", "word": "屏", "candId": null, "type": "delete" });
        let r2 = WordRow::from_json(WordCategory::Shadow, &del);
        assert_eq!(r2.kind, "delete");
        assert_eq!(kind_label(&r2.kind), "删除");
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

    /// 快捷输入行解析：`code` ← id、`position` ← 列表行号、`moveIndex` 单独存。
    ///
    /// ★ 两个下标必须**各归各位**：把 `displayPos` 当成 `move_index` 用，就是
    /// 「上移一位跳过好几条」。
    #[test]
    fn quick_format_from_json_keeps_two_indices_apart() {
        let v = json!({
            "kind": "date", "id": "date.lunar", "text": "农历$LMD",
            "displayPos": 6, "moveIndex": 5, "enabled": true, "adjusted": true,
            "sample": "农历七月初五"
        });
        let r = WordRow::from_json(WordCategory::QuickFormat, &v);
        assert_eq!(r.code, "date.lunar", "code 复用为格式 id");
        assert_eq!(r.text, "农历$LMD");
        assert_eq!(r.kind, "date");
        assert_eq!(r.position, 6, "position = 列表行号");
        assert_eq!(r.move_index, Some(5), "move_index = 候选内下标");
        assert_eq!(r.sample, "农历七月初五");
        assert!(r.enabled && r.adjusted);
        // 「类别」列显示中文
        assert_eq!(r.cell_text(WordField::Kind), "日期");
        assert_eq!(r.cell_text(WordField::Sample), "农历七月初五");
    }

    /// 停用行：`moveIndex` 为 null → `None`（表示"不能移动，得先启用"），
    /// 而行号照常有值——两者不可互相兜底。
    #[test]
    fn quick_format_disabled_row_has_no_move_index() {
        let v = json!({
            "kind": "number", "id": "number.digits", "text": "$DIG",
            "displayPos": 4, "moveIndex": null, "enabled": false, "adjusted": true, "sample": ""
        });
        let r = WordRow::from_json(WordCategory::QuickFormat, &v);
        assert_eq!(r.move_index, None);
        assert_eq!(r.position, 4, "行号仍在");
        assert!(!r.enabled);
        // 示例可以为空（农历超范围、表达式写错都会这样），不代表这条永远不出
        assert_eq!(r.sample, "");
    }

    /// 「类型」列被候选调整与快捷输入共用，两套取值域都要认。
    #[test]
    fn kind_label_covers_both_shadow_and_quick_kinds() {
        assert_eq!(kind_label("pin"), "位置");
        assert_eq!(kind_label("delete"), "删除");
        for (k, want) in [
            ("date", "日期"),
            ("month_day", "月日"),
            ("year_month", "年月"),
            ("number", "数字"),
            ("calc", "计算"),
        ] {
            assert_eq!(kind_label(k), want, "kind={k}");
        }
        assert_eq!(kind_label("weather"), "—", "未知类别给占位符而不是空白");
    }

    /// 快捷输入的能力声明：可调序 / 可停用 / 可恢复 / 可增删改（后三者按行再过一道）。
    ///
    /// **没有 clear**：清空会连出厂条目一起没了，而它们不可删——这一类没有「清空」的语义。
    #[test]
    fn quick_format_spec_allows_reorder_and_user_entries() {
        let q = spec_for(WordCategory::QuickFormat);
        assert!(q.ops.reorder && q.ops.set_enabled && q.ops.reset_entry);
        assert!(q.ops.import && q.ops.export);
        assert!(q.ops.add && q.ops.edit && q.ops.remove, "自定义条目");
        assert!(!q.ops.clear, "出厂条目不可删，故没有「清空」");
        assert!(!q.scoped_by_schema, "全局，不按方案");
        assert!(!q.paged, "二十余条，一次全取");
        assert!(q.client_search, "客户端过滤，省一轮 RPC");
        assert_eq!(q.rpc.list, "quick.list");
        assert_eq!(q.rpc.move_to, "quick.move");
        assert_eq!(q.rpc.set_enabled, "quick.setEnabled");
        assert_eq!(q.rpc.reset_entry, "quick.resetEntry");
        assert_eq!(q.rpc.reset_default, "quick.resetKind");
        assert_eq!(q.rpc.add, "quick.add");
        assert_eq!(q.rpc.update, "quick.setText", "改的是模板");
        assert_eq!(q.rpc.remove, "quick.delete");
    }

    /// ★★ 行级能力：出厂条目**只能停用**，用户条目**不能「恢复默认」**。
    ///
    /// 这是本页第一个「同类别里两种行权限不同」的域。类别级 `ops` 声明的是「这一类有没有
    /// 这个功能」，到了行级还要再过一道——否则出厂条目上会亮着「删除」，点下去收一句
    /// core 的报错，而用户完全不明白为什么。
    #[test]
    fn row_ops_split_factory_and_user_entries() {
        let factory = WordRow {
            kind: "date".into(),
            code: "date.cn".into(),
            adjusted: true,
            user: false,
            ..Default::default()
        };
        let mine = WordRow {
            kind: "date".into(),
            code: "date.u1".into(),
            adjusted: true,
            user: true,
            ..Default::default()
        };
        let f = row_ops(WordCategory::QuickFormat, &factory);
        assert!(!f.edit, "出厂模板不可改");
        assert!(!f.remove, "出厂条目只能停用");
        assert!(f.reset_entry, "调整过，可恢复默认");
        assert!(f.set_enabled && f.reorder, "停用与调序两类都能做");

        let u = row_ops(WordCategory::QuickFormat, &mine);
        assert!(u.edit && u.remove, "自己的条目可改可删");
        assert!(!u.reset_entry, "★ 用户条目没有「默认」可回到");
        assert!(u.set_enabled && u.reorder);

        // 没调整过的出厂条目：没什么可恢复的
        let untouched = WordRow {
            kind: "date".into(),
            code: "date.iso".into(),
            ..Default::default()
        };
        assert!(!row_ops(WordCategory::QuickFormat, &untouched).reset_entry);
    }

    /// 操作列宽度：快捷输入必须比默认宽——它的用户条目上有五个控件
    /// （启用 + ↑ + ↓ + 编辑 + 删除），按默认宽度排后面的按钮会被挤出可视区，
    /// 症状是「有些行的删除按钮不见了」。
    #[test]
    fn quick_format_actions_column_is_wider() {
        let quick = spec_for(WordCategory::QuickFormat).actions_weight();
        let other = spec_for(WordCategory::PhraseUser).actions_weight();
        assert!(
            quick > other,
            "快捷输入的操作列要宽一档: {quick} vs {other}"
        );
        // 其余类别保持默认，不该被这次改动波及
        for cat in [
            WordCategory::PhraseSystem,
            WordCategory::UserDict,
            WordCategory::Temp,
            WordCategory::Freq,
            WordCategory::Shadow,
        ] {
            assert_eq!(spec_for(cat).actions_weight(), other, "cat={cat:?}");
        }
    }

    /// 其余类别没有行级差异，`row_ops` 原样返回类别声明——加一层过滤不该悄悄改变它们。
    #[test]
    fn row_ops_is_transparent_for_other_categories() {
        let row = WordRow {
            code: "abc".into(),
            text: "词".into(),
            ..Default::default()
        };
        for cat in [
            WordCategory::PhraseSystem,
            WordCategory::PhraseUser,
            WordCategory::UserDict,
            WordCategory::Temp,
            WordCategory::Freq,
            WordCategory::Shadow,
        ] {
            let base = spec_for(cat).ops;
            let got = row_ops(cat, &row);
            assert_eq!(got.edit, base.edit, "cat={cat:?} edit");
            assert_eq!(got.remove, base.remove, "cat={cat:?} remove");
            assert_eq!(got.reset_entry, base.reset_entry, "cat={cat:?} reset_entry");
        }
    }

    /// 声明了某个 op 就必须有对应的 RPC 方法名，否则点了只会得到「该操作不可用」。
    #[test]
    fn declared_ops_have_rpc_methods() {
        for cat in [
            WordCategory::PhraseSystem,
            WordCategory::PhraseUser,
            WordCategory::UserDict,
            WordCategory::Temp,
            WordCategory::Freq,
            WordCategory::Shadow,
            WordCategory::QuickFormat,
        ] {
            let s = spec_for(cat);
            assert!(!s.rpc.list.is_empty(), "{cat:?} 缺 list RPC");
            if s.ops.reorder {
                assert!(
                    !s.rpc.move_to.is_empty(),
                    "{cat:?} 声明了 reorder 却没有 move RPC"
                );
            }
            if s.ops.reset_entry {
                assert!(
                    !s.rpc.reset_entry.is_empty(),
                    "{cat:?} 声明了 reset_entry 却没有对应 RPC"
                );
            }
            if s.ops.set_enabled {
                assert!(!s.rpc.set_enabled.is_empty(), "{cat:?} 缺 setEnabled RPC");
            }
        }
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

    /// CLI id 是对外契约：既要能往返，也要钉死具体字符串——改了就会破坏用户已有的命令行。
    #[test]
    fn logical_tab_cli_id_round_trips() {
        for t in LogicalTab::ALL.iter().copied() {
            assert_eq!(LogicalTab::from_cli_id(t.cli_id()), Some(t));
        }
        assert_eq!(LogicalTab::Shadow.cli_id(), "shadow");
        assert_eq!(LogicalTab::UserDict.cli_id(), "user-dict");
        // 别名与大小写/空白
        assert_eq!(LogicalTab::from_cli_id("dict"), Some(LogicalTab::UserDict));
        assert_eq!(LogicalTab::from_cli_id("SHADOW"), Some(LogicalTab::Shadow));
        assert_eq!(LogicalTab::from_cli_id(" freq "), Some(LogicalTab::Freq));
        // 中文标题不是 id
        assert_eq!(LogicalTab::from_cli_id("候选调整"), None);
        assert_eq!(LogicalTab::from_cli_id("nope"), None);
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

    /// 用户短语的 `overridesSystem`：该行遮蔽了同码同内容的系统条目（那条已从系统短语
    /// 列表隐去）。列表据此标「覆盖」，不解析的话用户只会觉得系统短语莫名少了一条。
    #[test]
    fn phrase_from_json_reads_overrides_system() {
        let shadowing = json!({
            "code": "date", "text": "$Y年$M月$D日", "isSystem": false, "overridesSystem": true
        });
        let row = WordRow::from_json(WordCategory::PhraseUser, &shadowing);
        assert!(row.overrides_system, "遮蔽标记须解析");
        assert!(!row.is_system, "归属是用户");

        let plain = json!({ "code": "me", "text": "自定义" });
        assert!(
            !WordRow::from_json(WordCategory::PhraseUser, &plain).overrides_system,
            "缺省 false"
        );
    }

    /// 候选调整里短语规则显示**模板原文**而非当日求值文本。
    ///
    /// 规则的 `word` 是最后一次调整时的候选文本，`date` 这类次日即过期——列表写着
    /// `2026/07/30`、候选已是 `07/31`，看着像条失效的僵尸规则。同时 `text` 字段本身
    /// 必须保持原值不动（编辑/删除按它定位）。
    #[test]
    fn shadow_row_displays_phrase_template() {
        let v = json!({
            "code": "date", "word": "2026/07/30", "type": "pin", "position": 0,
            "candId": "phrase:date:$Y/$MM/$DD"
        });
        let row = WordRow::from_json(WordCategory::Shadow, &v);
        assert_eq!(row.text, "2026/07/30", "text 保持规则原值（写操作依赖）");
        assert_eq!(row.display_text(), "$Y/$MM/$DD", "显示模板原文");
        assert_eq!(row.cell_text(WordField::Text), "$Y/$MM/$DD");
    }

    /// 无 candId 的规则（手动添加 / 码表候选）照旧显示 word 本身。
    #[test]
    fn shadow_row_without_cand_id_displays_word() {
        let v = json!({ "code": "aaaa", "word": "工", "type": "pin", "position": 0 });
        let row = WordRow::from_json(WordCategory::Shadow, &v);
        assert_eq!(row.display_text(), "工");
    }

    /// ⚠️ 模板自身含冒号时不得被截断——时间格式 `HH:mm:ss` 是最常见的一例。
    /// 按冒号切段取第三段会得到 `{time("HH`。
    #[test]
    fn phrase_template_keeps_colons_inside_template() {
        assert_eq!(
            phrase_template_of("phrase:time:{time(\"HH:mm:ss\")}").as_deref(),
            Some("{time(\"HH:mm:ss\")}")
        );
        // 非 phrase 命名空间 / 缺段 / 空模板 → None（调用方回落原文本）
        assert!(phrase_template_of("dict:abc:x").is_none());
        assert!(phrase_template_of("phrase:onlycode").is_none());
        assert!(phrase_template_of("phrase:date:").is_none());
        assert!(phrase_template_of("").is_none());
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
