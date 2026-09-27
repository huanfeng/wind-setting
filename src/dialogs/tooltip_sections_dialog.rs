//! 「提示内容」对话框：管理 `[[ui.tooltip.sections]]` —— 候选悬停提示（气泡）由哪几段组成。
//!
//! 一段 = 段名 + 内容模板（语法与变量同候选注释模板）+ 求值粒度等几个字段。这取代了旧的
//! 六个开关（`code_enabled` / `pinyin_enabled` / … / `debug_enabled`），core 加载时把旧开关
//! 迁成段列表。设计见 core 仓 `docs/design/candidate-tooltip-sections.md`。
//!
//! 形态照抄「角标规则」那屏（`langbar_badges_dialog`）：拖拽排序的条目列表 + 启用开关 +
//! 编辑 / 删除 + 二级表单；「按预设添加」照「注释词库」那屏的文件挑选弹层。
//!
//! # 顺序即显示顺序，所以必须能排序
//!
//! 数组顺序就是气泡里自上而下的顺序。列表用 `reorder_list_signal` 拖拽排序，与注释词库、
//! 角标规则两屏同一条理由。
//!
//! # ★ 启用态与条目定义分开存
//!
//! `rows` 只装段定义，启用开关另存在 `switches`，按行内生成的 `uid` 索引——段没有天然的
//! 唯一标识（段名可以重复、可以为空），下标又会被拖拽改掉。理由同角标规则那屏的模块文档。
//!
//! # ★ 写回在原对象上 insert，且六个字段一律写全
//!
//! 在 [`SectionRow::raw`] 上 insert 是为了不吃掉本屏不认识的字段（同注释词库那屏）。
//! 六个字段一律写全，是为了**与出厂列表逐字段可比**：core 保存时「等于出厂值即删键」
//! （`set_user_value` 的收口）按 TOML 值整体比较，少写一个取默认值的字段就比不上，
//! 用户点了「恢复出厂」却在用户层留下一份与出厂一模一样的冻结副本（设计 §9.1：用户层的
//! 段列表冻结在写下的那一刻；新增的出厂段有补段迁移送达，出厂段的后续调整则到不了他）。
//!
//! # ★ 「出厂」取 core 的 `config.getDefaults`，不取快照
//!
//! core 判「等于出厂值即删键」比的是系统预置（L1⊕L2⊕L2.5，`preset_for_pruning`），
//! 快照里的 `default` 是纯 L1。两者一旦不同，「恢复出厂」保存后留下的是一份与 core 口径
//! 不符的副本，「与出厂设置相同」的判定也跟着说错——同 `manifest.rs` 里
//! `core_defaults_config` 那条「⛔ 不能用纯 L1」。故每次打开对话框经
//! [`TooltipSectionsUi::factory_source`] 向 core 取一次，取不到才回落快照。
//!
//! # ★ 模板里的制表符与换行按转义显示
//!
//! 「拆字 / 拼音」合并段的模板里有一个真实的 `\t`（分列）。单行输入框里看不见它，
//! 用户一改就可能把分列删掉而毫无察觉，故表单里显示成 `\t`，确认时再还原
//! （[`escape_template`] / [`unescape_template`]，`\\` 表示反斜杠本身，互为逆运算）。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

/// 求值粒度，`(落盘值, 界面名)`。**必须与 core `TooltipSection::each` 的取值逐字一致。**
pub const EACH: &[(&str, &str)] = &[("", "整个候选"), ("han", "逐汉字"), ("char", "逐字符")];

/// 模板变量说明页（与候选注释那页 `settings/appearance/candidate-comment` 并列）。
const TOOLTIP_DOC_URL: &str = "https://windinput.com/docs/settings/appearance/candidate-tooltip";

/// 一个预设段：`(预设名, label, each, promote, template)`。
///
/// ⛔ **不是在这里另编一套**：除「释义」外，每一条都与 core 的出厂段列表
/// （`default_tooltip_sections`）或旧开关迁移（`tooltip_sections_from_legacy`）产出的那一段
/// 逐字段相同，由测试 `presets_match_core` 钉住。「释义」对应 core 气泡测试里的同名段
/// （`${dict}` 在 core 出厂与迁移里都没有）。
pub const PRESETS: &[(&str, &str, &str, &str, &str)] = &[
    ("完整原文", "完整原文", "", "", "${full_text}"),
    ("编码", "编码{(${code_source})}", "", "", "${word_code}"),
    ("拼音", "拼音", "han", "", "${char}：${readings}"),
    (
        "拆字",
        "拆字",
        "han",
        "",
        "${char}：${chaizi}{ [${chaizi_code}]}",
    ),
    (
        "拆字 + 拼音",
        "拆字 / 拼音",
        "han",
        "chaizi",
        "${char}：{${chaizi}{ [${chaizi_code}]}\t}${readings}",
    ),
    ("Unicode", "Unicode", "char", "", "${char}：${unicode}"),
    ("注释库释义", "释义", "", "", "${dict}"),
    ("调试", "调试", "", "", "${debug}"),
];

/// 预设的一句话说明（与 [`PRESETS`] 按下标对应），显示在挑选弹层里。
const PRESET_DESCS: &[&str] = &[
    "候选被截断时显示完整文字，未截断时整段不出现",
    "该词在编码来源方案里的全部编码",
    "每个汉字一行，列出全部读音",
    "每个汉字一行，显示字根与拆字编码",
    "拆字与读音合在一段，每字一行分两列",
    "每个字符一行，显示码位",
    "注释词库里的释义（需先挂载注释词库）",
    "编码来源、权重等调试信息",
];

/// 一段（对应一个 `[[ui.tooltip.sections]]`）。
///
/// **不含 `enabled`**：那一位存在 [`TooltipSectionsUi::switches`] 里，理由见模块文档。
#[derive(Clone, PartialEq)]
pub struct SectionRow {
    /// 仅进程内有效的稳定标识，用来索引启用开关。**不落盘**。
    pub uid: u32,
    /// 段名（本身也是模板）。空 = 无标题行。
    pub label: String,
    /// 段内容模板（真实字符，不转义）。
    pub template: String,
    /// 求值粒度，取值见 [`EACH`]；手写配置里的未知值原样保留。
    pub each: String,
    /// 仅逐字段：该变量非空的行排在前面。
    pub promote: String,
    /// 内容恰为一行时与段名同行显示。
    pub inline: bool,
    /// 读进来的原始对象，写回时在它上面 insert。见模块文档。
    pub raw: Value,
}

/// 新建态的 `editing` 哨兵。
pub const NEW_SECTION: usize = usize::MAX;

/// 新建 / 编辑一段的表单。**两者共用**——差别只在确认时是追加还是替换。
#[derive(Clone)]
pub struct SectionForm {
    pub visible: Signal<bool>,
    /// 正在编辑第几段；[`NEW_SECTION`] = 新建。
    pub editing: Signal<usize>,
    pub label: Signal<String>,
    /// 段内容，**转义后**的文本（见 [`escape_template`]）。
    pub template: Signal<String>,
    /// 粒度下拉的选项：[`EACH`] 的三项，外加（原值未识别时）一项「原值（未识别）」。
    pub each_options: Signal<Vec<String>>,
    /// 粒度在 [`Self::each_options`] 里的下标。
    pub each_idx: Signal<usize>,
    /// 打开表单时的原 `each`。未识别时下拉多出一项指向它，用户不动就原样写回——只改个段名
    /// 就把手写的值悄悄改成 `""`，是这一屏最不该有的静默改写。
    pub each_raw: Signal<String>,
    pub promote: Signal<String>,
    pub inline: Signal<bool>,
    /// 校验失败原因；空 = 无。就地显示，不再弹一层。
    pub error: Signal<String>,
    pub title: Signal<String>,
}

impl SectionForm {
    pub fn new() -> Self {
        Self {
            visible: signal(false),
            editing: signal(NEW_SECTION),
            label: signal(String::new()),
            template: signal(String::new()),
            each_options: signal(each_options("")),
            each_idx: signal(0),
            each_raw: signal(String::new()),
            promote: signal(String::new()),
            inline: signal(false),
            error: signal(String::new()),
            title: signal("新建段".to_string()),
        }
    }

    pub fn open_new(&self) {
        self.editing.set(NEW_SECTION);
        self.label.set(String::new());
        self.template.set(String::new());
        self.each_options.set(each_options(""));
        self.each_idx.set(0);
        self.each_raw.set(String::new());
        self.promote.set(String::new());
        self.inline.set(false);
        self.error.set(String::new());
        self.title.set("新建段".to_string());
        self.visible.set(true);
    }

    pub fn open_edit(&self, index: usize, row: &SectionRow) {
        self.editing.set(index);
        self.label.set(row.label.clone());
        self.template.set(escape_template(&row.template));
        self.each_options.set(each_options(&row.each));
        self.each_idx.set(index_of_each(&row.each));
        self.each_raw.set(row.each.clone());
        self.promote.set(row.promote.clone());
        self.inline.set(row.inline);
        self.error.set(String::new());
        self.title.set("编辑段".to_string());
        self.visible.set(true);
    }
}

impl Default for SectionForm {
    fn default() -> Self {
        Self::new()
    }
}

/// 向 core 取出厂段列表的回调，见 [`TooltipSectionsUi::factory_source`]。
pub type FactorySource = Box<dyn Fn() -> Option<Vec<Value>>>;

/// 「提示内容」对话框的全部可变状态。
#[derive(Clone)]
pub struct TooltipSectionsUi {
    pub visible: Signal<bool>,
    pub rows: Signal<Vec<SectionRow>>,
    /// uid → 启用开关。
    pub switches: Rc<RefCell<HashMap<u32, Signal<bool>>>>,
    /// 下一个可用 uid。只增不减，删了再加也不会撞上还没清干净的旧开关。
    next_uid: Rc<Cell<u32>>,
    pub form: SectionForm,
    /// 「按预设添加」弹层的可见性。
    pub presets: Signal<bool>,
    /// 出厂段列表，供「恢复出厂」与「是否与出厂相同」的判定。构造时是快照里的
    /// `default`，打开对话框时经 [`Self::factory_source`] 换成 core 的口径（见模块文档）。
    pub factory: Rc<RefCell<Vec<Value>>>,
    /// 向 core 取出厂段列表；`None` = 取不到（未连接等），保留原值。由 `manifest.rs` 接到
    /// `core_defaults_config` 上。
    pub factory_source: Rc<RefCell<Option<FactorySource>>>,
}

impl TooltipSectionsUi {
    pub fn new(factory: Vec<Value>) -> Self {
        Self {
            visible: signal(false),
            rows: signal(Vec::new()),
            switches: Rc::new(RefCell::new(HashMap::new())),
            next_uid: Rc::new(Cell::new(0)),
            form: SectionForm::new(),
            presets: signal(false),
            factory: Rc::new(RefCell::new(factory)),
            factory_source: Rc::new(RefCell::new(None)),
        }
    }

    fn take_uid(&self) -> u32 {
        let id = self.next_uid.get();
        self.next_uid.set(id.wrapping_add(1));
        id
    }

    /// 取某段的启用开关；不存在时**当场建一个**（新建那一刻还没有）。
    pub fn switch_of(&self, uid: u32, default_on: bool) -> Signal<bool> {
        if let Some(s) = self.switches.borrow().get(&uid) {
            return *s;
        }
        let s = signal(default_on);
        self.switches.borrow_mut().insert(uid, s);
        s
    }

    /// 用配置里的数组播种（构建 / 重载配置 / 恢复出厂时调用）。
    pub fn seed(&self, items: &[Value]) {
        self.switches.borrow_mut().clear();
        let mut rows = Vec::with_capacity(items.len());
        for (r, on) in rows_from_value(items).into_iter().zip(enabled_flags(items)) {
            let uid = self.take_uid();
            self.switches.borrow_mut().insert(uid, signal(on));
            rows.push(SectionRow { uid, ..r });
        }
        self.rows.set(rows);
    }

    /// 打开对话框时调用：经 [`Self::factory_source`] 向 core 重取一次出厂列表。
    pub fn refresh_factory(&self) {
        let got = self.factory_source.borrow().as_ref().and_then(|f| f());
        if let Some(v) = got {
            *self.factory.borrow_mut() = v;
        }
    }

    /// 恢复出厂：界面换成出厂列表；保存时 core 发现它等于出厂值，删掉用户层的这个键，
    /// 此后跟随出厂（出厂内容的调整会自动生效）。
    ///
    /// 出厂列表为空（core 取不到、快照也没有）时什么都不做：那不是「出厂就是零段」，
    /// 而是不知道出厂是什么，照样播种会把用户的列表清空。
    pub fn reset_to_factory(&self) {
        let f = self.factory.borrow().clone();
        if f.is_empty() {
            return;
        }
        self.seed(&f);
    }

    /// 当前列表是否与出厂列表相同（界面据此显示「跟随出厂」还是「已自定义」）。
    pub fn is_factory(&self) -> bool {
        self.to_value().as_array().map(Vec::as_slice) == Some(self.factory.borrow().as_slice())
    }

    /// 提交表单：新建则追加（即启用），编辑则替换。`template` 是**已还原**的真实文本。
    pub fn commit_form(
        &self,
        index: usize,
        label: String,
        template: String,
        each: String,
        promote: String,
        inline: bool,
    ) {
        if index == NEW_SECTION {
            let uid = self.take_uid();
            self.switch_of(uid, true);
            self.rows.update(|v| {
                v.push(SectionRow {
                    uid,
                    label,
                    template,
                    each,
                    promote,
                    inline,
                    raw: json!({}),
                })
            });
            return;
        }
        self.rows.update(|v| {
            if let Some(slot) = v.get_mut(index) {
                // uid 与 raw 原样留着：前者绑着启用开关，后者装着本屏不认识的字段。
                slot.label = label;
                slot.template = template;
                slot.each = each;
                slot.promote = promote;
                slot.inline = inline;
            }
        });
    }

    /// 按预设追加一段（启用）。
    pub fn add_preset(&self, i: usize) {
        let Some((_, label, each, promote, template)) = PRESETS.get(i) else {
            return;
        };
        self.commit_form(
            NEW_SECTION,
            label.to_string(),
            template.to_string(),
            each.to_string(),
            promote.to_string(),
            false,
        );
    }

    /// 删除第 `index` 段（定义与开关一并去掉）。
    pub fn delete(&self, index: usize) {
        let Some(r) = self.rows.get().get(index).cloned() else {
            return;
        };
        self.rows.update(|v| {
            if index < v.len() {
                v.remove(index);
            }
        });
        self.switches.borrow_mut().remove(&r.uid);
    }

    /// 当前工作态 → 回写给 core 的数组。
    pub fn to_value(&self) -> Value {
        let sw = self.switches.borrow();
        Value::Array(
            self.rows
                .get()
                .iter()
                .map(|r| {
                    let on = sw.get(&r.uid).map(|s| s.get()).unwrap_or(true);
                    row_to_value(r, on)
                })
                .collect(),
        )
    }
}

/// 粒度下拉的选项：[`EACH`] 的界面名；`raw` 未识别时末尾多一项「原值（未识别）」。
fn each_options(raw: &str) -> Vec<String> {
    let mut v: Vec<String> = EACH.iter().map(|(_, l)| l.to_string()).collect();
    if !EACH.iter().any(|(k, _)| *k == raw) {
        v.push(format!("{raw}（未识别）"));
    }
    v
}

/// 粒度落盘值 → [`each_options`] 下标；未识别的指向末尾那一项「原值（未识别）」。
fn index_of_each(each: &str) -> usize {
    EACH.iter()
        .position(|(k, _)| *k == each)
        .unwrap_or(EACH.len())
}

/// 下拉下标 → 落盘值：[`EACH`] 之外的那一项就是原值（`raw`），原样写回。
fn each_of_index(idx: usize, raw: &str) -> String {
    EACH.get(idx)
        .map(|(k, _)| k.to_string())
        .unwrap_or_else(|| raw.to_string())
}

/// 粒度落盘值 → 界面名；未知原样返回（手写配置里可能有本屏不认识的值）。
fn each_label(each: &str) -> String {
    EACH.iter()
        .find(|(k, _)| *k == each)
        .map(|(_, l)| l.to_string())
        .unwrap_or_else(|| each.to_string())
}

/// 解析配置数组 → 段定义（纯函数，供测试）。`uid` 由调用方补，这里恒填 0。
pub fn rows_from_value(items: &[Value]) -> Vec<SectionRow> {
    items
        .iter()
        .map(|it| {
            let s = |k: &str| {
                it.get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            SectionRow {
                uid: 0,
                label: s("label"),
                template: s("template"),
                each: s("each"),
                promote: s("promote"),
                inline: it.get("inline").and_then(|v| v.as_bool()).unwrap_or(false),
                raw: it.clone(),
            }
        })
        .collect()
}

/// 各段的启用位（缺省视为启用，与 core 的 `default_true` 同源）。
pub fn enabled_flags(items: &[Value]) -> Vec<bool> {
    items
        .iter()
        .map(|it| it.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true))
        .collect()
}

/// 一段 → JSON。在 `raw` 上 insert，六个字段一律写全（见模块文档）。
pub fn row_to_value(r: &SectionRow, enabled: bool) -> Value {
    let mut v = r.raw.clone();
    if !v.is_object() {
        v = json!({});
    }
    let obj = v.as_object_mut().expect("上面刚保证是对象");
    obj.insert("enabled".into(), json!(enabled));
    obj.insert("label".into(), json!(r.label));
    obj.insert("template".into(), json!(r.template));
    obj.insert("each".into(), json!(r.each));
    obj.insert("promote".into(), json!(r.promote));
    obj.insert("inline".into(), json!(r.inline));
    v
}

/// 模板 → 表单里显示的文本：`\` → `\\`、制表符 → `\t`、换行 → `\n`。见模块文档。
pub fn escape_template(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// [`escape_template`] 的逆运算。不成对的反斜杠（后面不是 `\` / `t` / `n`）原样保留。
pub fn unescape_template(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.peek() {
            Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            _ => {
                out.push('\\');
                continue;
            }
        }
        it.next();
    }
    out
}

/// 表单校验。`template` 是还原后的文本。
pub fn validate(template: &str, promote: &str) -> Option<&'static str> {
    if template.trim().is_empty() {
        return Some("段内容不能为空");
    }
    // promote 填的是变量名（不带 `${}`）：写成 `${chaizi}` 是最容易犯的错，core 只会
    // 查不到这个变量、整段不分组，界面上看不出为什么。
    if !promote
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Some("「优先显示」填变量名本身，如 chaizi，不带 ${ }");
    }
    None
}

/// 一段的副标题：粒度 · 模板（转义后，看得见制表符）。
fn row_desc(r: &SectionRow) -> String {
    format!("{} · {}", each_label(&r.each), escape_template(&r.template))
}

/// 表单里的一行：`标签 [控件]`。
fn form_row(label: &str, ctrl: Element) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(Element::label(label).font_size(12.5).width(72))
        .child(ctrl)
}

fn dialog_frame(width: i32) -> Element {
    Element::col()
        .width(width)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
}

fn dialog_title(text: impl Into<TextContent>) -> Element {
    Element::label(text)
        .font_size(14.0)
        .font_weight(600)
        .fg_role(Role::Text)
        .width_match()
        .padding_xy(18, 14)
}

fn dialog_note(text: &str, lines: usize) -> Element {
    Element::label(text)
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(lines)
        .padding_xy(18, 8)
}

/// 构建「提示内容」对话框（含二级的预设挑选与新建/编辑表单）。
pub fn build_tooltip_sections_dialog(ui: &TooltipSectionsUi) -> Element {
    let visible = ui.visible;
    let row_ui = ui.clone();

    let list = Element::reorder_list_signal(ui.rows, move |r: SectionRow, handle| {
        let idx = row_ui
            .rows
            .get()
            .iter()
            .position(|x| x.uid == r.uid)
            .unwrap_or(0);
        let sig = row_ui.switch_of(r.uid, true);
        let e_ui = row_ui.clone();
        let d_ui = row_ui.clone();
        let title = if r.label.trim().is_empty() {
            "（无段名）".to_string()
        } else {
            r.label.clone()
        };
        let ctrl = Element::row()
            .cross(Align::Center)
            .spacing(6)
            .child(
                Element::button("编辑")
                    .small()
                    .outline_soft()
                    .on_click(move |_| {
                        if let Some(row) = e_ui.rows.get().get(idx) {
                            e_ui.form.open_edit(idx, row);
                        }
                    }),
            )
            .child(
                Element::button("删除")
                    .small()
                    .outline_soft()
                    .on_click(move |_| d_ui.delete(idx)),
            )
            .child(Element::switch(sig));

        Element::col()
            .width_match()
            .child(Element::divider())
            .child(
                Element::row()
                    .width_match()
                    .cross(Align::Center)
                    .padding_xy(8, 0)
                    .child(handle)
                    .child(
                        crate::widgets::setting_row(&title, Some(&row_desc(&r)), ctrl).weight(1.0),
                    ),
            )
    })
    .on_reorder({
        let rows = ui.rows;
        move |_ctx, from, to| {
            rows.update(|v| {
                let x = v.remove(from);
                v.insert(to.min(v.len()), x);
            })
        }
    });

    // 段多了（出厂 6 段，用户再加几段）会把对话框撑出窗口：列表封顶可滚。封顶按主窗最小高
    // 440 算（约四行）——再高，窗口拉到最矮时底栏的按钮就被裁掉了。
    let list = Element::scroll()
        .width_match()
        .max_height(216)
        .padding_xy(16, 2)
        .child(list);

    // R8：列表与出厂相同 = 跟随出厂（保存时 core 删掉用户层的键）；否则是冻结的自定义副本。
    let (f_ui, c_ui) = (ui.clone(), ui.clone());
    let status = Element::col()
        .width_match()
        .padding_xy(18, 6)
        .child(
            Element::label("与出厂设置相同：保存后跟随出厂，出厂内容的调整会自动生效。")
                .font_size(12.0)
                .fg_role(Role::TextSubtle)
                .max_lines(2)
                .visible_when(move || f_ui.is_factory()),
        )
        .child(
            Element::label("已自定义：出厂段的后续调整不会同步到这里，可恢复出厂取回。")
                .font_size(12.0)
                .fg_role(Role::TextSubtle)
                .max_lines(2)
                .visible_when(move || !c_ui.is_factory()),
        );

    let preset_ui = ui.clone();
    let new_ui = ui.clone();
    let reset_ui = ui.clone();
    let frame = dialog_frame(560)
        .child(dialog_title("提示内容"))
        .child(Element::divider())
        .child(dialog_note(
            "悬停候选时，气泡按下面的顺序显示各段；段内容为空时整段不出现。\n\
             可拖动左侧手柄调整顺序，段名与内容的写法同候选注释模板。",
            3,
        ))
        .child(list)
        .child(Element::divider())
        .child(status)
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    Element::button("按预设添加…")
                        .small()
                        .outline_soft()
                        .on_click(move |_| preset_ui.presets.set(true)),
                )
                .child(
                    Element::button("新建段…")
                        .small()
                        .outline_soft()
                        .on_click(move |_| new_ui.form.open_new()),
                )
                .child(
                    Element::button("恢复出厂")
                        .small()
                        .outline_soft()
                        .on_click(move |_| reset_ui.reset_to_factory()),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::link("查看文档")
                        .url(TOOLTIP_DOC_URL)
                        .font_size(12.0),
                )
                .child(
                    Element::button("完成")
                        .small()
                        .on_click(move |_| visible.set(false)),
                ),
        );

    // 对话框每打开一次就向 core 重取一次出厂列表（见模块文档）。本屏的「设置」按钮由
    // manifest 的通用逻辑绑到 `visible` 上，没有「打开时」钩子，故盯 `visible` 的变化。
    let open_ui = ui.clone();
    let on_open = crate::pages::dict::reactive::reactive_effect(
        move || visible.version(),
        move || {
            if open_ui.visible.get() {
                open_ui.refresh_factory();
            }
        },
    );

    Element::stack()
        .child(on_open)
        .child(Element::dialog(visible, frame))
        .child(build_preset_picker(ui))
        .child(build_section_form_dialog(ui))
}

/// 二级弹层：从预设里挑一段追加。
fn build_preset_picker(ui: &TooltipSectionsUi) -> Element {
    let visible = ui.presets;
    let mut list = Element::col().width_match();
    for (i, (name, ..)) in PRESETS.iter().enumerate() {
        let add_ui = ui.clone();
        let btn = Element::button("添加").small().on_click(move |_| {
            add_ui.add_preset(i);
            add_ui.presets.set(false);
        });
        list = list.child(
            Element::col()
                .width_match()
                .child(Element::divider())
                .child(
                    Element::row().width_match().padding_xy(8, 0).child(
                        crate::widgets::setting_row(name, PRESET_DESCS.get(i).copied(), btn)
                            .weight(1.0),
                    ),
                ),
        );
    }

    let frame = dialog_frame(480)
        .child(dialog_title("按预设添加"))
        .child(Element::divider())
        .child(dialog_note(
            "添加的段排在末尾并已启用，可再拖动调整位置。已有同名段时照样追加。",
            2,
        ))
        .child(
            Element::scroll()
                .width_match()
                .max_height(270)
                .padding_xy(16, 2)
                .child(list),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .padding_xy(16, 12)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// 二级弹层：新建 / 编辑一段。
fn build_section_form_dialog(ui: &TooltipSectionsUi) -> Element {
    let form = ui.form.clone();
    let fc = form.clone();
    let ui2 = ui.clone();
    let visible = form.visible;

    let input = |sig: Signal<String>, placeholder: &str| {
        Element::text_input(sig, placeholder)
            .width_match()
            .height(32)
            .weight(1.0)
    };

    let frame = dialog_frame(520)
        .child(dialog_title(form.title))
        .child(Element::divider())
        .child(dialog_note(
            "段名、段内容都是模板，如 编码{(${code_source})}；段名留空则不显示标题行。\n\
             逐字时每个字求值一次、各占一行，${char} 是当前字。\n\
             内容里 \\t 表示制表符（分列），\\n 表示换行。",
            4,
        ))
        .child(
            Element::col()
                .width_match()
                .spacing(8)
                .padding_xy(18, 6)
                .child(form_row("段名", input(form.label, "拼音")))
                .child(form_row(
                    "段内容",
                    input(form.template, "${char}：${readings}"),
                ))
                .child(form_row(
                    "求值粒度",
                    Element::dropdown_signal(form.each_options, form.each_idx).min_width(160),
                ))
                .child(form_row(
                    "优先显示",
                    input(form.promote, "chaizi（仅逐字时有效）"),
                ))
                .child(form_row(
                    "同行显示",
                    Element::checkbox("内容只有一行时写成「段名: 内容」", form.inline)
                        .fg_role(Role::Text),
                )),
        )
        .child(
            Element::label_signal(form.error)
                .font_size(12.0)
                .fg_role(Role::Danger)
                .width_match()
                .max_lines(2)
                .padding_xy(18, 4),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| visible.set(false)),
                )
                .child(Element::button("确认").small().on_click(move |_| {
                    let template = unescape_template(&fc.template.get());
                    let promote = fc.promote.get().trim().to_string();
                    if let Some(msg) = validate(&template, &promote) {
                        fc.error.set(msg.to_string());
                        return;
                    }
                    fc.error.set(String::new());
                    let each = each_of_index(fc.each_idx.get(), &fc.each_raw.get());
                    ui2.commit_form(
                        fc.editing.get(),
                        fc.label.get(),
                        template,
                        each,
                        promote,
                        fc.inline.get(),
                    );
                    fc.visible.set(false);
                })),
        );
    Element::dialog(visible, frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn factory() -> Vec<Value> {
        let v = serde_json::to_value(wind_config::config::default_tooltip_sections()).unwrap();
        v.as_array().cloned().unwrap()
    }

    /// 预设与 core 的出厂段、旧开关迁移出的合并段逐字段相同（「释义」除外，见 [`PRESETS`]）。
    ///
    /// 漂移的后果是「按预设添加」得到的段与 core 出厂的同名段长得不一样——比如 core 改了
    /// 拼音段模板，这里还是旧的，用户删了再按预设加回来，外观就变了。
    #[test]
    fn presets_match_core() {
        use wind_config::config::{
            LegacyTooltipFlags, TooltipSection, default_tooltip_sections,
            tooltip_sections_from_legacy,
        };
        let mut core: Vec<TooltipSection> = default_tooltip_sections();
        core.extend(tooltip_sections_from_legacy(LegacyTooltipFlags {
            chaizi: true,
            ..Default::default()
        }));
        for (name, label, each, promote, template) in PRESETS {
            if *label == "释义" {
                continue;
            }
            let hit = core.iter().find(|s| s.label == *label);
            let s =
                hit.unwrap_or_else(|| panic!("预设「{name}」在 core 出厂 / 迁移里找不到同名段"));
            assert_eq!(
                (
                    s.each.as_str(),
                    s.promote.as_str(),
                    s.template.as_str(),
                    s.inline
                ),
                (*each, *promote, *template, false),
                "预设「{name}」与 core 不一致"
            );
        }
        // 反向：core 出厂的每一段都要能按预设加回来——否则用户删掉某个出厂段后，
        // 除了「恢复出厂」（连带丢掉其余自定义）没有别的路把它找回。
        for s in default_tooltip_sections() {
            assert!(
                PRESETS.iter().any(|(_, label, ..)| *label == s.label),
                "core 出厂段「{}」没有同名预设",
                s.label
            );
        }
        assert_eq!(PRESETS.len(), PRESET_DESCS.len(), "预设与说明要一一对应");
    }

    /// 出厂列表读进来再写回，逐字节不变——否则「什么都没动」也会判脏，且「恢复出厂」
    /// 后保存留下一份冻结副本（见模块文档）。
    #[test]
    fn factory_round_trips_exactly() {
        let ui = TooltipSectionsUi::new(factory());
        ui.seed(&factory());
        assert_eq!(ui.to_value(), Value::Array(factory()));
        assert!(ui.is_factory());
    }

    #[test]
    fn toggling_or_reordering_leaves_factory_and_reset_returns() {
        let ui = TooltipSectionsUi::new(factory());
        ui.seed(&factory());
        let uid = ui.rows.get()[3].uid; // 拆字段，出厂关
        ui.switch_of(uid, false).set(true);
        assert!(!ui.is_factory(), "开了一段就不再是出厂");
        ui.reset_to_factory();
        assert!(ui.is_factory());
        ui.rows.update(|v| v.swap(0, 1));
        assert!(!ui.is_factory(), "顺序即语义，换序就是自定义");
    }

    #[test]
    fn add_preset_appends_enabled_section_with_all_fields() {
        let ui = TooltipSectionsUi::new(factory());
        ui.seed(&[]);
        ui.add_preset(4); // 拆字 + 拼音
        let v = ui.to_value();
        assert_eq!(
            v,
            json!([{
                "enabled": true,
                "label": "拆字 / 拼音",
                "template": "${char}：{${chaizi}{ [${chaizi_code}]}\t}${readings}",
                "each": "han",
                "promote": "chaizi",
                "inline": false,
            }])
        );
    }

    #[test]
    fn edit_keeps_switch_and_unknown_fields() {
        let ui = TooltipSectionsUi::new(vec![]);
        ui.seed(&[json!({ "enabled": false, "label": "a", "template": "x", "future": 1 })]);
        ui.commit_form(
            0,
            "b".into(),
            "y".into(),
            "char".into(),
            String::new(),
            true,
        );
        let v = ui.to_value();
        assert_eq!(v[0]["enabled"], json!(false), "编辑不改启用态");
        assert_eq!(v[0]["future"], json!(1), "不认识的字段原样保留");
        assert_eq!(v[0]["label"], json!("b"));
        assert_eq!(v[0]["each"], json!("char"));
        assert_eq!(v[0]["inline"], json!(true));
    }

    #[test]
    fn delete_removes_row_and_switch() {
        let ui = TooltipSectionsUi::new(vec![]);
        ui.seed(&factory());
        ui.delete(0);
        assert_eq!(ui.rows.get().len(), factory().len() - 1);
        assert_eq!(ui.switches.borrow().len(), factory().len() - 1);
    }

    /// 缺省字段按 core 的 serde 缺省读：`enabled` 缺省为开，其余为空。
    #[test]
    fn missing_fields_read_as_core_defaults() {
        let items = [json!({ "template": "${debug}" })];
        assert_eq!(enabled_flags(&items), vec![true]);
        let r = &rows_from_value(&items)[0];
        assert_eq!(
            (r.label.as_str(), r.each.as_str(), r.inline),
            ("", "", false)
        );
    }

    #[test]
    fn escape_round_trips() {
        for s in [
            "${char}：{${chaizi}{ [${chaizi_code}]}\t}${readings}",
            "a\nb",
            "C:\\path\\t",
            "\\",
            "",
        ] {
            assert_eq!(
                unescape_template(&escape_template(s)),
                s,
                "往返未闭合：{s:?}"
            );
        }
        assert_eq!(escape_template("a\tb"), "a\\tb");
        // 手打的孤立反斜杠不吞。
        assert_eq!(unescape_template("a\\xb"), "a\\xb");
    }

    #[test]
    fn validate_rejects_empty_template_and_braced_promote() {
        assert!(validate("  ", "").is_some());
        assert!(validate("${x}", "${chaizi}").is_some());
        assert!(validate("${x}", "chaizi").is_none());
        assert!(validate("${x}", "").is_none());
    }

    #[test]
    fn unknown_each_survives_display() {
        assert_eq!(each_label("foo"), "foo");
        assert_eq!(index_of_each("char"), 2);
        assert_eq!(
            each_options("char").len(),
            EACH.len(),
            "已识别的值不多出选项"
        );
    }

    /// ★ `each` 未识别时只改段名，原值原样写回；用户真去下拉里选了已知项才改。
    #[test]
    fn unknown_each_is_kept_unless_user_picks_another() {
        let ui = TooltipSectionsUi::new(vec![]);
        ui.seed(&[json!({ "label": "a", "template": "x", "each": "word" })]);
        let row = ui.rows.get()[0].clone();
        ui.form.open_edit(0, &row);
        let opts = ui.form.each_options.get();
        assert_eq!(opts.last().map(String::as_str), Some("word（未识别）"));
        assert_eq!(ui.form.each_idx.get(), opts.len() - 1, "默认指向原值那一项");
        assert_eq!(
            each_of_index(ui.form.each_idx.get(), &ui.form.each_raw.get()),
            "word",
            "不动下拉就原样写回"
        );
        assert_eq!(each_of_index(1, &ui.form.each_raw.get()), "han");
        // 新建表单不带「未识别」项。
        ui.form.open_new();
        assert_eq!(ui.form.each_options.get().len(), EACH.len());
    }

    /// ★ 出厂以 core 的 `getDefaults` 为准：快照与之不同时，恢复出厂与「是否出厂」都跟 core。
    #[test]
    fn factory_follows_core_defaults_over_snapshot() {
        let mut core_defaults = factory();
        core_defaults[3]["enabled"] = json!(true); // 设想系统预置把拆字段打开了
        let ui = TooltipSectionsUi::new(factory()); // 快照 = 纯 L1
        let d = core_defaults.clone();
        *ui.factory_source.borrow_mut() = Some(Box::new(move || Some(d.clone())));
        ui.seed(&core_defaults);
        ui.refresh_factory();
        assert!(ui.is_factory(), "与 core 口径相同就是出厂");
        ui.seed(&factory());
        assert!(!ui.is_factory(), "与快照相同不算");
        ui.reset_to_factory();
        assert_eq!(ui.to_value(), Value::Array(core_defaults));
    }

    /// core 取不到时保留快照；快照也空时「恢复出厂」不清空用户的列表。
    #[test]
    fn factory_falls_back_to_snapshot_and_never_clears() {
        let ui = TooltipSectionsUi::new(factory());
        *ui.factory_source.borrow_mut() = Some(Box::new(|| None));
        ui.refresh_factory();
        assert_eq!(*ui.factory.borrow(), factory());

        let empty = TooltipSectionsUi::new(vec![]);
        empty.seed(&factory());
        empty.reset_to_factory();
        assert_eq!(
            empty.rows.get().len(),
            factory().len(),
            "不知道出厂是什么就别动"
        );
    }

    #[test]
    fn dialog_builds() {
        let ui = TooltipSectionsUi::new(factory());
        ui.seed(&factory());
        let _ = build_tooltip_sections_dialog(&ui);
    }
}
