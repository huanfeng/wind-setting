//! 方案级按键功能表（`[key_actions]`）编辑对话框。
//!
//! 一行一条绑定：`[键名] [动词下拉] [删除]`，底部「添加」。行数随 `Signal<Vec<Row>>`
//! 变化由 `Element::host_signal` 自动重建。
//!
//! # 与码表配置对话框的区别
//!
//! `schema_codetable` 管的是 `engine.codetable.*`（有「自定义」总开关，关掉即整段回落
//! 基线）；本表在方案**顶层** `key_actions`，与引擎类型无关——拼音方案里用 `\` 进快符
//! 同样合理，故不挂在那个总开关下，也不进 `SPEC_BEHAVIOR_FIELDS`。
//!
//! # 落盘
//!
//! 与双拼布局、码表配置写进**同一份 cfg**（`schema.getConfig` 的结果），由
//! `schema_manager::commit_settings_edits` 统一回传 `schema.saveConfig`。core 那边是
//! 「整份 cfg 与方案文件 diff、结果全量重写 override」，分两次提交会互相覆盖。
//!
//! 语义与值域见 core 仓 `docs/design/schema-key-actions.md`。

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

/// 动词选项：`value` 落盘、`label` 给人看。
#[derive(Clone)]
pub struct Verb {
    pub value: String,
    pub label: String,
}

/// 一行绑定的工作态。
///
/// ★ 键名与动词各自持有 signal，**不**把行内容直接塞进 `Signal<Vec<..>>` 的元素里：
/// `list_signal` 只在数据信号版本变化时重建行，行内编辑若也写数据信号，每敲一个字符
/// 整列表就会被销毁重建 —— 输入框当场失焦、光标回到行首。让行内值走各自的 signal，
/// 数据信号只在**增删行**时变，两类变化就分开了。
#[derive(Clone)]
struct Row {
    /// 键名字符串（落盘用）。由 `key_idx` 下拉回写，本身不直接编辑。
    key: Signal<String>,
    /// 键名下拉的选中索引。与 `key` 并存而非只留其一：下拉要索引，落盘要字符串，
    /// 且未知键名（手写配置里的生僻键）在选项里找不到时索引会落 0，靠 `key` 兜住原值。
    key_idx: Signal<usize>,
    verb: Signal<usize>,
}

/// 方案级按键功能表的工作态。
#[derive(Clone)]
pub struct KeyActionFields {
    rows: Signal<Vec<Row>>,
    verbs: Rc<RefCell<Vec<Verb>>>,
    /// 下拉的显示项。**必须是信号**：对话框结构建于选中方案之前，而 `seed` 可能追加
    /// 「当前值」占位项（见 `seed`）；构建时捕获一份 `Vec<String>` 的话，下拉会永远停在
    /// 第一次构建那一刻的选项集上。
    verb_labels: Signal<Vec<String>>,
    /// 键名下拉的取值表（与 `key_labels` 同序），落盘取值即查它。
    key_values: Rc<RefCell<Vec<String>>>,
    /// 键名下拉的显示项。与 `verb_labels` 同理必须是信号——`seed` 会为手写配置里的
    /// 生僻键追加选项。
    key_labels: Signal<Vec<String>>,
    /// 打开对话框那一刻的内容快照，供脏判定。
    at_open: Rc<RefCell<Vec<(String, String)>>>,
}

impl Default for KeyActionFields {
    fn default() -> Self {
        Self {
            rows: signal(Vec::new()),
            verbs: Rc::new(RefCell::new(Vec::new())),
            verb_labels: signal(Vec::new()),
            key_values: Rc::new(RefCell::new(Vec::new())),
            key_labels: signal(Vec::new()),
            at_open: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

impl KeyActionFields {
    /// 设定动词选项。实例类动词（`mix:<id>` / `special:<id>` / `toggle_schema:<id>`）随
    /// 已安装方案增删，故由调用方从全局配置组装后传入，本模块不依赖 `Config` 结构。
    pub fn set_verbs(&self, verbs: Vec<Verb>) {
        *self.verbs.borrow_mut() = verbs;
        self.sync_labels();
    }

    fn sync_labels(&self) {
        let labels: Vec<String> = self
            .verbs
            .borrow()
            .iter()
            .map(|v| v.label.clone())
            .collect();
        self.verb_labels.set(labels);
    }

    /// 从 `schema.getConfig` 的结果播种。
    ///
    /// 未知动词（方案被删、或手写了本版本不认识的值）**保留原值**并进下拉：直接丢弃会让
    /// 用户打开一次设置页就静默失去配置，和 `z_key_action` 下拉「保留当前配置」同一策略。
    pub fn seed(&self, cfg: &Value) {
        self.init_key_options();
        let mut pairs: Vec<(String, String)> = cfg
            .get("key_actions")
            .and_then(|v| v.as_object())
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                    .collect()
            })
            .unwrap_or_default();
        // 按键名排序：BTreeMap 落盘本就有序，UI 跟着有序，避免每次打开顺序不同。
        pairs.sort_by(|a, b| a.0.cmp(&b.0));

        for (_, action) in &pairs {
            if !self.verbs.borrow().iter().any(|v| &v.value == action) {
                self.verbs.borrow_mut().push(Verb {
                    value: action.clone(),
                    label: format!("{action}（当前值，目标可能已不存在）"),
                });
            }
        }
        self.sync_labels();

        let rows: Vec<Row> = pairs
            .iter()
            .map(|(k, a)| Row {
                key: signal(k.clone()),
                key_idx: signal(self.key_index(k)),
                verb: signal(self.verb_index(a)),
            })
            .collect();
        self.rows.set(rows);
        *self.at_open.borrow_mut() = pairs;
    }

    /// 初始化键名下拉的取值/显示表（幂等）。
    fn init_key_options(&self) {
        if !self.key_values.borrow().is_empty() {
            return;
        }
        let opts = Self::key_options();
        *self.key_values.borrow_mut() = opts.iter().map(|(v, _)| v.clone()).collect();
        self.key_labels
            .set(opts.iter().map(|(_, l)| l.clone()).collect());
    }

    /// 键名 → 下拉索引；不在词表里的（手写配置的生僻键）**追加**为选项，不丢弃。
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

    fn verb_index(&self, value: &str) -> usize {
        self.verbs
            .borrow()
            .iter()
            .position(|v| v.value == value)
            .unwrap_or(0)
    }

    /// 当前工作态（键名去空白、丢弃空键；**后写覆盖先写**，与 core 侧 Map 语义一致）。
    fn current(&self) -> Vec<(String, String)> {
        let verbs = self.verbs.borrow();
        let mut out: Vec<(String, String)> = Vec::new();
        let key_values = self.key_values.borrow();
        for r in self.rows.get() {
            // 键名以下拉选中项为准；索引越界（未播种就构建的空行）回落 Row::key。
            let k = key_values
                .get(r.key_idx.get())
                .cloned()
                .unwrap_or_else(|| r.key.get())
                .trim()
                .to_string();
            if k.is_empty() {
                continue;
            }
            let action = verbs
                .get(r.verb.get())
                .map(|v| v.value.clone())
                .unwrap_or_default();
            if let Some(slot) = out.iter_mut().find(|(ek, _)| *ek == k) {
                slot.1 = action;
            } else {
                out.push((k, action));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// 打开以来内容有无变化。
    pub fn is_dirty(&self) -> bool {
        self.current() != *self.at_open.borrow()
    }

    /// 是否存在重复键（保存前提示用）。
    pub fn duplicate_key(&self) -> Option<String> {
        let mut seen: Vec<String> = Vec::new();
        let key_values = self.key_values.borrow();
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
            if seen.contains(&k) {
                return Some(k);
            }
            seen.push(k);
        }
        None
    }

    /// 写进方案配置。
    ///
    /// 空表写 `{}` 而非 null：core 的 `json_diff` 比对方案文件基线，空对象与基线的空表
    /// 相等即不落 override；写 null 会让 `json_to_toml` 跳过，**清空操作就丢了**——用户
    /// 删光所有绑定后 override 里仍留着旧表。
    pub fn apply(&self, cfg: &mut Value) {
        if !cfg.is_object() {
            *cfg = json!({});
        }
        let mut map = serde_json::Map::new();
        for (k, a) in self.current() {
            map.insert(k, Value::String(a));
        }
        if let Some(o) = cfg.as_object_mut() {
            o.insert("key_actions".to_string(), Value::Object(map));
        }
    }

    /// 行摘要（「未设置」/「N 项」）。
    pub fn summary(&self) -> String {
        let n = self.current().len();
        if n == 0 {
            "未设置".to_string()
        } else {
            format!("{n} 项")
        }
    }

    /// 键名选项：符号键 + `z`。
    ///
    /// 用下拉而非文本框：键名是 core `keymap` 里的固定词表（`backslash` / `grave` …），
    /// 让用户手打等于把一张内部词表当成 API——拼错了也没有任何反馈，配了不生效还查不出。
    ///
    /// # 字母为什么只留 z
    ///
    /// core 侧任意字母都能绑（活码裁决已从 z 泛化到所有字母），但**实践上只有 z 有意义**：
    /// 其余字母在任何码表里几乎都是活码前缀，`bound_action_key_yields` 必然让位，配了等于
    /// 没配。列出来只会让人配完发现没反应，与「临拼触发键里那个失效的 z 选项」同型。
    ///
    /// # 修饰键（LShift/RShift）为什么不在这里
    ///
    /// core 目前**接不住**：① `key_name_to_vk` 查的 `KEY_TABLE` 只含符号键，`lshift`
    /// 解析不出；② 更根本的是修饰键作功能键只能走 **keyup 轻敲**通路（keydown 不能吃、
    /// `Ctrl+A` 的第一下会误触发、长按会连触发），而方案级表的分派点在 keydown 的
    /// `try_activate_mode` 里。两条都要 core 先支持（设计文档四期），在那之前把它们放进
    /// 下拉，就是又造一个「配了不生效」的选项。
    fn key_options() -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = crate::dialogs::schema_manager::TRIGGER_KEY_OPTIONS
            .iter()
            .map(|(v, l)| (v.to_string(), l.to_string()))
            .collect();
        out.push(("minus".into(), "减号 ( - )".into()));
        out.push(("equal".into(), "等号 ( = )".into()));
        out.push(("z".into(), "字母 z".into()));
        out
    }

    /// 对话框内容（外壳由调用方用 `build_group_dialog_shell_dyn` 套上，
    /// 以便与码表配置、扩展词库等其它三级浮层长得一致）。
    ///
    /// 返回 `(内容, 底部按钮)`——「添加绑定」交给外壳的底部按钮栏，与「关闭」同排；
    /// 放在列表下方会随列表滚动、且和内容挤在一起。
    ///
    /// 列表用 `host_signal`（非滚动的响应式 col）而非 `list_signal`：外壳
    /// `build_group_dialog_shell_dyn` 本身就是一个固定高的滚动区，内嵌第二层滚动会让
    /// 外层按无限高度测量内层、滚轮在两层间打架，结果是列表滚不动、且高度被内层写死
    /// 撑不满外壳视口。按自然高度展开后只保留外壳这一层滚动，两个问题一并消除。
    /// 判据是「父容器是否已在滚动」，不是「要不要响应式」——同文件的码表配置浮层同理。
    pub fn build_body(&self) -> (Element, Vec<Element>) {
        let rows_sig = self.rows;
        let labels_sig = self.verb_labels;
        let key_labels_sig = self.key_labels;

        let list = Element::host_signal(rows_sig, move |row: Row| {
            let me = row.key;
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .padding_xy(0, 3)
                .child(Element::dropdown_reactive(key_labels_sig, row.key_idx).width(150))
                .child(Element::dropdown_reactive(labels_sig, row.verb).weight(1.0))
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

        let add = Element::button("+ 添加绑定").small().on_click(move |_| {
            let mut v = rows_sig.get();
            v.push(Row {
                key: signal(String::new()),
                key_idx: signal(0),
                verb: signal(0),
            });
            rows_sig.set(v);
        });

        let content = Element::col()
            .width_match()
            .spacing(6)
            .child(
                Element::label("给本方案的按键指定功能，优先于全局设置。")
                    .font_size(12.0)
                    .fg_role(Role::TextMuted)
                    .width_match()
                    .max_lines(1),
            )
            .child(list);
        (content, vec![add])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verbs() -> Vec<Verb> {
        vec![
            Verb {
                value: "none".into(),
                label: "不启用".into(),
            },
            Verb {
                value: "temp_pinyin".into(),
                label: "临时拼音".into(),
            },
            Verb {
                value: "mix:quick_mix".into(),
                label: "快捷输入".into(),
            },
        ]
    }

    fn seeded(json_str: &str) -> KeyActionFields {
        let f = KeyActionFields::default();
        f.set_verbs(verbs());
        f.seed(&serde_json::from_str(json_str).unwrap());
        f
    }

    /// 播种 → 原样写回，内容不变且不算脏。
    ///
    /// 守的是「打开设置页什么都不动、点保存，配置却变了」——方案 override 走的是
    /// 「整份 cfg 与方案文件 diff」，往返若不等价，用户每开一次设置页就改一次方案。
    #[test]
    fn seed_apply_roundtrip_is_stable() {
        let f = seeded(r#"{"key_actions":{"backslash":"mix:quick_mix","z":"temp_pinyin"}}"#);
        assert!(!f.is_dirty(), "刚播种不应判为脏");
        let mut cfg = json!({});
        f.apply(&mut cfg);
        assert_eq!(
            cfg["key_actions"],
            json!({"backslash":"mix:quick_mix","z":"temp_pinyin"})
        );
    }

    /// 未知动词保留原值并进下拉，不静默丢弃。
    #[test]
    fn unknown_verb_is_preserved() {
        let f = seeded(r#"{"key_actions":{"grave":"special:gone"}}"#);
        assert!(!f.is_dirty(), "未知动词不该被当成改动");
        let mut cfg = json!({});
        f.apply(&mut cfg);
        assert_eq!(
            cfg["key_actions"]["grave"], "special:gone",
            "指向已删方案的绑定必须原样保留——丢弃等于用户打开一次设置页就失去配置"
        );
    }

    /// 删光所有绑定要能落盘成空表，而不是「没有改动」。
    #[test]
    fn clearing_all_rows_is_dirty_and_writes_empty() {
        let f = seeded(r#"{"key_actions":{"z":"temp_pinyin"}}"#);
        f.rows.set(Vec::new());
        assert!(f.is_dirty(), "删光绑定必须判脏，否则清空操作永远提交不了");
        let mut cfg = json!({});
        f.apply(&mut cfg);
        assert_eq!(cfg["key_actions"], json!({}), "空表要写 {{}} 而非 null");
    }

    /// 空键名的行被忽略（用户点了「添加」还没填就保存）。
    #[test]
    fn blank_key_rows_are_dropped() {
        let f = seeded(r#"{"key_actions":{}}"#);
        // key_idx 越界 → current() 回落 Row::key（此处是空白），即「添加了还没选」的行。
        f.rows.set(vec![Row {
            key: signal("  ".into()),
            key_idx: signal(usize::MAX),
            verb: signal(1),
        }]);
        assert!(!f.is_dirty(), "只有空行时不算改动");
        let mut cfg = json!({});
        f.apply(&mut cfg);
        assert_eq!(cfg["key_actions"], json!({}));
    }

    /// 重复键能被检出（供保存前提示）。
    #[test]
    fn duplicate_key_is_detected() {
        let f = seeded(r#"{"key_actions":{}}"#);
        let zi = f.key_index("z");
        f.rows.set(vec![
            Row {
                key: signal("z".into()),
                key_idx: signal(zi),
                verb: signal(1),
            },
            Row {
                key: signal("z".into()),
                key_idx: signal(zi),
                verb: signal(2),
            },
        ]);
        assert_eq!(f.duplicate_key().as_deref(), Some("z"));
        // 落盘按「后写覆盖先写」收敛，与 core 侧 Map 语义一致，不会写出两条。
        let mut cfg = json!({});
        f.apply(&mut cfg);
        assert_eq!(cfg["key_actions"], json!({"z":"mix:quick_mix"}));
    }
}
