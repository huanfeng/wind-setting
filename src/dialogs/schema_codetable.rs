//! 方案设置 —— 方案对话框「方案设置」页：给单个方案设它自己的码表 / 拼音 / 混输 / 英文行为。
//!
//! # 与方案页引擎卡共用 manifest 条目
//!
//! 每一项的 label、hint、控件类型、数值范围、`enabled_when` 联动都取自
//! `settings_manifest.toml` 里那几个引擎 section 的条目，与方案页的引擎卡是同一份定义。
//! 控件不能直接复用 `manifest::field()`：那条路径把信号注册进主窗口的 `FieldRegistry`，
//! 这里的工作态由本模块自持，随方案对话框的「保存」提交。
//!
//! # 模型：只列改过的项
//!
//! 每项都有一个方案默认值（方案文件写的，没写则是该类方案的内置默认）。绝大多数方案一项
//! 都不改，所以页面只列用户改过的项，其余经「添加设置项」挑出来；移除一行即恢复方案默认。
//!
//! 数据来自 core `schema.getSettings`（键沿用 `schema.<类型>.<项>`）：
//!
//! | 字段 | 回答的问题 |
//! | --- | --- |
//! | `effective` | 现在按什么跑（控件的初值） |
//! | `default` | 恢复默认后变成什么（候选列表里显示它） |
//!
//! 两者不等 ⇔ 这一项是用户改的。保存走 `schema.setSettings`：改过的项写值，移除的项恢复默认。
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
            Self::Codetable => MANIFEST_PREFIX,
            Self::Pinyin { .. } => "schema.pinyin.",
            Self::Mixed => "schema.mix.",
            Self::English => "schema.english.",
        }
    }

    /// 这一页收不收这个键。辅助码在对话框里有自己的页；双拼专有项只给双拼方案。
    pub(crate) fn accepts(self, key: &str) -> bool {
        key.starts_with(self.prefix())
            && !key.starts_with("schema.pinyin.aux_code.")
            && (self == Self::Pinyin { shuangpin: true }
                || !key.starts_with("schema.pinyin.shuangpin."))
    }
}

/// 候选列表里默认值前面的称呼。
const DEFAULT_LABEL: &str = "方案默认";

/// manifest key 的方案级前缀：`schema.codetable.x` ↔ 方案配置的 `engine.codetable.x`。
const MANIFEST_PREFIX: &str = "schema.codetable.";

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

    /// 只读展示文案：未勾选那一行右侧显示的字（跟随值长什么样）。
    ///
    /// 走 manifest 的 label 而不是原始值：下拉显示「全部简码」而不是 `3`，开关显示
    /// 「开」而不是 `true`——用户在这一列读到的东西必须和他勾上之后看到的控件一致。
    fn display(&self, item: &crate::manifest::MItem) -> String {
        match self {
            CtSig::Bool(s) => if s.get() { "开" } else { "关" }.to_string(),
            CtSig::Num(s) => {
                let v = s.get();
                let mut t = if v.fract() == 0.0 {
                    format!("{}", v as i64)
                } else {
                    format!("{v}")
                };
                if let Some(u) = &item.unit {
                    t.push_str(u);
                }
                t
            }
            CtSig::Sel { sig, values, .. } => values
                .get(sig.get())
                .map(|v| {
                    item.options
                        .iter()
                        .find(|o| &o.value == v)
                        .map(|o| o.label.clone())
                        .unwrap_or_else(|| v.clone())
                })
                .unwrap_or_default(),
        }
    }

    /// 把另一个同型信号的值复制进来（「取消覆盖」时把控件值拨回跟随值）。
    /// 形态不同即静默跳过——两个信号由同一个 manifest 项建出，不可能不同。
    fn assign_from(&self, other: &CtSig) {
        match (self, other) {
            (CtSig::Bool(a), CtSig::Bool(b)) => a.set(b.get()),
            (CtSig::Num(a), CtSig::Num(b)) => a.set(b.get()),
            (CtSig::Sel { sig: a, .. }, CtSig::Sel { sig: b, .. }) => a.set(b.get()),
            _ => {}
        }
    }
}

/// 一项的三态工作态：用户值 + 跟随值 + 覆盖勾选 + 来源文案。
#[derive(Clone)]
struct CtItemState {
    /// 用户值：勾选时控件绑它，落盘写它。初值取 `effectiveCodetable`（当前生效值）。
    val: CtSig,
    /// 跟随值：**只读**，恒为 `followedCodetable` 里的值（取消覆盖后会变成的那个）。
    /// 未勾选时右侧显示它的 `display()`，`enabled_when` 求值也取它。
    follow: CtSig,
    /// 这一项是不是用户改的（＝ 生效值与方案默认值不同）。
    over: Signal<bool>,
}

impl CtItemState {
    /// **实际生效值**——勾选时是用户值，未勾选时是跟随值。
    ///
    /// `enabled_when` 必须按它求值：某一项被上游开关 gate 时，判据是「上游那项现在到底
    /// 是开是关」，而不是「上游那项的控件里存着什么」。未勾选的项控件里存的是初值，
    /// 用户拨过又取消勾选的话，那个值已经不作数了。
    fn effective_value(&self) -> Value {
        if self.over.get() {
            self.val.value()
        } else {
            self.follow.value()
        }
    }
}

/// 「添加覆盖」弹框里的一个候选项。
#[derive(Clone, Debug, PartialEq)]
pub struct CtCandidate {
    pub key: String,
    pub label: String,
    /// manifest 里的说明；可能为空。
    pub hint: String,
    /// 当前跟随值的显示文案（添加后控件就从它起步）。
    pub follow: String,
    /// 跟随的来源称呼（「跟随全局」/「跟随基线」）。
    pub follow_label: String,
    /// 它依赖、且**当前没满足**、本身也还没列出的项（`enabled_when` 引用的上游开关），
    /// 形如 `(key, 名称)`。勾选本项时要一并添加，否则新行一出现就是灰的。
    pub deps: Vec<(String, String)>,
}

/// 渲染计划的一项：**一个分类卡片**（标题 + 组内 key 列表）。
///
/// 存 key 而非 Element，因为 `Element::host_signal` 的数据项要求 `Clone`。
///
/// 0.119 之前这里是扁平的 `Header | Field` 两态序列，分类靠「遇到标题就插一行」表达。
/// 加了行首勾选框和来源列之后，那一行标题被行的视觉重量压得看不见了——分类是这一屏的
/// 骨架，改成成组下发，由卡片把它撑住。
#[derive(Clone)]
pub struct CtGroup {
    pub title: String,
    pub keys: Vec<String>,
    /// 是不是第一组：首组小标题上方不画线（卡片顶边已是分隔）。
    pub first: bool,
}

/// `seed` 的入参：`schema.getSettings` 给的两份数据（键为 `schema.<类型>.<项>`）。
#[derive(Debug, Clone, Default)]
pub struct CtSeed {
    pub kind: SchemeKind,
    /// 当前生效值，控件的初值。
    pub effective: serde_json::Map<String, Value>,
    /// 方案默认值，恢复默认后的值。
    pub default: serde_json::Map<String, Value>,
}

/// 两个设置值是否相同；数值按大小比（`48` 与 `48.0` 是同一个值）。
fn same_value(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// 某项在方案级码表配置里归哪一组。
///
/// 全局那张「码表」卡把词频调整、自动造词、英文候选的参数收进了主开关的扩展设置
/// （`opens_dialog` / `dialog_group`），参数本身不再带 `subsection`。这里没有对话框，
/// 按主开关的标签把「主开关 + 它的参数」归成一组，否则参数会掉进首组「上屏行为」。
fn ct_group_of(manifest: &Manifest, item: &MItem) -> Option<String> {
    if item.opens_dialog.is_some() {
        return Some(item.label.clone());
    }
    if let Some(h) = manifest.dialog_host_of(item) {
        return Some(h.label.clone());
    }
    item.subsection.clone()
}

/// 方案级码表配置的工作态：打开方案设置对话框时播种，点「保存」时落盘。
#[derive(Clone, Default)]
pub struct CtFields {
    /// manifest key → 三态工作态。仅含实际渲染出来的项。
    items: Rc<RefCell<HashMap<String, CtItemState>>>,
    /// 渲染顺序（HashMap 无序，行的先后必须按 manifest 原序）。
    order: Rc<RefCell<Vec<String>>>,
    /// key → subsection（算渲染计划时用；避免 plan() 反查 manifest 而多传一个参数）。
    subsecs: Rc<RefCell<HashMap<String, Option<String>>>>,
    /// 分类标题 → 该组的摘要信号（「已自定义 N 项」）。由 `refresh_group_summaries` 推。
    group_disp: Rc<RefCell<HashMap<String, Signal<String>>>>,
    /// 控件位的统一宽度（见 `seed` 里的算法）。
    slot_w: Rc<std::cell::Cell<i32>>,
    /// `seed` 时各信号 version 之和。与当前值不等 = 用户动过某一项。
    ///
    /// 比「逐项存快照再比值」省事，且对「改了又改回去」同样判脏——那种情况下确实
    /// 该写一次盘：用户可能是在撤销上一次保存的结果。
    ///
    /// ⚠️ **必须把勾选框的 version 一起算进来**：只算值信号的话，「取消某项覆盖」
    /// （值没动、只取消了勾）判不出脏，用户点保存等于什么也没发生。
    ver_at_seed: Rc<std::cell::Cell<u64>>,
    /// 本次播种的方案类别。
    kind: Rc<std::cell::Cell<SchemeKind>>,
    /// 该方案全部设置项的键（含界面上没有控件的），「全部恢复默认」按它清。
    all_keys: Rc<RefCell<Vec<String>>>,
    /// 播种时就已改过的项：保存时只有它们被移除才需要发「恢复默认」。
    over_at_seed: Rc<RefCell<Vec<String>>>,
}

impl CtFields {
    /// 按 manifest 建立三态工作态。
    ///
    /// 键名与 manifest key 去掉 `schema.codetable.` 前缀后逐字对应（嵌套段如
    /// `frequency.enabled` 走 JSON pointer）。
    pub fn seed(&self, manifest: &Manifest, seed: &CtSeed) {
        let mut items = HashMap::new();
        let mut order = Vec::new();
        let mut subsecs = HashMap::new();
        self.kind.set(seed.kind);
        let mut over_at_seed = Vec::new();
        for key in visible_keys_of(manifest, seed.kind) {
            let Some(item) = manifest.get(&key) else {
                continue;
            };
            let eff = seed.effective.get(&key);
            // 其余控件类型这一页还不支持；出现即跳过（不渲染也不落盘），
            // 好过渲染一个读写落点不明的控件。
            let Some(val) = make_sig(item, eff) else {
                continue;
            };
            // 默认值取不到时（core 没给该字段）回落到生效值：显示一个陈旧但真实的数，
            // 好过显示类型零值。
            let def = seed.default.get(&key);
            let follow = make_sig(item, def.or(eff)).unwrap_or_else(|| val.clone());
            let overridden = matches!((eff, def), (Some(e), Some(d)) if !same_value(e, d));
            if overridden {
                over_at_seed.push(key.clone());
            }
            subsecs.insert(key.clone(), ct_group_of(manifest, item));
            items.insert(
                key.clone(),
                CtItemState {
                    val,
                    follow,
                    over: signal(overridden),
                },
            );
            order.push(key);
        }
        *self.all_keys.borrow_mut() = seed.default.keys().cloned().collect();
        *self.over_at_seed.borrow_mut() = over_at_seed;
        *self.items.borrow_mut() = items;
        *self.order.borrow_mut() = order;
        *self.subsecs.borrow_mut() = subsecs;
        // ★ 控件位**所有行统一宽度**，取本节各项 manifest `width` 的最大值。
        //
        // 逐行用自己的 `width` 会让列错位：这一节只有「出简让全」写了 `width = 160`
        // （下拉要放得下「一、二级简码」），其余走 150——于是它左边的来源文案整列
        // 被顶偏 10px，看起来就是「这一行和别人不一样宽」。控件自身仍按各自的 width
        // 渲染，只有**容器**统一。
        let slot = visible_keys_of(manifest, seed.kind)
            .iter()
            .filter_map(|k| manifest.get(k).and_then(|i| i.width))
            .fold(CTRL_W, |acc, w| acc.max(w as i32));
        self.slot_w.set(slot);
        // 换方案要重建分类摘要：上一个方案的分类可能与这个方案不同（特殊方案没有调频段），
        // 留着旧的会在卡片上显示一个本方案根本没有的分类。
        self.group_disp.borrow_mut().clear();
        for g in self.plan() {
            let _ = self.group_summary(&g.title);
        }
        self.refresh_group_summaries();
        self.ver_at_seed.set(self.version_sum());
    }

    /// 用户是否动过其中任何一项（相对上次 `seed`）。
    pub fn is_dirty(&self) -> bool {
        self.version_sum() != self.ver_at_seed.get()
    }

    pub fn version_sum(&self) -> u64 {
        self.items.borrow().values().fold(0u64, |acc, st| {
            acc.wrapping_add(st.val.version())
                .wrapping_add(st.over.version())
        })
    }

    /// 已勾选（＝用户自己设了值）的项数。行内摘要用。
    pub fn custom_count(&self) -> usize {
        self.items
            .borrow()
            .values()
            .filter(|st| st.over.get())
            .count()
    }

    /// 全部取消覆盖：勾选清空，控件值拨回跟随值。
    ///
    /// 只动 UI，不碰配置——落盘由 [`Self::reset`] 负责。分开是因为「全部恢复默认」要
    /// 立刻看到效果，而写盘统一走底栏的保存（见 core 仓那条「非 config.toml 落点一律
    /// 登记 SideCommitter，不加独立保存按钮」）。
    pub fn revert_all(&self) {
        for st in self.items.borrow().values() {
            st.over.set(false);
            st.val.assign_from(&st.follow);
        }
    }

    /// 某项归属的分类标题。
    ///
    /// manifest 里有一批项**不带 subsection**（顶码上屏、标点上屏这些），它们是这个 section
    /// 的直属主体项，不是「杂项」——给它们用 section 名（「上屏行为」）作组名，三张卡片
    /// 就都有名字了。留空的话第一张卡片会是个无名框，看起来像渲染坏了。
    ///
    /// ⚠️ `plan` 与 `refresh_group_summaries` **必须都走这里**：两处各写一遍取值，摘要就会
    /// 落在一个没人显示的 key 上（组标题右侧永远显示「全部跟随」，且不报错）。
    fn group_title_of(&self, key: &str) -> String {
        self.subsecs
            .borrow()
            .get(key)
            .cloned()
            .flatten()
            .unwrap_or_else(|| self.root_title())
    }

    /// 没有小标题的项归到的组名：该类方案的首个 section 名。
    fn root_title(&self) -> String {
        let secs = self.kind.get().sections();
        secs[secs.len() - 1].to_string()
    }

    /// 渲染计划：按 subsection 切成若干**分类卡片**（组内保持 manifest 原序）。
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

    /// 各分类标题右侧的摘要信号（「已自定义 N 项」/「全部跟随」）。
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
        {
            let items = self.items.borrow();
            let subsecs = self.subsecs.borrow();
            for (key, st) in items.iter() {
                if !st.over.get() {
                    continue;
                }
                let title = subsecs
                    .get(key)
                    .cloned()
                    .flatten()
                    .unwrap_or_else(|| self.root_title());
                *per_group.entry(title).or_default() += 1;
            }
        }
        // 先收集再写：`group_summary` 要借 `group_disp`，与上面的两个 borrow 不冲突，
        // 但把写操作留在借用作用域外仍是更稳的写法（将来加字段不必重新推一遍借用图）。
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

    /// 渲染一个分类：小标题 + 组内**已列出**的行；整组一行都没列出时连标题一起藏。
    ///
    /// 方案覆盖是**稀疏**的：绝大多数方案一项都不改，所以默认只列「用户覆盖了的」（`over`）和
    /// 「方案作者自己写了的」（来源为「方案自带」，不可移除）；其余项经节头的「添加覆盖」弹框挑出来。
    /// 行**预建、靠 `visible_when` 显隐**——工作态挂在 Element 上，重建会丢刚改的值。
    ///
    /// 「全部恢复默认」在这个 TAB 上是**整节**级别的动作，挂在外层节头（见
    /// `schema_manager.rs` 的 `SettingsSection::CodeTable`），不是某一分类自己的按钮。
    pub fn card(&self, manifest: &Manifest, group: CtGroup) -> Element {
        let listed: Vec<Signal<bool>> = {
            let items = self.items.borrow();
            group
                .keys
                .iter()
                .filter_map(|k| items.get(k).map(|st| st.over))
                .collect()
        };
        let mut col = Element::col()
            .width_match()
            .child(crate::widgets::list_heading_ext(
                &group.title,
                None,
                group.first,
            ));
        for k in group.keys.iter() {
            let Some(over) = self.items.borrow().get(k).map(|st| st.over) else {
                continue;
            };
            col = col.child(
                Element::col()
                    .width_match()
                    .child(crate::widgets::row_divider())
                    .child(crate::widgets::card_pad(self.row(manifest, k)))
                    .visible_when(move || over.get()),
            );
        }
        col.visible_when(move || listed.iter().any(|over| over.get()))
    }

    /// 「添加覆盖」弹框的候选：还没列出的项（用户没覆盖、方案文件也没写），按分类归拢，
    /// 带说明与当前跟随值——只给一个标题挑不出东西。分类与顺序同 [`Self::plan`]。
    pub fn candidates(&self, manifest: &Manifest) -> Vec<(String, Vec<CtCandidate>)> {
        let items = self.items.borrow();
        self.plan()
            .into_iter()
            .filter_map(|g| {
                let list: Vec<CtCandidate> = g
                    .keys
                    .iter()
                    .filter_map(|k| {
                        let st = items.get(k)?;
                        if st.over.get() {
                            return None;
                        }
                        let item = manifest.get(k)?;
                        // 上游条件按**生效值**判：全局本来就满足的（如全局已开调频）不用拉上游。
                        let deps = item
                            .enabled_when
                            .as_deref()
                            .and_then(parse_conds)
                            .unwrap_or_default()
                            .iter()
                            .filter_map(|c| {
                                let pst = items.get(&c.key)?;
                                if pst.over.get() || cond_matches(&pst.effective_value(), c) {
                                    return None;
                                }
                                Some((c.key.clone(), manifest.get(&c.key)?.label.clone()))
                            })
                            .collect();
                        Some(CtCandidate {
                            key: k.clone(),
                            label: item.label.clone(),
                            hint: item.hint.clone().unwrap_or_default(),
                            follow: st.follow.display(item),
                            follow_label: DEFAULT_LABEL.to_string(),
                            deps,
                        })
                    })
                    .collect();
                (!list.is_empty()).then_some((g.title, list))
            })
            .collect()
    }

    /// 添加一项覆盖：控件值已在 `seed` 时按**生效值**播种，所以勾上即「以当前值起步」，行为不变。
    pub fn add(&self, key: &str) {
        if let Some(st) = self.items.borrow().get(key) {
            st.over.set(true);
        }
    }

    /// 当前是否有任何一项列在覆盖页里（用户覆盖的，或方案自带的）。
    pub fn any_listed(&self) -> bool {
        self.items.borrow().values().any(|st| st.over.get())
    }

    /// 组内一行。
    fn row(&self, manifest: &Manifest, key: &str) -> Element {
        let (Some(item), Some(st)) = (manifest.get(key), self.items.borrow().get(key).cloned())
        else {
            return Element::col();
        };
        let over = st.over;
        // 控件按自己的 width 渲染，容器统一——两者分开，见 `seed` 里那段说明。
        let width = item.width.map(|w| w as i32).unwrap_or(CTRL_W);
        let slot = self.slot_w.get().max(width);
        // 联动按**方案级信号**求值，不是全局 FieldRegistry：同名 key 在两处的值可以不同
        // （全局开了调频、这个方案没开），拿全局的判会让整组控件的可用性说谎。
        let gate: Rc<dyn Fn() -> bool> = match self.enabled_when(item.enabled_when.as_deref()) {
            Some(f) => Rc::from(f),
            None => Rc::new(|| true),
        };
        // 原因文案是**静态**的：「需先开启「精确匹配模式」」这句话本身不随取值变化，
        // 变的只是显不显示。故这里算一次，交给 `visible_when` 决定露不露面。
        let reason = self.gate_reason(manifest, item.enabled_when.as_deref());
        let control = crate::widgets::override_control(
            over,
            gate.clone(),
            build_editor(item, &st.val, width),
            &st.follow.display(item),
            &reason,
            slot,
        );
        // 行存在即「已修改」，行尾 ✕ 移除（恢复方案默认）。
        let row = crate::widgets::setting_row_override_removable(
            &item.label,
            item.hint.as_deref(),
            control,
            move || over.set(false),
        );
        // ★ `enabled_when` **只表达真禁用**（上游开关没开），此时勾选框也一并置灰——
        // 上游关着时这一项连覆盖都不该谈。「未勾选」不走这条路：它是正常状态，
        // 置灰会让整屏发灰（见 `widgets::override_control` 的论证）。
        row.enabled_when(move || gate())
    }

    /// 首个不满足的条件对应的原因文案；全满足时为空串。
    ///
    /// 只报**第一条**：条件是 `&&` 串联的，全列出来会长得放不下，而用户按第一条去开
    /// 上游开关之后，这里会自动换成下一条（如果还有）。
    fn gate_reason(&self, manifest: &Manifest, expr: Option<&str>) -> String {
        let Some(conds) = expr.and_then(parse_conds) else {
            return String::new();
        };
        let items = self.items.borrow();
        for c in &conds {
            // 引用了本组没有的 key 时该子句视为满足——与 `enabled_when` 同向：拼写错误
            // 只让联动失效，不该把控件永久锁死，更不该报一句无从下手的原因。
            let Some(st) = items.get(&c.key) else {
                continue;
            };
            if !cond_matches(&st.effective_value(), c) {
                return crate::manifest::cond_reason(manifest, c);
            }
        }
        String::new()
    }

    /// 由 `enabled_when` 表达式构造联动闭包（取值来自本组信号的**生效值**）。
    /// 引用了本组没有的 key 时该子句视为满足——与全局页同向：拼写错误只让联动失效，
    /// 不该把控件永久锁死。
    fn enabled_when(&self, expr: Option<&str>) -> Option<Box<dyn Fn() -> bool>> {
        let conds = parse_conds(expr?)?;
        let items = self.items.clone();
        Some(Box::new(move || {
            let m = items.borrow();
            conds.iter().all(|c| {
                m.get(&c.key)
                    .map(|st| cond_matches(&st.effective_value(), c))
                    .unwrap_or(true)
            })
        }))
    }

    /// 把工作态写进方案配置（`cfg` 是 `schema.getConfig` 拿到的合并结果，回传 saveConfig）。
    ///
    /// 逐项三态：勾选的写值，没勾的写 `null`。
    ///
    /// `null` 就是「取消覆盖」的落地方式——core 的 `json_to_toml` 跳过 null，于是 override
    /// 里不含这个键；而 `saveConfig` 是拿**方案文件基线**（不含 override）与 cfg 做 diff，
    /// 所以取消覆盖之后该项回到方案作者的值，作者没写就回到全局。这一整条链是现成的，
    /// 三态模型没给 core 添任何东西。
    ///
    /// ⚠️ 隐藏项（manifest `hidden = true`；本 section 现存的是 `auto_phrase.*`）没有信号，
    /// 本函数**不动它**：既不写实也不清除，原样透传。设置页不暴露的项，设置页也不该替
    /// 用户决定它的死活；要清得走「全部恢复默认」（[`Self::reset`]）。
    /// 要提交的改动：`(写入的项, 恢复默认的项)`，键为 `schema.<类型>.<项>`，原样交给
    /// `schema.setSettings`。
    ///
    /// `reset_all`：用户按过「全部恢复默认」。`hidden(key)`：该项是否被隐藏。
    pub fn changes(
        &self,
        reset_all: bool,
        hidden: &dyn Fn(&str) -> bool,
    ) -> (Vec<(String, Value)>, Vec<String>) {
        let items = self.items.borrow();
        let mut sets: Vec<(String, Value)> = Vec::new();
        for key in self.order.borrow().iter() {
            if let Some(st) = items.get(key).filter(|st| st.over.get()) {
                sets.push((key.clone(), st.val.value()));
            }
        }
        let is_set = |k: &str| sets.iter().any(|(s, _)| s == k);
        let mut resets: Vec<String> = if reset_all {
            // 全集：含界面上此刻没有控件的项。被定制版或用户精简清单藏掉的项原样保留——
            // 藏起来就是不让人在界面上动它，一键清空不该把它的值悄悄抹掉。
            self.all_keys
                .borrow()
                .iter()
                .filter(|k| !is_set(k) && !hidden(k))
                .cloned()
                .collect()
        } else {
            self.over_at_seed
                .borrow()
                .iter()
                .filter(|k| !is_set(k))
                .cloned()
                .collect()
        };
        resets.sort();
        (sets, resets)
    }
}

/// 按 manifest 项建一个值信号，初值取 `cur`（取不到用类型零值）。
///
/// 用户值与跟随值各建一个，两者形态必须完全一致（`assign_from` 靠这个），故只此一处构造。
fn make_sig(item: &crate::manifest::MItem, cur: Option<&Value>) -> Option<CtSig> {
    Some(match item.typ.as_str() {
        "toggle" => CtSig::Bool(signal(cur.and_then(|v| v.as_bool()).unwrap_or(false))),
        "number" => CtSig::Num(signal(cur.and_then(|v| v.as_f64()).unwrap_or(0.0))),
        "select" | "number_select" => {
            let numeric = item.typ == "number_select";
            let values: Vec<String> = item.options.iter().map(|o| o.value.clone()).collect();
            // 生效值的 JSON 形态随类型不同（select 是字符串、number_select 是数字），
            // 但 options 的 value 一律是字符串——统一转成字符串再比，避免两边形态错位
            // 导致恒定落到 idx 0（那会让「当前档位」在方案对话框里永远显示成第一项）。
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

/// 可编辑控件（勾选时显示的那个）。
fn build_editor(item: &crate::manifest::MItem, sig: &CtSig, width: i32) -> Element {
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

/// 四类方案这一页可能出现的全部键（添加弹框的勾选状态按它预建）。
pub(crate) fn all_visible_keys(manifest: &Manifest) -> Vec<String> {
    let mut out = Vec::new();
    for kind in [
        SchemeKind::Codetable,
        SchemeKind::Pinyin { shuangpin: true },
        SchemeKind::Mixed,
        SchemeKind::English,
    ] {
        out.extend(visible_keys_of(manifest, kind));
    }
    out
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

    fn visible_keys(manifest: &Manifest) -> Vec<String> {
        visible_keys_of(manifest, SchemeKind::Codetable)
    }

    fn manifest() -> Manifest {
        Manifest::from_embedded()
    }

    /// 把 `{"a": {"b": 1}}` 拍平成 `{"schema.codetable.a.b": 1}`。
    fn flat(tree: &Value) -> serde_json::Map<String, Value> {
        fn walk(prefix: &str, v: &Value, out: &mut serde_json::Map<String, Value>) {
            match v {
                Value::Object(o) => {
                    for (k, c) in o {
                        walk(&format!("{prefix}.{k}"), c, out);
                    }
                }
                _ => {
                    out.insert(prefix.to_string(), v.clone());
                }
            }
        }
        let mut out = serde_json::Map::new();
        walk("schema.codetable", tree, &mut out);
        out
    }

    /// 组装码表方案的 `CtSeed`：`effective` 是当前生效值，`default` 是方案默认值，
    /// 都按 `schema.codetable` 之下的树写。
    fn seed_of(effective: &Value, default: &Value) -> CtSeed {
        CtSeed {
            kind: SchemeKind::Codetable,
            effective: flat(effective),
            default: flat(default),
        }
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
        f.seed(&m, &seed_of(&eff, &eff));
        let items = f.items.borrow();
        let b = |k: &str| match items.get(k).map(|st| st.val.clone()) {
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

        match items
            .get("schema.codetable.frequency.strategy")
            .map(|st| st.val.clone())
        {
            Some(CtSig::Sel { sig, values, .. }) => {
                assert_eq!(values[sig.get()], "position", "下拉初值应落在生效项上");
            }
            _ => panic!("strategy 应是 select"),
        }
        match items
            .get("schema.codetable.frequency.protect_top_n_len1")
            .map(|st| st.val.clone())
        {
            Some(CtSig::Num(s)) => assert_eq!(s.get(), 7.0),
            _ => panic!("protect_top_n_len1 应是 number"),
        }
    }

    /// 数值下拉（`number_select`）的初值按**数字**匹配，写回也是数字。
    ///
    /// 两侧都会静默出错，故必须一并断言：
    /// - 初值：生效值是 JSON 数字而 options 的 value 是字符串，比对时不转形态就恒落 idx 0
    ///   ——界面显示「关闭」，用户以为自己没开过；
    /// - 写回：`Sel` 原本恒写 `Value::String`，落进 TOML 是 `"3"`，core 的
    ///   `Option<usize>` 反序列化直接失败。UI 上两者都看不出来。
    #[test]
    fn number_select_round_trips_as_an_integer() {
        let m = manifest();
        let eff = json!({ "short_code_yield_level": 2 });
        let f = CtFields::default();
        f.seed(&m, &seed_of(&eff, &eff));
        let key = "schema.codetable.short_code_yield_level";
        let sig = f.items.borrow()[key].val.clone();
        match &sig {
            CtSig::Sel {
                sig: idx, values, ..
            } => {
                assert_eq!(values[idx.get()], "2", "数值下拉初值应落在生效档位上");
            }
            _ => panic!("{key} 应是 number_select"),
        }
        assert_eq!(sig.value(), json!(2), "写回必须是整数，不是字符串 \"2\"");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 分类
    // ─────────────────────────────────────────────────────────────────────────

    /// 渲染计划必须按 subsection 切成若干**有标题的组**。
    ///
    /// 这条是 0.119 那次翻车的守门：分类在数据层一直都在（`plan` 照常分组），但界面上
    /// 被行首勾选框和来源列的视觉重量压得看不见了。改成卡片之后，组的**结构**成了硬要求
    /// ——`plan` 若退回扁平序列，卡片就无从画起。
    /// 添加依赖上游开关的项（调频策略依赖「调频」总开关）时，候选里要标出没满足的上游；
    /// 上游已满足（全局本来就开着）则不拉。
    #[test]
    fn candidates_carry_unmet_dependencies() {
        let m = manifest();
        let off = json!({ "frequency": { "enabled": false, "strategy": "position" } });
        let f = CtFields::default();
        f.seed(&m, &seed_of(&off, &off));
        let all: Vec<CtCandidate> = f.candidates(&m).into_iter().flat_map(|(_, v)| v).collect();
        let strategy = all
            .iter()
            .find(|c| c.key == "schema.codetable.frequency.strategy")
            .expect("调频策略应在候选里");
        assert_eq!(
            strategy
                .deps
                .iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>(),
            vec!["schema.codetable.frequency.enabled"],
            "调频关着：策略依赖总开关"
        );

        let on = json!({ "frequency": { "enabled": true, "strategy": "position" } });
        let f = CtFields::default();
        f.seed(&m, &seed_of(&on, &on));
        let all: Vec<CtCandidate> = f.candidates(&m).into_iter().flat_map(|(_, v)| v).collect();
        let strategy = all
            .iter()
            .find(|c| c.key == "schema.codetable.frequency.strategy")
            .unwrap();
        assert!(strategy.deps.is_empty(), "全局已开调频：不需要拉上游");
    }

    #[test]
    fn plan_splits_into_titled_groups_by_subsection() {
        let m = manifest();
        let e = json!({});
        let f = CtFields::default();
        f.seed(&m, &seed_of(&e, &e));
        let groups = f.plan();
        assert!(
            groups.len() >= 2,
            "上屏行为至少有「常用功能」「词频调整」两组，实际 {} 组",
            groups.len()
        );
        assert!(
            groups.iter().all(|g| !g.title.is_empty()),
            "每组都该有标题，实际 {:?}",
            groups.iter().map(|g| &g.title).collect::<Vec<_>>()
        );
        assert!(
            groups.iter().all(|g| !g.keys.is_empty()),
            "空组说明分组逻辑漏了 key"
        );
        // 组内的 key 必须都属于该组——切错组比不分组更难查。
        // 无分组的项归到以 section 名命名的那一组（见 `group_title_of`）。
        for g in &groups {
            for k in &g.keys {
                assert_eq!(
                    ct_group_of(&m, m.get(k).unwrap()).unwrap_or_else(|| CT_SECTION.to_string()),
                    g.title,
                    "{k} 被切进了「{}」组",
                    g.title
                );
            }
        }
        // 全局卡把这几组参数收进了扩展设置、去掉了 subsection；这里仍要按主开关成组，
        // 参数不能掉进首组。
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
            if visible_keys(&m).iter().any(|x| x == k) {
                assert_eq!(group_of_key(k).as_deref(), Some(want), "{k}");
            }
        }
        // 第一组是 section 直属项（顶码上屏那些）：它们在 manifest 里排在所有 subsection
        // 之前，卡片顺序跟着 manifest 走，不该被排到子分类后面。
        assert_eq!(
            groups[0].title, CT_SECTION,
            "无 subsection 的直属项应排在最前，实际首组是「{}」",
            groups[0].title
        );
        // 覆盖面：分组不能吞 key。
        let total: usize = groups.iter().map(|g| g.keys.len()).sum();
        assert_eq!(
            total,
            visible_keys(&m).len(),
            "分组前后的项数必须相等，否则有项在界面上直接消失"
        );
    }

    /// 分类标题右侧的摘要只数**本组**改过的项。
    ///
    /// 串组的话，用户改了调频里的一项，「常用功能」也跟着显示「已自定义 1 项」——
    /// 那句摘要的用处正是让人一眼看出改动落在哪一组，串了就完全没用了。
    #[test]
    fn group_summary_counts_only_its_own_group() {
        let m = manifest();
        let e = json!({});
        let f = CtFields::default();
        f.seed(&m, &seed_of(&e, &e));
        let groups = f.plan();
        let (g0, g1) = (&groups[0], &groups[1]);
        assert_eq!(f.group_summary(&g0.title).get(), "全部默认");
        assert_eq!(f.group_summary(&g1.title).get(), "全部默认");

        // 勾第二组里的一项。
        f.items.borrow()[&g1.keys[0]].over.set(true);
        f.refresh_group_summaries();
        assert_eq!(
            f.group_summary(&g0.title).get(),
            "全部默认",
            "另一组不该被算进来"
        );
        assert_eq!(f.group_summary(&g1.title).get(), "已修改 1 项");
    }

    /// 换方案要重建分类摘要：两个方案的分类可能不同（特殊方案没有的段），
    /// 留着旧信号会在卡片上显示一个本方案根本没有的分类。
    #[test]
    fn reseed_resets_group_summaries() {
        let m = manifest();
        let e = json!({});
        let f = CtFields::default();
        f.seed(&m, &seed_of(&e, &e));
        let title = f.plan()[0].title.clone();
        f.items.borrow()[&f.plan()[0].keys[0]].over.set(true);
        f.refresh_group_summaries();
        assert_eq!(f.group_summary(&title).get(), "已修改 1 项");

        f.seed(&m, &seed_of(&e, &e));
        assert_eq!(
            f.group_summary(&title).get(),
            "全部默认",
            "换方案后摘要必须跟着重置"
        );
    }

    /// 上游开关没开时，给的是**一句能照着做的原因**，而不是一个灰控件。
    ///
    /// 「精确匹配空码补全」依赖「精确匹配模式」。灰着不说为什么，用户只能挨个试上游开关。
    ///
    /// ⚠️ 这条同时钉住「原因是静态的」：文案只依赖表达式与 manifest 的 label，不随取值变化
    /// ——变的只是显不显示。`row()` 因此只算一次，不能改成每帧重算。
    #[test]
    fn gate_reason_names_the_upstream_switch() {
        let m = manifest();
        let e = json!({ "single_code_input": false });
        let f = CtFields::default();
        f.seed(&m, &seed_of(&e, &e));
        let expr = m
            .get("schema.codetable.single_code_complete")
            .and_then(|i| i.enabled_when.clone())
            .expect("空码补全应有 enabled_when");
        let reason = f.gate_reason(&m, Some(&expr));
        assert!(
            reason.contains("精确匹配模式"),
            "原因里要点名上游那一项的**界面名字**（不是 key），实际: {reason}"
        );
        assert!(
            reason.starts_with("需先开启"),
            "布尔上游要说「需先开启」，实际: {reason}"
        );

        // 上游满足时不给原因——有原因就意味着控件位被它占着。
        f.items.borrow()["schema.codetable.single_code_input"]
            .over
            .set(true);
        if let CtSig::Bool(s) = &f.items.borrow()["schema.codetable.single_code_input"].val {
            s.set(true);
        }
        assert!(
            f.gate_reason(&m, Some(&expr)).is_empty(),
            "上游已满足时不该再报原因"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 逐项三态
    // ─────────────────────────────────────────────────────────────────────────

    /// `revert_all` 把控件值拨回**跟随值**，而不是留着用户刚改的那个。
    ///
    /// 界面上未勾选的行显示的就是跟随值；不拨回的话，用户按下「全部恢复默认」看到的
    /// 还是自己改过的数，而落盘落的是取消覆盖——界面与结果不一致。
    #[test]
    fn revert_all_pulls_values_back_to_the_followed_ones() {
        let m = manifest();
        // 生效值 2（用户改过），跟随值 3（方案作者写的）。
        let eff = json!({ "short_code_yield_level": 2 });
        let followed = json!({ "short_code_yield_level": 3 });
        let f = CtFields::default();
        f.seed(&m, &seed_of(&eff, &followed));
        let key = "schema.codetable.short_code_yield_level";
        assert!(f.items.borrow()[key].over.get(), "起点：这一项是勾着的");
        assert_eq!(f.custom_count(), 1);

        f.revert_all();
        assert!(!f.items.borrow()[key].over.get(), "勾选应被清空");
        assert_eq!(f.custom_count(), 0);
        assert_eq!(
            f.items.borrow()[key].val.value(),
            json!(3),
            "控件值应拨回跟随值（方案作者的 3），而不是停在用户改的 2"
        );
    }

    /// 未勾选那一行右侧显示的是**跟随值的文案**，且走 manifest 的 label。
    ///
    /// 显示原始值（`3`）用户看不懂，显示生效值（`2`）则是在说谎——那一行现在跟随的是 3。
    #[test]
    fn follow_text_shows_the_followed_value_with_its_label() {
        let m = manifest();
        let eff = json!({ "short_code_yield_level": 2, "punct_commit": false });
        let followed = json!({ "short_code_yield_level": 3, "punct_commit": true });
        let f = CtFields::default();
        f.seed(&m, &seed_of(&eff, &followed));
        let items = f.items.borrow();
        let key = "schema.codetable.short_code_yield_level";
        let item = m.get(key).unwrap();
        assert_eq!(
            items[key].follow.display(item),
            "全部简码",
            "下拉要显示 label 而不是数字 3"
        );
        let pk = "schema.codetable.punct_commit";
        assert_eq!(
            items[pk].follow.display(m.get(pk).unwrap()),
            "开",
            "开关显示开/关；且取的是跟随值（true），不是生效值（false）"
        );
    }

    /// 跟随值缺失时回落到生效值，而不是类型零值。
    ///
    /// core 少给一个字段时（版本错配、新加的项还没进 `followedCodetable`），显示一个陈旧
    /// 但真实的数，好过显示 `false`——后者会让「跟随全局」那一列出现界面上从未有过的
    /// 「关」，用户据此以为全局是关的。
    #[test]
    fn missing_followed_value_falls_back_to_effective_not_to_zero() {
        let m = manifest();
        let eff = json!({ "punct_commit": true });
        let f = CtFields::default();
        f.seed(&m, &seed_of(&eff, &json!({})));
        let items = f.items.borrow();
        assert_eq!(
            items["schema.codetable.punct_commit"].follow.value(),
            json!(true),
            "跟随值缺失应回落生效值"
        );
    }

    /// 脏判定决定「打开对话框看一眼再关掉」会不会写盘。
    ///
    /// 三个方向都要守，最后一个是三态引入的新形态：
    /// - 漏判则改动丢失；
    /// - 误判则每次打开关闭都写一次 override，且整个设置界面会一直显示「有未保存改动」；
    /// - **只取消勾选、值没动**也必须判脏——那是「取消覆盖」这个动作本身，`version_sum`
    ///   若只算值信号就会漏掉它，用户点保存等于什么也没发生。
    #[test]
    fn is_dirty_covers_checkbox_changes_and_resets_on_reseed() {
        let m = manifest();
        let f = CtFields::default();
        let eff = json!({ "punct_commit": true });
        f.seed(&m, &seed_of(&eff, &eff));
        assert!(!f.is_dirty(), "刚播种完不该判脏——用户还没动过任何一项");

        match f
            .items
            .borrow()
            .get("schema.codetable.punct_commit")
            .map(|st| st.val.clone())
        {
            Some(CtSig::Bool(s)) => s.set(false),
            _ => panic!("punct_commit 应是 bool 项"),
        }
        assert!(f.is_dirty(), "拨动任一项即为脏");

        // 换方案（重新播种）：基准跟着重置，否则上一个方案的脏状态会赖到这个方案头上。
        f.seed(&m, &seed_of(&eff, &eff));
        assert!(!f.is_dirty(), "换方案重新播种后应回到干净");

        // 只动勾选、不动值 —— 三态引入的新脏源。
        f.items.borrow()["schema.codetable.punct_commit"]
            .over
            .set(true);
        assert!(f.is_dirty(), "勾选状态变了也是脏，值没动也算");
    }

    /// 联动取值必须来自本组信号的**生效值**（勾选时用户值、未勾选跟随值）。
    ///
    /// 构造一个「拿全局判就会说谎」的场景：本组 `frequency.enabled` 为 false 时，
    /// 策略行必须判为禁用。
    ///
    /// 第二段是三态特有的：某一项没勾时，它的控件里存的是初值，而**实际生效的是跟随值**。
    /// 联动若读控件值，就会出现「上游实际是开的，下游却被置灰」。
    #[test]
    fn enabled_when_reads_scheme_scoped_effective_values() {
        let m = manifest();
        let f = CtFields::default();
        let eff = json!({ "frequency": { "enabled": false, "strategy": "position" } });
        f.seed(&m, &seed_of(&eff, &eff));
        let clo = f
            .enabled_when(Some("schema.codetable.frequency.enabled == true"))
            .expect("表达式应能解析");
        assert!(!clo(), "本组调频关着，联动项应判为禁用");

        // 勾上并翻转本组信号后应立即改判（闭包读的是信号当前值，不是快照）。
        {
            let items = f.items.borrow();
            let st = &items["schema.codetable.frequency.enabled"];
            st.over.set(true);
            if let CtSig::Bool(s) = &st.val {
                s.set(true);
            }
        }
        assert!(clo(), "勾选并打开后应判为启用");

        // 取消勾选 ⇒ 生效的是跟随值。这里跟随值是 false，故应重新判为禁用——
        // 尽管控件里存着的仍是刚才拨上去的 true。
        f.items.borrow()["schema.codetable.frequency.enabled"]
            .over
            .set(false);
        assert!(
            !clo(),
            "未勾选时联动必须按跟随值判，而不是控件里那个已经不作数的值"
        );
    }

    /// 「改过」的判据是生效值与方案默认值不同；数值按大小比。
    #[test]
    fn modified_state_is_effective_differing_from_default() {
        let m = manifest();
        let eff = json!({ "top_code_commit": true, "punct_commit": true,
                          "frequency": { "half_life": 48, "protect_top_n": 3 } });
        let def = json!({ "top_code_commit": false, "punct_commit": true,
                          "frequency": { "half_life": 48.0, "protect_top_n": 0 } });
        let f = CtFields::default();
        f.seed(&m, &seed_of(&eff, &def));
        let over = |k: &str| f.items.borrow()[k].over.get();
        assert!(over("schema.codetable.top_code_commit"));
        assert!(!over("schema.codetable.punct_commit"));
        assert!(over("schema.codetable.frequency.protect_top_n"));
        assert!(
            !over("schema.codetable.frequency.half_life"),
            "48 与 48.0 是同一个值"
        );
        assert_eq!(f.custom_count(), 2);
        assert!(f.any_listed());
    }

    /// 提交的改动：列着的项写值；打开时改过、现在被移除的项恢复默认；没动过的项不出现。
    #[test]
    fn changes_set_listed_items_and_reset_removed_ones() {
        let m = manifest();
        let eff = json!({ "top_code_commit": true, "punct_commit": true, "wildcard": false });
        let def = json!({ "top_code_commit": false, "punct_commit": false, "wildcard": false });
        let f = CtFields::default();
        f.seed(&m, &seed_of(&eff, &def));
        // 移除一项、新添一项。
        f.items.borrow()["schema.codetable.punct_commit"]
            .over
            .set(false);
        f.add("schema.codetable.wildcard");
        assert!(f.is_dirty());
        let (sets, resets) = f.changes(false, &|_| false);
        assert_eq!(
            sets,
            vec![
                ("schema.codetable.top_code_commit".to_string(), json!(true)),
                ("schema.codetable.wildcard".to_string(), json!(false)),
            ]
        );
        assert_eq!(resets, ["schema.codetable.punct_commit"]);
    }

    /// 「全部恢复默认」清的是全集（含界面上没有控件的项），但放过被隐藏的项和仍列着的项。
    #[test]
    fn reset_all_covers_every_key_except_hidden_and_still_listed() {
        let m = manifest();
        let eff = json!({ "top_code_commit": true, "punct_commit": true,
                          "no_ui_for_this": 1, "hidden_one": 2 });
        let def = json!({ "top_code_commit": false, "punct_commit": false,
                          "no_ui_for_this": 0, "hidden_one": 0 });
        let f = CtFields::default();
        f.seed(&m, &seed_of(&eff, &def));
        f.revert_all();
        // 全部恢复之后又把一项加了回来。
        f.add("schema.codetable.top_code_commit");
        let (sets, resets) = f.changes(true, &|k| k == "schema.codetable.hidden_one");
        assert_eq!(sets.len(), 1);
        assert_eq!(
            resets,
            [
                "schema.codetable.no_ui_for_this",
                "schema.codetable.punct_commit"
            ]
        );
    }

    /// 各类方案这一页收哪些项：只收本类的键；辅助码另有一页；双拼专有项只给双拼方案。
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
        assert!(
            !full
                .iter()
                .any(|k| k.starts_with("schema.pinyin.aux_code."))
        );
        assert!(
            !full
                .iter()
                .any(|k| k.starts_with("schema.pinyin.shuangpin."))
        );
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
        assert_eq!(
            SchemeKind::of("pinyin", "full"),
            Some(SchemeKind::Pinyin { shuangpin: false })
        );
        assert_eq!(SchemeKind::of("something", ""), None);
    }

    /// 拼音方案也能播种并产出改动（键前缀随类别走）。
    #[test]
    fn pinyin_scheme_seeds_and_reports_changes() {
        let m = manifest();
        let map = |v: Value| v.as_object().unwrap().clone();
        let f = CtFields::default();
        f.seed(
            &m,
            &CtSeed {
                kind: SchemeKind::Pinyin { shuangpin: false },
                effective: map(json!({ "schema.pinyin.single_char": true })),
                default: map(json!({ "schema.pinyin.single_char": false })),
            },
        );
        assert_eq!(f.custom_count(), 1);
        f.revert_all();
        let (sets, resets) = f.changes(false, &|_| false);
        assert!(sets.is_empty());
        assert_eq!(resets, ["schema.pinyin.single_char"]);
    }
}
