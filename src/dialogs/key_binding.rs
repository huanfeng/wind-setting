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
//! | 组合键 | key_down 热键表（**只编全局** `keys.key_actions`） | `BoundAction` 全值域，含 `command:`；只挡加词两项（见 [`verb_allowed`]）。方案级 `[key_actions]` 不给组合键（[`KeyPicker::disallow_combo`]） |
//! | 纯修饰键 | key_up 轻敲 | 全部 |
//! | 单个有字符的键 | keydown 引导键链 | 除「用来离开英文态」的那几个（`BoundAction::requires_modifier_key`）；`command:` 可绑 |
//!
//! 会话态表（`session_actions`）不按形态过滤：它只在有候选时才裁决，键值域本身已经收窄。
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
        // 组合键走 key_down 热键表，值域**就是** `BoundAction` 的全值域——与单键、修饰键
        // 同一张表（core `hotkey_action_entry` 2026-09-18 合流）。
        //
        // 此前这里与 core 各持一份逐条追加的六项白名单，组合键能配的功能恒比单键少一大截。
        // 那不是设计结论：组合键这条通路在按键链上位次**更早**（英文模式分水岭之前）、
        // 不与输入争键、可全局拦截，本该是限制最少的一条。两份手写清单还漂移过一次——
        // `softkeyboard` 漏在这里，而它**是出厂配置**（`data/config.toml` 的
        // `key_actions = { "ctrl+shift+k" = "softkeyboard" }`）⇒ 全新安装打开这个对话框，
        // 出厂那条就红着「组合键不支持」（2026-08-31 由只读组的截图自查撞出来）。
        //
        // ⚠ 仍是两份手写清单（跨仓，没有编译期约束），但现在两边都是「全放行」而不是
        // 「逐条列举」，漂移面小了一个量级。守门测试见 `combo_verbs_match_core_whitelist`。
        //
        // 唯二挡住的是 `add_word` / `open_add_word_dialog`：它们要返回占位 composition
        // 激活 C++ 转发全部按键，不符 `dispatch_hotkey` 的 bool 契约，core 的 `BoundAction`
        // 值域里根本没有它们（配了会 warn 后忽略）。这两个键在「按键 → 功能快捷键」页配。
        //
        // ★ 这一条**眼下滤不掉任何东西**（功能列表本就不含加词那两项），留着是因为
        // 「把加词加进功能列表」是个很自然的后续演进，而那时若没有这道闸，用户会配得上、
        // 按下去没反应。写在这里比写在将来那次改动的 review 意见里可靠。
        KeyShape::Combo => !matches!(value, "add_word" | "open_add_word_dialog"),
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

/// 组合键被 Windows 抢走的**确定性**分档。两档措辞不能混用，见
/// [`system_reserved_combo_notice`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SystemKeyRisk {
    /// OS / 宿主窗口层直接消费，输入法没有机会。
    Unreachable,
    /// Windows「输入语言热键」的默认值，**多数环境下仍能正常工作**。
    MayBeTaken,
}

/// `Alt+<主键>` 里会被窗口层吃掉的那几个：主键 → 系统功能名。
///
/// ⚠️ 只有这三个，**不是所有 Alt 组合**。`Alt+<字母>` 只在当前窗口有菜单栏时打开菜单，
/// 那是应用层行为不是 OS 截胡；把整个 Alt 族划进来会让 `alt+m` 这类正常配法挂上噪音。
const ALT_WINDOW_KEYS: &[(&str, &str)] = &[
    ("tab", "切换窗口"),
    ("space", "窗口菜单"),
    ("f4", "关闭窗口"),
];

/// Windows「输入语言的热键」对话框里那三个**默认已分配**的键：键 → 它在系统里的功能。
///
/// `.` 与 `period` 两种写法都列：本对话框的 Lead 表用字面 `.`（`manifest::hotkey_keys`），
/// 而手写配置里的 `period` 经 `key_index` 的「当前值追加」路径同样到得了这里。
///
/// ⚠️ **`Ctrl+Shift+<数字>` 与 `左Alt+Shift+<数字>` 刻意不列**，虽然它们出自同一个系统
/// 对话框（每个输入法的「启用键序列」槽）：那些槽**默认未分配**，只在装了多个输入法时
/// 才可能被 Windows 指派；而本仓出厂的 `delete_candidate` 用的正是 `ctrl+shift+number`
/// 模板 —— 给这一族挂「系统可能占用」会把一个**本程序自己占着**的键说成是 Windows 占的，
/// 指错方向比不提示更坏。真要提示那族，该提的是重复绑定，不是系统占用。
const IME_LANGUAGE_HOTKEYS: &[(&str, &str)] = &[
    ("ctrl+space", "中/英文切换"),
    ("shift+space", "全角/半角切换"),
    ("ctrl+.", "中/英文标点切换"),
    ("ctrl+period", "中/英文标点切换"),
];

/// Windows 可能**先于输入法**拿走这个组合键。返回 `(确定性档位, 完整提示语)`。
///
/// 起因是用户把 `Ctrl+Space` / `Shift+Space` 配成工具栏开关后「按下没反应」，而日志里
/// 什么都不会有 —— 键根本没到我们这一层，和「绑定没生效」完全同形。本模块开头那条
/// 「不过滤的后果是用户配得上、按下去没反应」在这里换了个来源：不是 core 拒绝，是 OS 截胡。
///
/// ⚠️ **只列这个对话框真的配得出来的键**。值域是 `manifest::hotkey_keys()`
/// （a-z / 0-9 / f1-f12 / space / tab / enter / 几个标点）—— 没有 Delete、没有 Escape，
/// 所以 `Ctrl+Alt+Del`、`Ctrl+Shift+Esc` 这些经典系统键在这里**配不出来**，列了就是死代码。
/// 守门测试 `every_system_reserved_key_is_actually_configurable` 迭代上面两张 const 表本身，
/// 往表里加一个配不出来的键会直接把它打红。
///
/// （Session 表更窄：只有 `shift+` 前缀且 `session_base_keys()` 里没有 space，两档一条都
/// 命中不了，故不接这个提示 —— 那个前提由 `session_table_cannot_hit_system_keys` 钉住。）
///
/// ★ 第二档措辞必须留有余地：出厂的 `toggle_punct` 用的就是 `Ctrl+.`，实测能用。
/// 写成「不要用」会把一批可用配置吓退，而那比不提示更糟。
///
/// ★★ 第二档是**全串精确匹配**，`Ctrl+Shift+Space` 不会命中 —— 这是有意的：Windows 的
/// 默认值就是光 `Ctrl+Space`，多一个修饰键就是另一个键了。别「顺手」改成 `has_mod` 式的
/// 宽松匹配，那会立刻把 `ctrl+shift+space` 这类正常配法变成误报。
pub(crate) fn system_reserved_combo_notice(key: &str) -> Option<(SystemKeyRisk, String)> {
    // 分段 trim 后再拼回来：两档看到的形状必须一致。只在开头 trim 整串的话，
    // `ctrl + space` 这种手写形态会被第一档的 `has_mod`（它自己 trim 分段）认出来、
    // 却被第二档的全串匹配漏掉。
    let k = key
        .split('+')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("+")
        .to_lowercase();
    // 没有 `+` 就不是组合键（单键与修饰键各有各的提示，不该落到这里）。
    let base = k.rsplit_once('+')?.1;
    let has_mod = |m: &str| k.split('+').any(|p| p == m);

    if has_mod("win") {
        return Some((
            SystemKeyRisk::Unreachable,
            "Win 组合键由 Windows 自己处理，输入法收不到，请换一个键。".to_string(),
        ));
    }
    if has_mod("alt")
        && let Some((_, what)) = ALT_WINDOW_KEYS.iter().find(|(b, _)| *b == base)
    {
        return Some((
            SystemKeyRisk::Unreachable,
            format!("这是 Windows 的窗口快捷键（{what}），输入法收不到，请换一个键。"),
        ));
    }
    let (_, what) = IME_LANGUAGE_HOTKEYS.iter().find(|(v, _)| *v == k)?;
    Some((
        SystemKeyRisk::MayBeTaken,
        format!(
            "Windows 默认把它作为输入法的「{what}」热键，可能先被系统吃掉。多数情况下仍能正常用；按下没反应就到系统的「输入语言热键」里改掉它，或换一个键。"
        ),
    ))
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
}

impl VerbCategory {
    fn new(name: &str, items: Vec<Verb>) -> Self {
        Self {
            name: name.to_string(),
            items,
        }
    }
}

/// 命令直通的值前缀：`command:<cmdbar 表达式>`，两张表都认。
///
/// 表达式只 trim 首尾，大小写与内部空白原样保留（core 同判据）；空表达式无效。
pub const COMMAND_PREFIX: &str = "command:";

/// 「高级」分类里那一项的值：**光前缀**，表达式另由对话框的文本框提供。
///
/// 光前缀本身不是合法绑定（空表达式），故它只作选择器里的标记，出口
/// [`KeyPicker::commit_value`] 把文本框内容拼上去；文本框为空则不可提交。
/// **不能用空串**：`refresh` 靠「当前功能值」恢复选中项，空值恒查不到，于是每次重算
/// 都把选择弹回第一个分类——那一类点开就跳走。
const COMMAND_MARKER: &str = COMMAND_PREFIX;

/// 剥掉 `command:` 前缀，返回其后的原文（未 trim）；不是命令返回 `None`。
///
/// ★ 判前缀前先 `trim_start`：core 先 trim 整串再认前缀，手写配置里的 `" command:…"`
/// 在 core 那边是命令，这边若不认，就会落成「其它」里的未知动词。**所有判前缀处都走这里**。
fn strip_command(value: &str) -> Option<&str> {
    value.trim_start().strip_prefix(COMMAND_PREFIX)
}

/// 是否为 `command:` 值（不论表达式是否为空）。
pub fn is_command(value: &str) -> bool {
    strip_command(value).is_some()
}

/// `command:` 值里的表达式（trim 首尾）；不是命令或表达式为空返回 `None`。
fn command_expr(value: &str) -> Option<&str> {
    strip_command(value)
        .map(str::trim)
        .filter(|e| !e.is_empty())
}

/// 只读侧的命令标签上限（显示宽度，全角记 2）。总览与编辑器的功能列都是单行截断，
/// 一条 `open("https://…")` 能长到把整行挤没，故在这里先截到一眼读得完的长度。
const COMMAND_LABEL_WIDTH: usize = 36;

/// `command:` 值 → 「运行命令：<表达式>」（过长按显示宽度截断）。两张表共用。
pub fn command_verb_label(value: &str) -> Option<String> {
    let expr = command_expr(value)?;
    Some(format!(
        "运行命令：{}",
        truncate_width(expr, COMMAND_LABEL_WIDTH)
    ))
}

/// 按显示宽度截断，超出时以「…」收尾（「…」计入宽度）。
///
/// 宽度只粗分两档：ASCII 记 1，其余记 2。表达式里绝大多数是 ASCII，偶有中文路径
/// ——按字符数截的话，一串中文会比同样字数的英文宽一倍。
fn truncate_width(s: &str, max: usize) -> String {
    let w = |c: char| if c.is_ascii() { 1 } else { 2 };
    if s.chars().map(w).sum::<usize>() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        if used + w(c) > max - 2 {
            break;
        }
        used += w(c);
        out.push(c);
    }
    out.push('…');
    out
}

/// 「高级」分类：调用直通命令。选中后对话框出现表达式输入框（见 [`KeyPicker`]）。
fn advanced_category() -> VerbCategory {
    VerbCategory::new(
        "高级",
        vec![Verb {
            value: COMMAND_MARKER.to_string(),
            label: "调用直通命令".to_string(),
        }],
    )
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
            "temp_pinyin" | "temp_english" | "aux_code" | "rare_char" | "reverse" => &mut modes,
            "toggle_mode" | "switch_engine" | "toggle_full_width" | "toggle_punct"
            | "toggle_s2t" | "toggle_t2s" | "toggle_toolbar" => &mut states,
            "open_settings" | "take_screenshot" | "softkeyboard" => &mut tools,
            s if s.starts_with("mix:") || s.starts_with("special:") => &mut modes,
            s if s.starts_with("toggle_schema:") || s.starts_with("switch_schema:") => &mut schemas,
            // 命令不逐条进列表：「高级」类里那一项代表**所有**命令，表达式由输入框回填
            // （`KeyPicker::select_verb`）。落「其它」的话，同一条绑定会在两处各出现一次。
            s if is_command(s) => continue,
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
/// - 选词 `select_candidate:` 给全 1..=10——候选窗每页最多开到 10 个（数字键 0 选第
///   10 个，core 侧 `SessionAction::parse` 认 1..=10）。此前只给 2、3 是因为「全列出来会
///   把下拉撑成一长条」，分了类之后这个理由不成立，而截断的代价是「想把某个键配成第 5
///   候选」在界面上根本表达不出来。
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
        // 「上屏当前高亮候选」即空格的功能，排在序号档之前：它不看位次，跟着高亮走，
        // 与方向键 / 高亮移动配合时才是用户要的那个「确认」。没有候选时 core 不吃键。
        VerbCategory::new(
            "选择候选",
            std::iter::once(Verb {
                value: "commit_highlighted".into(),
                label: "上屏当前高亮候选（同空格）".into(),
            })
            .chain(ord("select_candidate:", 10))
            .collect(),
        ),
        VerbCategory::new("以词定字", ord("select_char:", 4)),
        // 「上屏编码」恒上屏编码，不跟「回车键」那项设置——那是回车这个键的配置。
        VerbCategory::new(
            "编辑",
            vec![
                Verb {
                    value: "commit_code".into(),
                    label: "上屏编码".into(),
                },
                Verb {
                    value: "cancel".into(),
                    label: "放弃当前输入（同 Esc）".into(),
                },
            ],
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
    // 命令先于分类表判：分类表里那一项是光前缀的**选择器标记**，空表达式 core 不认，
    // 查到它就会把一条无效配置显示成「调用直通命令」。
    if is_command(value) {
        return command_verb_label(value);
    }
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
    // 什么）——两者要求相反，故值域在此**独立**按 core 的上限判，不复用分类表。
    // `select_candidate:` 认到 10（候选窗最多 10 页内项），`select_char:` 仍是 1..=9。
    for (prefix, max) in [("select_candidate:", 10u8), ("select_char:", 9u8)] {
        if let Some(rest) = value.strip_prefix(prefix) {
            return rest
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|n| (1..=max).contains(n))
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
    /// 是否提供「组合键」形态。方案级 `[key_actions]` 为 false，见 [`KeyPicker::disallow_combo`]。
    combo_allowed: Cell<bool>,
    /// 「修改」打开时的原功能值（「添加」为空）。命令表达式没改就原样交回它，
    /// 见 [`KeyPicker::commit_value`]。
    orig_verb: RefCell<String>,
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
    /// 「调用直通命令」的表达式输入框。只在选中那一项时显示（[`Self::command_on`]）。
    command_text: Signal<String>,
    /// 当前选中的是否为「调用直通命令」。由 `refresh` 现算，驱动输入框的显隐。
    command_on: Signal<bool>,
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
            command_text: signal(String::new()),
            command_on: signal(false),
            data: Rc::new(PickerData {
                table,
                single: RefCell::new(single.into_iter().map(|(v, _)| v).collect()),
                combo: RefCell::new(combo.into_iter().map(|(v, _)| v).collect()),
                all: RefCell::new(all),
                view: RefCell::new(Vec::new()),
                leading: RefCell::new(Vec::new()),
                editing: Cell::new(None),
                combo_allowed: Cell::new(true),
                orig_verb: RefCell::new(String::new()),
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

    /// 不提供「组合键」形态（方案级 `[key_actions]` 用）。
    ///
    /// core 只从**全局** `keys.key_actions` 编组合键热键表，方案级只按单键 VK 查——方案级
    /// 的组合键对任何动词都不生效，而 core 不打算补这个缺口。
    ///
    /// 「修改」一条已有的方案级组合键时，形态切换仍会露出来（否则用户看不到自己配的是
    /// 什么，也切不回单键），但组合键形态下不可提交，并说明原因。
    pub fn disallow_combo(&self) {
        self.data.combo_allowed.set(false);
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
        self.data.orig_verb.borrow_mut().clear();
        self.title.set("添加按键".to_string());
        self.mode.set(0);
        for m in &self.mods {
            m.set(false);
        }
        self.single_idx.set(0);
        self.combo_idx.set(0);
        self.cat_idx.set(0);
        self.item_idx.set(0);
        self.command_text.set(String::new());
        self.refresh();
        self.visible.set(true);
    }

    /// 打开「修改」，把已有绑定回填进各控件。
    pub fn open_edit(&self, id: RowId, key: &str, verb: &str) {
        self.data.editing.set(Some(id));
        *self.data.orig_verb.borrow_mut() = verb.to_string();
        self.title.set("修改按键".to_string());
        // 先清空：非命令绑定不该带着上一次打开时的表达式。是命令的话 `select_verb` 回填。
        self.command_text.set(String::new());

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
    ///
    /// `command:<表达式>` 选中「高级」类的那一项并把表达式回填进输入框。
    fn select_verb(&self, value: &str) {
        let value = match strip_command(value) {
            Some(expr) => {
                self.command_text.set(expr.trim().to_string());
                COMMAND_MARKER
            }
            None => value,
        };
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

    /// 当前选中的功能值（空串 = 没有可选项）。选中「调用直通命令」时是光前缀标记，
    /// 表达式不在这里——要落盘的完整值只由 [`Self::commit_value`] 拼。
    fn current_verb(&self) -> String {
        self.data
            .view
            .borrow()
            .get(self.cat_idx.get())
            .and_then(|c| c.items.get(self.item_idx.get()))
            .map(|v| v.value.clone())
            .unwrap_or_default()
    }

    /// 「确定」要落下去的 `(键名, 功能值)`；不可提交时返回 `None`。
    ///
    /// **提交的唯一出口**。按钮的置灰（`can_confirm`）与这里用的是同一条判据——分两处
    /// 各写一遍的话，只要哪天某条路径绕过了置灰（键盘回车、脚本、测试），空表达式的
    /// `command:` 就会真的写进配置，而 core 对它只 warn 后忽略。这类「两处必须一致」
    /// 的约束本仓已栽过四次。
    fn commit_value(&self) -> Option<(String, String)> {
        if !self.data.combo_allowed.get() && self.mode.get() == 1 {
            return None;
        }
        let (key, mut verb) = (self.current_key(), self.current_verb());
        if verb == COMMAND_MARKER {
            let text = self.command_text.get();
            let expr = text.trim();
            let orig = self.data.orig_verb.borrow();
            verb = if expr.is_empty() {
                String::new()
            } else if command_expr(&orig) == Some(expr) {
                // 表达式没改就交回原值：按 trim 重拼会把手写的 `" command: x "` 变成
                // `command:x`，编辑器随之判「已修改」，而用户什么都没动。
                orig.clone()
            } else {
                format!("{COMMAND_PREFIX}{expr}")
            };
        }
        (!key.is_empty() && !verb.is_empty()).then_some((key, verb))
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
                    c.items.retain(|it| verb_allowed(shape, &it.value));
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
        set_str_if_changed(self.notice, self.notice_text(&key, filtered));
        set_if_changed_bool(self.command_on, self.current_verb() == COMMAND_MARKER);
        set_if_changed_bool(self.can_confirm, self.commit_value().is_some());
    }

    /// 按当前键形态被滤掉的功能条数。
    fn filtered_count(&self) -> usize {
        let live = |cats: &[VerbCategory]| -> usize { cats.iter().map(|c| c.items.len()).sum() };
        live(&self.data.all.borrow()).saturating_sub(live(&self.data.view.borrow()))
    }

    fn notice_text(&self, key: &str, filtered: bool) -> String {
        // 排最前：这一条说的是「整条绑定都不会生效」，比键为空、形态过滤都更根本。
        if !self.data.combo_allowed.get() && self.mode.get() == 1 {
            return SCHEMA_COMBO_NOTICE.to_string();
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
                // 系统占用排在形态过滤**之前**：它说的是「这个键可能根本到不了输入法」，
                // 比「这个功能不能绑这个形态」更靠前地决定了配了有没有用。
                if let Some((_, n)) = system_reserved_combo_notice(key) {
                    return n;
                }
                // ★ 说明**只在真的滤掉了东西时**才出。恒挂就成了常驻噪音，而它要传达的
                // 恰恰是「你现在少看到了几项」这个一次性的事实。
                //
                // ⛔ 这几条一律写成**单行字面量**，不用反斜杠续行：rustfmt 会把续行接回一行，
                // 被反斜杠吞掉的缩进随之变成**字面空格**，用户看到的提示中间夹一大段空白。
                // 上面辅助码那条就是这么中招的（已修）。
                match key_shape(key) {
                    // 组合键现在与单键同值域（2026-09-18 合流），只剩加词那两项配不了，
                    // 而它们本就不在功能列表里 ⇒ 这一支实际到不了。留着是与
                    // `verb_allowed` 那道闸配套：真滤掉了东西就得说得出原因。
                    KeyShape::Combo if filtered => {
                        "加词类功能要接管后续按键，不能在这里配，请到「按键 → 功能快捷键」里设置。"
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
        // 不给组合键的实例（方案级）平时隐藏形态切换；正在编辑一条已有组合键时才露出来，
        // 让用户看得见它、切得回单键。
        let data = self.data.clone();
        let mode_row = Element::row()
            .width_match()
            .child(
                Element::segmented(vec!["单个按键".to_string(), "组合键".to_string()], mode)
                    .weight(1.0),
            )
            .visible_when(move || data.combo_allowed.get() || mode.get() == 1);

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

        // 选中「调用直通命令」时，功能项下拉换成表达式输入框：那一类只有一项，
        // 下拉留着也选不出别的，占的位置正好给输入框。
        let command_on = self.command_on;
        let verb_col = Element::col()
            .width_match()
            .spacing(8)
            .child(section_title("功能"))
            .child(Element::dropdown_signal(self.cat_labels, self.cat_idx).width(200))
            .child(
                Element::dropdown_signal(self.item_labels, self.item_idx)
                    .width(280)
                    .visible_when(move || !command_on.get()),
            )
            .child(
                Element::text_input(self.command_text, "如 proc.run(\"charmap.exe\")")
                    .width_match()
                    .visible_when(move || command_on.get()),
            )
            .child(
                Element::label(command_help(table))
                    .font_size(11.5)
                    .fg_role(Role::TextMuted)
                    .width_match()
                    .max_lines(3)
                    .visible_when(move || command_on.get()),
            );

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
        let ct = self.command_text;
        let effect = crate::pages::dict::reactive::reactive_effect(
            move || {
                mi.version()
                    .wrapping_add(si.version())
                    .wrapping_add(ci.version())
                    .wrapping_add(ki.version())
                    .wrapping_add(ct.version())
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

/// 方案级组合键的行提示：方案按键编辑器的行与按键总览的「不生效」行共用这一句。
/// 行里的提示列很窄，故只说结论；去哪儿配由选择器里 [`SCHEMA_COMBO_NOTICE`] 讲。
pub const SCHEMA_COMBO_HINT: &str = "组合键仅全局有效";

/// 方案级选择器停在组合键形态时的说明。⛔ 单行字面量。
const SCHEMA_COMBO_NOTICE: &str = "组合键只在全局按键功能中有效，方案里配了不会生效。请改选单个按键，或到「按键 → 全局自定义按键」里配。";

/// 「调用直通命令」输入框下的说明。
///
/// ⛔ 单行字面量，理由见 [`aux_code_backtick_notice`]。
fn command_help(table: BindingTable) -> &'static str {
    match table {
        BindingTable::Lead => {
            "按下该键时执行一条命令，写法同工具栏自定义按钮：如 proc.run(\"charmap.exe\")、open(\"https://…\")、key.tap(\"Ctrl+Shift+P\")，也可以直接写 $CC(…)。"
        }
        BindingTable::Session => {
            "输入过程中按下该键时执行一条命令，当前输入保持不动。写法同工具栏自定义按钮：如 proc.run(\"charmap.exe\")、web.search(…)，也可以直接写 $CC(…)。"
        }
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
    /// 归一化后的物理键 token，**只用于与上面那张表比对**，不显示。
    ///
    /// 与显示串分开存：显示要的是「Ctrl+数字」这样一行读得懂的话，比对要的是展开后的
    /// 十个具体键（`Ctrl+0`…`Ctrl+9`）——用显示串比，`Ctrl+1` 撞上模板就检不出来。
    pub tokens: Vec<String>,
    /// 与上面那张表的某条绑定撞了。由 [`foreign_section`] 每次刷新时现算，不落存储。
    pub conflict: bool,
}

/// [`ForeignBinding`] 的展示行：`[按键] [功能] [来源]`，无按钮。
///
/// 列宽与 [`binding_row`] 的前两列对齐——两组上下相邻，错开一格就会显得是两张表。
///
/// 撞了上面那张表的行整行转 [`Role::Warning`]（橙），末列换成「上面已占用」。
/// ★ 用 `Warning` 而不是 `Danger`：这不是错误配置，是**两处都合法、只有一处生效**
/// ——内核有确定的裁决（上面那张赢），红色会把可解释的行为报成待修的故障。同
/// `ConflictLevel::Coexist` 不进常驻警告条的理由。
fn foreign_row(b: &ForeignBinding) -> Element {
    let role = if b.conflict {
        Role::Warning
    } else {
        Role::TextMuted
    };
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(0, 3)
        .child(
            // 比 `binding_row` 的键列宽（130）且允许折行：本组的键列装的可能是**一整组**
            // 的符号（`PageUp/PageDown、- =`），比单个键名长一倍不止。130 下 `PageUp/`
            // 就断在斜杠上，`Tab/Shift+Tab` 只剩 `Tab/Shift`——看起来像配了个别的键。
            // 与上一组差 40px 不影响阅读：中间隔着折叠头，本来就是两块。
            Element::label(&b.keys)
                .font_size(12.5)
                .fg_role(role)
                .width(170)
                .max_lines(2),
        )
        .child(
            Element::label(&b.label)
                .font_size(12.5)
                .fg_role(role)
                .weight(1.0)
                .max_lines(1),
        )
        .child(
            Element::label(if b.conflict {
                "上面已占用"
            } else {
                &b.source
            })
            .font_size(11.0)
            .fg_role(role)
            .max_lines(1),
        )
}

/// 只读组的整块（折叠头 + 列表 + 驱动折叠的 effect）。返回 `None` 表示本编辑器不带这一组。
///
/// 方案级 `[key_actions]` 那个实例**不传**：专用字段全是全局的，在方案对话框里列出来
/// 会让人以为它们也能按方案配。
///
/// `used_tokens` 现算「上面那张表当前占用了哪些键」（归一化后），用于逐行标橙。
///
/// `version` 由**调用方**组合它自己的全部依赖（行版本 ⊕ 只读组内容版本）——判变依据与
/// 数据源必须同源：少了行版本，用户删掉一条绑定后橙色不退；少了内容版本，loader 送来的
/// 真实热键值不会被重算。折叠态由本函数自己叠进去，调用方不必管。
pub fn foreign_section(
    all: std::rc::Rc<RefCell<Vec<ForeignBinding>>>,
    shown: Signal<Vec<ForeignBinding>>,
    open: Signal<bool>,
    count_text: Signal<String>,
    used_tokens: std::rc::Rc<dyn Fn() -> std::collections::HashSet<String>>,
    version: std::rc::Rc<dyn Fn() -> u64>,
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
    //
    // ★ 冲突**折叠时也算**：撞车数要进标题的计数里，那正是让用户知道该展开看看的东西。
    // 只有行的渲染受折叠影响。代价是每次编辑都遍历一遍上面那张表（几十行，可忽略）。
    let refresh = {
        let all = all.clone();
        let used_tokens = used_tokens.clone();
        move || {
            let used = used_tokens();
            let rows: Vec<ForeignBinding> = all
                .borrow()
                .iter()
                .map(|b| ForeignBinding {
                    conflict: b.tokens.iter().any(|t| used.contains(t)),
                    ..b.clone()
                })
                .collect();
            let hit = rows.iter().filter(|b| b.conflict).count();
            count_text.set(if rows.is_empty() {
                String::new()
            } else if hit > 0 {
                format!("（{} 项，其中 {hit} 项与上面重复）", rows.len())
            } else {
                format!("（{} 项，改动请去各自的设置项）", rows.len())
            });
            shown.set(if open.get() { rows } else { Vec::new() });
        }
    };
    let sync = crate::pages::dict::reactive::reactive_effect(
        move || u64::from(open.get()) | (version() << 1),
        refresh,
    );

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

    /// ★★ 候选键组展开表的键名拼写必须落在会话态键值域内。
    ///
    /// `CANDIDATE_GROUP_KEYS` 与 core 的四张 `*_group_binds` 是跨仓手写清单，无编译期
    /// 约束；拼错一个（`pgup` / `left_bracket` / 大写）的表现是**少标一个橙色**——不报错、
    /// 不崩，只是提示悄悄失灵，正是本仓最难查的那类。
    ///
    /// 判据用 `session_base_keys()`：它注释里写明「与 core `session_key_to_vk` 的 match 臂
    /// 一一对应」，是本仓离 core 键名最近的一份。两边同时写错同一个名字才骗得过它。
    #[test]
    fn candidate_group_keys_are_valid_session_keys() {
        let base = session_base_keys();
        let mut checked = 0;
        for g in crate::key_conflict::candidate_group_names() {
            let keys = crate::key_conflict::candidate_group_keys(g);
            assert!(!keys.is_empty(), "组 {g:?} 展开为空，标橙时它恒不匹配");
            for k in keys {
                // 会话态只认 `shift+` 一种前缀（core `session_key_name_to_vk`）。
                let bare = k.strip_prefix("shift+").unwrap_or(k);
                assert!(
                    base.contains(&bare),
                    "组 {g:?} 展开出的键名 {k:?} 不在会话态键值域里——拼错的后果是这一组                     永远标不上橙色，而不会有任何报错"
                );
                checked += 1;
            }
        }
        assert!(checked >= 20, "只校验到 {checked} 个键名，表像是被清空了");
    }

    /// 组合键的值域必须与 core 的 `hotkey_action_entry` 对齐。
    ///
    /// ★ 判据取自**出厂配置**而不是凭记忆列举：`data/config.toml` 里出厂就有
    /// `"ctrl+shift+k" = "softkeyboard"`，任何让出厂配置显示成「不支持」的闸门
    /// 都是错的——那是全新安装打开对话框第一眼就能看到的红字。
    ///
    /// ⚠️ 这一侧与 core 那侧没有编译期约束（跨仓、两份手写）。2026-09-18 合流后两边
    /// 都是「全放行」而不是逐条列举，漂移面小了一个量级，但仍不是零。
    #[test]
    fn combo_verbs_match_core_whitelist() {
        for v in [
            "softkeyboard",
            "softkeyboard:punct",
            "toggle_schema:english",
            "switch_schema:wubi86",
            "special:kf",
            // 命令直通：core 为它放开了组合键白名单（2026-09-27）。
            "command:proc.run(\"charmap.exe\")",
            // 不是「功能」而是压制手段：低层（data/config.toml、定制版的
            // data_custom/config.toml）在 key_actions 里写的组合键绑定，用户只能靠
            // 显式 none 关掉——深合并表达不了删除。core 那侧在组合键分支静默跳过它。
            "none",
        ] {
            assert!(
                verb_allowed(KeyShape::Combo, v),
                "组合键动词 {v:?} 被判为不支持，而 core 的 hotkey_action_entry 认它"
            );
        }
        // 反向：加词那两个在组合键上确实不受支持（不符 `dispatch_hotkey` 的 bool 契约，
        // core 的 `BoundAction` 值域里根本没有它们）。别为了「看起来更统一」放开——
        // 那样用户配得上、按下去没反应，比标灰难查得多。
        for v in ["add_word", "open_add_word_dialog"] {
            assert!(
                !verb_allowed(KeyShape::Combo, v),
                "{v:?} 不该在组合键上放行：它要接管后续按键，core 那边配了也会 warn 后忽略"
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

    /// ★★ 组合键的可绑功能 == `BoundAction` 的全值域（2026-09-18 合流）。
    ///
    /// 收紧的后果是**能力凭空少一半**——这正是合流前的状态：用户在「自定义按键」里选
    /// 「组合键」，能配的功能比选「单个按键」少一大截，而组合键那条通路在按键链上位次
    /// 更早、不与输入争键，本该限制最少。放宽（把加词类也放进来）的后果则是用户配得上、
    /// 按下去什么都不发生，core 只在日志里 warn 一句，与「没绑上」同形。
    #[test]
    fn combo_keys_allow_the_whole_bound_action_domain() {
        for v in [
            // C 类切方案
            "toggle_schema:english",
            "switch_schema:wubi",
            // B 类进 overlay
            "special:fuhao",
            "temp_pinyin",
            "temp_english",
            "aux_code",
            "rare_char",
            "reverse",
            "mix:quick_mix",
            "single_char",
            "single_char:on",
            "softkeyboard",
            "softkeyboard:punct",
            // A 类状态切换
            "toggle_mode",
            "switch_engine",
            "toggle_full_width",
            "toggle_punct",
            "toggle_s2t",
            "toggle_t2s",
            "toggle_toolbar",
            "open_settings",
            "take_screenshot",
            // 见 `verb_allowed` 里那段：压制低层同键绑定的唯一手段，不是功能动词。
            "none",
        ] {
            assert!(verb_allowed(KeyShape::Combo, v), "{v} 应允许绑组合键");
        }
        for v in ["add_word", "open_add_word_dialog"] {
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

    /// 「高级」恒为末类，两张表都有，且只有「调用直通命令」那一项。
    #[test]
    fn advanced_category_is_last_in_both_tables() {
        for cats in [
            categorize_lead_verbs(&lead_verbs()),
            session_verb_categories(),
        ] {
            let last = cats.last().expect("至少有高级类");
            assert_eq!(last.name, "高级");
            let vals: Vec<&str> = last.items.iter().map(|v| v.value.as_str()).collect();
            assert_eq!(vals, vec![COMMAND_MARKER]);
        }
    }

    /// 已有的 `command:` 绑定归「高级」那一项代表，不落「其它」——落了的话同一条绑定
    /// 在选择器里出现两次，且「其它」里那条带着「当前值」字样，像是配坏了。
    #[test]
    fn command_verbs_fold_into_advanced() {
        let mut vs = lead_verbs();
        vs.push(Verb {
            value: "command:proc.run(\"charmap.exe\")".into(),
            label: "whatever".into(),
        });
        let cats = categorize_lead_verbs(&vs);
        let other = cats.iter().find(|c| c.name == "其它").expect("应有其它类");
        assert!(
            other
                .items
                .iter()
                .all(|v| !v.value.starts_with(COMMAND_PREFIX)),
            "command: 不该落「其它」"
        );
        assert_eq!(cats.last().expect("高级").items.len(), 1);
    }

    /// 两个序号档的上限**不同**，两边都要钉住——一样的写法很容易在后续维护里被「统一」
    /// 成同一个数，而它们不同的理由各自独立（见 `session_verb_categories` 的文档）：
    /// 选词 1..=10 是「候选窗每页最多开到 10 个」，以词定字 1..=4 是「第 5 字以上恒定落进
    /// overflow，与没绑上同形」。
    #[test]
    fn session_ordinal_verbs_cover_intended_domain() {
        let cats = session_verb_categories();
        for (name, prefix, max) in [
            ("选择候选", "select_candidate:", 10u8),
            ("以词定字", "select_char:", 4u8),
        ] {
            let c = cats.iter().find(|c| c.name == name).expect(name);
            // 「选择候选」里还有 `commit_highlighted`，只数序号档本身。
            let vals: Vec<String> = c
                .items
                .iter()
                .map(|v| v.value.clone())
                .filter(|v| v.starts_with(prefix))
                .collect();
            let want: Vec<String> = (1..=max).map(|n| format!("{prefix}{n}")).collect();
            assert_eq!(vals, want, "{name} 的序号档应为 1..={max}");
        }
    }

    /// 会话态动词值必须都在 core `SessionAction` 的值域里（拼写守门）。
    ///
    /// ⚠️ 序号上界这里恒按 core 的上限判（`select_candidate:` 1..=10、`select_char:`
    /// 1..=9），**不跟着选择器的收窄档位走**：本测断言的是「不超出 core 值域」，写入侧收窄
    /// 后照样成立。改成等值比较就把两件事绑死了——各自给到第几档是产品取舍
    /// （`session_ordinal_verbs_cover_intended_domain` 管），core 认到第几档是拼写守门
    /// （本测管）。
    ///
    /// ⚠ 与 core 的 `SessionAction::parse` 是两份手写清单，没有编译期约束——本仓已有过
    /// 「两仓键名拼写不一致致静默不匹配」的现场。
    #[test]
    fn session_verb_values_match_core_domain() {
        for c in session_verb_categories() {
            for v in c.items {
                if v.value == COMMAND_MARKER {
                    continue; // 选择器标记，落盘时拼上表达式（见 `commit_value`）
                }
                let ok = matches!(
                    v.value.as_str(),
                    "none"
                        | "commit_highlighted"
                        | "commit_code"
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
                    .map(|n| (n, 10u8))
                    .or_else(|| v.value.strip_prefix("select_char:").map(|n| (n, 9u8)))
                    .is_some_and(|(n, max)| n.parse::<u8>().is_ok_and(|n| (1..=max).contains(&n)));
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

    /// 选了组合键，功能列表**一项都不少**——2026-09-18 合流后组合键与单键同值域。
    ///
    /// 这条此前断言的是相反的事（「只剩 core 认的那两类，并给出一行说明」）。那个断言
    /// 忠实反映了当时的实现，而实现本身是演进残留：组合键这条通路位次更早、不与输入
    /// 争键，能配的功能却恒比单键少一大截。
    #[test]
    fn combo_selection_keeps_the_whole_verb_list() {
        let p = lead_picker();
        p.open_edit(1, "ctrl+shift+r", "toggle_schema:english");
        let combo_names: Vec<String> = p
            .data
            .view
            .borrow()
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert!(
            combo_names.contains(&"切换方案".to_string()),
            "组合键应保留「切换方案」：{combo_names:?}"
        );
        assert!(
            combo_names.contains(&"状态切换".to_string()),
            "组合键现在也能绑「状态切换」（含中英文切换）：{combo_names:?}"
        );
        assert_eq!(p.filtered_count(), 0, "组合键上不该有功能被滤掉");
        assert!(
            p.notice.get().is_empty(),
            "没滤掉东西就不该挂说明，否则成了常驻噪音：{:?}",
            p.notice.get()
        );

        // ★ 与**修饰键**逐类比对，而不是只看「含不含某一类」：修饰键是三种形态里值域
        // 最全的一条（`verb_allowed` 对它恒真），组合键与它一致才说明真的一项不少。
        p.open_edit(1, "rshift", "none");
        let modifier_names: Vec<String> = p
            .data
            .view
            .borrow()
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(
            combo_names, modifier_names,
            "组合键的功能分类应与修饰键完全一致"
        );
    }

    /// 任何一次「功能被滤掉」都必须有说明。
    ///
    /// 没有说明的过滤 = 用户选了个键，整类功能无声消失，界面上没有一处说得出原因。
    /// 这正是那轮改动想消灭的「配不出来还不知道为什么」，却在 Char 那一支上漏了。
    ///
    /// ⚠️ 合流后**只剩有字符的键还在滤**（滤掉「用来离开英文态」的那几个），组合键与
    /// 修饰键都是全放行。故这里只留 `backslash` 一个正例——把不再过滤的键留在列表里，
    /// 前提断言会红在「测不到东西」上，而那不是缺陷。
    #[test]
    fn every_filtering_shape_explains_itself() {
        let p = lead_picker();
        p.open_edit(1, "backslash", "none");
        assert!(
            p.filtered_count() > 0,
            "前提：有字符的键上应当有功能被滤掉，否则这条测不到东西"
        );
        assert!(
            !p.notice.get().is_empty(),
            "滤掉了功能却没有任何说明：{:?}",
            p.notice.get()
        );
        // 不滤的两种形态都不该挂说明，否则成了常驻噪音。
        for key in ["rshift", "ctrl+shift+r"] {
            p.open_edit(1, key, "none");
            assert_eq!(p.filtered_count(), 0, "{key} 上不该有功能被滤掉");
            assert!(
                p.notice.get().is_empty(),
                "{key} 没滤掉东西时不该有说明：{:?}",
                p.notice.get()
            );
        }
    }

    /// ★★ 系统占用提示列的每一个键，都必须**真能在这个对话框里配出来**。
    ///
    /// 这条守的是本仓反复出现的一类：写一道护栏，而它的条件恒不成立 —— 护栏从没生效，
    /// 「没报过」被当成「没问题」。这里的具体形态是「列了 `Ctrl+Alt+Del`，而基础键下拉里
    /// 根本没有 Delete」，那条提示永远出不来，读代码的人却以为已经覆盖了。
    ///
    /// ⚠️ **迭代 const 表本身**，不另写一份平行的字面量清单：手写清单与实现之间没有任何
    /// 强制对应，往实现里加一支配不出来的键，平行清单照样绿 —— 那就是把本测试要防的
    /// 缺陷原样复制到了上一层。初版正是那么写的，而且两份清单当场就已经不同步
    /// （实现有 `ctrl+period`，清单里没有）。
    #[test]
    fn every_system_reserved_key_is_actually_configurable() {
        let base: Vec<String> = combo_base_keys(BindingTable::Lead)
            .into_iter()
            .map(|(v, _)| v)
            .filter(|v| !v.is_empty())
            .collect();

        for (b, _) in ALT_WINDOW_KEYS {
            assert!(
                base.iter().any(|x| x == b),
                "alt+{b} 的基础键不在组合键下拉的值域里，这条提示永远出不来"
            );
            assert_eq!(
                system_reserved_combo_notice(&format!("alt+{b}")).map(|(r, _)| r),
                Some(SystemKeyRisk::Unreachable),
                "alt+{b} 应判为「收不到」"
            );
        }

        for (k, _) in IME_LANGUAGE_HOTKEYS {
            assert_eq!(
                system_reserved_combo_notice(k).map(|(r, _)| r),
                Some(SystemKeyRisk::MayBeTaken),
                "{k} 应判为「可能被占」"
            );
            let kb = k.rsplit_once('+').expect("表里恒是组合键").1;
            // `period` 是唯一的例外：它不在下拉值域里，靠 `key_index` 的「当前值追加」
            // 路径到达（手写配置 `ctrl+period` 打开对话框时会被追加成一个选项）。
            // 豁免它，而不是把它从表里删掉 —— 删了手写配置就没提示了。
            if kb == "period" {
                continue;
            }
            assert!(
                base.iter().any(|x| x == kb),
                "{k} 的基础键 {kb} 既不在下拉值域里、也不是已知的「当前值」豁免项"
            );
        }

        // Win 那一支与基础键无关（任意主键都成立），单独验一条，并确认 Win 复选框
        // 在 Lead 表**确实渲染**——Session 表只给 Shift，那边这一支恒不可达。
        assert_eq!(
            system_reserved_combo_notice("win+a").map(|(r, _)| r),
            Some(SystemKeyRisk::Unreachable)
        );
        assert_eq!(
            system_reserved_combo_notice("ctrl+win+a").map(|(r, _)| r),
            Some(SystemKeyRisk::Unreachable)
        );
    }

    /// Session 表接不上系统占用提示的**前提**：它的基础键里没有 space。
    ///
    /// `system_reserved_combo_notice` 的文档据此说明「Session 表一条都命中不了，故不接」。
    /// 那是一句对**别处数据**的断言，没有任何编译期约束 —— 哪天有人往
    /// `session_base_keys()` 加个 space，Session 表就静默少了一条本该有的提示。
    #[test]
    fn session_table_cannot_hit_system_keys() {
        assert!(
            !session_base_keys().contains(&"space"),
            "Session 表现在配得出 shift+space 了，请把 system_reserved_combo_notice 接上去"
        );
    }

    /// 反向：普通组合键不得挂提示，否则变成常驻噪音。
    ///
    /// ⚠️ `ctrl+alt+m` 与 `ctrl+shift+u` 这类是**最常见的正确配法**，误报一次就会让用户
    /// 对整条提示失去信任 —— 那比不提示更糟。`alt+m` 也必须干净：Alt 组合只有窗口那三个
    /// 键被系统吃，不是所有 Alt 组合。
    #[test]
    fn ordinary_combos_get_no_system_notice() {
        for key in [
            "ctrl+alt+m",
            "ctrl+shift+u",
            "ctrl+shift+b",
            "alt+m",
            "alt+1",
            "shift+f5",
            "ctrl+enter",
            // 多一个修饰键就是另一个键：Windows 的默认值是光 Ctrl+Space。
            "ctrl+shift+space",
            // 出厂 delete_candidate 用的就是这一族，刻意不列（见 IME_LANGUAGE_HOTKEYS 注释）。
            "ctrl+shift+1",
            "alt+shift+1",
            // 单键与修饰键根本不该进这个判断。
            "backtick",
            "rshift",
            "",
        ] {
            assert!(
                system_reserved_combo_notice(key).is_none(),
                "{key:?} 不该挂系统占用提示"
            );
        }
    }

    /// 手写配置的形态（分段带空格、大小写）两档都要认，不能一档认一档漏。
    #[test]
    fn hand_written_spacing_and_case_hit_both_tiers() {
        for key in ["ctrl + space", "Ctrl+Space", "CTRL + SPACE"] {
            assert_eq!(
                system_reserved_combo_notice(key).map(|(r, _)| r),
                Some(SystemKeyRisk::MayBeTaken),
                "{key:?} 应与规范形态判得一样"
            );
        }
        assert_eq!(
            system_reserved_combo_notice("win + a").map(|(r, _)| r),
            Some(SystemKeyRisk::Unreachable)
        );
    }

    /// 两档措辞的**确定性不同**，不能混为一谈。
    ///
    /// ★ `Ctrl+.` 是出厂 `toggle_punct` 的键、实测能用。若把第二档也写成「收不到」，
    /// 等于告诉用户出厂配置是坏的。
    #[test]
    fn two_tiers_word_their_certainty_differently() {
        for key in ["win+a", "alt+tab", "alt+space", "alt+f4"] {
            let (risk, text) =
                system_reserved_combo_notice(key).unwrap_or_else(|| panic!("{key} 应有提示"));
            assert_eq!(risk, SystemKeyRisk::Unreachable);
            assert!(text.contains("收不到"), "第一档应明确说收不到：{text}");
        }
        for (key, _) in IME_LANGUAGE_HOTKEYS {
            let (risk, text) =
                system_reserved_combo_notice(key).unwrap_or_else(|| panic!("{key} 应有提示"));
            assert_eq!(risk, SystemKeyRisk::MayBeTaken);
            assert!(
                text.contains("多数情况下仍能正常用"),
                "第二档必须留有余地（出厂 toggle_punct 用的就是 Ctrl+.）：{text}"
            );
            assert!(!text.contains("收不到"), "第二档不得写成确定收不到：{text}");
        }
    }

    /// 提示文案里不得混入连续空白。    /// 提示文案里不得混入连续空白。
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
            // ★ 系统占用那三条**必须在列**：它们是本文件最新加的文案，而这条测试原先
            // 的键表一个都走不到 `system_reserved_combo_notice` —— 护栏对新文案根本没在跑，
            // 与它自己要防的那类缺陷同形。三个基础键都在 `hotkey_keys()` 里，走得通。
            ("ctrl+space", "toggle_toolbar"),
            ("alt+tab", "toggle_toolbar"),
            ("win+a", "toggle_toolbar"),
            ("", "none"),
        ] {
            if key.is_empty() {
                p.open_new();
            } else {
                p.open_edit(1, key, verb);
            }
            let n = p.notice.get();
            assert!(!n.contains("  "), "提示里混入了连续空白：{n:?}");
            assert!(!n.contains('\n'), "提示里混入了换行：{n:?}");
        }
        // 命令说明也一样（它不经 notice_text，是输入框下的静态标签）。
        for table in [BindingTable::Lead, BindingTable::Session] {
            let h = command_help(table);
            assert!(!h.contains("  "), "命令说明混入连续空白：{h:?}");
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

    /// 在当前 view 里选中「调用直通命令」（模拟用户点「高级」分类）。
    fn pick_command(p: &KeyPicker) -> usize {
        let adv = p
            .data
            .view
            .borrow()
            .iter()
            .position(|c| c.name == "高级")
            .expect("应有高级类");
        p.cat_idx.set(adv);
        p.refresh();
        adv
    }

    /// ★★ 选中命令但表达式为空（或全是空白）时不能提交——core 对空表达式只 warn 后忽略，
    /// 落盘了就是一条「配了没反应」的绑定。两张表都要守。
    #[test]
    fn command_with_empty_expression_cannot_commit() {
        let lead = lead_picker();
        lead.open_edit(1, "backslash", "mix:quick_mix");
        let session = KeyPicker::new(BindingTable::Session);
        session.open_edit(1, "tab", "page_next");
        for p in [lead, session] {
            let adv = pick_command(&p);
            assert_eq!(
                p.cat_idx.get(),
                adv,
                "★ 选中高级类后不能被 refresh 弹回第一类——否则那一类点开就跳走"
            );
            assert!(p.command_on.get(), "选中命令时应显示表达式输入框");
            for blank in ["", "   "] {
                p.command_text.set(blank.to_string());
                p.refresh();
                assert!(
                    !p.can_confirm.get(),
                    "表达式 {blank:?} 时「确定」必须不可点"
                );
                assert!(p.commit_value().is_none(), "空表达式绝不能落盘");
            }
        }
    }

    /// 非空表达式可提交，值为 `command:<表达式>`：首尾空白去掉，大小写与内部空白原样。
    #[test]
    fn command_with_expression_commits_prefixed_value() {
        let lead = lead_picker();
        lead.open_edit(1, "backslash", "mix:quick_mix");
        let session = KeyPicker::new(BindingTable::Session);
        session.open_edit(1, "tab", "page_next");
        for (p, key) in [(lead, "backslash"), (session, "tab")] {
            pick_command(&p);
            p.command_text
                .set("  web.search(\"Hello  World\")  ".to_string());
            p.refresh();
            assert!(p.can_confirm.get());
            assert_eq!(
                p.commit_value(),
                Some((
                    key.to_string(),
                    "command:web.search(\"Hello  World\")".to_string()
                ))
            );
        }
        // 切走再切回来，已填的表达式不丢；切走时提交的是别的功能，不带表达式。
        let p = lead_picker();
        p.open_edit(1, "backslash", "command:proc.run(\"a.exe\")");
        let none = p
            .data
            .view
            .borrow()
            .iter()
            .position(|c| c.name == "不启用")
            .expect("不启用");
        p.cat_idx.set(none);
        p.refresh();
        assert!(!p.command_on.get());
        assert_eq!(p.commit_value().map(|(_, v)| v), Some("none".to_string()));
        pick_command(&p);
        assert_eq!(p.command_text.get(), "proc.run(\"a.exe\")");
    }

    /// 「修改」已有的命令绑定：选中「高级」并把表达式回填进输入框；原样确定不变形。
    /// 随后打开一条非命令绑定，输入框不得残留上一条的表达式。
    #[test]
    fn command_binding_round_trips_on_edit() {
        let lead = lead_picker();
        let session = KeyPicker::new(BindingTable::Session);
        for (p, key) in [(&lead, "ctrl+alt+c"), (&session, "shift+tab")] {
            let v = "command:$CC(\"字符映射\", proc.run(\"charmap.exe\"))";
            p.open_edit(3, key, v);
            assert!(p.command_on.get(), "{key}：应落在命令项");
            assert_eq!(
                p.command_text.get(),
                "$CC(\"字符映射\", proc.run(\"charmap.exe\"))"
            );
            assert_eq!(p.commit_value(), Some((key.to_string(), v.to_string())));
            assert!(
                p.data
                    .all
                    .borrow()
                    .iter()
                    .flat_map(|c| c.items.iter())
                    .all(|it| !it.label.contains("当前值")),
                "命令绑定不该被当成未知动词追加「当前值」"
            );
        }
        lead.open_edit(4, "backslash", "mix:quick_mix");
        assert!(
            lead.command_text.get().is_empty(),
            "非命令绑定不得带着旧表达式"
        );
        assert!(!lead.command_on.get());
    }

    /// 前缀前带空白的手写值（core 先 trim 整串再认前缀）同样认作命令：回填、标签、
    /// 分类都不能把它当成未知动词。
    #[test]
    fn command_prefix_tolerates_leading_whitespace() {
        let v = "  command: proc.run(\"a.exe\") ";
        assert!(is_command(v));
        assert_eq!(
            session_verb_label(v).as_deref(),
            Some("运行命令：proc.run(\"a.exe\")")
        );
        let mut vs = lead_verbs();
        vs.push(Verb {
            value: v.into(),
            label: v.into(),
        });
        assert!(
            categorize_lead_verbs(&vs)
                .iter()
                .all(|c| c.items.iter().all(|it| it.value != v)),
            "带前导空白的命令不该落「其它」"
        );
        let p = lead_picker();
        p.open_edit(1, "ctrl+alt+c", v);
        assert!(p.command_on.get());
        assert_eq!(p.command_text.get(), "proc.run(\"a.exe\")");
    }

    /// ★ 修改已有命令绑定、表达式没动就确定，必须**原样**交回原值——按 trim 重拼会把
    /// 手写的 `" command: x "` 改写成 `command:x`，编辑器随之判「已修改」。
    /// 表达式真改了才重拼。
    #[test]
    fn unchanged_command_commits_original_value() {
        let orig = " command:  key.tap(\"Ctrl+Shift+P\")  ";
        for p in [lead_picker(), KeyPicker::new(BindingTable::Session)] {
            let key = if p.data.table == BindingTable::Lead {
                "ctrl+alt+c"
            } else {
                "shift+tab"
            };
            p.open_edit(1, key, orig);
            assert_eq!(
                p.commit_value(),
                Some((key.to_string(), orig.to_string())),
                "没改表达式就该交回原值"
            );
            p.command_text.set("key.tap(\"Ctrl+Shift+Q\")".to_string());
            p.refresh();
            assert_eq!(
                p.commit_value().map(|(_, v)| v),
                Some("command:key.tap(\"Ctrl+Shift+Q\")".to_string())
            );
        }
        // 「添加」没有原值可比，恒按 trim 重拼。
        let p = lead_picker();
        p.open_edit(1, "ctrl+alt+c", orig);
        p.open_new();
        p.open_edit_key_only("backslash", "command:x");
        assert_eq!(
            p.commit_value().map(|(_, v)| v),
            Some("command:x".to_string())
        );
    }

    /// 选中命令后换键形态（组合键 → 单键），表达式与选中项都要保留：`refresh` 按新形态
    /// 重新过滤时，命令这一项在单键上仍合法，不该被弹回别的分类，也不该清掉用户填的字。
    #[test]
    fn switching_key_shape_keeps_command_selection() {
        let p = lead_picker();
        p.open_edit(1, "ctrl+alt+c", "command:proc.run(\"a.exe\")");
        assert_eq!(p.mode.get(), 1);
        p.mode.set(0);
        p.single_idx
            .set(key_index(&p.data.single, p.single_labels, "backslash"));
        p.refresh();
        assert_eq!(p.current_key(), "backslash");
        assert!(p.command_on.get(), "换成单键后仍应选中命令");
        assert_eq!(p.command_text.get(), "proc.run(\"a.exe\")");
        assert_eq!(
            p.commit_value(),
            Some((
                "backslash".to_string(),
                "command:proc.run(\"a.exe\")".to_string()
            ))
        );
    }

    /// ★★ 走 `build_dialog` 的真实链路：只改输入框里的表达式，「确定」的可用性要跟着变。
    ///
    /// 直接调 `refresh` 的测试证明不了这一点——输入框只写 `command_text`，要靠挂在对话框
    /// 里的 `reactive_effect` 监听它的版本才会重算；漏监听的表现是「填了表达式，确定仍是灰的」。
    /// 这里把对话框建进一棵 `Tree`，由 `layout_root`（它先广播 `on_update`）驱动 effect。
    #[test]
    fn typing_expression_toggles_confirm_through_dialog_effect() {
        use windui::core::Tree;
        use windui::geometry::Size;
        let p = lead_picker();
        p.open_edit(1, "backslash", "mix:quick_mix");
        let mut tree = Tree::new();
        let root = p.build_dialog().build(&mut tree);
        tree.root = Some(root);
        let frame = |tree: &mut Tree| {
            tree.layout_root(Size::new(800, 600), &mut windui::text::LineAwareTextEngine)
        };
        frame(&mut tree);

        let adv = p
            .data
            .view
            .borrow()
            .iter()
            .position(|c| c.name == "高级")
            .expect("高级");
        p.cat_idx.set(adv); // 只动信号，不手调 refresh
        frame(&mut tree);
        assert!(p.command_on.get(), "effect 应随分类切换重算");
        assert!(!p.can_confirm.get(), "空表达式时确定应置灰");

        p.command_text
            .set("open(\"https://example.com\")".to_string());
        frame(&mut tree);
        assert!(p.can_confirm.get(), "填了表达式，effect 应把确定点亮");

        p.command_text.set("   ".to_string());
        frame(&mut tree);
        assert!(!p.can_confirm.get(), "清空表达式，确定应再次置灰");
    }

    /// ★★ 方案级选择器（`disallow_combo`）：新增时停在单键档；「修改」一条已有组合键时
    /// 显示得出它、但不可提交并说明原因；切回单键即可提交。全局选择器不受影响。
    #[test]
    fn schema_level_picker_refuses_combo_keys() {
        let p = lead_picker();
        p.disallow_combo();

        p.open_new();
        assert_eq!(p.mode.get(), 0, "新增停在单键档");

        p.open_edit(1, "ctrl+shift+r", "toggle_schema:english");
        assert_eq!(p.mode.get(), 1, "已有组合键要显示得出来");
        assert_eq!(p.current_key(), "ctrl+shift+r");
        assert!(!p.can_confirm.get(), "方案级组合键不可提交");
        assert!(p.commit_value().is_none());
        assert_eq!(p.notice.get(), SCHEMA_COMBO_NOTICE);

        p.mode.set(0);
        p.single_idx
            .set(key_index(&p.data.single, p.single_labels, "rshift"));
        p.refresh();
        assert_eq!(
            p.commit_value(),
            Some(("rshift".to_string(), "toggle_schema:english".to_string())),
            "切回单键后应能提交，功能保留"
        );
        assert_ne!(p.notice.get(), SCHEMA_COMBO_NOTICE);

        // 全局实例：同一条组合键照常可提交。
        let g = lead_picker();
        g.open_edit(1, "ctrl+shift+r", "toggle_schema:english");
        assert!(g.can_confirm.get(), "全局表的组合键应可提交");
    }

    /// 三种键形态都能选到命令（core 放开了组合键白名单），不被形态过滤滤掉。
    #[test]
    fn command_is_offered_on_every_key_shape() {
        let p = lead_picker();
        for key in ["ctrl+shift+c", "rshift", "backslash"] {
            p.open_edit(1, key, "none");
            assert!(
                find_verb(&p.data.view.borrow(), COMMAND_MARKER).is_some(),
                "{key} 上应选得到「调用直通命令」"
            );
            assert!(verb_allowed(key_shape(key), "command:open(\"x\")"));
        }
    }

    /// 只读侧：命令显示成「运行命令：<表达式>」，过长按显示宽度截断；
    /// `commit_highlighted` 显示中文名；空表达式不给名字（落回显示原值）。
    #[test]
    fn read_side_labels_for_new_verbs() {
        assert_eq!(
            session_verb_label("commit_highlighted").as_deref(),
            Some("上屏当前高亮候选（同空格）")
        );
        assert_eq!(
            session_verb_label("command: key.tap(\"Ctrl+Shift+P\") ").as_deref(),
            Some("运行命令：key.tap(\"Ctrl+Shift+P\")")
        );
        assert_eq!(
            command_verb_label("command:proc.run(\"a.exe\")").as_deref(),
            Some("运行命令：proc.run(\"a.exe\")")
        );
        for bad in ["command:", "command:   "] {
            assert_eq!(command_verb_label(bad), None, "{bad:?}");
            assert_eq!(session_verb_label(bad), None, "{bad:?} 不该显示成命令项");
        }

        let long = format!("command:open(\"https://example.com/{}\")", "a".repeat(80));
        let l = command_verb_label(&long).expect("长命令也要有名字");
        let expr = l.strip_prefix("运行命令：").expect("前缀");
        assert!(expr.ends_with('…'), "过长应截断：{l}");
        let w: usize = expr.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum();
        assert!(w <= COMMAND_LABEL_WIDTH, "截断后宽 {w} 超出上限");
        // 全角按 2 算：同样字数的中文路径截得更早。
        let cjk = command_verb_label(&format!("command:{}", "字".repeat(30))).expect("中文");
        assert_eq!(
            cjk.strip_prefix("运行命令：")
                .expect("前缀")
                .chars()
                .count(),
            (COMMAND_LABEL_WIDTH - 2) / 2 + 1
        );
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
