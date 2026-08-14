//! 会话态按键功能表（`keys.session_actions`）编辑对话框。
//!
//! 一行一条绑定：`[键名下拉] [动词下拉] [删除]`，底部「添加」。
//!
//! # 与 `schema_key_actions` 的关系
//!
//! 两者是**同族编辑器**，形态刻意保持一致（同样的行布局、同样的增删交互、同样的
//! 「未知值保留」策略）。core 侧把它们分成两张表，判据是**触发态**：
//!
//! | | 表 | 管的是 |
//! | --- | --- | --- |
//! | [`super::schema_key_actions`] | 方案 `[key_actions]` / 全局 `keys.key_actions` | **空闲时**（没在打字）这个键干什么 |
//! | 本模块 | `keys.session_actions` | **正在打字时**（有编码或候选）这个键干什么 |
//!
//! 同一个键在两种状态下可以是两个动作——Tab 打字时翻页、空闲时仍是宿主的制表符。
//!
//! ★ 改动其一的**交互形态**时请同步另一个。`capabilities.rs` 对 `keys.key_actions` 的
//! 豁免理由里写着「两者的 UI 应当一并设计，避免出两个长得不一样的键绑定编辑器」——
//! 代码没有共用，靠的是这条注释和相同的行布局。
//!
//! # 为什么键名/动词都用下拉而非文本框
//!
//! 两者都是 core 内部的固定词表。让用户手打等于把内部词表当 API：拼错了没有任何反馈，
//! 配了不生效还查不出。本仓已有过「两仓键名拼写不一致导致静默不匹配」的现场。
//!
//! 语义与值域见 core 仓 `docs/design/session-key-actions.md`。

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Map, Value};
use windui::prelude::*;

/// 一行绑定的工作态。
///
/// ★ 键名与动词各自持有 signal，**不**把行内容直接塞进 `Signal<Vec<..>>` 的元素里：
/// 行内编辑若也写数据信号，每改一次整列表就被销毁重建。理由详见
/// [`super::schema_key_actions`] 的同名结构。
#[derive(Clone)]
struct Row {
    /// 键名字符串（落盘用）。由 `key_idx` 下拉回写，本身不直接编辑。
    key: Signal<String>,
    /// 键名下拉的选中索引。与 `key` 并存：下拉要索引、落盘要字符串，且手写配置里的
    /// 生僻键在选项里找不到时索引会落 0，靠 `key` 兜住原值。
    key_idx: Signal<usize>,
    verb: Signal<usize>,
}

/// 会话态按键表的工作态。
#[derive(Clone)]
pub struct SessionActionFields {
    rows: Signal<Vec<Row>>,
    /// 键名下拉的取值表（与 `key_labels` 同序），落盘取值即查它。
    key_values: Rc<RefCell<Vec<String>>>,
    /// 键名下拉的显示项。**必须是信号**：`seed` 会为手写配置里的生僻键追加选项，
    /// 构建时捕获一份 `Vec<String>` 的话下拉会永远停在初始选项集上。
    key_labels: Signal<Vec<String>>,
    /// 动词下拉的取值表（与 `verb_labels` 同序）。
    verb_values: Rc<RefCell<Vec<String>>>,
    verb_labels: Signal<Vec<String>>,
}

impl Default for SessionActionFields {
    fn default() -> Self {
        let me = Self {
            rows: signal(Vec::new()),
            key_values: Rc::new(RefCell::new(Vec::new())),
            key_labels: signal(Vec::new()),
            verb_values: Rc::new(RefCell::new(Vec::new())),
            verb_labels: signal(Vec::new()),
        };
        me.init_options();
        me
    }
}

impl SessionActionFields {
    fn init_options(&self) {
        let keys = Self::key_options();
        *self.key_values.borrow_mut() = keys.iter().map(|(v, _)| v.to_string()).collect();
        self.key_labels
            .set(keys.iter().map(|(_, l)| l.to_string()).collect());

        let verbs = Self::verb_options();
        *self.verb_values.borrow_mut() = verbs.iter().map(|(v, _)| v.to_string()).collect();
        self.verb_labels
            .set(verbs.iter().map(|(_, l)| l.to_string()).collect());
    }

    /// 键名选项。与 core `wind-keys` 的 `SESSION_FUNCTION_KEYS` + 符号键值域对应。
    ///
    /// 别名（`pgup`/`caps`/`lcontrol` 等）**不列**：它们与主名同义，列出来只会让人以为
    /// 是不同的键。手写配置里用了别名时，`seed` 会把原值当「当前值」追加进选项，不丢配置。
    ///
    /// **CapsLock 仅 Windows 可选**：拦住它需要低级键盘钩子（`WH_KEYBOARD_LL`），
    /// core 侧 `capslock_hook` 在非 Windows 是直接 `bail!` 的空壳。列出来只会让用户
    /// 配一个永远不生效的键。已有配置（如从 Windows 同步过来的）不受影响——`key_index`
    /// 会把词表外的键当「当前值」追加，不丢用户配置。
    fn key_options() -> &'static [(&'static str, &'static str)] {
        #[cfg(not(windows))]
        return &[
            ("tab", "Tab"),
            ("shift+tab", "Shift + Tab"),
            ("pageup", "PageUp"),
            ("pagedown", "PageDown"),
            ("up", "↑ 上"),
            ("down", "↓ 下"),
            ("left", "← 左"),
            ("right", "→ 右"),
            ("home", "Home"),
            ("end", "End"),
            ("lshift", "左 Shift"),
            ("rshift", "右 Shift"),
            ("lctrl", "左 Ctrl"),
            ("rctrl", "右 Ctrl"),
            ("minus", "减号 ( - )"),
            ("equal", "等号 ( = )"),
            ("lbracket", "左方括号 ( [ )"),
            ("rbracket", "右方括号 ( ] )"),
            ("comma", "逗号 ( , )"),
            ("period", "句号 ( . )"),
            ("semicolon", "分号 ( ; )"),
            ("quote", "单引号 ( ' )"),
            ("slash", "斜杠 ( / )"),
            ("backslash", "反斜杠 ( \\ )"),
            ("backtick", "反引号 ( ` )"),
        ];
        #[cfg(windows)]
        &[
            ("tab", "Tab"),
            ("shift+tab", "Shift + Tab"),
            ("capslock", "CapsLock"),
            ("pageup", "PageUp"),
            ("pagedown", "PageDown"),
            ("up", "↑ 上"),
            ("down", "↓ 下"),
            ("left", "← 左"),
            ("right", "→ 右"),
            ("home", "Home"),
            ("end", "End"),
            // 纯修饰键：走 keyup 轻敲通路（按住/组合键不误触发）。
            ("lshift", "左 Shift"),
            ("rshift", "右 Shift"),
            ("lctrl", "左 Ctrl"),
            ("rctrl", "右 Ctrl"),
            ("minus", "减号 ( - )"),
            ("equal", "等号 ( = )"),
            ("lbracket", "左方括号 ( [ )"),
            ("rbracket", "右方括号 ( ] )"),
            ("comma", "逗号 ( , )"),
            ("period", "句号 ( . )"),
            ("semicolon", "分号 ( ; )"),
            ("quote", "单引号 ( ' )"),
            ("slash", "斜杠 ( / )"),
            ("backslash", "反斜杠 ( \\ )"),
            ("backtick", "反引号 ( ` )"),
        ]
    }

    /// 动词选项。与 core `wind_config::SessionAction` 的值域对应。
    ///
    /// `select_candidate:N` / `select_char:N` 的 N 只列常用档：core 侧 N 无上限，但
    /// 次选/三选之外几乎无人使用，全列出来会把下拉撑成一长条。手写配置里的其它 N
    /// 由 `seed` 的「当前值」机制兜住。
    fn verb_options() -> &'static [(&'static str, &'static str)] {
        &[
            ("none", "打字时禁用该键"),
            ("page_next", "下一页"),
            ("page_prev", "上一页"),
            ("highlight_down", "高亮下移"),
            ("highlight_up", "高亮上移"),
            ("cancel", "放弃当前输入（同 Esc）"),
            ("select_candidate:2", "选中第 2 个候选"),
            ("select_candidate:3", "选中第 3 个候选"),
            ("select_char:1", "以词定字：取第 1 字"),
            ("select_char:2", "以词定字：取第 2 字"),
            ("select_char:3", "以词定字：取第 3 字"),
        ]
    }

    /// 从配置里的 map 播种。
    ///
    /// 未知键名/动词（手写了本版本不认识的值）**保留原值**并进下拉：直接丢弃会让用户
    /// 打开一次设置页就静默失去配置。
    pub fn seed(&self, map: &Map<String, Value>) {
        let mut pairs: Vec<(String, String)> = map
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
            .collect();
        // 按键名排序：core 侧 BTreeMap 落盘本就有序，UI 跟着有序，避免每次打开顺序不同。
        pairs.sort_by(|a, b| a.0.cmp(&b.0));

        let rows: Vec<Row> = pairs
            .iter()
            .map(|(k, a)| Row {
                key: signal(k.clone()),
                key_idx: signal(self.key_index(k)),
                verb: signal(self.verb_index(a)),
            })
            .collect();
        self.rows.set(rows);
    }

    /// 键名 → 下拉索引；不在词表里的**追加**为选项，不丢弃。
    fn key_index(&self, name: &str) -> usize {
        if let Some(i) = self.key_values.borrow().iter().position(|v| v == name) {
            return i;
        }
        self.key_values.borrow_mut().push(name.to_string());
        let mut labels = self.key_labels.get();
        labels.push(format!("{name}（当前值）"));
        self.key_labels.set(labels);
        self.key_values.borrow().len() - 1
    }

    /// 动词 → 下拉索引；同样保留未知值。
    fn verb_index(&self, value: &str) -> usize {
        if let Some(i) = self.verb_values.borrow().iter().position(|v| v == value) {
            return i;
        }
        self.verb_values.borrow_mut().push(value.to_string());
        let mut labels = self.verb_labels.get();
        labels.push(format!("{value}（当前值）"));
        self.verb_labels.set(labels);
        self.verb_values.borrow().len() - 1
    }

    /// 当前工作态（丢弃空键；**后写覆盖先写**，与 core 侧 Map 语义一致）。
    fn current(&self) -> Vec<(String, String)> {
        let key_values = self.key_values.borrow();
        let verb_values = self.verb_values.borrow();
        let mut out: Vec<(String, String)> = Vec::new();
        for r in self.rows.get() {
            let k = key_values
                .get(r.key_idx.get())
                .cloned()
                .unwrap_or_else(|| r.key.get())
                .trim()
                .to_string();
            if k.is_empty() {
                continue;
            }
            let action = verb_values.get(r.verb.get()).cloned().unwrap_or_default();
            if let Some(slot) = out.iter_mut().find(|(ek, _)| *ek == k) {
                slot.1 = action;
            } else {
                out.push((k, action));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// 落盘用的 map。
    pub fn to_map(&self) -> Map<String, Value> {
        let mut map = Map::new();
        for (k, a) in self.current() {
            map.insert(k, Value::String(a));
        }
        map
    }

    /// 对话框内容 + 底部附加按钮（「添加绑定」交给外壳底部栏，与「关闭」同排）。
    ///
    /// 列表用 `host_signal` 而非 `list_signal`：外壳本身已是固定高的滚动区，内嵌第二层
    /// 滚动会让两层打架（列表滚不动、高度撑不满）。判据是「父容器是否已在滚动」。
    pub fn build_body(&self) -> (Element, Vec<Element>) {
        let rows_sig = self.rows;
        let key_labels_sig = self.key_labels;
        let verb_labels_sig = self.verb_labels;
        let key_values = self.key_values.clone();
        let verb_values = self.verb_values.clone();

        let list = Element::host_signal(rows_sig, move |row: Row| {
            let me = row.key;
            // CapsLock 要靠全局键盘钩子才能拦住（大小写锁定在输入法拿到按键之前就已
            // 翻转，压不住）。这不是错误配置，但用户有权知道自己开启了什么，故用中性
            // 措辞提示、不标红。
            //
            // 非 Windows 上该钩子根本不存在（core 侧是 `bail!` 的空壳），此时选项里也
            // 没有 CapsLock——能走到这一支只可能是从 Windows 同步过来的既有配置，
            // 那就把话说明白：不是「需要什么」，而是本平台压根不生效。
            let kv = key_values.clone();
            let ki = row.key_idx;
            let is_capslock = move || {
                kv.borrow()
                    .get(ki.get())
                    .is_some_and(|k| k == "capslock" || k == "caps")
            };
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .padding_xy(0, 3)
                .child(Element::dropdown_signal(key_labels_sig, row.key_idx).width(150))
                .child(Element::dropdown_signal(verb_labels_sig, row.verb).weight(1.0))
                .child(
                    Element::label(if cfg!(windows) {
                        "需全局钩子"
                    } else {
                        "本平台不支持"
                    })
                    .font_size(11.0)
                    .fg_role(Role::TextMuted)
                    .max_lines(1)
                    .visible_when(is_capslock),
                )
                .child(
                    Element::button("✕")
                        .small()
                        .outline_soft()
                        .on_click(move |_| {
                            // 按 key signal 的身份定位：行序号随增删变化，存下标必然错位。
                            let mut v = rows_sig.get();
                            v.retain(|r| r.key != me);
                            rows_sig.set(v);
                        }),
                )
        })
        .width_match();

        let vv = verb_values.clone();
        let add = Element::button("+ 添加绑定").small().on_click(move |_| {
            let mut v = rows_sig.get();
            // 新行默认落在「下一页」而非首项「打字时禁用该键」：后者是个破坏性动作，
            // 用户添加一行却忘了改动词时，不该把那个键在打字时废掉。
            let default_verb = vv
                .borrow()
                .iter()
                .position(|x| x == "page_next")
                .unwrap_or(0);
            v.push(Row {
                key: signal(String::new()),
                key_idx: signal(0),
                verb: signal(default_verb),
            });
            rows_sig.set(v);
        });

        // ★ 内边距由**内容**加，外壳（`build_group_dialog_shell`）只管标题栏/滚动区/按钮栏。
        // 与 `build_group_dialog_with_link` 的 `.padding_xy(16, 2)` 对齐，否则同一个设置界面
        // 里会出现两种松紧度的弹窗。垂直给 8 是因为本对话框顶部有说明文字，2 会贴住标题栏。
        let content = Element::col()
            .width_match()
            .padding_xy(16, 8)
            .spacing(8)
            .child(
                Element::label("指定「正在打字时」这些键的功能；空闲时它们仍是原本的用途。")
                    .font_size(12.0)
                    .fg_role(Role::TextMuted)
                    .width_match()
                    .max_lines(2),
            )
            .child(
                Element::label("这里配的键优先于外面的翻页 / 高亮 / 次选 / 以词定字四项。")
                    .font_size(12.0)
                    .fg_role(Role::TextMuted)
                    .width_match()
                    .max_lines(2),
            )
            .child(list);
        (content, vec![add])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn seeded(src: Value) -> SessionActionFields {
        let f = SessionActionFields::default();
        f.seed(src.as_object().unwrap());
        f
    }

    #[test]
    fn seed_then_to_map_roundtrips() {
        let f = seeded(json!({"tab": "page_next", "capslock": "page_prev"}));
        assert_eq!(
            Value::Object(f.to_map()),
            json!({"capslock": "page_prev", "tab": "page_next"})
        );
    }

    /// ★ 未知动词/键名必须保留：直接丢弃会让用户打开一次设置页就静默失去配置。
    #[test]
    fn unknown_values_are_preserved() {
        let f = seeded(json!({"f13": "page_next", "tab": "some_future_verb"}));
        assert_eq!(
            Value::Object(f.to_map()),
            json!({"f13": "page_next", "tab": "some_future_verb"}),
            "生僻键名与未来版本的动词都应原样保留"
        );
    }

    /// 空表要落 `{}`，用户删光绑定后配置里不该留着旧表。
    #[test]
    fn empty_map_is_written_as_empty_object() {
        let f = seeded(json!({}));
        assert_eq!(Value::Object(f.to_map()), json!({}));
    }

    /// 选项表里的每个动词都必须是 core 认识的值域成员（拼写守门）。
    ///
    /// ⚠ 与 core 的 `SessionAction::parse` 是两份手写清单，没有编译期约束——本仓已有过
    /// 「两仓键名拼写不一致致静默不匹配」的现场，故至少把格式钉死在这里。
    #[test]
    fn verb_values_match_core_value_domain() {
        for (v, _) in SessionActionFields::verb_options() {
            let ok = matches!(
                *v,
                "none"
                    | "page_prev"
                    | "page_next"
                    | "highlight_up"
                    | "highlight_down"
                    | "cancel"
                    | "clear"
            ) || v
                .strip_prefix("select_candidate:")
                .or_else(|| v.strip_prefix("select_char:"))
                .is_some_and(|n| n.parse::<u8>().is_ok_and(|n| n >= 1));
            assert!(ok, "动词 {v:?} 不在 core SessionAction 的值域里");
        }
    }

    /// 键名选项不得含别名——别名与主名同义，列出来只会让人以为是不同的键。
    #[test]
    fn key_options_have_no_aliases() {
        for (k, _) in SessionActionFields::key_options() {
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
