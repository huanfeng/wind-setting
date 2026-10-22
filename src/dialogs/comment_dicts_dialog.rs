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
    /// **自动派生项**：某个方案的词库自己带了 `comment` 列，core 每次启动重新检测。
    ///
    /// 这类行只读 —— 路径与适用方案都是检测出来的**事实**而非偏好，用户能改的只有
    /// 「开不开」和「排第几」。写回配置时它落成一个只带 `id`/`auto`/`enabled` 的空壳，
    /// 由 core 按 id 认领（见 core 的 `merge_comment_dicts`）。
    pub auto: bool,
    /// **随包内置项**（`system.comments.toml` 的 `[[dicts]]`）：与自动项同为只读空壳，写回
    /// `{id, builtin = true, enabled}`，由 core 按 id 认领。
    pub builtin: bool,
    /// 自动项 / 内置项对应的词库相对路径（仅供显示；手工项为空）。
    pub rel: String,
    /// 读进来的原始对象，写回时在它上面 insert。见模块文档。
    pub raw: Value,
    /// `id` 是界面兜出来的（配置里没写），见 [`rows_from_value`]。
    ///
    /// 兜的 id 只是界面内部的索引（开关、挡重名），用户没改它就**不写回**：手写配置省掉
    /// `id` 是合法的，写回一个凭空造的值就是一次「什么都没动却有未保存更改」（论坛 t158）。
    pub id_is_fallback: bool,
}

/// core 给出的一个只读注释源：自动派生项（`comment.sources` 的 `auto` 段）或随包内置项
/// （`builtin` 段，`builtin = true`）。两者都以空壳挂进配置数组，只差空壳上的标记键。
#[derive(Clone, PartialEq, Debug, Default)]
pub struct AutoSource {
    pub id: String,
    pub label: String,
    pub rel: String,
    pub schemas: Vec<String>,
    pub builtin: bool,
}

/// `schemas\comments\` 下扫到的一个候选文件（`comment.sources` 的 `files` 段）。
#[derive(Clone, PartialEq, Debug, Default)]
pub struct CommentFile {
    /// 相对 `schemas\` 的路径，直接就是要写进配置的那个值。
    pub path: String,
    /// 词库 YAML 头里的 `name:`；空则由界面回落文件名。
    pub label: String,
    /// 是否真的声明了 `comment` 列。**为 false 也要列出来**：扫到却不显示，
    /// 用户看到的是「我的文件不见了」，比「列出来并标一句缺 comment 列」难查得多。
    pub has_comment: bool,
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
    /// 适用方案：方案 id → 勾选位。**一个都不勾 = 全部方案**（与 core 的 `applies_to` 同义）。
    ///
    /// 从「逗号分隔的文本框」换成勾选：方案 id 是内部标识（`wubi86`、`rime_frost`），
    /// 让用户凭记忆把它拼对，拼错了还没有任何反馈（core 只是查不到而已），是这一屏
    /// 最容易配错的一格。
    pub schema_sigs: Rc<RefCell<HashMap<String, Signal<bool>>>>,
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
            schema_sigs: Rc::new(RefCell::new(HashMap::new())),
            error: signal(String::new()),
            title: signal("添加注释词库".to_string()),
        }
    }

    /// 取某个方案的勾选位；不存在就现造一个。
    ///
    /// ⚠️ **不要在 `list_signal` 的行构建器里第一次调用它**：那里造出来的信号属于该行的
    /// `SignalScope`，行重建即被回收。调用点要么早于 `schemas_all.set`
    /// （见 [`CommentDictsUi::refresh_sources`]），要么落在已经造过的 id 上。
    pub fn schema_sig(&self, id: &str) -> Signal<bool> {
        if let Some(s) = self.schema_sigs.borrow().get(id) {
            return *s;
        }
        let s = signal(false);
        self.schema_sigs.borrow_mut().insert(id.to_string(), s);
        s
    }

    /// 当前勾选的方案 id（顺序取 `order`，即界面上的顺序，写回配置才稳定）。
    pub fn selected_schemas(&self, order: &[String]) -> Vec<String> {
        let sigs = self.schema_sigs.borrow();
        order
            .iter()
            .filter(|id| sigs.get(*id).is_some_and(|s| s.get()))
            .cloned()
            .collect()
    }

    /// 全部清空勾选（打开表单前调用；只清不删，信号本身要留着给已绑定的界面用）。
    fn clear_schema_sigs(&self) {
        for s in self.schema_sigs.borrow().values() {
            s.set(false);
        }
    }

    pub fn open_new(&self) {
        self.editing.set(NEW_DICT);
        self.id.set(String::new());
        self.label.set(String::new());
        self.path.set(String::new());
        self.clear_schema_sigs();
        self.error.set(String::new());
        self.title.set("添加注释词库".to_string());
        self.visible.set(true);
    }

    /// 从一个候选文件开门见山地新建：id / 显示名 / 路径全部预填，用户只需要点确认。
    ///
    /// id 取文件干并让重名的自动加序号 —— 它只用于内部定位（界面上根本不显示），
    /// 让用户为它想一个名字是这一屏原来最没道理的一步。
    pub fn open_new_from_file(&self, f: &CommentFile, existing_ids: &[String]) {
        self.open_new();
        self.id.set(fallback_id(&f.path, existing_ids));
        self.label.set(if f.label.is_empty() {
            file_stem_of(&f.path)
        } else {
            f.label.clone()
        });
        self.path.set(f.path.clone());
    }

    pub fn open_edit(&self, index: usize, row: &CommentDictRow) {
        self.editing.set(index);
        self.id.set(row.id.clone());
        self.label.set(row.label.clone());
        self.path.set(row.path.clone());
        self.clear_schema_sigs();
        for id in &row.schemas {
            self.schema_sig(id).set(true);
        }
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
    pub rows: Signal<Vec<CommentDictRow>>,
    /// id → 启用开关。
    pub switches: Rc<RefCell<HashMap<String, Signal<bool>>>>,
    pub form: DictForm,
    /// core 检测出的自动注释源（词库自带 `comment` 列）。
    pub auto_sources: Signal<Vec<AutoSource>>,
    /// `schemas\comments\` 下扫到的候选文件，供「添加词库」直接挑。
    pub files: Signal<Vec<CommentFile>>,
    /// 全部方案 `(id, 显示名)`，供表单里的适用方案勾选。
    pub schemas_all: Signal<Vec<(String, String)>>,
    /// 「从 comments 目录挑一个」弹层的可见性。
    pub picker: Signal<bool>,
    /// 播种时**没进界面**的条目（认领不到的空壳、重复空壳、非对象条目，见 `ui_mask`）：
    /// `(在配置数组里的原下标, 原值)`。写回时按原下标插回去。见 [`CommentDictsUi::to_value`]。
    pub orphans: Signal<Vec<(usize, Value)>>,
    /// 播种时**没被任何空壳认领**的自动源 id（检测顺序），即 core 会自己追加到末尾的那些。
    pub fresh_auto: Signal<Vec<String>>,
}

impl CommentDictsUi {
    pub fn new() -> Self {
        Self {
            rows: signal(Vec::new()),
            switches: Rc::new(RefCell::new(HashMap::new())),
            form: DictForm::new(),
            auto_sources: signal(Vec::new()),
            files: signal(Vec::new()),
            schemas_all: signal(Vec::new()),
            picker: signal(false),
            orphans: signal(Vec::new()),
            fresh_auto: signal(Vec::new()),
        }
    }

    /// 拉一次 core 的可选来源（自动派生项 + `comments\` 目录扫描）与方案列表。
    ///
    /// 放在**构建期**打一次而不是每次开对话框都打：这一屏的按钮由 manifest 通用逻辑
    /// 绑到 `visible` 上，本模块没有「打开时」这个钩子。代价是两次轻量 RPC
    /// （一次目录扫描 + 每个已启用词库读一次 YAML 头），比给通用分派链开一个洞划算。
    pub fn refresh_sources(&self) {
        let v = crate::rpc::call_core("comment.sources", &json!({})).unwrap_or(json!({}));
        self.auto_sources.set(auto_sources_from_value(&v));
        self.files.set(files_from_value(&v));
        let schemas: Vec<(String, String)> = crate::state::load_schemas()
            .into_iter()
            .map(|s| {
                let name = if s.name.is_empty() {
                    s.id.clone()
                } else {
                    s.name
                };
                (s.id, name)
            })
            .collect();
        // ★ 勾选位必须**在 `set` 之前**、于行作用域之外造好。
        //
        // `list_signal` 会把 `row_fn` 里现造的信号圈进那一行的 `SignalScope`，行一重建
        // 就随旧行回收 —— 而 `schema_sigs` 里还留着那个句柄，下次读写它就是
        // 「signal 句柄已失效：槽位已被回收」的 panic。惰性创建在这类列表里一律是错的。
        for (id, _) in &schemas {
            self.form.schema_sig(id);
        }
        self.schemas_all.set(schemas);
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

    /// 用配置里的数组播种（构建 / 重载配置时调用），并把 core 检测出的自动项合并进来。
    ///
    /// ⚠️ 合并规则必须与 core 的 `merge_comment_dicts` 一致（空壳按 id 认领、认领不到就
    /// 不显示、没被认领的自动项追加末尾并默认启用）。没进界面的条目只是**不显示**，写回时
    /// 原位还原（`orphans`），见 [`Self::to_value`]。两处各有一份实现是**刻意**的：core
    /// 那份决定实际加载什么，这份决定界面显示什么，而界面还要能把「用户没动过的自动项」
    /// 与「用户排过序的」区分开。改任一处都要改另一处 —— [`merge_rows`] 的测试钉住了
    /// 三条关键语义。
    pub fn seed(&self, items: &[Value]) {
        let auto = self.auto_sources.get();
        self.orphans.set(hidden_items(items, &auto));
        self.fresh_auto.set(unclaimed_auto_ids(items, &auto));
        let rows = merge_rows(items, &auto);
        self.switches.borrow_mut().clear();
        for (r, on) in rows
            .iter()
            .zip(merged_enabled_flags(items, &self.auto_sources.get()))
        {
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
                    auto: false,
                    builtin: false,
                    rel: String::new(),
                    raw: json!({}),
                    id_is_fallback: false,
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
                if slot.id != id {
                    slot.id_is_fallback = false; // 用户亲手改了 id ⇒ 从此是真值，要写回
                }
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
    ///
    /// # ★ 不改 → 写回与读入逐字相同
    ///
    /// 关窗守卫拿「全部 writer 跑一遍的结果」与 core 回包比 diff，这里多一个键、少一条、
    /// 换个位置都会让用户什么都没动就被拦（论坛 t158）。读入时界面做过的三处加工，写回时
    /// 各自还原：
    ///
    /// 1. **新冒出来的自动源**（没被任何空壳认领，`fresh_auto`）追加在末尾显示；用户没动
    ///    它们（仍按检测顺序整段排在末尾、全部启用）就**不写** —— core 对没被认领的自动源
    ///    本来就是「追加末尾、默认启用」，写不写效果相同，写了就是凭空多出几条。配置里
    ///    一条空壳都没有时这就是全部自动项，即「打开设置页点保存，配置里凭空多出几条」那
    ///    一类（守门测试 `untouched_settings_produce_no_diff`）。只要动过其中任何一处（关掉、
    ///    拖走、在后面加了手工项），整段固化成空壳，顺序才能完整表达。
    /// 2. **没进界面的条目**（`orphans`：认领不到的空壳——词库停用 / 总开关关了 /
    ///    `comment.sources` 没拉到；同一源的重复空壳；非对象条目）不显示，但按原下标插回。丢掉它不只是假脏：词库重新启用后，用户给它排的位置和
    ///    关掉的状态都没了。
    /// 3. 空壳的 `path` / `schemas` 空值、兜出来的 `id`，见 [`row_to_value`]。
    ///
    /// 已认领的空壳一律原样写回，哪怕它恰好处在「默认形态」：那是用户写进配置的东西，
    /// 保存一次就被抹掉说不过去。
    pub fn to_value(&self) -> Value {
        let sw = self.switches.borrow();
        let rows = self.rows.get();
        let fresh = self.fresh_auto.get();
        let keep = if fresh_tail_untouched(&rows, &sw, &fresh) {
            rows.len() - fresh.len()
        } else {
            rows.len()
        };
        let mut out: Vec<Value> = rows[..keep]
            .iter()
            .map(|r| {
                let on = sw.get(&r.id).map(|s| s.get()).unwrap_or(true);
                row_to_value(r, on)
            })
            .collect();
        // 升序插回：前面的先就位，后面的原下标才对得上。
        for (i, v) in self.orphans.get() {
            out.insert(i.min(out.len()), v);
        }
        Value::Array(out)
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
        let id_is_fallback = id.is_empty();
        if id_is_fallback {
            let taken: Vec<String> = out.iter().map(|r| r.id.clone()).collect();
            id = fallback_id(&path, &taken);
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
            auto: it.get("auto").and_then(|v| v.as_bool()).unwrap_or(false),
            builtin: it.get("builtin").and_then(|v| v.as_bool()).unwrap_or(false),
            rel: String::new(),
            raw: it.clone(),
            id_is_fallback,
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
fn fallback_id(path: &str, existing: &[String]) -> String {
    let stem = file_stem_of(path);
    let base = if stem.is_empty() { "dict" } else { &stem };
    let mut id = base.to_string();
    let mut n = 2;
    while existing.contains(&id) {
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
    if !r.id_is_fallback {
        obj.insert("id".into(), json!(r.id));
    }
    obj.insert("enabled".into(), json!(enabled));
    if r.auto || r.builtin {
        // 自动项（内置项同理）落成**空壳**：只有 id / auto / enabled / 位置归用户，`path` 与 `schemas`
        // 每次启动由 core 重新检测。把检测结果一起写回去的话，词库换了层或方案改了声明，
        // 配置里那份就成了过时的快照 —— 而 core 认领时用的是检测值，两者不一致时谁也
        // 看不出来配置里那两行早已没人读。
        //
        // ★ 只删**有内容**的那两个键。core 的 `config.get` 把空壳序列化成带 `path: ""` /
        // `schemas: []` 的完整形状（`CommentDictSpec` 没有 skip_serializing_if），空值与
        // 缺键对 core 同义；连空的也删，读进来的与写回去的就恒不相等 ⇒ 打开设置什么都
        // 不动也提示「有未保存的更改」（论坛 t158）。
        obj.insert(
            if r.builtin { "builtin" } else { "auto" }.into(),
            json!(true),
        );
        for k in ["path", "schemas"] {
            if obj.get(k).is_some_and(|x| !is_empty_json(x)) {
                obj.remove(k);
            }
        }
        return v;
    }
    obj.insert("label".into(), json!(r.label));
    obj.insert("path".into(), json!(r.path));
    obj.insert("schemas".into(), json!(r.schemas));
    v
}

/// 空串 / 空数组（core 对「没写」的序列化形状）。
fn is_empty_json(v: &Value) -> bool {
    match v {
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Null => true,
        _ => false,
    }
}

/// `comment.sources` 的 `builtin` 与 `auto` 段 → 只读源列表。内置项在前：core 对没被认领的
/// 源也是先追加内置、再追加自动（见 core 的 `merge_comment_dicts`），界面末尾的顺序要与之一致。
pub fn auto_sources_from_value(v: &Value) -> Vec<AutoSource> {
    let section = |key: &str, builtin: bool| -> Vec<AutoSource> {
        v.get(key)
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|it| {
                        Some(AutoSource {
                            id: it.get("id")?.as_str()?.to_string(),
                            label: str_of(it, "label"),
                            rel: str_of(it, "rel"),
                            schemas: strs_of(it, "schemas"),
                            builtin,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out = section("builtin", true);
    out.extend(section("auto", false));
    out
}

/// `comment.sources` 的 `files` 段 → 候选文件列表。
pub fn files_from_value(v: &Value) -> Vec<CommentFile> {
    v.get("files")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|it| {
                    Some(CommentFile {
                        path: it.get("path")?.as_str()?.to_string(),
                        label: str_of(it, "label"),
                        has_comment: it
                            .get("hasComment")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn str_of(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

fn strs_of(v: &Value, k: &str) -> Vec<String> {
    v.get(k)
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// 配置数组 × 自动源 → 界面上的行序列。**与 core 的 `merge_comment_dicts` 同规则。**
///
/// - `auto = true` 的条目是引用空壳：按 id 认领一个自动源，拿它的显示名/相对路径/适用
///   方案来填这一行；认领不到就不出行（词库被删/停用/总开关关了，都是常态而非配置错误；
///   写回时由 [`hidden_items`] 原位还原）。
/// - 没被认领的自动源追加**末尾**：装个新方案不该把用户排好的第一优先级挤下去。
pub fn merge_rows(items: &[Value], auto: &[AutoSource]) -> Vec<CommentDictRow> {
    let mut out: Vec<CommentDictRow> = Vec::new();
    for r in rows_from_value(&shown_items(items, auto)) {
        // 进得了界面的空壳都认领到了（`ui_mask` 保证），这里的 find 对空壳恒命中。
        match auto
            .iter()
            .find(|a| (r.auto || r.builtin) && shell_kind_matches(a, r.builtin) && a.id == r.id)
        {
            Some(a) => out.push(auto_row(a, r.raw)),
            None => out.push(r),
        }
    }
    let fresh = unclaimed_auto_ids(items, auto);
    for a in auto.iter().filter(|a| fresh.contains(&a.id)) {
        out.push(auto_row(a, json!({})));
    }
    out
}

/// 与 [`merge_rows`] 配套的启用位序列（下标一一对齐）。新自动项默认开。
pub fn merged_enabled_flags(items: &[Value], auto: &[AutoSource]) -> Vec<bool> {
    let mut out = enabled_flags(&shown_items(items, auto));
    out.extend(unclaimed_auto_ids(items, auto).iter().map(|_| true));
    out
}

/// 配置数组里空壳条目的 `(id, 是否内置)`（空壳认领的依据）。`auto = true` 与
/// `builtin = true` 各认领自己那一类源。
fn shell_id(it: &Value) -> Option<(&str, bool)> {
    let flag = |k: &str| it.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let builtin = flag("builtin");
    if !builtin && !flag("auto") {
        return None;
    }
    Some((
        it.get("id").and_then(|v| v.as_str()).unwrap_or_default(),
        builtin,
    ))
}

fn shell_kind_matches(a: &AutoSource, builtin: bool) -> bool {
    a.builtin == builtin
}

/// 每个配置条目**进不进界面**——[`merge_rows`]、[`merged_enabled_flags`]、[`hidden_items`]
/// 三者共用这**一份**判据，写回时才能把没进界面的条目逐个原位还原（判据分两份的话，
/// 一边丢了另一边没记，插回下标就错位）。
///
/// 不进界面的：
/// - 非对象条目（手写畸形配置）：界面没法表达它，但也不该替用户删掉；
/// - 认领不到任何自动源的空壳（词库停用 / 总开关关了 / 来源没拉到）；
/// - 同一个自动源的**第二个及以后**的空壳：一个源只出一行，否则两行共用一个按 id 索引的
///   开关、拖序也无从表达。core 那边重复挂载按解析后路径去重，后来的那条本就不生效。
fn ui_mask(items: &[Value], auto: &[AutoSource]) -> Vec<bool> {
    let mut claimed: Vec<(&str, bool)> = Vec::new();
    items
        .iter()
        .map(|it| {
            if !it.is_object() {
                return false;
            }
            let Some(id) = shell_id(it) else {
                return true;
            };
            if claimed.contains(&id)
                || !auto
                    .iter()
                    .any(|a| a.id == id.0 && shell_kind_matches(a, id.1))
            {
                return false;
            }
            claimed.push(id);
            true
        })
        .collect()
}

/// 进界面的条目（原序）。
fn shown_items(items: &[Value], auto: &[AutoSource]) -> Vec<Value> {
    items
        .iter()
        .zip(ui_mask(items, auto))
        .filter(|(_, show)| *show)
        .map(|(it, _)| it.clone())
        .collect()
}

/// 没进界面的条目：`(原下标, 原值)`，下标升序。写回时要原位还原，见
/// [`CommentDictsUi::to_value`]。
pub fn hidden_items(items: &[Value], auto: &[AutoSource]) -> Vec<(usize, Value)> {
    items
        .iter()
        .zip(ui_mask(items, auto))
        .enumerate()
        .filter(|(_, (_, show))| !show)
        .map(|(i, (it, _))| (i, it.clone()))
        .collect()
}

/// 没被任何空壳认领的自动源 id（检测顺序）—— [`merge_rows`] 追加在末尾的正是这些。
pub fn unclaimed_auto_ids(items: &[Value], auto: &[AutoSource]) -> Vec<String> {
    auto.iter()
        .filter(|a| {
            !items
                .iter()
                .any(|it| shell_id(it) == Some((a.id.as_str(), a.builtin)))
        })
        .map(|a| a.id.clone())
        .collect()
}

/// 新冒出来的自动源是否还是 [`merge_rows`] 刚合并出来的那副样子：按检测顺序整段排在
/// 最末、全部启用。见 [`CommentDictsUi::to_value`]。
fn fresh_tail_untouched(
    rows: &[CommentDictRow],
    switches: &HashMap<String, Signal<bool>>,
    fresh: &[String],
) -> bool {
    let Some(start) = rows.len().checked_sub(fresh.len()) else {
        return false;
    };
    rows[start..].iter().zip(fresh).all(|(r, id)| {
        (r.auto || r.builtin) && r.id == *id && switches.get(id).is_none_or(|s| s.get())
    })
}

fn auto_row(a: &AutoSource, raw: Value) -> CommentDictRow {
    CommentDictRow {
        id: a.id.clone(),
        label: a.label.clone(),
        path: String::new(),
        schemas: a.schemas.clone(),
        auto: !a.builtin,
        builtin: a.builtin,
        rel: a.rel.clone(),
        raw,
        id_is_fallback: false,
    }
}

/// 路径 → 不含扩展名的文件干（供显示名与 id 兜底）。
fn file_stem_of(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("")
        .to_string()
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
        return Some("标识重复");
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
    // 自动项显示的是**词库**的路径（注释就长在它身上），故取 `rel` 而非 `path`
    // ——后者对自动项恒为空，它没有自己的文件。
    let src = if r.auto || r.builtin { &r.rel } else { &r.path };
    // 打头给出模板里按库取它的写法：`${dict:id}` 要填的是 **id**，而 id 在别处都看不见
    // （行标题是显示名，编辑框得点进去），用户照着显示名或文件名写就对不上、注释为空。
    let var = format!("${{dict:{}}}", r.id);
    if src.is_empty() {
        format!("{var} · {scope}")
    } else {
        format!("{var} · {src} · {scope}")
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
                .weight(1.0),
        )
}

/// 「注释词库」页（嵌在候选注释对话框的 Tab 里）：说明 + 可拖序的列表 + 「添加词库…」。
/// 二级弹层（挑文件、新建/编辑表单）另由 [`build_comment_dicts_subdialogs`] 挂到根上。
pub fn build_comment_dicts_pane(ui: &CommentDictsUi) -> Element {
    let row_ui = ui.clone();

    // 词库多了会把对话框撑出窗口（窗口拉满也看不到底栏）：列表封顶可滚，与提示气泡分段
    // 对话框同一封顶——按主窗最小高 440 算约四行，再高则窗口最矮时底栏按钮被裁掉。
    let list = Element::scroll()
        .width_match()
        .max_height(216)
        .padding_xy(0, 2)
        .child(
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
                // ★ 自动项不给「编辑 / 删除」：它的路径与适用方案是 core **检测出来的事实**，
                // 改了下次启动就被检测结果覆盖，删了下次启动又长回来。给一个按不动的按钮，
                // 比不给更让人困惑。用户能改的只有开关与顺序，那两样都还在。
                let mut ctrl = Element::row().cross(Align::Center).spacing(6);
                if r.auto || r.builtin {
                    ctrl = ctrl.child(
                        Element::label(if r.builtin { "内置" } else { "自动" })
                            .font_size(11.0)
                            .fg_role(Role::TextSubtle),
                    );
                } else {
                    ctrl =
                        ctrl.child(Element::button("编辑").small().outline_soft().on_click(
                            move |_| {
                                if let Some(row) = e_ui.rows.get().get(idx) {
                                    e_ui.form.open_edit(idx, row);
                                }
                            },
                        ))
                        .child(
                            Element::button("删除")
                                .small()
                                .outline_soft()
                                .on_click(move |_| d_ui.delete(idx)),
                        );
                }
                let ctrl = ctrl.child(Element::switch(sig));

                crate::widgets::list_row(
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
    Element::col()
        .width_match()
        .child(
            Element::label(
                "供候选注释模板的 ${dict} 变量查询。词库文件放在数据目录的 \
                 schemas\\comments\\ 下，用「添加词库…」选取。\n\
                 标「内置」的随安装包提供、标「自动」的来自词库自带的 comment 列，仅可启停与排序。\n\
                 同一词条在多个库中都有注释时取靠前者，可拖动左侧手柄调整顺序。",
            )
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(5)
            .padding_xy(2, 8),
        )
        .child(list)
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(0, 10)
                .child(
                    Element::button("添加词库…")
                        .small()
                        .outline_soft()
                        .on_click(move |_| new_ui.picker.set(true)),
                ),
        )
}

/// 「注释词库」页的二级弹层：挑文件、新建/编辑表单。
pub fn build_comment_dicts_subdialogs(ui: &CommentDictsUi) -> Element {
    Element::stack()
        .child(build_file_picker_dialog(ui))
        .child(build_dict_form_dialog(ui))
}

/// 二级弹层：从 `schemas\comments\` 扫到的文件里挑一个。
///
/// # 为什么「选」比「填」值得多写这一屏
///
/// 原来这里只有一个路径输入框，用户得自己保证：相对的是 `schemas\` 而不是数据目录、
/// 分隔符写对、文件名一个字母不差。三样里错任何一样，core 都只往日志里写一行
/// 「注释词库不存在」，界面上什么都不会发生 —— 「配了没反应」这类问题在本仓反复出现，
/// 根子多半就是这种「让用户手打一个内部标识」的输入框。
fn build_file_picker_dialog(ui: &CommentDictsUi) -> Element {
    let visible = ui.picker;
    let pick_ui = ui.clone();

    // 滚动放在自己这一层、内层用 `host_signal`：`list_signal` 拿自己的 widget 顶掉了
    // 滚动容器的，滚轮与滚动条拖动都没人接（同 `build_schema_picker` 的说明）。
    let list = Element::host_signal(ui.files, move |f: CommentFile| {
        let mounted = pick_ui
            .rows
            .get()
            .iter()
            .any(|r| !r.auto && paths_eq(&r.path, &f.path));
        let name = if f.label.is_empty() {
            file_stem_of(&f.path)
        } else {
            f.label.clone()
        };
        let desc = match (mounted, f.has_comment) {
            (true, _) => format!("{} · 已添加", f.path),
            // 缺 comment 列的文件照样列出来（core 加载时会整库跳过）：滤掉的话
            // 用户看到的是「我放进去的文件不见了」，比一句说明难查得多。
            (false, false) => format!("{} · 缺 comment 列", f.path),
            (false, true) => f.path.clone(),
        };
        let sel_ui = pick_ui.clone();
        let file = f.clone();
        let btn = if mounted {
            Element::label("已添加")
                .font_size(11.0)
                .fg_role(Role::TextSubtle)
        } else {
            Element::button("添加").small().on_click(move |_| {
                let taken: Vec<String> = sel_ui.rows.get().iter().map(|r| r.id.clone()).collect();
                sel_ui.form.open_new_from_file(&file, &taken);
                sel_ui.picker.set(false);
            })
        };
        crate::widgets::list_row(crate::widgets::setting_row(&name, Some(&desc), btn).width_match())
    });

    let manual_ui = ui.clone();
    let frame = Element::col()
        .width(520)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("选择注释词库")
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(
                "数据目录 schemas\\comments\\ 下找到的词库文件（.dict.yaml 或 OpenCC 的 .txt）。\n\
                 列表为空表示该目录中还没有词库；放入后重新打开本页。",
            )
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(3)
            .padding_xy(18, 8),
        )
        .child(Element::scroll().width_match().height(240).child(list))
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                // 手填入口留着：词库放在 `comments\` 之外（第三方方案自带的、或用户自己
                // 分了子目录）时扫描列表里没有它，但那条路径本身完全合法。
                .child(
                    Element::button("手动填写路径…")
                        .small()
                        .outline_soft()
                        .on_click(move |_| {
                            manual_ui.form.open_new();
                            manual_ui.picker.set(false);
                        }),
                )
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

/// 两个相对路径是否指同一个文件：分隔符与大小写都不该让「已添加」判错。
///
/// Windows 上 `comments/en.dict.yaml` 与 `comments\En.Dict.yaml` 是同一个文件，判成
/// 两个的后果是同一个库被添加两次 —— core 按解析后路径去重，于是第二条静默失效，
/// 用户看到的是「我添加了它，可它不生效」。
fn paths_eq(a: &str, b: &str) -> bool {
    let norm = |x: &str| x.replace('\\', "/").to_lowercase();
    norm(a) == norm(b)
}

/// 表单里的「适用方案」：每个方案一个勾选框。**一个都不勾 = 全部方案。**
///
/// 从逗号分隔的文本框换过来的。方案 id 是内部标识（`wubi86`、`rime_frost` 之类），
/// 让用户凭记忆拼对、拼错了还没有任何反馈（core 只是查不到而已），是这一屏原来最容易
/// 配错的一格；全角逗号那条陷阱也随之一起消失 —— 没有输入框就没有分隔符。
///
/// # ★ `host_signal` + 自己的 `scroll`，不是 `list_signal`
///
/// `list_signal` 内部是 `scroll().fill()`，但随即用自己的列表 widget 顶掉了滚动容器的
/// `ScrollWidget`，而列表 widget 不接指针事件 ⇒ **看得见滚动条，滚轮与拖动却没人接**。
/// （另外 `fill` 还要父给确定视口。）这里把滚动放在**自己这一层**（显式 `width_match` +
/// `height`，同 `field_dialogs::shell_with_buttons` 的写法），内层用非滚动的 `host_signal`。
///
/// ⚠️ 本仓在这个坑上已经栽过四次（`build_schema_inline_list`、`schema_key_actions::build_body`、
/// 这里、`build_file_picker_dialog`），每次的症状都一模一样：**滚不动**。选响应式列表 API 时先问一句「谁负责滚」。
fn build_schema_picker(ui: &CommentDictsUi) -> Element {
    let form = ui.form.clone();
    let boxes = Element::host_signal(ui.schemas_all, move |(id, name): (String, String)| {
        Element::checkbox(&name, form.schema_sig(&id))
            .small()
            .fg_role(Role::Text)
    });
    Element::row()
        .width_match()
        .spacing(8)
        .child(Element::label("适用方案").font_size(12.5).width(72))
        .child(
            Element::col()
                .weight(1.0)
                .spacing(4)
                .child(
                    Element::scroll()
                        .width_match()
                        .height(108)
                        .border_role(Role::Border, 1)
                        .corner(6.0)
                        .padding_xy(8, 6)
                        .child(boxes),
                )
                // 说明贴在控件正下方而不是只写在表单顶部那段：**空列表本身没有任何形状**
                // ——一个都不勾时界面上看不出「这是限定了零个方案」还是「这是不限定」，
                // 而两者的行为正好相反。这句话是那个状态唯一的读法。
                .child(
                    Element::label("无勾选 - 对所有方案启用")
                        .font_size(11.5)
                        .fg_role(Role::TextMuted),
                ),
        )
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
                "标识仅用于内部定位，不在界面显示，且不可重复。\n\
                 路径相对 schemas\\，如 comments\\emoji.dict.yaml。",
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
                .child(build_schema_picker(ui)),
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
                    // 顺序取界面上的方案顺序（`schemas_all`），而不是 HashMap 的遍历序
                    // ——后者每次运行都不同，会让「什么都没改，保存后配置 diff 却变了」。
                    let order: Vec<String> = ui2
                        .schemas_all
                        .get()
                        .into_iter()
                        .map(|(id, _)| id)
                        .collect();
                    ui2.commit_form(
                        idx,
                        id,
                        fc.label.get().trim().to_string(),
                        path,
                        fc.selected_schemas(&order),
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

    /// 行说明打头给出 `${dict:id}`：模板按库取要填 id，而 id 在列表里别处看不见——
    /// 靶机上照着说明填了 `${dict:en}`，实际 id 是自动生成的 `chinese_english`，注释恒空。
    #[test]
    fn row_desc_shows_dict_var_with_id() {
        let rows = rows_from_value(&arr(json!([{
            "id": "chinese_english", "label": "中英",
            "path": "comments/chinese_english.txt", "schemas": [],
        }])));
        assert_eq!(
            row_desc(&rows[0]),
            "${dict:chinese_english} · comments/chinese_english.txt · 全部方案"
        );
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

    fn autos(ids: &[&str]) -> Vec<AutoSource> {
        ids.iter()
            .map(|id| AutoSource {
                id: format!("auto:{id}"),
                label: format!("{id} 词库"),
                rel: id.to_string(),
                schemas: vec!["pinyin".into()],
                builtin: false,
            })
            .collect()
    }

    fn builtins(ids: &[&str]) -> Vec<AutoSource> {
        ids.iter()
            .map(|id| AutoSource {
                id: id.to_string(),
                label: format!("{id} 释义"),
                rel: format!("comments/{id}.txt"),
                schemas: Vec::new(),
                builtin: true,
            })
            .collect()
    }

    /// 内置项与自动项同为只读空壳：没动过不写回；关掉一个就整段固化，空壳带 `builtin`。
    #[test]
    fn builtin_sources_round_trip_as_builtin_shells() {
        let ui = CommentDictsUi::new();
        let mut src = builtins(&["en_zh", "zh_en"]);
        src.extend(autos(&["frost"]));
        ui.auto_sources.set(src);
        ui.seed(&[]);
        let rows = ui.rows.get();
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["en_zh", "zh_en", "auto:frost"], "内置在前，同 core");
        assert!(rows[0].builtin && !rows[0].auto);
        assert_eq!(ui.to_value(), json!([]), "没动过不写回");

        ui.switch_of("zh_en", true).set(false);
        assert_eq!(
            ui.to_value(),
            json!([
                { "id": "en_zh", "builtin": true, "enabled": true },
                { "id": "zh_en", "builtin": true, "enabled": false },
                { "id": "auto:frost", "auto": true, "enabled": true },
            ])
        );
    }

    /// 配置里的内置空壳按 id 认领内置源；`auto` 空壳认领不了同 id 的内置源。
    #[test]
    fn builtin_shell_claims_only_builtin_source() {
        let ui = CommentDictsUi::new();
        ui.auto_sources.set(builtins(&["en_zh"]));
        let items = arr(json!([
            { "id": "en_zh", "auto": true, "enabled": false },
            { "id": "en_zh", "builtin": true, "enabled": false },
        ]));
        ui.seed(&items);
        let rows = ui.rows.get();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].builtin);
        assert!(!ui.switch_of("en_zh", true).get());
        assert_eq!(ui.to_value(), Value::Array(items), "认领不到的空壳原位保留");
    }

    /// 自动派生项要出现在列表里，且排在手工项**之后**。
    #[test]
    fn merge_rows_appends_new_auto_sources_at_the_end() {
        let rows = merge_rows(
            &arr(json!([{ "id": "en", "path": "comments/en.dict.yaml" }])),
            &autos(&["pinyin/frost.dict.yaml"]),
        );
        assert_eq!(rows.len(), 2);
        assert!(!rows[0].auto && rows[0].id == "en");
        assert!(rows[1].auto, "自动项排在手工项之后");
        assert_eq!(
            rows[1].rel, "pinyin/frost.dict.yaml",
            "副标题显示的是词库路径"
        );
        assert_eq!(rows[1].schemas, ["pinyin"], "白名单来自检测结果");
    }

    /// 引用空壳把自动项的位置交回用户；认领不到的空壳丢弃。
    #[test]
    fn merge_rows_honors_shell_position_and_drops_unclaimed() {
        let items = arr(json!([
            { "id": "auto:pinyin/frost.dict.yaml", "auto": true, "enabled": false },
            { "id": "auto:没了.dict.yaml", "auto": true },
            { "id": "en", "path": "comments/en.dict.yaml" },
        ]));
        let auto = autos(&["pinyin/frost.dict.yaml"]);
        let rows = merge_rows(&items, &auto);
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["auto:pinyin/frost.dict.yaml", "en"],
            "空壳在前 ⇒ 自动项优先级高于手工项；认领不到的那条丢弃"
        );
        assert_eq!(
            merged_enabled_flags(&items, &auto),
            vec![false, true],
            "启用位要与行一一对齐（丢掉的那条不能把后面的错位）"
        );
    }

    /// ★ 什么都没动就不该往配置里写空壳 —— 守门测试
    /// `untouched_settings_produce_no_diff` 拦的正是这类静默改写。
    #[test]
    fn untouched_auto_rows_are_not_written_back() {
        let ui = CommentDictsUi::new();
        ui.auto_sources.set(autos(&["pinyin/frost.dict.yaml"]));
        let items = arr(json!([{ "id": "en", "path": "comments/en.dict.yaml" }]));
        ui.seed(&items);
        assert_eq!(ui.rows.get().len(), 2, "界面上要看得见自动项");
        assert_eq!(
            ui.to_value().as_array().map(Vec::len),
            Some(1),
            "但配置里只留手工那条"
        );
    }

    /// 动过（这里是关掉）之后就要固化成空壳，否则关掉的状态存不下来。
    #[test]
    fn toggling_an_auto_row_writes_a_shell() {
        let ui = CommentDictsUi::new();
        ui.auto_sources.set(autos(&["pinyin/frost.dict.yaml"]));
        ui.seed(&arr(json!([])));
        ui.switch_of("auto:pinyin/frost.dict.yaml", true).set(false);

        let out = ui.to_value();
        let a = out.as_array().expect("数组");
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].get("auto").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(a[0].get("enabled").and_then(|v| v.as_bool()), Some(false));
        assert!(
            a[0].get("path").is_none() && a[0].get("schemas").is_none(),
            "空壳不落 path / schemas——它们每次启动由 core 重新检测，写进去就成了过时快照"
        );
    }

    /// 配置里本来就有空壳时原样写回，哪怕它恰好处在「默认形态」。
    #[test]
    fn hand_written_shell_survives_a_round_trip() {
        let ui = CommentDictsUi::new();
        ui.auto_sources.set(autos(&["pinyin/frost.dict.yaml"]));
        ui.seed(&arr(json!([
            { "id": "auto:pinyin/frost.dict.yaml", "auto": true, "enabled": true }
        ])));
        assert_eq!(
            ui.to_value().as_array().map(Vec::len),
            Some(1),
            "用户自己写进配置的空壳不能被保存一次就抹掉"
        );
    }

    /// core 的 `config.get` 回来的一条完整形状：`CommentDictSpec` 全部字段都在
    /// （`path`/`schemas`/`label` 空也照样序列化，见 core `config.rs`）。
    fn core_spec(id: &str, label: &str, path: &str, enabled: bool, auto: bool) -> Value {
        json!({
            "id": id, "label": label, "path": path,
            "enabled": enabled, "auto": auto, "schemas": [],
        })
    }

    /// ★★ 论坛 t158：配置里有引用空壳时，打开设置什么都不动也被判「有未保存的更改」。
    ///
    /// core 把空壳序列化成带 `path: ""` / `schemas: []` 的完整形状，写回时却把这两个键
    /// 删了 ⇒ 数组整体不等 ⇒ 恒脏。空值与缺键对 core 同义，没理由在这里制造差异。
    #[test]
    fn core_shaped_shells_round_trip_without_diff() {
        let ui = CommentDictsUi::new();
        ui.auto_sources.set(autos(&["a.dict.yaml", "b.dict.yaml"]));
        let items = arr(json!([
            core_spec("auto:a.dict.yaml", "", "", false, true),
            core_spec("en", "简明英汉", "comments/en.dict.yaml", true, false),
            core_spec("auto:b.dict.yaml", "", "", true, true),
        ]));
        ui.seed(&items);
        assert_eq!(
            ui.to_value(),
            Value::Array(items),
            "什么都没动，写回必须逐字相同"
        );
    }

    /// ★ 认领不到的空壳（词库停用 / 总开关关了 / `comment.sources` 没拉到）不显示，
    /// 但**写回时原位保留**：丢掉它既是一次假脏，又是一次静默丢数据——词库重新启用后
    /// 用户排好的位置与关掉的状态都没了。
    #[test]
    fn unclaimed_shells_are_kept_in_place() {
        let items = arr(json!([
            core_spec("en", "简明英汉", "comments/en.dict.yaml", true, false),
            core_spec("auto:gone.dict.yaml", "", "", false, true),
            core_spec("auto:a.dict.yaml", "", "", true, true),
        ]));
        for sources in [autos(&["a.dict.yaml"]), Vec::new()] {
            let ui = CommentDictsUi::new();
            ui.auto_sources.set(sources.clone());
            ui.seed(&items);
            assert!(
                !ui.rows.get().iter().any(|r| r.id == "auto:gone.dict.yaml"),
                "认领不到的不显示"
            );
            assert_eq!(
                ui.to_value(),
                Value::Array(items.clone()),
                "来源 {} 条时写回要与读入逐字相同",
                sources.len()
            );
        }
    }

    /// 手写畸形配置（同一自动源的重复空壳、非对象条目）：界面只出一行，但写回逐字不变——
    /// 进不进界面与写回时插回哪里必须是同一份判据，否则插回下标错位、重复的那条丢失。
    #[test]
    fn malformed_items_survive_round_trip_in_place() {
        let ui = CommentDictsUi::new();
        ui.auto_sources.set(autos(&["a.dict.yaml"]));
        let items = arr(json!([
            core_spec("auto:a.dict.yaml", "", "", false, true),
            42,
            core_spec("en", "简明英汉", "comments/en.dict.yaml", true, false),
            core_spec("auto:a.dict.yaml", "", "", true, true),
            "不是对象",
            core_spec("auto:gone.dict.yaml", "", "", true, true),
        ]));
        ui.seed(&items);
        assert_eq!(
            ui.rows
                .get()
                .iter()
                .map(|r| r.id.clone())
                .collect::<Vec<_>>(),
            ["auto:a.dict.yaml", "en"],
            "重复空壳只出一行，非对象条目不出行"
        );
        assert!(
            !ui.switch_of("auto:a.dict.yaml", true).get(),
            "开关取进界面的第一个空壳"
        );
        assert_eq!(ui.to_value(), Value::Array(items.clone()));

        // 真动了（拖序）照样写得出来，畸形条目仍在原位。
        ui.rows.update(|v| v.swap(0, 1));
        let out = ui.to_value();
        assert_eq!(out[0]["id"], json!("en"), "拖序生效：{out}");
        assert_eq!(out[1], json!(42));
        assert_eq!(
            out[3]["id"],
            json!("auto:a.dict.yaml"),
            "重复空壳不丢：{out}"
        );
        assert_eq!(out.as_array().map(Vec::len), Some(items.len()));
    }

    /// ★ 配置里已有空壳、又检测出一个新自动源：它追加在末尾显示，但用户没动它就不写。
    /// core 对没被认领的自动源本就是「追加末尾、默认启用」，写不写效果相同，写了就是假脏。
    #[test]
    fn new_auto_source_alongside_shells_is_not_written_until_touched() {
        let ui = CommentDictsUi::new();
        ui.auto_sources.set(autos(&["a.dict.yaml", "b.dict.yaml"]));
        let items = arr(json!([
            core_spec("auto:a.dict.yaml", "", "", false, true),
            core_spec("en", "简明英汉", "comments/en.dict.yaml", true, false),
        ]));
        ui.seed(&items);
        assert_eq!(ui.rows.get().len(), 3, "新自动源要显示出来");
        assert_eq!(ui.to_value(), Value::Array(items.clone()));

        // 真动了（关掉新来的那个）就得落成空壳。
        ui.switch_of("auto:b.dict.yaml", true).set(false);
        let out = ui.to_value();
        let a = out.as_array().expect("数组");
        assert_eq!(a.len(), 3, "关掉的新自动源要固化成空壳：{out}");
        assert_eq!(a[2]["id"], json!("auto:b.dict.yaml"));
        assert_eq!(a[2]["enabled"], json!(false));
    }

    /// 手写配置里省略了 `id`（core 读成空串）：界面兜的 id 只是内部索引，没被改就不写回。
    #[test]
    fn manual_entry_without_id_round_trips() {
        let ui = CommentDictsUi::new();
        let items = arr(json!([core_spec(
            "",
            "",
            "comments/en.dict.yaml",
            true,
            false
        )]));
        ui.seed(&items);
        assert_eq!(ui.to_value(), Value::Array(items));

        // 兜出来的 id 是文件干 `en`；用户在表单里把它改了 ⇒ 那就是真改动。
        assert_eq!(ui.rows.get()[0].id, "en");
        ui.commit_form(
            0,
            "my_en".into(),
            String::new(),
            "comments/en.dict.yaml".into(),
            vec![],
        );
        assert_eq!(ui.to_value()[0]["id"], json!("my_en"));
    }

    /// 真正的改动仍要检测得到：拖序、开关、删除都得让写回与读入不同。
    #[test]
    fn real_edits_on_core_shaped_data_still_differ() {
        let items = arr(json!([
            core_spec("auto:a.dict.yaml", "", "", true, true),
            core_spec("en", "简明英汉", "comments/en.dict.yaml", true, false),
        ]));
        let fresh = || {
            let ui = CommentDictsUi::new();
            ui.auto_sources.set(autos(&["a.dict.yaml"]));
            ui.seed(&items);
            ui
        };
        let ui = fresh();
        ui.switch_of("auto:a.dict.yaml", true).set(false);
        assert_ne!(ui.to_value(), Value::Array(items.clone()), "关掉自动项");

        let ui = fresh();
        ui.rows.update(|v| v.swap(0, 1));
        let out = ui.to_value();
        assert_eq!(out[0]["id"], json!("en"), "拖序要写出新顺序：{out}");

        let ui = fresh();
        ui.delete(1);
        assert_eq!(
            ui.to_value().as_array().map(Vec::len),
            Some(1),
            "删除手工项"
        );
    }

    /// 「已添加」的判定不该被分隔符或大小写骗过去 —— 判错的后果是同一个库被添加两次，
    /// 而 core 按解析后路径去重，第二条静默失效。
    #[test]
    fn paths_eq_ignores_separator_and_case() {
        assert!(paths_eq("comments/en.dict.yaml", "comments\\En.Dict.yaml"));
        assert!(!paths_eq("comments/en.dict.yaml", "comments/en2.dict.yaml"));
    }

    /// 从候选文件新建：id / 显示名 / 路径全部预填，用户不必手打任何一个。
    #[test]
    fn open_new_from_file_prefills_everything() {
        let form = DictForm::new();
        let f = CommentFile {
            path: "comments/en.dict.yaml".into(),
            label: "简明英汉".into(),
            has_comment: true,
        };
        form.open_new_from_file(&f, &["en".to_string()]);
        assert_eq!(form.id.get(), "en2", "id 撞了要自动加序号");
        assert_eq!(form.label.get(), "简明英汉");
        assert_eq!(form.path.get(), "comments/en.dict.yaml");
    }

    /// 适用方案改成勾选后，写回顺序取界面顺序而非 HashMap 遍历序 ——
    /// 后者每次运行都不同，会让「什么都没改，保存后配置 diff 却变了」。
    #[test]
    fn selected_schemas_follows_display_order() {
        let form = DictForm::new();
        let order = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        form.schema_sig("c").set(true);
        form.schema_sig("a").set(true);
        assert_eq!(form.selected_schemas(&order), vec!["a", "c"]);
        assert!(
            DictForm::new().selected_schemas(&order).is_empty(),
            "一个都不勾 = 全部方案"
        );
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
        let _ = (
            build_comment_dicts_pane(&ui),
            build_comment_dicts_subdialogs(&ui),
        );
    }

    /// 自动项行与「选择词库」弹层同样要能构建。
    ///
    /// 两段都只在数据非空时才走到（出厂配置是空数组、mock 之外没有 files），
    /// 不造几条进去就永远测不到 —— 而 windui 的 `.small()` / `.outline_soft()` 落到
    /// 非 Button/CheckBox/Switch 上是 debug 期 `debug_assert!(false)`，编译期一律放行。
    #[test]
    fn dialog_builds_with_auto_rows_and_files() {
        let ui = CommentDictsUi::new();
        ui.auto_sources.set(autos(&["pinyin/frost.dict.yaml"]));
        ui.files.set(vec![
            CommentFile {
                path: "comments/en.dict.yaml".into(),
                label: "简明英汉".into(),
                has_comment: true,
            },
            CommentFile {
                path: "comments/broken.dict.yaml".into(),
                label: String::new(),
                has_comment: false,
            },
        ]);
        ui.schemas_all.set(vec![
            ("pinyin".into(), "全拼".into()),
            ("wubi86".into(), "五笔".into()),
        ]);
        ui.seed(&arr(
            json!([{ "id": "en", "path": "comments/en.dict.yaml" }]),
        ));
        let _ = (
            build_comment_dicts_pane(&ui),
            build_comment_dicts_subdialogs(&ui),
        );
    }

    /// 「选择词库」弹层里文件一多，列表要能用滚轮滚动。
    ///
    /// 按真实挂法建树：根 stack 里垫一层页面，主对话框与叠在其上的选择器同开，
    /// 滚轮走 `dispatch_pointer` 的命中链路（含遮罩与叠层），不直接调滚动容器。
    #[test]
    fn file_picker_list_scrolls_with_wheel() {
        use windui::core::{Layout, Tree};
        use windui::event::{MouseButton, PointerEvent, PointerKind};
        use windui::geometry::{Point, Size};

        let ui = CommentDictsUi::new();
        ui.files.set(
            (0..30)
                .map(|i| CommentFile {
                    path: format!("comments/d{i}.dict.yaml"),
                    label: format!("词库 {i}"),
                    has_comment: true,
                })
                .collect(),
        );
        ui.visible.set(true);
        ui.picker.set(true);

        let mut tree = Tree::new();
        let root = Element::stack()
            .fill()
            .child(Element::leaf().fill())
            .child(build_comment_dicts_dialog(&ui))
            .build(&mut tree);
        tree.root = Some(root);
        let size = Size::new(800, 600);
        let mut te = windui::text::NullTextEngine;
        tree.layout_root(size, &mut te);

        // 选择器的列表：内容最高的那个滚动容器（30 行远超 240 的视口）。
        let mut stack = vec![root];
        let mut list = None;
        while let Some(id) = stack.pop() {
            let n = tree.get(id).unwrap();
            stack.extend(n.children.iter().copied());
            if matches!(n.layout, Layout::Scroll)
                && list.is_none_or(|l| tree.get(l).unwrap().content_h < n.content_h)
            {
                list = Some(id);
            }
        }
        let list = list.expect("应有滚动容器");
        let r = tree.abs_bounds(list);
        let n = tree.get(list).unwrap();
        assert!(
            n.content_h > r.h && r.h > 0,
            "前提：列表内容溢出视口（content_h={} 视口={}）",
            n.content_h,
            r.h
        );

        let at = Point::new(r.x + r.w / 2, r.y + r.h / 2);
        let (mut hover, mut capture) = (None, None);
        tree.dispatch_pointer(
            PointerEvent::single(PointerKind::Wheel(-120), at, MouseButton::Left),
            &mut hover,
            &mut capture,
        );
        tree.layout_root(size, &mut te);
        assert!(
            tree.get(list).unwrap().scroll_y > 0,
            "滚轮向下应滚动选择器列表"
        );
    }

    /// 空列表同样要能构建（出厂状态就是这一支）。
    #[test]
    fn dialog_builds_when_empty() {
        let ui = CommentDictsUi::new();
        let _ = (
            build_comment_dicts_pane(&ui),
            build_comment_dicts_subdialogs(&ui),
        );
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
