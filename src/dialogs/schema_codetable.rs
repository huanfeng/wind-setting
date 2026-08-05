//! 方案级码表配置 —— 给单个码表方案单独设一套上屏行为 / 调频，不跟随全局。
//!
//! # 为什么与全局页共用 manifest 条目
//!
//! 界面上的每一项（label、hint、控件类型、数值范围、`enabled_when` 联动）都取自
//! `settings_manifest.toml` 里 `section = "上屏行为"` 的那组条目——与全局码表配置对话框
//! **同一份定义**。不共用的话「精确匹配空码补全」这类解释就会有两份，改一处忘另一处。
//!
//! 共用的只有定义，读写落点完全不同：
//!
//! | | 全局页 | 本模块 |
//! | --- | --- | --- |
//! | 落点 | `schema.codetable.*` → `config.toml` | `engine.codetable.*` → `schema_overrides/<id>.toml` |
//! | 信号归属 | `FieldRegistry`（随主保存提交） | 本模块自持（随方案对话框的「保存」提交） |
//! | 值语义 | `bool`，恒有值 | `Option<bool>`，可以「没写」 |
//!
//! 所以控件不能直接复用 `manifest::field()`——那条路径把信号注册进全局 `FieldRegistry`，
//! 写回落点写死在 key 上。这里复刻的是**行布局**（`widgets::setting_row`），保证两个
//! 对话框长得一模一样。
//!
//! # 覆盖模型：整段式总开关
//!
//! core 侧 `CodeTableSpec` 的字段是逐个 `Option`（没写 = 跟随基线），理论上界面可以做成
//! 每项三态。实际选的是**一个总开关**：
//!
//! - 关 → 该方案一个行为字段都不写，整体跟随基线（普通方案跟随全局 `schema.codetable`，
//!   特殊方案跟随内置基线，见 core `EngineManager::codetable_baseline`）
//! - 开 → 全部写实，从**当前生效值**（`effectiveCodetable`）起步
//!
//! 代价是开了之后该方案不再跟随全局。这对特殊方案没有损失（它本就不继承全局），对普通
//! 方案是明确的取舍——「我要单独调这个表」本就是整体意图，不是逐项的。
//!
//! # 初值必须来自 `effectiveCodetable`
//!
//! 方案配置里这些字段未设置时是 `null`，不能显示成开关。而「不设置时实际是什么」取决于
//! 基线，基线又分普通/特殊两种，UI 侧算不出来——所以由 core 随 `schema.getConfig` 一并
//! 给出。拿 `false` 当初值会让用户一开总开关就把一堆功能悄悄关掉。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

use crate::manifest::{Manifest, cond_matches, parse_conds};
use crate::widgets::setting_row;

/// manifest 中承载码表配置的 section 名（与全局页的「设置」按钮取同一组）。
pub const CT_SECTION: &str = "上屏行为";

/// manifest key 的方案级前缀：`schema.codetable.x` ↔ 方案配置的 `engine.codetable.x`。
const MANIFEST_PREFIX: &str = "schema.codetable.";

/// `CodeTableSpec` 里可被方案覆盖的**行为字段**全集（core `wind-config/src/schema.rs`）。
///
/// 用途有两个，缺一不可：
/// 1. 判断「该方案是否已自定义」——任一字段非 null 即为已开
/// 2. 关闭总开关时逐个置 null 清除 override
///
/// 必须是全集而**不能**从 manifest 的可见项推导：`auto_commit_min_len` 在 manifest 里是
/// `hidden = true`（不渲染），但它照样能落进 override——只按可见项清除会把它留下，
/// 于是「关掉自定义」之后仍有一个字段在偷偷生效。
///
/// 不含 `max_code_length` / `base_sort` / `input_chars`：那些是方案自身的定义（码表怎么
/// 编的），不是用户偏好，不归这个开关管。
const SPEC_BEHAVIOR_FIELDS: &[&str] = &[
    "top_code_commit",
    "clear_on_empty_max",
    "auto_commit_at_full",
    "auto_commit_min_len",
    "punct_commit",
    "show_code_hint",
    "single_code_input",
    "single_code_complete",
    "z_key_repeat",
    // z 的另一个身份（引导键）。与 z_key_repeat 同为用户偏好、且同样按方案覆盖——
    // z 是不是死码取决于这张码表，故必须逐方案可调。
    "z_key_action",
];

/// 一个方案级配置项的工作态信号。三种形态对应 manifest 的 toggle / number / select。
#[derive(Clone)]
pub enum CtSig {
    Bool(Signal<bool>),
    Num(Signal<f64>),
    /// 下拉：信号存的是**索引**，写回时映射回 `values[idx]`。
    Sel {
        sig: Signal<usize>,
        values: Rc<Vec<String>>,
    },
}

impl CtSig {
    /// 信号版本号（脏判定用）。三种形态都只有一个信号，取它的即可。
    fn version(&self) -> u64 {
        match self {
            CtSig::Bool(s) => s.version(),
            CtSig::Num(s) => s.version(),
            CtSig::Sel { sig, .. } => sig.version(),
        }
    }

    /// 当前值的 JSON 形态（供 `enabled_when` 求值与写回共用——两处取值必须同源，
    /// 否则会出现「联动按 A 判、落盘写 B」）。
    pub fn value(&self) -> Value {
        match self {
            CtSig::Bool(s) => Value::Bool(s.get()),
            CtSig::Num(s) => {
                let v = s.get();
                // 整数形态回整数：`half_life` 是 f64，`protect_top_n` 是 usize，
                // 写成 72.0 会让 TOML 落一个 float，core 反序列化 usize 直接失败。
                if v.fract() == 0.0 {
                    Value::from(v as i64)
                } else {
                    Value::from(v)
                }
            }
            CtSig::Sel { sig, values } => values
                .get(sig.get())
                .map(|v| Value::String(v.clone()))
                .unwrap_or(Value::Null),
        }
    }
}

/// 渲染计划的一项：分组标题或配置行。
///
/// 存 key 而非 Element，因为 `Element::host_signal` 的数据项要求 `Clone`。
#[derive(Clone)]
pub enum CtItem {
    Header(String),
    Field(String),
}

/// 方案级码表配置的工作态：打开方案设置对话框时播种，点「保存」时落盘。
#[derive(Clone, Default)]
pub struct CtFields {
    /// manifest key → 信号。仅含实际渲染出来的项。
    sigs: Rc<RefCell<HashMap<String, CtSig>>>,
    /// 渲染顺序（HashMap 无序，行的先后必须按 manifest 原序）。
    order: Rc<RefCell<Vec<String>>>,
    /// key → subsection（算渲染计划时用；避免 plan() 反查 manifest 而多传一个参数）。
    subsecs: Rc<RefCell<HashMap<String, Option<String>>>>,
    /// `seed` 时各信号 version 之和。与当前值不等 = 用户动过某一项。
    ///
    /// 比「逐项存快照再比值」省事，且对「改了又改回去」同样判脏——那种情况下确实
    /// 该写一次盘：用户可能是在撤销上一次保存的结果。
    ver_at_seed: Rc<std::cell::Cell<u64>>,
}

impl CtFields {
    /// 按 manifest 建立信号，初值取自 `effective`（core 给的当前生效值快照）。
    ///
    /// `effective` 是 `CodetableGlobal` 的序列化形态，键名与 manifest key 去掉
    /// `schema.codetable.` 前缀后逐字对应。
    pub fn seed(&self, manifest: &Manifest, effective: &Value) {
        let mut sigs = HashMap::new();
        let mut order = Vec::new();
        let mut subsecs = HashMap::new();
        for key in visible_keys(manifest) {
            let Some(item) = manifest.get(&key) else {
                continue;
            };
            let cur = effective.pointer(&effective_pointer(&key));
            let sig = match item.typ.as_str() {
                "toggle" => CtSig::Bool(signal(cur.and_then(|v| v.as_bool()).unwrap_or(false))),
                "number" => CtSig::Num(signal(cur.and_then(|v| v.as_f64()).unwrap_or(0.0))),
                "select" => {
                    let values: Vec<String> =
                        item.options.iter().map(|o| o.value.clone()).collect();
                    let s = cur.and_then(|v| v.as_str()).unwrap_or("");
                    let idx = values.iter().position(|v| v == s).unwrap_or(0);
                    CtSig::Sel {
                        sig: signal(idx),
                        values: Rc::new(values),
                    }
                }
                // 其余控件类型方案级用不到；出现即跳过（不渲染也不落盘），
                // 好过渲染一个读写落点不明的控件。
                _ => continue,
            };
            subsecs.insert(key.clone(), item.subsection.clone());
            sigs.insert(key.clone(), sig);
            order.push(key);
        }
        *self.sigs.borrow_mut() = sigs;
        *self.order.borrow_mut() = order;
        *self.subsecs.borrow_mut() = subsecs;
        self.ver_at_seed.set(self.version_sum());
    }

    /// 用户是否动过其中任何一项（相对上次 `seed`）。
    pub fn is_dirty(&self) -> bool {
        self.version_sum() != self.ver_at_seed.get()
    }

    fn version_sum(&self) -> u64 {
        self.sigs
            .borrow()
            .values()
            .fold(0u64, |acc, s| acc.wrapping_add(s.version()))
    }

    /// 渲染计划（按 manifest 原序，subsection 变化处插标题）。
    ///
    /// 与实际渲染分成两步，是因为对话框结构在**打开方案设置之前**就建好了，而信号要等
    /// 选中方案才播种——中间靠 `Element::host_signal` 按计划信号重建。计划里只放 key，
    /// 不放 Element：`host_signal` 的数据项要求 `Clone`，Element 不是。
    pub fn plan(&self) -> Vec<CtItem> {
        let mut out = Vec::new();
        let mut cur_subsec: Option<String> = None;
        for key in self.order.borrow().iter() {
            let subsec = self.subsecs.borrow().get(key).cloned().flatten();
            if subsec != cur_subsec {
                if let Some(name) = &subsec {
                    out.push(CtItem::Header(name.clone()));
                }
                cur_subsec = subsec;
            }
            out.push(CtItem::Field(key.clone()));
        }
        out
    }

    /// 渲染计划中的一项。
    pub fn row(&self, manifest: &Manifest, plan_item: CtItem) -> Element {
        let key = match plan_item {
            CtItem::Header(name) => return crate::manifest::dialog_group_header(&name),
            CtItem::Field(k) => k,
        };
        let (Some(item), Some(sig)) = (manifest.get(&key), self.sigs.borrow().get(&key).cloned())
        else {
            return Element::col();
        };
        let control = match &sig {
            CtSig::Bool(s) => Element::switch(*s),
            CtSig::Num(s) => Element::stepper(
                *s,
                item.min.unwrap_or(0.0),
                item.max.unwrap_or(100.0),
                item.step.unwrap_or(1.0),
            )
            .width(CTRL_W),
            CtSig::Sel { sig, values } => {
                let labels: Vec<String> = values
                    .iter()
                    .map(|v| {
                        item.options
                            .iter()
                            .find(|o| &o.value == v)
                            .map(|o| o.label.clone())
                            .unwrap_or_else(|| v.clone())
                    })
                    .collect();
                Element::dropdown(labels, *sig).min_width(CTRL_W)
            }
        };
        let mut row = setting_row(&item.label, item.hint.as_deref(), control);
        // 联动按**方案级信号**求值，不是全局 FieldRegistry：同名 key 在两处的值可以
        // 不同（全局开了调频、这个方案没开），拿全局的判会让整组控件的可用性说谎。
        if let Some(f) = self.enabled_when(item.enabled_when.as_deref()) {
            row = row.enabled_when(f);
        }
        row
    }

    /// 由 `enabled_when` 表达式构造联动闭包（取值来自本组信号）。
    /// 引用了本组没有的 key 时该子句视为满足——与全局页同向：拼写错误只让联动失效，
    /// 不该把控件永久锁死。
    fn enabled_when(&self, expr: Option<&str>) -> Option<Box<dyn Fn() -> bool>> {
        let conds = parse_conds(expr?)?;
        let sigs = self.sigs.clone();
        Some(Box::new(move || {
            let m = sigs.borrow();
            conds.iter().all(|c| {
                m.get(&c.key)
                    .map(|s| cond_matches(&s.value(), c))
                    .unwrap_or(true)
            })
        }))
    }

    /// 把工作态写进方案配置（`cfg` 是 `schema.getConfig` 拿到的合并结果，回传 saveConfig）。
    ///
    /// `enabled = false` 时把**所有**可覆盖字段置 null：`json_to_toml` 跳过 null，
    /// 于是 override 里不含这些键 = 跟随基线。调频段整体置 null 而非逐字段——段内字段
    /// 可能多于 manifest 暴露的那几个，逐个清会漏。
    pub fn apply(&self, cfg: &mut Value, enabled: bool) {
        if !cfg.is_object() {
            *cfg = json!({});
        }
        if !enabled {
            for f in SPEC_BEHAVIOR_FIELDS {
                set_path(cfg, &["engine", "codetable", f], Value::Null);
            }
            set_path(cfg, &["engine", "codetable", "frequency"], Value::Null);
            return;
        }
        for (key, sig) in self.sigs.borrow().iter() {
            let rest = key.trim_start_matches(MANIFEST_PREFIX);
            let mut path = vec!["engine", "codetable"];
            path.extend(rest.split('.'));
            set_path(cfg, &path, sig.value());
        }
        // 隐藏项（manifest `hidden = true`，如 auto_commit_min_len）没有信号，但总开关
        // 打开时它也该有确定的值——否则「自定义」状态下它仍跟随基线，与本模式的整段
        // 语义相悖。用生效值补齐由调用方在 seed 时保证：见 `apply_hidden_from`。
    }

    /// 用生效值补齐未渲染的隐藏项（`apply` 之后调用）。
    ///
    /// 拆成单独一步是因为它的取值来源与信号无关——那些项没有控件，值只能原样取
    /// `effectiveCodetable`。合进 `apply` 就得把 effective 一路传下去，反而绕。
    pub fn apply_hidden(&self, cfg: &mut Value, effective: &Value) {
        let have = self.sigs.borrow();
        for f in SPEC_BEHAVIOR_FIELDS {
            let key = format!("{MANIFEST_PREFIX}{f}");
            if have.contains_key(&key) {
                continue;
            }
            if let Some(v) = effective.get(*f) {
                set_path(cfg, &["engine", "codetable", f], v.clone());
            }
        }
    }
}

/// 该方案是否已有自定义码表配置（总开关的初值）。
///
/// 判据是「`engine.codetable` 下任一行为字段非 null，或 frequency 段存在」。不另立一个
/// 布尔配置键，是因为多一个键就多一处可能与实际内容不一致的状态——用内容本身推导，
/// 手写过配置文件的用户打开设置页也能看到正确的开关态。
pub fn has_custom(cfg: &Value) -> bool {
    let Some(ct) = cfg.pointer("/engine/codetable") else {
        return false;
    };
    if ct
        .get("frequency")
        .is_some_and(|f| !f.is_null() && f.as_object().is_some_and(|o| !o.is_empty()))
    {
        return true;
    }
    SPEC_BEHAVIOR_FIELDS
        .iter()
        .any(|f| ct.get(*f).is_some_and(|v| !v.is_null()))
}

/// manifest 中属于方案级码表配置的可见项（按原序）。
///
/// 排除两类：
/// - `hidden = true`：与全局对话框一致，不渲染（但仍参与落盘，见 `apply_hidden`）
/// - `auto_phrase.*`：core 的 `CodeTableSpec` **没有**这一段，方案文件里写了也没人读。
///   自动造词目前只有全局一份配置。core 若补上方案级覆盖，把这条排除去掉即可，
///   `manifest_keys_match_spec_fields` 会提醒。
fn visible_keys(manifest: &Manifest) -> Vec<String> {
    section_keys(manifest)
        .into_iter()
        .filter(|k| !k.starts_with("schema.codetable.auto_phrase."))
        .filter(|k| manifest.get(k).is_some_and(|i| !i.hidden))
        .collect()
}

/// section 内的全部 key（含 hidden），按 manifest 原序。
fn section_keys(manifest: &Manifest) -> Vec<String> {
    manifest
        .sections_of("schema")
        .into_iter()
        .find(|(s, _)| s == CT_SECTION)
        .map(|(_, k)| k)
        .unwrap_or_default()
        .into_iter()
        .filter(|k| k.starts_with(MANIFEST_PREFIX))
        .collect()
}

/// manifest key → `effectiveCodetable` 内的 JSON pointer。
fn effective_pointer(key: &str) -> String {
    format!(
        "/{}",
        key.trim_start_matches(MANIFEST_PREFIX).replace('.', "/")
    )
}

/// 控件宽度基线（与 manifest 渲染器同值，两个对话框的控件必须对齐）。
const CTRL_W: i32 = 150;

/// 写入嵌套路径（缺失层自动建对象）。与 `schema_manager::set_json_path` 同构，
/// 独立一份是为了让本模块不反向依赖对话框。
fn set_path(v: &mut Value, path: &[&str], leaf: Value) {
    let Some((last, mids)) = path.split_last() else {
        return;
    };
    let mut cur = v;
    for k in mids {
        let Some(obj) = cur.as_object_mut() else {
            return;
        };
        let slot = obj.entry((*k).to_string()).or_insert_with(|| json!({}));
        if !slot.is_object() {
            *slot = json!({});
        }
        cur = slot;
    }
    if let Some(o) = cur.as_object_mut() {
        o.insert((*last).to_string(), leaf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest::from_embedded()
    }

    /// 清单与 core `CodeTableSpec` 的行为字段必须一一对应。
    ///
    /// 两个方向都要守：
    /// - manifest 有、清单没有 → 该项在方案级**静默丢失**（控件能拨，保存后没落盘）
    /// - 清单有、manifest 没有 → 关总开关时清了一个界面上根本不存在的键（无害但说明漂了）
    ///
    /// 唯一豁免是 `auto_phrase.*`：core 的 `CodeTableSpec` 没有这一段。core 若补上，
    /// 本测试会红，届时把 `visible_keys` 里那条 filter 去掉。
    #[test]
    fn manifest_keys_match_spec_fields() {
        let m = manifest();
        let keys = section_keys(&m);
        assert!(!keys.is_empty(), "manifest 里 `{CT_SECTION}` 分节不该为空");

        let (auto_phrase, rest): (Vec<String>, Vec<String>) = keys
            .into_iter()
            .partition(|k| k.starts_with("schema.codetable.auto_phrase."));
        assert!(
            !auto_phrase.is_empty(),
            "auto_phrase 组消失了：要么被挪走，要么 core 补了方案级覆盖——\
             后者的话去掉 visible_keys 的那条 filter"
        );

        let mut behavior: Vec<String> = rest
            .iter()
            .map(|k| k.trim_start_matches(MANIFEST_PREFIX).to_string())
            .filter(|k| !k.starts_with("frequency."))
            .collect();
        behavior.sort();
        let mut expect: Vec<String> = SPEC_BEHAVIOR_FIELDS.iter().map(|s| s.to_string()).collect();
        expect.sort();
        assert_eq!(
            behavior, expect,
            "manifest 的码表行为项与 SPEC_BEHAVIOR_FIELDS 不一致；\
             core 的 CodeTableSpec 加/删字段时两边都要改"
        );
    }

    /// 初值必须来自生效值。用一个「与结构体零值相反」的 effective 做输入：
    /// 若实现退回了 `unwrap_or(false)`，这些断言会全红。
    #[test]
    fn seed_takes_initial_from_effective() {
        let m = manifest();
        let eff = json!({
            "top_code_commit": true,
            "punct_commit": true,
            "single_code_input": true,
            "show_code_hint": false,
            "frequency": { "enabled": true, "strategy": "position", "protect_top_n_len1": 7 },
        });
        let f = CtFields::default();
        f.seed(&m, &eff);
        let sigs = f.sigs.borrow();
        let b = |k: &str| match sigs.get(k) {
            Some(CtSig::Bool(s)) => s.get(),
            _ => panic!("{k} 不是 bool 项"),
        };
        assert!(b("schema.codetable.top_code_commit"));
        assert!(b("schema.codetable.punct_commit"));
        assert!(b("schema.codetable.single_code_input"));
        assert!(!b("schema.codetable.show_code_hint"), "false 也要如实取到");
        assert!(
            b("schema.codetable.frequency.enabled"),
            "嵌套段同样按生效值"
        );

        match sigs.get("schema.codetable.frequency.strategy") {
            Some(CtSig::Sel { sig, values }) => {
                assert_eq!(values[sig.get()], "position", "下拉初值应落在生效项上");
            }
            _ => panic!("strategy 应是 select"),
        }
        match sigs.get("schema.codetable.frequency.protect_top_n_len1") {
            Some(CtSig::Num(s)) => assert_eq!(s.get(), 7.0),
            _ => panic!("protect_top_n_len1 应是 number"),
        }
    }

    /// 关掉总开关必须把**所有**可覆盖字段置 null——包括界面上没有的隐藏项。
    /// 漏掉隐藏项的话，用户关了自定义，`auto_commit_min_len` 仍在 override 里生效。
    #[test]
    fn disabling_clears_every_overridable_field() {
        let m = manifest();
        let f = CtFields::default();
        f.seed(&m, &json!({}));
        let mut cfg = json!({
            "engine": { "codetable": {
                "max_code_length": 4,
                "top_code_commit": true,
                "auto_commit_min_len": 3,
                "frequency": { "enabled": true },
            }}
        });
        f.apply(&mut cfg, false);
        let ct = cfg.pointer("/engine/codetable").unwrap();
        for field in SPEC_BEHAVIOR_FIELDS {
            assert_eq!(
                ct.get(*field),
                Some(&Value::Null),
                "{field} 未置 null，关掉自定义后它会继续生效"
            );
        }
        assert_eq!(ct.get("frequency"), Some(&Value::Null), "调频段应整段清除");
        assert_eq!(
            ct.get("max_code_length"),
            Some(&json!(4)),
            "引擎固定参数不归总开关管，不该被清"
        );
    }

    /// 开启时逐项写实，且隐藏项由生效值补齐——否则「自定义」状态下它仍跟随基线，
    /// 与整段语义相悖。
    #[test]
    fn enabling_writes_all_fields_including_hidden() {
        let m = manifest();
        let eff = json!({
            "top_code_commit": true,
            "auto_commit_min_len": 3,
            "frequency": { "enabled": true, "strategy": "top" },
        });
        let f = CtFields::default();
        f.seed(&m, &eff);
        let mut cfg = json!({ "engine": { "codetable": { "max_code_length": 4 } } });
        f.apply(&mut cfg, true);
        f.apply_hidden(&mut cfg, &eff);
        let ct = cfg.pointer("/engine/codetable").unwrap();
        assert_eq!(ct.get("top_code_commit"), Some(&json!(true)));
        assert_eq!(
            ct.get("auto_commit_min_len"),
            Some(&json!(3)),
            "隐藏项应由生效值补齐，不能留 null"
        );
        assert_eq!(ct.pointer("/frequency/enabled"), Some(&json!(true)));
        assert_eq!(ct.pointer("/frequency/strategy"), Some(&json!("top")));
        // 数值项必须落成整数：TOML 里的 3.0 会让 core 反序列化 usize 失败。
        assert!(
            ct.get("auto_commit_min_len").unwrap().is_i64(),
            "整数值不该写成浮点"
        );
    }

    /// 脏判定决定「打开对话框看一眼再关掉」会不会写盘。
    ///
    /// 两个方向都要守：漏判则改动丢失；误判则每次打开关闭都写一次 override，
    /// 且整个设置界面会一直显示「有未保存改动」。
    ///
    /// 最后一条最容易漏：对话框复用同一组信号，换方案重新 seed 时若不重置基准，
    /// 上一个方案的脏状态会被带到下一个方案身上。
    #[test]
    fn is_dirty_resets_on_reseed() {
        let m = manifest();
        let f = CtFields::default();
        f.seed(&m, &json!({ "punct_commit": true }));
        assert!(!f.is_dirty(), "刚播种完不该判脏——用户还没动过任何一项");

        match f.sigs.borrow().get("schema.codetable.punct_commit") {
            Some(CtSig::Bool(s)) => s.set(false),
            _ => panic!("punct_commit 应是 bool 项"),
        }
        assert!(f.is_dirty(), "拨动任一项即为脏");

        // 换方案（重新播种）：基准跟着重置，否则上一个方案的脏状态会赖到这个方案头上。
        f.seed(&m, &json!({ "punct_commit": true }));
        assert!(!f.is_dirty(), "换方案重新播种后应回到干净");
    }

    /// 总开关初值靠内容推导，不另存一个布尔键。手写过配置文件的用户打开设置页
    /// 也该看到「已自定义」。
    #[test]
    fn has_custom_detects_written_fields() {
        assert!(!has_custom(&json!({})), "空配置 = 未自定义");
        assert!(
            !has_custom(&json!({ "engine": { "codetable": { "max_code_length": 4 } } })),
            "只有引擎固定参数不算自定义"
        );
        assert!(
            !has_custom(&json!({ "engine": { "codetable": { "top_code_commit": null } } })),
            "显式 null = 跟随基线，不算自定义"
        );
        assert!(has_custom(
            &json!({ "engine": { "codetable": { "top_code_commit": false } } })
        ));
        assert!(
            has_custom(&json!({ "engine": { "codetable": { "frequency": { "enabled": true } } } })),
            "只写了调频段也算"
        );
    }

    /// 联动取值必须来自本组信号。构造一个「全局若为真则联动会说谎」的场景：
    /// 本组 `frequency.enabled` 为 false 时，策略行必须判为禁用。
    #[test]
    fn enabled_when_reads_scheme_scoped_signals() {
        let m = manifest();
        let f = CtFields::default();
        f.seed(
            &m,
            &json!({ "frequency": { "enabled": false, "strategy": "position" } }),
        );
        let clo = f
            .enabled_when(Some("schema.codetable.frequency.enabled == true"))
            .expect("表达式应能解析");
        assert!(!clo(), "本组调频关着，联动项应判为禁用");

        // 翻转本组信号后应立即改判（闭包读的是信号当前值，不是快照）。
        if let Some(CtSig::Bool(s)) = f.sigs.borrow().get("schema.codetable.frequency.enabled") {
            s.set(true);
        }
        assert!(clo(), "本组调频打开后应判为启用");
    }
}
