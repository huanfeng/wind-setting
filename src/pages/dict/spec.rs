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
    /// 用户分库（按方案·分页，增删改同主词库，写的是分库文件）。不是独立的类别入口：中栏
    /// 「用户词库」分组下选中某个分库时的类别，见 `user_libs`。
    UserLib,
    /// 临时词库（按方案·分页）。
    Temp,
    /// 词频（按方案·分页）。
    Freq,
    /// 候选调整 / 影子规则（按方案·分页，只读 + 撤销）。
    Shadow,
    /// 快捷输入格式（全局；调序 / 停用 / 恢复，出厂条目不可增删）。
    QuickFormat,
    /// 常用字覆盖（全局；只列**用户改过的字**，出厂那 8104 字不进列表）。
    CommonChars,
    /// 邮箱后缀学习（全局；只读 + 逐条删 + 清空）。
    ///
    /// 记的是 `@` **之后**的域名（`qq.com`），不含用户名——所以它与网址历史虽然同属
    /// 「输入补全」这个域，隐私量级并不一样：core 那边前者跟随邮箱模式开关，后者另有
    /// 一道 `input.url.history_enabled`。
    MailSuffix,
    /// 网址输入历史（全局；只读 + 逐条删 + 清空）。
    ///
    /// 出厂**不产生**任何数据（`input.url.history_enabled` 默认关），所以多数用户来到
    /// 这个标签只会看到一张空表，那是对的。
    UrlHistory,
    /// 快捷输入历史（全局；只读 + 逐条删 + 清空）。
    ///
    /// 与网址历史同一张 core 表的第三个分区。出厂同样**不产生**数据：只有用户在「快捷
    /// 候选来源」里勾上 `quick_input.history` 之后，快捷输入里上屏的字面文本才会记下来。
    QuickHistory,
    /// 字符集分类（全局；一行一个字符类，改的是**整个类**的属性）。
    ///
    /// ★ 与 [`Self::CommonChars`] 是同一件事的两层，差别必须让用户看见：
    ///
    /// | | 本类别 | 常用字 |
    /// |---|---|---|
    /// | 作用面 | 整个类，不问词库里有没有 | 一个字 |
    /// | 跟随出厂更新 | ✅ 出厂给类补了成员一并生效 | ⛔ 只有当时那些 |
    /// | 优先级 | 低 | **高**，压住前者 |
    ///
    /// 最后一行是「配了没反应」的现成来源，故列表要显示每个类被多少条逐条覆盖压着。
    Charsets,
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
    CommonChars,
    Charsets,
    MailSuffix,
    UrlHistory,
    QuickHistory,
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
            LogicalTab::CommonChars => "常用字",
            // 域名是「字符集」，tab 叫「字符分类」，两层各有其名。
            LogicalTab::Charsets => "字符分类",
            LogicalTab::MailSuffix => "邮箱后缀",
            LogicalTab::UrlHistory => "网址历史",
            LogicalTab::QuickHistory => "快捷输入历史",
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
            LogicalTab::CommonChars => WordCategory::CommonChars,
            LogicalTab::Charsets => WordCategory::Charsets,
            LogicalTab::MailSuffix => WordCategory::MailSuffix,
            LogicalTab::UrlHistory => WordCategory::UrlHistory,
            LogicalTab::QuickHistory => WordCategory::QuickHistory,
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
            LogicalTab::CommonChars => "common-chars",
            LogicalTab::Charsets => "charsets",
            LogicalTab::MailSuffix => "mail-suffix",
            LogicalTab::UrlHistory => "url-history",
            LogicalTab::QuickHistory => "quick-history",
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
        LogicalTab::CommonChars,
        LogicalTab::Charsets,
        LogicalTab::MailSuffix,
        LogicalTab::UrlHistory,
        LogicalTab::QuickHistory,
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
    /// 常用字专用：所属 Unicode 块的中文名（读 `block`），如「注音符号」。其余类别恒空。
    pub block: String,
    /// 常用字专用：块的码位范围（读 `blockRange`），如 `3100-312F`。整类操作的确认文案要用。
    pub block_range: String,
    /// 常用字专用：这一类能不能整类批量设常用/生僻（读 `blockBulkEditable`）。
    ///
    /// ⛔ 汉字块恒 `false`：列表里 8104 个默认字全是汉字，对着一行「我」点「将『基本汉字』
    /// 全部设为生僻」，一次误点就是七千多条覆盖。core 也会拒绝，这里据此**灰显**——
    /// 让一个注定失败的操作可点，用户只会以为功能坏了。
    pub block_bulk_editable: bool,
    /// 快捷输入专用：是不是用户自己加的条目（读 `user`）。
    ///
    /// ⚠️ 与 [`Self::adjusted`] **不是一回事**，行内操作按它分流：出厂条目被调序过也是
    /// `adjusted`，但它只能停用、能「恢复默认」；用户条目反过来——能改模板、能真删，
    /// 而它没有「默认」可回到（「恢复此条」对它无意义，要禁掉）。
    pub user: bool,
    /// 常用字专用：**用户设定**的判定（读 `common`）。`text` 存那个字本身。
    pub common_now: bool,
    /// 常用字专用：**出厂**判定（读 `baseCommon`）。
    ///
    /// 两个都要有，列表才能显示「出厂：常用 → 现在：生僻」这层对照。只给现在的话，
    /// 用户看到一行「的 · 生僻」根本不知道自己当初改的是什么、改之前是哪一边。
    pub common_base: bool,
    /// 常用字专用：**决定这个字常用性的那个字符类**（读 `class`）。
    ///
    /// 与 [`Self::block`] 不是一回事：块决定「整类批量」扫哪个码位区间，类决定
    /// 「改类的 default」影响谁。两个都显示，用户才知道该去哪一层改。
    pub class: String,
    /// 字符集分类专用：**仲裁优先级**（读 `order`）。数字小的先判定。
    ///
    /// ★ 只管「默认判定」这一件事：`noFreq` / `inRare` 是**并集**语义（任一命中的类
    /// 为真即真），与优先级无关。列头旁那句说明必须把这条说出来，否则用户会以为调
    /// 优先级能改免词频的生效顺序——那个顺序不存在。
    ///
    /// `None` = RPC 没给（只出现在测试夹具里，core 恒返回）。⛔ 不折成 0：0 是合法
    /// 且**最高**的优先级，缺席显示成 0 会让人以为这个类压过所有出厂类。
    pub class_order: Option<i32>,
    /// 字符集分类专用：本类的常用性表态（读 `default`）。`None` = **不表态**。
    ///
    /// ⚠️ 三态不能压成两态：「不表态」跟随出厂更新，「显式设成常用」会钉死。
    /// 压成 bool 之后，用户再也回不到第一种。
    pub class_default: Option<bool>,
    /// 字符集分类专用：免词频（读 `noFreq`）。
    pub class_no_freq: bool,
    /// 字符集分类专用：纳入生僻字模式（读 `inRare`）。
    pub class_in_rare: bool,
    /// 字符集分类专用：**有多少条逐条覆盖压在这个类上**（读 `overrideCount`）。
    ///
    /// ★ 大于 0 时界面必须提示：用户曾整类点过生僻的话，改本类的「默认」会一点反应
    /// 都没有——逐条覆盖优先级更高，全压着。
    pub class_override_count: usize,
    /// 字符集分类专用：本类的成员/范围摘要（码位段 + 离散成员数）。
    pub class_scope: String,
    /// 字符集分类专用：**出厂类**（读 `builtin`）。出厂类不能删、范围只读，只能
    /// 「恢复默认」；反之是用户自建类，右键有「删除」。
    pub class_builtin: bool,
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
            WordCategory::UserDict | WordCategory::UserLib => WordRow {
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
                last_used: fmt_last_used(v.get("lastUsed")),
                ..Default::default()
            },
            WordCategory::Freq => WordRow {
                code: s("code"),
                text: s("text"),
                count: i("count"),
                last_used: fmt_last_used(v.get("lastUsed")),
                ..Default::default()
            },
            // 补全学习数据：没有「编码」这个概念，只有文本本身 + 用过几次 + 何时用的。
            WordCategory::MailSuffix | WordCategory::UrlHistory | WordCategory::QuickHistory => {
                WordRow {
                    text: s("text"),
                    count: i("count"),
                    last_used: fmt_last_used(v.get("lastUsed")),
                    ..Default::default()
                }
            }
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
            // 常用字：`text` 就是那个字（主键），两个 bool 分别是现在与出厂的判定。
            // 没有 `code`——这张表的键不带编码，也不带方案。
            WordCategory::CommonChars => WordRow {
                text: s("char"),
                common_now: b("common"),
                common_base: b("baseCommon"),
                // 复用 `adjusted`（与快捷输入同义：用户改过没有）。全表里绝大多数行都是
                // false，「恢复默认」靠它只对改过的行放行。
                adjusted: b("adjusted"),
                block: s("block"),
                block_range: s("blockRange"),
                block_bulk_editable: b("blockBulkEditable"),
                class: s("class"),
                ..Default::default()
            },
            // 字符集分类：`text` 存显示名，`code` 存 key（写回 RPC 用的是 key）。
            WordCategory::Charsets => WordRow {
                text: s("name"),
                code: s("key"),
                class_order: v.get("order").and_then(|x| x.as_i64()).map(|x| x as i32),
                // ⚠️ 三态：字段缺席或为 null 都是「不表态」，与 false 不同。
                class_default: v.get("default").and_then(|x| x.as_bool()),
                class_no_freq: b("noFreq"),
                class_in_rare: b("inRare"),
                class_override_count: v.get("overrideCount").and_then(|x| x.as_u64()).unwrap_or(0)
                    as usize,
                class_scope: charset_scope_text(v),
                class_builtin: b("builtin"),
                enabled: v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true),
                adjusted: b("overridden"),
                ..Default::default()
            },
        }
    }

    /// 从列表 RPC 返回解析出 (行集合, 总数)。
    /// 分页类返回 `{items,total}`；其余返回裸数组，total = 长度。
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

    /// 字符类身上那几个标记的短文案。
    ///
    /// ⚠️ 文案要短：它与范围挤在同一列里，「生僻字模式」那样的长词会把整列顶到换行。
    fn class_flag_labels(&self) -> Vec<&'static str> {
        let mut f = Vec::new();
        if self.class_no_freq {
            f.push("免词频");
        }
        if self.class_in_rare {
            f.push("生僻模式");
        }
        if !self.enabled {
            f.push("已停用");
        }
        f
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
            WordField::CommonNow => common_label(self.common_now).to_string(),
            WordField::CommonBase => common_label(self.common_base).to_string(),
            WordField::Block => self.block.clone(),
            WordField::Class => self.class.clone(),
            // 缺席给占位符而不是 0——见 `class_order` 的字段注释。
            WordField::ClassOrder => match self.class_order {
                Some(n) => n.to_string(),
                None => "—".to_string(),
            },
            // 三态：`None` 是「跟随出厂」，与显式设成常用不是一回事——前者跟着出厂走，
            // 后者钉死。压成两态之后用户再也回不到第一种。
            WordField::ClassDefault => match self.class_default {
                Some(v) => common_label(v).to_string(),
                // 「未设置」而不是「跟随默认」：本页那一列的表头就是「默认判定」，
                // 值再叫「默认」会绕回自己。它说的是**用户没给这个类设过**——
                // 判定因此由自带的那份配置决定，将来自带的改了它也跟着改。
                None => "未设置".to_string(),
            },
            // 范围 + 标记合成一列（见 `COLS_CHARSETS` 里那条注释）。
            WordField::ClassScope => {
                let mut parts = Vec::new();
                if self.class_scope != "—" {
                    parts.push(self.class_scope.clone());
                }
                parts.extend(self.class_flag_labels().into_iter().map(str::to_string));
                if parts.is_empty() {
                    "—".to_string()
                } else {
                    parts.join(" · ")
                }
            }
            WordField::ClassFlags => {
                let f = self.class_flag_labels();
                if f.is_empty() {
                    "—".to_string()
                } else {
                    f.join(" · ")
                }
            }
            // 0 条不显示「0」：这一列是异常提示，恒显示数字会让人以为它总该有值。
            WordField::ClassOverrides => match self.class_override_count {
                0 => "—".to_string(),
                n => format!("{n} 条"),
            },
        }
    }
}

/// 常用性的显示文案。两列共用一个函数，避免「出厂」列写「常用」而「现在」列写「常用字」
/// 这种同义不同形的不一致。
pub fn common_label(common: bool) -> &'static str {
    if common { "常用" } else { "生僻" }
}

/// 对话框里「判为」选择器的两个选项，**下标即 `common_choice` 的取值**（0 = 常用）。
///
/// 文案与列表列同源（[`common_label`]）而不另写一份：同一个概念在列表里叫「常用」、
/// 在对话框里叫「常用字」，用户会以为它们是两回事。
pub fn common_choices() -> Vec<String> {
    vec![
        common_label(true).to_string(),
        common_label(false).to_string(),
    ]
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
    /// 常用字：**现在**的判定（用户设定的那一边）。
    CommonNow,
    /// 常用字：**出厂**的判定。与 [`Self::CommonNow`] 并排显示，用户才看得出改了什么。
    CommonBase,
    /// 常用字：所属 Unicode 块的中文名（「注音符号」「表意文字描述符」）。
    ///
    /// 光看字形分不清这些东西是什么——WindInput issue #83 的用户为此把整张码表喂给 AI
    /// 分类、再手工逐个试，才弄明白哪些会显示哪些不会。这一列就是把那份工作内建进来。
    Block,
    /// 常用字：决定它常用性的那个字符类。
    Class,
    /// 字符集分类：本类的仲裁优先级（`order`，数字小的先判定）。
    ClassOrder,
    /// 字符集分类：本类的常用性表态（常用 / 生僻 / 跟随出厂）。
    ClassDefault,
    /// 字符集分类：免词频 + 生僻模式两个开关的合并展示。
    ClassFlags,
    /// 字符集分类：范围摘要（码位段 + 成员数）。
    ClassScope,
    /// 字符集分类：压在本类上的逐条覆盖数。
    ClassOverrides,
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
            WordField::CommonNow => "common",
            WordField::CommonBase => "baseCommon",
            WordField::Block => "block",
            WordField::Class => "class",
            WordField::ClassOrder => "order",
            WordField::ClassDefault => "default",
            WordField::ClassScope => "memberCount",
            WordField::ClassFlags => "noFreq",
            WordField::ClassOverrides => "overrideCount",
        }
    }

    /// 客户端排序时是否按数值比较（其余按字符串）。
    ///
    /// ⚠️ 优先级必须在这里：自带取值是 10 / 20 / 100 / 900，按字符串排会得到
    /// 「10 100 20 900」——而这一列的全部意义就是让人看出先后。
    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            WordField::Weight | WordField::Count | WordField::Position | WordField::ClassOrder
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
    /// 是否支持「复制为一键导入文本」（短语两级）。
    ///
    /// 只对短语开：这个格式（`wind:p1`，见 `pages::phrase_import`）只承载 `code` 与
    /// `text` 二元组，词库/词频/候选调整那几类的 weight、position、type、cand_id 全部
    /// 表达不了——给它们开等于生成一串**看着像、装进去缺一半**的文本。
    pub share_text: bool,
    /// 是否支持「恢复此条默认」（快捷输入：清掉这一条的调序与停用）。
    ///
    /// 与 `reset_default`（整类/整表恢复）分开：出厂条目改坏了，用户想恢复的通常是
    /// 手上这一条，而不是把整类的排序偏好全丢掉。
    pub reset_entry: bool,
    /// 是否支持**多选删除**（t63）：编码列前挂勾选框，工具栏出「本页全选 / 删除选中」。
    ///
    /// 删除逐条走 [`DictManagerState::delete_call`]，与行内删除同一条路，故对任何
    /// `remove` 可用的类别都成立；目前只给临时词库开（反馈就是冲它来的）。
    ///
    /// [`DictManagerState::delete_call`]: super::state::DictManagerState::delete_call
    pub multi_select: bool,
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
        share_text: false,
        multi_select: false,
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

// 「最近使用」= 词频表里的最近选中时刻与加入时间取大者（core `web_temp_list_paged`），
// 供 t63「筛出长期没再输入的」——按它升序排，最久没用的排最前。
const COLS_TEMP: &[ColumnSpec] = &[
    ColumnSpec {
        header: "编码",
        weight: 1.0,
        field: WordField::Code,
    },
    ColumnSpec {
        header: "词条",
        weight: 1.8,
        field: WordField::Text,
    },
    ColumnSpec {
        header: "次数",
        weight: 1.0,
        field: WordField::Count,
    },
    ColumnSpec {
        header: "最近使用",
        weight: 1.2,
        field: WordField::LastUsed,
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
        share_text: true,
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
        share_text: true,
        multi_select: true,
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
    // 入口在右栏标题行（见 view::build_page_for），故此处不设 import/export。
    ops: OpFlags {
        add: true,
        edit: true,
        remove: true,
        clear: true,
        encode: true,
        multi_select: true,
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

/// 用户分库：同用户词库的列与操作（增删改、清空、多选），都按行补丁改分库文件。
/// 导入导出不在这里：分库的「导入」是中栏的「从文件导入为分库」，导出就是拷贝文件。
const SPEC_USERLIB: CategorySpec = CategorySpec {
    category: WordCategory::UserLib,
    title: "用户分库",
    scoped_by_schema: true,
    paged: true,
    searchable: true,
    client_search: false,
    columns: COLS_USERDICT,
    ops: OpFlags {
        add: true,
        edit: true,
        remove: true,
        clear: true,
        encode: true,
        multi_select: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        list: "userDicts.entries",
        // 加与改同一个接口：按 (词, 编码) upsert，改权重也走它。
        add: "userDicts.addEntry",
        update: "userDicts.addEntry",
        remove: "userDicts.removeEntry",
        clear: "userDicts.clear",
        encode: "dict.encode",
        ..RpcNames::EMPTY
    },
};

/// 临时词库。
///
/// **服务端分页 + 服务端搜索**（2026-08-25 由「一次全取 + 客户端过滤」改来）：临时词是
/// 打字过程中自动攒下的，只增不减直到用户转正或清空，量级与用户词库同级。全取要把整份
/// json 推过 IPC，设置端再给每条建一个表格行 widget（那张表不做虚拟化），上万条时开页即卡。
const SPEC_TEMP: CategorySpec = CategorySpec {
    category: WordCategory::Temp,
    title: "临时词库",
    scoped_by_schema: true,
    paged: true,
    searchable: true,
    client_search: false,
    columns: COLS_TEMP,
    ops: OpFlags {
        remove: true,
        clear: true,
        promote: true,
        promote_all: true,
        multi_select: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        // 分页版。`temp.list`（裸数组）仍在 core 侧保留给导出等全量取用方。
        list: "temp.listPaged",
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
        multi_select: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        list: "freq.listPaged",
        remove: "freq.delete",
        clear: "freq.clear",
        ..RpcNames::EMPTY
    },
};

/// 补全学习数据的列（邮箱后缀 / 网址历史共用）。
///
/// 没有「编码」列：这两类数据的 key 就是文本本身，core 那边也不返回 code 字段。
const COLS_COMPLETION: &[ColumnSpec] = &[
    ColumnSpec {
        header: "内容",
        weight: 3.0,
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

/// 邮箱后缀学习。
///
/// **全局**（`scoped_by_schema: false`）：「我常用哪个邮箱后缀」与五笔拼音无关，
/// core 侧那张表的 key 也不带方案。
///
/// **服务端分页**：用户可以打出任意后缀，条数无上限（不像预置表那样封闭），理由同
/// [`SPEC_TEMP`]。
const SPEC_MAIL_SUFFIX: CategorySpec = CategorySpec {
    category: WordCategory::MailSuffix,
    title: "邮箱后缀",
    scoped_by_schema: false,
    paged: true,
    searchable: true,
    client_search: false,
    columns: COLS_COMPLETION,
    ops: OpFlags {
        remove: true,
        clear: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        list: "mailSuffix.listPaged",
        remove: "mailSuffix.delete",
        clear: "mailSuffix.clear",
        ..RpcNames::EMPTY
    },
};

/// 网址输入历史。形态与 [`SPEC_MAIL_SUFFIX`] 完全一致，只换命名空间——两类共用
/// core 的同一张表，分开只为让「清空网址历史」不波及邮箱学习。
const SPEC_URL_HISTORY: CategorySpec = CategorySpec {
    category: WordCategory::UrlHistory,
    title: "网址历史",
    scoped_by_schema: false,
    paged: true,
    searchable: true,
    client_search: false,
    columns: COLS_COMPLETION,
    ops: OpFlags {
        remove: true,
        clear: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        list: "urlHistory.listPaged",
        remove: "urlHistory.delete",
        clear: "urlHistory.clear",
        ..RpcNames::EMPTY
    },
};

/// 快捷输入历史。形态同 [`SPEC_URL_HISTORY`]：core 同一张补全表的第三个分区，独立
/// 命名空间同样是为了让「清空快捷输入历史」不波及另外两类学习数据。
///
/// 「内容」列是用户上屏过的**字面文本**（`${reset_time}`、`RESET_TIME` 这类），不是
/// 编码——快捷输入历史按前缀匹配的就是这串文本本身。条数上限
/// `schema.quick_input.history_max` 在「输入 → 快捷输入 → 历史条数上限」。
const SPEC_QUICK_HISTORY: CategorySpec = CategorySpec {
    category: WordCategory::QuickHistory,
    title: "快捷输入历史",
    scoped_by_schema: false,
    paged: true,
    searchable: true,
    client_search: false,
    columns: COLS_COMPLETION,
    ops: OpFlags {
        remove: true,
        clear: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        list: "quickHistory.listPaged",
        remove: "quickHistory.delete",
        clear: "quickHistory.clear",
        ..RpcNames::EMPTY
    },
};

/// 候选调整（影子规则）。
///
/// **服务端分页 + 服务端搜索**（2026-08-25 由「一次全取 + 客户端过滤」改来）：规则随每次
/// 手动调序/删候选累加，没有上限，理由同 [`SPEC_TEMP`]。
///
/// ⚠️ 「词」这一列显示的是 `candId` 里的**模板原文**（见 [`WordRow::display_text`]），故
/// core 侧的搜索与排序也按显示文本算——两边判据一旦漂移，症状是「屏幕上看得见、搜索框
/// 里搜不着」。
const SPEC_SHADOW: CategorySpec = CategorySpec {
    category: WordCategory::Shadow,
    title: "候选调整",
    scoped_by_schema: true,
    paged: true,
    searchable: true,
    client_search: false,
    columns: COLS_SHADOW,
    ops: OpFlags {
        add: true,
        edit: true,
        remove_rule: true,
        clear: true,
        multi_select: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        // 分页版。`shadow.list`（裸数组）仍在 core 侧保留给全量取用方。
        list: "shadow.listPaged",
        add: "shadow.addRule",
        remove_rule: "shadow.removeRule",
        // 整表撤销走 core。此前是「视图侧循环 remove_rule」——分页之后设置端手上只剩
        // 当前页，那条路会静默只清一页。
        clear: "shadow.clear",
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

const COLS_COMMON: &[ColumnSpec] = &[
    ColumnSpec {
        header: "字",
        // 内容恒是一个汉字，给足点击区即可。
        weight: 0.8,
        field: WordField::Text,
    },
    ColumnSpec {
        header: "默认",
        weight: 1.0,
        field: WordField::CommonBase,
    },
    ColumnSpec {
        header: "当前",
        weight: 1.0,
        field: WordField::CommonNow,
    },
    ColumnSpec {
        // ⚠️ 这里原先是**两列**：「类型」（Unicode 区块）与「字符类」。并列的理由是
        // 「块决定整类批量扫哪个区间、类决定改默认判定影响谁」——但用户看到的是两列
        // 内容高度相似的东西（「表情符号」对「Emoji 表情」），分不清是必然的。
        //
        // ★ 既然「字符集分类」那一页已经能按类改判定，这里只需要回答一个问题：
        // **这个字的判定归哪个类管**。区块信息仍在行数据里（`blockRange` /
        // `blockBulkEditable`），整类批量的菜单与确认文案照常用它。
        header: "所属类",
        // 类名最长的是「带圈 CJK 字母及月份」，给得比两个判定列宽。
        weight: 2.6,
        field: WordField::Class,
    },
];

/// 字符集分类的列。
///
/// ⚠️ weight 要照**表头字数**给，不只看内容：四字表头在 1.2~1.4 下会被截成「默...」，
/// 而内容反而显示得下——截断的是列头，用户于是不知道这一列是什么。
///
/// ★ 五列与操作列（`actions_weight` = 1.3）是**一组配比**，合计 9.6：默认窗口
/// （820 宽）下表格区约 570px。改任何一个都用 `--size 820 640 --screenshot` 看一眼
/// ——列不会自动加宽，只会截断。
///
/// ★ 2026-09-18 这一版的腾挪：操作列由三枚小按钮换成一枚「操作…」菜单按钮
/// （`view.rs` 里那条 `show_dropdown_menu`），`actions_weight` 3.5 → 1.3 释放出 2.2。
/// 去向：1.3 给新增的「优先」列，0.3 给「范围 / 标记」，剩下 0.6 让**总量缩水**
/// （10.2 → 9.6）。⇒ 类别列的权重一个字没动（仍是 1.6），但分母小了，它的实际份额
/// 从 15.7% 涨到 16.7%——每一列都不比改动前窄，靠的是分母而不是加权重。
///
/// ⚠️ 中途试过把「默认」「覆盖」两个表头还原成四字的「默认判定」「逐字覆盖」
/// （各 1.6），**截图里被截成了「默认」「逐字」**：四字表头真实要的宽度比按字数算的
/// 多出一个排序指示器加左右内边距。两字表头是上一版实测站得住的配置，退回去。
/// ★ 列宽这件事只能靠截图定，不能靠算——这一版算了两次，两次都偏小。
const COLS_CHARSETS: &[ColumnSpec] = &[
    ColumnSpec {
        header: "类别",
        weight: 1.6,
        field: WordField::Text,
    },
    ColumnSpec {
        // ★★ 这一列回答用户的「谁先生效」：列表本身就按它升序排（core 的
        // `charset_rows` 排好再回），但顺序是**隐式**的——用户点一下别的列头重排之后
        // 就彻底看不出来了。数值摆在这里，排序怎么变都还在。
        header: "优先",
        // ⚠️ 1.0 时**列头**被截成「…」（内容 900 反而显示得下）——截断的是列头，
        // 用户于是不知道这一列是什么。两字表头连同排序指示器要 1.3 才排得开。
        weight: 1.3,
        field: WordField::ClassOrder,
    },
    ColumnSpec {
        // 表头两字：指「默认判定」——「操作…」菜单（与右键同源）里那三项改的就是它。
        // 四字会被截成「默认」，见列表头部那条 ⚠️。
        header: "默认",
        // 三态文案「未设置」最长。
        weight: 1.3,
        field: WordField::ClassDefault,
    },
    ColumnSpec {
        // ⚠️ 范围与标记**合成一列**：表格区只有约 570px，拆开会让每一列都换行截断
        // （「生僻模式 · 已」）。两者都是「这个类长什么样」的描述，合并语义上成立；
        // 而「默认判定」「逐字覆盖」是本页的操作焦点，必须独立且宽。
        header: "范围 / 标记",
        weight: 2.7,
        field: WordField::ClassScope,
    },
    ColumnSpec {
        // 表头两字：四字的「逐字覆盖」在这个配比下会被截成「逐字」。本页语境里
        // 「覆盖」就是指逐字覆盖，表格上方那句说明也把它说全了。
        header: "覆盖",
        // ★ 这一列是「配了没反应」的解释：用户改了默认判定却看不到变化时，
        // 答案就在这里——那些字被逐条覆盖压着（逐字覆盖优先于**所有**类）。
        weight: 1.4,
        field: WordField::ClassOverrides,
    },
];

/// 字符集分类。
///
/// 全局（不按方案）：字符类是语言学属性，与五笔/拼音无关。
///
/// **不分页**：类的数量是几十个量级（50 个 Unicode 区块 + emoji + 常用汉字 + 用户自建），
/// 一次取完比分页简单，也让「按范围找一个类」这件事可以在客户端搜索里做。
///
/// 「+ 添加」只建一个空分类（文件名、显示名、默认判定、优先级，见 `charset_new`）；
/// 成员与码位范围用这一行的「编辑文件」直接改那一类的文件。
const SPEC_CHARSETS: CategorySpec = CategorySpec {
    category: WordCategory::Charsets,
    title: "字符分类",
    scoped_by_schema: false,
    paged: false,
    searchable: true,
    client_search: true,
    columns: COLS_CHARSETS,
    ops: OpFlags {
        // ⛔ **不给 `edit`**：本类别没有编辑表单，`save_calls` 会走到兜底分支报
        // 「该类别不支持编辑」——用户点开对话框、填完、保存时被拒，是条死路。
        //
        // 改默认判定走右键菜单里三个明确的目标态（见 `view.rs`），那不依赖 `edit`。
        // 改范围要新建自己的类（手写一份 charsets/*.yaml），不是这个列表能做完的事。
        //
        // 撤销对这个类的全部调整，回到出厂。
        reset_entry: true,
        // 「+ 添加」= 新建一个空分类（只填基本信息，见 `charset_new`）。
        add: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        list: "charset.list",
        update: "charset.set",
        reset_entry: "charset.reset",
        ..RpcNames::EMPTY
    },
};

/// 常用字表。
///
/// 全局（不按方案）：「某个字常不常用」是语言学属性，与五笔/拼音无关，core 那张表的键
/// 就是那个字本身。
///
/// **列的是全表**（默认字表 8104 条 + 用户加的），不是只列改过的那几条：用户来这个页面
/// 最常问的是「这个字现在算不算常用」，只列改动答不了。改过的行由
/// [`WordRow::adjusted`] 标出，「恢复默认」据此只对它们放行。
///
/// 服务端分页 + 服务端搜索：8104 条一次全取约 400KB JSON，每次刷新整份过 IPC 不划算，
/// 走 `dict`/`freq` 那套现成的分页。搜索按「字出现在查询串里」匹配——用户既可以只打
/// 一个字查它，也可以粘一整句列出句中所有字。
const SPEC_COMMON: CategorySpec = CategorySpec {
    category: WordCategory::CommonChars,
    title: "常用字",
    scoped_by_schema: false,
    paged: true,
    searchable: true,
    client_search: false,
    columns: COLS_COMMON,
    ops: OpFlags {
        // 添加：把一个不在表里的字纳入管理（指定判为哪边）。
        add: true,
        // 编辑：把某个字改到另一边。
        edit: true,
        // 恢复这一条的默认判定；仅对改过的行开放（见 `row_ops`）。
        reset_entry: true,
        // 整表恢复默认（清掉全部调整）。
        reset_default: true,
        // 导入导出的是**用户调整**（那份稀疏覆盖），不是 8104 字全表：文件带到另一台机器
        // 上，全表快照会把对方默认里本就有的字也钉成显式覆盖，从此拿不到默认表升版。
        import: true,
        export: true,
        ..OpFlags::NONE
    },
    rpc: RpcNames {
        list: "commonChars.list",
        // add 与 update 是同一个 RPC：两者都是「把这个字设成这一边」，参数完全一样。
        // core 侧对已存在的字直接改写方向，不需要区分新增与修改。
        add: "commonChars.set",
        update: "commonChars.set",
        // 撤销一条调整（回到默认判定），不是删字。
        reset_entry: "commonChars.reset",
        reset_default: "commonChars.clear",
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
        // 常用字列的是全表，绝大多数行从没被碰过：给它们亮着「恢复默认」，点下去
        // 什么都不会发生。只对改过的行放行（`adjusted` 由 core 的 list 直接给出）。
        // 常用字列的是全表，绝大多数行从没被碰过：给它们亮着「恢复此条默认」，点下去
        // 什么都不会发生。只对改过的行放行（`adjusted` 由 core 的 list 直接给出）。
        WordCategory::CommonChars => OpFlags {
            reset_entry: base.reset_entry && row.adjusted,
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
            // 一枚「操作…」菜单按钮，与 COLS_CHARSETS 的五个权重是一组配比（合计 9.5）。
            //
            // ★ 曾是 3.5——那时操作列上并排着三枚小按钮（「常用」「生僻」「编辑」），
            // 而 2.0 会把第三枚整个挤出可视区（2026-09-06 真机：用户以为没有编辑入口）。
            // 2026-09-18 换成菜单按钮后，那三枚连同右键菜单里的其余各项都进了同一份
            // 菜单（`view.rs::row_menu_items`），列宽需求随之塌回一枚按钮。
            //
            // ⚠️ 要是哪天又往操作列里塞并排按钮，这个数得跟着回去——列不会自动加宽，
            // 只会截断。
            WordCategory::Charsets => 1.3,
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
        WordCategory::UserLib => &SPEC_USERLIB,
        WordCategory::Temp => &SPEC_TEMP,
        WordCategory::Freq => &SPEC_FREQ,
        WordCategory::Shadow => &SPEC_SHADOW,
        WordCategory::QuickFormat => &SPEC_QUICK,
        WordCategory::CommonChars => &SPEC_COMMON,
        WordCategory::Charsets => &SPEC_CHARSETS,
        WordCategory::MailSuffix => &SPEC_MAIL_SUFFIX,
        WordCategory::UrlHistory => &SPEC_URL_HISTORY,
        WordCategory::QuickHistory => &SPEC_QUICK_HISTORY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// ★ [`LogicalTab::ALL`] 必须列全每个变体。
    ///
    /// 它是手写数组，而 `from_cli_id` 只在它里面找——漏一项的后果是 `--type <那个>`
    /// 反查不到，深链变成「未知类型」。加变体时编译器不会提醒这个数组。
    ///
    /// 下面那个 match 是**穷尽**的：加了新变体它会编译失败，人就会看到这条测试，
    /// 进而想起来把变体加进 `ALL`。长度断言是第二道。
    #[test]
    fn all_lists_every_logical_tab() {
        const fn seq(t: LogicalTab) -> usize {
            match t {
                LogicalTab::PhraseSystem => 0,
                LogicalTab::PhraseUser => 1,
                LogicalTab::UserDict => 2,
                LogicalTab::Temp => 3,
                LogicalTab::Freq => 4,
                LogicalTab::Shadow => 5,
                LogicalTab::QuickFormat => 6,
                LogicalTab::CommonChars => 7,
                LogicalTab::Charsets => 8,
                LogicalTab::MailSuffix => 9,
                LogicalTab::UrlHistory => 10,
                LogicalTab::QuickHistory => 11,
            }
        }
        assert_eq!(
            LogicalTab::ALL.len(),
            12,
            "加了新的 LogicalTab 变体就要加进 ALL，否则 --type 反查不到它"
        );
        let mut seen: Vec<usize> = LogicalTab::ALL.iter().map(|t| seq(*t)).collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..12).collect::<Vec<_>>(), "ALL 里有重复或缺项");

        // 每个 cli_id 都能反查回自己——大小写不敏感那条也顺带覆盖。
        for t in LogicalTab::ALL {
            assert_eq!(
                LogicalTab::from_cli_id(t.cli_id()),
                Some(*t),
                "{} 反查不回来",
                t.cli_id()
            );
        }
    }

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
            WordCategory::UserLib,
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
    }

    /// 会无上限增长的两类（临时词库 / 候选调整）必须走服务端分页——它们此前是
    /// 「一次全取 + 客户端过滤」，条目上万时开页即卡。守住三件配套的事：分页开关、
    /// 搜索改走服务端、list 指向 `*Paged`。
    #[test]
    fn unbounded_categories_are_server_paged() {
        // 补全学习数据同样无界：用户能打出任意后缀、访问任意网址（网址历史另有
        // history_max 封顶，但那个值用户可以调到上万，客户端过滤照样撑不住）。
        for cat in [
            WordCategory::Temp,
            WordCategory::Shadow,
            WordCategory::MailSuffix,
            WordCategory::UrlHistory,
            WordCategory::QuickHistory,
        ] {
            let s = spec_for(cat);
            assert!(s.paged, "{:?} 应服务端分页", cat);
            assert!(s.searchable && !s.client_search, "{:?} 应服务端搜索", cat);
            assert!(
                s.rpc.list.ends_with("listPaged"),
                "{:?} 的 list 应指向分页 RPC，实为 {}",
                cat,
                s.rpc.list
            );
        }
    }

    /// 候选调整的「清空」必须有真 RPC。分页后设置端手上只剩当前页，靠循环 remove_rule
    /// 凑出来的清空会静默只清一页——这条断言就是那个回归的闸门。
    #[test]
    fn shadow_clear_has_dedicated_rpc() {
        let s = spec_for(WordCategory::Shadow);
        assert!(s.ops.clear);
        assert_eq!(s.rpc.clear, "shadow.clear");
    }

    /// 三类补全学习数据必须是**全局**的，且清空必须各走各的 RPC。
    ///
    /// `scoped_by_schema` 若写成 true，列表会带上 schemaId 发出去——core 侧那两个方法
    /// 压根不看这个参数，于是「列表看起来正常、按方案筛却筛不动」，要等有人去按方案
    /// 分组才发现。这与快捷输入那条（`quick_domain_is_global_and_sends_no_schema_id`）
    /// 是同一个坑。
    ///
    /// 各 clear 指向不同 RPC 这一条也要钉：写成同一个的话，「清空网址历史」会把用户
    /// 的邮箱后缀学习一并抹掉，而界面上没有任何提示。
    #[test]
    fn completion_categories_are_global_with_separate_clear_rpcs() {
        let mail = spec_for(WordCategory::MailSuffix);
        let url = spec_for(WordCategory::UrlHistory);
        let quick = spec_for(WordCategory::QuickHistory);
        for s in [mail, url, quick] {
            assert!(!s.scoped_by_schema, "补全学习数据是全局的，不该带 schemaId");
            assert!(s.ops.clear && s.ops.remove, "应支持清空与逐条删");
            // 列里没有「编码」：这几类的 key 就是文本本身，core 也不返回 code 字段。
            assert!(
                !s.columns.iter().any(|c| c.field == WordField::Code),
                "补全学习数据没有编码这个概念"
            );
        }
        assert_eq!(mail.rpc.clear, "mailSuffix.clear");
        assert_eq!(url.rpc.clear, "urlHistory.clear");
        assert_eq!(quick.rpc.clear, "quickHistory.clear");
        let clears = [mail.rpc.clear, url.rpc.clear, quick.rpc.clear];
        let distinct: std::collections::BTreeSet<_> = clears.iter().collect();
        assert_eq!(
            distinct.len(),
            clears.len(),
            "各类必须分开清 —— 清一类历史不该顺带抹掉另外两类"
        );
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
    /// 类型列：`block` 三个字段都要从 list 解析出来，并进得了单元格文本。
    ///
    /// ⚠️ 这里同时钉住**两处分派**：`from_json`（解析）与 `cell_text`（显示）。
    /// 只补一处的表现是「列出来了但永远是空白」，或「解析到了但没有这一列」——
    /// 两种都不报错，测试也照样绿，除非像这样把两头一起断言。
    #[test]
    fn common_chars_row_carries_block_type() {
        let v = json!({
            "char": "ㄅ",
            "common": false,
            "baseCommon": true,
            "adjusted": true,
            "block": "注音符号",
            "blockRange": "3100-312F",
            "blockBulkEditable": true,
        });
        let row = WordRow::from_json(WordCategory::CommonChars, &v);
        assert_eq!(row.text, "ㄅ");
        assert_eq!(row.block, "注音符号");
        assert_eq!(row.block_range, "3100-312F");
        assert!(row.block_bulk_editable);
        assert_eq!(
            row.cell_text(WordField::Block),
            "注音符号",
            "解析到了还得进得了单元格——两处分派漏一处就是一列空白"
        );

        // 汉字行：core 会把 bulk 标志置假，行结构要如实带过来。
        let han = WordRow::from_json(
            WordCategory::CommonChars,
            &json!({
                "char": "我", "common": true, "baseCommon": true, "adjusted": false,
                "block": "基本汉字", "blockRange": "4E00-9FFF", "blockBulkEditable": false,
            }),
        );
        assert!(
            !han.block_bulk_editable,
            "汉字块必须禁止整类操作——放行就是一个一键作废整张默认字表的按钮"
        );
    }

    /// 「所属类」列必须真的在常用字的列定义里，否则上面那条解析测试是空转的。
    ///
    /// ⚠️ 这里原先要求的是 `WordField::Block`（Unicode 区块）。两列并存时用户看到的是
    /// 两列内容高度相似的东西（「表情符号」对「Emoji 表情」），分不清；合并成一列后
    /// 回答的是唯一要紧的那个问题：**这个字的判定归哪个类管**。
    ///
    /// 区块数据仍在行里（`blockRange` / `blockBulkEditable`），整类批量照常用它——
    /// 故 `Block` 字段本身不删，只是不再单独占一列。
    #[test]
    fn common_chars_spec_has_class_column() {
        let cols = spec_for(WordCategory::CommonChars).columns;
        assert!(
            cols.iter().any(|c| c.field == WordField::Class),
            "常用字要有「所属类」列"
        );
        assert!(
            !cols.iter().any(|c| c.field == WordField::Block),
            "区块不再单独占一列——两列相似内容分不清，见本测试的文档"
        );
        assert_eq!(WordField::Class.rpc_key(), "class");
    }
    /// 整类操作必须**两个方向都提供**，不能按当前行的判定取反。
    ///
    /// 取反会造成一个够不着的死锁，且从代码上完全看不出来：域外字符（注音、假名、IDC）
    /// 默认判「常用」，而它**能出现在列表里的前提就是已被设为生僻**——列表只列默认字表
    /// 加用户覆盖过的字。于是这一行恒为「当前=生僻」，取反后菜单里只剩「全部设为常用」，
    /// 用户永远点不到「全部设为生僻」，也就永远关不掉这一类。
    ///
    /// 本条钉的是那个前提：域外字符出现在列表里时，`common_now` 恒为 false。
    #[test]
    fn out_of_scope_rows_are_always_rare_so_bulk_needs_both_directions() {
        // 用户把 ㄅ 设为生僻，它才进列表——这是域外字符进列表的唯一途径。
        let row = WordRow::from_json(
            WordCategory::CommonChars,
            &json!({
                "char": "ㄅ", "common": false, "baseCommon": true, "adjusted": true,
                "block": "注音符号", "blockRange": "3100-312F", "blockBulkEditable": true,
            }),
        );
        assert!(row.block_bulk_editable);
        assert!(
            !row.common_now && row.common_base,
            "域外字符默认放行、被设为生僻后才进列表：取反只会得到「设为常用」"
        );
        // 两个方向的标签都得拼得出来，且互不相同。
        assert_eq!(common_label(false), "生僻");
        assert_eq!(common_label(true), "常用");
    }
}

/// 字符类的「范围」摘要文本。
///
/// 码位段与离散成员**两支都要报**：emoji 类是纯字表（1427 条、零码位段），只报段会显示
/// 空白，用户以为这个类什么都不管；常用汉字类反过来。段多于两条时省略后面的，
/// 完整列表在配置文件里。
fn charset_scope_text(v: &serde_json::Value) -> String {
    let ranges: Vec<&str> = v
        .get("ranges")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    let members = v.get("memberCount").and_then(|x| x.as_u64()).unwrap_or(0);

    let mut parts: Vec<String> = Vec::new();
    if !ranges.is_empty() {
        let shown = ranges.iter().take(2).cloned().collect::<Vec<_>>().join(" ");
        parts.push(if ranges.len() > 2 {
            format!("{shown} 等 {} 段", ranges.len())
        } else {
            shown
        });
    }
    if members > 0 {
        parts.push(format!("{members} 字"));
    }
    if parts.is_empty() {
        "—".to_string()
    } else {
        parts.join(" · ")
    }
}
