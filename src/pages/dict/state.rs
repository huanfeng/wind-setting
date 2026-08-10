//! 词库管理状态 — 导航（数据域/子标签）、当前类别数据加载与刷新。
//!
//! 复刻 [`crate::dialogs::schema_manager::SchemaManagerState`] 的范式：
//! `Rc<RefCell<>>` 存原始数据 + `Signal` 驱动响应式 + `list_signal` 即时刷新。
//! 挂进 `AppState`（与 `schema_mgr` 平级）以便对话框挂根、状态跨页重建存活。
//!
//! 所有写操作走直接 RPC 即时生效，不进入设置页的批量保存通道。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::core::EventCtx;
use windui::prelude::*;

use crate::dialogs::schema_manager::engine_label;
use crate::rpc;
use crate::state::SchemaInfo;

use super::spec::{CategorySpec, LogicalTab, WordCategory, WordField, WordRow, spec_for};

/// 客户端排序（非分页类别）：数值字段按数值，其余按展示文本字符串序。稳定排序。
fn sort_rows(rows: &mut [WordRow], field: WordField, ord: SortOrder) {
    let num = |r: &WordRow| match field {
        WordField::Weight => r.weight,
        WordField::Count => r.count,
        WordField::Position => r.position,
        _ => 0,
    };
    rows.sort_by(|a, b| {
        let c = if field.is_numeric() {
            num(a).cmp(&num(b))
        } else {
            a.cell_text(field).cmp(&b.cell_text(field))
        };
        match ord {
            SortOrder::Asc => c,
            SortOrder::Desc => c.reverse(),
        }
    });
}

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

// 表格数据流：结构（列集随类别变）由 `table_epoch` 驱动整表重建；
// 数据（当前页行文本）由 `table_rows` 驱动表格正文重建（服务端排序模式）；
// 排序意图由 `sort` 信号承载（表头箭头双向绑定）。

/// 词库管理状态（Signal 为 Copy，可直接 Clone 进闭包）。
#[derive(Clone)]
pub struct DictManagerState {
    /// 数据域：0 = 快捷短语（全局）；>=1 = `schemas[idx-1]`。
    pub domain: Signal<usize>,
    /// 方案域下的子标签：0=用户词库 1=临时词库 2=词频 3=候选调整。
    pub sub_tab: Signal<usize>,
    /// 数据域下拉的响应式选项（首项"快捷短语"，其后各方案；set_schemas 后填充）。
    pub domain_items: Signal<Vec<String>>,
    /// 数据域下拉的富内容选项（label + 引擎类型徽章）；与 `domain_items` 同序同步刷新。
    pub domain_rich_items: Signal<Vec<DropdownItem>>,
    /// 可选方案列表（来自 `AppState.schemas`）。
    pub schemas: Rc<RefCell<Vec<SchemaInfo>>>,
    /// 各数据域上次停留的子标签（域下标 → sub_tab）：切域时恢复，避免统一跳到同一 TAB。
    sub_tab_memory: Rc<RefCell<HashMap<usize, usize>>>,
    /// 上次导航所在的数据域（用于在 `on_nav_changed` 里判定"域是否变化"）。
    last_domain: Rc<Cell<usize>>,

    /// 当前类别原始行。
    pub rows: Rc<RefCell<Vec<WordRow>>>,
    /// 当前显示页各行单元格文本（列与当前 spec.columns 对应），驱动表格正文。
    pub table_rows: Signal<Vec<Vec<String>>>,
    /// 与 `table_rows` 顺序一致的行模型（过滤+排序后），操作列回调按显示下标取行。
    pub display_rows: Rc<RefCell<Vec<WordRow>>>,
    /// 操作列「启用」开关的 signal 池，按页内显示下标复用（见 [`Self::enabled_sig`]）。
    /// windui 的 signal arena 只增不减（`signal.rs` 自承「静态树可接受」的前提），而表格正文
    /// 每次重建都会重跑行生成器——逐行新建 signal 会使每次搜索/翻页泄漏一批 slot。
    /// 池化后 signal 总数封顶为历史最大行数，不随重建次数增长。
    enabled_sigs: Rc<RefCell<Vec<Signal<bool>>>>,
    /// 表格结构版本（单元素，导航变化时递增）：列集随类别变，须整表重建。
    pub table_epoch: Signal<Vec<u64>>,
    /// 受控排序状态（None=无排序；表头点击循环切换，导航变化时重置）。
    pub sort: Signal<Option<(usize, SortOrder)>>,
    /// 空态提示文本（无数据/未连接/无方案）；有数据时为空串（视图据此显隐覆盖层）。
    pub empty_msg: Signal<String>,
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
    /// (三态 kind, 文案)。
    pub feedback: super::reactive::Feedback,

    /// 确认对话框可见标志（清空/删除/恢复默认等破坏性操作）。
    pub confirm_visible: Signal<bool>,
    /// 确认对话框正文。
    pub confirm_msg: Signal<String>,
    /// 待确认的操作（确认后执行）。
    pending: Rc<RefCell<Option<PendingAction>>>,

    // ── 词库导入对话框（格式说明 + 选文件 + 自动识别预览 + 合并/替换选择）──
    pub import_confirm_visible: Signal<bool>,
    pub import_preview_msg: Signal<String>,
    /// 已选文件名（仅展示；空 = 未选）。
    pub import_file_name: Signal<String>,
    /// 预览成功、可执行导入（驱动合并/替换按钮可用态）。
    pub import_ready: Signal<bool>,
    /// 待导入的文件内容（预览确认后执行导入）。
    pending_import: Rc<RefCell<Option<String>>>,
    // ── 方案级「导入/导出」统一对话框的数据类型勾选（导入导出共用；仅适用类型显示）──
    /// 导入/导出的**目标方案**下标（索引进 [`Self::folded_domains`]，即数据域下标 - 1）。
    ///
    /// 与页面导航的 `domain` 刻意解耦。混输方案自身只有候选调整（词条存在成员方案里），
    /// 而用户的心智是「我有个词库文件，导进去让它能打出来」——目标若跟着当前域走，
    /// 混输用户在混输域点导入就必然导错：预览写着「用户词库 2000」，实际一条都进不去。
    pub io_target: Signal<usize>,
    /// 目标方案下拉的响应式选项（**仅方案域**，不含短语域；与 `folded_domains` 同序同下标）。
    pub io_target_items: Signal<Vec<DropdownItem>>,
    /// 目标方案的自动匹配提示（空 = 不显示）。
    pub io_auto_note: Signal<String>,
    /// 导出对话框的引擎提示（文件带引擎标记、只能导回同类引擎）。
    pub io_engine_note: Signal<String>,
    /// 写入 `io_auto_note` 时记下的目标下标：目标再变（用户手动改下拉）即清提示，
    /// 避免「已自动选择「五笔86」」这句话挂在一个已被改成拼音的下拉上。
    io_auto_at: Rc<Cell<Option<usize>>>,
    /// 目标方案的作用域说明（如混输只有候选调整；空 = 不显示）。导入导出对话框共用。
    pub io_scope_note: Signal<String>,
    /// 用户词库
    pub io_user_words: Signal<bool>,
    /// 临时词库
    pub io_temp_words: Signal<bool>,
    /// 词频
    pub io_freq: Signal<bool>,
    /// 候选调整
    pub io_shadow: Signal<bool>,
    /// 导出对话框可见（导入复用 import_confirm_visible）。
    pub export_visible: Signal<bool>,
    /// 导入/导出对话框标题（含当前方案/类型说明，如「导出 · 五笔86 · 码表」）。
    pub io_title: Signal<String>,
    /// 导入来源与目标是否引擎兼容（false=不同类型，禁止导入并提示）。
    pub import_compatible: Signal<bool>,

    // ── 加/改词对话框（用户词库 / 短语）──
    pub edit_visible: Signal<bool>,
    pub edit_title: Signal<String>,
    /// 编码字段的标签（随类别）。
    pub edit_l_code: Signal<String>,
    /// 词条/短语字段的标签（随类别）。
    pub edit_l_text: Signal<String>,
    pub edit_code: Signal<String>,
    pub edit_text: Signal<String>,
    pub edit_weight: Signal<String>,
    /// 短语次序（`position`）。**仅在同编码、同权重时才决定先后**——主程序的短语层排序是
    /// 「weight 降序，然后 position 升序」，权重不同则 position 完全不起作用。
    /// 导入导出的 wdict 短语段一直带这一列，此前设置页却只能写死 0。
    pub edit_position: Signal<String>,
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
    /// 查看对话框：次序（与用户短语的编辑对话框一致，只是不可改）。
    pub view_position: Signal<String>,
    /// 当前查看的系统短语原值 `(code, text, weight, position)`，供「转为用户短语」原样复制。
    /// 存原值而不是从 `view_*` 信号回 parse——那几个是给人看的字符串，parse 失败就得静默兜底。
    viewing: Rc<RefCell<Option<(String, String, i64, i64)>>>,

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
    /// 正在编辑的候选调整规则的稳定 id（短语规则才有；新增模式为空）。
    ///
    /// **提交时必须原样回传**：这条路先按 id/word 匹配掉同一条旧规则再插入新规则，不带
    /// 就等于把原规则的 id 擦掉——规则退化成按当日文本匹配，次日必失配。用户侧表现为
    /// 「在设置页改了一下位置，第二天整条规则就不生效了」。
    shadow_cand_id: Rc<RefCell<String>>,
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
    /// 英文方案。**没有临时词库**——那是自动造词的暂存区，英文没有造词流程，
    /// 挂上去永远是空表。其余（用户词库/词频/候选调整）都有。
    English,
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
            domain_rich_items: signal(vec![
                DropdownItem::new("快捷短语").badge("全局", Intent::Neutral),
            ]),
            schemas: Rc::new(RefCell::new(Vec::new())),
            sub_tab_memory: Rc::new(RefCell::new(HashMap::new())),
            last_domain: Rc::new(Cell::new(0)),
            rows: Rc::new(RefCell::new(Vec::new())),
            table_rows: signal(vec![]),
            display_rows: Rc::new(RefCell::new(Vec::new())),
            enabled_sigs: Rc::new(RefCell::new(Vec::new())),
            table_epoch: signal(vec![0]),
            sort: signal(None),
            empty_msg: signal(String::new()),
            search: signal(String::new()),
            offset: Rc::new(RefCell::new(0)),
            total: signal(0),
            page_status: signal(String::new()),
            status: signal(String::new()),
            feedback: signal(None),
            confirm_visible: signal(false),
            confirm_msg: signal(String::new()),
            pending: Rc::new(RefCell::new(None)),
            import_confirm_visible: signal(false),
            import_preview_msg: signal(String::new()),
            import_file_name: signal(String::new()),
            import_ready: signal(false),
            pending_import: Rc::new(RefCell::new(None)),
            io_target: signal(0),
            io_target_items: signal(vec![]),
            io_auto_note: signal(String::new()),
            io_engine_note: signal(String::new()),
            io_auto_at: Rc::new(Cell::new(None)),
            io_scope_note: signal(String::new()),
            io_user_words: signal(true),
            io_temp_words: signal(true),
            io_freq: signal(true),
            io_shadow: signal(true),
            export_visible: signal(false),
            io_title: signal(String::new()),
            import_compatible: signal(true),
            edit_visible: signal(false),
            edit_title: signal(String::new()),
            edit_l_code: signal("编码".to_string()),
            edit_l_text: signal("词条".to_string()),
            edit_code: signal(String::new()),
            edit_text: signal(String::new()),
            edit_weight: signal("0".to_string()),
            edit_position: signal("0".to_string()),
            editing_orig: Rc::new(RefCell::new(None)),
            shadow_orig: Rc::new(RefCell::new(None)),
            shadow_cand_id: Rc::new(RefCell::new(String::new())),
            view_visible: signal(false),
            view_code: signal(String::new()),
            view_text: signal(String::new()),
            view_weight: signal(String::new()),
            view_position: signal(String::new()),
            viewing: Rc::new(RefCell::new(None)),
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
        self.feedback
            .set(Some((windui::prelude::ToastKind::Success, msg.to_string())));
    }
    /// 失败反馈（toast 红）。
    pub fn notify_err(&self, msg: &str) {
        self.feedback
            .set(Some((windui::prelude::ToastKind::Error, msg.to_string())));
    }
    /// 中性反馈（toast 灰/默认，如"没有变更"）。
    pub fn notify_info(&self, msg: &str) {
        self.feedback
            .set(Some((windui::prelude::ToastKind::Info, msg.to_string())));
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
                let name = if s.name.is_empty() {
                    s.id.clone()
                } else {
                    s.name.clone()
                };
                out.push(DomainEntry {
                    label: if tag == "未知" {
                        name
                    } else {
                        format!("{name} · {tag}")
                    },
                    kind: s.kind.clone(),
                    rep_schema_id: s.id.clone(),
                });
            }
        }
        out
    }

    // ── 导航解析 ─────────────────────────────────────────────

    /// 指定数据域的类型（0 = 快捷短语）。
    pub fn domain_kind_at(&self, domain: usize) -> DomainKind {
        if domain == 0 {
            return DomainKind::Phrase;
        }
        match self
            .folded_domains()
            .get(domain - 1)
            .map(|e| e.kind.as_str())
        {
            Some("pinyin") => DomainKind::Pinyin,
            Some("codetable") => DomainKind::Codetable,
            Some("mixed") => DomainKind::Mixed,
            Some("english") => DomainKind::English,
            _ => DomainKind::Other,
        }
    }

    /// 当前数据域类型。
    pub fn domain_kind(&self) -> DomainKind {
        self.domain_kind_at(self.domain.get())
    }

    /// 指定数据域的子标签序列（按 kind 生成）。
    ///
    /// 按域参数化而非只读当前信号：深链要在「切过去之前」就知道目标域有没有请求的类型，
    /// 否则只能像早期 `open_add_word_prefilled` 那样临时改写 `domain` 信号再恢复——
    /// 那种写法会平白触发信号版本变化，把导航 effect 卷进来。
    pub fn tabs_for_domain_at(&self, domain: usize) -> Vec<LogicalTab> {
        use LogicalTab::*;
        match self.domain_kind_at(domain) {
            DomainKind::Phrase => vec![PhraseSystem, PhraseUser],
            DomainKind::Pinyin => vec![UserDict, Temp, Freq],
            DomainKind::Mixed => vec![Shadow],
            // 英文：无临时词库（无造词流程，那张表恒空）。
            DomainKind::English => vec![UserDict, Freq, Shadow],
            // 码表与未知：全四类（未知从宽，不隐藏数据）。
            DomainKind::Codetable | DomainKind::Other => vec![UserDict, Temp, Freq, Shadow],
        }
    }

    /// 当前域的子标签序列。
    pub fn tabs_for_domain(&self) -> Vec<LogicalTab> {
        self.tabs_for_domain_at(self.domain.get())
    }

    /// 指定数据域的展示名（0 = "快捷短语"，其余取折叠域标签，如 "五笔86 · 码表"）。
    pub fn domain_label_at(&self, domain: usize) -> String {
        if domain == 0 {
            return "快捷短语".to_string();
        }
        self.folded_domains()
            .get(domain - 1)
            .map(|e| e.label.clone())
            .unwrap_or_default()
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
        self.folded_domains()
            .get(d - 1)
            .map(|e| e.rep_schema_id.clone())
    }

    // ── 程序化定位（命令行深链 / 加词小窗）─────────────────────

    /// 把外部传入的方案标识解析为数据域下标（0 = 快捷短语，≥1 = 折叠后的方案域）。
    ///
    /// 按序尝试：`phrase` → 精确方案 id → 引擎族名（`pinyin`/`codetable`/`mixed`）
    /// → 已启用但被折叠掉的拼音变体（如双拼 id）。全不中返回 `None`。
    ///
    /// 严格匹配，刻意不做「未知即拼音」的猜测：命令行里 `--schema` 拼错时，
    /// 静默落到别的域远比明确告诉用户"没找到"更难排查。需要宽松兜底的调用方
    /// （如 [`Self::open_add_word_prefilled`]）自行在外层追加。
    pub fn resolve_domain(&self, schema: &str) -> Option<usize> {
        let s = schema.trim();
        if s.is_empty() {
            return None;
        }
        if s.eq_ignore_ascii_case("phrase") {
            return Some(0);
        }
        let domains = self.folded_domains();
        if let Some(i) = domains.iter().position(|d| d.rep_schema_id == s) {
            return Some(i + 1);
        }
        let kind = s.to_ascii_lowercase();
        if let Some(i) = domains.iter().position(|d| d.kind == kind) {
            return Some(i + 1);
        }
        // 折叠掉的拼音变体：id 在方案列表里且引擎为拼音 → 落拼音域代表。
        let is_pinyin_variant = self
            .schemas
            .borrow()
            .iter()
            .any(|x| x.id == s && x.kind == "pinyin");
        if is_pinyin_variant {
            return domains
                .iter()
                .position(|d| d.kind == "pinyin")
                .map(|i| i + 1);
        }
        None
    }

    /// 程序化导航到指定域/子标签（`sub = None` 表示沿用该域上次停留处）。
    ///
    /// **不要直接 `domain.set()` + `sub_tab.set()`**：view 侧挂着一个监听二者版本的
    /// effect，变化时跑 [`Self::on_nav_changed`]，而那里把「domain 变了」唯一地解释成
    /// 用户手动切域，会用 `sub_tab_memory` 里的旧值把刚设的子标签覆盖回去。
    ///
    /// 这里的解法不是加抑制标志（易留下卡死态），而是**让状态提前自洽**：先把
    /// `last_domain` 与 `sub_tab_memory` 对齐到目标，effect 随后自然走「同域」分支，
    /// 只多做一次等价 reload。
    ///
    /// 本方法不自行 reload —— 有表格的页面由上述 effect 负责，无表格的调用方
    /// （加词小窗）则本就不该为此发一次 list RPC。
    pub fn navigate_to(&self, domain: usize, sub: Option<usize>) {
        let domain = domain.min(self.domain_count().saturating_sub(1));
        let tabs_len = self.tabs_for_domain_at(domain).len();
        let sub = sub
            .or_else(|| self.sub_tab_memory.borrow().get(&domain).copied())
            .unwrap_or(0)
            .min(tabs_len.saturating_sub(1));

        self.last_domain.set(domain);
        self.sub_tab_memory.borrow_mut().insert(domain, sub);
        self.domain.set(domain);
        self.sub_tab.set(sub);
    }

    /// 应用命令行深链（`--page dict --schema <id> --type <id>`）。
    ///
    /// 两维各自可选：省 `schema` = 保持当前域，省 `tab` = 沿用该域上次停留的子标签。
    /// 返回 `Some(提示)` 表示已尽力定位但有偏差（供上层 toast），`None` 表示完全命中。
    ///
    /// 偏差有两类，都降级而不报错——用户的意图是"打开设置看看"，把他挡在门外没有意义：
    /// - 方案没找到 → 保持当前域
    /// - 该域没有请求的类型（如拼音域没有候选调整）→ 落该域首个子标签
    pub fn apply_deep_link(&self, schema: &str, tab: Option<LogicalTab>) -> Option<String> {
        let mut note = None;
        let domain = match self.resolve_domain(schema) {
            Some(d) => d,
            None => {
                if !schema.trim().is_empty() {
                    note = Some(format!("未找到方案「{schema}」，已保持当前数据域"));
                }
                self.domain.get()
            }
        };

        // 子标签集随域而变，故必须先把域定下来再解析类型。
        let tabs = self.tabs_for_domain_at(domain);
        let sub = match tab {
            None => None,
            Some(t) => match tabs.iter().position(|x| *x == t) {
                Some(i) => Some(i),
                None => {
                    if let Some(first) = tabs.first() {
                        note = Some(format!(
                            "「{}」没有「{}」，已定位到「{}」",
                            self.domain_label_at(domain),
                            t.title(),
                            first.title()
                        ));
                    }
                    Some(0)
                }
            },
        };

        self.navigate_to(domain, sub);
        note
    }

    /// 数据域下拉选项标签（首项"快捷短语"，其后折叠后各域）。
    pub fn domain_labels(&self) -> Vec<String> {
        let mut v = vec!["快捷短语".to_string()];
        for e in self.folded_domains() {
            v.push(e.label);
        }
        v
    }

    /// 数据域下拉的富内容选项：方案名为主文本、引擎类型为尾随徽章（拼音=强调色，其余=中性）。
    /// 首项"快捷短语"标"全局"徽章。与 [`Self::domain_labels`] 同序。
    pub fn domain_rich_items(&self) -> Vec<DropdownItem> {
        let mut v = vec![DropdownItem::new("快捷短语").badge("全局", Intent::Neutral)];
        for e in self.folded_domains() {
            // 折叠标签形如 "五笔86 · 码表"：主文本取方案名（· 前段），徽章取引擎类型。
            let name = e
                .label
                .split_once(" · ")
                .map(|(n, _)| n.to_string())
                .unwrap_or_else(|| e.label.clone());
            // 每个方案域都带引擎类型徽章（统一中性色，不用强调色以免拼音一枝独秀）。
            let tag = engine_label(&e.kind);
            v.push(DropdownItem::new(name).badge(tag, Intent::Neutral));
        }
        v
    }

    /// 方案子标签的标题列表。
    pub fn sub_tab_titles(&self) -> Vec<String> {
        self.tabs_for_domain()
            .iter()
            .map(|t| t.title().to_string())
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
                // 分页类服务端按「编码前缀 ∪ 词条内容包含」并集匹配（dict/freq/phrase 一致）：
                // prefix 作编码前缀项，query 为兼容旧键的同值别名，都带上。
                p.insert("prefix".into(), json!(q));
                p.insert("query".into(), json!(q));
            }
        }
        if spec.paged {
            p.insert("offset".into(), json!(*self.offset.borrow()));
            p.insert("limit".into(), json!(PAGE_SIZE));
            // 服务端排序：跨页全局排序由后端完成（sortBy 为返回 item 的 json 字段名）。
            if let Some((col, ord)) = self.sort.get()
                && let Some(c) = spec.columns.get(col)
            {
                p.insert("sortBy".into(), json!(c.field.rpc_key()));
                let order = if ord == SortOrder::Asc { "asc" } else { "desc" };
                p.insert("sortOrder".into(), json!(order));
            }
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
                // 同时匹配原始文本与显示文本：候选调整里短语规则**显示的是模板原文**，
                // 而 `text` 是最后一次调整时的候选文本（见 `WordRow::display_text`）。
                // 只匹配其中一边，用户按屏幕上看到的内容搜就会搜不到。
                r.code.to_lowercase().contains(&q)
                    || r.text.to_lowercase().contains(&q)
                    || r.display_text().to_lowercase().contains(&q)
            })
            .cloned()
            .collect()
    }

    /// 重建表格数据（过滤 → 排序 → 写 display_rows/table_rows）并刷新分页状态文字。
    /// 分页类别由服务端排好序返回，这里不再排；非分页类别按当前 sort 客户端排序。
    pub fn rebuild_list(&self) {
        let mut rows = self.visible_rows();
        let spec = self.current_spec();
        let total = if spec.paged {
            self.total.get()
        } else {
            rows.len()
        };

        if !spec.paged
            && let Some((col, ord)) = self.sort.get()
            && let Some(c) = spec.columns.get(col)
        {
            sort_rows(&mut rows, c.field, ord);
        }

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
        self.empty_msg.set(if rows.is_empty() {
            self.empty_hint()
        } else {
            String::new()
        });

        let cells: Vec<Vec<String>> = rows
            .iter()
            .map(|r| spec.columns.iter().map(|c| r.cell_text(c.field)).collect())
            .collect();
        *self.display_rows.borrow_mut() = rows;
        self.table_rows.set(cells);
    }

    /// 按显示下标取当前显示行（操作列回调用；正文与 display_rows 同源同序）。
    pub fn display_row(&self, disp: usize) -> Option<WordRow> {
        self.display_rows.borrow().get(disp).cloned()
    }

    /// 取第 `disp` 行「启用」开关的复用 signal，并对齐到 `enabled` 当前值。
    ///
    /// 两处约束不体现在代码里，改动前先看这里：
    /// 1. **池只增不清**：`signal.rs` 的 arena 从不回收 slot（`free` 只 pop 从不 push），
    ///    清空池只会让后续行分配全新 slot，反而放大泄漏。池的只增不减正对应 arena 语义。
    /// 2. **仅在值变化时写**：`Signal::set` 无条件 `notify_changed()` 请求重绘，而本方法在
    ///    正文重建（layout 期）内调用——无条件写会自激成重绘循环，破坏空闲零 CPU。
    ///    值已一致时不写，故稳态收敛、不产生额外帧。
    ///
    /// 复用 signal 意味着行内容变化时旧值会被下一次重建纠正回数据侧的 `enabled`，
    /// 即写操作失败时开关自动回弹到真实值——正是期望行为。
    pub(super) fn enabled_sig(&self, disp: usize, enabled: bool) -> Signal<bool> {
        // 先取出句柄再比较：避免持有池的 borrow 时进入 signal 运行时。
        let sig = {
            let mut pool = self.enabled_sigs.borrow_mut();
            while pool.len() <= disp {
                pool.push(signal(enabled));
            }
            pool[disp]
        };
        if sig.get() != enabled {
            sig.set(enabled);
        }
        sig
    }

    /// 排序意图变化（点表头后由表格回调触发）：分页类别归零分页走服务端重拉，
    /// 非分页类别客户端重排当前数据。
    pub fn on_sort_changed(&self) {
        if self.current_spec().paged {
            *self.offset.borrow_mut() = 0;
            self.reload();
        } else {
            self.rebuild_list();
        }
    }

    /// 搜索框内容变化：分页类走服务端（归零分页+重载），非分页类走客户端重过滤。
    pub fn on_search_changed(&self) {
        if self.current_spec().paged {
            self.apply_search();
        } else {
            self.rebuild_list();
        }
    }

    /// 空表时的提示语。搜索词跨导航保留（见 [`Self::on_nav_changed`]），故必须把
    /// 「本来就没数据」和「有数据但被搜索词滤没了」分开说——否则切过来看到空表，
    /// 用户会以为这个类别是空的，而其实只是上一个标签的搜索词还挂着。
    fn empty_hint(&self) -> String {
        if self.current_spec().scoped_by_schema && self.current_schema_id().is_none() {
            return "暂无已启用方案".to_string();
        }
        let q = self.search.get();
        let q = q.trim();
        if !q.is_empty() {
            return format!("没有匹配「{q}」的内容（清除搜索可看全部）");
        }
        "暂无数据".to_string()
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
                        let max_off = if total == 0 {
                            0
                        } else {
                            (total - 1) / PAGE_SIZE * PAGE_SIZE
                        };
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
                self.notify_err(&format!("加载失败: {e}"));
                self.rebuild_list();
            }
        }
    }

    // ── 导航切换（切换后归零分页/搜索并刷新）─────────────────

    /// 注入方案列表但不触发 reload（供无表格视图的场景，如加词小窗，避免多余 list RPC）。
    pub(crate) fn set_schemas_no_reload(&self, schemas: &[SchemaInfo]) {
        *self.schemas.borrow_mut() = schemas.to_vec();
        self.domain_items.set(self.domain_labels());
        self.domain_rich_items.set(self.domain_rich_items());
        // 导入导出的目标下拉与域下拉同源，只是去掉首项「快捷短语」（短语走原生对话框，
        // 不是这套多类型导入的目标）。方案集变了就要重建，否则目标下拉会停在旧方案上。
        self.io_target_items.set(self.io_target_item_list());
        if self.io_target.get() >= self.folded_domains().len() {
            self.io_target.set(0);
        }
        // 域集合变化后，按下标记忆的子标签可能对不上位，清空避免错位恢复。
        self.sub_tab_memory.borrow_mut().clear();
        if self.domain.get() > self.folded_domains().len() {
            self.domain.set(0);
        }
        self.last_domain.set(self.domain.get());
    }

    /// 数据域总数（快捷短语 + 折叠后各方案域）。
    pub fn domain_count(&self) -> usize {
        1 + self.folded_domains().len()
    }

    /// 切到上一个数据域（循环，用于下拉右侧 ‹ 按钮）。
    pub fn prev_domain(&self) {
        let n = self.domain_count();
        if n <= 1 {
            return;
        }
        let cur = self.domain.get();
        let next = if cur == 0 { n - 1 } else { cur - 1 };
        self.domain.set(next);
    }

    /// 切到下一个数据域（循环，用于下拉右侧 › 按钮）。
    pub fn next_domain(&self) {
        let n = self.domain_count();
        if n <= 1 {
            return;
        }
        let cur = self.domain.get();
        let next = if cur + 1 >= n { 0 } else { cur + 1 };
        self.domain.set(next);
    }

    /// 注入方案列表（`AppState.apply_loaded` 后调用）。domain 越界则收敛回短语域。
    pub fn set_schemas(&self, schemas: &[SchemaInfo]) {
        self.set_schemas_no_reload(schemas);
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

    /// 导航（数据域/子标签）变化后：归零分页、重置排序、重载（**搜索词保留**），
    /// 并递增表格结构版本（列集随类别变化，视图据此整表重建）。
    /// 由 view 侧的响应式 effect 在 domain/sub_tab 信号变化时调用。
    pub fn on_nav_changed(&self) {
        let cur_domain = self.domain.get();
        let prev_domain = self.last_domain.replace(cur_domain);
        if cur_domain != prev_domain {
            // 切域：记住旧域当前子标签（此刻 sub_tab 仍是旧域的值），恢复新域上次停留的子标签
            // （无记录则回第一个）。restored 与当前不同时 set 会再触发一次本 effect（回声）——
            // 那次走"同域"分支，仅多做一次等价 reload，无副作用（换取逻辑简单、无抑制态可卡死）。
            self.sub_tab_memory
                .borrow_mut()
                .insert(prev_domain, self.sub_tab.get());
            let tabs_len = self.tabs_for_domain().len();
            let restored = self
                .sub_tab_memory
                .borrow()
                .get(&cur_domain)
                .copied()
                .unwrap_or(0)
                .min(tabs_len.saturating_sub(1));
            if restored != self.sub_tab.get() {
                self.sub_tab.set(restored);
            }
        } else {
            // 同域内切子标签：记住之，并做越界收敛。
            self.sub_tab_memory
                .borrow_mut()
                .insert(cur_domain, self.sub_tab.get());
            self.reset_sub_tab_if_out_of_range();
        }
        *self.offset.borrow_mut() = 0;
        // 搜索词**刻意跨导航保留**：搜索框自始至终是同一个 `search` 信号，用户在五笔的
        // 用户词库里搜「程序」再切到临时词库，想看的多半还是「程序」。切一次清一次，等于
        // 逼他每换一个标签就重打一遍。搜索词导致的空表由 `empty_hint` 显式说明，
        // 不会退化成分不清"没数据"还是"没匹配"。
        self.status.set(String::new());
        self.sort.set(None);
        let epoch = self.table_epoch.get().first().copied().unwrap_or(0);
        self.table_epoch.set(vec![epoch + 1]);
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
            self.notify_info("该操作不可用");
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
    ///
    /// **不带 candId**（与 pin 路径不同）：这里的 `text` 取自规则自身的 `word` 字段，
    /// 与库里存的完全一致，按 word 就能定位。唯一失配场景是「同一 code 下两条规则的 word
    /// 恰好相同」（两个不同短语当天求值出同样文本），此时会删错那一条——概率极低，而带上
    /// id 需要一路穿过 `PendingAction`（删除走确认流程），暂不做。真遇到再补。
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
        let msg = if self.current_spec().ops.remove_rule {
            "已撤销"
        } else {
            "已删除"
        };
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
        self.write(
            spec.rpc.promote_all,
            Value::Object(self.scoped_params()),
            "已全部转正",
        );
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
            self.write(
                spec.rpc.clear,
                Value::Object(self.scoped_params()),
                "已清空",
            );
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
        self.confirm_msg
            .set(format!("确定删除「{text}」？此操作不可恢复。"));
        self.confirm_visible.set(true);
    }

    pub fn request_clear(&self) {
        *self.pending.borrow_mut() = Some(PendingAction::Clear);
        let n = self.total.get();
        self.confirm_msg
            .set(format!("确定清空当前 {n} 条数据？此操作不可恢复。"));
        self.confirm_visible.set(true);
    }

    pub fn request_reset_default(&self) {
        *self.pending.borrow_mut() = Some(PendingAction::ResetDefault);
        self.confirm_msg
            .set("确定恢复为默认短语？将丢弃自定义短语的改动。".into());
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
            WordCategory::PhraseUser => ("添加短语", "编码", "短语", "1000"),
            _ => return,
        };
        *self.editing_orig.borrow_mut() = None;
        self.edit_code.set(String::new());
        self.edit_text.set(String::new());
        self.edit_weight.set(w.into());
        self.edit_position.set("0".into());
        self.edit_title.set(title.into());
        self.edit_l_code.set(lc.into());
        self.edit_l_text.set(lt.into());
        self.edit_visible.set(true);
    }

    /// 预填加词对话框（供加词小窗/快捷键使用）：定位 schema 所属域 → 用户词库子标签，
    /// 预填 词/编码/权重(1200)，`editing_orig=None`（新增），打开对话框。
    /// schema 命中规则：先精确匹配 rep_schema_id；未命中且为拼音变体时落拼音域；
    /// 命中域若无「用户词库」子标签（如混输仅候选调整），回退到首个含用户词库的域。
    pub fn open_add_word_prefilled(&self, text: &str, code: &str, schema: &str) {
        // 1) 目标域：走统一解析，另加「未知方案即拼音」的宽松兜底。
        //    这里的 schema 由 core 透传（可能是尚未列入已启用列表的双拼变体），
        //    与命令行深链不同——猜错只是落错域，不猜则连对话框都开不出来。
        let domain = self.resolve_domain(schema).or_else(|| {
            if schema.trim().is_empty() {
                None
            } else {
                self.folded_domains()
                    .iter()
                    .position(|d| d.kind == "pinyin")
                    .map(|i| i + 1)
            }
        });

        // 2) 该域是否含用户词库子标签；无则回退到首个含用户词库的方案域。
        let userdict_sub = |d: usize| -> Option<usize> {
            self.tabs_for_domain_at(d)
                .iter()
                .position(|t| t.category() == WordCategory::UserDict)
        };
        let (domain, sub) = domain
            .and_then(|d| userdict_sub(d).map(|s| (d, s)))
            .or_else(|| (1..self.domain_count()).find_map(|d| userdict_sub(d).map(|s| (d, s))))
            // 无任何方案域：退化到短语域，仍打开对话框（提交会因类别不支持而报错）
            .unwrap_or((0, 0));

        self.navigate_to(domain, Some(sub));

        *self.editing_orig.borrow_mut() = None;
        self.edit_code.set(code.to_string());
        self.edit_text.set(text.to_string());
        self.edit_weight.set("1200".to_string());
        self.edit_position.set("0".to_string());
        self.edit_title.set("添加用户词库".to_string());
        self.edit_l_code.set("编码".to_string());
        self.edit_l_text.set("词条".to_string());
        self.edit_visible.set(true);
    }

    /// 打开"编辑"对话框（回填字段，记录原始键）。
    /// `position` 仅短语用（用户词库无此维度，传 0 即可）。
    pub fn open_edit(&self, code: &str, text: &str, weight: i64, position: i64) {
        let (title, lc, lt) = match self.current_category() {
            WordCategory::UserDict => ("编辑词条", "编码", "词条"),
            WordCategory::PhraseUser => ("编辑短语", "编码", "短语"),
            _ => return,
        };
        *self.editing_orig.borrow_mut() = Some((code.into(), text.into()));
        self.edit_code.set(code.into());
        self.edit_text.set(text.into());
        self.edit_weight.set(weight.to_string());
        self.edit_position.set(position.to_string());
        self.edit_title.set(title.into());
        self.edit_l_code.set(lc.into());
        self.edit_l_text.set(lt.into());
        self.edit_visible.set(true);
    }

    pub fn cancel_edit(&self) {
        self.edit_visible.set(false);
    }

    /// 打开系统短语只读查看对话框（写入独立 view_* 信号，不污染 edit_*）。
    pub fn open_view(&self, code: &str, text: &str, weight: i64, position: i64) {
        self.view_code.set(code.into());
        self.view_text.set(text.into());
        self.view_weight.set(weight.to_string());
        self.view_position.set(position.to_string());
        *self.viewing.borrow_mut() = Some((code.into(), text.into(), weight, position));
        self.view_visible.set(true);
    }

    pub fn cancel_view(&self) {
        self.view_visible.set(false);
        *self.viewing.borrow_mut() = None;
    }

    /// 查看对话框里的「转为用户短语」：按当前查看的那条原值转归属，成功后关闭对话框
    /// （该条随即从系统短语列表消失、出现在用户短语列表，留着对话框只会显示一条已不在本列表的记录）。
    pub fn claim_viewing_as_user(&self) {
        let Some((code, text, weight, position)) = self.viewing.borrow().clone() else {
            return;
        };
        self.claim_phrase_as_user(&code, &text, weight, position);
        self.cancel_view();
    }

    /// 由"出码"按钮触发：用户词库下按词条反查编码填入编码框。
    pub fn encode_current(&self) {
        if self.current_category() != WordCategory::UserDict {
            return;
        }
        let text = self.edit_text.get();
        if text.trim().is_empty() {
            self.notify_info("请先填写词条");
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
                // 次序：同编码同权重时才起作用（主程序按 weight 降序 → position 升序排）。
                // 新增与编辑都要带上——此前 add 写死 0、update 根本不发，用户在导入的
                // wdict 里设好的 position 一经设置页保存就被抹平。
                let position: i64 = match self.edit_position.get().trim().parse() {
                    Ok(p) => p,
                    Err(_) => return Err("次序需为整数".into()),
                };
                match orig {
                    Some((oc, ot)) => {
                        let p = json!({
                            "code": oc, "text": ot,
                            "newCode": code, "newText": text,
                            "weight": weight, "position": position
                        });
                        Ok((vec![(spec.rpc.update, p)], "已更新"))
                    }
                    None => {
                        let p = json!({
                            "code": code, "text": text,
                            "position": position, "weight": weight
                        });
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

    /// 把一条系统短语转为用户短语（原样复制其编码/内容/权重/次序）。
    ///
    /// 主程序的短语表主键只有 `(code, text)` 一把，`phrase.add` 撞上同键的系统行时即把该行
    /// 转为用户行并记下遮蔽关系——所以「转为用户短语」不需要专门的 RPC，用该行现值调一次
    /// `phrase.add` 就是它本身的语义（内容一字不改，只换归属）。
    ///
    /// 转换后：该条从系统短语列表隐去、出现在用户短语列表（带「覆盖」标记），可自由编辑；
    /// 想还原走系统短语的「恢复默认」。
    pub fn claim_phrase_as_user(&self, code: &str, text: &str, weight: i64, position: i64) {
        let p = json!({
            "code": code, "text": text,
            "weight": weight, "position": position
        });
        match rpc::call_core("phrase.add", &p) {
            Ok(_) => {
                self.reload();
                self.notify_ok("已转为用户短语");
            }
            Err(e) => self.notify_err(&format!("转换失败: {e}")),
        }
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
        self.shadow_cand_id.borrow_mut().clear(); // 手动新增没有候选实体，无稳定 id
        self.shadow_add_visible.set(true);
    }

    /// 打开候选调整编辑对话框（预填该行现值，记录原始键）。
    /// kind 为 "delete" 映射到隐藏(1)，其余（"pin"）映射到固定位置(0)。
    /// 提交走 `submit_shadow` → `shadow_save_calls`：键不变 addRule upsert 覆盖，
    /// 改了 code/word 则加新 + 删旧原子替换。
    /// `cand_id` 为该规则的候选稳定 id（短语规则非空），提交时原样回传，见 `shadow_cand_id`。
    /// `word` 传的是规则里的**原始文本**（不是列表显示的模板原文）——它是规则的实际字段，
    /// 换成模板提交后会把 word 写成模板串。
    pub fn open_shadow_edit(
        &self,
        code: &str,
        word: &str,
        kind: &str,
        position: i64,
        cand_id: &str,
    ) {
        self.shadow_title.set("编辑候选调整".into());
        self.shadow_code.set(code.into());
        self.shadow_word.set(word.into());
        self.shadow_type.set(if kind == "delete" { 1 } else { 0 });
        self.shadow_pos.set(position.to_string());
        *self.shadow_orig.borrow_mut() = Some((code.into(), word.into()));
        *self.shadow_cand_id.borrow_mut() = cand_id.to_string();
        self.shadow_add_visible.set(true);
    }

    pub fn cancel_shadow_add(&self) {
        self.shadow_add_visible.set(false);
    }

    /// 导出调用（用户短语）。返回 RPC 方法名（内容由 view 层拿到后写文件）。
    pub fn export_method(&self) -> &'static str {
        "phrase.export"
    }

    /// 导出的默认文件名：`<来源>-<时间戳>.wdict.yaml`。短语与方案级导出共用同一套命名。
    fn export_file_name(source: &str) -> String {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        format!("{source}-{stamp}.wdict.yaml")
    }

    /// 导入调用参数（内容来自文件）。
    pub fn import_params(&self, content: &str) -> Value {
        json!({ "content": content })
    }

    /// 导出用户短语到文件（native save 对话框）。
    ///
    /// 对话框请求经 `ctx.request_save_file` 交给宿主，待本次事件分发完全返回
    /// （OS 鼠标捕获已同步）后再真正弹出——绝不能在这个点击回调里同步调用
    /// `PickDialog::save_file()`，那会在 OS 捕获尚未释放时重入模态对话框的
    /// 消息泵，反复开关几次就会把鼠标彻底锁死。
    pub fn do_export_phrases(&self, ctx: &mut EventCtx) {
        let state = self.clone();
        ctx.request_save_file(
            PickDialog::new()
                .title("导出 · 用户短语")
                // 带时间戳，与方案级导出同一套命名：连着导两次不会互相覆盖，
                // 攒了一堆备份时也能一眼看出先后。
                .file_name(Self::export_file_name("phrases"))
                .filter("WindDict", &["yaml"]),
            move |path| {
                let Some(path) = path else { return }; // 用户取消
                match rpc::call_core(state.export_method(), &json!({})) {
                    Ok(v) => {
                        let content = v.get("content").and_then(|c| c.as_str()).unwrap_or("");
                        match std::fs::write(&path, content) {
                            Ok(_) => state.notify_ok("已导出"),
                            Err(e) => state.notify_err(&format!("写文件失败: {e}")),
                        }
                    }
                    Err(e) => state.notify_err(&format!("导出失败: {e}")),
                }
            },
        );
    }

    /// 从文件导入用户短语（native open 对话框 + 合并）。同 `do_export_phrases`，
    /// 对话框请求延迟到事件分发返回后才执行，避免重入导致鼠标捕获错位。
    pub fn do_import_phrases(&self, ctx: &mut EventCtx) {
        let state = self.clone();
        ctx.request_pick_file(
            PickDialog::new()
                .title("导入 · 用户短语")
                .filter("WindDict", &["yaml"]),
            move |path| {
                let Some(path) = path else { return };
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(e) => {
                        state.notify_err(&format!("读文件失败: {e}"));
                        return;
                    }
                };
                match rpc::call_core("phrase.import", &state.import_params(&content)) {
                    Ok(v) => {
                        let n = v.get("imported").and_then(|x| x.as_u64()).unwrap_or(0);
                        let m = v.get("skipped").and_then(|x| x.as_u64()).unwrap_or(0);
                        state.reload();
                        state.notify_ok(&format!("已导入 {n}（跳过 {m}）"));
                    }
                    Err(e) => state.notify_err(&format!("导入失败: {e}")),
                }
            },
        );
    }

    // ── 用户词库导入导出（按当前方案,dict.export/previewImport/import）──

    /// 子标签行「导入」入口：短语域走原生对话框；方案域打开导入对话框。
    pub fn do_import(&self, ctx: &mut EventCtx) {
        if matches!(self.domain_kind(), DomainKind::Phrase) {
            self.do_import_phrases(ctx);
        } else {
            self.open_import_dialog();
        }
    }

    /// 子标签行「导出」入口：短语域走原生对话框；方案域打开导出对话框。
    pub fn do_export(&self, ctx: &mut EventCtx) {
        if matches!(self.domain_kind(), DomainKind::Phrase) {
            self.do_export_phrases(ctx);
        } else {
            self.open_export_dialog();
        }
    }

    // ── 方案级「导入/导出」统一对话框（多类型可选；对齐 Go 融合式对话框）──

    /// 当前方案域适用的数据类型 (key, 标签, 勾选信号)。短语/无方案域返回空。
    pub fn io_section_defs(&self) -> Vec<(&'static str, &'static str, Signal<bool>)> {
        let all = [
            ("userWords", "用户词库", self.io_user_words),
            ("tempWords", "临时词库", self.io_temp_words),
            ("freq", "词频", self.io_freq),
            ("shadow", "候选调整", self.io_shadow),
        ];
        all.into_iter()
            .filter(|(k, _, _)| self.io_type_applicable(k))
            .collect()
    }

    /// 某数据类型是否适用于**导入导出的目标方案**（注意：不是当前浏览的域）。
    pub fn io_type_applicable(&self, key: &str) -> bool {
        Self::types_for_kind(self.io_target_kind()).contains(&key)
    }

    /// 某引擎类型的方案能承载哪些数据类型（与 `tabs_for_domain_at` 的标签集一一对应）。
    fn types_for_kind(kind: DomainKind) -> &'static [&'static str] {
        match kind {
            DomainKind::Pinyin => &["userWords", "tempWords", "freq"],
            DomainKind::Codetable | DomainKind::Other => {
                &["userWords", "tempWords", "freq", "shadow"]
            }
            // 与 tabs_for_domain_at 保持一致：英文无临时词库。
            DomainKind::English => &["userWords", "freq", "shadow"],
            // 混输方案自身只有候选调整——五笔/拼音词条落在它的成员方案里。
            DomainKind::Mixed => &["shadow"],
            DomainKind::Phrase => &[],
        }
    }

    /// 当前勾选且适用的数据类型 key 列表。
    pub fn io_selected_sections(&self) -> Vec<String> {
        self.io_section_defs()
            .into_iter()
            .filter(|(_, _, sig)| sig.get())
            .map(|(k, _, _)| k.to_string())
            .collect()
    }

    /// 子标签行「导入/导出」按钮是否显示：方案域（有方案）或短语域。
    pub fn io_entry_visible(&self) -> bool {
        match self.domain_kind() {
            DomainKind::Phrase => true,
            _ => self.current_schema_id().is_some(),
        }
    }

    /// 「导入/导出」按钮是否可用：短语域仅「用户短语」子标签可用（导入导出的是用户短语，
    /// 系统短语只读故禁用）；方案域有方案即可用。
    pub fn io_entry_enabled(&self) -> bool {
        match self.domain_kind() {
            DomainKind::Phrase => self.current_category() == WordCategory::PhraseUser,
            _ => self.current_schema_id().is_some(),
        }
    }

    // ── 导入导出的目标方案（`io_target` 索引进 folded_domains）─────

    /// 目标方案下拉选项（去掉域下拉首项「快捷短语」，其余同序）。
    fn io_target_item_list(&self) -> Vec<DropdownItem> {
        self.domain_rich_items().into_iter().skip(1).collect()
    }

    /// 目标方案 id（无方案时 None）。
    pub fn io_target_schema_id(&self) -> Option<String> {
        self.folded_domains()
            .get(self.io_target.get())
            .map(|e| e.rep_schema_id.clone())
    }

    /// 目标方案的引擎类型（无方案时按 `Other` 从宽，不隐藏数据）。
    pub fn io_target_kind(&self) -> DomainKind {
        match self.io_target.get().checked_add(1) {
            Some(d) if d < self.domain_count() => self.domain_kind_at(d),
            _ => DomainKind::Other,
        }
    }

    /// 目标方案展示名（如「五笔86 · 码表」）。
    pub fn io_target_label(&self) -> String {
        self.folded_domains()
            .get(self.io_target.get())
            .map(|e| e.label.clone())
            .unwrap_or_default()
    }

    /// 找到首个引擎类型为 `engine` 的方案域下标（用于按文件来源自动选目标）。
    fn io_target_by_engine(&self, engine: &str) -> Option<usize> {
        self.folded_domains().iter().position(|d| d.kind == engine)
    }

    /// 导出对话框的引擎提示：导出文件带来源方案与引擎标记，只能导回同引擎的方案。
    ///
    /// 这条要在**导出时**就说清楚，而不是等用户把五笔词库喂给拼音方案再报错——那时
    /// 文件已经传给别人、或者已经躺在备份盘里半年了，回头翻查代价大得多。
    fn io_engine_note_for_target(&self) -> String {
        let engine = match self.io_target_kind() {
            DomainKind::Pinyin => "pinyin",
            DomainKind::Codetable => "codetable",
            DomainKind::Mixed => "mixed",
            DomainKind::English => "english",
            _ => return String::new(),
        };
        let label = engine_label(engine);
        format!("文件会记录引擎类型（{label}），只能导回{label}类方案。")
    }

    /// 目标方案的作用域说明：只对「自身承载的数据类型明显少于用户预期」的方案给提示。
    /// 混输是唯一这样的——用户看到"五笔拼音混输"会以为词库在这里，实际只有候选调整。
    fn io_scope_note_for_target(&self) -> String {
        if self.io_target_kind() == DomainKind::Mixed {
            format!(
                "「{}」自身只有候选调整，五笔/拼音词条存放在它的成员方案里。要导入导出词条，请在上方改选对应方案。",
                self.io_target_label()
            )
        } else {
            String::new()
        }
    }

    /// 勾选重置为全部适用类型。
    fn reset_io_checks(&self) {
        self.io_user_words.set(true);
        self.io_temp_words.set(true);
        self.io_freq.set(true);
        self.io_shadow.set(true);
    }

    /// 打开对话框前的共用准备：目标默认取当前浏览的方案域，刷新选项/说明与勾选。
    fn prepare_io_dialog(&self, verb: &str) -> bool {
        if self.folded_domains().is_empty() {
            self.notify_err("暂无已启用方案");
            return false;
        }
        self.io_target_items.set(self.io_target_item_list());
        self.io_target.set(self.domain.get().saturating_sub(1));
        self.io_auto_note.set(String::new());
        self.io_auto_at.set(None);
        self.io_scope_note.set(self.io_scope_note_for_target());
        self.io_engine_note.set(self.io_engine_note_for_target());
        self.reset_io_checks();
        self.io_title
            .set(format!("{verb} · {}", self.io_target_label()));
        true
    }

    /// 目标方案变化（下拉选择或自动匹配）后的同步：标题/说明/勾选/预览重算。
    /// 由 view 侧监听 `io_target` 版本的 effect 调用——windui 的下拉只写信号、无变更回调。
    pub fn on_io_target_changed(&self) {
        let importing = self.import_confirm_visible.get();
        self.io_title.set(format!(
            "{} · {}",
            if importing { "导入" } else { "导出" },
            self.io_target_label()
        ));
        self.io_scope_note.set(self.io_scope_note_for_target());
        self.io_engine_note.set(self.io_engine_note_for_target());
        // 自动提示只对它当初指向的那个目标有效；用户手动改走后即失效。
        if self.io_auto_at.get() != Some(self.io_target.get()) {
            self.io_auto_note.set(String::new());
            self.io_auto_at.set(None);
        }
        if !importing {
            self.reset_io_checks();
            return;
        }
        // 导入：换目标等于换了兼容性判定与"能收哪些段"，必须按新目标重新预览。
        let content = self.pending_import.borrow().clone();
        match content {
            Some(c) => self.run_import_preview(&c, false),
            None => self.reset_io_checks(),
        }
    }

    /// 打开导出对话框（方案域）。
    pub fn open_export_dialog(&self) {
        if !self.prepare_io_dialog("导出") {
            return;
        }
        self.export_visible.set(true);
    }

    /// 关闭导出对话框。
    pub fn cancel_export(&self) {
        self.export_visible.set(false);
    }

    /// 打开导入对话框（方案域）：清空预览、勾选默认全开。
    pub fn open_import_dialog(&self) {
        if !self.prepare_io_dialog("导入") {
            return;
        }
        self.import_file_name.set(String::new());
        self.import_preview_msg.set(String::new());
        self.import_ready.set(false);
        self.import_compatible.set(true);
        *self.pending_import.borrow_mut() = None;
        self.import_confirm_visible.set(true);
    }

    /// 导出勾选的数据类型为单个 .wdict 文件（对话框异步请求，见 `do_export_phrases` 注释）。
    pub fn do_export_io(&self, ctx: &mut EventCtx) {
        let Some(schema) = self.io_target_schema_id() else {
            self.notify_err("暂无已启用方案");
            return;
        };
        let sections = self.io_selected_sections();
        if sections.is_empty() {
            self.notify_err("请至少勾选一种数据类型");
            return;
        }
        let state = self.clone();
        ctx.request_save_file(
            PickDialog::new()
                .title("导出词库数据")
                .file_name(Self::export_file_name(&schema))
                .filter("WindDict", &["yaml"]),
            move |path| {
                let Some(path) = path else { return };
                match rpc::call_core(
                    "dict.export",
                    &json!({ "schemaId": schema, "sections": sections }),
                ) {
                    Ok(v) => {
                        let content = v.get("content").and_then(|c| c.as_str()).unwrap_or("");
                        match std::fs::write(&path, content) {
                            Ok(_) => state.notify_ok("已导出"),
                            Err(e) => state.notify_err(&format!("写文件失败: {e}")),
                        }
                    }
                    Err(e) => state.notify_err(&format!("导出失败: {e}")),
                }
            },
        );
    }

    /// 导入对话框内选文件：读文件 → `dict.previewImport`（格式内容自动识别）→ 预览回显。
    pub fn pick_import_file(&self, ctx: &mut EventCtx) {
        let state = self.clone();
        ctx.request_pick_file(
            PickDialog::new()
                .title("选择词库文件")
                .filter("词库文件 (WindDict/Rime/TSV)", &["yaml", "txt"]),
            move |path| {
                let Some(path) = path else { return };
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(e) => {
                        state.notify_err(&format!("读文件失败: {e}"));
                        return;
                    }
                };
                state.import_file_name.set(name);
                *state.pending_import.borrow_mut() = Some(content.clone());
                state.run_import_preview(&content, true);
            },
        );
    }

    /// 按当前目标方案预览 `content`，把结果落到预览相关信号。
    ///
    /// `allow_redirect` 仅在「刚选完文件」时为 true：目标收不下这个文件时，按文件自报的
    /// 来源引擎去已启用方案里找一个能收的，找到就把目标改过去——用户的意图是「把这个词库
    /// 装进输入法」，而不是「往我当前正看着的那个域里塞」。混输用户正卡在这上面：混输方案
    /// 只有候选调整，不改目标就只能眼看着预览写了 2000 条、导完一条也打不出来。
    ///
    /// 改目标后本方法**不自行重跑预览**：`io_target` 变化会被 view 的 effect 接住，
    /// 走 [`Self::on_io_target_changed`] 重新预览（`allow_redirect=false`，不会来回弹）。
    fn run_import_preview(&self, content: &str, allow_redirect: bool) {
        let Some(schema) = self.io_target_schema_id() else {
            self.import_preview_msg.set("暂无已启用方案".into());
            self.import_ready.set(false);
            return;
        };
        match rpc::call_core(
            "dict.previewImport",
            &json!({ "schemaId": schema, "content": content }),
        ) {
            Ok(v) => {
                let compatible = v
                    .get("compatible")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(true);
                if allow_redirect
                    && !compatible
                    && let Some(src) = v.get("sourceEngine").and_then(|x| x.as_str())
                    && let Some(i) = self.io_target_by_engine(src)
                    && i != self.io_target.get()
                {
                    self.io_target.set(i);
                    self.io_auto_at.set(Some(i));
                    self.io_auto_note.set(format!(
                        "已按文件内容把目标切换为「{}」",
                        self.io_target_label()
                    ));
                    return;
                }
                self.import_compatible.set(compatible);
                self.apply_import_preview_sections(&v);
                self.import_preview_msg
                    .set(self.format_import_preview_for_target(&v));
                // 不兼容（如五笔词库导入拼音方案）：保留内容但禁止导入。
                self.import_ready.set(compatible);
            }
            Err(e) => {
                self.import_preview_msg.set(format!("无法导入：{e}"));
                *self.pending_import.borrow_mut() = None;
                self.import_ready.set(false);
            }
        }
    }

    /// 数据类型 key → 对应子标签（导入后把视图落到刚写入的那一类上）。
    fn tab_for_section(key: &str) -> Option<LogicalTab> {
        match key {
            "userWords" => Some(LogicalTab::UserDict),
            "tempWords" => Some(LogicalTab::Temp),
            "freq" => Some(LogicalTab::Freq),
            "shadow" => Some(LogicalTab::Shadow),
            _ => None,
        }
    }

    /// 数据类型 key → 中文标签。
    fn section_label(key: &str) -> &'static str {
        match key {
            "userWords" => "用户词库",
            "tempWords" => "临时词库",
            "freq" => "词频",
            "shadow" => "候选调整",
            _ => "数据",
        }
    }

    /// 依预览响应里的 sections，把适用类型的勾选设为「文件是否含该段」。
    fn apply_import_preview_sections(&self, v: &Value) {
        let present = |key: &str| {
            v.get("sections")
                .and_then(|s| s.as_array())
                .map(|arr| {
                    arr.iter()
                        .any(|x| x.get("key").and_then(|k| k.as_str()) == Some(key))
                })
                .unwrap_or(false)
        };
        self.io_user_words.set(present("userWords"));
        self.io_temp_words.set(present("tempWords"));
        self.io_freq.set(present("freq"));
        self.io_shadow.set(present("shadow"));
    }

    /// previewImport 响应里的来源标记 → 「五笔86（码表）」这样的一句话；两项都缺则 None。
    ///
    /// 导出文件带方案 id 与引擎类型（core 侧写入），这里只是把它翻出来给人看。
    fn source_desc(v: &Value) -> Option<String> {
        let schema = v
            .get("sourceSchema")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        let engine = v
            .get("sourceEngine")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        match (schema.is_empty(), engine.is_empty()) {
            (true, true) => None,
            (false, false) => Some(format!("「{schema}」（{}）", engine_label(engine))),
            (false, true) => Some(format!("「{schema}」")),
            (true, false) => Some(format!("「{}」类词库", engine_label(engine))),
        }
    }

    /// 用当前目标方案的口径格式化预览正文。
    fn format_import_preview_for_target(&self, v: &Value) -> String {
        Self::format_import_preview(
            v,
            Self::types_for_kind(self.io_target_kind()),
            &self.io_target_label(),
        )
    }

    /// previewImport 响应 → 对话框预览正文（纯函数，便于单测）。
    ///
    /// 不兼容（来源引擎≠目标方案）优先提示并阻止；否则**按目标方案能不能收把段分成两拨**：
    /// 能收的列数量，收不了的单列一行明说「不会导入」。此前两拨混在一起列，于是混输方案下
    /// 预览写着「用户词库 2000」、导入按钮可点、toast 报成功，实际 `io_selected_sections`
    /// 把这段过滤掉了，一条也没进——用户只能得出"导了但打不出来"的结论。
    fn format_import_preview(v: &Value, applicable: &[&str], target_label: &str) -> String {
        let mut s = String::new();
        // 不兼容优先提示（如五笔词库导入拼音方案）。
        if !v
            .get("compatible")
            .and_then(|x| x.as_bool())
            .unwrap_or(true)
        {
            let src = engine_label(v.get("sourceEngine").and_then(|x| x.as_str()).unwrap_or(""));
            let tgt = engine_label(v.get("targetEngine").and_then(|x| x.as_str()).unwrap_or(""));
            s.push_str(&format!(
                "⚠ 该文件为「{src}」类型词库，与目标「{tgt}」方案不一致，无法导入（编码不通用）。\n请在上方「导入到」改选一个{src}方案。"
            ));
            return s;
        }
        let fmt_name = match v.get("format").and_then(|x| x.as_str()).unwrap_or("") {
            "rime" => "Rime 词库",
            "tsv" => "TSV 文本",
            "winddict" => "WindDict 词库",
            _ => "",
        };
        if !fmt_name.is_empty() {
            s.push_str(&format!("识别为 {fmt_name}"));
            // 兼容时也把来源方案/引擎摆出来：导出文件本就带这个标记，导入前让用户自己
            // 核对一眼「我拿的是不是这套编码的词库」，比事后发现打不出来再回溯便宜得多。
            // （不兼容时上面那支已经把两边引擎说清了，不重复。）
            if let Some(desc) = Self::source_desc(v) {
                s.push_str(&format!(" · 来源{desc}"));
            }
            s.push('\n');
        }
        let mut ok: Vec<String> = Vec::new();
        let mut skipped_sections: Vec<String> = Vec::new();
        let mut skipped_total = 0u64;
        if let Some(arr) = v.get("sections").and_then(|x| x.as_array()) {
            for sec in arr {
                let key = sec.get("key").and_then(|x| x.as_str()).unwrap_or("");
                let count = sec.get("count").and_then(|x| x.as_u64()).unwrap_or(0);
                skipped_total += sec.get("skipped").and_then(|x| x.as_u64()).unwrap_or(0);
                let part = format!("{} {}", Self::section_label(key), count);
                if applicable.contains(&key) {
                    ok.push(part);
                } else {
                    skipped_sections.push(part);
                }
            }
        }
        if ok.is_empty() && skipped_sections.is_empty() {
            s.push_str("文件不含可导入的词库数据");
            return s;
        }
        if ok.is_empty() {
            s.push_str(&format!(
                "⚠ 文件里的 {} 都不属于「{target_label}」，一条也不会导入。\n请在上方「导入到」改选目标方案。",
                skipped_sections.join(" · ")
            ));
            return s;
        }
        s.push_str(&format!("可导入：{}", ok.join(" · ")));
        if skipped_total > 0 {
            s.push_str(&format!(" · 跳过 {skipped_total} 行"));
        }
        if !skipped_sections.is_empty() {
            s.push_str(&format!(
                "\n⚠ 不会导入：{}（「{target_label}」不含此类数据）",
                skipped_sections.join(" · ")
            ));
        }
        s.push_str("\n\n勾选上方类型决定导入哪些；合并保留本地已有，替换先清空所选类型后导入。");
        s
    }

    /// 导入按钮是否可点：预览通过 **且** 至少有一种目标能收的类型被勾选。
    ///
    /// 后半个条件是这次修复的要害——`io_selected_sections` 会把目标收不了的段过滤掉，
    /// 过滤到空还照发 `dict.import`，后端拿一个空 sections 回一句成功，界面弹绿 toast。
    /// 「点了、报成功、零条」是最难自查的一类假象，宁可让按钮灰着。
    pub fn import_enabled(&self) -> bool {
        self.import_ready.get() && !self.io_selected_sections().is_empty()
    }

    /// 确认导入（strategy: "merge" | "replace"）：按勾选的数据类型导入。
    pub fn confirm_import(&self, strategy: &str) {
        let sections = self.io_selected_sections();
        if sections.is_empty() {
            self.notify_err("没有可导入的数据类型：请改选目标方案或勾选类型");
            return;
        }
        self.import_confirm_visible.set(false);
        self.import_ready.set(false);
        let Some(content) = self.pending_import.borrow_mut().take() else {
            return;
        };
        let Some(schema) = self.io_target_schema_id() else {
            return;
        };
        // 先记下要落到哪个子标签（`sections` 随即被 json! 取走）。
        let landing = Self::tab_for_section(sections.first().map(String::as_str).unwrap_or(""));
        match rpc::call_core(
            "dict.import",
            &json!({ "schemaId": schema, "content": content, "strategy": strategy, "sections": sections }),
        ) {
            Ok(v) => {
                // 导入目标可能不是当前正看着的域（自动匹配或手动改过）。留在原地只会让人
                // 再次得出"导了但没有"的结论——把视图带到刚写入的方案与类型上，眼见为实。
                let target_domain = self.io_target.get() + 1;
                let sub = landing.and_then(|t| {
                    self.tabs_for_domain_at(target_domain)
                        .iter()
                        .position(|x| *x == t)
                });
                self.navigate_to(target_domain, sub);
                self.reload();
                self.notify_ok(&Self::format_import_result(&v));
            }
            Err(e) => self.notify_err(&format!("导入失败: {e}")),
        }
    }

    /// dict.import 响应 → toast 文案（逐段汇总，纯函数便于单测）。
    fn format_import_result(v: &Value) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(arr) = v.get("sections").and_then(|x| x.as_array()) {
            for sec in arr {
                let key = sec.get("key").and_then(|x| x.as_str()).unwrap_or("");
                let n = |k: &str| sec.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
                if sec.get("added").is_some() {
                    parts.push(format!(
                        "{} 新增 {}·更新 {}",
                        Self::section_label(key),
                        n("added"),
                        n("updated")
                    ));
                } else {
                    parts.push(format!("{} {} 条", Self::section_label(key), n("imported")));
                }
            }
        }
        if parts.is_empty() {
            "已导入".to_string()
        } else {
            format!("已导入：{}", parts.join("；"))
        }
    }

    pub fn cancel_import(&self) {
        *self.pending_import.borrow_mut() = None;
        self.import_ready.set(false);
        self.import_confirm_visible.set(false);
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
        // 编辑既有短语规则时原样回传稳定 id：这条路会先匹配掉旧规则再插新的，
        // 不带 id 就把它擦了（次日失配，见 `shadow_cand_id`）。
        let cand_id = self.shadow_cand_id.borrow().clone();
        if !cand_id.is_empty() {
            p.insert("candId".into(), json!(cand_id));
        }
        if self.shadow_type.get() == 0 {
            let pos: i64 = self
                .shadow_pos
                .get()
                .trim()
                .parse()
                .map_err(|_| "位置需为整数")?;
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

    /// 预览格式化的目标口径（与 `types_for_kind` 同源，测试里直接写出以固定预期）。
    const CODETABLE_TYPES: &[&str] = &["userWords", "tempWords", "freq", "shadow"];
    const PINYIN_TYPES: &[&str] = &["userWords", "tempWords", "freq"];
    const MIXED_TYPES: &[&str] = &["shadow"];

    fn schema(id: &str, name: &str, kind: &str) -> SchemaInfo {
        SchemaInfo {
            id: id.into(),
            name: name.into(),
            kind: kind.into(),
            ..Default::default()
        }
    }

    fn schema_s(id: &str, name: &str, kind: &str, scheme: &str) -> SchemaInfo {
        SchemaInfo {
            id: id.into(),
            name: name.into(),
            kind: kind.into(),
            scheme: scheme.into(),
            hidden: false,
        }
    }

    /// 直接写 schemas/domain，不经 set_schemas（避免触发 rpc）。
    fn with_schemas(st: &DictManagerState, list: Vec<SchemaInfo>) {
        *st.schemas.borrow_mut() = list;
    }

    /// 同一显示下标跨重建复用同一 signal——池化的核心性质。
    /// 若退化为逐行新建，写 a 不会被 b 读到，signal arena 将随每次重建增长。
    #[test]
    fn enabled_sig_reuses_same_signal_per_index() {
        let st = DictManagerState::new();
        let a = st.enabled_sig(0, true);
        let b = st.enabled_sig(0, true); // 模拟正文重建时再次生成同一行
        a.set(false);
        assert!(!b.get(), "同下标应复用同一 signal（b 应读到经 a 写入的值）");
    }

    /// 不同下标是彼此独立的 signal（避免池化把各行开关串成一个）。
    #[test]
    fn enabled_sig_distinct_across_indices() {
        let st = DictManagerState::new();
        let r0 = st.enabled_sig(0, true);
        let r1 = st.enabled_sig(1, true);
        r0.set(false);
        assert!(r1.get(), "不同下标不应互相影响");
    }

    /// 重建时把复用的 signal 对齐到数据侧当前值：
    /// 翻页/搜索后同一下标换成了另一行，或写操作失败需回弹，均依赖这个同步。
    #[test]
    fn enabled_sig_syncs_to_row_value_on_rebuild() {
        let st = DictManagerState::new();
        let first = st.enabled_sig(0, true);
        assert!(first.get());
        let reused = st.enabled_sig(0, false); // 该下标现在是 enabled=false 的行
        assert!(!reused.get(), "复用的 signal 应对齐到数据侧当前值");
    }

    /// 回归守护：正文反复重建时池不增长——这正是本次修复的目的。
    /// 修复前每次重建逐行新建 signal，arena 只增不减，系统短语每敲一键即泄漏一批 slot。
    #[test]
    fn enabled_sig_pool_bounded_across_rebuilds() {
        let st = DictManagerState::new();
        for _ in 0..100 {
            // 模拟 100 次正文重建（≈ 搜索框连打 100 键），每次 10 行
            for disp in 0..10 {
                let _ = st.enabled_sig(disp, true);
            }
        }
        assert_eq!(
            st.enabled_sigs.borrow().len(),
            10,
            "池应封顶为最大行数，不随重建次数增长"
        );
    }

    /// 池按需扩容到请求的下标，且扩容不丢已有行的值。
    #[test]
    fn enabled_sig_pool_grows_and_preserves() {
        let st = DictManagerState::new();
        let r0 = st.enabled_sig(0, true);
        let _ = st.enabled_sig(5, false); // 跳跃下标触发扩容
        assert!(r0.get(), "扩容不应影响已有下标的 signal");
        assert_eq!(st.enabled_sigs.borrow().len(), 6);
    }

    #[test]
    fn prefill_selects_userdict_domain_for_pinyin() {
        let st = DictManagerState::new();
        with_schemas(
            &st,
            vec![
                schema("wubi", "五笔", "codetable"),
                schema_s("quanpin", "全拼", "pinyin", "full"),
            ],
        );
        st.open_add_word_prefilled("你好", "nihao", "quanpin");
        // 命中拼音域（domain>=1），子标签落在用户词库
        assert_eq!(st.current_category(), WordCategory::UserDict);
        assert_eq!(st.edit_text.get(), "你好");
        assert_eq!(st.edit_code.get(), "nihao");
        assert_eq!(st.edit_weight.get(), "1200");
        assert!(st.edit_visible.get());
        assert_eq!(st.current_schema_id().as_deref(), Some("quanpin"));
    }

    #[test]
    fn prefill_matches_pinyin_alias() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema_s("quanpin", "全拼", "pinyin", "full")]);
        // core 传来的双拼 id 归一到拼音域代表
        st.open_add_word_prefilled("好", "hao", "shuangpin_ziranma");
        assert_eq!(st.current_category(), WordCategory::UserDict);
    }

    // ── 程序化定位（命令行深链）──────────────────────────────

    /// 三条方案：五笔（码表）+ 全拼 + 自然码（双拼，会被折叠进拼音域）。
    /// 折叠后数据域为 [0]快捷短语 [1]五笔86·码表 [2]拼音。
    fn deep_link_fixture() -> DictManagerState {
        let st = DictManagerState::new();
        with_schemas(
            &st,
            vec![
                schema("wb", "五笔86", "codetable"),
                schema_s("quanpin", "全拼", "pinyin", "full"),
                schema_s("ziranma", "自然码", "pinyin", "shuangpin"),
            ],
        );
        st
    }

    #[test]
    fn resolve_domain_matches_id_kind_and_folded_pinyin_variant() {
        let st = deep_link_fixture();
        assert_eq!(st.resolve_domain(""), None, "空 = 不指定");
        assert_eq!(st.resolve_domain("phrase"), Some(0));
        assert_eq!(st.resolve_domain("wb"), Some(1), "精确方案 id");
        assert_eq!(st.resolve_domain("quanpin"), Some(2));
        assert_eq!(st.resolve_domain("codetable"), Some(1), "引擎族名");
        assert_eq!(st.resolve_domain("pinyin"), Some(2));
        assert_eq!(
            st.resolve_domain("ziranma"),
            Some(2),
            "被折叠掉的双拼变体应落到拼音域"
        );
        assert_eq!(
            st.resolve_domain("wubi"),
            None,
            "严格匹配：拼错不猜，交由上层提示"
        );
    }

    /// 本次设计的核心风险点：view 侧那个监听 domain/sub_tab 的 effect 会在下一帧跑
    /// `on_nav_changed`，其切域分支会用 `sub_tab_memory` 里的旧值覆盖子标签。
    /// `navigate_to` 靠提前对齐 `last_domain` / 记忆来规避——这条测试就守它。
    #[test]
    fn navigate_to_survives_nav_effect() {
        let st = deep_link_fixture();
        assert_eq!(st.apply_deep_link("wb", Some(LogicalTab::Shadow)), None);
        assert_eq!(st.current_category(), WordCategory::Shadow);

        st.on_nav_changed(); // 模拟 effect 在下一帧触发
        assert_eq!(
            st.current_category(),
            WordCategory::Shadow,
            "导航 effect 不得把深链定位的子标签打回首个子标签"
        );
        assert_eq!(st.current_schema_id().as_deref(), Some("wb"));
    }

    /// 深链之后用户手动切域，仍应走正常的「记忆恢复」语义（不能被 navigate_to 带偏）。
    #[test]
    fn manual_switch_after_deep_link_still_restores_memory() {
        let st = deep_link_fixture();
        st.apply_deep_link("wb", Some(LogicalTab::Shadow));
        st.on_nav_changed();

        st.domain.set(2); // 用户切到拼音域
        st.on_nav_changed();
        assert_eq!(
            st.current_category(),
            WordCategory::UserDict,
            "新域无记忆→首项"
        );

        st.domain.set(1); // 切回五笔域
        st.on_nav_changed();
        assert_eq!(
            st.current_category(),
            WordCategory::Shadow,
            "回到五笔域应恢复到离开时的候选调整"
        );
    }

    #[test]
    fn deep_link_downgrades_when_type_absent_in_domain() {
        let st = deep_link_fixture();
        // 拼音域没有候选调整（tabs_for_domain 只给 用户词库/临时词库/词频）
        let note = st
            .apply_deep_link("pinyin", Some(LogicalTab::Shadow))
            .expect("落空档应给出提示");
        assert!(note.contains("候选调整"), "提示要点名用户要的类型: {note}");
        assert!(note.contains("用户词库"), "提示要说明实际落点: {note}");
        assert_eq!(st.domain.get(), 2, "域仍按 schema 定位");
        assert_eq!(
            st.current_category(),
            WordCategory::UserDict,
            "降级到该域首个子标签，而非报错或错位"
        );
    }

    #[test]
    fn deep_link_reports_unknown_schema_and_keeps_domain() {
        let st = deep_link_fixture();
        st.navigate_to(1, Some(0));
        let note = st.apply_deep_link("typo", None).expect("未知方案应提示");
        assert!(note.contains("typo"), "提示要带上用户输入的原值: {note}");
        assert_eq!(st.domain.get(), 1, "未找到方案时保持当前域，不乱跳");
    }

    #[test]
    fn deep_link_dimensions_are_independently_optional() {
        let st = deep_link_fixture();
        st.navigate_to(1, Some(0));

        // 只给类型：不改域，只换子标签
        assert_eq!(st.apply_deep_link("", Some(LogicalTab::Freq)), None);
        assert_eq!(st.domain.get(), 1, "省 --schema 时保持当前域");
        assert_eq!(st.current_category(), WordCategory::Freq);

        // 只给方案：切域，子标签沿用该域记忆（新域无记忆则落首项）
        assert_eq!(st.apply_deep_link("pinyin", None), None);
        assert_eq!(st.domain.get(), 2);
        assert_eq!(st.current_category(), WordCategory::UserDict);

        // 回到五笔域：省 --type 时应恢复到刚才停留的词频
        assert_eq!(st.apply_deep_link("wb", None), None);
        assert_eq!(st.current_category(), WordCategory::Freq);
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
        with_schemas(
            &st,
            vec![
                schema("wb", "五笔", "codetable"),
                schema("py", "全拼", "pinyin"),
            ],
        );

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
        with_schemas(
            &st,
            vec![
                schema("wb", "五笔86", "codetable"),
                schema("py", "全拼", "pinyin"),
            ],
        );
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
        // table_rows = 两行单元格文本（列 = 编码/词条/权重），display_rows 同序。
        let cells = st.table_rows.get();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0], vec!["aaaa", "工", "0"]);
        assert_eq!(st.display_rows.borrow()[1].text, "王中");
        assert!(st.empty_msg.get().is_empty(), "有数据时无空态提示");
        // 分页状态文字
        assert_eq!(st.page_status.get(), "第 1-2 / 共 120 条");
    }

    #[test]
    fn apply_rows_empty_yields_empty_hint() {
        let st = DictManagerState::new(); // 短语（非分页）
        st.apply_rows(&json!([]));
        assert!(st.rows.borrow().is_empty());
        assert!(st.table_rows.get().is_empty());
        assert!(!st.empty_msg.get().is_empty(), "空数据应给出空态提示");
        assert_eq!(st.page_status.get(), "共 0 条");
    }

    #[test]
    fn client_sort_reorders_unpaged_rows() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(1); // 临时词库（非分页，列 = 编码/词条/次数）
        st.apply_rows(&json!([
            { "code": "b", "text": "乙", "count": 2 },
            { "code": "a", "text": "甲", "count": 30 },
            { "code": "c", "text": "丙", "count": 4 },
        ]));
        // 无排序：保持原序。
        assert_eq!(st.table_rows.get()[0][0], "b");
        // 按编码列（0）升序。
        st.sort.set(Some((0, SortOrder::Asc)));
        st.on_sort_changed();
        let cells = st.table_rows.get();
        assert_eq!(
            vec![&cells[0][0], &cells[1][0], &cells[2][0]],
            ["a", "b", "c"]
        );
        assert_eq!(
            st.display_rows.borrow()[0].text,
            "甲",
            "display_rows 与表格同序"
        );
        // 按次数列（2）降序：数值序 30 > 4 > 2（字符串序会是 4 > 30 > 2）。
        st.sort.set(Some((2, SortOrder::Desc)));
        st.on_sort_changed();
        let cells = st.table_rows.get();
        assert_eq!(
            vec![&cells[0][2], &cells[1][2], &cells[2][2]],
            ["30", "4", "2"]
        );
    }

    #[test]
    fn paged_sort_params_and_reset_on_nav() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(0); // 用户词库（分页）
        st.sort.set(Some((2, SortOrder::Desc))); // 权重列
        let p = st.build_list_params();
        assert_eq!(
            p.get("sortBy"),
            Some(&json!("weight")),
            "分页类带服务端排序参数"
        );
        assert_eq!(p.get("sortOrder"), Some(&json!("desc")));
        // 排序变化归零分页（模拟已翻页）。
        *st.offset.borrow_mut() = PAGE_SIZE;
        st.on_sort_changed();
        assert_eq!(*st.offset.borrow(), 0, "点排序应回到第一页");
        // 导航变化：重置排序 + 表格结构版本递增。
        let epoch = st.table_epoch.get()[0];
        st.on_nav_changed();
        assert_eq!(st.sort.get(), None, "切类别应重置排序");
        assert_eq!(st.table_epoch.get()[0], epoch + 1, "切类别应触发整表重建");
        assert!(
            st.build_list_params().get("sortBy").is_none(),
            "无排序不带参数"
        );
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
        assert!(matches!(
            *st.pending.borrow(),
            Some(PendingAction::DeleteRow(_, _))
        ));
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
        assert!(matches!(
            *st.pending.borrow(),
            Some(PendingAction::ResetDefault)
        ));
    }

    #[test]
    fn import_preview_msg_and_cancel() {
        let st = DictManagerState::new();
        // 新 sections 契约：多段，只显数量（无示例、无新增/更新/不变细分）。
        let v = json!({ "format": "winddict", "compatible": true, "sections": [
            { "key": "userWords", "count": 6, "skipped": 0 },
            { "key": "tempWords", "count": 4, "skipped": 0 },
        ] });
        let msg = DictManagerState::format_import_preview(&v, CODETABLE_TYPES, "五笔86 · 码表");
        assert!(msg.contains("识别为 WindDict 词库"));
        assert!(msg.contains("用户词库 6"));
        assert!(msg.contains("临时词库 4"));
        assert!(!msg.contains("样例"), "不显示示例");
        assert!(!msg.contains("跳过"), "skipped=0 不显示跳过");
        // cancel 清空待导入内容/可用态并关对话框
        *st.pending_import.borrow_mut() = Some("x".into());
        st.import_ready.set(true);
        st.import_confirm_visible.set(true);
        st.cancel_import();
        assert!(!st.import_confirm_visible.get());
        assert!(st.pending_import.borrow().is_none());
        assert!(!st.import_ready.get());
    }

    #[test]
    fn import_preview_msg_shows_format_and_skipped() {
        let v = json!({ "format": "rime", "compatible": true, "sections": [
            { "key": "userWords", "count": 5, "skipped": 2 },
        ] });
        let msg = DictManagerState::format_import_preview(&v, CODETABLE_TYPES, "五笔86 · 码表");
        assert!(msg.contains("识别为 Rime 词库"));
        assert!(msg.contains("跳过 2 行"));
        let v = json!({ "format": "tsv", "compatible": true, "sections": [
            { "key": "userWords", "count": 1, "skipped": 0 },
        ] });
        assert!(
            DictManagerState::format_import_preview(&v, CODETABLE_TYPES, "五笔86 · 码表")
                .contains("识别为 TSV 文本")
        );
    }

    #[test]
    fn import_preview_incompatible_blocks() {
        // 来源码表、目标拼音 → 不兼容提示，阻止导入。
        let v = json!({ "format": "winddict", "compatible": false,
            "sourceEngine": "codetable", "targetEngine": "pinyin", "sections": [] });
        let msg = DictManagerState::format_import_preview(&v, PINYIN_TYPES, "拼音");
        assert!(msg.contains("无法导入"));
        assert!(msg.contains("码表") && msg.contains("拼音"));
        assert!(msg.contains("改选"), "应指出可以改选目标方案");
    }

    /// 混输场景的要害：文件里全是目标收不了的段时，预览必须明说"一条也不会导入"，
    /// 而不是像从前那样把数量原样列出来，让人以为导进去了。
    #[test]
    fn import_preview_flags_sections_target_cannot_take() {
        let v = json!({ "format": "winddict", "compatible": true, "sections": [
            { "key": "userWords", "count": 2000, "skipped": 0 },
            { "key": "freq", "count": 340, "skipped": 0 },
        ] });
        let msg = DictManagerState::format_import_preview(&v, MIXED_TYPES, "五笔拼音混输 · 混输");
        assert!(msg.contains("一条也不会导入"), "实际: {msg}");
        assert!(msg.contains("五笔拼音混输"));
        assert!(!msg.contains("可导入："), "没有任何一段能进，不该说可导入");

        // 部分可收：能收的与收不了的分两拨列，都要出现。
        let v = json!({ "format": "winddict", "compatible": true, "sections": [
            { "key": "userWords", "count": 2000, "skipped": 0 },
            { "key": "shadow", "count": 12, "skipped": 0 },
        ] });
        let msg = DictManagerState::format_import_preview(&v, PINYIN_TYPES, "拼音");
        assert!(msg.contains("可导入：用户词库 2000"), "实际: {msg}");
        assert!(msg.contains("不会导入：候选调整 12"), "实际: {msg}");
    }

    /// 目标方案决定勾选可见性与选中集合——不再跟着当前浏览的数据域走。
    #[test]
    fn io_applicable_types_follow_target_not_domain() {
        let st = DictManagerState::new();
        with_schemas(
            &st,
            vec![
                schema("wb", "五笔86", "codetable"),
                schema("mx", "五笔拼音混输", "mixed"),
            ],
        );
        st.domain.set(2); // 页面停在混输域
        st.io_target.set(1); // 目标也是混输
        assert!(!st.io_type_applicable("userWords"));
        assert!(st.io_type_applicable("shadow"));

        st.io_target.set(0); // 目标改到五笔86，页面仍在混输域
        assert!(
            st.io_type_applicable("userWords"),
            "目标是码表方案就应能收用户词库，与当前浏览域无关"
        );
        assert_eq!(st.io_target_schema_id().as_deref(), Some("wb"));
    }

    /// 目标收不了任何勾选类型时导入按钮必须灰掉——这正是"点了、报成功、零条"的来源。
    #[test]
    fn import_disabled_when_nothing_selectable() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("mx", "混输", "mixed")]);
        st.io_target.set(0);
        st.import_ready.set(true);
        st.io_shadow.set(false); // 混输只有候选调整，取消勾选后无可导入类型
        assert!(st.io_selected_sections().is_empty());
        assert!(!st.import_enabled());
        st.io_shadow.set(true);
        assert!(st.import_enabled());
    }

    /// 导出文件名一律带时间戳：短语此前写死 `phrases.wdict.yaml`，连导两次后一次
    /// 直接盖掉前一次，而方案级导出早就带了戳——两处口径必须一致。
    #[test]
    fn export_file_name_always_carries_timestamp() {
        for source in ["phrases", "wubi86"] {
            let name = DictManagerState::export_file_name(source);
            assert!(name.starts_with(&format!("{source}-")), "实际: {name}");
            assert!(name.ends_with(".wdict.yaml"), "实际: {name}");
            // <来源>-YYYYmmdd-HHMMSS.wdict.yaml → 戳部分为 8 位日期 + 6 位时间。
            let stamp = name
                .trim_start_matches(&format!("{source}-"))
                .trim_end_matches(".wdict.yaml");
            let (d, t) = stamp.split_once('-').expect("戳应为 日期-时间 两段");
            assert!(
                d.len() == 8 && d.chars().all(|c| c.is_ascii_digit()),
                "{name}"
            );
            assert!(
                t.len() == 6 && t.chars().all(|c| c.is_ascii_digit()),
                "{name}"
            );
        }
    }

    /// 导出文件带引擎标记，提示必须在**导出前**说清能导回哪类方案——等用户把五笔词库
    /// 喂给拼音方案再报错，文件早就传出去或躺进备份盘了。
    #[test]
    fn export_engine_note_names_the_target_engine() {
        let st = DictManagerState::new();
        with_schemas(
            &st,
            vec![
                schema("wb", "五笔86", "codetable"),
                schema("py", "拼音", "pinyin"),
                schema("en", "英文", "english"),
            ],
        );
        st.io_target.set(0);
        assert!(st.io_engine_note_for_target().contains("码表"));
        st.io_target.set(1);
        assert!(st.io_engine_note_for_target().contains("拼音"));
        // 英文方案曾因 state.rs 里那份不完整的 engine_label 副本显示成「未知」。
        st.io_target.set(2);
        let note = st.io_engine_note_for_target();
        assert!(
            note.contains("英文") && !note.contains("未知"),
            "实际: {note}"
        );
    }

    /// 兼容也要把来源方案与引擎摆出来：导入前核对一眼，比事后发现打不出来再回溯便宜。
    #[test]
    fn import_preview_shows_source_schema_and_engine() {
        let v = json!({ "format": "winddict", "compatible": true,
            "sourceSchema": "wubi86", "sourceEngine": "codetable",
            "sections": [{ "key": "userWords", "count": 6, "skipped": 0 }] });
        let msg = DictManagerState::format_import_preview(&v, CODETABLE_TYPES, "五笔86 · 码表");
        assert!(msg.contains("来源「wubi86」（码表）"), "实际: {msg}");

        // 来源标记缺失时不硬凑「未知」，直接不显示这一段。
        let v = json!({ "format": "tsv", "compatible": true,
            "sections": [{ "key": "userWords", "count": 1, "skipped": 0 }] });
        let msg = DictManagerState::format_import_preview(&v, CODETABLE_TYPES, "五笔86 · 码表");
        assert!(!msg.contains("来源"), "实际: {msg}");
        assert!(msg.contains("识别为 TSV 文本"));
    }

    /// 自动匹配的提示只对它当初指向的目标有效：用户手动改走下拉后必须消失，
    /// 否则「已按文件内容把目标切换为「五笔86」」会挂在一个已改成拼音的下拉旁边。
    #[test]
    fn auto_note_expires_when_user_changes_target() {
        let st = DictManagerState::new();
        with_schemas(
            &st,
            vec![
                schema("wb", "五笔86", "codetable"),
                schema("py", "拼音", "pinyin"),
            ],
        );
        st.io_target.set(0);
        st.io_auto_at.set(Some(0));
        st.io_auto_note
            .set("已按文件内容把目标切换为「五笔86」".into());
        st.on_io_target_changed();
        assert!(!st.io_auto_note.get().is_empty(), "目标未变，提示应保留");

        st.io_target.set(1); // 用户手动改走
        st.on_io_target_changed();
        assert!(st.io_auto_note.get().is_empty(), "目标已变，提示必须失效");
    }

    /// 切换子标签/数据域不再清空搜索框——搜索框全程是同一个信号，切一次清一次等于
    /// 逼用户每换一个标签重打一遍。
    #[test]
    fn nav_change_keeps_search_text() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔86", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(0);
        st.search.set("程序".into());
        st.sub_tab.set(1);
        st.on_nav_changed();
        assert_eq!(st.search.get(), "程序", "切子标签不应清空搜索词");
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 搜索词跨导航保留后，空表必须说清是"没数据"还是"没匹配"。
    #[test]
    fn empty_hint_distinguishes_no_data_from_no_match() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔86", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(0);
        assert_eq!(st.empty_hint(), "暂无数据");
        st.search.set("程序".into());
        let hint = st.empty_hint();
        assert!(
            hint.contains("程序") && hint.contains("没有匹配"),
            "实际: {hint}"
        );
    }

    /// 混输目标要给出「词条在成员方案里」的说明，其余方案不加噪音。
    #[test]
    fn mixed_target_explains_where_words_live() {
        let st = DictManagerState::new();
        with_schemas(
            &st,
            vec![
                schema("wb", "五笔86", "codetable"),
                schema("mx", "混输", "mixed"),
            ],
        );
        st.io_target.set(1);
        let note = st.io_scope_note_for_target();
        assert!(
            note.contains("候选调整") && note.contains("成员方案"),
            "实际: {note}"
        );
        st.io_target.set(0);
        assert!(
            st.io_scope_note_for_target().is_empty(),
            "码表方案不该有此提示"
        );
    }

    #[test]
    fn import_result_summary_per_section() {
        let v = json!({ "sections": [
            { "key": "userWords", "added": 10, "updated": 2, "unchanged": 3, "skipped": 0 },
            { "key": "freq", "imported": 50, "skipped": 0 },
        ] });
        let msg = DictManagerState::format_import_result(&v);
        assert!(msg.contains("用户词库 新增 10·更新 2"));
        assert!(msg.contains("词频 50 条"));
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
        st.open_edit("aaaa", "工", 0, 0);
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
        st.open_edit("aaaa", "工", 0, 0);
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

        st.open_edit("bj", "北京", 1, 0);
        st.edit_text.set("北京市".into());
        let (calls2, _) = st.save_calls().unwrap();
        assert_eq!(calls2[0].0, "phrase.update");
        assert_eq!(calls2[0].1["newText"], json!("北京市"));
        assert_eq!(calls2[0].1["text"], json!("北京")); // 原键
    }

    /// 短语的 `position` 必须在新增与编辑两条路上都发出去。
    ///
    /// 此前 add 写死 `position: 0`、update 根本不带该字段——用户在导入的 wdict 里设好的
    /// 次序，一经设置页保存就被抹平（且导出文件里那一列看着还在，很难联想到是被谁改的）。
    #[test]
    fn save_calls_phrase_carries_position() {
        let st = DictManagerState::new(); // domain 0 = 短语
        st.sub_tab.set(1); // 用户短语
        st.edit_code.set("date".into());
        st.edit_text.set("$Y-$MM-$DD".into());
        st.edit_weight.set("800".into());
        st.edit_position.set("3".into());

        // 新增
        let (add, _) = st.save_calls().unwrap();
        assert_eq!(add[0].0, "phrase.add");
        assert_eq!(add[0].1["position"], json!(3), "新增须带 position");

        // 编辑：回填现值后改次序
        st.open_edit("date", "$Y-$MM-$DD", 800, 3);
        assert_eq!(st.edit_position.get(), "3", "编辑对话框回填 position");
        st.edit_position.set("1".into());
        let (upd, _) = st.save_calls().unwrap();
        assert_eq!(upd[0].0, "phrase.update");
        assert_eq!(upd[0].1["position"], json!(1), "编辑须带 position");
        assert_eq!(upd[0].1["weight"], json!(800), "权重不因改次序而丢失");
    }

    #[test]
    fn save_calls_rejects_non_numeric_position() {
        let st = DictManagerState::new();
        st.sub_tab.set(1);
        st.edit_code.set("date".into());
        st.edit_text.set("x".into());
        st.edit_weight.set("1".into());
        st.edit_position.set("abc".into());
        assert!(st.save_calls().is_err(), "非数字次序应校验失败");
    }

    /// 「转为用户短语」只在系统短语类别可用；用户短语侧不该出现这个操作。
    #[test]
    fn claim_as_user_only_for_system_phrases() {
        let st = DictManagerState::new(); // domain 0 = 短语
        st.sub_tab.set(0); // 系统短语
        assert!(st.current_spec().ops.claim_as_user);
        st.sub_tab.set(1); // 用户短语
        assert!(!st.current_spec().ops.claim_as_user);
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
        with_schemas(
            &st,
            vec![
                schema("wb", "五笔", "codetable"),
                schema("py", "全拼", "pinyin"),
                schema("mx", "混输", "mixed"),
            ],
        );
        // 短语域：系统 + 用户
        st.domain.set(0);
        assert_eq!(
            st.tabs_for_domain(),
            vec![LogicalTab::PhraseSystem, LogicalTab::PhraseUser]
        );
        // 码表：四类含候选调整
        st.domain.set(1);
        assert_eq!(
            st.tabs_for_domain(),
            vec![
                LogicalTab::UserDict,
                LogicalTab::Temp,
                LogicalTab::Freq,
                LogicalTab::Shadow
            ]
        );
        // 拼音：三类无候选调整
        st.domain.set(2);
        assert_eq!(
            st.tabs_for_domain(),
            vec![LogicalTab::UserDict, LogicalTab::Temp, LogicalTab::Freq]
        );
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
            WordRow {
                code: "bj".into(),
                text: "北京".into(),
                is_system: true,
                ..Default::default()
            },
            WordRow {
                code: "me".into(),
                text: "我的".into(),
                is_system: false,
                ..Default::default()
            },
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
            WordRow {
                code: "nihao".into(),
                text: "你好".into(),
                ..Default::default()
            },
            WordRow {
                code: "women".into(),
                text: "我们".into(),
                ..Default::default()
            },
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
        use windui::prelude::ToastKind;
        let st = DictManagerState::new();
        st.notify_ok("已添加");
        assert_eq!(
            st.feedback.get(),
            Some((ToastKind::Success, "已添加".to_string()))
        );
        st.notify_err("失败了");
        assert_eq!(
            st.feedback.get(),
            Some((ToastKind::Error, "失败了".to_string()))
        );
        st.notify_info("没有变更");
        assert_eq!(
            st.feedback.get(),
            Some((ToastKind::Info, "没有变更".to_string()))
        );
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
        st.open_shadow_edit("wm", "我们", "pin", 2, "");
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
        st.open_shadow_edit("wm", "我们", "pin", 2, "");
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

    /// 编辑短语规则必须**原样回传 candId**。
    ///
    /// 这条路（`shadow.addRule`）在 core 侧会先按 id/word 匹配掉同一条旧规则再插入新规则，
    /// 不带 id 就等于把原规则的稳定 id 擦掉——规则退化成按当日文本匹配，次日必失配。
    /// 用户侧表现为「在设置页只改了个位置，第二天整条规则就不生效了」。
    #[test]
    fn shadow_edit_preserves_cand_id() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(3); // 候选调整
        let cid = "phrase:date:$Y/$MM/$DD";

        // 编辑一条短语规则：只改位置，文本不动。
        st.open_shadow_edit("date", "2026/07/30", "pin", 0, cid);
        st.shadow_pos.set("2".into());
        let (calls, _) = st.shadow_save_calls().unwrap();

        assert_eq!(calls[0].0, "shadow.addRule");
        assert_eq!(calls[0].1["candId"], json!(cid), "稳定 id 必须回传");
        assert_eq!(calls[0].1["word"], json!("2026/07/30"), "word 用规则原值");
        assert_eq!(calls[0].1["position"], json!(2));
        assert_eq!(calls.len(), 1, "键未变，不该删旧规则");
    }

    /// 手动新增的规则没有候选实体，不带 candId（否则会凭空造一个匹配不到任何候选的 id）。
    #[test]
    fn shadow_add_omits_cand_id() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(3);
        st.open_shadow_add();
        st.shadow_code.set("aaaa".into());
        st.shadow_word.set("工".into());
        let (m, p) = st.shadow_add_call().unwrap();
        assert_eq!(m, "shadow.addRule");
        assert!(p.get("candId").is_none(), "手动新增不带 candId");
    }

    /// 先编辑带 id 的规则、再打开「新增」，不得把上一条的 id 带过去。
    #[test]
    fn shadow_add_after_edit_clears_stale_cand_id() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(3);
        st.open_shadow_edit("date", "2026/07/30", "pin", 0, "phrase:date:$Y/$MM/$DD");
        st.open_shadow_add();
        st.shadow_code.set("aaaa".into());
        st.shadow_word.set("工".into());
        let (_, p) = st.shadow_add_call().unwrap();
        assert!(p.get("candId").is_none(), "新增须清掉上一条的 id");
    }

    #[test]
    fn open_shadow_edit_prefills_signals_and_sets_visible() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(1);
        st.sub_tab.set(3); // 候选调整

        // pin 类型 → 固定位置(0)
        st.open_shadow_edit("ab", "阿", "pin", 2, "");
        assert!(
            st.shadow_add_visible.get(),
            "编辑后 shadow_add_visible=true"
        );
        assert_eq!(st.shadow_code.get(), "ab");
        assert_eq!(st.shadow_word.get(), "阿");
        assert_eq!(st.shadow_type.get(), 0, "pin → 固定位置(0)");
        assert_eq!(st.shadow_pos.get(), "2");
        assert_eq!(st.shadow_title.get(), "编辑候选调整");

        // delete 类型 → 隐藏(1)
        st.shadow_add_visible.set(false);
        st.open_shadow_edit("cd", "屏", "delete", 0, "");
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
        st.open_view("bj", "北京", 1000, 4);
        assert!(st.view_visible.get(), "open_view 后 view_visible=true");
        // 写入独立 view_* 信号，不污染 edit_*
        assert_eq!(st.view_code.get(), "bj");
        assert_eq!(st.view_text.get(), "北京");
        assert_eq!(st.view_weight.get(), "1000");
        assert_eq!(st.view_position.get(), "4", "查看也显示次序");
        // edit_* 信号不受影响
        assert_eq!(st.edit_code.get(), "", "open_view 不写 edit_code");
        assert_eq!(st.edit_text.get(), "", "open_view 不写 edit_text");
        // editing_orig 不写，仍为 None
        assert!(st.editing_orig.borrow().is_none(), "查看不设 editing_orig");
        assert!(
            st.viewing.borrow().is_some(),
            "记住原值供「转为用户短语」原样复制"
        );
        st.cancel_view();
        assert!(!st.view_visible.get(), "cancel_view 后 view_visible=false");
        assert!(
            st.viewing.borrow().is_none(),
            "关闭后清掉原值，避免下次误用上一条的数据"
        );
    }

    /// 查看对话框里的「转为用户短语」：无当前查看项时必须是安全的空操作。
    ///
    /// 该入口只在对话框内可点，正常流程下 `viewing` 必然有值；但它读的是 RefCell 而非
    /// 按钮回调捕获的行数据，一旦将来被别处复用（如快捷键）就可能在无查看项时触发。
    #[test]
    fn claim_viewing_as_user_is_noop_without_viewing() {
        let st = DictManagerState::new();
        assert!(st.viewing.borrow().is_none());
        st.claim_viewing_as_user(); // 不 panic、不发 RPC
        assert!(!st.view_visible.get());
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
        with_schemas(
            &st,
            vec![
                schema("mx", "混输", "mixed"),
                schema("wb", "五笔", "codetable"),
            ],
        );
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
        with_schemas(
            &st,
            vec![
                schema_s("pinyin_simp", "全拼·简", "pinyin", "full"),
                schema_s("double_pinyin", "自然码双拼", "pinyin", "shuangpin"),
                schema("wubi86", "五笔86", "codetable"),
            ],
        );
        let labels = st.domain_labels();
        // 域：快捷短语 + 拼音(折叠) + 五笔86 = 3 项（而非 4）
        assert_eq!(
            labels,
            vec![
                "快捷短语".to_string(),
                "拼音".to_string(),
                "五笔86 · 码表".to_string()
            ]
        );
        // 选"拼音"域，current_schema_id 返回代表(全拼优先)
        st.domain.set(1);
        st.sub_tab.set(0);
        assert_eq!(st.current_schema_id().as_deref(), Some("pinyin_simp"));
    }

    #[test]
    fn shuangpin_only_folds_with_shuangpin_rep() {
        let st = DictManagerState::new();
        with_schemas(
            &st,
            vec![schema_s(
                "double_pinyin",
                "自然码双拼",
                "pinyin",
                "shuangpin",
            )],
        );
        st.domain.set(1);
        // 无全拼时代表取该双拼
        assert_eq!(st.current_schema_id().as_deref(), Some("double_pinyin"));
    }

    #[test]
    fn shuangpin_before_full_upgrades_rep() {
        let st = DictManagerState::new();
        with_schemas(
            &st,
            vec![
                schema_s("double_pinyin", "自然码双拼", "pinyin", "shuangpin"),
                schema_s("pinyin_simp", "全拼·简", "pinyin", "full"),
            ],
        );
        st.domain.set(1);
        // 双拼在前、全拼在后 → 代表应升级为全拼
        assert_eq!(st.current_schema_id().as_deref(), Some("pinyin_simp"));
        // 仍折叠为一个"拼音"域
        assert_eq!(
            st.domain_labels(),
            vec!["快捷短语".to_string(), "拼音".to_string()]
        );
    }

    #[test]
    fn sub_tab_remembered_per_domain_across_switch() {
        let st = DictManagerState::new();
        with_schemas(
            &st,
            vec![
                schema("wb", "五笔", "codetable"),
                schema("py", "全拼", "pinyin"),
            ],
        );
        // 域 0→1（码表）：新域无记录，落第一个子标签。
        st.domain.set(1);
        st.on_nav_changed();
        assert_eq!(st.sub_tab.get(), 0);
        // 在码表域停到"词频"(2)。
        st.sub_tab.set(2);
        st.on_nav_changed();
        // 切到拼音域(2)：新域无记录，回第一个（不再沿用码表的 2）。
        st.domain.set(2);
        st.on_nav_changed();
        assert_eq!(st.sub_tab.get(), 0, "切到新域回第一个子标签");
        // 切回码表域(1)：恢复上次停留的"词频"(2)。
        st.domain.set(1);
        st.on_nav_changed();
        assert_eq!(st.sub_tab.get(), 2, "切回旧域应恢复其上次子标签");
    }

    #[test]
    fn switch_domain_defaults_first_tab_when_no_memory() {
        // 复现用户反馈：在短语域选"用户短语"(1)，切到方案域不应沿用下标 1。
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(0);
        st.sub_tab.set(1); // 用户短语
        st.on_nav_changed();
        st.domain.set(1); // 切到码表域
        st.on_nav_changed();
        assert_eq!(st.sub_tab.get(), 0, "切到未访问过的方案域应回第一个子标签");
    }

    #[test]
    fn prev_next_domain_cycles() {
        let st = DictManagerState::new();
        with_schemas(
            &st,
            vec![
                schema("wb", "五笔", "codetable"),
                schema("py", "全拼", "pinyin"),
            ],
        );
        // 3 个域：快捷短语(0) + 五笔(1) + 拼音(2)。
        assert_eq!(st.domain_count(), 3);
        assert_eq!(st.domain.get(), 0);
        st.next_domain();
        assert_eq!(st.domain.get(), 1);
        st.next_domain();
        assert_eq!(st.domain.get(), 2);
        st.next_domain();
        assert_eq!(st.domain.get(), 0, "末尾循环回首");
        st.prev_domain();
        assert_eq!(st.domain.get(), 2, "首部循环到末尾");
    }

    #[test]
    fn domain_rich_items_carry_engine_badges() {
        let st = DictManagerState::new();
        with_schemas(
            &st,
            vec![
                schema("wb", "五笔86", "codetable"),
                schema("py", "全拼", "pinyin"),
            ],
        );
        let items = st.domain_rich_items();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].label, "快捷短语");
        // 主文本取方案名（去掉 " · 码表" 后缀），引擎类型进徽章。
        assert_eq!(items[1].label, "五笔86");
        assert_eq!(
            items[1].badge.as_ref().map(|(t, _)| t.as_str()),
            Some("码表")
        );
        // 拼音域也带徽章，保持统一。
        assert_eq!(items[2].label, "拼音");
        assert_eq!(
            items[2].badge.as_ref().map(|(t, _)| t.as_str()),
            Some("拼音"),
            "拼音域也应带引擎徽章以保持统一"
        );
    }
}
