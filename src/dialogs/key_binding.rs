//! 按键绑定的共用件：键名显示、功能分类、按键选择对话框、绑定行渲染。
//!
//! 服务于四个编辑器实例——全局 `keys.key_actions` / `keys.session_actions`，以及方案级
//! `[key_actions]` / `[session_actions]`。它们此前各自用「两个下拉一行」的形态，问题有二：
//!
//! 1. **组合键表达不出来**。`Ctrl+Shift+R → 切换方案` 这类条目本就存在（方案设置里的
//!    「切换到本方案按键」写的就是它），却只能在一个「键名下拉」里当作生僻值兜住。
//! 2. **功能下拉长到不能用**。「切换方案（往返）」「切到方案（单向）」各按已安装方案展开
//!    一条，装了六个方案就是十二条，再加模式与状态切换，一条竖直下拉滚不到头。
//!
//! 改成 `[按键] [功能] [修改][删除]` 的行 + 一个共用的选择对话框后，两个问题一并消失：
//! 行只负责展示，选择在对话框里分两段（键形态 / 功能分类）做。
//!
//! # 值域为什么要按键形态过滤
//!
//! core 对这两张表的裁决**本来就分形态**，不是本模块新加的规矩：
//!
//! | 键形态 | 通路 | 能绑什么 |
//! |---|---|---|
//! | 组合键 | key_down 热键表 | 只有 `toggle_schema:` / `switch_schema:` / `special:` 三族（`hotkey_action_entry` 白名单） |
//! | 纯修饰键 | key_up 轻敲 | 全部 |
//! | 单个有字符的键 | keydown 引导键链 | 除「用来离开英文态」的那几个（`BoundAction::requires_modifier_key`） |
//!
//! 不过滤的后果是用户配得上、按下去没反应，且 core 只在日志里 warn 一句——正是本仓
//! 反复出现的那类缺陷。故过滤 + 一行说明指路，而不是让人试错一次才知道。
//!
//! # 与 `keys_overview` 共用什么
//!
//! 只共用 [`key_display`]。总览是只读视图，编辑仍回各自入口（见 `keys_overview` 的
//! 「为什么只读」）——共用一个显示函数不会把它变成第二个写回口。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use windui::core::EventCtx;
use windui::prelude::*;

/// 功能选项：`value` 落盘、`label` 给人看。
#[derive(Clone)]
pub struct Verb {
    pub value: String,
    pub label: String,
}

/// 一条绑定。`id` 是**行身份**：增删改都靠它定位。
///
/// 用单调 id 而不是行下标或键名：下标随增删错位；键名在「用户把两行配成同一个键」的
/// 中间态下不唯一，而那个中间态恰恰是要检出重复键的时候。
#[derive(Clone)]
pub struct Binding {
    pub id: u64,
    pub key: String,
    pub verb: String,
}

/// 行身份。见 [`Binding::id`]。
pub type RowId = u64;

/// 绑定编辑器对话框的尺寸。比通用外壳的 460×380 宽一档。
///
/// 本家族的行是 `[按键][功能][提示][修改][删除]` 五列，固定开销约 340px；460 下留给
/// 「功能」那一列的空间放不下「切到方案（单向） - 五笔拼音」这类标签。
///
/// **没有并用 `build_group_dialog_shell_large`（620×460）**：那个尺寸是给分了 TAB 的方案
/// 设置用的，内容本来就多；本家族多数时候只有三五行，撑到那么大只剩留白。
const DIALOG_W: i32 = 560;
const DIALOG_H: i32 = 420;

/// 内容区内边距。**全家族只在 [`build_binding_dialog`] 里加这一次**。
const PAD_X: i32 = 16;
const PAD_Y: i32 = 10;

/// 绑定编辑器的对话框主体：内容 + 底部附加按钮。
///
/// ★ 用 newtype 而不是裸 `(Element, Vec<Element>)`：五个调用点此前各自套壳，内边距长成了
/// 四种样子——全局按键那个**一点都没有**（内容没加、外壳也不加），方案那两个套了两层
/// （内容 16,8 ＋ 外壳 16,6 ＝ 32,14）。字段私有 ⇒ 内容出不了这个类型，半路再包一层就不
/// 可能了，`build_binding_dialog` 成为唯一出口。
pub struct BindingBody {
    content: Element,
    buttons: Vec<Element>,
}

impl BindingBody {
    pub fn new(content: Element, buttons: Vec<Element>) -> Self {
        Self { content, buttons }
    }
}

/// 绑定编辑器家族的统一外壳：四个编辑器（全局 / 方案 × 按键 / 输入中按键）+ 只读总览，
/// 共五处。尺寸、内边距、标题样式在此一处决定。
///
/// 标题一律取 `Signal<String>`（全局那两个是静态文案，`signal(...)` 包一下即可）：为两种
/// 标题各开一个入口，就等于给这五处又留了一条各走各的路。
pub fn build_binding_dialog(
    visible: Signal<bool>,
    title: Signal<String>,
    body: BindingBody,
) -> Element {
    crate::dialogs::field_dialogs::build_group_dialog_shell_dyn_sized(
        visible,
        title,
        Element::col()
            .width_match()
            .padding_xy(PAD_X, PAD_Y)
            .child(body.content),
        body.buttons,
        DIALOG_W,
        DIALOG_H,
    )
}

/// 「确定」的回调：`(被编辑行的 id 或 None（新增）, 键名, 功能值)`。
type CommitFn = Box<dyn Fn(Option<RowId>, String, String)>;

/// 这个编辑器服务于哪张表。两张表的**键值域**与**功能值域**都不同，见模块文档。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BindingTable {
    /// `key_actions`：给按键指定功能。修饰键任意组合，功能受键形态限制。
    Lead,
    /// `session_actions`：输入过程中的按键功能。**只认 `shift+` 前缀**
    /// （core `session_key_to_vk` 只剥这一种），故没有 Ctrl/Alt/Win。
    Session,
}

/// 按键的形态。core 按它分三条通路，见模块文档的表。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyShape {
    /// 单个有字符的键（`backtick` / `z` / `minus`…）。
    Char,
    /// 纯修饰键（`lshift` / `rctrl`…）。
    Modifier,
    /// 带 Ctrl/Alt/Shift/Win 的组合键。
    Combo,
}

/// 判定键形态。**只看键名字面**，与 core 的 `route_of_key_action` 同判据
/// （有修饰位 → 组合；裸修饰键 VK → 修饰键；其余 → 有字符的键）。
pub fn key_shape(name: &str) -> KeyShape {
    let n = name.trim().to_ascii_lowercase();
    if n.contains('+') {
        return KeyShape::Combo;
    }
    if matches!(
        n.as_str(),
        "lshift" | "rshift" | "lctrl" | "lcontrol" | "rctrl" | "rcontrol"
    ) {
        return KeyShape::Modifier;
    }
    KeyShape::Char
}

/// 该功能是否**只能绑修饰键**。镜像 core 的 `BoundAction::requires_modifier_key`。
///
/// 判据是「它是不是**从英文态出来**的手段」：是的话绑在有字符的键上就是单程票——
/// 那条 keydown 链在英文模式分水岭之后，切过去就再也按不动。
///
/// ⚠ 与 core 是**两份手写清单**，没有编译期约束。加功能时两处都要改。
pub fn verb_requires_modifier(value: &str) -> bool {
    value.starts_with("toggle_schema:")
        || value.starts_with("switch_schema:")
        || matches!(value, "toggle_mode" | "switch_engine")
}

/// 这个功能能不能绑在这个键形态上（`Lead` 表用；`Session` 表没有此限制）。
pub fn verb_allowed(shape: KeyShape, value: &str) -> bool {
    match shape {
        // 组合键走 key_down 热键表，动词受 core `hotkey_action_entry` 白名单限制。
        // ⚠ 与那张白名单是两份手写清单，加动词时两处都要改。
        //
        // ★ `softkeyboard` 曾漏在这里，而它**是出厂配置**（`data/config.toml` 的
        // `key_actions = { "ctrl+shift+k" = "softkeyboard" }`）⇒ 全新安装打开这个对话框，
        // 出厂那条就红着「组合键不支持」。漂移的方向恰是这条注释预言的那种，只是没人
        // 在两处都看一眼——2026-08-31 由只读组的截图自查撞出来。守门测试见
        // `combo_verbs_match_core_whitelist`。
        KeyShape::Combo => {
            value.starts_with("toggle_schema:")
                || value.starts_with("switch_schema:")
                || value.starts_with("special:")
                || value == "softkeyboard"
                || value.starts_with("softkeyboard:")
        }
        KeyShape::Char => !verb_requires_modifier(value),
        KeyShape::Modifier => true,
    }
}

/// 「辅助码触发键绑在反引号上」的专属提示，**两张表共用**。
///
/// 全拼方案出厂 `separator = "auto"` 且 `'` 作选词键 ⇒ 反引号恒为音节分隔符，绑在它上面
/// 走不到 `enter_aux_code`。core 那边只 warn 一句，用户看到的是「配了完全没反应」。
///
/// 两种形态的后果不同，文案也不同：专用触发键是**彻底没反应**（门卫拒绝后该键落回分隔符），
/// 共键则会降级成翻页 ⇒ 用户得到的是一个翻页键，而不是他要的辅助码。
///
/// 这条不放进 `verb_allowed` 过滤：它只对拼音方案成立，而这个对话框也服务码表方案，
/// 一刀切会挡掉合法配置。
///
/// ⛔ 文案一律写成**单行字面量**：反斜杠续行被 rustfmt 接回一行时，被吞掉的缩进会变成
/// 字面空格，用户看到的提示中间夹一大段空白。
fn aux_code_backtick_notice(verb: &str, key: &str) -> Option<String> {
    if !matches!(key, "backtick" | "grave" | "`") {
        return None;
    }
    match verb {
        "aux_code" => Some(
            "全拼方案的反引号被音节分隔符占用，绑在它上面进不了辅助码，请换一个键。".to_string(),
        ),
        "aux_code:page_next" => Some(
            "全拼方案的反引号被音节分隔符占用，绑在它上面进不了辅助码，只会当成下翻页键。"
                .to_string(),
        ),
        _ => None,
    }
}

// ───────────────────────────── 键名显示 ─────────────────────────────

/// 键名 → 给人看的文本。`rbracket` → `]`，`ctrl+shift+rbracket` → `Ctrl + Shift + ]`。
///
/// ★ 与 [`key_choice_label`] 分工：**这个用于展示**（列表里一眼认出是哪个键），那个用于
/// **选择**（下拉里要能把 `'` 和 `` ` `` 分辨开）。同一个函数干两件事的结果，是列表里挤着
/// 「右方括号 ( ] )」这样的长串，或者下拉里并排两个看不出差别的引号。
pub fn key_display(name: &str) -> String {
    let n = name.trim();
    if n.is_empty() {
        return "未设置".to_string();
    }
    n.split('+')
        .map(|p| key_part_display(p.trim()))
        .collect::<Vec<_>>()
        .join(" + ")
}

fn key_part_display(part: &str) -> String {
    let k = part.to_ascii_lowercase();
    let fixed = match k.as_str() {
        "ctrl" | "control" => "Ctrl",
        "alt" => "Alt",
        "shift" => "Shift",
        "win" | "super" | "meta" | "cmd" => "Win",
        // 符号键：显示符号本身。配置里用的是规范名（`rbracket`），但手写配置也可能
        // 直接写字面符号——core 的 `KEY_TABLE` 两种都认，故这里也两种都收。
        "backtick" | "grave" | "`" => "`",
        "semicolon" | ";" => ";",
        "quote" | "'" => "'",
        "comma" | "," => ",",
        "period" | "." => ".",
        "slash" | "/" => "/",
        "backslash" | "\\" => "\\",
        "lbracket" | "[" => "[",
        "rbracket" | "]" => "]",
        "minus" | "hyphen" | "-" => "-",
        "equal" | "equals" | "=" => "=",
        "lshift" => "左 Shift",
        "rshift" => "右 Shift",
        "lctrl" | "lcontrol" => "左 Ctrl",
        "rctrl" | "rcontrol" => "右 Ctrl",
        "lalt" | "lmenu" => "左 Alt",
        "ralt" | "rmenu" => "右 Alt",
        "capslock" | "caps" => "CapsLock",
        "space" => "Space",
        "return" | "enter" => "Enter",
        "escape" | "esc" => "Esc",
        "backspace" | "back" => "Backspace",
        "delete" | "del" => "Delete",
        "insert" | "ins" => "Insert",
        "tab" => "Tab",
        "home" => "Home",
        "end" => "End",
        "pageup" | "pgup" | "prior" => "PageUp",
        "pagedown" | "pgdn" | "next" => "PageDown",
        "up" => "↑",
        "down" => "↓",
        "left" => "←",
        "right" => "→",
        _ => "",
    };
    if !fixed.is_empty() {
        return fixed.to_string();
    }
    // F 键与单字母大写；其余原样（手写配置里本版本不认识的键名，显示原值不显示「未知」
    // ——它在 core 里可能跑得好好的，同 `keys_overview::action_label` 的兜底策略）。
    if let Some(n) = k.strip_prefix('f')
        && n.parse::<u8>().is_ok_and(|n| (1..=24).contains(&n))
    {
        return format!("F{n}");
    }
    if k.len() == 1 && k.as_bytes()[0].is_ascii_alphabetic() {
        return k.to_ascii_uppercase();
    }
    part.to_string()
}

/// 键名 → 下拉里的**可辨识**标签。符号键带中文名，其余同 [`key_display`]。
pub fn key_choice_label(name: &str) -> String {
    let named = match name {
        "backtick" => "反引号 ( ` )",
        "semicolon" => "分号 ( ; )",
        "quote" => "单引号 ( ' )",
        "comma" => "逗号 ( , )",
        "period" => "句号 ( . )",
        "slash" => "斜杠 ( / )",
        "backslash" => "反斜杠 ( \\ )",
        "lbracket" => "左方括号 ( [ )",
        "rbracket" => "右方括号 ( ] )",
        "minus" => "减号 ( - )",
        "equal" => "等号 ( = )",
        "z" => "字母 z",
        _ => "",
    };
    if named.is_empty() {
        key_display(name)
    } else {
        named.to_string()
    }
}

// ───────────────────────────── 功能分类 ─────────────────────────────

/// 一个功能分类。
#[derive(Clone)]
pub struct VerbCategory {
    pub name: String,
    pub items: Vec<Verb>,
    /// 选中本类即禁用「确定」，并显示 [`Self::notice`]。给「预留但未开放」的项用。
    pub locked: bool,
    pub notice: String,
}

impl VerbCategory {
    fn new(name: &str, items: Vec<Verb>) -> Self {
        Self {
            name: name.to_string(),
            items,
            locked: false,
            notice: String::new(),
        }
    }
}

/// 预留项的值。**不能用空串**：`refresh` 靠「当前功能值」恢复选中项，空值恒查不到，
/// 于是每次重算都把选择弹回第一个分类——那一类点开就跳走，里面写了什么都看不到。
///
/// 反过来也要保证它绝不落盘：出口只有 [`KeyPicker::commit_value`]，它对 `locked` 分类
/// 一律返回 `None`（「确定」也据同一判据置灰）。
const RESERVED_VERB: &str = "__reserved__";

/// 「高级」分类：当前只有一个占位项。
///
/// 露出而不实现，是为了让「一个键能不能调外部程序」这个问题在界面上有答案。整类 `locked`
/// ——windui 的 `DropdownItem` 没有禁用位，逐项置灰做不到，故把「不可用」提到分类这一层：
/// 选中它就显示说明并禁用「确定」，效果与置灰一致且不需要改控件。
fn advanced_category() -> VerbCategory {
    VerbCategory {
        name: "高级".to_string(),
        items: vec![Verb {
            value: RESERVED_VERB.to_string(),
            label: "调用直通命令（暂未开放）".to_string(),
        }],
        locked: true,
        notice: "「调用直通命令」（按一个键运行外部程序或内部指令）尚未开放，将在后续版本提供。"
            .to_string(),
    }
}

/// 把扁平功能表按**值的形态**分类。
///
/// ★ 只认 `value` 前缀，不认 `label`：label 里的方案名是用户起的，随时会改，
/// 用它分类等于把用户输入当协议。
pub fn categorize_lead_verbs(verbs: &[Verb]) -> Vec<VerbCategory> {
    let (mut disable, mut modes, mut schemas) = (vec![], vec![], vec![]);
    let (mut states, mut tools, mut other) = (vec![], vec![], vec![]);
    for v in verbs {
        let bucket = match v.value.as_str() {
            "none" => &mut disable,
            "temp_pinyin" | "temp_english" | "aux_code" => &mut modes,
            "toggle_mode" | "switch_engine" | "toggle_full_width" | "toggle_punct"
            | "toggle_s2t" | "toggle_toolbar" => &mut states,
            "open_settings" | "take_screenshot" | "softkeyboard" => &mut tools,
            s if s.starts_with("mix:") || s.starts_with("special:") => &mut modes,
            s if s.starts_with("toggle_schema:") || s.starts_with("switch_schema:") => &mut schemas,
            // 本版本不认识的动词（手写配置 / 更新前的旧配置）。不丢弃：丢弃等于用户
            // 打开一次设置页就静默失去配置，与两个编辑器的 seed 同策略。
            _ => &mut other,
        };
        bucket.push(v.clone());
    }
    let mut out = vec![
        VerbCategory::new("不启用", disable),
        VerbCategory::new("输入模式", modes),
        VerbCategory::new("切换方案", schemas),
        VerbCategory::new("状态切换", states),
        VerbCategory::new("工具", tools),
        VerbCategory::new("其它", other),
    ];
    out.retain(|c| !c.items.is_empty());
    out.push(advanced_category());
    out
}

/// 序号动词的中文名——**措辞的单一出处**。
///
/// 选择器（[`session_verb_categories`]）与读取侧（[`session_verb_label`]）都从这里取：
/// 两边各自抄一份的话，改了措辞只改一处，表现是同一条绑定在选择对话框里叫一个名、在按键
/// 总览里叫另一个名。本仓在键名显示上已经栽过一次同形的问题。
fn ordinal_label(prefix: &str, n: u8) -> Option<String> {
    match prefix {
        "select_candidate:" => Some(format!("选中第 {n} 个候选")),
        "select_char:" => Some(format!("以词定字：取第 {n} 字")),
        _ => None,
    }
}

/// 会话态（`session_actions`）的功能分类。
///
/// 两个序号档的上限**不同**，这个不同是有意的：
///
/// - 选词 `select_candidate:` 给全 1..=9——一页真有 9 个候选。此前只给 2、3 是因为
///   「全列出来会把下拉撑成一长条」，分了类之后这个理由不成立，而截断的代价是「想把某个
///   键配成第 5 候选」在界面上根本表达不出来。
/// - 以词定字 `select_char:` 只到 4。core 的 `SessionAction::parse` 认 1..=9，但那是
///   **解析器的宽容度**（与 `select_candidate:` 共用同一句 `Ok(n @ 1..=9)` 守卫），不是
///   被设计过的能力：三个既有键组 `comma_period` / `minus_equal` / `brackets` 经
///   `select_char_group_binds` 折算出的清一色只有 `SelectChar(1)` 与 `(2)`——以词定字在
///   配置层面从来就是「取第一字 / 取第二字」这一对。
///
/// 放到 4 而不是照搬那 2 档：键组只给两档是**键位预算**所致（一组就两个键），不是语义
/// 上限，而逐键指定不受这个约束，四字成语取第 3/4 字是真实场景（「守株待兔」取「待」）。
/// 再往上则不然——`handle_select_char` 在 `char_index >= 词长` 时返回 None 转 overflow
/// 策略，而中文候选绝大多数 2-4 字，配「取第 7 字」按下去**恒定**走 overflow，与「这个
/// 键根本没绑上」完全同形，用户无从分辨。
///
/// ⚠️ 收窄的只是**写入侧**（选择器给不给配）。读取侧不能跟着收：用户手写的
/// `select_char:7` core 照样执行，按键总览与值域守门仍按 core 的 1..=9 认全，否则那条
/// 绑定会落到「显示原值」兜底上，总览里出现一行 `select_char:7`。
pub fn session_verb_categories() -> Vec<VerbCategory> {
    let ord = |prefix: &str, max: u8| -> Vec<Verb> {
        (1..=max)
            .map(|n| Verb {
                value: format!("{prefix}{n}"),
                label: ordinal_label(prefix, n).expect("序号前缀已在 ordinal_label 里登记"),
            })
            .collect()
    };
    vec![
        VerbCategory::new(
            "不启用",
            vec![Verb {
                value: "none".into(),
                label: "输入中禁用该键".into(),
            }],
        ),
        VerbCategory::new(
            "候选导航",
            ["page_prev", "page_next", "highlight_up", "highlight_down"]
                .iter()
                .zip(["上一页", "下一页", "高亮上移", "高亮下移"])
                .map(|(v, l)| Verb {
                    value: (*v).to_string(),
                    label: l.to_string(),
                })
                .collect(),
        ),
        VerbCategory::new("选择候选", ord("select_candidate:", 9)),
        VerbCategory::new("以词定字", ord("select_char:", 4)),
        VerbCategory::new(
            "编辑",
            vec![Verb {
                value: "cancel".into(),
                label: "放弃当前输入（同 Esc）".into(),
            }],
        ),
        // 辅助码同时在**两张表**里：`key_actions` 那份是既有的（双拼出厂
        // `backtick = "aux_code"`），本表这份让 Tab / PageDown / 方向键那一批也绑得上——
        // `key_action_name_to_vk` 解析不出它们，写进 `key_actions` 会被静默丢弃。
        //
        // 它落在本表是语义上的归位：`enter_aux_code` 第一件事就是查候选是否为空，
        // 而「只在有候选时才有意义」正是本表的定义性质。
        // `aux_code:page_next` 是 `aux_code` 的**参数**（与下翻页共用一个键），不是另一个
        // 动词：能进辅助码就进、进不去就翻下一页。只有本表有这个形态——`key_actions` 解析
        // 不出翻页键，那张表里也就不存在「与翻页共键」这回事。
        VerbCategory::new(
            "候选筛选",
            vec![
                Verb {
                    value: "aux_code".into(),
                    label: "辅助码筛选（按字形码二次筛选）".into(),
                },
                Verb {
                    value: "aux_code:page_next".into(),
                    label: "辅助码筛选（进不去时翻下一页）".into(),
                },
            ],
        ),
        advanced_category(),
    ]
}

/// 会话态功能值 → 中文名（供只读视图查）。未知值返回 `None`，由调用方兜底显示原值。
pub fn session_verb_label(value: &str) -> Option<String> {
    if let Some(label) = session_verb_categories()
        .iter()
        .flat_map(|c| c.items.iter())
        .find(|v| v.value == value)
        .map(|v| v.label.clone())
    {
        return Some(label);
    }
    // ★★ 序号动词**不能只查分类表**：那张表是**选择器**的值域（以词定字只给到第 4 字，
    // 见 `session_verb_categories`），而本函数服务的是**读取侧**——按键总览要显示用户手写
    // 的、core 照样执行的 `select_char:7`。只查分类表的话它翻不出名字，落到「显示原值」
    // 兜底上，总览里出现一行 `select_char:7`；同 `switch_schema:wubi86` 那次（见
    // `keys_overview` 的 `global_only_verbs_are_named`）。
    //
    // 写入侧要窄（别让人配出按下去没反应的档位），读取侧要宽（core 认什么就得显示得出
    // 什么）——两者要求相反，故值域在此**独立**按 core 的 1..=9 判，不复用分类表。
    for prefix in ["select_candidate:", "select_char:"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            return rest
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|n| (1..=9).contains(n))
                .and_then(|n| ordinal_label(prefix, n));
        }
    }
    None
}

// ───────────────────────────── 键值域 ─────────────────────────────

/// 可作**独立绑定**的单个键（不带修饰）。
fn single_keys(table: BindingTable) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![(String::new(), "（请选择）".to_string())];
    let push = |out: &mut Vec<(String, String)>, k: &str| {
        out.push((k.to_string(), key_choice_label(k)));
    };
    match table {
        BindingTable::Lead => {
            // 符号键取自 `TRIGGER_KEY_OPTIONS`——它是本仓「哪些符号键可以绑功能」的
            // 单一真相源（引导键多选那个对话框也读它）。这里再抄一份的话，那张表增删时
            // 本处不会跟着改，表现是新键在引导键里选得到、在按键自定义里选不到。
            //
            // 只取**值**，标签一律走 `key_choice_label`：显示怎么写是另一件事，
            // 让它也有单一出处，免得同一个键在两个下拉里长得不一样。
            for (k, _) in crate::dialogs::schema_manager::TRIGGER_KEY_OPTIONS {
                push(&mut out, k);
            }
            for k in [
                "minus", "equal",
                // 字母只列 z：其余字母在任何码表里几乎都是活码前缀，core 的活码裁决
                // 必然让位，配了等于没配。列出来只会让人配完发现没反应。
                "z",
                // 与 core `keymap::modifier_name_to_vk` 的规范名一一对应。
                "lshift", "rshift", "lctrl", "rctrl",
            ] {
                push(&mut out, k);
            }
        }
        BindingTable::Session => {
            for k in session_base_keys() {
                push(&mut out, k);
            }
        }
    }
    out
}

/// 会话态表认的基础键。与 core `session_key_to_vk` 的 match 臂一一对应。
///
/// 别名（`pgup` / `caps` / `lcontrol`…）**不列**：与主名同义，列出来只会让人以为是
/// 不同的键。手写配置里用了别名时由「当前值」机制兜住，不丢配置。
///
/// **CapsLock 仅 Windows**：拦住它要低级键盘钩子，core 的 `capslock_hook` 在非 Windows
/// 是 `bail!` 的空壳，列出来就是配一个永远不生效的键。
fn session_base_keys() -> &'static [&'static str] {
    #[cfg(not(windows))]
    {
        &[
            "tab",
            "pageup",
            "pagedown",
            "up",
            "down",
            "left",
            "right",
            "home",
            "end",
            "lshift",
            "rshift",
            "lctrl",
            "rctrl",
            "minus",
            "equal",
            "lbracket",
            "rbracket",
            "comma",
            "period",
            "semicolon",
            "quote",
            "slash",
            "backslash",
            "backtick",
            "z",
        ]
    }
    #[cfg(windows)]
    {
        &[
            "tab",
            "capslock",
            "pageup",
            "pagedown",
            "up",
            "down",
            "left",
            "right",
            "home",
            "end",
            "lshift",
            "rshift",
            "lctrl",
            "rctrl",
            "minus",
            "equal",
            "lbracket",
            "rbracket",
            "comma",
            "period",
            "semicolon",
            "quote",
            "slash",
            "backslash",
            "backtick",
            "z",
        ]
    }
}

/// 组合键的基础键（修饰键之外的那一个）。
fn combo_base_keys(table: BindingTable) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![(String::new(), "（请选择）".to_string())];
    match table {
        // Lead 表的组合键走 `parse_hotkey`，值域与「功能快捷键」那套一致，故直接复用
        // 同一份主键表——两处各写一份的结果，是同一个应用里两个热键编辑器能选的键不一样。
        // 首项「（未设置）」在那边表示解绑，这边由 `single_keys` 的空项承担，故跳过。
        BindingTable::Lead => {
            for (v, l) in crate::manifest::hotkey_keys() {
                if !v.is_empty() {
                    out.push((v, l));
                }
            }
        }
        // Session 表的组合只有 `shift+<基础键>`，基础键值域与单键相同。
        BindingTable::Session => {
            for k in session_base_keys() {
                out.push((k.to_string(), key_choice_label(k)));
            }
        }
    }
    out
}

// ───────────────────────────── 选择对话框 ─────────────────────────────

struct PickerData {
    table: BindingTable,
    /// 两个键下拉的**取值**表（显示项在 `KeyPicker::{single,combo}_labels`，须是信号：
    /// `open_edit` 会为手写配置里的生僻键追加选项，构建时捕获一份 Vec 的话下拉会永远
    /// 停在初始选项集上——而那意味着用户点开「修改」看到的是「（请选择）」，
    /// 自己配的那个键像是凭空消失了）。
    single: RefCell<Vec<String>>,
    combo: RefCell<Vec<String>>,
    /// 全量分类（由调用方 `set_verbs` 灌入；Session 表恒为固定表）。
    all: RefCell<Vec<VerbCategory>>,
    /// 当前按键形态过滤后的分类，与 `cat_labels` / `item_labels` 同序。
    view: RefCell<Vec<VerbCategory>>,
    /// 本方案首码集里的符号键（core `getConfig` 的 `leadingCodeKeys` 旁路字段）。
    leading: RefCell<Vec<String>>,
    editing: Cell<Option<RowId>>,
    on_commit: RefCell<Option<CommitFn>>,
}

/// 按键选择对话框。四个编辑器各持一个实例。
///
/// 为什么不做成全应用一个共享实例：对话框元素要挂进**它所属的那棵浮层栈**才叠得对
/// （全局两个挂 `fields.dialogs`，方案两个挂方案管理器的 `stack`），共享实例就得处理
/// 「同一个 Element 挂两处」，而 windui 的 Element 是值语义、挂两处等于两份互不相干的
/// 状态——比多持有一份工作态更难查。
#[derive(Clone)]
pub struct KeyPicker {
    pub visible: Signal<bool>,
    title: Signal<String>,
    /// 0 = 单个按键，1 = 组合键
    mode: Signal<usize>,
    /// ctrl / shift / alt / win，顺序同 `manifest::compose_hotkey`
    mods: [Signal<bool>; 4],
    single_idx: Signal<usize>,
    combo_idx: Signal<usize>,
    cat_idx: Signal<usize>,
    item_idx: Signal<usize>,
    single_labels: Signal<Vec<String>>,
    combo_labels: Signal<Vec<String>>,
    cat_labels: Signal<Vec<String>>,
    item_labels: Signal<Vec<String>>,
    preview: Signal<String>,
    notice: Signal<String>,
    can_confirm: Signal<bool>,
    data: Rc<PickerData>,
}

impl KeyPicker {
    pub fn new(table: BindingTable) -> Self {
        let all = match table {
            BindingTable::Lead => Vec::new(), // 由 set_verbs 灌入（实例类动词随方案变）
            BindingTable::Session => session_verb_categories(),
        };
        let (single, combo) = (single_keys(table), combo_base_keys(table));
        let me = Self {
            visible: signal(false),
            title: signal("添加按键".to_string()),
            mode: signal(0),
            mods: [signal(false), signal(false), signal(false), signal(false)],
            single_idx: signal(0),
            combo_idx: signal(0),
            cat_idx: signal(0),
            item_idx: signal(0),
            single_labels: signal(single.iter().map(|(_, l)| l.clone()).collect()),
            combo_labels: signal(combo.iter().map(|(_, l)| l.clone()).collect()),
            cat_labels: signal(Vec::new()),
            item_labels: signal(Vec::new()),
            preview: signal("未设置".to_string()),
            notice: signal(String::new()),
            can_confirm: signal(false),
            data: Rc::new(PickerData {
                table,
                single: RefCell::new(single.into_iter().map(|(v, _)| v).collect()),
                combo: RefCell::new(combo.into_iter().map(|(v, _)| v).collect()),
                all: RefCell::new(all),
                view: RefCell::new(Vec::new()),
                leading: RefCell::new(Vec::new()),
                editing: Cell::new(None),
                on_commit: RefCell::new(None),
            }),
        };
        me.refresh();
        me
    }

    /// 灌入功能表（`Lead` 用；实例类动词随已安装方案增删，故每次打开设置都重算）。
    pub fn set_verbs(&self, verbs: &[Verb]) {
        *self.data.all.borrow_mut() = categorize_lead_verbs(verbs);
        self.refresh();
    }

    /// 本方案首码集里的符号键，用于「绑了这个键，该符号在本方案就起不了头」提示。
    pub fn set_leading_keys(&self, keys: Vec<String>) {
        *self.data.leading.borrow_mut() = keys;
    }

    /// 「确定」时的回调：`(被编辑行的 id 或 None（新增）, 键名, 功能值)`。
    pub fn on_commit(&self, f: impl Fn(Option<RowId>, String, String) + 'static) {
        *self.data.on_commit.borrow_mut() = Some(Box::new(f));
    }

    /// 打开「添加」。
    pub fn open_new(&self) {
        self.data.editing.set(None);
        self.title.set("添加按键".to_string());
        self.mode.set(0);
        for m in &self.mods {
            m.set(false);
        }
        self.single_idx.set(0);
        self.combo_idx.set(0);
        self.cat_idx.set(0);
        self.item_idx.set(0);
        self.refresh();
        self.visible.set(true);
    }

    /// 打开「修改」，把已有绑定回填进各控件。
    pub fn open_edit(&self, id: RowId, key: &str, verb: &str) {
        self.data.editing.set(Some(id));
        self.title.set("修改按键".to_string());

        let (mods, base) = crate::manifest::parse_hotkey(key);
        if mods.iter().any(|b| *b) {
            self.mode.set(1);
            for (i, m) in self.mods.iter().enumerate() {
                m.set(mods[i]);
            }
            self.combo_idx
                .set(key_index(&self.data.combo, self.combo_labels, &base));
            self.single_idx.set(0);
        } else {
            self.mode.set(0);
            for m in &self.mods {
                m.set(false);
            }
            self.single_idx
                .set(key_index(&self.data.single, self.single_labels, key));
            self.combo_idx.set(0);
        }
        self.select_verb(verb);
        self.refresh();
        // refresh 会按键形态过滤，可能把当前值筛掉（历史配置本就可能是非法组合）。
        // 过滤后再选一次，让「修改」对话框至少显示得出用户现在配的是什么。
        self.select_verb(verb);
        self.visible.set(true);
    }

    /// 把 `value` 选中（找不到则在「其它」类里补一条「当前值」，不丢配置）。
    fn select_verb(&self, value: &str) {
        if let Some((c, i)) = find_verb(&self.data.view.borrow(), value) {
            self.cat_idx.set(c);
            self.item_idx.set(i);
            return;
        }
        if find_verb(&self.data.all.borrow(), value).is_none() && !value.is_empty() {
            let mut all = self.data.all.borrow_mut();
            let label = format!("{value}（当前值）");
            match all.iter_mut().find(|c| c.name == "其它") {
                Some(c) => c.items.push(Verb {
                    value: value.to_string(),
                    label,
                }),
                None => {
                    // 「高级」恒为末类，「其它」插在它之前，保持分类顺序稳定。
                    let at = all.len().saturating_sub(1);
                    all.insert(
                        at,
                        VerbCategory::new(
                            "其它",
                            vec![Verb {
                                value: value.to_string(),
                                label,
                            }],
                        ),
                    );
                }
            }
        }
    }

    /// 当前选中的键名（空串 = 还没选全）。
    fn current_key(&self) -> String {
        if self.mode.get() == 0 {
            return self
                .data
                .single
                .borrow()
                .get(self.single_idx.get())
                .cloned()
                .unwrap_or_default();
        }
        let base = self
            .data
            .combo
            .borrow()
            .get(self.combo_idx.get())
            .cloned()
            .unwrap_or_default();
        if base.is_empty() {
            return String::new();
        }
        let m = [
            self.mods[0].get(),
            self.mods[1].get(),
            self.mods[2].get(),
            self.mods[3].get(),
        ];
        // 一个修饰键都没勾 = 还不是组合键。此时不回落成裸键：那会让「组合键」这一档
        // 悄悄产出一个单键绑定，而单键有自己的值域（比如 Lead 表的组合档能选任意字母，
        // 单键档只给 z）——回落等于绕过单键那份值域限制。
        if !m.iter().any(|b| *b) {
            return String::new();
        }
        crate::manifest::compose_hotkey(m, &base)
    }

    /// 当前选中的功能值（空串 = 没有可选项 / 选中的是占位项）。
    fn current_verb(&self) -> String {
        self.data
            .view
            .borrow()
            .get(self.cat_idx.get())
            .and_then(|c| c.items.get(self.item_idx.get()))
            .map(|v| v.value.clone())
            .unwrap_or_default()
    }

    /// 当前选中的分类是否为预留类（选中即不可提交）。
    fn locked_now(&self) -> bool {
        self.data
            .view
            .borrow()
            .get(self.cat_idx.get())
            .is_some_and(|c| c.locked)
    }

    /// 「确定」要落下去的 `(键名, 功能值)`；不可提交时返回 `None`。
    ///
    /// **提交的唯一出口**。按钮的置灰（`can_confirm`）与这里用的是同一条判据——分两处
    /// 各写一遍的话，只要哪天某条路径绕过了置灰（键盘回车、脚本、测试），预留项就会
    /// 真的写进配置。这类「两处必须一致」的约束本仓已栽过四次。
    fn commit_value(&self) -> Option<(String, String)> {
        if self.locked_now() {
            return None;
        }
        let (key, verb) = (self.current_key(), self.current_verb());
        (!key.is_empty() && !verb.is_empty() && verb != RESERVED_VERB).then_some((key, verb))
    }

    /// 按当前按键形态重算可选功能、预览与提示。
    ///
    /// 幂等：第二次跑不会再改任何信号（选中项此时已合法），故挂在 `reactive_effect` 上
    /// 不会自激——effect 比对的是各信号版本，而稳定后版本不再变。
    fn refresh(&self) {
        let key = self.current_key();
        let keep = self.current_verb();
        // 分类名也要记：换分类时 `item_idx` 还停在上一类的位置，多半已越界，于是
        // `current_verb()` 取不到值。只按值恢复的话这时会双双回落 (0,0)——表现是
        // **点哪个分类都跳回第一个**，高级类那种单项分类尤其明显（永远打不开）。
        let keep_cat = self
            .data
            .view
            .borrow()
            .get(self.cat_idx.get())
            .map(|c| c.name.clone())
            .unwrap_or_default();

        // ── 过滤 ──
        let view: Vec<VerbCategory> = if key.is_empty() || self.data.table == BindingTable::Session
        {
            self.data.all.borrow().clone()
        } else {
            let shape = key_shape(&key);
            let mut v: Vec<VerbCategory> = self
                .data
                .all
                .borrow()
                .iter()
                .map(|c| {
                    let mut c = c.clone();
                    if !c.locked {
                        c.items.retain(|it| verb_allowed(shape, &it.value));
                    }
                    c
                })
                .collect();
            v.retain(|c| !c.items.is_empty());
            v
        };
        *self.data.view.borrow_mut() = view;

        // ── 选中项：先按值保、再按分类保 ──
        //
        // 两级兜底对应两种情形：值还在（换了键但这个功能仍可选）→ 跟着它走；值没了
        // （刚换分类，或该功能被键形态滤掉）→ 至少留在用户点的那个分类里，选它的第一项。
        let (c, i) = find_verb(&self.data.view.borrow(), &keep).unwrap_or_else(|| {
            let c = self
                .data
                .view
                .borrow()
                .iter()
                .position(|x| x.name == keep_cat)
                .unwrap_or(0);
            (c, 0)
        });
        set_if_changed(self.cat_idx, c);
        set_if_changed(self.item_idx, i);

        let cats: Vec<String> = self
            .data
            .view
            .borrow()
            .iter()
            .map(|c| c.name.clone())
            .collect();
        let items: Vec<String> = self
            .data
            .view
            .borrow()
            .get(self.cat_idx.get())
            .map(|c| c.items.iter().map(|v| v.label.clone()).collect())
            .unwrap_or_default();
        set_vec_if_changed(self.cat_labels, cats);
        set_vec_if_changed(self.item_labels, items);

        // ── 预览 / 提示 / 确定可用性 ──
        set_str_if_changed(self.preview, key_display(&key));
        // 「这次滤掉东西了吗」必须在这里算：`notice_text` 只看得到过滤**之后**的 view，
        // 无从判断少了什么。不算的话，用户选个符号键、「切换方案」整类无声消失，界面上
        // 没有任何一处说得出原因——那正是这轮改动本想消灭的「配不出来还不知道为什么」。
        let filtered = self.filtered_count() > 0;
        set_str_if_changed(
            self.notice,
            self.notice_text(&key, self.locked_now(), filtered),
        );
        set_if_changed_bool(self.can_confirm, self.commit_value().is_some());
    }

    /// 按当前键形态被滤掉的功能条数（不含 `locked` 类，它从不参与过滤）。
    fn filtered_count(&self) -> usize {
        let live = |cats: &[VerbCategory]| -> usize {
            cats.iter()
                .filter(|c| !c.locked)
                .map(|c| c.items.len())
                .sum()
        };
        live(&self.data.all.borrow()).saturating_sub(live(&self.data.view.borrow()))
    }

    fn notice_text(&self, key: &str, locked: bool, filtered: bool) -> String {
        if locked {
            return self
                .data
                .view
                .borrow()
                .get(self.cat_idx.get())
                .map(|c| c.notice.clone())
                .unwrap_or_default();
        }
        if key.is_empty() {
            return match self.mode.get() {
                1 => "组合键需要至少一个修饰键和一个基础键。".to_string(),
                _ => "请先选择按键。".to_string(),
            };
        }
        match self.data.table {
            BindingTable::Session => {
                if key.starts_with("capslock") || key == "caps" {
                    return "CapsLock 需要全局键盘钩子才能拦住，非 Windows 平台不生效。"
                        .to_string();
                }
                // 辅助码收编进本表后这条一直没跟过来——而「在会话表里把反引号配成 aux_code」
                // 正是最自然的配法。
                if let Some(n) = aux_code_backtick_notice(&self.current_verb(), key) {
                    return n;
                }
                String::new()
            }
            BindingTable::Lead => {
                // 辅助码专属：全拼方案出厂 `separator = "auto"`，反引号恒为音节分隔符，
                // 而分隔符臂在按键 match 里排在 `key_actions` 裁决**之前**——绑在它上面
                // 根本走不到 `enter_aux_code`。core 那边只 warn 一句，用户看到的是
                // 「配了完全没反应」。这条不放进 `verb_allowed` 过滤：它只对拼音方案成立，
                // 而这个对话框也服务码表方案，一刀切会挡掉合法配置。
                if let Some(n) = aux_code_backtick_notice(&self.current_verb(), key) {
                    return n;
                }
                // ★ 说明**只在真的滤掉了东西时**才出。恒挂就成了常驻噪音，而它要传达的
                // 恰恰是「你现在少看到了几项」这个一次性的事实。
                //
                // ⛔ 这几条一律写成**单行字面量**，不用反斜杠续行：rustfmt 会把续行接回一行，
                // 被反斜杠吞掉的缩进随之变成**字面空格**，用户看到的提示中间夹一大段空白。
                // 上面辅助码那条就是这么中招的（已修）。
                match key_shape(key) {
                    KeyShape::Combo if filtered => {
                        "组合键只能绑「切换方案」与「特殊模式」。其它功能请改用单个按键，或到「按键 → 功能快捷键」里配。"
                            .to_string()
                    }
                    // 有字符的键滤掉的是「用来离开英文态」的那几个。此前这一支恒返回空串
                    // ⇒ 选个符号键，「切换方案」「状态切换」两整类无声消失，无处可查原因。
                    KeyShape::Char if filtered => {
                        "「切换方案」「中英文切换」「循环切换方案」不能绑在有字符的键上——那条通路在英文模式下到不了，切过去就按不回来。请改用左右 Shift / Ctrl，或组合键。"
                            .to_string()
                    }
                    KeyShape::Char => {
                        if self.data.leading.borrow().iter().any(|l| l == key) {
                            // 同层冲突，两条配置都不算坏：内核裁决绑定优先，该符号在本方案
                            // 起不了头（组码中仍可用）。故用中性措辞告知实际行为，不标红。
                            "该键是本方案的码元首码：绑定后它在本方案起不了头（组码中仍可用）。"
                                .to_string()
                        } else {
                            String::new()
                        }
                    }
                    KeyShape::Combo | KeyShape::Modifier => String::new(),
                }
            }
        }
    }

    /// 对话框元素。**必须**挂在比它的宿主编辑器更靠后的位置，否则会被压在下面。
    pub fn build_dialog(&self) -> Element {
        let me = self.clone();
        let [mc, ms, ma, mw] = self.mods;
        let table = self.data.table;

        let mode = self.mode;
        let mode_row = Element::row().width_match().child(
            Element::segmented(vec!["单个按键".to_string(), "组合键".to_string()], mode)
                .weight(1.0),
        );

        // Session 表只认 `shift+` 前缀（core `session_key_to_vk` 只剥这一种），故 Ctrl/Alt/Win
        // 不给：列出来就是配得上、core 解析不了、整条静默丢弃。
        let mut mod_row = Element::row()
            .spacing(16)
            .cross(Align::Center)
            .visible_when(move || mode.get() == 1);
        if table == BindingTable::Lead {
            mod_row = mod_row.child(Element::checkbox("Ctrl", mc).small().fg_role(Role::Text));
        }
        mod_row = mod_row.child(Element::checkbox("Shift", ms).small().fg_role(Role::Text));
        if table == BindingTable::Lead {
            mod_row = mod_row
                .child(Element::checkbox("Alt", ma).small().fg_role(Role::Text))
                .child(Element::checkbox("Win", mw).small().fg_role(Role::Text));
        }

        let key_col = Element::col()
            .width_match()
            .spacing(8)
            .child(section_title("按键"))
            .child(mode_row)
            .child(mod_row)
            .child(
                Element::dropdown_signal(self.single_labels, self.single_idx)
                    .width(200)
                    .visible_when(move || mode.get() == 0),
            )
            .child(
                Element::dropdown_signal(self.combo_labels, self.combo_idx)
                    .width(200)
                    .visible_when(move || mode.get() == 1),
            )
            .child(
                Element::row()
                    .width_match()
                    .cross(Align::Center)
                    .spacing(8)
                    .child(
                        Element::label("当前按键")
                            .font_size(12.0)
                            .fg_role(Role::TextMuted),
                    )
                    .child(
                        Element::label_signal(self.preview)
                            .font_size(13.0)
                            .font_weight(600)
                            .fg_role(Role::Text)
                            .max_lines(1),
                    ),
            );

        let verb_col = Element::col()
            .width_match()
            .spacing(8)
            .child(section_title("功能"))
            .child(Element::dropdown_signal(self.cat_labels, self.cat_idx).width(200))
            .child(Element::dropdown_signal(self.item_labels, self.item_idx).width(280));

        let notice = self.notice;
        let notice_row = Element::label_signal(notice)
            .font_size(11.5)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(3)
            .visible_when(move || !notice.get().is_empty());

        // 任一控件动了就重算可选功能与提示。用 effect 而不是给每个控件挂 on_change：
        // 挂七处等于赌自己数全了，漏一处的表现是「换了键、功能列表没跟着变」。
        let eff_me = self.clone();
        let (mi, si, ci, ki) = (self.mode, self.single_idx, self.combo_idx, self.cat_idx);
        let effect = crate::pages::dict::reactive::reactive_effect(
            move || {
                mi.version()
                    .wrapping_add(si.version())
                    .wrapping_add(ci.version())
                    .wrapping_add(ki.version())
                    .wrapping_add(mc.version())
                    .wrapping_add(ms.version())
                    .wrapping_add(ma.version())
                    .wrapping_add(mw.version())
            },
            move || eff_me.refresh(),
        );

        let content = Element::col()
            .width_match()
            .spacing(12)
            .padding_xy(PAD_X, PAD_Y)
            .child(key_col)
            .child(Element::divider())
            .child(verb_col)
            .child(notice_row)
            .child(effect);

        crate::dialogs::field_dialogs::build_confirm_dialog_shell(
            self.visible,
            self.title,
            content,
            self.can_confirm,
            move || {
                let Some((key, verb)) = me.commit_value() else {
                    return;
                };
                if let Some(f) = me.data.on_commit.borrow().as_ref() {
                    f(me.data.editing.get(), key, verb);
                }
            },
        )
    }
}

fn section_title(t: &str) -> Element {
    Element::label(t)
        .font_size(12.0)
        .font_weight(600)
        .fg_role(Role::TextMuted)
        .width_match()
}

/// 键名 → 下拉索引；不在词表里的（手写配置的生僻键）**追加**为选项，不丢弃。
///
/// 丢弃的表现是：用户点「修改」看到「（请选择）」，自己配的那个键像凭空消失了，
/// 而它在 core 里跑得好好的。与两个编辑器对未知**动词**的处置同策略。
fn key_index(vals: &RefCell<Vec<String>>, labels: Signal<Vec<String>>, name: &str) -> usize {
    let name = name.trim();
    if name.is_empty() {
        return 0;
    }
    if let Some(i) = vals.borrow().iter().position(|v| v == name) {
        return i;
    }
    vals.borrow_mut().push(name.to_string());
    let mut l = labels.get();
    l.push(format!("{}（当前值）", key_display(name)));
    labels.set(l);
    vals.borrow().len() - 1
}

fn find_verb(cats: &[VerbCategory], value: &str) -> Option<(usize, usize)> {
    if value.is_empty() {
        return None;
    }
    for (ci, c) in cats.iter().enumerate() {
        if let Some(ii) = c.items.iter().position(|v| v.value == value) {
            return Some((ci, ii));
        }
    }
    None
}

// 写前先比：`refresh` 挂在 `reactive_effect` 上，而 effect 的触发判据是信号版本。
// 无条件 `set` 会让每次 refresh 都推高版本 ⇒ 下一帧再触发一次，永不收敛。
fn set_if_changed(sig: Signal<usize>, v: usize) {
    if sig.get() != v {
        sig.set(v);
    }
}
fn set_if_changed_bool(sig: Signal<bool>, v: bool) {
    if sig.get() != v {
        sig.set(v);
    }
}
fn set_str_if_changed(sig: Signal<String>, v: String) {
    if sig.get() != v {
        sig.set(v);
    }
}
fn set_vec_if_changed(sig: Signal<Vec<String>>, v: Vec<String>) {
    if sig.get() != v {
        sig.set(v);
    }
}

// ───────────────────────────── 行渲染 ─────────────────────────────

/// 一条绑定的展示行：`[按键] [功能] [提示] [修改] [删除]`。
///
/// 展示用静态文本而不是信号：整份列表挂在 `Signal<Vec<Binding>>` 上，增删改一律整表
/// `set`（编辑走对话框，不再有行内逐字符输入那种"每敲一下都重建"的问题），故每次变化
/// 本来就会重建行——再让单元格自己响应式就是两套刷新路径。
pub fn binding_row(
    key: &str,
    verb_label: &str,
    hint: Option<(&str, Role)>,
    on_edit: impl FnMut(&mut EventCtx) + 'static,
    on_delete: impl FnMut(&mut EventCtx) + 'static,
) -> Element {
    let mut row = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(0, 3)
        .child(
            Element::label(key_display(key))
                .font_size(12.5)
                .fg_role(Role::Text)
                .width(130)
                .max_lines(1),
        )
        .child(
            Element::label(verb_label)
                .font_size(12.5)
                .fg_role(Role::Text)
                .weight(1.0)
                .max_lines(1),
        );
    if let Some((text, role)) = hint {
        row = row.child(
            Element::label(text)
                .font_size(11.0)
                .fg_role(role)
                .max_lines(1),
        );
    }
    row.child(
        Element::button("修改")
            .small()
            .outline_soft()
            .on_click(on_edit),
    )
    .child(
        Element::button("删除")
            .small()
            .outline_soft()
            .on_click(on_delete),
    )
}

/// 一条**别处占用**的按键绑定：住在专用字段里，本编辑器只展示、不写回。
///
/// # 为什么要在这里展示
///
/// 「左 Shift = 中英文切换」住在 `keys.toggle_mode_keys`，`Ctrl+Shift+E` 住在
/// `keys.switch_engine`——它们与本表**编译进同一张热键表**，撞了就是一方静默失效，
/// 而在本表里完全看不见。用户于是会在这里再配一个左 Shift：配完两处都指着切换，
/// 界面上没有任何东西告诉他撞了（实际是本表赢，core 的 `handle_bound_modifier_key_up`
/// 先于 `is_toggle_mode_keycode`）。**「看不见」才是那次困惑的根，不是「配得上」。**
///
/// # 为什么只读
///
/// 本编辑器的写回口只有一个（`keys.key_actions`），而本组条目的落点各不相同。让它可写
/// 就要在这里决定写哪个字段 ＝ 第二真相源，正是 core `key-resolver-unification.md` §4.3
/// 否决「可编辑总览」的那条。本组只展示 + 指路，编辑仍回各自入口。
#[derive(Clone)]
pub struct ForeignBinding {
    /// 已经翻好的键名（`左 Shift、右 Shift` / `Ctrl+数字`）。
    ///
    /// ★ 存翻好的串而不是原始键名：本组的值有三种形态——单键名数组
    /// （`toggle_mode_keys`）、组合键串（`switch_engine`）、模板（`ctrl+number`），
    /// 各有各的翻法。让行渲染去认形态，等于把三处知识摊进 UI 层。
    pub keys: String,
    /// 功能中文名。
    pub label: String,
    /// 去哪儿改（如「按键 › 中英切换键」）。
    pub source: String,
}

/// [`ForeignBinding`] 的展示行：`[按键] [功能] [来源]`，无按钮。
///
/// 列宽与 [`binding_row`] 的前两列对齐——两组上下相邻，错开一格就会显得是两张表。
fn foreign_row(b: &ForeignBinding) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(0, 3)
        .child(
            Element::label(&b.keys)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width(130)
                .max_lines(1),
        )
        .child(
            Element::label(&b.label)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .weight(1.0)
                .max_lines(1),
        )
        .child(
            Element::label(&b.source)
                .font_size(11.0)
                .fg_role(Role::TextMuted)
                .max_lines(1),
        )
}

/// 只读组的整块（折叠头 + 列表 + 驱动折叠的 effect）。返回 `None` 表示本编辑器不带这一组。
///
/// 方案级 `[key_actions]` 那个实例**不传**：专用字段全是全局的，在方案对话框里列出来
/// 会让人以为它们也能按方案配。
pub fn foreign_section(
    all: std::rc::Rc<RefCell<Vec<ForeignBinding>>>,
    shown: Signal<Vec<ForeignBinding>>,
    open: Signal<bool>,
    count_text: Signal<String>,
) -> Element {
    // 折叠开关用 checkbox 而不是「展开/收起」按钮：按钮的文案得跟着状态变，而本仓没有
    // 文案绑信号的按钮，绕出来的写法（host_signal 套单元素 Vec）比它解决的问题还长。
    let head = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(6)
        .child(
            Element::checkbox("其它设置项占用的按键", open)
                .small()
                .fg_role(Role::TextMuted),
        )
        .child(
            Element::label_signal(count_text)
                .font_size(11.0)
                .fg_role(Role::TextMuted)
                .max_lines(1),
        );

    // 折叠只换 `shown` 的内容，不隐藏容器：`host_signal` 本来就按数据重建子树，空表
    // 就是零行，比再套一层显隐条件少一条状态。
    let refresh = {
        let all = all.clone();
        move || {
            shown.set(if open.get() {
                all.borrow().clone()
            } else {
                Vec::new()
            })
        }
    };
    let sync =
        crate::pages::dict::reactive::reactive_effect(move || u64::from(open.get()), refresh);

    Element::col()
        .width_match()
        .spacing(2)
        .child(sync)
        .child(head)
        .child(Element::host_signal(shown, |b: ForeignBinding| {
            foreign_row(&b)
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 组合键的动词白名单必须与 core 的 `hotkey_action_entry` 对齐。
    ///
    /// ★ 判据取自**出厂配置**而不是凭记忆列举：`data/config.toml` 里出厂就有
    /// `"ctrl+shift+k" = "softkeyboard"`，任何让出厂配置显示成「不支持」的白名单
    /// 都是错的——那是全新安装打开对话框第一眼就能看到的红字。
    ///
    /// ⚠️ 这张表与 core 那张没有编译期约束（跨仓、两份手写）。本测试只守「已知会出现
    /// 在出厂配置里的动词」这条底线，core 加了新动词仍要人工同步。
    #[test]
    fn combo_verbs_match_core_whitelist() {
        for v in [
            "softkeyboard",
            "softkeyboard:punct",
            "toggle_schema:english",
            "switch_schema:wubi86",
            "special:kf",
        ] {
            assert!(
                verb_allowed(KeyShape::Combo, v),
                "组合键动词 {v:?} 被判为不支持，而 core 的 hotkey_action_entry 认它"
            );
        }
        // 反向：A 类动词在组合键上确实不受支持（core 那边 warn 后忽略），别为了消红
        // 把闸门整个放开——那样用户配得上、按下去没反应，比标红难查得多。
        for v in ["toggle_mode", "switch_engine", "toggle_punct"] {
            assert!(
                !verb_allowed(KeyShape::Combo, v),
                "{v:?} 不该在组合键上放行：core 的组合键白名单不含 A 类动词"
            );
        }
    }

    /// 键名转义：符号显示成符号本身，组合键逐段翻译。
    #[test]
    fn key_names_are_rendered_for_humans() {
        assert_eq!(key_display("rbracket"), "]");
        assert_eq!(key_display("backtick"), "`");
        assert_eq!(key_display("rshift"), "右 Shift");
        assert_eq!(key_display("ctrl+shift+r"), "Ctrl + Shift + R");
        assert_eq!(key_display("shift+tab"), "Shift + Tab");
        assert_eq!(key_display("f5"), "F5");
        assert_eq!(key_display(""), "未设置");
        // 字面符号写法（手写配置）与规范名等价，core 的 KEY_TABLE 两种都认。
        assert_eq!(key_display("]"), "]");
    }

    /// 认不出的键名显示**原值**，不显示「未知」——它在 core 里可能跑得好好的。
    #[test]
    fn unknown_key_name_falls_back_to_raw() {
        assert_eq!(key_display("f99"), "f99");
        assert_eq!(key_display("某个未来的键"), "某个未来的键");
    }

    /// 选择用标签要能把形近符号分辨开，展示用标签则要短。两者是不同的函数。
    #[test]
    fn choice_label_disambiguates_where_display_is_terse() {
        assert_eq!(key_choice_label("quote"), "单引号 ( ' )");
        assert_eq!(key_choice_label("backtick"), "反引号 ( ` )");
        assert_eq!(key_display("quote"), "'");
        assert_eq!(key_display("backtick"), "`");
        // 非符号键两者一致，不该凭空造第二套名字。
        assert_eq!(key_choice_label("rshift"), key_display("rshift"));
    }

    #[test]
    fn key_shapes_match_core_routing() {
        assert_eq!(key_shape("ctrl+shift+r"), KeyShape::Combo);
        assert_eq!(key_shape("rshift"), KeyShape::Modifier);
        assert_eq!(key_shape("lctrl"), KeyShape::Modifier);
        assert_eq!(key_shape("backtick"), KeyShape::Char);
        assert_eq!(key_shape("z"), KeyShape::Char);
    }

    /// ★★ 组合键的可绑功能必须与 core `hotkey_action_entry` 的白名单一致。
    ///
    /// 放宽的后果是用户配得上、按下去什么都不发生，core 只在日志里 warn 一句——
    /// 与「没绑上」完全同形，无从分辨。收紧的后果是能力凭空少一半。
    #[test]
    fn combo_keys_allow_exactly_the_core_whitelist() {
        for v in [
            "toggle_schema:english",
            "switch_schema:wubi",
            "special:fuhao",
        ] {
            assert!(verb_allowed(KeyShape::Combo, v), "{v} 应允许绑组合键");
        }
        for v in [
            "none",
            "temp_pinyin",
            "temp_english",
            "aux_code",
            "mix:quick_mix",
            "toggle_mode",
            "switch_engine",
            "toggle_punct",
            "toggle_s2t",
            "open_settings",
        ] {
            assert!(!verb_allowed(KeyShape::Combo, v), "{v} 不该允许绑组合键");
        }
    }

    /// 「用来离开英文态」的动作绑有字符的键是单程票，必须挡住；修饰键上则全放行。
    #[test]
    fn char_keys_exclude_modifier_only_verbs() {
        for v in [
            "toggle_mode",
            "switch_engine",
            "toggle_schema:english",
            "switch_schema:wubi",
        ] {
            assert!(!verb_allowed(KeyShape::Char, v), "{v} 不该允许绑有字符的键");
            assert!(verb_allowed(KeyShape::Modifier, v), "{v} 应允许绑修饰键");
        }
        for v in ["temp_pinyin", "aux_code", "toggle_punct", "none"] {
            assert!(verb_allowed(KeyShape::Char, v), "{v} 应允许绑有字符的键");
        }
    }

    fn lead_verbs() -> Vec<Verb> {
        [
            ("none", "不启用"),
            ("temp_pinyin", "临时拼音"),
            ("aux_code", "辅助码筛选"),
            ("mix:quick_mix", "快捷输入 - 快符"),
            ("special:fuhao", "特殊模式 - 符号"),
            ("toggle_schema:english", "切换方案 - 英文"),
            // 状态切换类里两种都要有：`toggle_mode` 限修饰键、`toggle_punct` 不限。
            // 只放前者的话，「有字符的键上这一类被整类滤掉」是正确行为，测不出过滤有没有
            // 过头——本测试第一版就是这么假绿的。
            ("toggle_mode", "中英文切换"),
            ("toggle_punct", "中/英标点切换"),
            ("open_settings", "打开设置"),
            ("some_future_verb", "未来动词"),
        ]
        .iter()
        .map(|(v, l)| Verb {
            value: (*v).to_string(),
            label: (*l).to_string(),
        })
        .collect()
    }

    /// 分类按 value 前缀分，且**不丢项**——丢一项就等于用户配的那个功能在界面上消失。
    #[test]
    fn categorization_keeps_every_verb() {
        let cats = categorize_lead_verbs(&lead_verbs());
        let mut got: Vec<String> = cats
            .iter()
            .filter(|c| c.name != "高级")
            .flat_map(|c| c.items.iter())
            .map(|v| v.value.clone())
            .collect();
        got.sort();
        let mut want: Vec<String> = lead_verbs().iter().map(|v| v.value.clone()).collect();
        want.sort();
        assert_eq!(got, want, "分类不得丢弃任何动词");
        // 未知动词落「其它」，不静默消失。
        let other = cats
            .iter()
            .find(|c| c.name == "其它")
            .expect("应有「其它」类");
        assert_eq!(other.items[0].value, "some_future_verb");
    }

    /// 空分类要被删掉：给用户一个点开只有空列表的分类，比不给更让人困惑。
    #[test]
    fn empty_categories_are_dropped() {
        let cats = categorize_lead_verbs(&[Verb {
            value: "none".into(),
            label: "不启用".into(),
        }]);
        let names: Vec<&str> = cats.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["不启用", "高级"],
            "只有一个动词时不该出现空分类"
        );
    }

    /// 「高级」恒为末类且 locked——它是预留位，选中即禁用「确定」。
    #[test]
    fn advanced_category_is_last_and_locked() {
        let cats = categorize_lead_verbs(&lead_verbs());
        let last = cats.last().expect("至少有高级类");
        assert_eq!(last.name, "高级");
        assert!(last.locked, "高级类必须 locked，否则能选中一个不存在的功能");
        assert!(!last.notice.is_empty(), "locked 的类必须说明为什么不能选");

        let s = session_verb_categories();
        assert_eq!(s.last().expect("至少有高级类").name, "高级");
    }

    /// 两个序号档的上限**不同**，两边都要钉住——一样的写法很容易在后续维护里被「统一」
    /// 成同一个数，而它们不同的理由各自独立（见 `session_verb_categories` 的文档）：
    /// 选词 1..=9 是「一页真有 9 个候选」，以词定字 1..=4 是「第 5 字以上恒定落进
    /// overflow，与没绑上同形」。
    #[test]
    fn session_ordinal_verbs_cover_intended_domain() {
        let cats = session_verb_categories();
        for (name, prefix, max) in [
            ("选择候选", "select_candidate:", 9u8),
            ("以词定字", "select_char:", 4u8),
        ] {
            let c = cats.iter().find(|c| c.name == name).expect(name);
            let vals: Vec<String> = c.items.iter().map(|v| v.value.clone()).collect();
            let want: Vec<String> = (1..=max).map(|n| format!("{prefix}{n}")).collect();
            assert_eq!(vals, want, "{name} 的序号档应为 1..={max}");
        }
    }

    /// 会话态动词值必须都在 core `SessionAction` 的值域里（拼写守门）。
    ///
    /// ⚠️ 序号上界这里恒按 core 的 1..=9 判，**不跟着选择器的 1..=4 收窄**：本测断言的是
    /// 「不超出 core 值域」，写入侧收窄后照样成立。改成等值比较就把两件事绑死了——以词定字
    /// 给到第几档是产品取舍（`session_ordinal_verbs_cover_intended_domain` 管），core 认到
    /// 第几档是拼写守门（本测管）。
    ///
    /// ⚠ 与 core 的 `SessionAction::parse` 是两份手写清单，没有编译期约束——本仓已有过
    /// 「两仓键名拼写不一致致静默不匹配」的现场。
    #[test]
    fn session_verb_values_match_core_domain() {
        for c in session_verb_categories() {
            if c.locked {
                continue; // 占位项，值恒为空
            }
            for v in c.items {
                let ok = matches!(
                    v.value.as_str(),
                    "none"
                        | "page_prev"
                        | "page_next"
                        | "highlight_up"
                        | "highlight_down"
                        | "cancel"
                        | "clear"
                        | "aux_code"
                        | "aux_code:page_next"
                ) || v
                    .value
                    .strip_prefix("select_candidate:")
                    .or_else(|| v.value.strip_prefix("select_char:"))
                    .is_some_and(|n| n.parse::<u8>().is_ok_and(|n| (1..=9).contains(&n)));
                assert!(ok, "动词 {:?} 不在 core SessionAction 的值域里", v.value);
            }
        }
    }

    fn lead_picker() -> KeyPicker {
        let p = KeyPicker::new(BindingTable::Lead);
        p.set_verbs(&lead_verbs());
        p
    }

    /// 组合键条目往返：回填 → 读出必须一字不差。
    ///
    /// 这类条目**本就存在**（方案设置的「切换到本方案按键」写的就是它）。回填错了的表现是
    /// 用户点开「修改」、什么都不改点确定，那条绑定就变了形。
    #[test]
    fn picker_round_trips_a_combo_binding() {
        let p = lead_picker();
        p.open_edit(7, "ctrl+shift+r", "toggle_schema:english");
        assert_eq!(p.mode.get(), 1, "有修饰键应落在「组合键」档");
        assert_eq!(p.current_key(), "ctrl+shift+r");
        assert_eq!(p.current_verb(), "toggle_schema:english");
    }

    #[test]
    fn picker_round_trips_a_single_key_binding() {
        let p = lead_picker();
        p.open_edit(1, "backslash", "mix:quick_mix");
        assert_eq!(p.mode.get(), 0, "无修饰键应落在「单个按键」档");
        assert_eq!(p.current_key(), "backslash");
        assert_eq!(p.current_verb(), "mix:quick_mix");
    }

    /// 会话态的 `shift+` 前缀同样要能往返（core 只认这一种组合形态）。
    #[test]
    fn session_picker_round_trips_shift_prefix() {
        let p = KeyPicker::new(BindingTable::Session);
        p.open_edit(1, "shift+tab", "page_prev");
        assert_eq!(p.mode.get(), 1);
        assert_eq!(p.current_key(), "shift+tab");
        assert_eq!(p.current_verb(), "page_prev");
    }

    /// ★★ `refresh` 必须幂等。
    ///
    /// 它挂在 `reactive_effect` 上，而 effect 的触发判据是各信号的版本号。若 refresh 每次
    /// 都无条件写信号，版本每帧都变 ⇒ 每帧都再跑一次，永不收敛——表现是设置页 CPU 空转，
    /// 与本仓「UI 8ms 轮询」那次同型。
    #[test]
    fn refresh_is_idempotent() {
        let p = lead_picker();
        p.open_edit(1, "backslash", "mix:quick_mix");
        p.refresh();
        let before = (
            p.cat_idx.version(),
            p.item_idx.version(),
            p.cat_labels.version(),
            p.item_labels.version(),
            p.preview.version(),
            p.notice.version(),
            p.can_confirm.version(),
        );
        p.refresh();
        let after = (
            p.cat_idx.version(),
            p.item_idx.version(),
            p.cat_labels.version(),
            p.item_labels.version(),
            p.preview.version(),
            p.notice.version(),
            p.can_confirm.version(),
        );
        assert_eq!(before, after, "第二次 refresh 不该再改动任何信号");
    }

    /// 选了组合键，功能列表就只剩 core 认的那两类，并给出一行说明。
    #[test]
    fn combo_selection_narrows_the_verb_list() {
        let p = lead_picker();
        p.open_edit(1, "ctrl+shift+r", "toggle_schema:english");
        let names: Vec<String> = p
            .data
            .view
            .borrow()
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert!(
            names.contains(&"切换方案".to_string()),
            "组合键应保留「切换方案」：{names:?}"
        );
        assert!(
            !names.contains(&"状态切换".to_string()),
            "组合键不该出现「状态切换」：{names:?}"
        );
        assert!(
            !p.notice.get().is_empty(),
            "过滤后必须说明为什么，否则像是少了功能"
        );

        // 换回单个按键，列表要恢复。
        p.open_edit(1, "backslash", "mix:quick_mix");
        let names: Vec<String> = p
            .data
            .view
            .borrow()
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert!(
            names.contains(&"状态切换".to_string()),
            "换回单键后应恢复完整列表：{names:?}"
        );
    }

    /// 任何一次「功能被滤掉」都必须有说明。
    ///
    /// 没有说明的过滤 = 用户选了个键，整类功能无声消失，界面上没有一处说得出原因。
    /// 这正是本轮改动想消灭的「配不出来还不知道为什么」，却在 Char 那一支上漏了。
    #[test]
    fn every_filtering_shape_explains_itself() {
        let p = lead_picker();
        for key in ["backslash", "ctrl+shift+r"] {
            p.open_edit(1, key, "none");
            assert!(
                p.filtered_count() > 0,
                "前提：{key} 上应当有功能被滤掉，否则这条测不到东西"
            );
            assert!(
                !p.notice.get().is_empty(),
                "{key} 上滤掉了功能却没有任何说明：{:?}",
                p.notice.get()
            );
        }
        // 修饰键什么都不滤 ⇒ 不该挂说明，否则成了常驻噪音。
        p.open_edit(1, "rshift", "none");
        assert_eq!(p.filtered_count(), 0, "修饰键上不该有功能被滤掉");
        assert!(p.notice.get().is_empty(), "没滤掉东西时不该有说明");
    }

    /// 提示文案里不得混入连续空白。
    ///
    /// 反斜杠续行会被 rustfmt 接回一行，被它吞掉的缩进随之变成字面空格——用户看到的是
    /// 句子中间夹一大段空白。本文件的辅助码那条已经这么中招过一次。
    #[test]
    fn notice_texts_have_no_stray_whitespace() {
        let mut vs = lead_verbs();
        vs.push(Verb {
            value: "aux_code".into(),
            label: "辅助码筛选".into(),
        });
        let p = KeyPicker::new(BindingTable::Lead);
        p.set_verbs(&vs);
        for (key, verb) in [
            ("backtick", "aux_code"),
            ("backslash", "none"),
            ("ctrl+shift+r", "toggle_schema:english"),
            ("", "none"),
        ] {
            if key.is_empty() {
                p.open_new();
            } else {
                p.open_edit(1, key, verb);
            }
            let n = p.notice.get();
            assert!(!n.contains("  "), "提示里混入了连续空白：{n:?}");
        }
        // 分类里那条也一样（它不经 notice_text，是 VerbCategory::notice）。
        for c in categorize_lead_verbs(&vs) {
            assert!(
                !c.notice.contains("  "),
                "分类说明混入连续空白：{:?}",
                c.notice
            );
        }
    }

    /// 单向切换在**能合法绑它的每一种键形态**上都要出得来。
    ///
    /// 它是「切换到本方案按键」的落点动词，全局表里天天有；选不到就等于那条绑定只能靠
    /// 方案页那个专用入口配，改不了也加不了第二个。
    #[test]
    fn one_way_schema_switch_is_offered_wherever_legal() {
        let mut vs = lead_verbs();
        vs.push(Verb {
            value: "switch_schema:wubi".into(),
            label: "切到方案（单向） - 五笔".into(),
        });
        let p = KeyPicker::new(BindingTable::Lead);
        p.set_verbs(&vs);
        for key in ["rshift", "ctrl+shift+r"] {
            p.open_edit(1, key, "switch_schema:wubi");
            assert_eq!(
                p.current_verb(),
                "switch_schema:wubi",
                "{key} 上应当选得到单向切换"
            );
        }
        // 有字符的键上它确实不该有（core 的 requires_modifier_key 拒绝），
        // 但此时必须有说明——见 every_filtering_shape_explains_itself。
        p.open_edit(1, "backslash", "none");
        assert!(
            find_verb(&p.data.view.borrow(), "switch_schema:wubi").is_none(),
            "有字符的键上不该提供单向切换"
        );
    }

    /// 词表外的生僻键要被保留成「当前值」，不能悄悄变回「（请选择）」。
    #[test]
    fn unknown_key_is_kept_as_current_value() {
        let p = lead_picker();
        p.open_edit(1, "f13", "temp_pinyin");
        assert_eq!(p.current_key(), "f13", "生僻键必须原样保住");
        assert!(
            p.single_labels.get().iter().any(|l| l.contains("当前值")),
            "生僻键应作为「当前值」进下拉"
        );
    }

    /// 「组合键」档一个修饰键都没勾时不产出绑定——不能悄悄回落成裸键。
    ///
    /// 回落的话就绕过了单键那份值域限制（Lead 表的组合档能选任意字母，单键档只给 z），
    /// 结果是用户配出一个 core 必然让位的字母绑定。
    #[test]
    fn combo_mode_without_modifier_yields_nothing() {
        let p = lead_picker();
        p.open_edit(1, "ctrl+shift+r", "toggle_schema:english");
        for m in &p.mods {
            m.set(false);
        }
        p.refresh();
        assert_eq!(p.current_key(), "", "没勾修饰键就不该产出键名");
        assert!(!p.can_confirm.get(), "此时「确定」必须不可点");
    }

    /// 「高级」类选中即禁用确定——它是预留位，不能真配上去。
    #[test]
    fn advanced_category_blocks_confirm() {
        let p = lead_picker();
        p.open_edit(1, "backslash", "mix:quick_mix");
        let adv = p
            .data
            .view
            .borrow()
            .iter()
            .position(|c| c.name == "高级")
            .expect("应有高级类");
        p.cat_idx.set(adv);
        p.refresh();
        assert_eq!(
            p.cat_idx.get(),
            adv,
            "★ 选中高级类后不能被 refresh 弹回第一类——否则那一类点开就跳走，             用户根本看不到里面写了什么"
        );
        assert!(!p.can_confirm.get(), "选中高级类时「确定」必须不可点");
        assert!(!p.notice.get().is_empty(), "必须说明为什么不能选");
        assert!(p.commit_value().is_none(), "预留项绝不能落盘");
    }

    /// ★ 换分类不能被 refresh 弹回。
    ///
    /// `item_idx` 换类后多半越界（上一类项多、这一类项少），若只按「当前功能值」恢复
    /// 选中项，此时取不到值就会双双回落 (0,0)——表现是点哪个分类都跳回第一个。
    #[test]
    fn switching_category_sticks() {
        let p = lead_picker();
        // 先选一个项多的分类里靠后的项，制造出「换类后 item_idx 越界」这个前提。
        p.open_edit(1, "backslash", "special:fuhao");
        let modes = p
            .data
            .view
            .borrow()
            .iter()
            .position(|c| c.name == "输入模式")
            .expect("应有输入模式类");
        assert_eq!(p.cat_idx.get(), modes);
        assert!(p.item_idx.get() > 0, "前提：选中项不是首项，否则测不出越界");

        // 换到一个只有一项的分类。
        let disable = p
            .data
            .view
            .borrow()
            .iter()
            .position(|c| c.name == "不启用")
            .expect("应有不启用类");
        p.cat_idx.set(disable);
        p.refresh();
        assert_eq!(p.cat_idx.get(), disable, "换分类后不该被弹回");
        assert_eq!(p.item_idx.get(), 0, "换分类后应选中它的第一项");
        assert_eq!(p.current_verb(), "none");
    }

    /// 辅助码 + 反引号要当场提示：全拼的反引号被音节分隔符占着，绑上去完全没反应。
    #[test]
    fn aux_code_on_backtick_is_warned() {
        let mut vs = lead_verbs();
        vs.push(Verb {
            value: "aux_code".into(),
            label: "辅助码筛选".into(),
        });
        let p = KeyPicker::new(BindingTable::Lead);
        p.set_verbs(&vs);
        p.open_edit(1, "backtick", "aux_code");
        assert!(
            p.notice.get().contains("音节分隔符"),
            "反引号 + 辅助码必须提示，否则就是「配了没反应」：{:?}",
            p.notice.get()
        );
        // 换个键就不该再提示这一条——常驻警告会把可用的配置误报成待修问题。
        // ⚠️ 断言查的是**内容**不是「提示为空」：分号是有字符的键，会另出一条「切换方案
        // 之类绑不上去」的过滤说明。按「为空」断言的话，这条测的其实是「那边有没有别的
        // 提示」，与它的名字无关——加过滤说明时它就假红了一次。
        p.open_edit(1, "semicolon", "aux_code");
        assert!(
            !p.notice.get().contains("音节分隔符"),
            "别的键上不该有这条提示：{:?}",
            p.notice.get()
        );
        // 而且它**不是**过滤：分号上照样选得到辅助码。
        assert_eq!(p.current_verb(), "aux_code");
    }

    /// ★ 会话表这一侧同样要提示。辅助码收编进 `session_actions` 之后，这条提示一直没跟
    /// 过来——而「在会话表里把反引号配成 aux_code」恰恰是最自然的配法。
    ///
    /// 共键（`aux_code:page_next`）的文案必须**不同**：它进不去时会降级成翻页，用户拿到的
    /// 是一个翻页键而不是「完全没反应」，说成「请换一个键」就不准确了。
    #[test]
    fn aux_code_on_backtick_is_warned_in_session_table_too() {
        let p = KeyPicker::new(BindingTable::Session);

        p.open_edit(1, "backtick", "aux_code");
        assert!(
            p.notice.get().contains("音节分隔符"),
            "会话表里同样要提示：{:?}",
            p.notice.get()
        );
        assert!(
            p.notice.get().contains("请换一个键"),
            "专用触发键进不去 = 彻底没反应：{:?}",
            p.notice.get()
        );

        p.open_edit(1, "backtick", "aux_code:page_next");
        assert!(
            p.notice.get().contains("音节分隔符"),
            "共键同样进不去辅助码：{:?}",
            p.notice.get()
        );
        assert!(
            p.notice.get().contains("下翻页"),
            "共键会降级成翻页，文案不能说「请换一个键」：{:?}",
            p.notice.get()
        );

        // 换个键就不该再提示这一条——常驻警告会把可用的配置误报成待修问题。
        p.open_edit(1, "pagedown", "aux_code:page_next");
        assert!(
            !p.notice.get().contains("音节分隔符"),
            "别的键上不该有这条提示：{:?}",
            p.notice.get()
        );
    }

    /// 确定回调拿到的是 `(行 id, 键, 功能)`；新增时行 id 为 None。
    #[test]
    fn commit_reports_edit_target() {
        type Call = (Option<RowId>, String, String);
        let got: Rc<RefCell<Vec<Call>>> = Rc::new(RefCell::new(Vec::new()));
        let p = lead_picker();
        let sink = got.clone();
        p.on_commit(move |id, k, v| sink.borrow_mut().push((id, k, v)));

        p.open_edit(42, "backslash", "mix:quick_mix");
        commit(&p);
        p.open_new();
        p.open_edit_key_only("semicolon", "temp_pinyin");
        commit(&p);

        let g = got.borrow();
        assert_eq!(g[0].0, Some(42), "「修改」必须带上被改那一行的 id");
        assert_eq!(g[0].1, "backslash");
        assert_eq!(g[1].0, None, "「添加」不带 id");
        assert_eq!(g[1].1, "semicolon");
    }

    /// 走一遍「确定」按钮里的那段逻辑（按钮闭包本身要靠 UI 才点得到）。
    /// 与按钮共用 `commit_value`，故这里测得到的就是按下去会发生的。
    fn commit(p: &KeyPicker) {
        let Some((key, verb)) = p.commit_value() else {
            return;
        };
        if let Some(f) = p.data.on_commit.borrow().as_ref() {
            f(p.data.editing.get(), key, verb);
        }
    }

    impl KeyPicker {
        /// 测试用：模拟「新增」态下用户选好了键与功能。
        fn open_edit_key_only(&self, key: &str, verb: &str) {
            self.single_idx
                .set(key_index(&self.data.single, self.single_labels, key));
            self.mode.set(0);
            self.select_verb(verb);
            self.refresh();
            self.select_verb(verb);
        }
    }

    /// 单键值域里的每个键都要有可辨识的标签——落回原始键名（`rbracket` 这种内部写法）
    /// 就等于让用户在下拉里读内部词表。
    #[test]
    fn every_single_key_has_a_readable_label() {
        for table in [BindingTable::Lead, BindingTable::Session] {
            for (v, l) in single_keys(table) {
                if v.is_empty() {
                    continue; // 「（请选择）」占位项
                }
                assert_ne!(l, v, "{v:?} 的下拉标签落回了原始键名");
            }
        }
    }

    /// 修饰键必须都在单键值域里——「按右 Shift 切方案」是最常见的用法之一，
    /// 而那类动作**只能**绑修饰键（见 `verb_requires_modifier`）。少了它们，
    /// 整类「切换方案」在界面上就配不出来了。
    #[test]
    fn lead_single_keys_include_all_modifiers() {
        let vals: Vec<String> = single_keys(BindingTable::Lead)
            .into_iter()
            .map(|(v, _)| v)
            .collect();
        for k in ["lshift", "rshift", "lctrl", "rctrl"] {
            assert!(vals.iter().any(|v| v == k), "单键值域缺 {k}");
            assert_eq!(key_shape(k), KeyShape::Modifier);
        }
    }

    /// 会话态键名不得含别名——别名与主名同义，列出来只会让人以为是不同的键。
    #[test]
    fn session_key_options_have_no_aliases() {
        for k in session_base_keys() {
            assert!(
                !matches!(
                    *k,
                    "pgup" | "pgdn" | "prior" | "next" | "caps" | "lcontrol" | "rcontrol"
                ),
                "{k:?} 是别名，不该出现在下拉里"
            );
        }
    }
}
