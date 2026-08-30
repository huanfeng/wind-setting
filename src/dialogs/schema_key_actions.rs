//! 按键功能表（方案 `[key_actions]` / 全局 `keys.key_actions`）编辑对话框。
//!
//! 一行一条绑定：`[按键] [功能] [修改][删除]`，底部「添加」。增删改都走
//! [`crate::dialogs::key_binding::KeyPicker`]，与会话态那张表共用同一个选择器。
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

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::{Map, Value, json};
use windui::prelude::*;

use super::key_binding::{
    Binding, BindingBody, BindingTable, KeyPicker, KeyShape, binding_row, key_shape, verb_allowed,
    verb_requires_modifier,
};

pub use super::key_binding::Verb;

/// 按键功能表的工作态。
#[derive(Clone)]
pub struct KeyActionFields {
    rows: Signal<Vec<Binding>>,
    next_id: Rc<Cell<u64>>,
    verbs: Rc<RefCell<Vec<Verb>>>,
    /// 打开对话框那一刻的内容快照，供脏判定。
    at_open: Rc<RefCell<Vec<(String, String)>>>,
    /// 本方案首码集里的符号键名（core `getConfig` 的 `leadingCodeKeys` 旁路字段）。
    /// 随 `seed` 刷新——同一个对话框服务于任意方案，不刷会拿上一个方案的码元集去提示。
    leading_keys: Rc<RefCell<Vec<String>>>,
    picker: KeyPicker,
}

impl Default for KeyActionFields {
    fn default() -> Self {
        let me = Self {
            rows: signal(Vec::new()),
            next_id: Rc::new(Cell::new(1)),
            verbs: Rc::new(RefCell::new(Vec::new())),
            at_open: Rc::new(RefCell::new(Vec::new())),
            leading_keys: Rc::new(RefCell::new(Vec::new())),
            picker: KeyPicker::new(BindingTable::Lead),
        };
        me.wire_picker();
        me
    }
}

impl KeyActionFields {
    /// 把选择对话框的「确定」接到本表上。**在构造期接一次**，理由同
    /// [`super::session_actions::SessionActionFields`]：接在 `build_body` 里会挂重复回调。
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
            v.sort_by(|a, b| a.key.cmp(&b.key));
            rows.set(v);
        });
    }

    /// 设定功能选项。实例类动词（`mix:<id>` / `special:<id>` / `toggle_schema:<id>`）随
    /// 已安装方案增删，故由调用方从全局配置组装后传入，本模块不依赖 `Config` 结构。
    pub fn set_verbs(&self, verbs: Vec<Verb>) {
        *self.verbs.borrow_mut() = verbs;
        self.sync_picker_verbs();
    }

    /// 把当前动词表推给选择器（分类在那边算）。
    ///
    /// `set_verbs` 与 `seed` 之后**都要调**：`seed` 会为未知动词追加「当前值」条目，
    /// 只在 `set_verbs` 里推的话，那些条目进不了选择器，用户点「修改」看到的是空白。
    fn sync_picker_verbs(&self) {
        self.picker.set_verbs(&self.verbs.borrow());
    }

    /// 从 `schema.getConfig` 的结果播种。
    ///
    /// 未知动词（方案被删、或手写了本版本不认识的值）**保留原值**并进选项：直接丢弃会让
    /// 用户打开一次设置页就静默失去配置。
    pub fn seed(&self, cfg: &Value) {
        // 本方案首码集里的符号键（core 的只读旁路字段）。用来提示「绑了这个键，该符号
        // 在本方案就起不了头了」——内核裁决是绑定优先，码表那边静默失效。
        //
        // 由 core 给键名而非字符集原文：`input_chars` 有区间语法，这边再解析一遍就是
        // 两处慢慢漂移。缺字段（旧版 core）时为空表 = 不提示，不阻塞使用。
        let leading: Vec<String> = cfg
            .get("leadingCodeKeys")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        *self.leading_keys.borrow_mut() = leading.clone();
        self.picker.set_leading_keys(leading);

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
        self.sync_picker_verbs();

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
        *self.at_open.borrow_mut() = pairs;
    }

    /// 从**裸 map** 播种（全局 `keys.key_actions` 用）。
    ///
    /// 与 [`Self::seed`] 的差别只在数据从哪来：那个从方案 cfg 的 `key_actions` 段取，并额外
    /// 读 `leadingCodeKeys` 给「绑的键是本方案码元」提示；全局表没有方案上下文，无此提示
    /// （码元集是 per-schema 的，全局层无从知道）。
    ///
    /// 直接转调 `seed` 而不是复制一份：两条播种路径的「未知动词保留为当前值」「按键名
    /// 排序」「记录 at_open 快照」都必须一致，分两份写迟早漂移。
    pub fn seed_map(&self, map: &Map<String, Value>) {
        self.seed(&json!({ "key_actions": Value::Object(map.clone()) }));
    }

    /// 行数据的版本号，供调用方挂 `reactive_effect` 监听编辑。
    ///
    /// 全局 `keys.key_actions` 那张表**不再自带 writer**（见
    /// `manifest::build_dialog_button_key_actions`），编辑要经此同步到唯一真相源
    /// `SchemaManagerState::key_actions`，否则两处各写各的、按注册顺序互相覆盖。
    pub fn rows_version(&self) -> u64 {
        self.rows.version()
    }

    /// 导出成裸 map（全局 `keys.key_actions` 用）。方案级走 [`Self::apply`]。
    pub fn to_map(&self) -> Map<String, Value> {
        self.current()
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect()
    }

    /// 功能值 → 中文名。查不到显示原值（同 `keys_overview::action_label` 的兜底策略）。
    fn verb_label(&self, value: &str) -> String {
        self.verbs
            .borrow()
            .iter()
            .find(|v| v.value == value)
            .map(|v| v.label.clone())
            .unwrap_or_else(|| value.to_string())
    }

    /// 当前工作态（键名去空白、丢弃空键；**后写覆盖先写**，与 core 侧 Map 语义一致）。
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

    /// 打开以来内容有无变化。
    pub fn is_dirty(&self) -> bool {
        self.current() != *self.at_open.borrow()
    }

    /// 是否存在重复键（保存前提示用）。
    pub fn duplicate_key(&self) -> Option<String> {
        let mut seen: Vec<String> = Vec::new();
        for r in self.rows.get() {
            let k = r.key.trim().to_string();
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

    /// 该键名是否为纯修饰键。转调 [`key_shape`]，与 core 的裁决保持同一判据。
    pub fn is_modifier_key(name: &str) -> bool {
        key_shape(name) == KeyShape::Modifier
    }

    /// 该功能是否**只能绑修饰键**。见 [`verb_requires_modifier`]。
    pub fn verb_requires_modifier(value: &str) -> bool {
        verb_requires_modifier(value)
    }

    /// 按键选择浮层。**必须挂在本编辑器所在浮层之后**，否则会被压在下面。
    pub fn build_picker(&self) -> Element {
        self.picker.build_dialog()
    }

    /// 对话框主体（外壳由 [`super::key_binding::build_binding_dialog`] 套上，
    /// 尺寸与内边距在那里统一决定，**本函数不加内边距**）。
    ///
    /// 「添加」交给外壳的底部按钮栏，与「关闭」同排；放在列表下方会随列表滚动、
    /// 且和内容挤在一起。
    ///
    /// 列表用 `host_signal`（非滚动的响应式 col）而非 `list_signal`：外壳
    /// `build_binding_dialog` 的外壳本身就是一个固定高的滚动区，内嵌第二层滚动会让
    /// 外层按无限高度测量内层、滚轮在两层间打架，结果是列表滚不动、且高度被内层写死
    /// 撑不满外壳视口。按自然高度展开后只保留外壳这一层滚动，两个问题一并消除。
    /// 判据是「父容器是否已在滚动」，不是「要不要响应式」——同文件的码表配置浮层同理。
    pub fn build_body(&self) -> BindingBody {
        let rows_sig = self.rows;
        let picker = self.picker.clone();
        let leading = self.leading_keys.clone();
        let me = self.clone();

        let list = Element::host_signal(rows_sig, move |row: Binding| {
            let shape = key_shape(&row.key);
            // 选择器已按键形态过滤，配不出非法组合；但**历史配置里可能有**（手写的、
            // 或旧版设置页留下的）。行内当场标出来，否则用户只会得到一个安静不工作的
            // 绑定——这类「配了不生效」的求助，本模块已经贡献过一次了。
            let hint = if !verb_allowed(shape, &row.verb) {
                Some(match shape {
                    KeyShape::Combo => ("组合键不支持", Role::Danger),
                    _ => ("需修饰键", Role::Danger),
                })
            } else if shape == KeyShape::Char
                && row.verb != "none"
                && !row.verb.is_empty()
                && leading.borrow().contains(&row.key)
            {
                // 该键同时是本方案的码元首码：内核裁决「绑定优先」，于是这个符号在本方案
                // 起不了头（组码中仍可用）。这是**同层冲突**，两条配置都不算坏，故用中性
                // 措辞告知实际行为，不作错误标红——常驻警告会把可用的设计误报成待修问题。
                //
                // 与上一条互斥：那条是错配（配了不工作），优先级更高；两条一起挂在窄行上
                // 也读不过来。
                Some(("本方案码元", Role::TextMuted))
            } else {
                None
            };

            let (p, r) = (picker.clone(), row.clone());
            let id = row.id;
            binding_row(
                &row.key,
                &me.verb_label(&row.verb),
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

        let content = Element::col()
            .width_match()
            .spacing(6)
            .child(
                // ⚠️ 第二句是 2026-08-30 那条「英文态跳过方案级层」的**唯一**用户可见说明
                // （core 侧只有代码注释）。少了它，用户在英文半角态下按同一个键得到的是
                // 全局那条绑定，而方案页这里明明写着「方案里配的优先」——两句话自相矛盾，
                // 且现象完全看不出成因。
                Element::label(
                    "给按键指定功能。同一个键，方案里配的优先于全局。\
                     英文状态下本页配置不生效，那时按全局配置执行。",
                )
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3),
            )
            .child(list);
        BindingBody::new(content, vec![add])
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

    /// 未知动词保留原值并进选项，不静默丢弃。
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
        // 行里显示成「当前值」而不是空白，用户看得出这条配的是什么。
        assert!(
            f.verb_label("special:gone").starts_with("special:gone"),
            "未知动词的行标签应含原值"
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

    /// 空键名的行被忽略（数据里混进了空键，或将来某条路径产出了空行）。
    #[test]
    fn blank_key_rows_are_dropped() {
        let f = seeded(r#"{"key_actions":{}}"#);
        f.rows.set(vec![Binding {
            id: 1,
            key: "  ".into(),
            verb: "temp_pinyin".into(),
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
        f.rows.set(vec![
            Binding {
                id: 1,
                key: "z".into(),
                verb: "temp_pinyin".into(),
            },
            Binding {
                id: 2,
                key: "z".into(),
                verb: "mix:quick_mix".into(),
            },
        ]);
        assert_eq!(f.duplicate_key().as_deref(), Some("z"));
        // 落盘按「后写覆盖先写」收敛，与 core 侧 Map 语义一致，不会写出两条。
        let mut cfg = json!({});
        f.apply(&mut cfg);
        assert_eq!(cfg["key_actions"], json!({"z":"mix:quick_mix"}));
    }

    /// `leadingCodeKeys` 旁路字段要被 seed 接住，且**随方案刷新**。
    ///
    /// 不刷的后果：同一个对话框服务于任意方案，切到方案 B 后仍拿着方案 A 的码元集提示，
    /// 用户会看到一条指向别的方案的「本方案码元」。
    #[test]
    fn leading_code_keys_refresh_per_seed() {
        let f = KeyActionFields::default();
        f.set_verbs(verbs());
        f.seed(
            &serde_json::from_str(
                r#"{"key_actions":{"semicolon":"mix:quick_mix"},"leadingCodeKeys":["semicolon"]}"#,
            )
            .unwrap(),
        );
        assert_eq!(f.leading_keys.borrow().as_slice(), ["semicolon"]);

        // 换一个方案：字段变了要跟着变。
        f.seed(&serde_json::from_str(r#"{"leadingCodeKeys":["backslash"]}"#).unwrap());
        assert_eq!(f.leading_keys.borrow().as_slice(), ["backslash"]);

        // 旧版 core 不给这个字段 → 空表 = 不提示，而不是沿用上一次的。
        f.seed(&serde_json::from_str(r#"{"key_actions":{}}"#).unwrap());
        assert!(
            f.leading_keys.borrow().is_empty(),
            "缺字段时应清空，否则会拿陈旧码元集提示"
        );
    }

    /// 「限修饰键」的动词分类必须与 core 的 `requires_modifier_key` 一致。
    ///
    /// 两仓各存一份手写清单、无编译期约束，漂移的表现是行内提示与实际行为对不上：
    /// 提示「需修饰键」但配上去照样不工作，或反过来——提示没出、配完却静默失效。
    #[test]
    fn verbs_requiring_modifier_match_core_classification() {
        // 用来「离开英文态」的动作：绑有字符键是单程票。
        for v in ["toggle_mode", "switch_engine", "toggle_schema:english"] {
            assert!(KeyActionFields::verb_requires_modifier(v), "{v} 应限修饰键");
        }
        // 只在中文态有意义的，有字符键可用。
        for v in [
            "toggle_punct",
            "toggle_s2t",
            "toggle_full_width",
            "toggle_toolbar",
            "open_settings",
            "take_screenshot",
            "temp_pinyin",
            "aux_code",
            "mix:quick_mix",
            "special:fuhao",
            "none",
        ] {
            assert!(
                !KeyActionFields::verb_requires_modifier(v),
                "{v} 不该限修饰键"
            );
        }
    }

    /// 修饰键判定与 core `modifier_name_to_vk` 的四个规范名一致。
    #[test]
    fn modifier_predicate_matches_core_names() {
        for k in ["lshift", "rshift", "lctrl", "rctrl"] {
            assert!(KeyActionFields::is_modifier_key(k), "{k} 应是修饰键");
        }
        for k in ["backslash", "semicolon", "z", "grave", "ctrl+shift+r"] {
            assert!(!KeyActionFields::is_modifier_key(k), "{k} 不是修饰键");
        }
    }

    /// `toggle_schema:<id>` 动词能往返：存得进、读得出、不算脏。
    ///
    /// C 类是四期新增的值域，seed/apply 若认不得它，用户手写的配置会在下次保存时
    /// 被静默丢掉——`apply` 走的是「整份 cfg 与方案文件 diff」，认不得等于删掉。
    #[test]
    fn toggle_schema_verb_round_trips() {
        let f = KeyActionFields::default();
        let mut vs = verbs();
        vs.push(Verb {
            value: "toggle_schema:english".into(),
            label: "切换方案 - 英文".into(),
        });
        f.set_verbs(vs);
        f.seed(
            &serde_json::from_str(r#"{"key_actions":{"rshift":"toggle_schema:english"}}"#).unwrap(),
        );
        assert!(!f.is_dirty(), "原样播种不该算脏");
        let mut cfg = serde_json::json!({});
        f.apply(&mut cfg);
        assert_eq!(
            cfg.pointer("/key_actions/rshift").and_then(|v| v.as_str()),
            Some("toggle_schema:english")
        );
    }

    /// 组合键条目（方案切换热键写进来的那种）必须能原样往返。
    ///
    /// 这类条目**本就存在**于全局 `keys.key_actions` 里——方案设置的「切换到本方案按键」
    /// 写的就是它。编辑器若在往返中把它弄丢或改形，用户在方案页配的切换键会在打开一次
    /// 全局按键设置后消失。
    #[test]
    fn combo_key_entries_round_trip() {
        let f = KeyActionFields::default();
        let mut vs = verbs();
        vs.push(Verb {
            value: "toggle_schema:wubi".into(),
            label: "切换方案 - 五笔".into(),
        });
        f.set_verbs(vs);
        f.seed_map(
            serde_json::from_str::<Value>(r#"{"ctrl+shift+r":"toggle_schema:wubi"}"#)
                .unwrap()
                .as_object()
                .unwrap(),
        );
        assert!(!f.is_dirty(), "组合键条目原样播种不该算脏");
        assert_eq!(
            Value::Object(f.to_map()),
            json!({"ctrl+shift+r":"toggle_schema:wubi"})
        );
    }
}
