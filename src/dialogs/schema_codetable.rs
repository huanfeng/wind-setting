//! 方案设置 —— 方案对话框「方案设置」「辅助码」两页：单个方案自己的码表 / 拼音 / 混输 / 英文行为。
//!
//! # 与方案页引擎卡共用 manifest 条目
//!
//! 每一项的 label、hint、控件类型、数值范围、`enabled_when` 联动都取自
//! `settings_manifest.toml` 里那几个引擎 section 的条目，与方案页的引擎卡是同一份定义。
//! 引擎卡只放常用项（`on_card = false` 的项不上卡），这两页列出全部。
//! 控件不能直接复用 `manifest::field()`：那条路径把信号注册进主窗口的 `FieldRegistry`，
//! 这里的工作态由本模块自持，随方案对话框的「保存」提交。
//!
//! # 模型：完整表单
//!
//! 每项都列出，控件值即生效值。每项有一个方案默认值（方案文件写的，没写则是该类方案的内置
//! 默认）；当前值与它不同即「已修改」，行内可单独恢复。
//!
//! 数据来自 core `schema.getSettings`（键沿用 `schema.<类型>.<项>`）：
//!
//! | 字段 | 回答的问题 |
//! | --- | --- |
//! | `effective` | 现在按什么跑（控件的初值） |
//! | `default` | 恢复默认后变成什么 |
//! | `builtin` | 方案文件改过的项原本的内置默认（「本方案预设」） |
//!
//! 保存走 `schema.setSettings`：与默认不同的项写值，打开时改过、现在回到默认的项恢复默认。
//!
//! 模块与类型名里的 `Ct` / `codetable` 是历史遗留（最初只有码表方案有这一页）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::Value;
use windui::prelude::*;

use crate::manifest::{MItem, Manifest, cond_matches, parse_conds};

/// manifest 中承载码表配置的 section 名（与方案页码表卡取同一组）。
pub const CT_SECTION: &str = "上屏行为";

/// 辅助码的设置项前缀：它在对话框里有自己的「辅助码」页，不进「方案设置」页。
pub const AUX_PREFIX: &str = "schema.pinyin.aux_code.";

/// 这一页设的是哪类方案。决定取 manifest 的哪些 section、认哪些键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchemeKind {
    #[default]
    Codetable,
    Pinyin {
        /// 双拼方案多出「双拼」那一组（`schema.pinyin.shuangpin.*`）。
        shuangpin: bool,
    },
    Mixed,
    English,
}

impl SchemeKind {
    /// 按方案的 `[engine] type` 与拼音子类型判；不是这四类返回 `None`。
    pub fn of(engine_type: &str, pinyin_scheme: &str) -> Option<Self> {
        match engine_type {
            "codetable" => Some(Self::Codetable),
            "pinyin" => Some(Self::Pinyin {
                shuangpin: pinyin_scheme == "shuangpin",
            }),
            "mixed" => Some(Self::Mixed),
            "english" => Some(Self::English),
            _ => None,
        }
    }

    fn sections(self) -> &'static [&'static str] {
        match self {
            Self::Codetable => &[CT_SECTION],
            Self::Pinyin { shuangpin: false } => &["候选行为"],
            Self::Pinyin { shuangpin: true } => &["双拼", "候选行为"],
            Self::Mixed => &["混输行为"],
            Self::English => &["英文行为"],
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Codetable => "schema.codetable.",
            Self::Pinyin { .. } => "schema.pinyin.",
            Self::Mixed => "schema.mix.",
            Self::English => "schema.english.",
        }
    }

    /// 「方案设置」页收不收这个键。辅助码有自己的页；双拼专有项只给双拼方案。
    /// 引擎卡上的双拼镜像项（`schema.shuangpin.*`）前缀不符，自然不收。
    pub(crate) fn accepts(self, key: &str) -> bool {
        key.starts_with(self.prefix())
            && !key.starts_with(AUX_PREFIX)
            && (self == Self::Pinyin { shuangpin: true }
                || !key.starts_with("schema.pinyin.shuangpin."))
    }
}

/// 一个方案级配置项的**值**信号。三种形态对应 manifest 的
/// toggle / number / select + number_select。
#[derive(Clone)]
pub enum CtSig {
    Bool(Signal<bool>),
    Num(Signal<f64>),
    /// 下拉：信号存的是**索引**，写回时映射回 `values[idx]`。
    ///
    /// `numeric` 区分 manifest 的 `select` 与 `number_select`：两者的 options 都把 value
    /// 写成字符串，但后者对应的 core 字段是整数（`short_code_yield_level: Option<usize>`）。
    /// 原样写字符串会让 TOML 落一个 `"3"`，core 反序列化 usize 直接失败——与下面
    /// `Num` 那条「整数形态回整数」是同一族陷阱的另一个入口。
    Sel {
        sig: Signal<usize>,
        values: Rc<Vec<String>>,
        numeric: bool,
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
            CtSig::Sel {
                sig,
                values,
                numeric,
            } => values
                .get(sig.get())
                .map(|v| {
                    if *numeric {
                        v.parse::<i64>().map(Value::from).unwrap_or(Value::Null)
                    } else {
                        Value::String(v.clone())
                    }
                })
                .unwrap_or(Value::Null),
        }
    }

    /// 把一个 JSON 值拨进控件（恢复默认用）。形态对不上就不动。
    fn set_value(&self, v: &Value) {
        match self {
            CtSig::Bool(s) => {
                if let Some(b) = v.as_bool() {
                    s.set(b);
                }
            }
            CtSig::Num(s) => {
                if let Some(n) = v.as_f64() {
                    s.set(n);
                }
            }
            CtSig::Sel {
                sig,
                values,
                numeric,
            } => {
                let want = if *numeric {
                    v.as_i64().map(|n| n.to_string())
                } else {
                    v.as_str().map(str::to_string)
                };
                if let Some(i) = want.and_then(|w| values.iter().position(|x| *x == w)) {
                    sig.set(i);
                }
            }
        }
    }
}

/// 一个值的显示文案，走 manifest 的 label：下拉显示「全部简码」而不是 `3`，开关显示
/// 「开」而不是 `true`——提示里读到的东西必须和控件上看到的一致。
fn display_value(item: &MItem, v: &Value) -> String {
    match item.typ.as_str() {
        "toggle" => if v.as_bool().unwrap_or(false) {
            "开"
        } else {
            "关"
        }
        .to_string(),
        "select" | "number_select" => {
            let raw = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            item.options
                .iter()
                .find(|o| o.value == raw)
                .map(|o| o.label.clone())
                .unwrap_or(raw)
        }
        _ => {
            let mut t = match v.as_f64() {
                Some(n) if n.fract() == 0.0 => format!("{}", n as i64),
                Some(n) => format!("{n}"),
                None => v.to_string(),
            };
            if let Some(u) = &item.unit {
                t.push_str(u);
            }
            t
        }
    }
}

/// 一项的工作态：控件信号 + 方案默认值。
#[derive(Clone)]
struct CtItemState {
    /// 控件值，初值取生效值，落盘写它。
    val: CtSig,
    /// 方案默认值（恢复默认后变成的值）。core 没给时取生效值，即视为没改过。
    def: Value,
}

impl CtItemState {
    fn modified(&self) -> bool {
        !same_value(&self.val.value(), &self.def)
    }
}

/// 渲染计划的一项：**一个分类**（标题 + 组内 key 列表）。
///
/// 存 key 而非 Element，因为 `Element::host_signal` 的数据项要求 `Clone`。
#[derive(Clone)]
pub struct CtGroup {
    pub title: String,
    pub keys: Vec<String>,
    /// 是不是第一组：首组小标题上方不画线（卡片顶边已是分隔）。
    pub first: bool,
}

/// 「本方案预设」里的一行：方案文件把这一项改成了什么、内置默认原本是什么。
#[derive(Clone, Debug, PartialEq)]
pub struct CtPreset {
    pub label: String,
    pub value: String,
    pub builtin: String,
}

/// `seed` 的入参：`schema.getSettings` 给的数据（键为 `schema.<类型>.<项>`）。
#[derive(Debug, Clone, Default)]
pub struct CtSeed {
    pub kind: SchemeKind,
    /// 当前生效值，控件的初值。
    pub effective: serde_json::Map<String, Value>,
    /// 方案默认值，恢复默认后的值。
    pub default: serde_json::Map<String, Value>,
    /// 方案文件改过的项原本的内置默认（只含与方案默认不同的项）。
    pub builtin: serde_json::Map<String, Value>,
}

/// 两个设置值是否相同；数值按大小比（`48` 与 `48.0` 是同一个值）。
fn same_value(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// 某项在表单里归哪一组。
///
/// 引擎卡把词频调整、自动造词、英文候选的参数收进了主开关的扩展设置
/// （`opens_dialog` / `dialog_group`），参数本身不带 `subsection`。这里没有对话框，
/// 按主开关的标签把「主开关 + 它的参数」归成一组，否则参数会掉进首组。
fn ct_group_of(manifest: &Manifest, item: &MItem) -> Option<String> {
    if item.opens_dialog.is_some() {
        return Some(item.label.clone());
    }
    if let Some(h) = manifest.dialog_host_of(item) {
        return Some(h.label.clone());
    }
    item.subsection.clone()
}

/// 方案设置的工作态：打开方案对话框时播种，点「保存」时落盘。
#[derive(Clone, Default)]
pub struct CtFields {
    /// manifest key → 工作态。仅含实际渲染出来的项。
    items: Rc<RefCell<HashMap<String, CtItemState>>>,
    /// 渲染顺序（HashMap 无序，行的先后必须按 manifest 原序）。
    order: Rc<RefCell<Vec<String>>>,
    /// key → 所属分组标题（无分组的为 `None`，归到根组）。
    subsecs: Rc<RefCell<HashMap<String, Option<String>>>>,
    /// 无分组的项归到的组名。
    root_title: Rc<RefCell<String>>,
    /// 分类标题 → 该组的摘要信号（「已修改 N 项」）。由 `refresh_group_summaries` 推。
    group_disp: Rc<RefCell<HashMap<String, Signal<String>>>>,
    /// 控件位的统一宽度（见 `seed` 里的算法）。
    slot_w: Rc<std::cell::Cell<i32>>,
    /// `seed` 时各信号 version 之和。与当前值不等 = 用户动过某一项。
    ///
    /// 比「逐项存快照再比值」省事，且对「改了又改回去」同样判脏——那种情况下确实
    /// 该写一次盘：用户可能是在撤销上一次保存的结果。
    ver_at_seed: Rc<std::cell::Cell<u64>>,
    /// 该方案全部设置项的键（含界面上没有控件的），「全部恢复默认」按它清。
    all_keys: Rc<RefCell<Vec<String>>>,
    /// 播种时就已改过的项：保存时它们回到默认才需要发「恢复默认」。
    over_at_seed: Rc<RefCell<Vec<String>>>,
    /// 「本方案预设」：方案文件改过、且在本页有控件的项。
    presets: Rc<RefCell<Vec<CtPreset>>>,
}

impl CtFields {
    /// 按该类方案「方案设置」页的全部可见项播种。
    pub fn seed(&self, manifest: &Manifest, seed: &CtSeed) {
        let keys = visible_keys_of(manifest, seed.kind);
        let root = seed.kind.sections().last().copied().unwrap_or(CT_SECTION);
        self.seed_keys(manifest, seed, &keys, root);
    }

    /// 按给定的键播种（辅助码页只取辅助码那几项）。`root` 是无分组项归到的组名。
    ///
    /// 键名与 manifest key 逐字对应。`seed.default` 的全部键记作「全部恢复默认」的全集：
    /// 只给了部分 `keys` 时调用方负责传一份只含这些键的 `seed`。
    pub fn seed_keys(&self, manifest: &Manifest, seed: &CtSeed, keys: &[String], root: &str) {
        let mut items = HashMap::new();
        let mut order = Vec::new();
        let mut subsecs = HashMap::new();
        let mut over_at_seed = Vec::new();
        let mut presets = Vec::new();
        for key in keys {
            let Some(item) = manifest.get(key) else {
                continue;
            };
            // core 没给这一项（老 core、这类方案没有这个键）：不出控件，免得画一个读写落点不明的行。
            let Some(eff) = seed.effective.get(key) else {
                continue;
            };
            // 其余控件类型这一页还不支持；出现即跳过（不渲染也不落盘）。
            let Some(val) = make_sig(item, Some(eff)) else {
                continue;
            };
            let def = seed
                .default
                .get(key)
                .cloned()
                .unwrap_or_else(|| eff.clone());
            if !same_value(eff, &def) {
                over_at_seed.push(key.clone());
            }
            if let Some(b) = seed.builtin.get(key) {
                presets.push(CtPreset {
                    label: item.label.clone(),
                    value: display_value(item, &def),
                    builtin: display_value(item, b),
                });
            }
            subsecs.insert(key.clone(), ct_group_of(manifest, item));
            items.insert(key.clone(), CtItemState { val, def });
            order.push(key.clone());
        }
        *self.root_title.borrow_mut() = root.to_string();
        *self.all_keys.borrow_mut() = seed.default.keys().cloned().collect();
        *self.over_at_seed.borrow_mut() = over_at_seed;
        *self.presets.borrow_mut() = presets;
        *self.items.borrow_mut() = items;
        *self.order.borrow_mut() = order;
        *self.subsecs.borrow_mut() = subsecs;
        // ★ 控件位**所有行统一宽度**，取本页各项 manifest `width` 的最大值：逐行用自己的
        // `width` 会让右侧控件列参差不齐。控件自身仍按各自的 width 渲染，只有**容器**统一。
        let slot = keys
            .iter()
            .filter_map(|k| manifest.get(k).and_then(|i| i.width))
            .fold(CTRL_W, |acc, w| acc.max(w as i32));
        self.slot_w.set(slot);
        // 换方案要重建分类摘要：上一个方案的分类可能与这个方案不同。
        self.group_disp.borrow_mut().clear();
        for g in self.plan() {
            let _ = self.group_summary(&g.title);
        }
        self.refresh_group_summaries();
        self.ver_at_seed.set(self.version_sum());
    }

    /// 把控件位放宽到至少 `w`：同一张卡里还有别的行（辅助码来源下拉）时，控件列要对齐。
    pub fn widen_slot(&self, w: i32) {
        self.slot_w.set(self.slot_w.get().max(w));
    }

    #[cfg(test)]
    pub fn set_for_test(&self, key: &str, v: &Value) {
        self.items.borrow()[key].val.set_value(v);
    }

    /// 用户是否动过其中任何一项（相对上次 `seed`）。
    pub fn is_dirty(&self) -> bool {
        self.version_sum() != self.ver_at_seed.get()
    }

    pub fn version_sum(&self) -> u64 {
        self.items
            .borrow()
            .values()
            .fold(0u64, |acc, st| acc.wrapping_add(st.val.version()))
    }

    /// 与方案默认不同的项数。摘要用。
    pub fn custom_count(&self) -> usize {
        self.items
            .borrow()
            .values()
            .filter(|st| st.modified())
            .count()
    }

    /// 有没有可显示的项。
    pub fn is_empty(&self) -> bool {
        self.order.borrow().is_empty()
    }

    /// 「本方案预设」各行。
    pub fn presets(&self) -> Vec<CtPreset> {
        self.presets.borrow().clone()
    }

    /// 全部拨回方案默认值。
    ///
    /// 只动 UI，不碰配置——写盘统一走底栏的保存（见 core 仓那条「非 config.toml 落点一律
    /// 登记 SideCommitter，不加独立保存按钮」）。
    pub fn revert_all(&self) {
        for st in self.items.borrow().values() {
            st.val.set_value(&st.def);
        }
    }

    /// 某项归属的分类标题；没有小标题的项归到根组（该类方案的 section 名）。
    ///
    /// ⚠️ `plan` 与 `refresh_group_summaries` **必须都走这里**：两处各写一遍取值，摘要就会
    /// 落在一个没人显示的 key 上。
    fn group_title_of(&self, key: &str) -> String {
        self.subsecs
            .borrow()
            .get(key)
            .cloned()
            .flatten()
            .unwrap_or_else(|| self.root_title.borrow().clone())
    }

    /// 渲染计划：按分组切成若干**分类**（组内保持 manifest 原序）。
    pub fn plan(&self) -> Vec<CtGroup> {
        let mut out: Vec<CtGroup> = Vec::new();
        for key in self.order.borrow().iter() {
            let title = self.group_title_of(key);
            match out.last_mut() {
                Some(g) if g.title == title => g.keys.push(key.clone()),
                _ => out.push(CtGroup {
                    title,
                    keys: vec![key.clone()],
                    first: out.is_empty(),
                }),
            }
        }
        out
    }

    /// 各分类标题右侧的摘要信号（「已修改 N 项」/「全部默认」）。
    ///
    /// 由 [`Self::refresh_group_summaries`] 刷新，而不是每帧算——windui 的信号没有变更
    /// 回调，摘要只能由外部 effect 推。信号在 `seed` 时按分类建好，卡片渲染时取用。
    fn group_summary(&self, title: &str) -> Signal<String> {
        *self
            .group_disp
            .borrow_mut()
            .entry(title.to_string())
            .or_insert_with(|| signal(String::new()))
    }

    /// 重算各分类的摘要文案。挂在监听 [`Self::version_sum`] 的 effect 上。
    pub fn refresh_group_summaries(&self) {
        let mut per_group: HashMap<String, usize> = HashMap::new();
        let modified: Vec<String> = self
            .items
            .borrow()
            .iter()
            .filter(|(_, st)| st.modified())
            .map(|(k, _)| k.clone())
            .collect();
        for key in modified {
            *per_group.entry(self.group_title_of(&key)).or_default() += 1;
        }
        let titles: Vec<String> = self.group_disp.borrow().keys().cloned().collect();
        for t in titles {
            let n = per_group.get(&t).copied().unwrap_or(0);
            let text = if n == 0 {
                "全部默认".to_string()
            } else {
                format!("已修改 {n} 项")
            };
            self.group_summary(&t).set(text);
        }
    }

    /// 渲染一个分类：小标题（带本组摘要）+ 组内各行。
    ///
    /// 「全部恢复默认」是**整页**级别的动作，挂在外层节头（见 `schema_manager.rs`）。
    pub fn card(&self, manifest: &Manifest, group: CtGroup) -> Element {
        let mut col = Element::col().width_match().child({
            let sum = self.group_summary(&group.title);
            crate::widgets::list_heading_ext(
                &group.title,
                Some(&move || crate::widgets::summary_label(sum)),
                group.first,
            )
        });
        for k in group.keys.iter() {
            if !self.items.borrow().contains_key(k) {
                continue;
            }
            col = col
                .child(crate::widgets::row_divider())
                .child(crate::widgets::card_pad(self.row(manifest, k)));
        }
        col
    }

    /// 单独取某一项的行（辅助码页按自己的顺序排行）；本页没有这一项时为 `None`。
    pub fn row_of(&self, manifest: &Manifest, key: &str) -> Option<Element> {
        self.items
            .borrow()
            .contains_key(key)
            .then(|| self.row(manifest, key))
    }

    /// 组内一行：控件 + 「已修改」标记与恢复按钮。
    fn row(&self, manifest: &Manifest, key: &str) -> Element {
        let (Some(item), Some(st)) = (manifest.get(key), self.items.borrow().get(key).cloned())
        else {
            return Element::col();
        };
        // 控件按自己的 width 渲染，容器统一——两者分开，见 `seed` 里那段说明。
        let width = item.width.map(|w| w as i32).unwrap_or(CTRL_W);
        let slot = self.slot_w.get().max(width);
        // 联动按**本页信号**求值，不是全局 FieldRegistry：同名 key 在两处的值可以不同
        // （引擎卡绑的是另一个方案），拿全局的判会让整组控件的可用性说谎。
        let gate: Rc<dyn Fn() -> bool> = match self.enabled_when(item.enabled_when.as_deref()) {
            Some(f) => Rc::from(f),
            None => Rc::new(|| true),
        };
        let st_mod = st.clone();
        let st_reset = st.clone();
        let tip = format!("恢复方案默认值（{}）", display_value(item, &st.def));
        let control = Element::row()
            .width(slot)
            .cross(Align::Center)
            .child(Element::col().weight(1.0))
            .child(build_editor(item, &st.val, width));
        let g = gate.clone();
        crate::widgets::setting_row_scheme(
            &item.label,
            item.hint.as_deref(),
            control,
            Rc::new(move || st_mod.modified()),
            &tip,
            move || st_reset.val.set_value(&st_reset.def),
        )
        .enabled_when(move || g())
    }

    /// 由 `enabled_when` 表达式构造联动闭包（取值来自本页信号）。
    /// 引用了本页没有的 key 时该子句视为满足——与全局页同向：拼写错误只让联动失效，
    /// 不该把控件永久锁死。
    fn enabled_when(&self, expr: Option<&str>) -> Option<Box<dyn Fn() -> bool>> {
        let conds = parse_conds(expr?)?;
        let items = self.items.clone();
        Some(Box::new(move || {
            let m = items.borrow();
            conds.iter().all(|c| {
                m.get(&c.key)
                    .map(|st| cond_matches(&st.val.value(), c))
                    .unwrap_or(true)
            })
        }))
    }

    /// 要提交的改动：`(写入的项, 恢复默认的项)`，键为 `schema.<类型>.<项>`，原样交给
    /// `schema.setSettings`。
    ///
    /// - 与方案默认不同的项写值；
    /// - 打开时改过、现在回到默认的项恢复默认（不写一份等于默认的值：方案文件日后改了
    ///   默认，没动过的项该跟着走）；
    /// - `reset_all`（用户按过「全部恢复默认」）时，全集里没有控件的项也一并恢复，
    ///   但放过被隐藏的项（`hidden(key)`）：藏起来就是不让人在界面上动它。
    pub fn changes(
        &self,
        reset_all: bool,
        hidden: &dyn Fn(&str) -> bool,
    ) -> (Vec<(String, Value)>, Vec<String>) {
        let items = self.items.borrow();
        let mut sets: Vec<(String, Value)> = Vec::new();
        for key in self.order.borrow().iter() {
            if let Some(st) = items.get(key).filter(|st| st.modified()) {
                sets.push((key.clone(), st.val.value()));
            }
        }
        let is_set = |k: &str| sets.iter().any(|(s, _)| s == k);
        let mut resets: Vec<String> = self
            .over_at_seed
            .borrow()
            .iter()
            .filter(|k| !is_set(k))
            .cloned()
            .collect();
        if reset_all {
            resets.extend(
                self.all_keys
                    .borrow()
                    .iter()
                    .filter(|k| !items.contains_key(*k) && !hidden(k))
                    .cloned(),
            );
        }
        resets.sort();
        resets.dedup();
        (sets, resets)
    }
}

/// 按 manifest 项建一个值信号，初值取 `cur`（取不到用类型零值）。
fn make_sig(item: &MItem, cur: Option<&Value>) -> Option<CtSig> {
    Some(match item.typ.as_str() {
        "toggle" => CtSig::Bool(signal(cur.and_then(|v| v.as_bool()).unwrap_or(false))),
        "number" => CtSig::Num(signal(cur.and_then(|v| v.as_f64()).unwrap_or(0.0))),
        "select" | "number_select" => {
            let numeric = item.typ == "number_select";
            let values: Vec<String> = item.options.iter().map(|o| o.value.clone()).collect();
            // 生效值的 JSON 形态随类型不同（select 是字符串、number_select 是数字），
            // 但 options 的 value 一律是字符串——统一转成字符串再比，避免两边形态错位
            // 导致恒定落到 idx 0。
            let s = if numeric {
                cur.and_then(|v| v.as_i64()).map(|n| n.to_string())
            } else {
                cur.and_then(|v| v.as_str()).map(str::to_string)
            }
            .unwrap_or_default();
            let idx = values.iter().position(|v| v == &s).unwrap_or(0);
            CtSig::Sel {
                sig: signal(idx),
                values: Rc::new(values),
                numeric,
            }
        }
        _ => return None,
    })
}

/// 可编辑控件。
fn build_editor(item: &MItem, sig: &CtSig, width: i32) -> Element {
    match sig {
        CtSig::Bool(s) => Element::switch(*s),
        CtSig::Num(s) => Element::stepper(
            *s,
            item.min.unwrap_or(0.0),
            item.max.unwrap_or(100.0),
            item.step.unwrap_or(1.0),
        )
        .width(width),
        CtSig::Sel { sig, values, .. } => {
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
            Element::dropdown(labels, *sig).min_width(width)
        }
    }
}

/// 某类方案这一页的可见项，按 manifest 原序。
pub(crate) fn visible_keys_of(manifest: &Manifest, kind: SchemeKind) -> Vec<String> {
    section_keys(manifest, kind)
        .into_iter()
        .filter(|k| manifest.get(k).is_some_and(|i| !i.hidden))
        .collect()
}

/// 该类方案各 section 内的全部 key（含 hidden），按 manifest 原序。
fn section_keys(manifest: &Manifest, kind: SchemeKind) -> Vec<String> {
    let all = manifest.sections_of("schema");
    kind.sections()
        .iter()
        .flat_map(|sec| {
            all.iter()
                .find(|(s, _)| s == sec)
                .map(|(_, k)| k.clone())
                .unwrap_or_default()
        })
        .filter(|k| kind.accepts(k))
        .collect()
}

/// 控件宽度基线（与 manifest 渲染器同值，两个对话框的控件必须对齐）。
const CTRL_W: i32 = 150;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest() -> Manifest {
        Manifest::from_embedded()
    }

    /// 某类方案每个可见项一个类型零值（下拉取首项），作为 `effective` / `default` 的底。
    /// core 给的是全集，本页只为 core 给了的键建控件，测试也照全集喂。
    fn base(m: &Manifest, kind: SchemeKind) -> serde_json::Map<String, Value> {
        visible_keys_of(m, kind)
            .into_iter()
            .filter_map(|k| {
                let it = m.get(&k)?;
                let v = match it.typ.as_str() {
                    "toggle" => json!(false),
                    "number" => json!(it.min.unwrap_or(0.0) as i64),
                    "select" => json!(it.options.first()?.value),
                    "number_select" => json!(it.options.first()?.value.parse::<i64>().ok()?),
                    _ => return None,
                };
                Some((k, v))
            })
            .collect()
    }

    /// 在底上叠几项（键写全名）。
    fn with(mut m: serde_json::Map<String, Value>, over: Value) -> serde_json::Map<String, Value> {
        for (k, v) in over.as_object().unwrap() {
            m.insert(k.clone(), v.clone());
        }
        m
    }

    /// 码表方案的 `CtSeed`：生效值 = 底 + `eff`，方案默认 = 底 + `def`。
    fn ct_seed(m: &Manifest, eff: Value, def: Value) -> CtSeed {
        let b = base(m, SchemeKind::Codetable);
        CtSeed {
            kind: SchemeKind::Codetable,
            effective: with(b.clone(), eff),
            default: with(b, def),
            builtin: Default::default(),
        }
    }

    fn set_bool(f: &CtFields, key: &str, v: bool) {
        match &f.items.borrow()[key].val {
            CtSig::Bool(s) => s.set(v),
            _ => panic!("{key} 不是 bool 项"),
        }
    }

    fn modified(f: &CtFields, key: &str) -> bool {
        f.items.borrow()[key].modified()
    }

    /// 初值必须来自生效值。用一个「与零值相反」的 effective 做输入：若实现退回了
    /// `unwrap_or(false)`，这些断言会全红。
    #[test]
    fn seed_takes_initial_from_effective() {
        let m = manifest();
        let eff = json!({
            "schema.codetable.top_code_commit": true,
            "schema.codetable.single_code_input": true,
            "schema.codetable.frequency.enabled": true,
            "schema.codetable.frequency.strategy": "position",
            "schema.codetable.frequency.protect_top_n_len1": 7,
        });
        let f = CtFields::default();
        f.seed(&m, &ct_seed(&m, eff.clone(), eff));
        let items = f.items.borrow();
        let b = |k: &str| items[k].val.value();
        assert_eq!(b("schema.codetable.top_code_commit"), json!(true));
        assert_eq!(b("schema.codetable.single_code_input"), json!(true));
        assert_eq!(b("schema.codetable.frequency.enabled"), json!(true));
        assert_eq!(b("schema.codetable.frequency.strategy"), json!("position"));
        assert_eq!(b("schema.codetable.frequency.protect_top_n_len1"), json!(7));
    }

    /// 数值下拉（`number_select`）的初值按**数字**匹配，写回也是数字：写成字符串 `"2"`
    /// 会让 core 的 `Option<usize>` 反序列化失败。
    #[test]
    fn number_select_round_trips_as_an_integer() {
        let m = manifest();
        let eff = json!({ "schema.codetable.short_code_yield_level": 2 });
        let f = CtFields::default();
        f.seed(&m, &ct_seed(&m, eff.clone(), eff));
        let key = "schema.codetable.short_code_yield_level";
        assert_eq!(f.items.borrow()[key].val.value(), json!(2));
    }

    /// core 没给的键不出控件。
    #[test]
    fn keys_missing_from_core_get_no_row() {
        let m = manifest();
        let mut seed = ct_seed(&m, json!({}), json!({}));
        seed.effective.remove("schema.codetable.punct_commit");
        let f = CtFields::default();
        f.seed(&m, &seed);
        assert!(f.row_of(&m, "schema.codetable.punct_commit").is_none());
        assert!(f.row_of(&m, "schema.codetable.top_code_commit").is_some());
    }

    /// 渲染计划按分组切成若干**有标题的组**，参数跟着主开关成组，组不吞 key。
    #[test]
    fn plan_splits_into_titled_groups_by_subsection() {
        let m = manifest();
        let f = CtFields::default();
        f.seed(&m, &ct_seed(&m, json!({}), json!({})));
        let groups = f.plan();
        assert!(groups.len() >= 2, "实际 {} 组", groups.len());
        assert!(
            groups
                .iter()
                .all(|g| !g.title.is_empty() && !g.keys.is_empty())
        );
        let group_of_key = |k: &str| {
            groups
                .iter()
                .find(|g| g.keys.iter().any(|x| x == k))
                .map(|g| g.title.clone())
        };
        for (k, want) in [
            ("schema.codetable.frequency.enabled", "词频调整"),
            ("schema.codetable.frequency.strategy", "词频调整"),
            ("schema.codetable.auto_phrase.promote_count", "自动造词"),
            ("schema.codetable.english_merge.block_commit", "英文候选"),
        ] {
            assert_eq!(group_of_key(k).as_deref(), Some(want), "{k}");
        }
        assert_eq!(groups[0].title, CT_SECTION, "无分组的直属项排最前");
        let total: usize = groups.iter().map(|g| g.keys.len()).sum();
        assert_eq!(
            total,
            visible_keys_of(&m, SchemeKind::Codetable).len(),
            "分组前后的项数必须相等"
        );
    }

    /// 分类标题右侧的摘要只数**本组**改过的项；换方案重播后归零。
    #[test]
    fn group_summary_counts_only_its_own_group_and_resets_on_reseed() {
        let m = manifest();
        let f = CtFields::default();
        let seed = ct_seed(&m, json!({}), json!({}));
        f.seed(&m, &seed);
        let groups = f.plan();
        let (g0, g1) = (&groups[0].title, "词频调整");
        assert_eq!(f.group_summary(g0).get(), "全部默认");

        set_bool(&f, "schema.codetable.frequency.enabled", true);
        f.refresh_group_summaries();
        assert_eq!(f.group_summary(g0).get(), "全部默认", "另一组不该被算进来");
        assert_eq!(f.group_summary(g1).get(), "已修改 1 项");

        f.seed(&m, &seed);
        assert_eq!(
            f.group_summary(g1).get(),
            "全部默认",
            "换方案后摘要必须重置"
        );
    }

    /// 「已修改」= 当前值与方案默认不同；数值按大小比。
    #[test]
    fn modified_means_differing_from_scheme_default() {
        let m = manifest();
        let eff = json!({ "schema.codetable.top_code_commit": true,
                          "schema.codetable.frequency.half_life": 48,
                          "schema.codetable.frequency.protect_top_n": 3 });
        let def = json!({ "schema.codetable.top_code_commit": false,
                          "schema.codetable.frequency.half_life": 48.0,
                          "schema.codetable.frequency.protect_top_n": 0 });
        let f = CtFields::default();
        f.seed(&m, &ct_seed(&m, eff, def));
        assert!(modified(&f, "schema.codetable.top_code_commit"));
        assert!(modified(&f, "schema.codetable.frequency.protect_top_n"));
        assert!(
            !modified(&f, "schema.codetable.frequency.half_life"),
            "48 与 48.0 是同一个值"
        );
        assert_eq!(f.custom_count(), 2);
        // 改回默认值即不再算改过。
        set_bool(&f, "schema.codetable.top_code_commit", false);
        assert_eq!(f.custom_count(), 1);
    }

    /// 方案默认缺失时视为没改过（取生效值），不显示一个界面上从未有过的零值。
    #[test]
    fn missing_default_counts_as_unmodified() {
        let m = manifest();
        let mut seed = ct_seed(
            &m,
            json!({ "schema.codetable.punct_commit": true }),
            json!({}),
        );
        seed.default.remove("schema.codetable.punct_commit");
        let f = CtFields::default();
        f.seed(&m, &seed);
        assert!(!modified(&f, "schema.codetable.punct_commit"));
    }

    /// `revert_all` 把控件值拨回方案默认。
    #[test]
    fn revert_all_pulls_values_back_to_scheme_defaults() {
        let m = manifest();
        let f = CtFields::default();
        f.seed(
            &m,
            &ct_seed(
                &m,
                json!({ "schema.codetable.short_code_yield_level": 2 }),
                json!({ "schema.codetable.short_code_yield_level": 3 }),
            ),
        );
        let key = "schema.codetable.short_code_yield_level";
        assert_eq!(f.custom_count(), 1);
        f.revert_all();
        assert_eq!(f.custom_count(), 0);
        assert_eq!(f.items.borrow()[key].val.value(), json!(3));
    }

    /// 脏判定决定「打开看一眼再关掉」会不会写盘：刚播种不脏，拨动即脏，重播后回到干净。
    #[test]
    fn is_dirty_tracks_edits_and_resets_on_reseed() {
        let m = manifest();
        let f = CtFields::default();
        let seed = ct_seed(
            &m,
            json!({ "schema.codetable.punct_commit": true }),
            json!({}),
        );
        f.seed(&m, &seed);
        assert!(!f.is_dirty());
        set_bool(&f, "schema.codetable.punct_commit", false);
        assert!(f.is_dirty());
        f.seed(&m, &seed);
        assert!(!f.is_dirty());
    }

    /// 联动取值来自本页信号（不是全局 FieldRegistry），且随信号即时改判。
    #[test]
    fn enabled_when_reads_this_pages_values() {
        let m = manifest();
        let f = CtFields::default();
        f.seed(&m, &ct_seed(&m, json!({}), json!({})));
        let clo = f
            .enabled_when(Some("schema.codetable.frequency.enabled == true"))
            .expect("表达式应能解析");
        assert!(!clo());
        set_bool(&f, "schema.codetable.frequency.enabled", true);
        assert!(clo());
    }

    /// 提交：与默认不同的项写值；打开时改过、现在回到默认的项恢复默认；没动过的不出现。
    #[test]
    fn changes_set_modified_items_and_reset_reverted_ones() {
        let m = manifest();
        let f = CtFields::default();
        f.seed(
            &m,
            &ct_seed(
                &m,
                json!({ "schema.codetable.top_code_commit": true,
                        "schema.codetable.punct_commit": true }),
                json!({}),
            ),
        );
        set_bool(&f, "schema.codetable.punct_commit", false);
        set_bool(&f, "schema.codetable.wildcard", true);
        let (sets, resets) = f.changes(false, &|_| false);
        assert_eq!(
            sets,
            vec![
                ("schema.codetable.top_code_commit".to_string(), json!(true)),
                ("schema.codetable.wildcard".to_string(), json!(true)),
            ]
        );
        assert_eq!(resets, ["schema.codetable.punct_commit"]);
    }

    /// 「全部恢复默认」还清界面上没有控件的项，但放过被隐藏的项。
    #[test]
    fn reset_all_covers_keys_without_rows_except_hidden() {
        let m = manifest();
        let mut seed = ct_seed(
            &m,
            json!({ "schema.codetable.top_code_commit": true }),
            json!({}),
        );
        for k in [
            "schema.codetable.no_ui_for_this",
            "schema.codetable.hidden_one",
        ] {
            seed.effective.insert(k.into(), json!(1));
            seed.default.insert(k.into(), json!(0));
        }
        let f = CtFields::default();
        f.seed(&m, &seed);
        f.revert_all();
        let (sets, resets) = f.changes(true, &|k| k == "schema.codetable.hidden_one");
        assert!(sets.is_empty());
        assert_eq!(
            resets,
            [
                "schema.codetable.no_ui_for_this",
                "schema.codetable.top_code_commit"
            ]
        );
    }

    /// 「本方案预设」只列 core 标出的项，值走 manifest 的显示名。
    #[test]
    fn presets_list_scheme_file_values_with_builtin() {
        let m = manifest();
        let mut seed = ct_seed(
            &m,
            json!({}),
            json!({ "schema.codetable.z_key_repeat": false }),
        );
        seed.builtin
            .insert("schema.codetable.z_key_repeat".into(), json!(true));
        let f = CtFields::default();
        f.seed(&m, &seed);
        assert_eq!(
            f.presets(),
            vec![CtPreset {
                label: m
                    .get("schema.codetable.z_key_repeat")
                    .unwrap()
                    .label
                    .clone(),
                value: "关".into(),
                builtin: "开".into(),
            }]
        );
    }

    /// 只给部分键播种（辅助码页）：只建这些行，全部恢复默认的全集来自传入的快照。
    #[test]
    fn seed_keys_builds_only_the_given_rows() {
        let m = manifest();
        let map = |v: Value| v.as_object().unwrap().clone();
        let f = CtFields::default();
        let seed = CtSeed {
            kind: SchemeKind::Pinyin { shuangpin: true },
            effective: map(json!({ "schema.pinyin.aux_code.enabled": true,
                                   "schema.pinyin.aux_code.max_phrase_len": 4 })),
            default: map(json!({ "schema.pinyin.aux_code.enabled": false,
                                 "schema.pinyin.aux_code.max_phrase_len": 4 })),
            builtin: Default::default(),
        };
        f.seed_keys(
            &m,
            &seed,
            &[
                "schema.pinyin.aux_code.enabled".into(),
                "schema.pinyin.aux_code.max_phrase_len".into(),
            ],
            "辅助码",
        );
        assert_eq!(f.plan().len(), 1);
        assert_eq!(f.custom_count(), 1);
        f.revert_all();
        let (sets, resets) = f.changes(true, &|_| false);
        assert!(sets.is_empty());
        assert_eq!(resets, ["schema.pinyin.aux_code.enabled"]);
    }

    /// 各类方案这一页收哪些项：只收本类的键；辅助码另有一页；双拼专有项只给双拼方案；
    /// 引擎卡的双拼镜像项不进来。
    #[test]
    fn each_kind_lists_only_its_own_keys() {
        let m = manifest();
        let keys = |k| visible_keys_of(&m, k);
        let full = keys(SchemeKind::Pinyin { shuangpin: false });
        let sp = keys(SchemeKind::Pinyin { shuangpin: true });
        assert!(!full.is_empty());
        assert!(
            full.iter().all(|k| k.starts_with("schema.pinyin.")),
            "{full:?}"
        );
        assert!(!full.iter().any(|k| k.starts_with(AUX_PREFIX)));
        assert!(
            !full
                .iter()
                .any(|k| k.starts_with("schema.pinyin.shuangpin."))
        );
        assert!(sp.iter().all(|k| k.starts_with("schema.pinyin.")), "{sp:?}");
        assert!(
            sp.iter()
                .any(|k| k == "schema.pinyin.shuangpin.allow_full_pinyin")
        );
        assert_eq!(sp.len(), full.len() + 1, "双拼只多出双拼专有项");
        for (kind, prefix) in [
            (SchemeKind::Codetable, "schema.codetable."),
            (SchemeKind::Mixed, "schema.mix."),
            (SchemeKind::English, "schema.english."),
        ] {
            let ks = keys(kind);
            assert!(!ks.is_empty(), "{kind:?}");
            assert!(ks.iter().all(|k| k.starts_with(prefix)), "{kind:?} {ks:?}");
        }
        assert_eq!(
            SchemeKind::of("pinyin", "shuangpin"),
            Some(SchemeKind::Pinyin { shuangpin: true })
        );
        assert_eq!(SchemeKind::of("something", ""), None);
    }

    /// 引擎卡上不显示的项（`on_card = false`）在这一页照样有。
    #[test]
    fn form_includes_items_hidden_from_the_engine_card() {
        let m = manifest();
        let ks = visible_keys_of(&m, SchemeKind::Codetable);
        for k in [
            "schema.codetable.short_code_yield_level",
            "schema.codetable.wildcard",
            "schema.codetable.frequency.enabled",
        ] {
            assert!(ks.iter().any(|x| x == k), "{k}");
        }
    }

    /// 恢复单项：下拉按值找回档位。
    #[test]
    fn set_value_finds_the_select_option() {
        let m = manifest();
        let f = CtFields::default();
        f.seed(&m, &ct_seed(&m, json!({}), json!({})));
        let key = "schema.codetable.frequency.strategy";
        let st = f.items.borrow()[key].clone();
        st.val.set_value(&json!("position"));
        assert_eq!(st.val.value(), json!("position"));
    }
}
