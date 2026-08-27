//! 「注释词库」对话框：管理 `[[ui.comment_dicts]]` —— 供候选注释模板 `${dict}` 变量查询的
//! 独立词库列表（英汉释义、Emoji 名称之类）。
//!
//! # 为什么它此前没有界面
//!
//! `ui.comment_dicts` 长期登记在 `capabilities::UNCOVERED_BY_DESIGN` 里，理由是「形态是
//! 可增删的条目列表，manifest 的『一个 key 一个控件』模型套不上」——那条登记同时写明它是
//! **待接入**而非设计上不暴露。本模块就是来销它的。
//!
//! # 顺序即优先级，所以必须能排序
//!
//! core 侧同一个词在多个库里都有注释时**取靠前那个库**的。列表因此用
//! `reorder_list_signal` 拖拽排序，而不是「按 id 排」或「按添加顺序排」——后两者会让
//! 用户没有任何办法表达「我要这个库优先」。
//!
//! # ★ 启用态与条目定义分开存
//!
//! `rows` 只装条目定义，启用开关另存在 `switches`（按 id 索引）。合在一起的话，
//! 每次重建行都要现造一个 `Signal<bool>`，而行是随 `rows` 变化重建的 ⇒ 开关信号
//! 每次都换一个新的，绑在上面的 `on_toggle` 写回旧信号。与工具栏「显示内容」那屏同构。
//!
//! # ★ 写回在原对象上 insert，不重建
//!
//! [`CommentDictRow::raw`] 留着读进来的整个 JSON 对象。core 的 `CommentDictSpec` 将来
//! 加字段时，用户手写的那些值不会被这一屏静默抹掉——`[overlay]` 那节踩过这个坑
//! （用字面量重建对象，把段里本 UI 不管的项一并吃掉）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

/// 一条注释词库的条目定义（对应 core 的一个 `[[ui.comment_dicts]]`）。
///
/// **不含 `enabled`**：那一位存在 [`CommentDictsUi::switches`] 里，理由见模块文档。
#[derive(Clone, PartialEq)]
pub struct CommentDictRow {
    /// 稳定标识。core 用它做日志定位，本屏用它索引开关与挡重名，故要求非空且唯一。
    pub id: String,
    /// 显示名。
    pub label: String,
    /// 词库路径，**相对 `schemas\`**（如 `comments\emoji.dict.yaml`）。
    pub path: String,
    /// 适用方案 id；**空 = 全部方案**（与 core 的 `applies_to` 同义）。
    pub schemas: Vec<String>,
    /// 读进来的原始对象，写回时在它上面 insert。见模块文档。
    pub raw: Value,
}

/// 新建态的 `editing` 哨兵。
pub const NEW_DICT: usize = usize::MAX;

/// 新建 / 编辑一条词库的表单。**两者共用**——差别只在确认时是追加还是替换。
#[derive(Clone)]
pub struct DictForm {
    pub visible: Signal<bool>,
    /// 正在编辑第几条；[`NEW_DICT`] = 新建。
    pub editing: Signal<usize>,
    pub id: Signal<String>,
    pub label: Signal<String>,
    pub path: Signal<String>,
    /// 适用方案，逗号分隔的文本。**留空 = 全部方案**。
    pub schemas: Signal<String>,
    /// 校验失败原因；空 = 无。就地显示，不再弹一层。
    pub error: Signal<String>,
    pub title: Signal<String>,
}

impl DictForm {
    pub fn new() -> Self {
        Self {
            visible: signal(false),
            editing: signal(NEW_DICT),
            id: signal(String::new()),
            label: signal(String::new()),
            path: signal(String::new()),
            schemas: signal(String::new()),
            error: signal(String::new()),
            title: signal("添加注释词库".to_string()),
        }
    }

    pub fn open_new(&self) {
        self.editing.set(NEW_DICT);
        self.id.set(String::new());
        self.label.set(String::new());
        self.path.set(String::new());
        self.schemas.set(String::new());
        self.error.set(String::new());
        self.title.set("添加注释词库".to_string());
        self.visible.set(true);
    }

    pub fn open_edit(&self, index: usize, row: &CommentDictRow) {
        self.editing.set(index);
        self.id.set(row.id.clone());
        self.label.set(row.label.clone());
        self.path.set(row.path.clone());
        self.schemas.set(row.schemas.join("，"));
        self.error.set(String::new());
        self.title.set("编辑注释词库".to_string());
        self.visible.set(true);
    }
}

impl Default for DictForm {
    fn default() -> Self {
        Self::new()
    }
}

/// 「注释词库」对话框的全部可变状态。
#[derive(Clone)]
pub struct CommentDictsUi {
    pub visible: Signal<bool>,
    pub rows: Signal<Vec<CommentDictRow>>,
    /// id → 启用开关。
    pub switches: Rc<RefCell<HashMap<String, Signal<bool>>>>,
    pub form: DictForm,
}

impl CommentDictsUi {
    pub fn new() -> Self {
        Self {
            visible: signal(false),
            rows: signal(Vec::new()),
            switches: Rc::new(RefCell::new(HashMap::new())),
            form: DictForm::new(),
        }
    }

    /// 取某条的启用开关；不存在时**当场建一个**（新建那一刻还没有）。
    pub fn switch_of(&self, id: &str, default_on: bool) -> Signal<bool> {
        if let Some(s) = self.switches.borrow().get(id) {
            return *s;
        }
        let s = signal(default_on);
        self.switches.borrow_mut().insert(id.to_string(), s);
        s
    }

    /// 用配置里的数组播种（打开对话框时调用）。
    pub fn seed(&self, items: &[Value]) {
        let rows = rows_from_value(items);
        self.switches.borrow_mut().clear();
        for (r, on) in rows.iter().zip(enabled_flags(items)) {
            self.switches.borrow_mut().insert(r.id.clone(), signal(on));
        }
        self.rows.set(rows);
    }

    /// 除第 `skip` 条之外的全部 id（供表单挡重名；[`NEW_DICT`] = 一条都不跳）。
    pub fn other_ids(&self, skip: usize) -> Vec<String> {
        self.rows
            .get()
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != skip)
            .map(|(_, r)| r.id.clone())
            .collect()
    }

    /// 提交表单：新建则追加，编辑则替换。
    ///
    /// ⚠️ **改 id 要把开关一起改名**，否则那条的启用态会退回默认——用户改了个显示不到的
    /// 标识，结果词库自己开了或关了。同 `ToolbarItemsUi::commit_form` 的教训。
    pub fn commit_form(
        &self,
        index: usize,
        id: String,
        label: String,
        path: String,
        schemas: Vec<String>,
    ) {
        if index == NEW_DICT {
            self.switch_of(&id, true); // 新建即启用
            self.rows.update(|v| {
                v.push(CommentDictRow {
                    id,
                    label,
                    path,
                    schemas,
                    raw: json!({}),
                })
            });
            return;
        }
        let old_id = self.rows.get().get(index).map(|r| r.id.clone());
        if let Some(old) = old_id.filter(|o| *o != id) {
            let was_on = self
                .switches
                .borrow()
                .get(&old)
                .map(|s| s.get())
                .unwrap_or(true);
            self.switches.borrow_mut().remove(&old);
            self.switch_of(&id, was_on);
        }
        self.rows.update(|v| {
            if let Some(slot) = v.get_mut(index) {
                slot.id = id;
                slot.label = label;
                slot.path = path;
                slot.schemas = schemas;
            }
        });
    }

    /// 删除第 `index` 条（定义与开关一并去掉）。
    pub fn delete(&self, index: usize) {
        let Some(r) = self.rows.get().get(index).cloned() else {
            return;
        };
        self.rows.update(|v| {
            if index < v.len() {
                v.remove(index);
            }
        });
        self.switches.borrow_mut().remove(&r.id);
    }

    /// 当前工作态 → 回写给 core 的数组。
    pub fn to_value(&self) -> Value {
        let sw = self.switches.borrow();
        Value::Array(
            self.rows
                .get()
                .iter()
                .map(|r| {
                    let on = sw.get(&r.id).map(|s| s.get()).unwrap_or(true);
                    row_to_value(r, on)
                })
                .collect(),
        )
    }
}

impl Default for CommentDictsUi {
    fn default() -> Self {
        Self::new()
    }
}

/// 解析配置数组 → 条目定义（纯函数，供测试）。
///
/// ★ 缺 `id` 的条目**用路径兜一个**而不是丢弃：手写配置里 `id` 是可省的（core 只拿它做
/// 日志），丢弃会让用户看到「我明明配了三个库，界面里只有两个」，而那两个一保存就把
/// 第三个抹掉了。
pub fn rows_from_value(items: &[Value]) -> Vec<CommentDictRow> {
    let mut out: Vec<CommentDictRow> = Vec::new();
    for it in items {
        let s = |k: &str| {
            it.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let path = s("path");
        let mut id = s("id");
        if id.is_empty() {
            id = fallback_id(&path, &out);
        }
        let schemas = it
            .get("schemas")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        out.push(CommentDictRow {
            id,
            label: s("label"),
            path,
            schemas,
            raw: it.clone(),
        });
    }
    out
}

/// 各条目的启用位（缺省视为启用，与 core 的 `default_true` 同源）。
pub fn enabled_flags(items: &[Value]) -> Vec<bool> {
    items
        .iter()
        .map(|it| it.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true))
        .collect()
}

/// 给缺 id 的条目兜一个：取路径的文件名，重了就加序号。
fn fallback_id(path: &str, existing: &[CommentDictRow]) -> String {
    let stem = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("");
    let base = if stem.is_empty() { "dict" } else { stem };
    let mut id = base.to_string();
    let mut n = 2;
    while existing.iter().any(|r| r.id == id) {
        id = format!("{base}{n}");
        n += 1;
    }
    id
}

/// 一条 → JSON。在 `raw` 上 insert，本 UI 不管的字段原样保留（见模块文档）。
fn row_to_value(r: &CommentDictRow, enabled: bool) -> Value {
    let mut v = r.raw.clone();
    if !v.is_object() {
        v = json!({});
    }
    let obj = v.as_object_mut().expect("上面刚保证是对象");
    obj.insert("id".into(), json!(r.id));
    obj.insert("label".into(), json!(r.label));
    obj.insert("path".into(), json!(r.path));
    obj.insert("enabled".into(), json!(enabled));
    obj.insert("schemas".into(), json!(r.schemas));
    v
}

/// 「适用方案」输入框 → 方案 id 列表。中英文逗号、分号、空白都当分隔符。
///
/// ⚠️ 用户会照着界面上的「，」打全角逗号，只认半角会得到一个叫「pinyin，english」的
/// 方案 id ——它谁也匹配不上，表现是「限定了方案，结果哪个方案都不显示」。
pub fn parse_schemas(s: &str) -> Vec<String> {
    s.split(['，', ',', '；', ';', ' ', '\t', '、'])
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect()
}

/// 校验表单，返回错误原因（`None` = 可以保存）。纯函数，供测试。
pub fn validate(id: &str, path: &str, existing_ids: &[String]) -> Option<&'static str> {
    let id = id.trim();
    if id.is_empty() {
        return Some("请填写标识（id）");
    }
    if id.contains(char::is_whitespace) {
        return Some("标识不能含空格");
    }
    if existing_ids.iter().any(|x| x == id) {
        return Some("标识重复，换一个");
    }
    if path.trim().is_empty() {
        return Some("请填写词库路径");
    }
    None
}

/// 一条的副标题：路径 · 适用方案。
fn row_desc(r: &CommentDictRow) -> String {
    let scope = if r.schemas.is_empty() {
        "全部方案".to_string()
    } else {
        r.schemas.join("、")
    };
    if r.path.is_empty() {
        scope
    } else {
        format!("{} · {}", r.path, scope)
    }
}

/// 表单里的一行：`标签 [输入框]`。
fn field_row(label: &str, sig: Signal<String>, placeholder: &str) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(Element::label(label).font_size(12.5).width(72))
        .child(
            Element::text_input(sig, placeholder)
                .width_match()
                .height(32)
                .weight(1.0),
        )
}

/// 构建「注释词库」对话框（含二级的新建/编辑表单）。
pub fn build_comment_dicts_dialog(ui: &CommentDictsUi) -> Element {
    let visible = ui.visible;
    let row_ui = ui.clone();

    let list = Element::col().width_match().padding_xy(16, 2).child(
        Element::reorder_list_signal(ui.rows, move |r: CommentDictRow, handle| {
            let idx = row_ui
                .rows
                .get()
                .iter()
                .position(|x| x.id == r.id)
                .unwrap_or(0);
            let sig = row_ui.switch_of(&r.id, true);
            let e_ui = row_ui.clone();
            let d_ui = row_ui.clone();
            let label = if r.label.trim().is_empty() {
                r.id.clone()
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
                            crate::widgets::setting_row(&label, Some(&row_desc(&r)), ctrl)
                                .weight(1.0),
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
        }),
    );

    let new_ui = ui.clone();
    let frame = Element::col()
        .width(560)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("注释词库")
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(
                "供候选注释模板的 ${dict} 变量查询（英汉释义、Emoji 名称等）。\
                 词库文件请放在数据目录的 schemas\\comments\\ 下，这里填相对 schemas\\ 的路径。\n\
                 同一个词在多个库里都有注释时取靠前那个，可拖动左侧手柄调整顺序。",
            )
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(4)
            .padding_xy(18, 10),
        )
        .child(list)
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    Element::button("添加词库…")
                        .small()
                        .outline_soft()
                        .on_click(move |_| new_ui.form.open_new()),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("完成")
                        .small()
                        .on_click(move |_| visible.set(false)),
                ),
        );

    Element::stack()
        .child(Element::dialog(visible, frame))
        .child(build_dict_form_dialog(ui))
}

/// 二级弹层：新建 / 编辑一条词库。
fn build_dict_form_dialog(ui: &CommentDictsUi) -> Element {
    let form = ui.form.clone();
    let fc = form.clone();
    let ui2 = ui.clone();
    let visible = form.visible;

    let frame = Element::col()
        .width(480)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label_signal(form.title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(
                "标识只用于内部定位（不显示），同一个标识不能重复。\n\
                 路径相对 schemas\\，如 comments\\emoji.dict.yaml。\n\
                 适用方案留空 = 全部方案；填多个用逗号分隔，如 pinyin，wubi86。",
            )
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(4)
            .padding_xy(18, 8),
        )
        .child(
            Element::col()
                .width_match()
                .spacing(8)
                .padding_xy(18, 6)
                .child(field_row("标识", form.id, "emoji"))
                .child(field_row("显示名", form.label, "Emoji 名称"))
                .child(field_row("路径", form.path, "comments\\emoji.dict.yaml"))
                .child(field_row("适用方案", form.schemas, "留空 = 全部方案")),
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
                    let id = fc.id.get().trim().to_string();
                    let path = fc.path.get().trim().to_string();
                    let idx = fc.editing.get();
                    if let Some(msg) = validate(&id, &path, &ui2.other_ids(idx)) {
                        fc.error.set(msg.to_string());
                        return;
                    }
                    fc.error.set(String::new());
                    ui2.commit_form(
                        idx,
                        id,
                        fc.label.get().trim().to_string(),
                        path,
                        parse_schemas(&fc.schemas.get()),
                    );
                    fc.visible.set(false);
                })),
        );
    Element::dialog(visible, frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `json!([...])` → `Vec<Value>`（测试里省一次 unwrap 链）。
    fn arr(v: Value) -> Vec<Value> {
        v.as_array().cloned().unwrap_or_default()
    }

    /// ★ 读进来的字段要原样读出去，含本 UI 不认识的那些。
    ///
    /// core 的 `CommentDictSpec` 将来加字段时，这一屏必须不吃掉用户手写的值——
    /// `[overlay]` 那节正是因为用字面量重建对象而抹掉过手写的注释模板。
    #[test]
    fn unknown_fields_survive_round_trip() {
        let src = arr(json!([{
            "id": "emoji", "label": "Emoji", "path": "comments/emoji.dict.yaml",
            "enabled": true, "schemas": ["pinyin"],
            "future_field": "别把我吃了"
        }]));
        let rows = rows_from_value(&src);
        let out = row_to_value(&rows[0], true);
        assert_eq!(
            out.get("future_field").and_then(|v| v.as_str()),
            Some("别把我吃了"),
            "本 UI 不管的字段必须原样保留"
        );
        assert_eq!(
            out.get("path").and_then(|v| v.as_str()),
            Some("comments/emoji.dict.yaml")
        );
    }

    /// 缺 id 的条目要兜一个而不是被丢弃——丢弃会让它在下次保存时被静默抹掉。
    #[test]
    fn missing_id_falls_back_to_file_stem() {
        let rows = rows_from_value(&arr(json!([
            { "path": "comments/en.dict.yaml" },
            { "path": "comments\\en.dict.yaml" },
        ])));
        assert_eq!(rows.len(), 2, "缺 id 不得丢条目");
        assert_eq!(rows[0].id, "en");
        assert_eq!(rows[1].id, "en2", "重名要加序号，否则开关会串");
    }

    /// 启用位缺省视为启用，与 core 的 `default_true` 同源。
    #[test]
    fn enabled_defaults_to_true() {
        let flags = enabled_flags(&arr(json!([
            { "path": "a" },
            { "path": "b", "enabled": false }
        ])));
        assert_eq!(flags, vec![true, false]);
    }

    /// ⚠️ 全角逗号必须认：界面上的示例写的就是全角，只认半角会拼出一个匹配不到任何
    /// 方案的 id，表现是「限定了方案，结果哪个方案都不显示」。
    #[test]
    fn parse_schemas_accepts_full_width_separators() {
        assert_eq!(parse_schemas("pinyin，wubi86"), vec!["pinyin", "wubi86"]);
        assert_eq!(
            parse_schemas("pinyin, wubi86 ;english"),
            vec!["pinyin", "wubi86", "english"]
        );
        assert!(parse_schemas("  ").is_empty(), "留空 = 全部方案");
    }

    /// ★ 有行时也要能构建出来。
    ///
    /// 本仓的测试覆盖的是数据契约，不是「画出来长什么样」——但**运行时 panic** 是能测的：
    /// windui 的 `.small()` / `.outline_soft()` 是 Button/CheckBox/Switch 专属，落到别的
    /// widget 上会在 debug 构建下当场 `debug_assert!(false)`，而编译期一律放行
    /// （`every_settings_section_builds_without_panicking` 防的就是这个）。
    /// 行区只在列表非空时才走到，出厂配置是空数组 ⇒ 不造几行进去就永远测不到那段。
    #[test]
    fn dialog_builds_with_rows() {
        let ui = CommentDictsUi::new();
        ui.seed(&arr(json!([
            { "id": "emoji", "label": "Emoji 名称", "path": "comments/emoji.dict.yaml" },
            { "id": "en", "path": "comments/en.dict.yaml", "enabled": false, "schemas": ["english"] },
        ])));
        let _ = build_comment_dicts_dialog(&ui);
    }

    /// 空列表同样要能构建（出厂状态就是这一支）。
    #[test]
    fn dialog_builds_when_empty() {
        let _ = build_comment_dicts_dialog(&CommentDictsUi::new());
    }

    /// 增删改与启用位的往返：这几步是「保存后配置里到底写了什么」的唯一来源。
    #[test]
    fn commit_delete_and_to_value_round_trip() {
        let ui = CommentDictsUi::new();
        ui.seed(&arr(json!([{ "id": "a", "path": "a.yaml" }])));

        ui.commit_form(
            NEW_DICT,
            "b".into(),
            "B 库".into(),
            "b.yaml".into(),
            vec!["english".into()],
        );
        let v = ui.to_value();
        let items = v.as_array().expect("应是数组");
        assert_eq!(items.len(), 2, "新建要追加在末尾（顺序即优先级）");
        assert_eq!(items[1]["id"], json!("b"));
        assert_eq!(items[1]["enabled"], json!(true), "新建即启用");
        assert_eq!(items[1]["schemas"], json!(["english"]));

        // 改 id：启用位要跟着走，不能退回默认。
        ui.switch_of("b", true).set(false);
        ui.commit_form(1, "b2".into(), "B 库".into(), "b.yaml".into(), vec![]);
        let v = ui.to_value();
        assert_eq!(v[1]["id"], json!("b2"));
        assert_eq!(
            v[1]["enabled"],
            json!(false),
            "改标识后启用位必须跟着改名走，否则用户改个名字词库就自己开了"
        );

        ui.delete(0);
        let v = ui.to_value();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["id"], json!("b2"));
    }

    #[test]
    fn validate_rejects_empty_and_duplicate_id() {
        assert!(validate("", "p", &[]).is_some());
        assert!(validate("a b", "p", &[]).is_some(), "标识不能含空格");
        assert!(validate("x", "", &[]).is_some(), "路径必填");
        assert!(validate("x", "p", &["x".into()]).is_some(), "重名要挡");
        assert!(validate("x", "p", &["y".into()]).is_none());
    }

    /// 空数组与缺键都读成空列表，不 panic。
    #[test]
    fn absent_array_reads_as_empty() {
        assert!(rows_from_value(&[]).is_empty());
        assert!(rows_from_value(&arr(json!([]))).is_empty());
    }
}
