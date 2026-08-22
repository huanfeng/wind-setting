//! 会话态按键功能表（`keys.session_actions`）编辑对话框。
//!
//! 一行一条绑定：`[按键] [功能] [修改][删除]`，底部「添加」。增删改都走
//! [`crate::dialogs::key_binding::KeyPicker`]，与引导键那张表共用同一个选择器。
//!
//! # 与 `schema_key_actions` 的关系
//!
//! 两者是**同族编辑器**，共用 `key_binding` 里的键名显示、行渲染与选择对话框。core 侧把
//! 它们分成两张表，判据是**触发态**：
//!
//! | | 表 | 管的是 |
//! | --- | --- | --- |
//! | [`super::schema_key_actions`] | 方案 `[key_actions]` / 全局 `keys.key_actions` | 给这个键指定一个功能（进模式 / 切方案 / 切状态） |
//! | 本模块 | `keys.session_actions` | **输入中**（有编码或候选）这个键干什么 |
//!
//! ⚠️ 分野**不是**「空闲时 vs 输入中」：`key_actions` 的三条通路（组合键热键、修饰键
//! keyup、单字符引导键）在输入中同样触发——「按 Shift 切中英文」正是走的第二条。真正的
//! 差别是本表**只**在有会话时生效，且管的是对当前这段输入与候选的操作。
//!
//! # 为什么键与功能都是选的，不能手打
//!
//! 两者都是 core 内部的固定词表。让用户手打等于把内部词表当 API：拼错了没有任何反馈，
//! 配了不生效还查不出。本仓已有过「两仓键名拼写不一致导致静默不匹配」的现场。
//!
//! 语义与值域见 core 仓 `docs/design/session-key-actions.md`。

use std::cell::Cell;
use std::rc::Rc;

use serde_json::{Map, Value};
use windui::prelude::*;

use super::key_binding::{Binding, BindingTable, KeyPicker, binding_row};

/// 会话态按键表的工作态。
#[derive(Clone)]
pub struct SessionActionFields {
    rows: Signal<Vec<Binding>>,
    next_id: Rc<Cell<u64>>,
    picker: KeyPicker,
}

impl Default for SessionActionFields {
    fn default() -> Self {
        let me = Self {
            rows: signal(Vec::new()),
            next_id: Rc::new(Cell::new(1)),
            picker: KeyPicker::new(BindingTable::Session),
        };
        me.wire_picker();
        me
    }
}

impl SessionActionFields {
    /// 把选择对话框的「确定」接到本表上。**在构造期接一次**：接在 `build_body` 里的话，
    /// 同一份工作态被建两次（如设置树重建）就会挂两个回调，一次确定写两遍。
    fn wire_picker(&self) {
        let rows = self.rows;
        let next_id = self.next_id.clone();
        self.picker.on_commit(move |id, key, verb| {
            let mut v = rows.get();
            match id {
                Some(id) => {
                    if let Some(r) = v.iter_mut().find(|r| r.id == id) {
                        r.key = key;
                        r.verb = verb;
                    }
                }
                None => {
                    let id = next_id.get();
                    next_id.set(id + 1);
                    v.push(Binding { id, key, verb });
                }
            }
            // 按键名排序：core 侧 BTreeMap 落盘本就有序，UI 跟着有序，改完一行也不会
            // 让它从原地跳到列表末尾。
            v.sort_by(|a, b| a.key.cmp(&b.key));
            rows.set(v);
        });
    }

    /// 功能值 → 中文名。查不到显示原值——手写配置里可能有本版本设置页还不认识的动词，
    /// 显示「未知」会让用户以为配错了，而它在 core 里跑得好好的。
    fn verb_label(value: &str) -> String {
        super::key_binding::session_verb_label(value).unwrap_or_else(|| value.to_string())
    }

    /// 从配置里的 map 播种。
    ///
    /// 未知键名/动词（手写了本版本不认识的值）**保留原值**：直接丢弃会让用户打开一次
    /// 设置页就静默失去配置。
    pub fn seed(&self, map: &Map<String, Value>) {
        let mut pairs: Vec<(String, String)> = map
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));

        let mut id = 1u64;
        let rows: Vec<Binding> = pairs
            .iter()
            .map(|(k, a)| {
                let b = Binding {
                    id,
                    key: k.clone(),
                    verb: a.clone(),
                };
                id += 1;
                b
            })
            .collect();
        self.next_id.set(id);
        self.rows.set(rows);
    }

    /// 当前工作态（丢弃空键；**后写覆盖先写**，与 core 侧 Map 语义一致）。
    fn current(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for r in self.rows.get() {
            let k = r.key.trim().to_string();
            if k.is_empty() {
                continue;
            }
            if let Some(slot) = out.iter_mut().find(|(ek, _)| *ek == k) {
                slot.1 = r.verb.clone();
            } else {
                out.push((k, r.verb.clone()));
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

    /// 按键选择浮层。**必须挂在本编辑器所在浮层之后**，否则会被压在下面。
    pub fn build_picker(&self) -> Element {
        self.picker.build_dialog()
    }

    /// 对话框内容 + 底部附加按钮（「添加」交给外壳底部栏，与「关闭」同排）。
    ///
    /// 列表用 `host_signal` 而非 `list_signal`：外壳本身已是固定高的滚动区，内嵌第二层
    /// 滚动会让两层打架（列表滚不动、高度撑不满）。判据是「父容器是否已在滚动」。
    pub fn build_body(&self) -> (Element, Vec<Element>) {
        let rows_sig = self.rows;
        let picker = self.picker.clone();

        let list = Element::host_signal(rows_sig, move |row: Binding| {
            // CapsLock 要靠全局键盘钩子才能拦住（大小写锁定在输入法拿到按键之前就已翻转，
            // 压不住）。这不是错误配置，但用户有权知道自己开启了什么，故中性措辞、不标红。
            // 非 Windows 上该钩子根本不存在（core 侧是 `bail!` 的空壳）。
            let base = row.key.rsplit('+').next().unwrap_or("");
            let hint = (base == "capslock" || base == "caps").then_some((
                if cfg!(windows) {
                    "需全局钩子"
                } else {
                    "本平台不支持"
                },
                Role::TextMuted,
            ));
            let (p, r) = (picker.clone(), row.clone());
            let id = row.id;
            binding_row(
                &row.key,
                &Self::verb_label(&row.verb),
                hint,
                move |_| p.open_edit(r.id, &r.key, &r.verb),
                move |_| {
                    // 按行 id 定位：行序号随增删变化，存下标必然错位。
                    let mut v = rows_sig.get();
                    v.retain(|x| x.id != id);
                    rows_sig.set(v);
                },
            )
        })
        .width_match();

        let p_add = self.picker.clone();
        let add = Element::button("+ 添加").small().on_click(move |_| {
            p_add.open_new();
        });

        // ★ 内边距由**内容**加，外壳（`build_group_dialog_shell`）只管标题栏/滚动区/按钮栏。
        // 与 `build_group_dialog_with_link` 的 `.padding_xy(16, 2)` 对齐，否则同一个设置界面
        // 里会出现两种松紧度的弹窗。垂直给 8 是因为本对话框顶部有说明文字，2 会贴住标题栏。
        let content = Element::col()
            .width_match()
            .padding_xy(16, 8)
            .spacing(8)
            .child(
                Element::label(
                    "指定这些键在输入过程中的功能；没有编码和候选时它们仍是原本的用途。",
                )
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
        // 认不出的动词在行里显示原值，不显示「未知」。
        assert_eq!(
            SessionActionFields::verb_label("some_future_verb"),
            "some_future_verb"
        );
    }

    /// 空表要落 `{}`，用户删光绑定后配置里不该留着旧表。
    #[test]
    fn empty_map_is_written_as_empty_object() {
        let f = seeded(json!({}));
        assert_eq!(Value::Object(f.to_map()), json!({}));
    }

    /// 删光绑定要能落盘成空表，而不是留着旧表。脏判定由外层
    /// [`super::schema_session_actions::SchemaSessionActionFields`] 按 `to_map()` 比对。
    #[test]
    fn clearing_all_rows_writes_empty() {
        let f = seeded(json!({"tab": "page_next"}));
        f.rows.set(Vec::new());
        assert_eq!(Value::Object(f.to_map()), json!({}));
    }

    /// 同一个键配两行时按「后写覆盖先写」收敛，与 core 侧 Map 语义一致。
    #[test]
    fn duplicate_keys_collapse_to_the_last_one() {
        let f = seeded(json!({}));
        f.rows.set(vec![
            Binding {
                id: 1,
                key: "tab".into(),
                verb: "page_next".into(),
            },
            Binding {
                id: 2,
                key: "tab".into(),
                verb: "page_prev".into(),
            },
        ]);
        assert_eq!(Value::Object(f.to_map()), json!({"tab": "page_prev"}));
    }
}
