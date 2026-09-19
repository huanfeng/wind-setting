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

use crate::auto_encode::AutoEncode;
use crate::dialogs::schema_manager::engine_label;
use crate::rpc;
use crate::state::SchemaInfo;

use super::spec::{
    CategorySpec, LogicalTab, WordCategory, WordField, WordRow, common_label, spec_for,
};

/// 客户端排序（非分页类别）：数值字段按数值，其余按展示文本字符串序。稳定排序。
fn sort_rows(rows: &mut [WordRow], field: WordField, ord: SortOrder) {
    let num = |r: &WordRow| match field {
        WordField::Weight => r.weight,
        WordField::Count => r.count,
        WordField::Position => r.position,
        // ⚠️ 缺席排最后而不是当 0：0 是合法且**最高**的优先级，按它排会把「RPC 没给
        // 这个字段」的行顶到列表最前，看起来像一个压过所有出厂类的类。
        WordField::ClassOrder => r.class_order.map_or(i64::MAX, i64::from),
        // ⚠️ 新增数值列时这里和 `is_numeric` 要**一起**改：只改后者的话 `num` 落到这条
        // 兜底、每行都返回 0，稳定排序于是原样不动——表现是「点了表头没反应」，不报错。
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

/// 页码（1-based）→ 分页偏移，越界钳到 `[1, total_pages]`。
///
/// 单独抽出来是为了可测：`goto_page` 里这一步之后紧跟 `reload()`，而 reload 会按服务端
/// 回来的 total 再做一次越界回收，钳制本身的行为在集成路径上被盖住、断言不到。
pub fn page_offset(page: usize, total_pages: usize) -> usize {
    (page.clamp(1, total_pages.max(1)) - 1) * PAGE_SIZE
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
    /// 表格结构版本（单元素，导航变化时递增）：列集随类别变，须整表重建。
    pub table_epoch: Signal<Vec<u64>>,
    /// 域列表版本（单元素，`set_schemas_no_reload` 时递增）：驱动词库窗左栏整体重建。
    ///
    /// 与 `domain_items` 分开而不是直接把左栏挂在它上面：`host_signal` 是按 Vec 的**每个
    /// 元素**调一次构建器的，挂多元素信号会得到 N 份完整列表叠在一起。同 `table_epoch`，
    /// 这里要的是「变了就整体重建」，故用单元素计数器。
    pub domain_epoch: Signal<Vec<u64>>,
    /// 受控排序状态（None=无排序；表头点击循环切换，导航变化时重置）。
    pub sort: Signal<Option<SortKey>>,
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
    /// 跳页输入框的内容（1-based 页码）。列表每次重建后由 [`Self::sync_page_input`]
    /// 回填成实际页码，故翻页/搜索/切标签之后框里恒是真值。
    pub page_input: Signal<String>,
    /// 跳页框右侧的「/ N 页」。非分页类别为空串（那组控件本身也不显示）。
    pub page_count_text: Signal<String>,
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

    /// 「外部编辑」对话框（字符类首用；其它词库接入见 `external_edit` 模块文档）。
    pub xedit: super::external_edit::ExternalEditState,
    /// 待导入的文件内容（预览确认后执行导入）。
    ///
    /// ⚠️ 仍然读全文，但**不再拿它当 RPC 载荷**——那是 [`Self::pending_import_path`] 的活。
    /// 这份内容留给客户端自己判格式（`wants_word_list`）与纯词列表解析，都在本进程内，
    /// 不过 IPC，不受帧长约束。
    pending_import: Rc<RefCell<Option<String>>>,
    /// 待导入文件的**路径**，`dict.previewImport` / `dict.import` 的首选载荷。
    ///
    /// ## 为什么传路径而不是内容
    ///
    /// 走 `content` 时整份词库要塞进一帧 JSON-RPC，而本进程的读帧上限是 16MB
    /// （[`crate::rpc`] 的 `MAX_FRAME`）。按每条约 30 字节算，单是用户词库就在 55 万条
    /// 左右撞墙，带上词频/候选调整等段则 25 万条起——**随词库增长必然撞上，不是偶发**。
    /// 见 issue #101（用户实测报「导出失败: 读取失败: 帧长超限」）。
    ///
    /// 传路径后 payload 是 O(1)，上限**消失**。core 侧同权限同用户，直接读得到这个文件。
    ///
    /// `None` = 内容不来自文件（纯词列表那条路在内存里合成 TSV），此时回落 `content`。
    pending_import_path: Rc<RefCell<Option<String>>>,
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

    // ── 纯词列表导入（一行一个词、无编码；由目标方案现场出码）──────
    /// 导入格式：0 = 自动识别，1 = 强制纯词列表。
    ///
    /// 只给这两项。强制指定 WindDict/Rime/TSV 需要 core 的 preview/import 接受 format
    /// 参数（它们现在一律按内容自动识别），给了也不生效——列一个点了没反应的选项比不列更糟。
    pub io_format: Signal<usize>,
    /// 格式行右侧的识别结果回显（"纯词列表" / "WindDict 词库" / …；空 = 未选文件）。
    pub io_format_note: Signal<String>,
    /// 纯词列表入库权重（用户可改）。纯词列表没有权重列，只能由用户给一个。
    pub io_weight: Signal<String>,
    /// 已解析待导入的纯词列表；`Some` 即表示本次导入走纯词列表通道。
    ///
    /// 这里只存**词**，不存码：出码在点「导入」时才分批发生（刻意不做出码预览）。
    /// 因此切换目标方案无需作废任何东西——那一刻还没有依赖方案的中间结果。
    pending_wordlist: Rc<RefCell<Option<super::wordlist::WordList>>>,
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
    /// 拼音词条准入契约的二选一：true=由程序补充音节分隔，false=跳过这些词。
    ///
    /// 默认 true 有实测依据：60 万条出厂词条把码拍平重解，补齐正确率 99.9997%
    /// （主仓 `docs/design/pinyin-entry-boundary-contract.md` §5.0）。
    /// ⚠️ false 是**跳过**不是「照原样导入」——后者会让这些词以 boundary=0 入库，
    /// 简拼索引与双拼校验对它们静默失效，正是这套契约要消灭的东西。
    pub io_fill_boundary: Signal<bool>,
    /// 预览报告的「缺音节分隔、可补齐」行数（0 = 不显示那一行选择）。
    pub import_boundary_pending: Signal<u64>,
    /// 预览报告的「无法解析为拼音音节、一律不导入」行数。
    pub import_boundary_rejected: Signal<u64>,

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
    /// 自动出码状态（词条一变即反查编码）。所有**外部改写** `edit_code`/`edit_text` 的地方
    /// 都要跟一句 `auto_encode.reseed()`，否则那次改写会被判成「用户手改编码」而停掉自动。
    pub auto_encode: AutoEncode,
    /// 快捷输入的模板变量提示，`[(kind, [(变量名, 说明)])]`。
    ///
    /// 懒加载 + 常驻缓存：它是静态数据（core 的编译期常量），一次 RPC 取完够用整个会话，
    /// 每次开对话框都拉一遍只是白费一轮往返。
    ///
    /// ⚠️ 用 `Option` 而非空 `Vec` 表达「取没取过」：空 `Vec` 分不清「还没取」与「取回来
    /// 就是空的」，于是 core 一断开，[`Self::build_quick_var_lines`] 的每类循环都会各发
    /// 一轮 RPC（类别有几个就几轮），"一次取完"的承诺形同虚设。`None`=没取过、
    /// `Some`=取过（失败仍留 `None`，下次开对话框可重试，不会因一次连不上就永久没提示）。
    quick_vars: Rc<RefCell<Option<QuickVarsByKind>>>,

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
    /// 快捷输入「添加/编辑格式」对话框的可见性。
    ///
    /// 字段本身复用 `edit_code`（存类别）/`edit_text`（存模板）/`edit_title`/`editing_orig`
    /// 与 `submit_word` 那条提交路径，只有可见性独立——两个对话框的布局差别太大
    /// （类别下拉 + 变量提示 vs 编码/权重/次序），塞进一个会变成一堆 visible_when。
    pub quick_edit_visible: Signal<bool>,
    /// 常用字对话框的可见性（独立于 `edit_visible`：那个对话框有编码/权重/次序，这个没有）。
    pub common_edit_visible: Signal<bool>,
    /// 常用字对话框里「判为哪边」的选择，下标对应 [`crate::pages::dict::spec::common_choices`]。
    pub common_choice: Signal<usize>,
    /// 常用字列表的「只看已修改」过滤。
    ///
    /// 列表是全表（8104 条），自己动过的那几个靠翻页根本找不到；搜索也不行——用户记不住
    /// 自己改过哪些字，正是要靠这个开关**列出来**。
    pub only_modified: Signal<bool>,
    /// 类别选择器的选项标签。开对话框时填充。
    ///
    /// ⚠️ 必须是信号、不能在构建期算好：对话框只构建一次，而那一刻列表数据还没加载——
    /// 选项会被永久冻结成空的。`Element::segmented` 甚至直接 `debug_assert` 非空，
    /// 测试因此当场炸了；若换成容忍空列表的控件，这个 bug 会一直潜伏到真机，
    /// 症状是「类别下拉里什么都没有」。
    pub quick_kind_labels: Signal<Vec<String>>,
    /// 变量提示文本（见 [`Self::build_quick_var_text`]）。开对话框时填充。
    ///
    /// 用信号而不是构建期算好：`quick.vars` 要走一轮 RPC，而对话框只构建一次——在构建期
    /// 取，万一那一刻 core 还没连上，这块提示就永久是空的。开对话框时 core 必然可用。
    ///
    /// ★★ 承载控件选过三轮，判据是**选区粒度**：
    /// - `label` 不可选中，用户只能手打变量名，出局；
    /// - `RichText` 有划选/双击选词/右键复制，看着正合适，但它的选区是**碎片级**、
    ///   且「Latin 按空格切」⇒ `$LMD` 整个是一个碎片，而选区要求「拖出锚点碎片」才成立。
    ///   于是在一个变量名内部怎么拖都选不中（`sel` 恒 None，右键菜单也只剩「复制全部」）
    ///   ——恰好挡死了「只想复制一个变量名」这个主用例。其 doc 里「Latin 词内字符级选区」
    ///   正列在已知限制中。
    /// - `text_input` 多行：字符级选区，与用户在别处的选中习惯一致。它可编辑，但每次
    ///   打开对话框都重设内容，且没有任何地方读这个信号，改了不会有后果。
    pub quick_var_text: Signal<String>,
    /// 新增格式时选中的类别，是 [`Self::quick_kind_options`] 的下标。
    ///
    /// 用下标而不是 kind 字符串：分段选择器绑的就是下标信号。选项列表**从当前数据现取**
    /// （见 `quick_kind_options`），故主程序加一类解析器时这里自动跟上，不必同步一份常量。
    pub quick_kind_idx: Signal<usize>,
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
/// 按类别索引的模板变量提示：`[(kind, [(变量名, 说明)])]`。
type QuickVarsByKind = Vec<(String, Vec<(String, String)>)>;

/// 变量提示的一节：`(合并到一起的类别名, 该节的变量集)`。
///
/// 变量集相同的类别合并成一节（core 里 date 与 month_day 完全相同），故类别名是一组。
type QuickVarSection = (Vec<String>, Vec<(String, String)>);

#[derive(Clone)]
enum PendingAction {
    /// 删除一行（code, text/word）。
    DeleteRow(String, String),
    /// 删除一条快捷输入的自定义格式（kind, id）。
    ///
    /// 与 [`Self::DeleteRow`] 分开而不是复用它：快捷输入的主键是 `(kind, id)`，
    /// 而 `DeleteRow` 只带 `(code, text)`——把 kind 丢在半路，确认之后就拼不出请求了。
    /// 确认文案在入队时就已生成，故这里不必带模板原文。
    DeleteQuickFormat { kind: String, id: String },
    /// 清空当前类别。
    Clear,
    /// 恢复默认（短语）。
    ResetDefault,
    /// 把某个字所属的整个 Unicode 块设为常用/生僻（`char` 用来定位块，core 自己推导）。
    ///
    /// 只带定位用的那个字、不带扫描出来的字符清单：预览与执行**必须是同一次扫描**。
    /// 若把清单缓存在这里，用户确认前切了方案或热插拔了扩展词库，执行时写的就不是他
    /// 看到的那批字——而两次扫描之间隔着的正是用户的思考时间。core 侧 `apply` 开关走
    /// 的是同一个方法，就是为了不让这两条路分叉。
    CommonBulk { ch: String, common: bool },
}

/// 当前数据域的类型（全局域或方案 kind）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DomainKind {
    Phrase,
    /// 快捷输入格式表。**全局域**（与方案无关：core 那张表的键只到格式类别）。
    QuickFormat,
    /// 常用字覆盖。**全局域**（core 那张表的键就是一个字，不带方案也不带编码）。
    CommonChars,
    /// 输入补全的学习数据（邮箱后缀 / 网址历史）。**全局域**：core 那张表的键是
    /// `{类别}\0{文本}`，不带方案——「我常用哪个邮箱后缀」与五笔拼音无关。
    Completion,
    Pinyin,
    Codetable,
    Mixed,
    /// 英文方案。**没有临时词库**——那是自动造词的暂存区，英文没有造词流程，
    /// 挂上去永远是空表。其余（用户词库/词频/候选调整）都有。
    English,
    Other,
}

/// 全局域（不按方案）在域列表里的固定槽位；方案域从 [`GLOBAL_DOMAINS`] 起算。
///
/// ⚠️ 此前只有一个全局域，于是 `domain - 1` 这个偏移在四处各手写一遍。加第二个全局域时
/// **漏改任何一处，那处就整体错位一格**——选「五笔86」却看到拼音的数据，而且不报错、
/// 不 panic。故域下标一律经 [`domain_slot`] 解析，不再手写下标算术。
pub(crate) const DOMAIN_PHRASE: usize = 0;
pub(crate) const DOMAIN_QUICK: usize = 1;
pub(crate) const DOMAIN_COMMON: usize = 2;
pub(crate) const DOMAIN_COMPLETION: usize = 3;
pub(crate) const GLOBAL_DOMAINS: usize = 4;

/// 全局域的展示名，**按槽位顺序**（`[DOMAIN_PHRASE]` / `[DOMAIN_QUICK]` /
/// `[DOMAIN_COMMON]` / `[DOMAIN_COMPLETION]`）。
///
/// ★ 数组长度锁死 [`GLOBAL_DOMAINS`]：加一个全局域却忘了在这里补名字会**当场编译失败**。
/// 这条约束是补出来的——上面那段注释当年把「域下标算术」收敛进了 [`domain_slot`]，
/// 却漏了**标签列表**这一路：`domain_labels` 与 `domain_rich_items` 各自手写着两项字面量，
/// 与 `GLOBAL_DOMAINS` 毫无编译期关联。加第三个全局域时它们照旧只产出两项，症状正是那段
/// 注释描述的：侧栏少画一项、其后的方案域整体错位一格，且不报错不 panic。
pub(crate) const GLOBAL_DOMAIN_LABELS: [&str; GLOBAL_DOMAINS] =
    ["快捷短语", "快捷输入", "字符集分类", "输入补全"];

/// 域下标的语义。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DomainSlot {
    Phrase,
    QuickFormat,
    /// 常用字覆盖（全局：那张表的键就是一个字，不带方案）。
    CommonChars,
    /// 输入补全的学习数据（全局：邮箱后缀 + 网址历史两张子表）。
    Completion,
    /// 折叠后的方案域下标（**已减去全局槽位**，可直接索引 `folded_domains()`）。
    Schema(usize),
}

/// 域下标 → 语义。越界一律当方案域（由调用方 `get()` 兜空）。
pub fn domain_slot(domain: usize) -> DomainSlot {
    match domain {
        DOMAIN_PHRASE => DomainSlot::Phrase,
        DOMAIN_QUICK => DomainSlot::QuickFormat,
        DOMAIN_COMMON => DomainSlot::CommonChars,
        DOMAIN_COMPLETION => DomainSlot::Completion,
        d => DomainSlot::Schema(d - GLOBAL_DOMAINS),
    }
}

/// 某类数据域有哪些子标签——**全页唯一真值**。
///
/// 渲染层（`view.rs` 的子标签分段）的标题走这里 + [`LogicalTab::title`] 派生，**不另写一份**。
/// 两处各持一份清单时，漏改任一边都很隐蔽，且症状不同：
///
/// - 逻辑层有、渲染层没有 ⇒「有功能、没入口」：能加词条、能导入导出，就是看不到标签。
/// - 渲染层多画一段 ⇒ 点得到一个逻辑层不认的下标。
///
/// 拼音曾无候选调整（那时拼音下调位被整体屏蔽，规则表恒空）。置顶放开后规则真的会产生，
/// 不给这个标签就等于「调得了、看不见、也删不掉」——撤销只剩右键「恢复默认」，且必须先把
/// 那条候选重新打出来。
///
/// ⚠️ 还与主仓 `webdata.rs::default_dict_sections`（导出默认段）同增同减，那条跨仓契约
/// **没有编译期约束**：只改这边而导出仍缺 Shadow，用户会在设置页看见规则、导出的文件里
/// 却没有，直到还原时才发现丢了数据。
pub fn tabs_for_kind(kind: DomainKind) -> Vec<LogicalTab> {
    use LogicalTab::*;
    match kind {
        DomainKind::Phrase => vec![PhraseSystem, PhraseUser],
        // 快捷输入只有一张表（五个类别在同一列表里靠「类别」列区分，排序是组内语义）。
        DomainKind::QuickFormat => vec![QuickFormat],
        // ★ 两层视图（设计文档 §7.4）：先按**字**，再按**类**。
        //
        // 顺序按**到达频率**定，不按抽象层级定：绝大多数人来这个域是为了调某个具体的字
        // （右键「设为生僻字」之后来这里翻看、撤销），字符类是少数人偶尔配一次的东西。
        // 层级上「类」确实在「字」之上，但把低频的那层摆在第一屏，等于让多数人每次都
        // 多点一下（2026-09-05 与用户确认）。
        DomainKind::CommonChars => vec![CommonChars, Charsets],
        // 输入补全：两类学习数据一个域。顺序按「哪个更可能有内容」定——邮箱后缀跟随
        // 邮箱模式开关即开始积累，网址历史还要多开一道 history_enabled，出厂两者都空。
        DomainKind::Completion => vec![MailSuffix, UrlHistory],
        DomainKind::Pinyin => vec![UserDict, Temp, Freq, Shadow],
        DomainKind::Mixed => vec![Shadow],
        // 英文：无临时词库（无造词流程，那张表恒空）。
        DomainKind::English => vec![UserDict, Freq, Shadow],
        // 码表与未知：全四类（未知从宽，不隐藏数据）。
        DomainKind::Codetable | DomainKind::Other => vec![UserDict, Temp, Freq, Shadow],
    }
}

/// 多行输入框里的真换行 → 字面两字符 `\n`（转义域）。
///
/// 设置页与 core 之间的 text 走的是**转义形态**：core 出口 `ui_text`
/// (`escape_text_field`) 把存储域的真换行投影成 `\n`，入口 `store_text`
/// (`unescape_text_field`) 还原。列表回填给对话框的串因此本就是转义形态；用户在多行框里
/// 新敲的回车是唯一的例外，由本函数补上这一投影，使提交出去的串整串同域。
///
/// `\r\n` / 裸 `\r` 一并归一：来源可能是从别处粘贴进来的 Windows 行尾，漏掉的话 core
/// 落库的文本会带一个不可见的 `\r`，表现为上屏时多一个诡异空白，且从界面上完全看不出来。
///
/// 快捷加词小窗（`add_word_window`）的词条框改多行之后同样在用它——**不要各写一份**，
/// 两个界面提交的是同一个 `dict.add`，口径分叉的表现是「同一段文字从这个窗进去能用、
/// 从那个窗进去多一个不可见字符」。
///
/// **只动换行，不动反斜杠**——用户写的 `\\`（字面反斜杠）与 `$CC(...)` 里的路径都已是
/// 转义域写法，再转一层会让反斜杠每保存一轮翻一倍（同 `phrase_import` 那条分享路径的教训）。
pub(crate) fn escape_input_newlines(s: &str) -> String {
    if !s.contains('\n') && !s.contains('\r') {
        return s.to_string();
    }
    s.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\\n")
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
            table_epoch: signal(vec![0]),
            domain_epoch: signal(vec![0]),
            sort: signal(None),
            empty_msg: signal(String::new()),
            search: signal(String::new()),
            offset: Rc::new(RefCell::new(0)),
            total: signal(0),
            page_status: signal(String::new()),
            page_input: signal("1".to_string()),
            page_count_text: signal(String::new()),
            status: signal(String::new()),
            feedback: signal(None),
            confirm_visible: signal(false),
            confirm_msg: signal(String::new()),
            pending: Rc::new(RefCell::new(None)),
            import_confirm_visible: signal(false),
            import_preview_msg: signal(String::new()),
            import_file_name: signal(String::new()),
            import_ready: signal(false),
            xedit: super::external_edit::ExternalEditState::new(),
            pending_import: Rc::new(RefCell::new(None)),
            pending_import_path: Rc::new(RefCell::new(None)),
            io_target: signal(0),
            io_target_items: signal(vec![]),
            io_auto_note: signal(String::new()),
            io_engine_note: signal(String::new()),
            io_format: signal(0),
            io_format_note: signal(String::new()),
            io_weight: signal("1200".to_string()),
            pending_wordlist: Rc::new(RefCell::new(None)),
            io_auto_at: Rc::new(Cell::new(None)),
            io_scope_note: signal(String::new()),
            io_user_words: signal(true),
            io_temp_words: signal(true),
            io_freq: signal(true),
            io_shadow: signal(true),
            export_visible: signal(false),
            io_title: signal(String::new()),
            import_compatible: signal(true),
            io_fill_boundary: signal(true),
            import_boundary_pending: signal(0),
            import_boundary_rejected: signal(0),
            edit_visible: signal(false),
            edit_title: signal(String::new()),
            edit_l_code: signal("编码".to_string()),
            edit_l_text: signal("词条".to_string()),
            edit_code: signal(String::new()),
            edit_text: signal(String::new()),
            edit_weight: signal("0".to_string()),
            edit_position: signal("0".to_string()),
            editing_orig: Rc::new(RefCell::new(None)),
            auto_encode: AutoEncode::new(),
            quick_vars: Rc::new(RefCell::new(None)),
            shadow_orig: Rc::new(RefCell::new(None)),
            shadow_cand_id: Rc::new(RefCell::new(String::new())),
            view_visible: signal(false),
            view_code: signal(String::new()),
            view_text: signal(String::new()),
            view_weight: signal(String::new()),
            view_position: signal(String::new()),
            viewing: Rc::new(RefCell::new(None)),
            shadow_add_visible: signal(false),
            quick_edit_visible: signal(false),
            common_edit_visible: signal(false),
            common_choice: signal(0),
            only_modified: signal(false),
            quick_kind_idx: signal(0),
            quick_kind_labels: signal(Vec::new()),
            quick_var_text: signal(String::new()),
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

    /// 指定数据域的类型（前两槽是全局域，见 [`domain_slot`]）。
    pub fn domain_kind_at(&self, domain: usize) -> DomainKind {
        let i = match domain_slot(domain) {
            DomainSlot::Phrase => return DomainKind::Phrase,
            DomainSlot::QuickFormat => return DomainKind::QuickFormat,
            DomainSlot::CommonChars => return DomainKind::CommonChars,
            DomainSlot::Completion => return DomainKind::Completion,
            DomainSlot::Schema(i) => i,
        };
        match self.folded_domains().get(i).map(|e| e.kind.as_str()) {
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
        tabs_for_kind(self.domain_kind_at(domain))
    }

    /// 当前域的子标签序列。
    pub fn tabs_for_domain(&self) -> Vec<LogicalTab> {
        self.tabs_for_domain_at(self.domain.get())
    }

    /// 指定数据域的展示名（两个全局域各有固定名，其余取折叠域标签，如 "五笔86 · 码表"）。
    pub fn domain_label_at(&self, domain: usize) -> String {
        let i = match domain_slot(domain) {
            // 名字取自 [`GLOBAL_DOMAIN_LABELS`]，不另写字面量：这一处与 `domain_labels`
            // 必须逐字相同，否则侧栏与下拉会对同一个域显示两种叫法。
            DomainSlot::Phrase => return GLOBAL_DOMAIN_LABELS[DOMAIN_PHRASE].to_string(),
            DomainSlot::QuickFormat => return GLOBAL_DOMAIN_LABELS[DOMAIN_QUICK].to_string(),
            DomainSlot::CommonChars => return GLOBAL_DOMAIN_LABELS[DOMAIN_COMMON].to_string(),
            DomainSlot::Completion => {
                return GLOBAL_DOMAIN_LABELS[DOMAIN_COMPLETION].to_string();
            }
            DomainSlot::Schema(i) => i,
        };
        self.folded_domains()
            .get(i)
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

    /// 当前作用域方案 id（**两个全局域都返回 None**；拼音域返回代表方案 id）。
    pub fn current_schema_id(&self) -> Option<String> {
        let DomainSlot::Schema(i) = domain_slot(self.domain.get()) else {
            return None;
        };
        self.folded_domains()
            .get(i)
            .map(|e| e.rep_schema_id.clone())
    }

    // ── 程序化定位（命令行深链 / 加词小窗）─────────────────────

    /// 把外部传入的方案标识解析为数据域下标（全局域用固定关键字，其余为折叠后的方案域）。
    ///
    /// 按序尝试：`phrase` / `quick` → 精确方案 id → 引擎族名（`pinyin`/`codetable`/`mixed`）
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
            return Some(DOMAIN_PHRASE);
        }
        if s.eq_ignore_ascii_case("quick") {
            return Some(DOMAIN_QUICK);
        }
        // `charsets` 是这个域改名后的叫法（域标签已是「字符集分类」）；旧的两个别名
        // 保留——用户可能把 `--schema common-chars` 写进了快捷方式，改名不该弄坏它。
        if s.eq_ignore_ascii_case("common")
            || s.eq_ignore_ascii_case("common-chars")
            || s.eq_ignore_ascii_case("charsets")
        {
            return Some(DOMAIN_COMMON);
        }
        // 两个 tab 的 cli_id 也收进来：`--type mail-suffix` 单独给时用户多半没想过还要
        // 再给一个 `--schema`，而少了这一档的后果不是报错而是**静默跑偏**——
        // `LogicalTab::from_cli_id` 解析得动（`bad_tab` 为空，不会提示「未知类型」），
        // 域却停在启动态的「快捷短语」，最后 toast 一句「『快捷短语』没有『邮箱后缀』」。
        if s.eq_ignore_ascii_case("completion")
            || s.eq_ignore_ascii_case("mail-suffix")
            || s.eq_ignore_ascii_case("url-history")
        {
            return Some(DOMAIN_COMPLETION);
        }
        let domains = self.folded_domains();
        if let Some(i) = domains.iter().position(|d| d.rep_schema_id == s) {
            return Some(i + GLOBAL_DOMAINS);
        }
        let kind = s.to_ascii_lowercase();
        if let Some(i) = domains.iter().position(|d| d.kind == kind) {
            return Some(i + GLOBAL_DOMAINS);
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
                .map(|i| i + GLOBAL_DOMAINS);
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

    /// 数据域下拉选项标签（先两个全局域，其后折叠后各方案域）。
    ///
    /// 顺序必须与 [`domain_slot`] 的槽位一致——这里多写一项或少写一项，下拉的每一项都会
    /// 指向隔壁的域。
    pub fn domain_labels(&self) -> Vec<String> {
        let mut v: Vec<String> = GLOBAL_DOMAIN_LABELS.iter().map(|s| s.to_string()).collect();
        for e in self.folded_domains() {
            v.push(e.label);
        }
        v
    }

    /// 数据域下拉的富内容选项：方案名为主文本、引擎类型为尾随徽章（拼音=强调色，其余=中性）。
    /// 全局域一律标"全局"徽章。与 [`Self::domain_labels`] 同序、同源
    /// （[`GLOBAL_DOMAIN_LABELS`]）——两处各写一份字面量正是上一次加全局域时的漏网点。
    pub fn domain_rich_items(&self) -> Vec<DropdownItem> {
        let mut v: Vec<DropdownItem> = GLOBAL_DOMAIN_LABELS
            .iter()
            .map(|s| DropdownItem::new(*s).badge("全局", Intent::Neutral))
            .collect();
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

    /// 总页数。**空表也算一页**——否则跳页框旁边会显示「/ 0 页」，而"第 1 页 / 共 0 页"
    /// 本身就是自相矛盾的说法。
    pub fn total_pages(&self) -> usize {
        self.total.get().div_ceil(PAGE_SIZE).max(1)
    }

    /// 当前页码（1-based）。
    pub fn current_page(&self) -> usize {
        *self.offset.borrow() / PAGE_SIZE + 1
    }

    /// 跳到第 `page` 页（1-based）。
    ///
    /// **越界一律钳到 `[1, total_pages]`，不报错**：这个入口只有一种合理反应——用户打
    /// 「999」是想去最后一页，不是想看一条错误提示。
    pub fn goto_page(&self, page: usize) {
        if !self.current_spec().paged {
            return;
        }
        let off = page_offset(page, self.total_pages());
        if off == *self.offset.borrow() {
            // 页没变也要回填输入框：用户在只有 3 页的表里打了「99」，框里得显示钳回的 3，
            // 否则它会一直挂着一个按了没反应的数字。
            self.sync_page_input();
            return;
        }
        *self.offset.borrow_mut() = off;
        self.reload();
    }

    /// 跳页框回车提交。非数字/空串按「什么也没发生」处理（回填当前页）——为一个空框
    /// 弹错误提示，比直接复位更烦人。
    pub fn submit_page_jump(&self) {
        match self.page_input.get().trim().parse::<usize>() {
            Ok(p) => self.goto_page(p),
            Err(_) => self.sync_page_input(),
        }
    }

    /// 把跳页框回填成当前实际页码。
    ///
    /// 同值不写：`Signal::set` 每次都自增 version，正在编辑的输入框被重写会跳光标。
    fn sync_page_input(&self) {
        let now = self.current_page().to_string();
        if self.page_input.get() != now {
            self.page_input.set(now);
        }
    }

    pub fn first_page(&self) {
        self.goto_page(1);
    }

    pub fn last_page(&self) {
        self.goto_page(self.total_pages());
    }

    pub fn prev_page(&self) {
        if self.has_prev() {
            self.goto_page(self.current_page() - 1);
        }
    }

    pub fn next_page(&self) {
        if self.has_next() {
            self.goto_page(self.current_page() + 1);
        }
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
        // 「只看已修改」只对常用字有意义；别的类别没有「改过没有」这个维度，
        // 无条件发出去会让 core 侧多一个永远为 false 的参数。
        if spec.category == WordCategory::CommonChars && self.only_modified.get() {
            p.insert("onlyModified".into(), json!(true));
        }
        if spec.paged {
            p.insert("offset".into(), json!(*self.offset.borrow()));
            p.insert("limit".into(), json!(PAGE_SIZE));
            // 服务端排序：跨页全局排序由后端完成（sortBy 为返回 item 的 json 字段名）。
            if let Some(SortKey { column, order: ord }) = self.sort.get()
                && let Some(c) = spec.columns.get(column)
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
            && let Some(SortKey { column, order }) = self.sort.get()
            && let Some(c) = spec.columns.get(column)
        {
            sort_rows(&mut rows, c.field, order);
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
        // 跳页控件：页数文字 + 把框回填成实际页码。非分页类别置空（那组控件也不显示），
        // 留着上一个类别的「/ 7 页」比空着更糟。
        self.page_count_text.set(if spec.paged {
            format!("/ {} 页", self.total_pages())
        } else {
            String::new()
        });
        self.sync_page_input();
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
        // 导入导出的目标下拉与域下拉同源，只是去掉两个全局域（它们各有自己的导入导出流：
        // 短语走原生对话框、快捷输入走 quick.* 全局 RPC，都不是这套按方案分段的目标）。
        // 方案集变了就要重建，否则目标下拉会停在旧方案上。
        self.io_target_items.set(self.io_target_item_list());
        if self.io_target.get() >= self.folded_domains().len() {
            self.io_target.set(0);
        }
        // 域集合变化后，按下标记忆的子标签可能对不上位，清空避免错位恢复。
        self.sub_tab_memory.borrow_mut().clear();
        if self.domain.get() >= self.domain_count() {
            self.domain.set(DOMAIN_PHRASE);
        }
        self.last_domain.set(self.domain.get());
        // 域集合已换一批，词库窗左栏须整体重建（新方案要出现、删掉的要消失）。
        let epoch = self.domain_epoch.get().first().copied().unwrap_or(0);
        self.domain_epoch.set(vec![epoch + 1]);
    }

    /// 数据域总数（两个全局域 + 折叠后各方案域）。
    pub fn domain_count(&self) -> usize {
        GLOBAL_DOMAINS + self.folded_domains().len()
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
    /// `ok_msg` 为空 = **成功时不弹 toast**（失败仍要弹）。
    ///
    /// 给「点一下就完事」的轻操作用：常用字的常用⇄生僻切换，结果就在同一行的徽章上
    /// 立刻看得见，再飘一条 toast 属于用一个更重的东西去通报一件更轻的事。
    /// 加词、导入这类看不出结果的操作仍然要提示。
    fn write(&self, method: &str, params: Value, ok_msg: &str) {
        if method.is_empty() {
            self.notify_info("该操作不可用");
            return;
        }
        match rpc::call_core(method, &params) {
            Ok(_) => {
                self.reload();
                if !ok_msg.is_empty() {
                    self.notify_ok(ok_msg);
                }
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
    /// 启用 / 停用一行。
    ///
    /// 主键按类别分流：短语是 `(code, text)`，快捷输入是 `(kind, id)`。后者的 `text` 是模板、
    /// 用户可以改，认不了人；而格式 id 只在**同类内**唯一（高级用户手写 `system.quick.toml`
    /// 时不强制带 kind 前缀），跨类可能重名，故必须带上 kind。
    pub fn do_set_enabled(&self, row: &WordRow, enabled: bool) {
        let spec = self.current_spec();
        let mut p = self.scoped_params();
        if spec.category == WordCategory::QuickFormat {
            p.insert("kind".into(), json!(row.kind));
            p.insert("id".into(), json!(row.code));
        } else {
            p.insert("code".into(), json!(row.code));
            p.insert("text".into(), json!(row.text));
        }
        p.insert("enabled".into(), json!(enabled));
        let msg = if enabled { "已启用" } else { "已停用" };
        self.write(spec.rpc.set_enabled, Value::Object(p), msg);
    }

    // ── 快捷输入格式（全局域）────────────────────────────────

    /// 相对调序的目标下标；`None` = 这一行动不了。
    ///
    /// ★★ 基准是 [`WordRow::move_index`]（**候选内**下标），**不是列表行号**
    /// （[`WordRow::position`] / `displayPos`）。停用项占着行号却不在候选里，
    /// 拿行号算目标位置会让「上移一位」跳过好几条——而这种错在界面上极难看出来，
    /// 故抽成纯函数由测试钉住。
    ///
    /// 两种 `None`：停用（没有 move_index，得先启用）与已在首位（上移无意义）。
    pub(crate) fn quick_move_target(row: &WordRow, delta: i64) -> Option<usize> {
        let cur = row.move_index?;
        if delta < 0 && cur == 0 {
            return None;
        }
        Some((cur as i64 + delta).max(0) as usize)
    }

    /// 快捷输入写操作的 `(method, params)`。主键 `(kind, id)` + 附加参数。
    pub(crate) fn quick_call(
        &self,
        method: &'static str,
        row: &WordRow,
        extra: &[(&str, Value)],
    ) -> (&'static str, Value) {
        let mut p = serde_json::Map::new();
        p.insert("kind".into(), json!(row.kind));
        p.insert("id".into(), json!(row.code));
        for (k, v) in extra {
            p.insert((*k).to_string(), v.clone());
        }
        (method, Value::Object(p))
    }

    /// 快捷输入：相对调序（`delta` = -1 上移 / +1 下移）。
    pub fn do_quick_move(&self, row: &WordRow, delta: i64) {
        let Some(target) = Self::quick_move_target(row, delta) else {
            // 停用项要给出原因；已在首位则静默（按钮此时也是灰的，不该弹提示）。
            if row.move_index.is_none() {
                self.notify_info("已停用的格式不参与排序，请先启用");
            }
            return;
        };
        let msg = if delta < 0 { "已上移" } else { "已下移" };
        let (method, params) = self.quick_call(
            self.current_spec().rpc.move_to,
            row,
            &[("index", json!(target))],
        );
        self.write(method, params, msg);
    }

    /// 快捷输入：移到本类首位。
    pub fn do_quick_move_top(&self, row: &WordRow) {
        if row.move_index.is_none() {
            self.notify_info("已停用的格式不参与排序，请先启用");
            return;
        }
        let (method, params) =
            self.quick_call(self.current_spec().rpc.move_to, row, &[("index", json!(0))]);
        self.write(method, params, "已置顶");
    }

    /// 快捷输入：恢复这一条的默认（清掉它的调序与停用）。
    pub fn do_quick_reset_entry(&self, row: &WordRow) {
        let (method, params) = self.quick_call(self.current_spec().rpc.reset_entry, row, &[]);
        self.write(method, params, "已恢复此条");
    }

    /// 快捷输入：恢复**全部**默认（对每个类别各调一次 `quick.resetKind`）。
    ///
    /// 类别取自当前列表里**实际出现的** kind，不写死那五个：core 侧 kind 是解析器白名单，
    /// 将来新增一类，这里自动跟上；写死列表则会漏掉新类别，症状是「点了恢复默认，
    /// 偏偏有一类还保持着旧顺序」。
    pub fn do_quick_reset_all(&self) {
        let kinds = self.quick_kinds_present();
        let method = self.current_spec().rpc.reset_default;
        let mut failed = 0usize;
        for k in &kinds {
            if rpc::call_core(method, &json!({ "kind": k })).is_err() {
                failed += 1;
            }
        }
        self.reload();
        if failed == 0 {
            self.notify_ok("已恢复全部默认");
        } else {
            self.notify_err(&format!("{failed}/{} 个类别恢复失败", kinds.len()));
        }
    }

    /// 当前列表里出现过的格式类别（保序去重）。
    ///
    /// 取自数据而非写死那五个：core 侧 kind 是解析器白名单，将来新增一类，这里自动跟上。
    pub(crate) fn quick_kinds_present(&self) -> Vec<String> {
        let mut kinds: Vec<String> = Vec::new();
        for r in self.rows.borrow().iter() {
            if !r.kind.is_empty() && !kinds.contains(&r.kind) {
                kinds.push(r.kind.clone());
            }
        }
        kinds
    }

    /// 快捷输入：导出用户改动到文件。
    ///
    /// 导出的是**改动**（调序 / 停用），不是完整格式表——换机导入后，出厂新增的写法
    /// 仍会自然生效。文件是 TOML，可手工编辑后再导入。
    /// 打开「添加格式」对话框。`kind` 预选当前列表里最后一次看到的类别（见调用侧）。
    ///
    /// 复用 `edit_*` 那套信号：`edit_code` 存**类别**、`edit_text` 存模板，
    /// `editing_orig = None` 表示新增。快捷输入没有权重与位置维度，那两个信号不参与
    /// （`save_calls` 对本类别在解析权重**之前**就分流了，见 `quick_save_calls`）。
    pub fn open_quick_add(&self, kind: &str) {
        *self.editing_orig.borrow_mut() = None;
        let opts = self.quick_kind_options();
        self.quick_kind_idx
            .set(opts.iter().position(|k| k == kind).unwrap_or(0));
        self.edit_code.set(kind.to_string());
        self.edit_text.set(String::new());
        self.edit_title.set("添加快捷输入格式".into());
        self.edit_l_code.set("类别".into());
        self.edit_l_text.set("模板".into());
        self.quick_kind_labels.set(
            self.quick_kind_options()
                .iter()
                .map(|k| crate::pages::dict::spec::kind_label(k).to_string())
                .collect(),
        );
        self.quick_var_text.set(self.build_quick_var_text());
        self.quick_edit_visible.set(true);
    }

    /// 打开「编辑格式」对话框。
    ///
    /// 只对**用户自己加的**条目开放（出厂模板不可改，见 `spec::row_ops`）。这里不再判一次
    /// 归属：判据在 `row_ops` 一处，菜单与操作列都读它——两处各判一遍迟早漂移，
    /// 而症状是「菜单里能点、点了报错」。
    pub fn open_quick_edit(&self, row: &WordRow) {
        *self.editing_orig.borrow_mut() = Some((row.kind.clone(), row.code.clone()));
        let opts = self.quick_kind_options();
        self.quick_kind_idx
            .set(opts.iter().position(|k| *k == row.kind).unwrap_or(0));
        self.edit_code.set(row.kind.clone());
        self.edit_text.set(row.text.clone());
        self.edit_title.set("编辑快捷输入格式".into());
        self.edit_l_code.set("类别".into());
        self.edit_l_text.set("模板".into());
        self.quick_kind_labels.set(
            self.quick_kind_options()
                .iter()
                .map(|k| crate::pages::dict::spec::kind_label(k).to_string())
                .collect(),
        );
        self.quick_var_text.set(self.build_quick_var_text());
        self.quick_edit_visible.set(true);
    }

    /// 请求删除一条自定义格式（走确认弹窗——真删不可恢复）。
    pub fn request_quick_delete(&self, row: &WordRow) {
        *self.pending.borrow_mut() = Some(PendingAction::DeleteQuickFormat {
            kind: row.kind.clone(),
            id: row.code.clone(),
        });
        self.confirm_msg.set(format!(
            "确定删除自定义格式「{}」？此操作不可恢复。",
            row.text
        ));
        self.confirm_visible.set(true);
    }

    pub fn do_quick_delete(&self, kind: &str, id: &str) {
        let method = self.current_spec().rpc.remove;
        self.write(
            method,
            json!({ "kind": kind, "id": id }),
            "已删除自定义格式",
        );
    }

    /// 快捷输入的保存调用（新增 / 改模板）。抽成纯函数便于单测。
    ///
    /// `code` 是**类别**、`text` 是模板；`orig = Some((kind, id))` 表示改的是已有条目。
    #[allow(clippy::type_complexity)]
    pub(crate) fn quick_save_calls(
        &self,
        code: &str,
        text: &str,
        orig: Option<(String, String)>,
    ) -> Result<(Vec<(&'static str, Value)>, &'static str), String> {
        let spec = self.current_spec();
        if text.trim().is_empty() {
            return Err("模板不能为空".into());
        }
        match orig {
            // 编辑：类别不可改（它决定了可用变量与出厂分组），故只发新模板。
            // 想换类别就删掉重加——那本就是另一条格式。
            Some((kind, id)) => Ok((
                vec![(
                    spec.rpc.update,
                    json!({ "kind": kind, "id": id, "text": text }),
                )],
                "已保存",
            )),
            None => {
                if code.trim().is_empty() {
                    return Err("请选择类别".into());
                }
                Ok((
                    vec![(spec.rpc.add, json!({ "kind": code, "text": text }))],
                    "已添加",
                ))
            }
        }
    }

    /// 新增格式时可选的类别（kind 列表），**从当前列表数据现取**。
    ///
    /// ★ 不硬编码一份类别常量：core 的 `FormatKind::ALL` 是真相源，而 `quick.list` 会列出
    /// 每一类的条目（每类至少有出厂条目），故数据里必然带着完整的类别集合。主程序加一类
    /// 解析器时这里自动跟上——硬编码那份则会静默少一项，且没人会想到来改设置仓。
    pub(crate) fn quick_kind_options(&self) -> Vec<String> {
        self.quick_kinds_present()
    }

    /// 类别下标 → kind 字符串。越界回落首项（选项列表恒非空时不会发生）。
    pub(crate) fn quick_selected_kind(&self) -> String {
        let opts = self.quick_kind_options();
        opts.get(self.quick_kind_idx.get())
            .or_else(|| opts.first())
            .cloned()
            .unwrap_or_default()
    }

    /// 变量提示的展示行：`(左列, 右列)`。右列为空的那行是**节标题**。
    ///
    /// 一次列出全部类别而不是只列当前选中的那一类，有两个原因：
    /// 一是分段选择器没有变更回调，只显示当前类就得让整块跟着信号重建（windui 的
    /// `reactive` 是标记而非动态构建）；二是列全了反而更有用——用户能直接看到
    /// 「年月那类没有农历变量」这种对比，而那正是最容易写错的地方。
    ///
    /// 变量集相同的类别合并成一节（date 与 month_day 完全相同），否则同样的 17 行要出两遍。
    pub(crate) fn build_quick_var_lines(&self) -> Vec<(String, String)> {
        // 一轮 RPC 覆盖所有类别；放在循环外，core 断开时也只发一次。
        self.ensure_quick_vars();
        let kinds = self.quick_kind_options();
        let mut sections: Vec<QuickVarSection> = Vec::new();
        for k in &kinds {
            let vars = self.quick_vars_of(k);
            if vars.is_empty() {
                continue;
            }
            let label = crate::pages::dict::spec::kind_label(k).to_string();
            match sections.iter_mut().find(|(_, v)| *v == vars) {
                Some((names, _)) => names.push(label),
                None => sections.push((vec![label], vars)),
            }
        }
        let mut out = Vec::new();
        for (names, vars) in sections {
            out.push((names.join(" / "), String::new()));
            for (name, desc) in vars {
                out.push((format!("${name}"), desc));
            }
        }
        out
    }

    /// 变量提示的展示文本（多行）。
    ///
    /// 与 [`Self::build_quick_var_lines`] 的分工：那边算「哪些类别合并、列哪些变量」
    /// （纯数据，可单测），这里只把它拼成文本。两件事分开，排版改动不会牵动合并逻辑
    /// 的测试。
    ///
    /// 变量名与说明之间用**制表符**：等宽对齐交给控件，用空格填充会在非等宽字体下参差，
    /// 而用户复制走的正是这段文本，填充空格也会被一起带走。
    ///
    /// 末尾追加那句用法说明：它同样是「说明文本」，一并放进来就一并可选中复制。
    pub(crate) fn build_quick_var_text(&self) -> String {
        let mut out = String::new();
        for (left, right) in self.build_quick_var_lines() {
            if right.is_empty() {
                // 右列为空 = 节标题（「日期 / 月日」这种）
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&format!("【{left}】\n"));
            } else {
                out.push_str(&format!("{left}\t{right}\n"));
            }
        }
        out.push_str(
            "\n模板里的 $变量 会按实际输入替换；也可整条写成 {函数()} 表达式，两者不可混用。",
        );
        out
    }

    /// 确保变量提示已取过（没取过才发一轮 RPC）。
    ///
    /// 真相源在 core（`FormatKind::var_hints`）：设置仓自己硬编码一份，会在主程序加新变量时
    /// 静默过时——用户照提示写不出新变量，或提示里的变量已被删，两种都看不出是提示的错。
    ///
    /// 拉取独立于 [`Self::quick_vars_of`]，由 [`Self::build_quick_var_lines`] 在遍历类别
    /// **之前**调一次：拉取若写在按类查询里，core 断开时每类都会各发一轮，类别有几个就几轮。
    ///
    /// 失败**不**写缓存：core 可能只是还没起来（设置页先于它启动），留 `None` 让下次开
    /// 对话框重试，比一次连不上就整个会话没提示要好。
    fn ensure_quick_vars(&self) {
        if self.quick_vars.borrow().is_some() {
            return;
        }
        match rpc::call_core("quick.vars", &json!({})) {
            Ok(v) => {
                let mut cache: QuickVarsByKind = Vec::new();
                for k in v.as_array().map(|a| a.as_slice()).unwrap_or_default() {
                    let name = k
                        .get("kind")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let vars: Vec<(String, String)> = k
                        .get("vars")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .map(|it| {
                                    let s = |f: &str| {
                                        it.get(f).and_then(|x| x.as_str()).unwrap_or("").to_string()
                                    };
                                    (s("name"), s("desc"))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    cache.push((name, vars));
                }
                *self.quick_vars.borrow_mut() = Some(cache);
            }
            // 拿不到就不显示提示：对话框照样能用（模板非法时保存期会报错）。
            // 这里不弹提示——用户是来加条目的，一个「变量提示加载失败」只会添乱。
            Err(e) => tracing::warn!("快捷输入变量提示加载失败: {e}"),
        }
    }

    /// 取某类的模板变量提示。**纯查询**，不发 RPC——拉取见 [`Self::ensure_quick_vars`]。
    pub(crate) fn quick_vars_of(&self, kind: &str) -> Vec<(String, String)> {
        self.quick_vars
            .borrow()
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|(k, _)| k == kind)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    pub fn do_quick_export(&self, ctx: &mut EventCtx) {
        let state = self.clone();
        ctx.request_save_file(
            PickDialog::new()
                .title("导出 · 快捷输入")
                .file_name(Self::export_file_name_ext("quick-input", "toml"))
                .filter("TOML", &["toml"]),
            move |path| {
                let Some(path) = path else { return }; // 用户取消
                match rpc::call_core("quick.export", &json!({})) {
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

    /// 快捷输入：从文件导入用户改动（合并）。
    pub fn do_quick_import(&self, ctx: &mut EventCtx) {
        let state = self.clone();
        ctx.request_pick_file(
            PickDialog::new()
                .title("导入 · 快捷输入")
                .filter("TOML", &["toml"]),
            move |path| {
                let Some(path) = path else { return };
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(e) => {
                        state.notify_err(&format!("读文件失败: {e}"));
                        return;
                    }
                };
                match rpc::call_core("quick.import", &json!({ "content": content })) {
                    Ok(v) => {
                        state.reload();
                        state.notify_ok(&Self::quick_import_summary(&v));
                    }
                    Err(e) => state.notify_err(&format!("导入失败: {e}")),
                }
            },
        );
    }

    /// 导入结果的一句话摘要。
    ///
    /// 跳过项与「已忽略的自定义条目」**必须出现在文案里**：用户看到「导入成功」却少了规则，
    /// 无从判断是文件坏了还是程序吃了。core 那边如实上报，这里就不能咽下去。
    fn quick_import_summary(v: &Value) -> String {
        let n = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        let skipped = v
            .get("skipped")
            .and_then(|x| x.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let mut s = format!("已导入：调序 {} · 停用 {}", n("moved"), n("disabled"));
        // 字段名与语义在 core 侧一起改过：P1 是 `ignoredFormats`（没有落点，如实报「已忽略」），
        // 现在是 `formats`（真的写入）。文案跟着走，否则会一直说「暂不支持」。
        let formats = n("formats");
        if formats > 0 {
            s.push_str(&format!("；自定义条目 {formats} 条"));
        }
        if skipped > 0 {
            s.push_str(&format!("；{skipped} 条无法识别或重复已跳过"));
        }
        s
    }

    /// 常用字：导出用户调整为 TOML 文件。
    ///
    /// 导出的是**「我改过的那些字」**（core 侧只存与默认不同的字），不是 8104 字全表——
    /// 全表快照到了另一台机器上，会把对方默认字表里本就有的字也钉成显式覆盖，从此拿不到
    /// 默认表升版，而这件事导入时毫无异样。
    pub fn do_common_chars_export(&self, ctx: &mut EventCtx) {
        let state = self.clone();
        ctx.request_save_file(
            PickDialog::new()
                .title("导出 · 常用字")
                .file_name(Self::export_file_name_ext("common-chars", "toml"))
                .filter("TOML", &["toml"]),
            move |path| {
                let Some(path) = path else { return }; // 用户取消
                match rpc::call_core("commonChars.export", &json!({})) {
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

    /// 常用字：从文件导入用户调整（合并）。
    ///
    /// 过滤器带上 jsonl：整份备份里的 `userdata/common_chars.jsonl` 段是现成的一份，
    /// core 侧认得它，文件对话框却把它藏起来的话，用户只能改扩展名才选得到。
    pub fn do_common_chars_import(&self, ctx: &mut EventCtx) {
        let state = self.clone();
        ctx.request_pick_file(
            PickDialog::new()
                .title("导入 · 常用字")
                .filter("常用字调整", &["toml", "jsonl"]),
            move |path| {
                let Some(path) = path else { return };
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(e) => {
                        state.notify_err(&format!("读文件失败: {e}"));
                        return;
                    }
                };
                match rpc::call_core("commonChars.import", &json!({ "content": content })) {
                    Ok(v) => {
                        state.reload();
                        state.notify_ok(&Self::common_chars_import_summary(&v));
                    }
                    Err(e) => state.notify_err(&format!("导入失败: {e}")),
                }
            },
        );
    }

    /// 导入结果的一句话摘要。
    ///
    /// ★ 「与默认相同」这一项**必须出现在文案里**：两台机器的默认字表可能是不同版本，
    /// 文件里 100 个字到了本机可能只有 30 个需要记录（其余本就是这个判定）。core 如实
    /// 上报，这里咽下去的话，用户看到「已导入 30」会以为丢了 70 个字。
    /// 跳过项同理（同 [`Self::quick_import_summary`] 那条）。
    fn common_chars_import_summary(v: &Value) -> String {
        let n = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        let skipped = v
            .get("skipped")
            .and_then(|x| x.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let mut s = format!("已导入 {} 条调整", n("imported"));
        let same = n("sameAsDefault");
        if same > 0 {
            s.push_str(&format!("；{same} 个字与本机默认判定相同，无需记录"));
        }
        if skipped > 0 {
            s.push_str(&format!("；{skipped} 条无法识别已跳过"));
        }
        s
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
        } else {
            // ⚠️ 这里曾有一条「候选调整没有 clear RPC，就循环 remove_rule 逐条撤销」的
            // 退路。它依赖 `self.rows` 是全量——候选调整改服务端分页后那里只剩当前页，
            // 那条路会**静默只清一页**。故 core 侧补了 `shadow.clear`，退路一并退役：
            // 凡 `ops.clear` 为真的类别，`rpc.clear` 都必须非空（见 spec 的守护测试）。
            self.notify_err("该类别不支持清空");
        }
    }

    /// 恢复默认短语。
    pub fn do_reset_default(&self) {
        let spec = self.current_spec();
        // 快捷输入的「恢复默认」是逐类别的（core 那张表按 kind 索引），文案也不能写死短语。
        if spec.category == WordCategory::QuickFormat {
            return self.do_quick_reset_all();
        }
        // 反馈文案按类别给：写死「短语」的话，常用字整表恢复后会提示「已恢复默认短语」。
        let msg = match spec.category {
            WordCategory::CommonChars => "已恢复默认",
            _ => "已恢复默认短语",
        };
        self.write(spec.rpc.reset_default, json!({}), msg);
    }

    /// 常用字行内的一键切换：常用 ⇄ 生僻，**不经对话框**。
    ///
    /// 这一类只有一个二值属性，为改一个 bool 开一层模态再点确定，比操作本身还重。
    /// 复用 `commonChars.set`（与对话框、与候选右键同一个写端）——切到默认方向时
    /// core 会把那条调整删掉而不是写一条同向的冗余记录。
    pub fn toggle_common_row(&self, row: &WordRow) {
        let (method, params) = self.toggle_common_call(row);
        // **不弹 toast**：结果就在同一行的「当前」徽章上立刻看得见（改过的还会变色），
        // 再飘一条提示是用更重的东西去通报一件更轻的事。加词那类看不出结果的才需要。
        self.write(method, params, "");
    }

    /// 一键切换要发的 (方法, 参数)。**纯函数，便于单测键名与方向**（同 [`Self::delete_call`]）
    /// ——`toggle_common_row` 本身直接发 RPC，测不到。
    pub fn toggle_common_call(&self, row: &WordRow) -> (&'static str, Value) {
        let spec = self.current_spec();
        (
            spec.rpc.update,
            json!({ "char": row.text, "common": !row.common_now }),
        )
    }

    // ── 字符类：外部编辑 / 删除 ──────────────────────────────────────────────
    //
    // GUI **不做**成员与范围的逐条编辑：词库这类东西在界面里一条条改效率反而低，还让
    // 页面复杂度失控（2026-09-05 用户拍板）。改类走「文本作中介」——导出、打开编辑器、
    // 回读全在 `external_edit` 那个对话框里（`xedit_open`），文件首行写明「不会被自动
    // 读取」：那份文件是导出的副本，不是配置。

    /// 打开字符类的「外部编辑」对话框；`preselect` 为右键/行内按钮带来的那一行。
    pub fn charset_open_external(&self, preselect: Option<&str>) {
        let targets: Vec<(String, String)> = self
            .rows
            .borrow()
            .iter()
            .map(|r| (r.code.clone(), r.text.clone()))
            .collect();
        self.xedit_open(&super::external_edit::CHARSET_DOMAIN, &targets, preselect);
    }

    /// 删一个自建类。出厂类没有这个入口（右键菜单按 `class_builtin` 隐藏），core 也会拒。
    pub fn charset_delete(&self, row: &WordRow) {
        self.write(
            "charset.delete",
            json!({ "key": row.code }),
            "已删除该字符类",
        );
    }

    /// 把一个字符类的**默认判定**设成指定的三态之一。
    ///
    /// ⚠️ 三个明确的目标态，而不是「点一下循环切换」：三态的循环顺序用户猜不出来，
    /// 点两下发现回不到原处比多一层菜单烦得多。
    pub fn set_class_default(&self, row: &WordRow, value: Option<bool>) {
        let (method, params) = self.set_class_default_call(row, value);
        // 同 `toggle_common_row`：结果就在同一行的「默认判定」列上，不弹 toast。
        self.write(method, params, "");
    }

    /// 上者要发的 (方法, 参数)。纯函数便于单测键名与三态取值。
    pub fn set_class_default_call(
        &self,
        row: &WordRow,
        value: Option<bool>,
    ) -> (&'static str, Value) {
        let spec = self.current_spec();
        // `inherit` 是「本层不表态、跟随自带配置」，与显式设成常用**不是一回事**——后者会
        // 钉死，出厂哪天给这个类定了调，用户也跟不上。core 侧据此删掉那一行而不是写值。
        let v = match value {
            Some(true) => "common",
            Some(false) => "rare",
            None => "inherit",
        };
        // 主键是 key（`row.code`），不是显示名：显示名可被用户改，key 不会。
        (spec.rpc.update, json!({ "key": row.code, "default": v }))
    }

    /// 恢复一行默认要发的 (方法, 参数, 提示语)；`None` = 该类别另有专用流程。
    ///
    /// ★ 抽成纯函数是为了**单测键名**（同 [`Self::toggle_common_call`]）：两类的主键
    /// 完全不同——常用字是 `char`（那个字本身），字符类是 `key`。写错的话 core 报
    /// 「缺少参数」，而用户只是点了一下「恢复默认」。
    pub fn reset_entry_call(&self, row: &WordRow) -> Option<(&'static str, Value, &'static str)> {
        let spec = self.current_spec();
        match spec.category {
            // 行内轻操作，结果就在那一行的徽章上（值翻回默认、强调色褪掉），不弹 toast。
            WordCategory::CommonChars => {
                Some((spec.rpc.reset_entry, json!({ "char": row.text }), ""))
            }
            // 字符类撤掉的是用户层对这个类的**全部**自定义（那份 yaml 整个删掉），
            // 不只是默认判定——故这条要提示，别的字段变回去了用户看不见。
            WordCategory::Charsets => Some((
                spec.rpc.reset_entry,
                json!({ "key": row.code }),
                "已清除该类的全部自定义",
            )),
            _ => None,
        }
    }

    /// 恢复**一行**的默认（按类别分派）。
    ///
    /// 两类的主键完全不同：快捷输入是 `(kind, id)`，常用字是那个字本身。故不能共用
    /// 一套参数构造——写错键名 core 会报「缺少参数」，用户看到的是一句莫名其妙的报错。
    pub fn do_reset_entry(&self, row: &WordRow) {
        let spec = self.current_spec();
        match spec.category {
            WordCategory::QuickFormat => self.do_quick_reset_entry(row),
            // 同 `toggle_common_row`：行内轻操作，结果就在那一行的徽章上（值翻回默认、
            // 强调色褪掉），不弹 toast。整表恢复才提示——那个看不出结果。
            WordCategory::CommonChars | WordCategory::Charsets => {
                if let Some((m, p, toast)) = self.reset_entry_call(row) {
                    self.write(m, p, toast);
                }
            }
            _ => {}
        }
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
        // 文案按类别给：常用字这一类丢弃的是「哪些字算常用」的调整，说成「自定义短语」
        // 会让用户以为自己要删的是别的东西。
        let msg = match self.current_category() {
            WordCategory::CommonChars => "确定恢复默认？你对常用字做过的全部调整都会被清除。",
            _ => "确定恢复为默认短语？将丢弃自定义短语的改动。",
        };
        self.confirm_msg.set(msg.into());
        self.confirm_visible.set(true);
    }

    /// 整类操作：先取预览，把影响面摆给用户看，确认后才写。
    ///
    /// ★ **预览必须报「命中多少条词条」，不能只报「多少个字符」**：虎码里全角逗号 `，`
    /// 只是 1 个字符，却出现在 326 条词条里（词条内部的标点）。只说「1 个字符」，谁都会
    /// 毫不犹豫地点确定，然后那 326 个词一起判非常用。字符数与影响面差着两个数量级。
    pub fn open_common_bulk(&self, row: &WordRow, common: bool) {
        let params = json!({ "char": row.text, "common": common, "apply": false });
        let preview = match rpc::call_core("commonChars.bulkByBlock", &params) {
            Ok(v) => v,
            // core 的报错文案本身可读（如「『基本汉字』由默认字表逐字管辖」），原样透出。
            Err(e) => return self.notify_err(&e.to_string()),
        };
        let n = |k: &str| preview.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        let (chars, entries) = (n("chars"), n("entries"));
        let block = preview
            .get("block")
            .and_then(|v| v.as_str())
            .unwrap_or(&row.block)
            .to_string();

        if chars == 0 {
            // 不是错误：这一类在当前词库里根本没出现过，本来就没什么可设的。
            // 说成「操作失败」会让用户以为功能坏了，反复去点。
            return self.notify_info(&format!("当前词库里没有「{block}」这一类的字符"));
        }
        let label = common_label(common);
        *self.pending.borrow_mut() = Some(PendingAction::CommonBulk {
            ch: row.text.clone(),
            common,
        });
        self.confirm_msg.set(format!(
            "将「{block}」的 {chars} 个字符全部设为{label}？\n\
             这些字符出现在当前词库的 {entries} 条词条里。"
        ));
        self.confirm_visible.set(true);
    }

    /// 执行待确认操作。
    pub fn confirm_pending(&self) {
        let act = self.pending.borrow_mut().take();
        self.confirm_visible.set(false);
        match act {
            Some(PendingAction::DeleteRow(c, t)) => self.do_delete_row(&c, &t),
            Some(PendingAction::DeleteQuickFormat { kind, id }) => self.do_quick_delete(&kind, &id),
            Some(PendingAction::Clear) => self.do_clear(),
            Some(PendingAction::ResetDefault) => self.do_reset_default(),
            Some(PendingAction::CommonBulk { ch, common }) => self.write(
                "commonChars.bulkByBlock",
                json!({ "char": ch, "common": common, "apply": true }),
                // 这一类看不出结果——被设为生僻的字符多半根本不在当前这一页上，
                // 不给回执用户无从判断到底生效没有。
                "已按类型批量调整",
            ),
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
            // 短语默认 1800：主程序删除 `PHRASE_WEIGHT_BASE`(40M) 类别硬顶后，短语与码表
            // 精确候选**按权重竞争**（见主仓 docs/architecture/candidate-sorting-rules.md §5.1），
            // 这个默认值直接决定「新建的短语打不打得出」。五笔主库 median=941、p99=9000，
            // 1800 越过约 90% 的条目，又留足余量给用户手动上调（约定值域 0~10000）。
            // 此前是 1000，只压得住一半——在 40M 时代无所谓，现在会让新短语时灵时不灵。
            WordCategory::PhraseUser => ("添加短语", "编码", "短语", "1800"),
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
        // 预填字段同样会 bump 版本号，不复位就会被自动出码判成「用户手改编码」。
        self.auto_encode.reseed();
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
            .or_else(|| {
                (GLOBAL_DOMAINS..self.domain_count()).find_map(|d| userdict_sub(d).map(|s| (d, s)))
            })
            // 无任何方案域：退化到短语域，仍打开对话框（提交会因类别不支持而报错）
            .unwrap_or((0, 0));

        self.navigate_to(domain, Some(sub));

        *self.editing_orig.borrow_mut() = None;
        // core 只给词条不给码时当场出一次：自动出码的叶子以构建时版本为基线、不管预填，
        // 少了这一步用户看到的就是「词条填好了、编码空着」。出不了码就留空。
        let code = if code.trim().is_empty() && !text.trim().is_empty() {
            self.encode_text_silent(text).unwrap_or_default()
        } else {
            code.to_string()
        };
        self.edit_code.set(code);
        self.edit_text.set(text.to_string());
        self.edit_weight.set("1200".to_string());
        self.edit_position.set("0".to_string());
        self.edit_title.set("添加用户词库".to_string());
        self.edit_l_code.set("编码".to_string());
        self.edit_l_text.set("词条".to_string());
        self.auto_encode.reseed();
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
        // 回填已有词条不算用户手改；此后用户改词条仍会自动重算编码（改了词，旧码本就不对了）。
        self.auto_encode.reseed();
        self.edit_visible.set(true);
    }

    pub fn cancel_edit(&self) {
        self.edit_visible.set(false);
    }

    pub fn cancel_quick_edit(&self) {
        self.quick_edit_visible.set(false);
    }

    /// 当前快捷输入对话框是「编辑已有条目」而非「新增」。
    ///
    /// 编辑时类别锁定：类别决定可用变量与出厂分组，换类别本就是另一条格式。
    pub(crate) fn is_editing_quick(&self) -> bool {
        self.editing_orig.borrow().is_some()
    }

    /// 打开「添加常用字调整」对话框。
    pub fn open_common_add(&self) {
        *self.editing_orig.borrow_mut() = None;
        self.edit_text.set(String::new());
        // 默认「常用」：来这里手动添加多半是想把某个被判生僻的字提上来
        // （想压掉某个常用字，更顺手的入口是打出它然后右键）。
        self.common_choice.set(0);
        self.edit_title.set("添加常用字调整".into());
        self.common_edit_visible.set(true);
    }

    pub fn cancel_common_edit(&self) {
        self.common_edit_visible.set(false);
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

    /// 按当前类别为 `text` 反查编码。`Ok(None)` = 出不了码（core 回空码，典型是纯符号）。
    ///
    /// **出不了码时绝不写回空串**：编码框里可能已经有一个用户手填的、能用的码，
    /// 被一个符号抹掉是净损失。自动出码与手动按钮共用这一条判据。
    pub fn encode_text(&self, text: &str) -> Result<Option<String>, String> {
        let spec = self.current_spec();
        if !spec.ops.encode || text.trim().is_empty() {
            return Ok(None);
        }
        // 同 `save_calls`：`dict.encode` 也走 `store_text` 还原，收的是转义形态。
        // 词条框改多行后这里若发真换行，core 那侧还原不动它，逐字反查会撞上一个
        // 不可读音的字符而整条出不了码——按钮看着"没反应"。
        let text = escape_input_newlines(text);
        let mut p = self.scoped_params();
        p.insert("text".into(), json!(text));
        let v = rpc::call_core(spec.rpc.encode, &Value::Object(p))?;
        let code = v.as_str().unwrap_or_default().trim().to_string();
        Ok(if code.is_empty() { None } else { Some(code) })
    }

    /// 自动出码叶子用的静默版：失败一律当作「出不了码」。
    ///
    /// 刻意不提示：这条路每敲一个字就跑一次，把 RPC 错误弹成 toast 会变成一串刷屏，
    /// 而用户此刻要的只是「编码框先别动」。真要知道为什么，点「生成编码」会说。
    pub fn encode_text_silent(&self, text: &str) -> Option<String> {
        self.encode_text(text).ok().flatten()
    }

    /// 由「生成编码」按钮触发：按词条反查编码填入编码框，并恢复自动生成。
    pub fn encode_current(&self) {
        if !self.current_spec().ops.encode {
            return;
        }
        let text = self.edit_text.get();
        if text.trim().is_empty() {
            self.notify_info("请先填写词条");
            return;
        }
        match self.encode_text(&text) {
            Ok(Some(code)) => {
                self.edit_code.set(code);
                // 先写码再复位：这次写回不算用户手改，且把自动生成重新打开——
                // 点这个按钮本身就表达了「我要自动算出来的那个码」。
                self.auto_encode.reseed();
            }
            Ok(None) => self.notify_info("无法为该词条生成编码，已保留原编码"),
            Err(e) => self.notify_err(&format!("生成编码失败: {e}")),
        }
    }

    /// 常用字：`{char, common}`。新增与编辑走**同一个** RPC——两者都是「把这个字设成
    /// 这一边」，core 侧对已存在的字直接改写方向。
    ///
    /// 这里只拦「不是一个字」这种参数形态问题，**不判它是不是汉字**：准入判据在 core 一处
    /// （`is_markable`），前端抄一份迟早漂移，而漂移的症状是「设置页放行了、core 拒绝了」
    /// 或反过来「设置页拦住了一个其实能用的字」。core 拒绝时的报错文案本身可读，由
    /// `run_calls` 原样显示。
    ///
    /// 这份克制在 WindInput issue #83 得到了回报：core 把准入从「只认汉字」放开到「除空白
    /// 与控制字符外全放行」，本仓一行代码都不用改就跟着放开了。
    #[allow(clippy::type_complexity)]
    fn common_save_calls(
        &self,
        text: &str,
        editing: bool,
    ) -> Result<(Vec<(&'static str, Value)>, &'static str), String> {
        let spec = self.current_spec();
        let t = text.trim();
        if t.is_empty() {
            return Err("请填一个字".into());
        }
        // ⚠️ **不在这里数字符个数**。「一个字」按 Unicode 字素簇算，而 `⚽️` 是 2 个码位、
        // `👨‍👩‍👧` 是 5 个——按 `chars().count() == 1` 拦，用户粘一个 emoji 进来就被前端直接
        // 拒了，而 core 明明收得下。判据留在 core 一处（`single_markable_char`），
        // 它拒绝时的报错文案本身可读，由 `run_calls` 原样显示。
        let p = json!({ "char": t, "common": self.common_choice.get() == 0 });
        Ok((
            vec![(spec.rpc.add, p)],
            if editing { "已更新" } else { "已添加" },
        ))
    }

    /// 计算"保存"要执行的 RPC 调用序列（纯函数，便于单测）。
    /// 用户词库：改键走删旧+加新，仅权重变走 update；短语：走 add / update。
    #[allow(clippy::type_complexity)]
    pub fn save_calls(&self) -> Result<(Vec<(&'static str, Value)>, &'static str), String> {
        let spec = self.current_spec();
        let cat = spec.category;
        let code = self.edit_code.get();
        let text = self.edit_text.get();
        let orig = self.editing_orig.borrow().clone();
        // 快捷输入在解析权重**之前**分流：这一类没有权重维度，`edit_weight` 从来不被设置，
        // 让它先跑下面那个 parse 会得到「权重需为整数」——一个与用户所做之事毫无关系的报错。
        if cat == WordCategory::QuickFormat {
            // 类别取自分段选择器（`edit_code` 只是开对话框时的初值，用户可能改过选择）
            return self.quick_save_calls(&self.quick_selected_kind(), &text, orig);
        }
        // 同上：常用字也没有权重维度，落到下面那个 parse 会得到「权重需为整数」——
        // 一个与用户所做之事毫无关系的报错。
        // 同上：常用字也没有权重维度，落到下面那个 parse 会得到「权重需为整数」——
        // 一个与用户所做之事毫无关系的报错。回归见
        // `common_save_calls_emit_char_and_direction`（它刻意把 edit_weight 置空来锁这条）。
        if cat == WordCategory::CommonChars {
            return self.common_save_calls(&text, orig.is_some());
        }
        // 正文框是多行的，用户按回车得到的是**真换行**；而 core 收的 text 是**转义形态**
        // （出口 `ui_text` / 入口 `store_text` 一对投影，见 wind-webdata）。不在这里投影
        // 也能落库——`unescape_text_field` 对真换行原样放行——但那是绕开契约碰巧成立：
        // 一旦同一串里还有用户写的 `\\`，两种域混在一起，反斜杠的还原口径就说不清了。
        // 显式转成字面 `\n`，发出去的串自始至终只属于转义域一个域。
        //
        // 放在快捷输入/常用字两条早退**之后**：那两类没有多行入口（模板单行、常用字单字），
        // 且各自的 save_calls 对 text 另有形态约定，不该被这层投影穿过。
        let text = escape_input_newlines(&text);
        let weight: i64 = match self.edit_weight.get().trim().parse() {
            Ok(w) => w,
            Err(_) => return Err("权重需为整数".into()),
        };
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
                // 三个对话框共用 edit_* 字段与本提交路径，各有自己的可见性信号。
                // 无条件关掉全部，比在这里再判一次当前类别更不容易错（判错的症状是
                // 「保存成功了但窗口不关」）。
                self.edit_visible.set(false);
                self.quick_edit_visible.set(false);
                self.common_edit_visible.set(false);
                self.run_calls(calls, ok);
            }
            Err(e) => self.notify_err(&e),
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
        Self::export_file_name_ext(source, "wdict.yaml")
    }

    /// 同上，但指定扩展名——快捷输入导出的是 TOML，不是 wdict。
    fn export_file_name_ext(source: &str, ext: &str) -> String {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        format!("{source}-{stamp}.{ext}")
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
        match self.domain_kind() {
            DomainKind::Phrase => self.do_import_phrases(ctx),
            DomainKind::QuickFormat => self.do_quick_import(ctx),
            DomainKind::CommonChars => self.do_common_chars_import(ctx),
            _ => self.open_import_dialog(),
        }
    }

    /// 子标签行「导出」入口：短语域走原生对话框；方案域打开导出对话框。
    pub fn do_export(&self, ctx: &mut EventCtx) {
        match self.domain_kind() {
            DomainKind::Phrase => self.do_export_phrases(ctx),
            DomainKind::QuickFormat => self.do_quick_export(ctx),
            DomainKind::CommonChars => self.do_common_chars_export(ctx),
            _ => self.open_export_dialog(),
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
        // 纯词列表只产出用户词库这一类，别的勾选框在这条路上没有意义。
        // （`pending_wordlist` 仅在导入流程内有值，确认与取消都会清，不会漏进导出对话框。）
        if key != "userWords" && self.pending_wordlist.borrow().is_some() {
            return false;
        }
        Self::types_for_kind(self.io_target_kind()).contains(&key)
    }

    /// 某引擎类型的方案能承载哪些数据类型（与 `tabs_for_domain_at` 的标签集一一对应）。
    ///
    /// ⚠️ 这份清单是**三处同增同减**契约的一环：本函数（导入导出的分段）、
    /// [`tabs_for_kind`]（设置页子标签）、主仓 `wind-webdata::default_dict_sections`
    /// （core 侧的默认导出段）。三处都没有编译期约束，只改其中一两处的症状是
    /// 「设置页看得见、导出文件里没有」——用户直到还原时才发现丢了数据。
    /// 拼音就这么丢过候选调整：它曾因下调位被整体屏蔽而只有三段，置顶放开后
    /// tabs 与 core 都补了 Shadow，唯独这里漏了。
    fn types_for_kind(kind: DomainKind) -> &'static [&'static str] {
        match kind {
            // 拼音与码表同口径：置顶放开后拼音同样会产生候选调整规则。
            DomainKind::Pinyin | DomainKind::Codetable | DomainKind::Other => {
                &["userWords", "tempWords", "freq", "shadow"]
            }
            // 与 tabs_for_domain_at 保持一致：英文无临时词库。
            DomainKind::English => &["userWords", "freq", "shadow"],
            // 混输方案自身只有候选调整——五笔/拼音词条落在它的成员方案里。
            DomainKind::Mixed => &["shadow"],
            // 四个全局域都不参与这套「按方案分段」的导入导出，前三个各有自己的一条流：
            // 短语走原生对话框，快捷输入走 quick.export/import，常用字走
            // commonChars.export/import（另外它也随整份备份走 core 的
            // `userdata/common_chars.jsonl` 段——两条路并存，前者是单独分发一份
            // 「我的常用字判断」，后者是换机还原）。
            // 补全学习数据同样不走「按方案分段」的导入导出。它随整份备份走 core 的
            // `userdata/completions.jsonl` 段（两类装一个文件，每行自带 kind），设置页
            // 这边只提供查看与清空——单独分发一份「我打过哪些网址」没有使用场景，
            // 而那恰恰是隐私上最不该鼓励的操作。
            DomainKind::Phrase
            | DomainKind::QuickFormat
            | DomainKind::CommonChars
            | DomainKind::Completion => &[],
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
            // 这三个全局域各有自己的导入导出流（短语走 phrase.*、快捷输入走 quick.*、
            // 常用字走 commonChars.*），都不依赖「当前有没有方案」。
            DomainKind::Phrase | DomainKind::QuickFormat | DomainKind::CommonChars => true,
            // 输入补全**没有**自己的导入导出流：它随整份备份走 core 的
            // `userdata/completions.jsonl`，而单独分发一份「我打过哪些网址」没有使用
            // 场景，也恰恰是隐私上最不该鼓励的操作。
            //
            // 显式写出来而不是交给下面的 `_`：本函数是全页唯一一处**列举全局域**的
            // 地方，交给 catch-all 的话，下一个人按这张表核对新域时会得到「没跟上」
            // 的错觉，或者反过来照着 `_` 臂以为全局域不用管。落到 `_` 的结果
            // （`current_schema_id()` 对全局域恒 `None` ⇒ false）碰巧是对的，但那是
            // 巧合而不是声明。
            DomainKind::Completion => false,
            _ => self.current_schema_id().is_some(),
        }
    }

    /// 「导入/导出」按钮是否可用：短语域仅「用户短语」子标签可用（导入导出的是用户短语，
    /// 系统短语只读故禁用）；方案域有方案即可用。
    pub fn io_entry_enabled(&self) -> bool {
        match self.domain_kind() {
            DomainKind::Phrase => self.current_category() == WordCategory::PhraseUser,
            // ⚠️ 与短语域同款：这个域下有两个 tab，只有「常用字」那张表走通用导入导出流
            // （commonChars.*）。字符类的文件出入口是它自己的「新建类 / 从文件加载 /
            // 外部编辑」三枚按钮——语义不是「导入导出整表」，而是「一个类一份文件」，
            // 挂在通用按钮上文案对不上。
            //
            // ★ 这两道判据按**域**走、不看当前 tab 的 spec——`SPEC_CHARSETS` 里没声明
            // import/export，按钮却照样亮着。反过来的坑（声明了却不显示）本仓记过一次，
            // 这是它的另一面。
            DomainKind::CommonChars => self.current_category() == WordCategory::CommonChars,
            DomainKind::QuickFormat => true,
            _ => self.current_schema_id().is_some(),
        }
    }

    // ── 导入导出的目标方案（`io_target` 索引进 folded_domains）─────

    /// 目标方案下拉选项（去掉域下拉开头的全部全局域，其余同序）。
    ///
    /// ⚠️ 跳过的项数必须是 [`GLOBAL_DOMAINS`]，不能写字面量：`io_target` 是
    /// `folded_domains()` 的下标（不含全局槽），列表里多留一项就让「显示第 i 项」与
    /// 「实际第 i 个方案」整体错位一格——不报错、不 panic，只是选「五笔86」导出的是拼音。
    /// 加「快捷输入」这第二个全局域时正是这么错的，故此处与 [`Self::prepare_io_dialog`]
    /// 一并由 `io_target_dropdown_index_matches_target` 钉住。
    pub(crate) fn io_target_item_list(&self) -> Vec<DropdownItem> {
        self.domain_rich_items()
            .into_iter()
            .skip(GLOBAL_DOMAINS)
            .collect()
    }

    /// 目标方案 id（无方案时 None）。
    pub fn io_target_schema_id(&self) -> Option<String> {
        self.folded_domains()
            .get(self.io_target.get())
            .map(|e| e.rep_schema_id.clone())
    }

    /// 目标方案的引擎类型（无方案时按 `Other` 从宽，不隐藏数据）。
    pub fn io_target_kind(&self) -> DomainKind {
        match self.io_target.get().checked_add(GLOBAL_DOMAINS) {
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
        // domain 含全局槽、io_target 不含，换算减的是 GLOBAL_DOMAINS 而非 1
        // （见 io_target_item_list 的告警）。
        self.io_target
            .set(self.domain.get().saturating_sub(GLOBAL_DOMAINS));
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
        // 纯词列表：出码要到点导入才发生，此刻没有任何依赖方案的中间结果需要作废，
        // 只把文案里的方案名换掉即可（不做出码预览换来的简化，见 apply_word_list）。
        if let Some(p) = self.pending_wordlist.borrow().as_ref() {
            self.import_preview_msg.set(self.word_list_summary(p));
            self.import_ready
                .set(!p.words.is_empty() && self.io_type_applicable("userWords"));
            return;
        }
        // 其余格式：换目标等于换了兼容性判定与"能收哪些段"，必须按新目标重新预览。
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
        // 每次开对话框都回到「补充」：这是推荐项，上一次选了「跳过」不该粘在这儿——
        // 那会让下一次导入悄悄少掉一批词，而用户并不记得自己上次选过什么。
        self.io_fill_boundary.set(true);
        self.import_boundary_pending.set(0);
        self.import_boundary_rejected.set(0);
        self.io_format.set(0);
        self.io_format_note.set(String::new());
        *self.pending_import.borrow_mut() = None;
        *self.pending_import_path.borrow_mut() = None;
        *self.pending_wordlist.borrow_mut() = None;
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
                // ★ 把落点交给 core 直接写，而不是「core 回全文、这边再写盘」。后者要让
                // 整份词库穿过一帧 JSON-RPC，几十万条就撞 16MB 读帧上限（issue #101 的
                // 「导出失败: 读取失败: 帧长超限」正是在这里报的）。这一步之后 payload
                // 是 O(1)，上限消失。文件对话框本来就已经把路径给到手了，中转纯属多余。
                match rpc::call_core(
                    "dict.export",
                    &json!({
                        "schemaId": schema,
                        "sections": sections,
                        "path": path.to_string_lossy(),
                    }),
                ) {
                    Ok(_) => state.notify_ok("已导出"),
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
                // 路径与内容都留下：路径是 RPC 载荷（不受帧长限制），内容供本进程判格式。
                *state.pending_import_path.borrow_mut() = Some(path.to_string_lossy().into_owned());
                *state.pending_import.borrow_mut() = Some(content.clone());
                state.apply_picked_content(&content, true);
            },
        );
    }

    /// 选完文件（或改了格式下拉）后的分派：纯词列表走客户端解析，其余交给 core 预览。
    ///
    /// `allow_redirect` 只对 core 那条路有意义（见 [`Self::run_import_preview`]）。
    fn apply_picked_content(&self, content: &str, allow_redirect: bool) {
        if self.wants_word_list(content) {
            self.apply_word_list(content);
        } else {
            *self.pending_wordlist.borrow_mut() = None;
            self.run_import_preview(content, allow_redirect);
        }
    }

    /// 本次是否按纯词列表处理：用户强制指定，或自动识别通过了严格判据。
    fn wants_word_list(&self, content: &str) -> bool {
        match self.io_format.get() {
            1 => true,
            _ => super::wordlist::looks_like_word_list(content),
        }
    }

    /// 纯词列表：客户端解析即出结果，**不发 RPC、不出码**。
    ///
    /// 这一步只回答「这文件里有多少词」。出码要等点导入——不做出码预览换来的正是
    /// 选文件的即时响应（否则这里就得同步跑完整批出码，几千词卡住一两秒）。
    fn apply_word_list(&self, content: &str) {
        let parsed = super::wordlist::parse_word_list(content);
        self.io_format_note.set("纯词列表".into());
        self.import_compatible.set(true);
        // 纯词列表只可能进用户词库，其余类型与它无关。
        self.io_user_words.set(true);
        self.io_temp_words.set(false);
        self.io_freq.set(false);
        self.io_shadow.set(false);
        self.import_preview_msg.set(self.word_list_summary(&parsed));
        self.import_ready
            .set(!parsed.words.is_empty() && self.io_type_applicable("userWords"));
        *self.pending_wordlist.borrow_mut() = Some(parsed);
    }

    /// 纯词列表的回显文案（词数 + 跳过/去重 + 出码归属说明）。
    fn word_list_summary(&self, p: &super::wordlist::WordList) -> String {
        if p.words.is_empty() {
            return "文件里没有可用的词条。".into();
        }
        let mut extra: Vec<String> = Vec::new();
        if p.skipped > 0 {
            extra.push(format!("跳过 {} 行", p.skipped));
        }
        if p.duplicates > 0 {
            extra.push(format!("去重 {}", p.duplicates));
        }
        let tail = if extra.is_empty() {
            String::new()
        } else {
            format!("（{}）", extra.join(" · "))
        };
        format!(
            "识别为 纯词列表 · {} 个词{tail}\n编码将按「{}」现场生成，出不了码的词会被跳过。",
            p.words.len(),
            self.io_target_label()
        )
    }

    /// 本次导入是否已按纯词列表解析（驱动权重行显隐）。
    pub fn is_word_list_import(&self) -> bool {
        self.pending_wordlist.borrow().is_some()
    }

    /// 格式下拉变化：按新格式重新解释已选文件。由 view 侧 effect 调用。
    pub fn on_io_format_changed(&self) {
        let content = self.pending_import.borrow().clone();
        match content {
            // 改格式后 redirect 必须关掉：那是「刚选完文件」的一次性自动纠偏，
            // 用户此刻正在手动指定格式，再把目标弹走就是跟他对着干。
            Some(c) => self.apply_picked_content(&c, false),
            None => self.io_format_note.set(String::new()),
        }
    }

    /// 给 `dict.previewImport` / `dict.import` 的载荷挂上文本来源：**有路径就传路径**，
    /// 否则回落整份内容。
    ///
    /// ★ 预览与导入必须共用这一个函数。两处各写各的，早晚会一处传了 path、另一处还传
    /// content——预览通过、真导入却撞帧长上限，而这两步之间用户什么也没改，最难自查。
    ///
    /// 回落那条路留给纯词列表：它的 TSV 是内存里现拼的，没有文件可指。⚠️ 那条路因此仍
    /// 受 16MB 约束（约 50 万词），本轮未处理——见 issue #101 的收尾说明。
    fn attach_import_source(&self, params: &mut Value, content: &str) {
        let path = self.pending_import_path.borrow().clone();
        let obj = params.as_object_mut().expect("params 必是对象");
        match path {
            Some(p) => {
                obj.insert("path".into(), Value::String(p));
            }
            None => {
                obj.insert("content".into(), Value::String(content.to_string()));
            }
        }
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
        let mut params = json!({ "schemaId": schema });
        self.attach_import_source(&mut params, content);
        match rpc::call_core("dict.previewImport", &params) {
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
                self.io_format_note.set(
                    Self::format_label(v.get("format").and_then(|x| x.as_str()).unwrap_or(""))
                        .into(),
                );
                self.apply_import_preview_sections(&v);
                let (pending, rejected) = Self::boundary_counts(&v);
                self.import_boundary_pending.set(pending);
                self.import_boundary_rejected.set(rejected);
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

    /// previewImport 响应 → 拼音词条准入契约的两行说明（纯函数便于单测）。
    ///
    /// 两档都为 0 时返回空串——码表方案与本就带分隔的拼音词库都走这一支，
    /// **不给它们凭空加一段看不懂的话**。
    ///
    /// ★ 「非法」档一定要列样例：用户看到 `wgkq 工` 才能立刻明白自己选错了文件。
    /// 只说「有 M 条不导入」等于让他自己去猜是哪些、为什么。
    fn format_boundary_note(v: &Value) -> String {
        let (pending, rejected) = Self::boundary_counts(v);
        let mut s = String::new();
        if pending > 0 {
            s.push_str(&format!(
                "\n· {pending} 条缺少音节分隔，可由程序按「音节数=字数」解出（下方可改）"
            ));
        }
        let no_reading = Self::sum_section_field(v, "boundaryNoReading");
        if no_reading > 0 {
            // ★ 这一档**会导入**，措辞不能沾「不会导入 / 选错文件」的边——它多半是用户
            // 自己加的「拼音码 → 符号」词条（`zuo ←`），那是正当用法，报警只会吓着他。
            // 但数还是要报：从前这类行按非法拒收并告警，改判之后若某次导入这个数很大，
            // 用户仍该有机会怀疑自己拿错了文件。
            s.push_str(&format!(
                "\n· {no_reading} 条含符号或外文（拼音词典里无读音），已按音节数切分导入"
            ));
        }
        if rejected > 0 {
            s.push_str(&format!(
                "\n⚠ {rejected} 条无法解析为拼音音节，不会导入——可能是码表词库选错了文件"
            ));
            let samples: Vec<&str> = v
                .get("sections")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|sec| sec.get("rejectedSamples").and_then(|x| x.as_array()))
                        .flatten()
                        .filter_map(|x| x.as_str())
                        .take(3)
                        .collect()
                })
                .unwrap_or_default();
            if !samples.is_empty() {
                s.push_str(&format!("：{}", samples.join("、")));
            }
        }
        s
    }

    /// 跨全部段累加某个计数字段。缺字段按 0——core 只在拼音方案下产出这些字段。
    fn sum_section_field(v: &Value, key: &str) -> u64 {
        v.get("sections")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|sec| sec.get(key).and_then(|x| x.as_u64()))
                    .sum()
            })
            .unwrap_or(0)
    }

    /// previewImport 响应 → `(可补齐行数, 判非法行数)`，跨全部段累加。纯函数便于单测。
    ///
    /// 两个数都只在拼音方案下非零：码表方案的编码没有音节语义，core 侧原样放行、
    /// 这些字段压根不出现。⇒ **不必在 UI 侧判断目标是不是拼音**，为 0 即不显示。
    fn boundary_counts(v: &Value) -> (u64, u64) {
        let mut pending = 0u64;
        let mut rejected = 0u64;
        if let Some(arr) = v.get("sections").and_then(|x| x.as_array()) {
            for sec in arr {
                pending += sec
                    .get("boundaryFilled")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
                rejected += sec.get("rejected").and_then(|x| x.as_u64()).unwrap_or(0);
            }
        }
        (pending, rejected)
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

    /// previewImport 的 `format` → 中文名（空串 = 未知，调用方据此不显示）。
    fn format_label(fmt: &str) -> &'static str {
        match fmt {
            "rime" => "Rime 词库",
            "tsv" => "TSV 文本",
            "winddict" => "WindDict 词库",
            _ => "",
        }
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
        let fmt_name = Self::format_label(v.get("format").and_then(|x| x.as_str()).unwrap_or(""));
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
        s.push_str(&Self::format_boundary_note(v));
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

    /// 分批出码：把词列表送 `dict.encodeWords`，回收 `(code, text)`（丢弃出不了码的）。
    /// 返回 `(可入库行, 出不了码数)`。
    ///
    /// 分批而非一次全量，两个理由都是硬的：`rpc::call_core` 有 5 秒超时；core 侧单次
    /// `encode_word` 每调一次就重读一次方案文件，批量入口才会把 spec 提到循环外。
    fn encode_word_batches(
        &self,
        schema: &str,
        words: &[String],
    ) -> Result<(Vec<(String, String)>, usize), String> {
        let mut rows: Vec<(String, String)> = Vec::with_capacity(words.len());
        let mut unencodable = 0usize;
        for batch in super::wordlist::batches(words) {
            let v = rpc::call_core(
                "dict.encodeWords",
                &json!({ "schemaId": schema, "texts": batch }),
            )
            .map_err(|e| format!("出码失败: {e}"))?;
            let (batch_rows, n) = Self::zip_codes(batch, &v)?;
            rows.extend(batch_rows);
            unencodable += n;
        }
        Ok((rows, unencodable))
    }

    /// 把一批出码结果配回词并剔除空码。返回 `(可入库行, 空码数)`。纯函数，便于单测。
    ///
    /// 契约是 **codes 与 texts 同序等长**。长度对不上宁可整体失败也不按下标硬配——
    /// 错位的结果是把 A 的码安到 B 的词上，静默写进词库，事后极难发现。
    fn zip_codes(words: &[String], v: &Value) -> Result<(Vec<(String, String)>, usize), String> {
        let codes = v
            .get("codes")
            .and_then(|x| x.as_array())
            .ok_or("出码返回缺 codes")?;
        if codes.len() != words.len() {
            return Err(format!(
                "出码返回条数不符（送 {} 回 {}），已中止",
                words.len(),
                codes.len()
            ));
        }
        let mut rows = Vec::with_capacity(words.len());
        let mut unencodable = 0usize;
        for (word, code) in words.iter().zip(codes) {
            match code.as_str().unwrap_or("") {
                "" => unencodable += 1,
                c => rows.push((c.to_string(), word.clone())),
            }
        }
        Ok((rows, unencodable))
    }

    /// 纯词列表导入：分批出码 → 拼 TSV → 走既有 `dict.import` 通道。
    ///
    /// 复用 TSV 而不新开一条导入协议，是因为出好码之后这就是标准的
    /// `code<TAB>text<TAB>weight`——那条路的解析、去重、merge/replace 语义全部现成，
    /// core 侧因此只需新增一个批量出码 RPC，`wind-store` 一行都不用动。
    fn import_word_list(&self, strategy: &str, words: &[String]) {
        let Some(schema) = self.io_target_schema_id() else {
            self.notify_err("暂无已启用方案");
            return;
        };
        let weight: i64 = match self.io_weight.get().trim().parse() {
            Ok(w) => w,
            Err(_) => {
                self.notify_err("权重需为整数");
                return;
            }
        };
        let (rows, unencodable) = match self.encode_word_batches(&schema, words) {
            Ok(r) => r,
            Err(e) => {
                self.notify_err(&e);
                return;
            }
        };
        if rows.is_empty() {
            // 不做出码预览的代价：这只能等到点了导入才发现，故话要说全（是方案不对，
            // 而不是文件有问题——文件在选的时候已经报过词数了）。
            self.notify_err(&format!(
                "{} 个词在「{}」下都出不了码，请确认目标方案是否正确",
                words.len(),
                self.io_target_label()
            ));
            return;
        }
        self.import_confirm_visible.set(false);
        self.import_ready.set(false);
        *self.pending_import.borrow_mut() = None;
        *self.pending_wordlist.borrow_mut() = None;

        // 分批发送：这条路的 TSV 是内存里现拼的、没有文件可指，只能走 `content`，于是
        // 仍受 16MB 读帧上限约束。词表规模由用户决定，我们无从预知，故按字节切批。
        let batches =
            super::wordlist::import_batches(&rows, weight, super::wordlist::IMPORT_BATCH_BYTES);
        let mut reports: Vec<Value> = Vec::with_capacity(batches.len());
        for (i, batch) in batches.iter().enumerate() {
            let tsv = super::wordlist::to_tsv(batch, weight);
            let params = Self::word_list_batch_params(&schema, &tsv, i, strategy);
            match rpc::call_core("dict.import", &params) {
                Ok(v) => reports.push(v),
                Err(e) => {
                    // 部分成功要如实说：前面几批已经落库了，不能只报一句「导入失败」，
                    // 那会让用户以为什么都没发生，于是重来一遍、把前面的又导一次。
                    self.reload();
                    let done: u64 = reports
                        .iter()
                        .map(|v| Self::sum_section_field(v, "added"))
                        .sum();
                    self.notify_err(&format!(
                        "导入中断（第 {}/{} 批）：{e}；此前已导入 {done} 条，可重新导入剩余部分（已在库的会按合并处理）",
                        i + 1,
                        batches.len()
                    ));
                    return;
                }
            }
        }
        self.navigate_to(
            self.io_target.get() + GLOBAL_DOMAINS,
            self.landing_sub(LogicalTab::UserDict),
        );
        self.reload();
        let mut msg = Self::format_import_result(&Self::merge_import_reports(&reports));
        if unencodable > 0 {
            msg.push_str(&format!("；{unencodable} 个词出不了码已跳过"));
        }
        self.notify_ok(&msg);
    }

    /// 纯词列表第 `index` 批的 `dict.import` 载荷。
    ///
    /// ★★ **只有第一批能用用户选的策略，其余一律 merge。**
    ///
    /// `replace` 的语义是「先清空所选类型，再导入」。每批都照做的话，第二批会把第一批
    /// 刚写进去的清掉，最终库里只剩最后一批——用户选了替换、导入 50 万词、界面报成功，
    /// 实际只进去最后几万条。
    ///
    /// ⚠️ 抽成纯函数正是为了能测：这个 bug 只在**切得出第二批**时出现，而日常用的词表
    /// 都只有一批，走 RPC 的集成测试永远绿着。
    fn word_list_batch_params(schema: &str, tsv: &str, index: usize, chosen: &str) -> Value {
        json!({
            "schemaId": schema,
            "content": tsv,
            "strategy": if index == 0 { chosen } else { "merge" },
            "sections": ["userWords"],
        })
    }

    /// 把多批 `dict.import` 的响应合成一份，供 [`Self::format_import_result`] 照常渲染。
    ///
    /// 按 `key` 分组，**所有数值字段一律累加**——不列举字段名是有意的：core 侧后来新增
    /// 的计数档（如 `boundaryNoReading`）会自动跟上，列白名单则会在加字段那天静默漏掉，
    /// 而表现只是某个数偏小，没人看得出来。非数值字段取首批的值。
    fn merge_import_reports(reports: &[Value]) -> Value {
        // 单批是绝大多数情况（19 万词也只有一批），原样返回省掉一趟重建。
        if let [only] = reports {
            return only.clone();
        }
        let mut order: Vec<String> = Vec::new();
        let mut acc: std::collections::HashMap<String, serde_json::Map<String, Value>> =
            std::collections::HashMap::new();
        for rep in reports {
            let Some(arr) = rep.get("sections").and_then(|x| x.as_array()) else {
                continue;
            };
            for sec in arr {
                let Some(obj) = sec.as_object() else { continue };
                let key = obj
                    .get("key")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let slot = acc.entry(key.clone()).or_insert_with(|| {
                    order.push(key.clone());
                    serde_json::Map::new()
                });
                for (k, v) in obj {
                    match (v.as_u64(), slot.get(k).and_then(|x| x.as_u64())) {
                        (Some(n), Some(prev)) => {
                            slot.insert(k.clone(), json!(prev + n));
                        }
                        _ => {
                            slot.entry(k.clone()).or_insert_with(|| v.clone());
                        }
                    }
                }
            }
        }
        let sections: Vec<Value> = order
            .into_iter()
            .filter_map(|k| acc.remove(&k).map(Value::Object))
            .collect();
        json!({ "sections": sections })
    }

    /// 目标域里 `tab` 对应的子标签下标（该域没有这一类时 None）。
    fn landing_sub(&self, tab: LogicalTab) -> Option<usize> {
        self.tabs_for_domain_at(self.io_target.get() + GLOBAL_DOMAINS)
            .iter()
            .position(|x| *x == tab)
    }

    /// 确认导入（strategy: "merge" | "replace"）：按勾选的数据类型导入。
    pub fn confirm_import(&self, strategy: &str) {
        // 纯词列表走独立通道（客户端出码 + TSV），与多段导入无关。
        let wl = self
            .pending_wordlist
            .borrow()
            .as_ref()
            .map(|p| p.words.clone());
        if let Some(words) = wl {
            self.import_word_list(strategy, &words);
            return;
        }
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
        // fillBoundary 只在有可补齐行时才有意义，但恒传更省事也更诚实：core 侧缺省 true，
        // 传与不传对码表方案完全等价（那边根本不进求解链）。
        let fill_boundary = self.io_fill_boundary.get();
        let mut params = json!({ "schemaId": schema, "strategy": strategy, "sections": sections, "fillBoundary": fill_boundary });
        self.attach_import_source(&mut params, &content);
        match rpc::call_core("dict.import", &params) {
            Ok(v) => {
                // 导入目标可能不是当前正看着的域（自动匹配或手动改过）。留在原地只会让人
                // 再次得出"导了但没有"的结论——把视图带到刚写入的方案与类型上，眼见为实。
                let sub = landing.and_then(|t| self.landing_sub(t));
                self.navigate_to(self.io_target.get() + GLOBAL_DOMAINS, sub);
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
        // 少掉的词按**原因**分列。三个原因对用户意味着三种不同的下一步：拒收 = 可能选错
        // 文件；跳过 = 他自己刚做的选择；只报一个「跳过 N 行」等于把这两件事混成一件。
        let mut why: Vec<String> = Vec::new();
        let sum = |k: &str| {
            v.get("sections")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.get(k).and_then(|x| x.as_u64()))
                        .sum::<u64>()
                })
                .unwrap_or(0)
        };
        let rejected = sum("rejected");
        let unfilled = sum("boundarySkipped");
        if rejected > 0 {
            why.push(format!("{rejected} 条非拼音音节未导入"));
        }
        if unfilled > 0 {
            why.push(format!("{unfilled} 条按你的选择跳过"));
        }
        let tail = if why.is_empty() {
            String::new()
        } else {
            format!("（{}）", why.join("，"))
        };
        if parts.is_empty() {
            format!("已导入{tail}")
        } else {
            format!("已导入：{}{tail}", parts.join("；"))
        }
    }

    pub fn cancel_import(&self) {
        *self.pending_import.borrow_mut() = None;
        *self.pending_wordlist.borrow_mut() = None;
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
    const PINYIN_TYPES: &[&str] = &["userWords", "tempWords", "freq", "shadow"];
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

    /// 期望的域标签：全局域那一段 + 给定的方案域标签。
    ///
    /// 全局域从 [`GLOBAL_DOMAIN_LABELS`] 派生而不在各测试里重抄一遍——那些用例要钉的是
    /// 「方案域跟在全局域之后、且拼音族已折叠」，全局域有几个、各叫什么由
    /// `domain_slot_maps_global_slots_then_schemas` 单独把关。抄一份的代价是每加一个
    /// 全局域就有一批不相干的用例跟着变红，红得多了就没人细看红的是什么。
    fn global_labels_then(schema_labels: &[&str]) -> Vec<String> {
        GLOBAL_DOMAIN_LABELS
            .iter()
            .chain(schema_labels.iter())
            .map(|s| s.to_string())
            .collect()
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
                // 混输域只有「候选调整」一类，是夹具里唯一还缺类型的普通数据域——
                // 深链落空的用例靠它。**加在末尾**：前面三项的域下标被多处断言写死。
                schema("mixed", "混输", "mixed"),
            ],
        );
        st
    }

    // ───────── 快捷输入格式（全局域）─────────

    /// 域下标 → 语义的三段映射。**本次改动的全部风险都在这里**：偏移错一格，
    /// 用户选 A 方案就会看到 B 方案的数据，而且不 panic、不报错。
    #[test]
    fn domain_slot_maps_global_slots_then_schemas() {
        assert_eq!(domain_slot(DOMAIN_PHRASE), DomainSlot::Phrase);
        assert_eq!(domain_slot(DOMAIN_QUICK), DomainSlot::QuickFormat);
        assert_eq!(domain_slot(DOMAIN_COMMON), DomainSlot::CommonChars);
        assert_eq!(domain_slot(DOMAIN_COMPLETION), DomainSlot::Completion);
        // 方案域从 GLOBAL_DOMAINS 起，且**已减去**全局槽位（可直接索引 folded_domains）。
        assert_eq!(domain_slot(GLOBAL_DOMAINS), DomainSlot::Schema(0));
        assert_eq!(domain_slot(GLOBAL_DOMAINS + 3), DomainSlot::Schema(3));
        // 全局槽位必须两两不同且**连续占满** [0, GLOBAL_DOMAINS)：
        // 写重会让其中一个域永远打不开，留空洞会让那一格点进去落到方案域上。
        let slots = [
            DOMAIN_PHRASE,
            DOMAIN_QUICK,
            DOMAIN_COMMON,
            DOMAIN_COMPLETION,
        ];
        assert_eq!(slots.len(), GLOBAL_DOMAINS, "新增全局域要在这里补一格");
        let mut sorted = slots;
        sorted.sort_unstable();
        assert_eq!(sorted, [0, 1, 2, 3], "全局槽位必须从 0 起连续，不能重号或跳号");
        // 标签表与槽位同长，且每格都有名字（空名字＝侧栏上一条看不见的项）。
        assert_eq!(GLOBAL_DOMAIN_LABELS.len(), GLOBAL_DOMAINS);
        assert!(GLOBAL_DOMAIN_LABELS.iter().all(|s| !s.is_empty()));
    }

    /// 快捷输入是**全局**域：没有 schemaId，参数里也不该出现它。
    ///
    /// 带上 schemaId 的后果不是报错而是「core 侧忽略掉、但列表看起来正常」，
    /// 直到有人按方案去筛数据才会发现。
    #[test]
    fn quick_domain_is_global_and_sends_no_schema_id() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(DOMAIN_QUICK);
        st.sub_tab.set(0);
        assert_eq!(st.domain_kind(), DomainKind::QuickFormat);
        assert_eq!(st.current_category(), WordCategory::QuickFormat);
        assert_eq!(st.current_schema_id(), None, "全局域没有方案");
        assert_eq!(st.domain_label_at(DOMAIN_QUICK), "快捷输入");
        // 只有一个子标签（五个类别在同一张表里靠「类别」列区分）
        assert_eq!(st.tabs_for_domain(), vec![LogicalTab::QuickFormat]);
        let p = st.build_list_params();
        assert!(p.get("schemaId").is_none(), "全局域不该带 schemaId: {p:?}");
        // 导入导出入口可用（走 quick.* 全局流，不依赖有没有方案）
        assert!(st.io_entry_visible() && st.io_entry_enabled());
    }

    // ───────── 字符集分类（全局域，两层视图的第一层）─────────

    fn charsets_state() -> DictManagerState {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(DOMAIN_COMMON);
        st.sub_tab.set(SUB_TAB_CHARSETS);
        st
    }

    /// 「字符类」tab 走 charset.* 那套 RPC，而不是常用字那套。
    #[test]
    fn charsets_tab_uses_its_own_rpcs() {
        let st = charsets_state();
        assert_eq!(st.current_category(), WordCategory::Charsets);
        let spec = st.current_spec();
        assert_eq!(spec.rpc.list, "charset.list");
        assert_eq!(spec.rpc.update, "charset.set");
        assert_eq!(spec.rpc.reset_entry, "charset.reset");
        // 全局域：不带 schemaId。
        assert!(st.build_list_params().get("schemaId").is_none());
    }

    /// ★★ 三态各自发对的值，**主键是 key 不是显示名**。
    ///
    /// 显示名可被用户改（`name:` 字段），key 不会。发错的话 core 报「没有名为 X 的
    /// 字符类」——而用户只是点了一下菜单。
    #[test]
    fn setting_a_class_default_sends_key_and_the_right_three_state_value() {
        let st = charsets_state();
        let row = WordRow {
            code: "emoji".into(),
            text: "Emoji 表情".into(),
            ..Default::default()
        };
        for (target, expect) in [
            (Some(true), "common"),
            (Some(false), "rare"),
            (None, "inherit"),
        ] {
            let (m, p) = st.set_class_default_call(&row, target);
            assert_eq!(m, "charset.set");
            assert_eq!(p["key"], json!("emoji"), "主键必须是 key");
            assert_eq!(p["default"], json!(expect));
        }
    }

    /// `inherit` 与「设为常用」是两个不同的档，不能合并。
    ///
    /// 前者不表态、跟随自带配置更新；后者钉死。合并之后用户设过一次就再也回不到第一种。
    #[test]
    fn inherit_is_a_distinct_state_from_explicitly_common() {
        let st = charsets_state();
        let row = WordRow {
            code: "emoji".into(),
            ..Default::default()
        };
        let (_, a) = st.set_class_default_call(&row, None);
        let (_, b) = st.set_class_default_call(&row, Some(true));
        assert_ne!(a["default"], b["default"], "两个档必须发不同的值");
    }

    /// 恢复默认按 key 发，且**要提示**——撤掉的是整份调整，别的字段变回去用户看不见。
    #[test]
    fn resetting_a_class_sends_key() {
        let st = charsets_state();
        let row = WordRow {
            code: "emoji".into(),
            ..Default::default()
        };
        let (m, p, toast) = st.reset_entry_call(&row).expect("字符类该有恢复默认");
        assert_eq!(m, "charset.reset");
        assert_eq!(p["key"], json!("emoji"), "主键是 key，不是显示名");
        assert!(!toast.is_empty(), "撤掉的是整份调整，得提示一句");
    }

    /// 出厂类与自建类在行上分得开：右键「删除此类」只给后者。
    #[test]
    fn charset_rows_carry_the_builtin_flag() {
        let factory = WordRow::from_json(
            WordCategory::Charsets,
            &json!({ "key": "emoji", "builtin": true }),
        );
        let mine = WordRow::from_json(
            WordCategory::Charsets,
            &json!({ "key": "mine", "builtin": false }),
        );
        let absent = WordRow::from_json(WordCategory::Charsets, &json!({ "key": "x" }));
        assert!(factory.class_builtin);
        assert!(!mine.class_builtin);
        assert!(
            !absent.class_builtin,
            "缺席按自建算——宁可多给一个删除入口，core 会拒出厂类"
        );
    }

    /// ★ 三态从 json 读回来：`null` / 缺席 = 不表态，与 `false`（生僻）不是一回事。
    #[test]
    fn class_default_reads_back_as_three_states() {
        let common = WordRow::from_json(WordCategory::Charsets, &json!({"default": true}));
        let rare = WordRow::from_json(WordCategory::Charsets, &json!({"default": false}));
        let inherit = WordRow::from_json(WordCategory::Charsets, &json!({"default": null}));
        let absent = WordRow::from_json(WordCategory::Charsets, &json!({}));
        assert_eq!(common.class_default, Some(true));
        assert_eq!(rare.class_default, Some(false));
        assert_eq!(inherit.class_default, None, "null = 不表态");
        assert_eq!(absent.class_default, None, "缺席同样是不表态");
    }

    /// ★★ 优先级：读得回、缺席不装成 0、并且**按数值**排序。
    ///
    /// ⚠️ 数值排序是这一列的存在前提：自带取值是 10 / 20 / 100 / 900，按字符串序
    /// 「100」会排在「20」前面——一列专门用来看先后的数字，排出来的先后却是错的。
    /// 样本特意取 20 / 100 / 900：10 / 100 / 900 两种序恰好同结果，测不出东西。
    #[test]
    fn class_order_reads_back_and_sorts_numerically() {
        let r = WordRow::from_json(
            WordCategory::Charsets,
            &json!({"key": "emoji", "order": 20}),
        );
        assert_eq!(r.class_order, Some(20));
        assert_eq!(r.cell_text(WordField::ClassOrder), "20");
        let absent = WordRow::from_json(WordCategory::Charsets, &json!({"key": "x"}));
        assert_eq!(absent.class_order, None);
        assert_eq!(
            absent.cell_text(WordField::ClassOrder),
            "—",
            "缺席给占位符：0 是合法且最高的优先级，装成它会让人以为这个类压过所有出厂类"
        );

        let st = charsets_state();
        // 「优先」列的下标，顺带钉住列序（类别 0 / 优先 1 / 默认判定 2 / 范围 3 / 覆盖 4）。
        const COL_ORDER: usize = 1;
        assert_eq!(
            st.current_spec().columns[COL_ORDER].field,
            WordField::ClassOrder
        );
        st.apply_rows(&json!([
            {"key": "blk", "name": "ASCII", "order": 900},
            {"key": "mine", "name": "我的", "order": 100},
            {"key": "emoji", "name": "Emoji 表情", "order": 20},
        ]));
        st.sort.set(Some(SortKey::asc(COL_ORDER)));
        st.on_sort_changed();
        let cells = st.table_rows.get();
        assert_eq!(
            vec![
                &cells[0][COL_ORDER],
                &cells[1][COL_ORDER],
                &cells[2][COL_ORDER]
            ],
            ["20", "100", "900"],
            "按数值排；字符串序会给出 100 < 20 < 900"
        );
    }

    /// 列表行把 key 与显示名分开存：写回用 key，展示用 name。
    #[test]
    fn class_rows_keep_key_and_display_name_apart() {
        let r = WordRow::from_json(
            WordCategory::Charsets,
            &json!({"key": "emoji", "name": "Emoji 表情", "overrideCount": 1204}),
        );
        assert_eq!(r.code, "emoji");
        assert_eq!(r.text, "Emoji 表情");
        assert_eq!(r.class_override_count, 1204);
        // ★ 压制条数要显示成「N 条」而不是裸数字——它是异常提示，不是统计值。
        assert_eq!(r.cell_text(WordField::ClassOverrides), "1204 条");
        // 0 条不显示 0：恒显示数字会让人以为它总该有值。
        let clean = WordRow::from_json(WordCategory::Charsets, &json!({"key": "x"}));
        assert_eq!(clean.cell_text(WordField::ClassOverrides), "—");
    }

    /// 范围摘要两支都报：纯字表类（emoji）只有成员数，纯段类只有码位段。
    #[test]
    fn class_scope_text_reports_both_ranges_and_members() {
        let table = WordRow::from_json(WordCategory::Charsets, &json!({"memberCount": 1427}));
        assert_eq!(table.class_scope, "1427 字");

        let seg = WordRow::from_json(
            WordCategory::Charsets,
            &json!({"ranges": ["U+2600-U+26FF"]}),
        );
        assert_eq!(seg.class_scope, "U+2600-U+26FF");

        let many = WordRow::from_json(
            WordCategory::Charsets,
            &json!({"ranges": ["U+1", "U+2", "U+3"], "memberCount": 5}),
        );
        assert_eq!(many.class_scope, "U+1 U+2 等 3 段 · 5 字");

        let none = WordRow::from_json(WordCategory::Charsets, &json!({}));
        assert_eq!(none.class_scope, "—", "什么都没有时给占位符，不留空白");
    }

    // ───────── 常用字调整（全局域）─────────

    /// ★ 导入导出按钮在「字符类」tab 下必须禁用。
    ///
    /// 这两道判据按**域**走、不看当前 tab 的 spec——`SPEC_CHARSETS` 里没声明
    /// import/export，按钮却照样亮着。点下去会拿常用字的导入流处理字符类，
    /// 而用户看到的是一个与他所做之事毫无关系的结果。
    #[test]
    fn io_buttons_are_disabled_on_the_charsets_tab() {
        let st = charsets_state();
        assert_eq!(st.current_category(), WordCategory::Charsets);
        assert!(
            st.io_entry_visible(),
            "按钮该在（同短语域，灰着而不是消失）"
        );
        assert!(!st.io_entry_enabled(), "字符类没有导入导出流，必须禁用");

        // 切到「常用字」tab 就该可用——那张表确实有 commonChars.* 的导入导出。
        st.sub_tab.set(SUB_TAB_COMMON_CHARS);
        assert_eq!(st.current_category(), WordCategory::CommonChars);
        assert!(st.io_entry_enabled());
    }

    /// 「字符集分类」域下两个 tab 的下标。
    ///
    /// ⚠️ 写死数字的测试不会报错，只会静默测到另一个类别上去——两个类别的 spec 都有
    /// list/update，断言会以「参数不对」的形态失败，看不出根因是选错了 tab。顺序本身
    /// 改过一次（曾是「类在前」），常量化就是为了下次再改时只动这里。
    const SUB_TAB_COMMON_CHARS: usize = 0;
    const SUB_TAB_CHARSETS: usize = 1;

    /// 常用字是**全局**域：没有 schemaId，参数里也不该出现它。
    ///
    /// 带上 schemaId 的后果不是报错而是「core 侧忽略掉、但列表看起来正常」——
    /// 那张表的键就是一个字，方案维度从头到尾都不存在。
    #[test]
    fn common_domain_is_global_and_sends_no_schema_id() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(DOMAIN_COMMON);
        st.sub_tab.set(SUB_TAB_COMMON_CHARS);
        assert_eq!(st.domain_kind(), DomainKind::CommonChars);
        assert_eq!(st.current_category(), WordCategory::CommonChars);
        assert_eq!(st.current_schema_id(), None, "全局域没有方案");
        assert_eq!(st.domain_label_at(DOMAIN_COMMON), "字符集分类");
        assert_eq!(
            st.tabs_for_domain(),
            vec![LogicalTab::CommonChars, LogicalTab::Charsets]
        );
        let p = st.build_list_params();
        assert!(p.get("schemaId").is_none(), "全局域不该带 schemaId: {p:?}");
        // 导入导出入口可用（走 commonChars.* 全局流，不依赖有没有方案）。
        // ⚠️ 光有 `spec.ops.import/export` 不够：按钮的显隐/可用是另外两道
        //（`io_entry_visible`/`io_entry_enabled`），漏了它们的表现是「能力声明写着有，
        // 按钮却根本不出现」——spec 的测试全绿也照样看不见。
        assert!(spec_for(WordCategory::CommonChars).ops.import);
        assert!(spec_for(WordCategory::CommonChars).ops.export);
        assert!(st.io_entry_visible() && st.io_entry_enabled());
    }

    /// 导入摘要必须交代「与默认相同」和「已跳过」两项。
    ///
    /// 两台机器的默认字表可能是不同版本，文件里 3 个字到本机可能只有 1 个需要记录。
    /// 只报 imported 的话，用户看到「已导入 1 条」会以为另外两个字丢了。
    #[test]
    fn common_chars_import_summary_reports_skipped_and_same_as_default() {
        let s = DictManagerState::common_chars_import_summary(&json!({
            "imported": 1,
            "sameAsDefault": 2,
            "skipped": ["「、」不受常用字表管辖，已跳过"],
        }));
        assert!(s.contains('1') && s.contains('2'), "{s}");
        assert!(s.contains("默认"), "必须说明那两个字去哪了: {s}");
        assert!(s.contains("跳过"), "{s}");

        // 没有这两项时不啰嗦。
        let clean =
            DictManagerState::common_chars_import_summary(&json!({ "imported": 3, "skipped": [] }));
        assert!(
            !clean.contains("默认") && !clean.contains("跳过"),
            "{clean}"
        );
    }

    /// 保存产出 `{char, common}`，**新增与编辑同一个 RPC**。
    ///
    /// 也钉住「不走权重那条路」：这一类没有权重维度，`edit_weight` 从来不被设置，
    /// 若分流写在权重解析之后，用户会收到「权重需为整数」——一个与他所做之事毫无
    /// 关系的报错（快捷输入正是为此在 parse 之前分流的）。
    #[test]
    fn common_save_calls_emit_char_and_direction() {
        let st = DictManagerState::new();
        st.domain.set(DOMAIN_COMMON);
        st.sub_tab.set(SUB_TAB_COMMON_CHARS);

        st.open_common_add();
        st.edit_text.set("槮".into());
        st.common_choice.set(0); // 常用
        // ★ 关键装置：把 `edit_weight` 置成**解析不了**的值。
        //
        // 这一类没有权重维度，`edit_weight` 从来不被本对话框设置，但它是全页共用的信号
        // ——用户在别的类别里把权重框删空过，切过来时它就是空串。分流若写在权重解析
        // 之后，这里会得到「权重需为整数」而不是保存成功。
        //
        // ⚠️ 不置这一下的话本用例是**假绿**：初值恰好能 parse，把分流挪到 parse 之后
        // 测试照样通过（实测确认过）。
        st.edit_weight.set(String::new());
        let (calls, msg) = st.save_calls().expect("应能保存");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "commonChars.set");
        assert_eq!(calls[0].1, json!({ "char": "槮", "common": true }));
        assert_eq!(msg, "已添加");
    }

    /// 列表里的**一键切换**：不经对话框，直接发 `commonChars.set` 到反方向。
    ///
    /// 键名是 `char` 不是 `text`——这张表没有编码维度，主键就是那个字。
    #[test]
    fn common_toggle_sends_opposite_direction() {
        let st = DictManagerState::new();
        st.domain.set(DOMAIN_COMMON);
        st.sub_tab.set(SUB_TAB_COMMON_CHARS);

        let common_row = WordRow {
            text: "的".into(),
            common_now: true,
            common_base: true,
            ..Default::default()
        };
        let (method, params) = st.toggle_common_call(&common_row);
        assert_eq!(method, "commonChars.set");
        assert_eq!(
            params,
            json!({ "char": "的", "common": false }),
            "当前判常用 ⇒ 点一下切到生僻"
        );

        // 反方向：现在是生僻的行，点一下切回常用。
        let rare_row = WordRow {
            common_now: false,
            ..common_row.clone()
        };
        assert_eq!(
            st.toggle_common_call(&rare_row).1,
            json!({ "char": "的", "common": true })
        );
    }

    /// 「只看已修改」要真的发到 core（列表是全表，靠客户端过滤当页那几十条没有意义）。
    #[test]
    fn common_only_modified_goes_to_server() {
        let st = DictManagerState::new();
        st.domain.set(DOMAIN_COMMON);
        st.sub_tab.set(SUB_TAB_COMMON_CHARS);
        assert!(
            st.build_list_params().get("onlyModified").is_none(),
            "默认不发这个参数"
        );

        st.only_modified.set(true);
        assert_eq!(
            st.build_list_params().get("onlyModified"),
            Some(&json!(true))
        );

        // 别的类别没有「改过没有」这个维度，不该带上它。
        st.domain.set(DOMAIN_PHRASE);
        st.sub_tab.set(0);
        assert!(st.build_list_params().get("onlyModified").is_none());
    }

    /// 非单字一律拒绝，且报错要说得出所以然（不能是「权重需为整数」那种驴唇不对马嘴的）。
    #[test]
    fn common_save_calls_reject_non_single_char() {
        let st = DictManagerState::new();
        st.domain.set(DOMAIN_COMMON);
        st.sub_tab.set(SUB_TAB_COMMON_CHARS);
        st.open_common_add();
        // 同 `common_save_calls_emit_char_and_direction`：空权重是这条分流的试金石，
        // 报错必须是「填一个字」而不是「权重需为整数」。
        st.edit_weight.set(String::new());

        st.edit_text.set("  ".into());
        assert!(
            st.save_calls().unwrap_err().contains("一个字"),
            "空白仍要本地拦"
        );

        // ⚠️ **词组不在这里拦**：本地曾按 `chars().count() == 1` 判，于是 `⚽️`(2 个码位)、
        // `👨‍👩‍👧`(5 个) 这类「屏幕上一个图形」被前端直接拒了，而 core 明明收得下。
        // 「一个字」是 Unicode 字素簇的事，判据留在 core 一处（`single_markable_char`），
        // 前端只管把原文发过去、把 core 的报错原样显示。
        st.edit_text.set("我们".into());
        let (calls, _) = st.save_calls().expect("非空文本一律交给 core 判");
        assert_eq!(calls[0].0, "commonChars.set");
        assert_eq!(
            calls[0].1["char"],
            json!("我们"),
            "原文照发，不在本地截取首字"
        );
    }

    /// 「恢复默认」只对**改过的行**放行。
    ///
    /// 列表是全表（8104 条），绝大多数行从没被碰过；给它们亮着「恢复此条默认」，
    /// 点下去什么都不会发生——一个永远无效的按钮比没有这个按钮更让人困惑。
    #[test]
    fn common_reset_entry_only_for_adjusted_rows() {
        use crate::pages::dict::spec::row_ops;
        let untouched = WordRow {
            text: "一".into(),
            common_now: true,
            common_base: true,
            adjusted: false,
            ..Default::default()
        };
        let changed = WordRow {
            adjusted: true,
            ..untouched.clone()
        };
        assert!(
            !row_ops(WordCategory::CommonChars, &untouched).reset_entry,
            "没改过的行不该给「恢复此条默认」"
        );
        assert!(row_ops(WordCategory::CommonChars, &changed).reset_entry);
        // 编辑对两种行都开放：全表里任何一个字都可以改判定。
        assert!(row_ops(WordCategory::CommonChars, &untouched).edit);
    }

    /// 全表列表是**分页 + 服务端搜索**：8104 条一次全取约 400KB，走不得客户端过滤。
    #[test]
    fn common_list_is_paged_and_server_searched() {
        let st = DictManagerState::new();
        st.domain.set(DOMAIN_COMMON);
        st.sub_tab.set(SUB_TAB_COMMON_CHARS);
        let spec = st.current_spec();
        assert!(spec.paged && spec.searchable && !spec.client_search);
        assert_eq!(spec.rpc.list, "commonChars.list");
        assert_eq!(spec.rpc.reset_entry, "commonChars.reset");
        assert_eq!(spec.rpc.reset_default, "commonChars.clear");

        // 分页参数要发出去，且**不带 schemaId**（全局域）。
        st.search.set("槮".into());
        let p = st.build_list_params();
        assert!(p.get("schemaId").is_none(), "全局域不该带 schemaId: {p:?}");
        assert_eq!(p.get("prefix").and_then(|v| v.as_str()), Some("槮"));
        assert!(p.get("offset").is_some() && p.get("limit").is_some());
    }

    /// 点列头要真的把 `sortBy` 发出去。
    ///
    /// 服务端分页的类别，排序也必须在服务端做——只排当页那几十条毫无意义。发不出去的
    /// 表现是「点了列头没反应」，而列头照旧画着可排序的样子，也不报错。
    #[test]
    fn common_list_sends_sort_key_of_clicked_column() {
        let st = DictManagerState::new();
        st.domain.set(DOMAIN_COMMON);
        st.sub_tab.set(SUB_TAB_COMMON_CHARS);

        // 第 0 列「字」——它复用 WordField::Text，故发出去的键是 "text"
        // （core 侧两个键都认，见 `sort_common_char_rows`）。
        st.sort.set(Some(SortKey {
            column: 0,
            order: SortOrder::Asc,
        }));
        let p = st.build_list_params();
        assert_eq!(p.get("sortBy").and_then(|v| v.as_str()), Some("text"));
        assert_eq!(p.get("sortOrder").and_then(|v| v.as_str()), Some("asc"));

        // 第 2 列「当前」→ common，降序。
        st.sort.set(Some(SortKey {
            column: 2,
            order: SortOrder::Desc,
        }));
        let p = st.build_list_params();
        assert_eq!(p.get("sortBy").and_then(|v| v.as_str()), Some("common"));
        assert_eq!(p.get("sortOrder").and_then(|v| v.as_str()), Some("desc"));
    }

    /// `adjusted` 要从 list 的 json 读出来——漏读的话全表每一行都是「没改过」，
    /// 「恢复此条默认」在所有行上永久灰着，且不报错。
    #[test]
    fn common_from_json_reads_adjusted() {
        let row = WordRow::from_json(
            WordCategory::CommonChars,
            &json!({ "char": "的", "common": false, "baseCommon": true, "adjusted": true }),
        );
        assert_eq!(row.text, "的");
        assert!(!row.common_now && row.common_base && row.adjusted);
    }

    /// ★★ 调序基准是**候选内下标**（`move_index`），不是列表行号（`position`）。
    ///
    /// 夹具刻意让两者不同（行号 5、候选内第 1 位）：拿行号算目标位置会得到 4/6，
    /// 而正确答案是 0/2。停用项占着行号却不在候选里，这个差值就是「上移一位跳过
    /// 好几条」的成因——界面上极难看出，故在这里钉死。
    #[test]
    fn quick_move_target_uses_candidate_index_not_row_number() {
        let row = WordRow {
            code: "date.lunar".into(),
            kind: "date".into(),
            position: 5,         // 列表行号（displayPos）
            move_index: Some(1), // 候选内下标
            enabled: true,
            ..Default::default()
        };
        assert_eq!(DictManagerState::quick_move_target(&row, -1), Some(0));
        assert_eq!(DictManagerState::quick_move_target(&row, 1), Some(2));
    }

    /// 两种「动不了」：已在首位（上移无意义）与已停用（不在候选里，得先启用）。
    #[test]
    fn quick_move_target_rejects_first_row_and_disabled() {
        let first = WordRow {
            move_index: Some(0),
            enabled: true,
            ..Default::default()
        };
        assert_eq!(
            DictManagerState::quick_move_target(&first, -1),
            None,
            "首位不能再上移"
        );
        assert_eq!(
            DictManagerState::quick_move_target(&first, 1),
            Some(1),
            "但能下移"
        );

        let disabled = WordRow {
            position: 9,      // 有行号
            move_index: None, // 却不在候选里
            enabled: false,
            ..Default::default()
        };
        assert_eq!(DictManagerState::quick_move_target(&disabled, -1), None);
        assert_eq!(
            DictManagerState::quick_move_target(&disabled, 1),
            None,
            "停用项两个方向都动不了，且不能拿行号 9 兜底"
        );
    }

    /// 写操作的主键是 `(kind, id)`，不是 `(code, text)`。
    ///
    /// 格式 id 只在**同类内**唯一（高级用户手写 system.quick.toml 时不强制带 kind 前缀），
    /// 漏了 kind 就可能改到另一类的同名条目上。
    #[test]
    fn quick_write_keys_on_kind_and_id() {
        let st = DictManagerState::new();
        st.domain.set(DOMAIN_QUICK);
        st.sub_tab.set(0);
        let row = WordRow {
            code: "date.lunar".into(),
            text: "农历$LMD".into(),
            kind: "date".into(),
            move_index: Some(2),
            ..Default::default()
        };
        let (method, params) = st.quick_call("quick.move", &row, &[("index", json!(1))]);
        assert_eq!(method, "quick.move");
        assert_eq!(params["kind"], json!("date"));
        assert_eq!(params["id"], json!("date.lunar"));
        assert_eq!(params["index"], json!(1));
        // 模板是可变的展示内容，绝不能当主键
        assert!(params.get("text").is_none(), "模板不该作为主键的一部分");
        assert!(params.get("code").is_none(), "快捷输入用 id 而非 code");
    }

    /// 「恢复全部默认」的类别集取自**数据**，不写死那五个——core 新增一类时自动跟上。
    #[test]
    fn quick_kinds_come_from_data_not_hardcoded() {
        let st = DictManagerState::new();
        st.domain.set(DOMAIN_QUICK);
        st.sub_tab.set(0);
        *st.rows.borrow_mut() = vec![
            WordRow {
                kind: "date".into(),
                ..Default::default()
            },
            WordRow {
                kind: "date".into(),
                ..Default::default()
            },
            WordRow {
                kind: "number".into(),
                ..Default::default()
            },
            // 假想的将来新增类别：不改代码也应被收进来
            WordRow {
                kind: "weekday".into(),
                ..Default::default()
            },
        ];
        assert_eq!(
            st.quick_kinds_present(),
            vec![
                "date".to_string(),
                "number".to_string(),
                "weekday".to_string()
            ],
            "保序去重，且不写死类别表"
        );
    }

    /// 当前域为「快捷输入」、列表里有两个类别的 state。
    ///
    /// `quick_save_calls` 读 `current_spec()` 拿 RPC 名，故域必须切过去；类别选项与变量
    /// 提示都从 `rows` 现取，故 rows 也要有内容。
    fn quick_state_fixture() -> DictManagerState {
        let st = DictManagerState::new();
        st.domain.set(DOMAIN_QUICK);
        *st.rows.borrow_mut() = vec![
            WordRow {
                kind: "date".into(),
                code: "date.cn".into(),
                ..Default::default()
            },
            WordRow {
                kind: "month_day".into(),
                code: "month_day.cn".into(),
                ..Default::default()
            },
            WordRow {
                kind: "number".into(),
                code: "number.amount".into(),
                ..Default::default()
            },
        ];
        st
    }

    /// 快捷输入的保存：新增发 `quick.add`（带类别）、编辑发 `quick.setText`（带 kind+id）。
    ///
    /// ★ 编辑时**不发类别**：类别决定可用变量与出厂分组，换类别本就是另一条格式。
    #[test]
    fn quick_save_calls_split_add_and_update() {
        let st = quick_state_fixture();

        // 新增：类别取自选择器
        let (calls, msg) = st
            .quick_save_calls("number", "共$THOU元", None)
            .expect("合法输入");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "quick.add");
        assert_eq!(calls[0].1["kind"], json!("number"));
        assert_eq!(calls[0].1["text"], json!("共$THOU元"));
        assert!(calls[0].1.get("id").is_none(), "新增没有 id，由 core 分配");
        assert_eq!(msg, "已添加");

        // 编辑：主键 (kind, id)，类别取自 orig 而非选择器
        let orig = Some(("date".to_string(), "date.u1".to_string()));
        let (calls, msg) = st
            .quick_save_calls("number", "$Y/$M/$D", orig)
            .expect("合法输入");
        assert_eq!(calls[0].0, "quick.setText");
        assert_eq!(
            calls[0].1["kind"],
            json!("date"),
            "★ 编辑用原类别，不受选择器影响"
        );
        assert_eq!(calls[0].1["id"], json!("date.u1"));
        assert_eq!(calls[0].1["text"], json!("$Y/$M/$D"));
        assert_eq!(msg, "已保存");
    }

    /// 空模板与空类别在**发请求之前**被拦下，并给出说明。
    ///
    /// 纯空白也算空：它会渲染出一条「看起来是空的」候选，选中则上屏几个空格。
    #[test]
    fn quick_save_calls_rejects_empty_input() {
        let st = quick_state_fixture();
        assert!(
            st.quick_save_calls("date", "   ", None)
                .unwrap_err()
                .contains("模板")
        );
        assert!(
            st.quick_save_calls("", "$Y", None)
                .unwrap_err()
                .contains("类别")
        );
        // 编辑时类别来自 orig，故不校验选择器那个值
        let orig = Some(("date".to_string(), "date.u1".to_string()));
        assert!(st.quick_save_calls("", "$Y", orig).is_ok());
    }

    /// ★ 变量提示：变量集**相同**的类别合并成一节，不同的各占一节。
    ///
    /// core 里 date 与 month_day 的变量完全一样（17 条），不合并就要把同样的十七行出两遍。
    ///
    /// 直接注入缓存而不是走 mock：mock 要靠进程级环境变量 `WIND_RPC_MOCK`，测试并发跑时
    /// 互相干扰；而注入还能造出「两类变量集相同」这个真实场景（mock 数据里没有）。
    #[test]
    fn quick_var_lines_merge_kinds_sharing_one_var_set() {
        let st = quick_state_fixture();
        let date_vars = vec![
            ("Y".to_string(), "年".to_string()),
            ("LMD".to_string(), "农历月日".to_string()),
        ];
        *st.quick_vars.borrow_mut() = Some(vec![
            ("date".to_string(), date_vars.clone()),
            // 与 date 逐条相同 → 应与它合并成一节
            ("month_day".to_string(), date_vars.clone()),
            (
                "number".to_string(),
                vec![("AMT".to_string(), "金额大写".to_string())],
            ),
        ]);

        let lines = st.build_quick_var_lines();
        let headers: Vec<&str> = lines
            .iter()
            .filter(|(_, r)| r.is_empty())
            .map(|(l, _)| l.as_str())
            .collect();
        assert_eq!(
            headers,
            vec!["日期 / 月日", "数字"],
            "★ 变量集相同的两类合并成一节标题"
        );
        // 合并后 date 的两条变量只出现一次
        let y_count = lines.iter().filter(|(l, _)| l == "$Y").count();
        assert_eq!(y_count, 1, "$Y 不该出现两遍");
        // 变量行的左列带 $ 前缀（照着抄就能用）
        assert!(
            lines
                .iter()
                .filter(|(_, r)| !r.is_empty())
                .all(|(l, _)| l.starts_with('$')),
            "变量名要带 $: {lines:?}"
        );
    }

    /// ★ 变量提示放进可选中的输入框是为了**能复制**（用户要把 `$LMD` 照抄进模板）。
    ///
    /// 断言的是这段文本本身——它就是用户选中后复制走的内容，验的不是排版，
    /// 而是「复制出来的东西对不对」这件用户真正在乎的事。
    #[test]
    fn quick_var_text_carries_copyable_variable_names() {
        let st = quick_state_fixture();
        *st.quick_vars.borrow_mut() = Some(vec![
            (
                "date".to_string(),
                vec![
                    ("Y".to_string(), "年，如 2026".to_string()),
                    ("LMD".to_string(), "农历月日，四月廿九".to_string()),
                ],
            ),
            (
                "number".to_string(),
                vec![("AMT".to_string(), "金额大写".to_string())],
            ),
        ]);
        let text = st.build_quick_var_text();
        assert!(text.contains("$Y"), "变量名要能复制到: {text}");
        assert!(text.contains("$LMD"), "农历变量同理: {text}");
        assert!(text.contains("农历月日，四月廿九"), "说明也要在");
        assert!(text.contains("日期"), "节标题要在");
        assert!(text.contains("不可混用"), "末尾那句用法说明也一并可复制");
    }

    /// 拿不到变量提示时文本仍可构造（只剩末尾那句说明），不 panic——core 没连上时
    /// 对话框要照常能用。
    ///
    /// ⚠️ 「拿不到」必须**显式注入** `Some(vec![])`，不能靠「测试环境连不上 core」：
    /// 开发机上往往正跑着输入法，RPC 会真的连上命名管道取回真实变量，本条随即变红。
    /// 曾经就是这么红的——CI 上没有 core 所以一直绿，本地一装输入法就失败。
    #[test]
    fn quick_var_text_survives_missing_data() {
        let st = quick_state_fixture();
        *st.quick_vars.borrow_mut() = Some(Vec::new());
        let text = st.build_quick_var_text();
        // 断言具体变量名而不是裸 `$`：末尾那句说明本身就含「$变量」三个字。
        assert!(!text.contains("$Y"), "没有变量数据时不该凭空出现变量名");
        assert!(!text.contains("$AMT"));
        assert!(text.contains("不可混用"), "静态的用法说明仍在");
    }

    /// 拿不到变量提示时返回空、不 panic——core 没连上时对话框仍要能用
    /// （模板非法会在保存期由 core 拒绝，提示只是便利）。
    ///
    /// 同 `quick_var_text_survives_missing_data`：注入空缓存表达「取过、core 没给数据」，
    /// 不依赖开发机上恰好没运行输入法。
    #[test]
    fn quick_var_lines_tolerate_missing_data() {
        let st = quick_state_fixture();
        *st.quick_vars.borrow_mut() = Some(Vec::new());
        assert!(st.build_quick_var_lines().is_empty());
    }

    /// 变量提示只拉一轮：类别有几个就发几轮 RPC 的话，core 断开时开一次对话框要连着
    /// 撞好几次超时。`None`=没取过、`Some`=取过，正是为了让这件事可判定。
    #[test]
    fn quick_vars_fetch_is_skipped_once_loaded() {
        let st = quick_state_fixture();
        assert!(st.quick_vars.borrow().is_none(), "初始应是「没取过」");
        // 注入 = 已取过：ensure 必须直接返回，不碰 RPC（本机可能正跑着 core，
        // 真发了 RPC 就会拿回真实数据，下面的空断言随即失败）。
        *st.quick_vars.borrow_mut() = Some(Vec::new());
        st.ensure_quick_vars();
        assert_eq!(
            st.quick_vars.borrow().as_deref(),
            Some(&[][..]),
            "已取过就不该再拉取覆盖"
        );
    }

    /// 类别选项从数据现取：同一个 `quick_kinds_present`，故不会与列表里的类别脱节。
    #[test]
    fn quick_kind_options_follow_the_data() {
        let st = quick_state_fixture();
        assert_eq!(st.quick_kind_options(), st.quick_kinds_present());
        // 下标 → kind；越界回落首项而不是 panic 或空串
        st.quick_kind_idx.set(999);
        assert_eq!(
            st.quick_selected_kind(),
            st.quick_kind_options()[0],
            "越界回落首项"
        );
    }

    /// 导入摘要必须**如实报出**跳过项与自定义条目数。
    ///
    /// 用户看到「导入成功」却少了规则，无从判断是文件坏了还是程序吃了。
    ///
    /// ⚠️ 字段名是 `formats`（实际写入数）。P1 时叫 `ignoredFormats`（当时没有存储落点，
    /// 如实报「已忽略」）——core 侧改名时这里必须跟着改，否则摘要会永远说「暂不支持」。
    #[test]
    fn quick_import_summary_never_hides_skipped_items() {
        let clean = json!({ "moved": 3, "disabled": 1, "formats": 0, "skipped": [] });
        let s = DictManagerState::quick_import_summary(&clean);
        assert!(s.contains('3') && s.contains('1'), "{s}");
        assert!(!s.contains("跳过"), "干净的导入不该提跳过: {s}");
        assert!(!s.contains("自定义"), "一条都没有时不提自定义条目: {s}");

        let dirty = json!({
            "moved": 1, "disabled": 0, "formats": 2,
            "skipped": ["条目 x：未知类别 weather", "调整段：未知类别 nope"]
        });
        let s = DictManagerState::quick_import_summary(&dirty);
        assert!(s.contains("自定义条目 2"), "自定义条目数要报出来: {s}");
        assert!(s.contains("跳过"), "跳过项要报出来: {s}");
        assert!(
            !s.contains("不支持"),
            "★ 自定义条目已能导入，摘要不该再说「暂不支持」: {s}"
        );
    }

    #[test]
    fn resolve_domain_matches_id_kind_and_folded_pinyin_variant() {
        let st = deep_link_fixture();
        assert_eq!(st.resolve_domain(""), None, "空 = 不指定");
        // 四个全局域**逐个**钉住：此前这里只有 phrase 一条，于是「加了全局域却忘了
        // 给它留关键字」不会变红——`--type` 那一侧解析得动、深链却到不了，且不报错，
        // 只 toast 一句「『快捷短语』没有『邮箱后缀』」。输入补全当初正是这么漏的。
        assert_eq!(st.resolve_domain("phrase"), Some(DOMAIN_PHRASE));
        assert_eq!(st.resolve_domain("quick"), Some(DOMAIN_QUICK));
        assert_eq!(st.resolve_domain("charsets"), Some(DOMAIN_COMMON));
        assert_eq!(st.resolve_domain("common-chars"), Some(DOMAIN_COMMON), "旧别名");
        assert_eq!(st.resolve_domain("completion"), Some(DOMAIN_COMPLETION));
        // 两个 tab 的 cli_id 也该认：单给 `--type mail-suffix` 时用户多半没再给 --schema。
        assert_eq!(st.resolve_domain("mail-suffix"), Some(DOMAIN_COMPLETION));
        assert_eq!(st.resolve_domain("url-history"), Some(DOMAIN_COMPLETION));
        // 每个全局域的 cli 关键字都必须指向一个**互不相同**的槽位。
        let globals = [
            st.resolve_domain("phrase"),
            st.resolve_domain("quick"),
            st.resolve_domain("charsets"),
            st.resolve_domain("completion"),
        ];
        assert!(globals.iter().all(|x| x.is_some()), "每个全局域都要有 cli 关键字");
        let mut slots: Vec<usize> = globals.into_iter().flatten().collect();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), GLOBAL_DOMAINS, "关键字与全局域应一一对应，不能重号");
        assert_eq!(st.resolve_domain("wb"), Some(GLOBAL_DOMAINS), "精确方案 id");
        assert_eq!(st.resolve_domain("quanpin"), Some(GLOBAL_DOMAINS + 1));
        assert_eq!(
            st.resolve_domain("codetable"),
            Some(GLOBAL_DOMAINS),
            "引擎族名"
        );
        assert_eq!(st.resolve_domain("pinyin"), Some(GLOBAL_DOMAINS + 1));
        assert_eq!(
            st.resolve_domain("ziranma"),
            Some(GLOBAL_DOMAINS + 1),
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

        st.domain.set(GLOBAL_DOMAINS + 1); // 用户切到拼音域
        st.on_nav_changed();
        assert_eq!(
            st.current_category(),
            WordCategory::UserDict,
            "新域无记忆→首项"
        );

        st.domain.set(GLOBAL_DOMAINS); // 切回五笔域
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
        // 混输域只有候选调整（tabs_for_domain 不给用户词库）。
        // 这里原本用「拼音域 + 候选调整」作样例——拼音补上该标签后那个前提就不成立了，
        // 测试会因**样例失效**而红，而不是机制坏了。换成夹具里仍然缺类型的混输域。
        let note = st
            .apply_deep_link("mixed", Some(LogicalTab::UserDict))
            .expect("落空档应给出提示");
        assert!(note.contains("用户词库"), "提示要点名用户要的类型: {note}");
        assert!(note.contains("候选调整"), "提示要说明实际落点: {note}");
        assert_eq!(st.domain.get(), GLOBAL_DOMAINS + 2, "域仍按 schema 定位");
        assert_eq!(
            st.current_category(),
            WordCategory::Shadow,
            "降级到该域首个子标签，而非报错或错位"
        );
    }

    #[test]
    fn deep_link_reports_unknown_schema_and_keeps_domain() {
        let st = deep_link_fixture();
        st.navigate_to(GLOBAL_DOMAINS, Some(0));
        let note = st.apply_deep_link("typo", None).expect("未知方案应提示");
        assert!(note.contains("typo"), "提示要带上用户输入的原值: {note}");
        assert_eq!(
            st.domain.get(),
            GLOBAL_DOMAINS,
            "未找到方案时保持当前域，不乱跳"
        );
    }

    #[test]
    fn deep_link_dimensions_are_independently_optional() {
        let st = deep_link_fixture();
        st.navigate_to(GLOBAL_DOMAINS, Some(0));

        // 只给类型：不改域，只换子标签
        assert_eq!(st.apply_deep_link("", Some(LogicalTab::Freq)), None);
        assert_eq!(st.domain.get(), GLOBAL_DOMAINS, "省 --schema 时保持当前域");
        assert_eq!(st.current_category(), WordCategory::Freq);

        // 只给方案：切域，子标签沿用该域记忆（新域无记忆则落首项）
        assert_eq!(st.apply_deep_link("pinyin", None), None);
        assert_eq!(st.domain.get(), GLOBAL_DOMAINS + 1);
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
        st.domain.set(GLOBAL_DOMAINS);
        st.sub_tab.set(0);
        assert_eq!(st.current_category(), WordCategory::UserDict);
        assert_eq!(st.current_schema_id().as_deref(), Some("wb"));

        // 子标签 3 = 候选调整
        st.sub_tab.set(3);
        assert_eq!(st.current_category(), WordCategory::Shadow);

        // domain 2 = 第二个方案
        st.domain.set(GLOBAL_DOMAINS + 1);
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
        // 前两项是全局域（不按方案），方案域从 GLOBAL_DOMAINS 起。
        assert_eq!(labels[DOMAIN_PHRASE], "快捷短语");
        assert_eq!(labels[DOMAIN_QUICK], "快捷输入");
        assert_eq!(labels[GLOBAL_DOMAINS], "五笔86 · 码表");
        // 拼音方案折叠后统一显示"拼音"（不再拼接各方案的 name · 拼音）
        assert_eq!(labels[GLOBAL_DOMAINS + 1], "拼音");
    }

    #[test]
    fn build_params_userdict_has_schema_and_paging() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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
        // 样例用系统短语：临时词库/候选调整改走服务端分页后，非分页类别只剩短语系统与
        // 快捷输入两类，拿分页类别当样例会因 parse_list 取不到 items 而空表。
        let st = DictManagerState::new(); // domain 0 = 短语，sub_tab 0 = 系统短语
        st.apply_rows(&json!([
            { "code": "b", "text": "乙", "weight": 2, "isSystem": true },
            { "code": "a", "text": "甲", "weight": 30, "isSystem": true },
            { "code": "c", "text": "丙", "weight": 4, "isSystem": true },
        ]));
        // 无排序：保持原序。
        assert_eq!(st.table_rows.get()[0][0], "b");
        // 按编码列（0）升序。
        st.sort.set(Some(SortKey::asc(0)));
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
        // 按权重列（2）降序：数值序 30 > 4 > 2（字符串序会是 4 > 30 > 2）。
        st.sort.set(Some(SortKey::desc(2)));
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
        st.domain.set(GLOBAL_DOMAINS);
        st.sub_tab.set(0); // 用户词库（分页）
        st.sort.set(Some(SortKey::desc(2))); // 权重列
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
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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

    /// 拼音词条准入契约的三档在预览正文里的呈现，以及**没有这类词时一个字都不多说**。
    ///
    /// ★ 后半条是要点：码表词库与本就带分隔的拼音词库都走这一支，给它们凭空加一段
    /// 「音节分隔」的说明，只会让绝大多数导入变得更难读。
    #[test]
    fn import_preview_shows_boundary_tiers_only_when_present() {
        let v = json!({ "format": "rime", "compatible": true, "sections": [
            { "key": "userWords", "count": 10, "skipped": 2,
              "rejected": 2, "rejectedSamples": ["wgkq 工", "aaaa 田"],
              "boundaryFilled": 5, "boundaryAmbiguous": 1, "noBoundary": 0 },
        ] });
        let msg = DictManagerState::format_import_preview(&v, PINYIN_TYPES, "拼音");
        assert!(msg.contains("5 条缺少音节分隔"), "可补档要报数：{msg}");
        assert!(
            msg.contains("2 条无法解析为拼音音节"),
            "非法档要报数：{msg}"
        );
        assert!(
            msg.contains("wgkq 工"),
            "★ 非法档必须列样例，否则用户无从判断是不是选错了文件：{msg}"
        );
        assert_eq!(DictManagerState::boundary_counts(&v), (5, 2));

        // 无这两档 ⇒ 不多说一个字
        let plain = json!({ "format": "rime", "compatible": true, "sections": [
            { "key": "userWords", "count": 3, "skipped": 0 },
        ] });
        let msg = DictManagerState::format_import_preview(&plain, PINYIN_TYPES, "拼音");
        assert!(
            !msg.contains("音节分隔"),
            "没有可补齐的词就不该提这件事：{msg}"
        );
        assert_eq!(DictManagerState::boundary_counts(&plain), (0, 0));
    }

    /// 「含符号/外文」这一档要报数，且**措辞不能沾拒收的边**。
    ///
    /// 它是 issue #97 的产物：`zuo ←` 这类「拼音码 → 符号」词条从前被判非法丢弃，现在
    /// 照常入库。★ 用户看到的必须是「已导入」而不是「不会导入 / 选错文件」——那两句话
    /// 会让他去查一个根本不存在的问题。
    #[test]
    fn import_preview_reports_no_reading_as_imported_not_rejected() {
        let v = json!({ "format": "rime", "compatible": true, "sections": [
            { "key": "userWords", "count": 10, "skipped": 0,
              "boundaryFilled": 3, "boundaryNoReading": 3, "noBoundary": 0 },
        ] });
        let msg = DictManagerState::format_import_preview(&v, PINYIN_TYPES, "拼音");
        assert!(msg.contains("3 条含符号或外文"), "该档要报数：{msg}");
        assert!(
            msg.contains("已按音节数切分导入"),
            "★ 必须说清它导入了：{msg}"
        );
        assert!(
            !msg.contains("不会导入") && !msg.contains("选错了文件"),
            "★ 这一档不是拒收，措辞不得沾边：{msg}"
        );
        // 反向对照：没有这一档就一个字都不多说（同「不给码表词库凭空加音节说明」的原则）。
        let plain = json!({ "format": "rime", "compatible": true, "sections": [
            { "key": "userWords", "count": 3, "skipped": 0 },
        ] });
        let msg = DictManagerState::format_import_preview(&plain, PINYIN_TYPES, "拼音");
        assert!(!msg.contains("符号或外文"), "没有就别提：{msg}");
    }

    /// 导入载荷：**选了文件就传路径，没有文件才回落整份内容**。
    ///
    /// ★ 这条守的是 issue #101 的修法本身。传 content 时整库要穿过一帧 JSON-RPC，
    /// 几十万条就撞 `rpc::MAX_FRAME`（16MB）。回归成 content 不会有任何编译错误、
    /// 小词库也照常绿——只有大词库用户会撞上，而那时已经发版了。
    #[test]
    fn import_payload_prefers_path_over_content() {
        let st = DictManagerState::new();

        // 有路径（从文件对话框选来的）⇒ 传 path，且**不带 content**
        *st.pending_import_path.borrow_mut() = Some(r"C:\tmp\my.wdict.yaml".into());
        let mut p = json!({ "schemaId": "pinyin" });
        st.attach_import_source(&mut p, "整份词库内容……");
        assert_eq!(
            p.get("path").and_then(|x| x.as_str()),
            Some(r"C:\tmp\my.wdict.yaml")
        );
        assert!(
            p.get("content").is_none(),
            "★ 带了 path 就不能再塞 content，否则帧长照旧超限：{p}"
        );

        // 无路径（纯词列表在内存里拼的 TSV）⇒ 回落 content
        *st.pending_import_path.borrow_mut() = None;
        let mut p = json!({ "schemaId": "pinyin" });
        st.attach_import_source(&mut p, "ni\t你\t100\t0\n");
        assert_eq!(
            p.get("content").and_then(|x| x.as_str()),
            Some("ni\t你\t100\t0\n")
        );
        assert!(p.get("path").is_none(), "没有文件就不该编一个路径出来：{p}");
    }

    /// 多批导入的结果要合成一份再渲染，**所有计数字段一律累加**。
    ///
    /// ★ 不列白名单：core 侧后来新增的计数档会自动跟上。列举字段名的话，加字段那天
    /// 会静默漏掉一个数，而表现只是某个数偏小——没人看得出来。
    #[test]
    fn multi_batch_import_reports_are_summed() {
        let b1 = json!({ "sections": [
            { "key": "userWords", "added": 100, "updated": 2, "unchanged": 0, "skipped": 1,
              "boundaryNoReading": 3 },
        ] });
        let b2 = json!({ "sections": [
            { "key": "userWords", "added": 50, "updated": 1, "unchanged": 4, "skipped": 2,
              "boundaryNoReading": 1 },
        ] });
        let m = DictManagerState::merge_import_reports(&[b1.clone(), b2]);
        let sec = &m["sections"][0];
        assert_eq!(sec["added"], json!(150));
        assert_eq!(sec["updated"], json!(3));
        assert_eq!(sec["unchanged"], json!(4));
        assert_eq!(sec["skipped"], json!(3));
        assert_eq!(
            sec["boundaryNoReading"],
            json!(4),
            "★ 没在合并里列举过的字段也要累加"
        );
        assert_eq!(sec["key"], json!("userWords"), "非数值字段取首批");

        // 单批原样返回（绝大多数情况，19 万词也只有一批）
        assert_eq!(DictManagerState::merge_import_reports(&[b1.clone()]), b1);

        // toast 用合并后的数，不是最后一批的数
        let msg = DictManagerState::format_import_result(&m);
        assert!(msg.contains("150"), "toast 要报总数而不是某一批：{msg}");
    }

    /// ★★ 分批切法：按**字节**切，且每批非空。
    ///
    /// 按条数切等于用一个跟约束无关的量去逼近字节上限——行长随编码方案与词长变化很大。
    /// 「每批至少一行」则是防死循环：单行超上限时若在那里切，start 不前进。
    #[test]
    fn import_batches_split_by_bytes_and_never_empty() {
        use super::super::wordlist::import_batches;
        let rows: Vec<(String, String)> = (0..10)
            .map(|i| ("abcd".to_string(), format!("词{i}")))
            .collect();
        // 每行 = 4(code) + 6(text，3 字节/汉字 × 2) + 3(权重"100") + 3(两个\t一个\n) = 16
        let b = import_batches(&rows, 100, 32);
        assert_eq!(
            b.len(),
            5,
            "32 字节装得下 2 行 ⇒ 10 行切 5 批：{:?}",
            b.len()
        );
        assert!(b.iter().all(|x| !x.is_empty()), "不得有空批");
        assert_eq!(b.iter().map(|x| x.len()).sum::<usize>(), 10, "不得丢行");

        // 上限大到装得下全部 ⇒ 一批（19 万词的常态）
        assert_eq!(import_batches(&rows, 100, 1 << 20).len(), 1);

        // 单行就超上限 ⇒ 仍每行一批地发出去，交给 core 报错，而不是原地死循环
        let b = import_batches(&rows, 100, 1);
        assert_eq!(b.len(), 10);
        assert!(b.iter().all(|x| x.len() == 1));

        assert!(import_batches(&[], 100, 32).is_empty(), "空输入不产生空批");
    }

    /// ★★ 分批下 `replace` 只能清一次：首批替换，其余合并。
    ///
    /// 每批都 replace 的话，后一批会把前一批刚写进去的清掉——用户选了替换、导了 50 万词、
    /// 界面报成功，库里只剩最后一批。这个 bug 只在切得出第二批时出现，日常词表都只有
    /// 一批，走 RPC 的集成测试永远绿着，只能在这里锁。
    #[test]
    fn word_list_replace_only_clears_on_first_batch() {
        let p0 = DictManagerState::word_list_batch_params("pinyin", "a\t啊\t100\n", 0, "replace");
        assert_eq!(p0["strategy"], json!("replace"), "首批用用户选的策略");

        for i in 1..4 {
            let p =
                DictManagerState::word_list_batch_params("pinyin", "b\t吧\t100\n", i, "replace");
            assert_eq!(
                p["strategy"],
                json!("merge"),
                "★ 第 {i} 批必须降为 merge，否则会清掉前面已导入的"
            );
        }

        // merge 本就无副作用，各批一致
        for i in 0..3 {
            let p = DictManagerState::word_list_batch_params("pinyin", "c\t次\t100\n", i, "merge");
            assert_eq!(p["strategy"], json!("merge"));
        }

        // 其余字段每批都得齐全（漏 sections 会让 core 收到空选择、回一句成功零条）
        assert_eq!(p0["schemaId"], json!("pinyin"));
        assert_eq!(p0["sections"], json!(["userWords"]));
        assert_eq!(p0["content"], json!("a\t啊\t100\n"));
    }

    /// 导入结果 toast 要把「少掉的词」按原因分开讲。
    ///
    /// 「非拼音音节未导入」和「按你的选择跳过」对用户是两种不同的下一步：前者要去
    /// 检查是不是拿错了文件，后者是他自己刚做的决定。混成一个「跳过 N 行」讲不清。
    #[test]
    fn import_result_toast_separates_rejected_from_user_skip() {
        let v = json!({ "sections": [
            { "key": "userWords", "added": 8, "updated": 0, "unchanged": 0,
              "skipped": 3, "rejected": 2, "boundarySkipped": 1 },
        ] });
        let msg = DictManagerState::format_import_result(&v);
        assert!(msg.contains("新增 8"));
        assert!(msg.contains("2 条非拼音音节未导入"), "{msg}");
        assert!(msg.contains("1 条按你的选择跳过"), "{msg}");

        // 两个原因都为 0 ⇒ 不加括号尾巴
        let clean = json!({ "sections": [
            { "key": "userWords", "added": 1, "updated": 0, "unchanged": 0, "skipped": 0 },
        ] });
        assert!(!DictManagerState::format_import_result(&clean).contains('（'));
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
        // 用混输做样本而非拼音——拼音四段全收（含候选调整），演示不出"收不了"这一拨。
        let v = json!({ "format": "winddict", "compatible": true, "sections": [
            { "key": "userWords", "count": 2000, "skipped": 0 },
            { "key": "shadow", "count": 12, "skipped": 0 },
        ] });
        let msg = DictManagerState::format_import_preview(&v, MIXED_TYPES, "五笔拼音混输 · 混输");
        assert!(msg.contains("可导入：候选调整 12"), "实际: {msg}");
        assert!(msg.contains("不会导入：用户词库 2000"), "实际: {msg}");
    }

    /// 目标下拉的**显示序**与 `io_target` 的**语义序**必须逐项对齐。
    ///
    /// 这是加第二个全局域（快捷输入）时真实翻车的那一格：`io_target_item_list` 只
    /// `skip(1)`，把「快捷输入」留在了列表首位，于是选「五笔86」导出的是拼音、选最后
    /// 一项则 `io_target_schema_id()` 为 None 直接报「暂无已启用方案」。全程不报错、
    /// 不 panic，光看语义层测试（都是直接 `io_target.set(i)`）永远抓不到。
    #[test]
    fn io_target_dropdown_index_matches_target() {
        let st = DictManagerState::new();
        with_schemas(
            &st,
            vec![
                schema("wb", "五笔86", "codetable"),
                schema("py", "全拼", "pinyin"),
                schema("en", "英文", "english"),
            ],
        );
        let items = st.io_target_item_list();
        assert_eq!(
            items.len(),
            st.folded_domains().len(),
            "下拉项数须等于方案域数——多一项就是全局域漏跳，整列错位"
        );
        for (i, item) in items.iter().enumerate() {
            st.io_target.set(i);
            assert!(
                st.io_target_label().starts_with(&item.label),
                "第 {i} 项显示「{}」，实际目标却是「{}」",
                item.label,
                st.io_target_label()
            );
            assert!(
                st.io_target_schema_id().is_some(),
                "第 {i} 项「{}」解析不出方案 id",
                item.label
            );
        }
    }

    /// 打开对话框的默认目标 = 当前浏览的那个方案，不是它的邻居。
    #[test]
    fn io_dialog_defaults_to_the_browsed_schema() {
        let st = DictManagerState::new();
        with_schemas(
            &st,
            vec![
                schema("wb", "五笔86", "codetable"),
                schema("py", "全拼", "pinyin"),
            ],
        );
        for (i, want) in [(0usize, "wb"), (1, "py")] {
            st.domain.set(GLOBAL_DOMAINS + i);
            assert!(st.prepare_io_dialog("导出"));
            assert_eq!(
                st.io_target_schema_id().as_deref(),
                Some(want),
                "浏览第 {i} 个方案域，默认目标应是它自己"
            );
        }
    }

    /// 拼音的导入导出段必须含**候选调整**——与设置页的拼音「候选调整」子标签、以及主仓
    /// `wind-webdata::default_dict_sections` 三处同增同减（见 `types_for_kind` 的告警）。
    ///
    /// 拼音曾因下调位被整体屏蔽而只有三段；置顶放开后 tabs 与 core 都补了 Shadow，唯独
    /// 这里漏了，症状是「设置页看得见规则，导出的文件里没有」。
    #[test]
    fn pinyin_io_sections_include_shadow() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("py", "全拼", "pinyin")]);
        st.io_target.set(0);
        assert_eq!(st.io_target_kind(), DomainKind::Pinyin);
        assert!(st.io_type_applicable("shadow"), "拼音须能导入导出候选调整");
        assert!(
            st.io_selected_sections().iter().any(|k| k == "shadow"),
            "默认全选时候选调整须在导出段里，实际: {:?}",
            st.io_selected_sections()
        );
        // 与设置页子标签对齐：tabs 有 Shadow、段清单也有，两边同口径。
        assert!(
            tabs_for_kind(DomainKind::Pinyin).contains(&LogicalTab::Shadow),
            "前提被改动：拼音子标签若不再有候选调整，本条契约需一并复审"
        );
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
        st.domain.set(GLOBAL_DOMAINS + 1); // 页面停在混输域
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

    /// 选完文件走客户端解析即出结果：词数、跳过、去重都要报，且**不发 RPC**
    /// （不做出码预览换来的正是这一步的即时响应）。
    #[test]
    fn word_list_is_parsed_client_side_on_pick() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔86", "codetable")]);
        st.io_target.set(0);
        st.apply_word_list("中国\n计算机\n中国\n= 噪声 =\n");

        assert!(st.is_word_list_import());
        assert!(st.import_ready.get());
        assert_eq!(st.io_format_note.get(), "纯词列表");
        let msg = st.import_preview_msg.get();
        assert!(msg.contains("2 个词"), "实际: {msg}");
        assert!(
            msg.contains("跳过 1 行") && msg.contains("去重 1"),
            "实际: {msg}"
        );
        assert!(msg.contains("五笔86"), "要说明按哪个方案出码，实际: {msg}");
        // 纯词列表只进用户词库，其余勾选在这条路上不可用。
        assert!(st.io_type_applicable("userWords"));
        assert!(!st.io_type_applicable("freq"));
        assert!(!st.io_type_applicable("shadow"));
    }

    /// 切换目标方案时纯词列表**不需要作废任何东西**（出码还没发生），
    /// 只把文案里的方案名换掉——这正是不做出码预览省下的那套缓存逻辑。
    #[test]
    fn switching_target_only_relabels_word_list() {
        let st = DictManagerState::new();
        with_schemas(
            &st,
            vec![
                schema("wb", "五笔86", "codetable"),
                schema("py", "拼音", "pinyin"),
            ],
        );
        st.io_target.set(0);
        st.import_confirm_visible.set(true);
        st.apply_word_list("中国\n计算机\n");
        assert!(st.import_preview_msg.get().contains("五笔86"));

        st.io_target.set(1);
        st.on_io_target_changed();
        assert!(st.is_word_list_import(), "词列表应原样留着，不该被作废");
        let msg = st.import_preview_msg.get();
        assert!(
            msg.contains("拼音") && msg.contains("2 个词"),
            "实际: {msg}"
        );
    }

    /// 空码行必须剔除、并计入 unencodable —— 若混进 TSV，会被 TSV 解析器判非法
    /// 并计进它自己的 skipped，统计口径就分裂成两处，界面数字对不上。
    #[test]
    fn encoding_drops_unencodable_words() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let st = DictManagerState::new();
        // mock 出码对 BMP 外字符回空串（模拟方案未配拆字）。
        let words = vec![
            "中国".to_string(),
            "𰻞𰻞面".to_string(),
            "计算机".to_string(),
        ];
        let (rows, unencodable) = st.encode_word_batches("wb", &words).unwrap();
        assert_eq!(unencodable, 1);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1, "中国");
        assert_eq!(rows[1].1, "计算机");
        assert!(rows.iter().all(|(c, _)| !c.is_empty()));
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }

    /// 出码结果条数与送出的词数不符 = core 契约违例。宁可整体失败也不按下标硬配：
    /// 错位会把 A 的码安到 B 的词上静默入库，事后极难发现。
    #[test]
    fn zip_codes_rejects_contract_violations() {
        let words: Vec<String> = ["中国", "计算机", "人工智能"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // 少回一条 → 整体失败，不按下标硬配。
        let err = DictManagerState::zip_codes(&words, &json!({ "codes": ["a", "b"] })).unwrap_err();
        assert!(
            err.contains("条数不符") && err.contains("送 3 回 2"),
            "实际: {err}"
        );

        // 多回一条同样拒绝。
        assert!(
            DictManagerState::zip_codes(&words, &json!({ "codes": ["a", "b", "c", "d"] })).is_err()
        );

        // 缺 codes 字段。
        assert!(DictManagerState::zip_codes(&words, &json!({})).is_err());

        // 等长即通过；空码剔除但**不破坏其余词与码的对应**。
        let (rows, n) =
            DictManagerState::zip_codes(&words, &json!({ "codes": ["khgo", "", "dpdb"] })).unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            rows,
            vec![
                ("khgo".to_string(), "中国".to_string()),
                ("dpdb".to_string(), "人工智能".to_string()),
            ],
            "剔除中间的空码后，第三个词必须仍配第三个码"
        );
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
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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

    /// ★★ 多行词条框里敲的**真换行**必须在出门前投影成字面 `\n`。
    ///
    /// core 收 text 的口径是转义域（`store_text` = `unescape_text_field`）。真换行发过去
    /// 虽然也能落库（unescape 对它原样放行），但同一串里就同时混进了两个域——用户若还写了
    /// `\\`，反斜杠该还原几层便无从判定。这条锁的是「发出去的串整串只属于转义域」。
    #[test]
    fn save_calls_escapes_real_newlines_in_text() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(GLOBAL_DOMAINS);
        st.sub_tab.set(0); // 用户词库
        st.edit_code.set("aaaa".into());
        st.edit_text.set("第一行\n第二行".into());
        st.edit_weight.set("5".into());
        let (calls, _) = st.save_calls().unwrap();
        assert_eq!(calls[0].1["text"], json!(r"第一行\n第二行"));

        // 短语侧同口径——此前它靠「真换行恰好能穿过 unescape」隐式成立，现在是显式契约。
        let ph = DictManagerState::new(); // domain 0 = 短语
        ph.sub_tab.set(1); // 用户短语
        ph.edit_code.set("dz".into());
        ph.edit_text.set("上\n下".into());
        ph.edit_weight.set("1800".into());
        let (pcalls, _) = ph.save_calls().unwrap();
        assert_eq!(pcalls[0].1["text"], json!(r"上\n下"));
    }

    /// 粘贴进来的 Windows 行尾要归一，不能把裸 `\r` 带进库。
    ///
    /// 漏掉的话 core 落的是一个含不可见 `\r` 的文本，上屏时多一个诡异空白，而界面上
    /// 从头到尾看不出任何异样——最难往回追的那一类。
    #[test]
    fn save_calls_normalizes_crlf_in_text() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(GLOBAL_DOMAINS);
        st.sub_tab.set(0);
        st.edit_code.set("aaaa".into());
        st.edit_text.set("甲\r\n乙\r丙".into());
        st.edit_weight.set("5".into());
        let (calls, _) = st.save_calls().unwrap();
        assert_eq!(calls[0].1["text"], json!(r"甲\n乙\n丙"));
    }

    /// 反斜杠**不得**被这层投影碰到：`\\`（字面反斜杠）与命令短语里的路径都已是转义域
    /// 写法，再转一层的话每保存一轮翻一倍（同 `phrase_import` 分享路径的教训）。
    #[test]
    fn save_calls_leaves_backslashes_untouched() {
        let st = DictManagerState::new();
        st.sub_tab.set(1); // 用户短语
        st.edit_code.set("zd".into());
        let cmd = r#"$CC("知典", proc.run("D:\\Program Files\\知典.exe"))"#;
        st.edit_text.set(cmd.into());
        st.edit_weight.set("1800".into());
        let (calls, _) = st.save_calls().unwrap();
        assert_eq!(calls[0].1["text"], json!(cmd), "正文必须逐字保留");

        // 用户手打的字面 `\n` 也不该被再转一层成 `\\n`。
        st.edit_text.set(r"第一行\n第二行".into());
        let (again, _) = st.save_calls().unwrap();
        assert_eq!(again[0].1["text"], json!(r"第一行\n第二行"));
    }

    /// 编辑一条多行词条、只改权重时仍须命中 `update` 这条路。
    ///
    /// 判「是否只改了权重」比较的是 `edit_text` 与回填的原值，而原值来自列表（转义域）。
    /// 若投影只做了一半（比如改在别处、或漏掉这条路径），未改动的多行词条会被判成
    /// 「键变了」→ 走删旧+加新，用户只是调了个权重却看到词条被重建。
    #[test]
    fn save_calls_multiline_weight_only_still_updates() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(GLOBAL_DOMAINS);
        st.sub_tab.set(0);
        st.open_edit("aaaa", r"第一行\n第二行", 0, 0); // 回填的是转义形态
        st.edit_weight.set("9".into());
        let (calls, _) = st.save_calls().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "dict.update");
    }

    #[test]
    fn save_calls_dict_add() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
        st.sub_tab.set(0);
        st.edit_code.set(String::new());
        st.edit_text.set(String::new());
        assert!(st.save_calls().is_err(), "空词条应校验失败");
    }

    #[test]
    fn save_calls_rejects_non_numeric_weight() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(DOMAIN_PHRASE);
        assert_eq!(
            st.tabs_for_domain(),
            vec![LogicalTab::PhraseSystem, LogicalTab::PhraseUser]
        );
        // 码表：四类含候选调整
        st.domain.set(GLOBAL_DOMAINS);
        assert_eq!(
            st.tabs_for_domain(),
            vec![
                LogicalTab::UserDict,
                LogicalTab::Temp,
                LogicalTab::Freq,
                LogicalTab::Shadow
            ]
        );
        // 拼音：四类含候选调整。曾是三类——那时拼音下调位被整体屏蔽、规则表恒空；
        // 置顶放开后规则真的会产生，标签必须给，否则用户看不见也删不掉。
        st.domain.set(GLOBAL_DOMAINS + 1);
        assert_eq!(
            st.tabs_for_domain(),
            vec![
                LogicalTab::UserDict,
                LogicalTab::Temp,
                LogicalTab::Freq,
                LogicalTab::Shadow
            ]
        );
        // 混输：仅候选调整
        st.domain.set(GLOBAL_DOMAINS + 2);
        assert_eq!(st.tabs_for_domain(), vec![LogicalTab::Shadow]);
    }

    #[test]
    fn current_category_follows_dynamic_tabs() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("mx", "混输", "mixed")]);
        st.domain.set(GLOBAL_DOMAINS); // 混输
        st.sub_tab.set(0);
        assert_eq!(st.current_category(), WordCategory::Shadow);
        // 短语用户页
        st.domain.set(DOMAIN_PHRASE);
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
    fn client_search_filters_unpaged_rows() {
        // 同 `client_sort_reorders_unpaged_rows`：客户端过滤现在只剩短语系统/快捷输入两类。
        let st = DictManagerState::new(); // domain 0 = 短语，sub_tab 0 = 系统短语
        *st.rows.borrow_mut() = vec![
            WordRow {
                code: "nihao".into(),
                text: "你好".into(),
                is_system: true,
                ..Default::default()
            },
            WordRow {
                code: "women".into(),
                text: "我们".into(),
                is_system: true,
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

    /// 页码 ⇄ 偏移的换算与钳制。越界**不报错也不跳空页**：打「999」是想去最后一页。
    #[test]
    fn page_offset_clamps_to_valid_range() {
        assert_eq!(page_offset(1, 3), 0);
        assert_eq!(page_offset(2, 3), PAGE_SIZE);
        assert_eq!(page_offset(3, 3), PAGE_SIZE * 2);
        assert_eq!(page_offset(999, 3), PAGE_SIZE * 2, "超出末页钳到末页");
        assert_eq!(page_offset(0, 3), 0, "0 页不存在，钳到首页");
        assert_eq!(page_offset(5, 0), 0, "空表也当一页，不产生非法偏移");
    }

    /// 空表也算一页：否则跳页框旁边会写「第 1 / 0 页」，那是自相矛盾的说法。
    #[test]
    fn total_pages_counts_partial_page_and_never_zero() {
        let st = DictManagerState::new();
        st.total.set(0);
        assert_eq!(st.total_pages(), 1);
        st.total.set(1);
        assert_eq!(st.total_pages(), 1);
        st.total.set(PAGE_SIZE);
        assert_eq!(st.total_pages(), 1);
        st.total.set(PAGE_SIZE + 1);
        assert_eq!(st.total_pages(), 2, "余数要单独占一页");
        st.total.set(PAGE_SIZE * 3);
        assert_eq!(st.total_pages(), 3);
    }

    #[test]
    fn current_page_is_one_based() {
        let st = DictManagerState::new();
        assert_eq!(st.current_page(), 1);
        *st.offset.borrow_mut() = PAGE_SIZE * 4;
        assert_eq!(st.current_page(), 5);
    }

    /// 跳页框输入非数字/空串：按「什么也没发生」处理并把框回填成当前页。
    /// 为一个打错的框弹错误提示，比直接复位更烦人。
    #[test]
    fn page_jump_restores_input_on_garbage() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(GLOBAL_DOMAINS);
        st.sub_tab.set(0); // 用户词库（分页）
        *st.offset.borrow_mut() = PAGE_SIZE * 2;
        st.page_input.set("abc".into());
        st.submit_page_jump();
        assert_eq!(st.page_input.get(), "3", "非数字回填当前页");
        assert_eq!(*st.offset.borrow(), PAGE_SIZE * 2, "偏移不动");

        st.page_input.set("  ".into());
        st.submit_page_jump();
        assert_eq!(st.page_input.get(), "3", "空串同理");
    }

    /// 非分页类别不接受跳页（那组控件也不显示），别让 offset 被写脏。
    #[test]
    fn goto_page_is_noop_for_unpaged_category() {
        let st = DictManagerState::new(); // 系统短语（非分页）
        st.total.set(500);
        st.goto_page(3);
        assert_eq!(*st.offset.borrow(), 0);
    }

    /// 列表每次重建后，「/ N 页」与跳页框都要跟上；非分页类别不显示页数。
    #[test]
    fn page_widgets_sync_on_rebuild() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(GLOBAL_DOMAINS);
        st.sub_tab.set(0); // 用户词库（分页）
        *st.offset.borrow_mut() = PAGE_SIZE;
        st.apply_rows(&json!({ "items": [
            { "code": "aaaa", "text": "工", "weight": 0 }
        ], "total": 250 }));
        assert_eq!(st.page_count_text.get(), "/ 3 页");
        assert_eq!(st.page_input.get(), "2", "跳页框回填实际页码");
        assert_eq!(st.page_status.get(), "第 101-101 / 共 250 条");

        // 切到非分页类别：页数文字清空，别留着上一个类别的「/ 3 页」。
        st.domain.set(0);
        st.sub_tab.set(0); // 系统短语
        st.apply_rows(&json!([]));
        assert!(st.page_count_text.get().is_empty());
    }

    /// 临时词库与候选调整改服务端分页后必须真的发出分页参数——spec 里开关拨对了、
    /// 参数没跟上的话，症状是「翻到第 2 页还是第 1 页那批数据」。
    #[test]
    fn build_params_temp_and_shadow_are_paged() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(GLOBAL_DOMAINS);
        for (tab, name) in [(1usize, "临时词库"), (3, "候选调整")] {
            st.sub_tab.set(tab);
            *st.offset.borrow_mut() = PAGE_SIZE;
            st.search.set("ni".into());
            let p = st.build_list_params();
            assert_eq!(p["offset"], json!(PAGE_SIZE), "{name} 应带 offset");
            assert_eq!(p["limit"], json!(PAGE_SIZE), "{name} 应带 limit");
            assert_eq!(p["prefix"], json!("ni"), "{name} 的搜索走服务端");
            assert_eq!(p["schemaId"], json!("wb"), "{name} 按方案取数");
        }
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
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
        st.sub_tab.set(3);
        st.shadow_code.set(String::new());
        assert!(st.shadow_add_call().is_err());
    }

    #[test]
    fn shadow_save_calls_add_mode_single_call() {
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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

    /// 新建短语的默认权重。**不是随手挑的数**：主程序侧短语与码表精确候选按权重竞争
    /// （`PHRASE_WEIGHT_BASE` 40M 硬顶已删除），这个值决定新短语打不打得出。改它要同时
    /// 核对主仓 `webdata.rs::web_phrase_add` 的兜底默认。
    #[test]
    fn open_add_phrase_user_weight_1800() {
        let st = DictManagerState::new();
        st.sub_tab.set(1); // 用户短语
        assert_eq!(st.current_category(), WordCategory::PhraseUser);
        st.open_add();
        assert_eq!(st.edit_weight.get(), "1800");
    }

    #[test]
    fn phrase_user_list_params_paged() {
        let st = DictManagerState::new();
        st.domain.set(DOMAIN_PHRASE);
        st.sub_tab.set(1); // 用户短语（分页）
        let p = st.build_list_params();
        assert_eq!(p["offset"], serde_json::json!(0));
        assert_eq!(p["limit"], serde_json::json!(PAGE_SIZE));
        assert!(p.get("schemaId").is_none(), "短语全局无 schemaId");
    }

    #[test]
    fn phrase_system_list_params_bare() {
        let st = DictManagerState::new();
        st.domain.set(DOMAIN_PHRASE);
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
        st.domain.set(GLOBAL_DOMAINS + 1);
        st.sub_tab.set(3);
        st.domain.set(GLOBAL_DOMAINS); // 混输
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
        // 域：全局域 + 拼音(折叠) + 五笔86（两个拼音方案折叠成一个域，而非各占一格）。
        // 全局域那一段从 `GLOBAL_DOMAIN_LABELS` 派生而不再写死：这里要钉的是「方案域
        // 跟在全局域后面、且拼音族折叠成一项」，全局域叫什么由那个常量单独管。
        assert_eq!(labels, global_labels_then(&["拼音", "五笔86 · 码表"]));
        // 选"拼音"域，current_schema_id 返回代表(全拼优先)
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
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
        st.domain.set(GLOBAL_DOMAINS);
        // 双拼在前、全拼在后 → 代表应升级为全拼
        assert_eq!(st.current_schema_id().as_deref(), Some("pinyin_simp"));
        // 仍折叠为一个"拼音"域
        assert_eq!(st.domain_labels(), global_labels_then(&["拼音"]));
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
        st.domain.set(GLOBAL_DOMAINS);
        st.on_nav_changed();
        assert_eq!(st.sub_tab.get(), 0);
        // 在码表域停到"词频"(2)。
        st.sub_tab.set(2);
        st.on_nav_changed();
        // 切到拼音域(2)：新域无记录，回第一个（不再沿用码表的 2）。
        st.domain.set(GLOBAL_DOMAINS + 1);
        st.on_nav_changed();
        assert_eq!(st.sub_tab.get(), 0, "切到新域回第一个子标签");
        // 切回码表域(1)：恢复上次停留的"词频"(2)。
        st.domain.set(GLOBAL_DOMAINS);
        st.on_nav_changed();
        assert_eq!(st.sub_tab.get(), 2, "切回旧域应恢复其上次子标签");
    }

    #[test]
    fn switch_domain_defaults_first_tab_when_no_memory() {
        // 复现用户反馈：在短语域选"用户短语"(1)，切到方案域不应沿用下标 1。
        let st = DictManagerState::new();
        with_schemas(&st, vec![schema("wb", "五笔", "codetable")]);
        st.domain.set(DOMAIN_PHRASE);
        st.sub_tab.set(1); // 用户短语
        st.on_nav_changed();
        st.domain.set(GLOBAL_DOMAINS); // 切到码表域
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
        // 域 = 全部全局域 + 五笔 + 拼音。循环要走遍**全部**域，全局域也在其中——
        // 漏了它们就等于「‹ › 按钮切不到快捷输入 / 常用字」。
        let total = GLOBAL_DOMAINS + 2;
        assert_eq!(st.domain_count(), total);
        assert_eq!(st.domain.get(), DOMAIN_PHRASE);
        // 逐格前进，下标必须严格 +1 走完一整圈（写死序列的话每加一个全局域都要重抄）。
        for expect in 1..total {
            st.next_domain();
            assert_eq!(st.domain.get(), expect, "第 {expect} 格");
        }
        st.next_domain();
        assert_eq!(st.domain.get(), DOMAIN_PHRASE, "末尾循环回首");
        st.prev_domain();
        assert_eq!(st.domain.get(), total - 1, "首部循环到末尾");
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
        assert_eq!(items.len(), GLOBAL_DOMAINS + 2);
        // 两个全局域都标「全局」徽章——它们不按方案，徽章位不能空着，
        // 否则下拉里只有这两项没有尾标，看着像渲染缺了一块。
        assert_eq!(items[DOMAIN_PHRASE].label, "快捷短语");
        assert_eq!(
            items[DOMAIN_PHRASE].badge.as_ref().map(|(t, _)| t.as_str()),
            Some("全局")
        );
        assert_eq!(items[DOMAIN_QUICK].label, "快捷输入");
        assert_eq!(
            items[DOMAIN_QUICK].badge.as_ref().map(|(t, _)| t.as_str()),
            Some("全局")
        );
        // 主文本取方案名（去掉 " · 码表" 后缀），引擎类型进徽章。
        assert_eq!(items[GLOBAL_DOMAINS].label, "五笔86");
        assert_eq!(
            items[GLOBAL_DOMAINS]
                .badge
                .as_ref()
                .map(|(t, _)| t.as_str()),
            Some("码表")
        );
        // 拼音域也带徽章，保持统一。
        assert_eq!(items[GLOBAL_DOMAINS + 1].label, "拼音");
        assert_eq!(
            items[GLOBAL_DOMAINS + 1]
                .badge
                .as_ref()
                .map(|(t, _)| t.as_str()),
            Some("拼音"),
            "拼音域也应带引擎徽章以保持统一"
        );
    }
}
