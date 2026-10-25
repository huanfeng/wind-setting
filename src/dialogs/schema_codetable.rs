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
//! # 覆盖模型：逐项三态
//!
//! core 侧 `CodeTableSpec` 的字段是逐个 `Option`（没写 = 跟随基线），界面与它**同构**：
//! 每一行左侧一个勾选框，勾上才写这一项，没勾就是 `null`（跟随）。
//!
//! 0.119 之前是「一个总开关 + 开了就整段写实」。那个模型有三个毛病，逐项三态一次治掉：
//!
//! 1. **整段冻结**——想改一项，代价是整个方案从此不跟随全局；
//! 2. **界面说谎**——方案文件里作者写死的值也算进「已自定义」判据，于是任何声明过
//!    `[engine.codetable]` 字段的方案一打开就显示为已自定义；而用户把总开关关掉时，
//!    清掉的只是 override，作者那行清不掉，界面写着「跟随全局」实际仍在生效；
//! 3. **来源不可见**——用户看不出某项是跟随全局、方案作者定的、还是自己改的。
//!
//! # 未勾选时显示的是「跟随值」，不是当前值
//!
//! 关键区分：**已覆盖的项，「当前生效值」与「取消覆盖后的值」是两个不同的数**。
//! 界面在勾选时显示前者（可编辑控件），未勾选时显示后者（只读文本）。两份都由 core 给：
//!
//! | 字段 | 回答的问题 |
//! | --- | --- |
//! | `effectiveCodetable` | 现在按什么跑（勾选时控件的初值） |
//! | `followedCodetable` | 取消覆盖后变成什么（未勾选时显示的文本） |
//! | `codetableOverride` | 这一项是不是用户改的（勾选状态） |
//!
//! 为什么不让 UI 自己算跟随值：基线分普通/特殊两种（core `EngineManager::codetable_baseline`），
//! UI 侧算不出来——这正是 `effectiveCodetable` 当初存在的理由，对跟随值同样成立。
//!
//! # 「方案自带」怎么判
//!
//! `codetableOverride` 里有 ⇒ 用户改的（勾选）。没有、但合并值（`engine.codetable`）里
//! 有 ⇒ 方案作者写的。两处都没有 ⇒ 跟随全局/基线。判据只能这么绕一道：合并值是
//! 「方案文件 ⊕ override」，单看它答不了「这个 `Some(3)` 是谁写的」。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

use crate::manifest::{MItem, Manifest, cond_matches, parse_conds};

/// manifest 中承载码表配置的 section 名（与全局页的「设置」按钮取同一组）。
pub const CT_SECTION: &str = "上屏行为";

/// 「这一项是方案作者写的」的来源文案。
///
/// 提成常量是因为它有**两个消费点**：`seed` 里写进 `CtItemState::src`，`row` 里据它选
/// 琥珀色。写成两处字面量的话，改一处就会让着色静默失效——文案照旧、颜色变回灰。
const SRC_FROM_SCHEMA: &str = "方案自带";

/// manifest key 的方案级前缀：`schema.codetable.x` ↔ 方案配置的 `engine.codetable.x`。
const MANIFEST_PREFIX: &str = "schema.codetable.";

/// `CodeTableSpec` 里可被方案覆盖的**行为字段**全集（core `wind-config/src/schema.rs`）。
///
/// 用途只剩一个：「全部恢复跟随」（[`CtFields::reset`]）时逐个置 null 清除 override。
/// 日常保存走逐项三态，只碰有控件的那些项。
///
/// 必须是全集而**不能**从 manifest 的可见项推导：override 里可以有界面上当下看不到的字段
/// （0.118 的「开总开关整段写实」把这张表的每一项都写进去过），而**可见集本身会变**——
/// `auto_commit_min_len` 到 0.122 才从 `hidden = true` 转明，同 section 的 `auto_phrase.*`
/// 至今不渲染。按可见项清除的话，这些字段会留下继续生效，用户却在界面上找不到它们。
///
/// 不含 `max_code_length` / `base_sort` / `input_chars`：那些是方案自身的定义（码表怎么
/// 编的），不是用户偏好，不归覆盖这一层管。
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
    // 出简让全的档位。按方案覆盖的理由是「短码首选 = 简码」这个前提只对五笔这类
    // 前缀式简码成立，别的码表可以在自己的方案里关掉。
    "short_code_yield_level",
    // 单字输入。与出简让全同一条论证：一台机器上常有多张码表、简码体系深浅不同，
    // 「只出单字」该由装了这张码表的人自己定。
    // ⚠️ 拼音那一份（schema.pinyin.single_char）**没有**方案级覆盖，不在本清单里也不该加
    // ——本清单只描述 `CodeTableSpec` 的字段。
    "single_char",
    // 通配输入（万能键）。按方案覆盖：26 码元方案与五笔的合适通配键不同，
    // 开不开也取决于这张码表的编码结构。
    "wildcard",
    "wildcard_key",
    // 仅单字按方案覆盖，理由同通配两项。
    "wildcard_single_only",
];

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
    /// 这一项是不是用户改的（＝ override 层里有没有它）。
    over: Signal<bool>,
    /// 未勾选时的来源文案：「方案自带」或调用方给的「跟随全局」/「跟随基线」。
    src: String,
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

/// `seed` 的入参：`schema.getConfig` 给的四份数据 + 基线称呼。
///
/// 打包成结构体而不是五个位置参数——它们全是 `&Value`，顺序传错编译器一声不吭，
/// 而后果是「跟随值当成生效值显示」这类只有真机才看得出来的错。
pub struct CtSeed<'a> {
    /// `effectiveCodetable`：当前生效值，勾选时控件的初值。
    pub effective: &'a Value,
    /// `followedCodetable`：取消覆盖后的值，未勾选时显示它。
    pub followed: &'a Value,
    /// `codetableOverride`：override 层原文，判勾选状态。
    pub overridden: &'a Value,
    /// `engine.codetable`：方案文件 ⊕ override 的合并值，与上一份配合判「方案自带」。
    pub merged: &'a Value,
    /// 未勾选且非方案自带时的文案：普通方案「跟随全局」，特殊方案「跟随基线」。
    pub follow_label: &'a str,
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
        for key in visible_keys(manifest) {
            let Some(item) = manifest.get(&key) else {
                continue;
            };
            let ptr = effective_pointer(&key);
            // 其余控件类型方案级用不到；出现即跳过（不渲染也不落盘），
            // 好过渲染一个读写落点不明的控件。
            let Some(val) = make_sig(item, seed.effective.pointer(&ptr)) else {
                continue;
            };
            // 跟随值取不到时（core 没给该字段）回落到生效值：显示一个陈旧但真实的数，
            // 好过显示类型零值——后者会让「跟随全局」那一列出现界面上从未有过的「关」，
            // 用户据此以为全局是关的。
            let follow = make_sig(
                item,
                seed.followed.pointer(&ptr).or(seed.effective.pointer(&ptr)),
            )
            .unwrap_or_else(|| val.clone());
            let overridden = seed.overridden.pointer(&ptr).is_some_and(|v| !v.is_null());
            // 「方案自带」＝ 合并值里有、override 里没有。合并值是「方案文件 ⊕ override」，
            // 单看它答不了「谁写的」，必须减去 override 这一层。
            let src = if !overridden && seed.merged.pointer(&ptr).is_some_and(|v| !v.is_null()) {
                SRC_FROM_SCHEMA.to_string()
            } else {
                seed.follow_label.to_string()
            };
            subsecs.insert(key.clone(), ct_group_of(manifest, item));
            items.insert(
                key.clone(),
                CtItemState {
                    val,
                    follow,
                    over: signal(overridden),
                    src,
                },
            );
            order.push(key);
        }
        *self.items.borrow_mut() = items;
        *self.order.borrow_mut() = order;
        *self.subsecs.borrow_mut() = subsecs;
        // ★ 控件位**所有行统一宽度**，取本节各项 manifest `width` 的最大值。
        //
        // 逐行用自己的 `width` 会让列错位：这一节只有「出简让全」写了 `width = 160`
        // （下拉要放得下「一、二级简码」），其余走 150——于是它左边的来源文案整列
        // 被顶偏 10px，看起来就是「这一行和别人不一样宽」。控件自身仍按各自的 width
        // 渲染，只有**容器**统一。
        let slot = visible_keys(manifest)
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
    /// 只动 UI，不碰配置——落盘由 [`Self::reset`] 负责。分开是因为「全部恢复跟随」要
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
            .unwrap_or_else(|| CT_SECTION.to_string())
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
                    .unwrap_or_else(|| CT_SECTION.to_string());
                *per_group.entry(title).or_default() += 1;
            }
        }
        // 先收集再写：`group_summary` 要借 `group_disp`，与上面的两个 borrow 不冲突，
        // 但把写操作留在借用作用域外仍是更稳的写法（将来加字段不必重新推一遍借用图）。
        let titles: Vec<String> = self.group_disp.borrow().keys().cloned().collect();
        for t in titles {
            let n = per_group.get(&t).copied().unwrap_or(0);
            let text = if n == 0 {
                "全部跟随".to_string()
            } else {
                format!("已自定义 {n} 项")
            };
            self.group_summary(&t).set(text);
        }
    }

    /// 渲染一个分类：小标题（右侧报本组摘要）+ 组内各行。各分类同在「码表配置」那
    /// **一张**卡片里（[`crate::widgets::CardItem::Bare`]），小标题与分割线跟随「界面细节」。
    ///
    /// 「全部恢复跟随」在这个 TAB 上是**整节**级别的动作，挂在外层节头（见
    /// `schema_manager.rs` 的 `SettingsSection::CodeTable`），不是某一分类自己的按钮。
    pub fn card(&self, manifest: &Manifest, group: CtGroup) -> Element {
        let summary = self.group_summary(&group.title);
        let mut col = Element::col()
            .width_match()
            .child(crate::widgets::list_heading_ext(
                &group.title,
                Some(&move || crate::widgets::summary_label(summary)),
                group.first,
            ));
        for (i, k) in group.keys.iter().enumerate() {
            if i > 0 {
                col = col.child(crate::widgets::row_divider());
            }
            col = col.child(crate::widgets::card_pad(self.row(manifest, k)));
        }
        col
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
        // 「方案自带」用满对比度正文色挑出来（见 `setting_row_override` 的三态三色表）——
        // 它是这一列里唯一「与全局不同、却不是你改的」状态，一屏灰字里必须看得见，
        // 但它不是错误：用琥珀会被读成「这里出问题了」，而正文色在灰字里同样跳得出来。
        let follow_role = if st.src == SRC_FROM_SCHEMA {
            Role::Text
        } else {
            Role::TextMuted
        };
        let row = crate::widgets::setting_row_override(
            over,
            &item.label,
            item.hint.as_deref(),
            &st.src,
            follow_role,
            control,
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
    /// 用户决定它的死活；要清得走「全部恢复跟随」（[`Self::reset`]）。
    pub fn apply(&self, cfg: &mut Value) {
        if !cfg.is_object() {
            *cfg = json!({});
        }
        for (key, st) in self.items.borrow().iter() {
            let rest = key.trim_start_matches(MANIFEST_PREFIX);
            let mut path = vec!["engine", "codetable"];
            path.extend(rest.split('.'));
            let v = if st.over.get() {
                st.val.value()
            } else {
                Value::Null
            };
            set_path(cfg, &path, v);
        }
        // 两个嵌套子段各收一次空表（见 `prune_empty_section`）。
        prune_empty_section(cfg, "frequency");
        prune_empty_section(cfg, "english_merge");
    }

    /// 「全部恢复跟随」：把**所有**可覆盖字段置 null，包括界面上此刻没有控件的那些。
    ///
    /// 逐项 `apply` 够不着没有控件的字段（没控件就没信号），而 override 里可能留着
    /// 0.118 时代「开总开关整段写实」写进去的值。这个按钮是清掉它们的唯一入口。
    ///
    /// ★ 例外：被定制版 `[settings] hide` 或用户精简清单藏掉的项**原样保留**。藏起来
    /// 就是不让人在界面上动它，一键清空却能把它的方案覆盖值悄悄抹掉，与「隐藏只影响
    /// 显示」相悖。`user_hidden` 由调用方给：用户那一份要**现读**，清单上的快照在窗口
    /// 开着时新藏的项上是过时的。
    pub fn reset(
        &self,
        manifest: &Manifest,
        user_hidden: &dyn Fn(&MItem) -> bool,
        cfg: &mut Value,
    ) {
        if !cfg.is_object() {
            *cfg = json!({});
        }
        let is_hidden = |i: &MItem| i.custom_hidden || user_hidden(i);
        let hidden = |key: &str| manifest.get(key).is_some_and(is_hidden);
        for f in SPEC_BEHAVIOR_FIELDS {
            if !hidden(&format!("{MANIFEST_PREFIX}{f}")) {
                set_path(cfg, &["engine", "codetable", f], Value::Null);
            }
        }
        for sub in ["frequency", "english_merge"] {
            let prefix = format!("{MANIFEST_PREFIX}{sub}.");
            let in_sub: Vec<&MItem> = manifest
                .items
                .values()
                .filter(|i| i.key.starts_with(&prefix))
                .collect();
            if !in_sub.iter().any(|i| is_hidden(i)) {
                // 嵌套子段整段置 null 而非逐字段——段内字段可能多于 manifest 暴露的那几个，
                // 逐个清会漏。
                set_path(cfg, &["engine", "codetable", sub], Value::Null);
                continue;
            }
            // 段内有被藏的项：只能逐个清没藏的那些（清单外的字段因此留下）。
            for i in in_sub.iter().filter(|i| !is_hidden(i)) {
                let leaf = &i.key[prefix.len()..];
                set_path(cfg, &["engine", "codetable", sub, leaf], Value::Null);
            }
        }
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

/// 嵌套子段若逐字段全是 null，整段置 null。`section` 为段名（`frequency` / `english_merge`）。
///
/// 落盘形态与 0.118 的「整段清除」保持一致：留一个只含 null 的对象，`json_to_toml` 会
/// 把它写成一个空的 `[engine.codetable.<section>]` 表——语义上等价（字段全 None ＝ 跟随），
/// 但 override 文件里会多出一段谁也不读的空表，日后手工排查时纯属干扰。
fn prune_empty_section(cfg: &mut Value, section: &str) {
    let Some(freq) = cfg.pointer(&format!("/engine/codetable/{section}")) else {
        return;
    };
    let all_null = freq
        .as_object()
        .is_some_and(|o| !o.is_empty() && o.values().all(Value::is_null));
    if all_null {
        set_path(cfg, &["engine", "codetable", section], Value::Null);
    }
}

/// manifest 中属于方案级码表配置的可见项（按原序）。
///
/// 排除两类：
/// - `hidden = true`：与全局对话框一致，不渲染（也不参与逐项落盘，只在
///   [`CtFields::reset`] 里被清除；定制版藏掉的连那里也不清）
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

    /// 组装 `CtSeed`。四份数据都是 `&Value`，位置传错编译器不会吭声——用具名字段挡住。
    fn seed_of<'a>(
        effective: &'a Value,
        followed: &'a Value,
        overridden: &'a Value,
        merged: &'a Value,
    ) -> CtSeed<'a> {
        CtSeed {
            effective,
            followed,
            overridden,
            merged,
            follow_label: "跟随全局",
        }
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
            // 嵌套子段不进 SPEC_BEHAVIOR_FIELDS（那张表是扁平字段名，用来拼写回路径）；
            // 它们由 `reset` / `prune_empty_section` 按整段处理。
            .filter(|k| !k.starts_with("frequency.") && !k.starts_with("english_merge."))
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
        f.seed(&m, &seed_of(&eff, &eff, &json!({}), &json!({})));
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
        f.seed(&m, &seed_of(&eff, &eff, &json!({}), &json!({})));
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
    #[test]
    fn plan_splits_into_titled_groups_by_subsection() {
        let m = manifest();
        let e = json!({});
        let f = CtFields::default();
        f.seed(&m, &seed_of(&e, &e, &e, &e));
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
        f.seed(&m, &seed_of(&e, &e, &e, &e));
        let groups = f.plan();
        let (g0, g1) = (&groups[0], &groups[1]);
        assert_eq!(f.group_summary(&g0.title).get(), "全部跟随");
        assert_eq!(f.group_summary(&g1.title).get(), "全部跟随");

        // 勾第二组里的一项。
        f.items.borrow()[&g1.keys[0]].over.set(true);
        f.refresh_group_summaries();
        assert_eq!(
            f.group_summary(&g0.title).get(),
            "全部跟随",
            "另一组不该被算进来"
        );
        assert_eq!(f.group_summary(&g1.title).get(), "已自定义 1 项");
    }

    /// 换方案要重建分类摘要：两个方案的分类可能不同（特殊方案没有的段），
    /// 留着旧信号会在卡片上显示一个本方案根本没有的分类。
    #[test]
    fn reseed_resets_group_summaries() {
        let m = manifest();
        let e = json!({});
        let f = CtFields::default();
        f.seed(&m, &seed_of(&e, &e, &e, &e));
        let title = f.plan()[0].title.clone();
        f.items.borrow()[&f.plan()[0].keys[0]].over.set(true);
        f.refresh_group_summaries();
        assert_eq!(f.group_summary(&title).get(), "已自定义 1 项");

        f.seed(&m, &seed_of(&e, &e, &e, &e));
        assert_eq!(
            f.group_summary(&title).get(),
            "全部跟随",
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
        f.seed(&m, &seed_of(&e, &e, &e, &e));
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

    /// 勾选状态**只能**来自 override 层。
    ///
    /// 这是整个三态模型的地基。合并值（`engine.codetable`）里的 `Some(_)` 既可能是方案
    /// 作者写的、也可能是用户改的，拿它判勾选就会把「方案自带」显示成「用户已改」，
    /// 于是用户一取消勾选就以为回到了全局值，实际回到的是作者写的值。
    ///
    /// ⚠️ 内置方案从 0.119 起**一项都不声明**（理由见主仓 `wubi86.schema.toml`），
    /// 所以这条路径在出厂状态下走不到——只有装了第三方码表的用户才会踩。别因为
    /// 「本机试了没问题」就以为它没在用。
    #[test]
    fn checkbox_state_comes_from_the_override_layer_only() {
        let m = manifest();
        let eff = json!({ "short_code_yield_level": 2, "punct_commit": true });
        // 合并值里两项都有；override 里只有一项 ⇒ 另一项是方案作者写的。
        let merged = json!({ "short_code_yield_level": 2, "punct_commit": true });
        let ov = json!({ "short_code_yield_level": 2 });
        let f = CtFields::default();
        f.seed(&m, &seed_of(&eff, &eff, &ov, &merged));
        let items = f.items.borrow();
        assert!(
            items["schema.codetable.short_code_yield_level"].over.get(),
            "override 里有 ⇒ 勾选"
        );
        assert!(
            !items["schema.codetable.punct_commit"].over.get(),
            "只在合并值里有（＝方案作者写的）⇒ 不勾选"
        );
        assert_eq!(
            items["schema.codetable.punct_commit"].src, "方案自带",
            "来源要说清是方案作者定的，否则用户找不到它为什么不是全局值"
        );
        assert_eq!(
            items["schema.codetable.show_code_hint"].src, "跟随全局",
            "两处都没有 ⇒ 跟随基线，文案用调用方给的称呼"
        );
    }

    /// 落盘：勾选的写值，没勾的写 null。
    ///
    /// `null` 是「取消覆盖」的唯一落地方式（core 的 `json_to_toml` 跳过 null）。写成
    /// 「不写这个键」是不行的——cfg 是从 getConfig 拿来的整份合并结果，键本来就在里面，
    /// 不覆盖等于原样回传，那一项就永远取消不掉。
    #[test]
    fn apply_writes_values_for_checked_and_null_for_unchecked() {
        let m = manifest();
        let eff = json!({ "top_code_commit": true, "punct_commit": true });
        let f = CtFields::default();
        f.seed(&m, &seed_of(&eff, &eff, &json!({}), &json!({})));
        f.items.borrow()["schema.codetable.top_code_commit"]
            .over
            .set(true);
        let mut cfg = json!({ "engine": { "codetable": {
            "max_code_length": 4,
            "top_code_commit": true,
            "punct_commit": true,
        }}});
        f.apply(&mut cfg);
        let ct = cfg.pointer("/engine/codetable").unwrap();
        assert_eq!(
            ct.get("top_code_commit"),
            Some(&json!(true)),
            "勾选的项写实值"
        );
        assert_eq!(
            ct.get("punct_commit"),
            Some(&Value::Null),
            "没勾的项必须写 null（＝取消覆盖），不能原样留着"
        );
        assert_eq!(
            ct.get("max_code_length"),
            Some(&json!(4)),
            "引擎固定参数不归覆盖这层管，不该被动"
        );
        // 隐藏项（`auto_phrase.*`）没有控件 ⇒ 日常保存不碰它：既不凭空造一个……
        assert!(
            ct.get("auto_phrase").is_none(),
            "cfg 里本来就没有的隐藏项，apply 不该凭空造一个"
        );

        // ……也不清掉 cfg 里原有的那份。设置页不暴露的项，设置页也不替用户决定它的死活；
        // 清它的唯一入口是「全部恢复跟随」。
        let mut cfg2 = json!({ "engine": { "codetable": {
            "auto_phrase": { "min_phrase_len": 2 },
        }}});
        f.apply(&mut cfg2);
        assert_eq!(
            cfg2.pointer("/engine/codetable/auto_phrase/min_phrase_len"),
            Some(&json!(2)),
            "隐藏项在 cfg 里原有的值必须原样透传，不能被 apply 清成 null"
        );
    }

    /// 调频段逐项全没勾时整段收成一个 null，而不是留一堆 null 字段。
    ///
    /// 两者语义等价（字段全 None ＝ 跟随），差别在落盘形态：不收的话 override 文件里会
    /// 多出一段空的 `[engine.codetable.frequency]`，日后手工排查纯属干扰。
    #[test]
    fn apply_collapses_an_all_null_frequency_section() {
        let m = manifest();
        let eff = json!({ "frequency": { "enabled": true, "strategy": "top" } });
        let f = CtFields::default();
        f.seed(&m, &seed_of(&eff, &eff, &json!({}), &json!({})));
        let mut cfg = json!({ "engine": { "codetable": {} } });
        f.apply(&mut cfg);
        assert_eq!(
            cfg.pointer("/engine/codetable/frequency"),
            Some(&Value::Null),
            "整段没勾 ⇒ 整段 null"
        );

        // 反向对照：只要有一项勾着，就必须保留段结构，否则那一项跟着被清掉。
        f.items.borrow()["schema.codetable.frequency.enabled"]
            .over
            .set(true);
        let mut cfg2 = json!({ "engine": { "codetable": {} } });
        f.apply(&mut cfg2);
        assert_eq!(
            cfg2.pointer("/engine/codetable/frequency/enabled"),
            Some(&json!(true)),
            "有勾选项时不能把整段收掉"
        );
    }

    /// 「全部恢复跟随」必须清掉**所有**可覆盖字段，不只是界面上此刻有控件的那些。
    ///
    /// 没有控件就没有信号，逐项 `apply` 够不着；而 0.118 的「开总开关整段写实」会把
    /// 整张表写进 override。漏清的话，那些字段仍在偷偷生效，用户却在界面上找不到它们。
    #[test]
    fn reset_clears_every_overridable_field() {
        let m = manifest();
        let f = CtFields::default();
        let empty = json!({});
        f.seed(&m, &seed_of(&empty, &empty, &empty, &empty));
        let mut cfg = json!({
            "engine": { "codetable": {
                "max_code_length": 4,
                "top_code_commit": true,
                "auto_commit_min_len": 3,
                "frequency": { "enabled": true },
            }}
        });
        f.reset(&m, &|i| i.user_hidden, &mut cfg);
        let ct = cfg.pointer("/engine/codetable").unwrap();
        for field in SPEC_BEHAVIOR_FIELDS {
            assert_eq!(
                ct.get(*field),
                Some(&Value::Null),
                "{field} 未置 null，「全部恢复跟随」之后它会继续生效"
            );
        }
        assert_eq!(ct.get("frequency"), Some(&Value::Null), "调频段应整段清除");
        assert_eq!(
            ct.get("max_code_length"),
            Some(&json!(4)),
            "引擎固定参数不归覆盖这层管，不该被清"
        );
    }

    /// 定制版藏掉的项「全部恢复跟随」不清：扁平字段保留，子段里只清没藏的那几项。
    #[test]
    fn reset_keeps_custom_hidden_fields() {
        let (hide, errors) = wind_config::SettingsHide::parse(&[
            "key:schema.codetable.top_code_commit",
            "key:schema.codetable.frequency.half_life",
        ]);
        assert!(errors.is_empty(), "{errors:?}");
        let m = crate::custom_hide::settings_manifest(&hide, false);
        let f = CtFields::default();
        let mut cfg = json!({
            "engine": { "codetable": {
                "top_code_commit": true,
                "punct_commit": true,
                "frequency": { "enabled": true, "half_life": 7 },
                "english_merge": { "enable": true },
            }}
        });
        f.reset(&m, &|i| i.user_hidden, &mut cfg);
        let ct = cfg.pointer("/engine/codetable").unwrap();
        assert_eq!(ct.get("top_code_commit"), Some(&json!(true)));
        assert_eq!(ct.get("punct_commit"), Some(&Value::Null));
        assert_eq!(ct.pointer("/frequency/enabled"), Some(&Value::Null));
        assert_eq!(ct.pointer("/frequency/half_life"), Some(&json!(7)));
        assert_eq!(
            ct.get("english_merge"),
            Some(&Value::Null),
            "没藏项的子段照旧整段清"
        );
    }

    /// 「全部恢复跟随」只该清界面上看得见的键（R8.5）。
    ///
    /// `reset` 清的不是逐项信号，而是 `SPEC_BEHAVIOR_FIELDS` 十五个扁平字段，
    /// 外加 `frequency` / `english_merge` 两个子段**整段**置 null——整段清的话，
    /// 子段里任何一个 `hidden = true` 的字段会跟着被清掉，却在界面上找不到：
    /// 用户既看不见它被清过，也没法在界面上把它拨回来。
    ///
    /// 这条测试把「reset 实际会碰的键」与 `visible_keys()` 对齐，谁清了却不可见，
    /// 谁就在这里炸。核实过它能红：临时把一个不存在于 manifest 的键塞进
    /// `SPEC_BEHAVIOR_FIELDS`，这条测试会先报「界面上看不见的键」再还原；frequency /
    /// english_merge 分支的红态见下面 `struct_field_names` 旁的说明（记录见
    /// task-7-report.md）。
    #[test]
    fn reset_touches_only_keys_visible_for_this_engine() {
        let m = manifest();
        let visible = visible_keys(&m);
        let visible_set: std::collections::HashSet<&str> =
            visible.iter().map(String::as_str).collect();

        // SPEC_BEHAVIOR_FIELDS：扁平字段名，逐个映射回 manifest key 再查可见性。
        for f in SPEC_BEHAVIOR_FIELDS {
            let key = format!("{MANIFEST_PREFIX}{f}");
            assert!(
                visible_set.contains(key.as_str()),
                "reset 清了界面上看不见的键 {key}（R8.5）"
            );
        }

        // frequency / english_merge：reset 把整段置 null，段内任何字段都会被一并清掉。
        // 字段集**不能**从 manifest 的 section_keys 取——那只反映 manifest 里写了的
        // 条目，「core 给 CodeTableFrequencySpec/CodeTableEnglishMergeSpec 加了字段
        // 但 manifest 压根没补条目」这种漏配，section_keys 看不出来，reset 却照样把
        // 那个字段一并清掉。改成直接枚举 core 结构体自身的字段名。
        for (section, keys) in [
            (
                "frequency",
                struct_field_names(&wind_config::schema::CodeTableFrequencySpec::default()),
            ),
            (
                "english_merge",
                struct_field_names(&wind_config::schema::CodeTableEnglishMergeSpec::default()),
            ),
        ] {
            assert!(
                !keys.is_empty(),
                "{section} 的字段集取空了，struct_field_names 的取值方式需要重新核实"
            );
            for f in keys {
                let key = format!("{MANIFEST_PREFIX}{section}.{f}");
                assert!(
                    visible_set.contains(key.as_str()),
                    "reset 整段清掉 {section}，其中 {key} 在界面上不可见（R8.5）"
                );
            }
        }

        // auto_phrase 原样保留：reset 不动它（不在 SPEC_BEHAVIOR_FIELDS，也不是
        // frequency/english_merge 子段），这里只确认这组前提本身没漂——具体的透传
        // 行为由 `apply_writes_values_for_checked_and_null_for_unchecked` 钉住。
        assert!(
            section_keys(&m)
                .iter()
                .any(|k| k.starts_with(&format!("{MANIFEST_PREFIX}auto_phrase."))),
            "auto_phrase 组消失了，reset 的排除逻辑需要重新核实"
        );
    }

    /// 把一个 core 配置结构体自身的字段名取出来（不经过 manifest）。
    ///
    /// 用 `serde_json::to_value` 而不是手写字段名列表：这两个结构体的字段都是
    /// `Option<T>` 且没挂 `skip_serializing_if`（见 `wind_config::schema`），序列化
    /// 后每个字段都以 `null` 或实际值的形式出现在对象里，键名与结构体字段名逐一对应，
    /// 用 `Default` 实例就够，不必费劲构造一个各字段都 `Some` 的实例。
    fn struct_field_names(v: &impl serde::Serialize) -> Vec<String> {
        serde_json::to_value(v)
            .ok()
            .and_then(|j| j.as_object().map(|o| o.keys().cloned().collect()))
            .unwrap_or_default()
    }

    /// `revert_all` 把控件值拨回**跟随值**，而不是留着用户刚改的那个。
    ///
    /// 界面上未勾选的行显示的就是跟随值；不拨回的话，用户按下「全部恢复跟随」看到的
    /// 还是自己改过的数，而落盘落的是取消覆盖——界面与结果不一致。
    #[test]
    fn revert_all_pulls_values_back_to_the_followed_ones() {
        let m = manifest();
        // 生效值 2（用户改过），跟随值 3（方案作者写的）。
        let eff = json!({ "short_code_yield_level": 2 });
        let followed = json!({ "short_code_yield_level": 3 });
        let ov = json!({ "short_code_yield_level": 2 });
        let f = CtFields::default();
        f.seed(&m, &seed_of(&eff, &followed, &ov, &ov));
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
        f.seed(&m, &seed_of(&eff, &followed, &json!({}), &json!({})));
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
        f.seed(&m, &seed_of(&eff, &json!({}), &json!({}), &json!({})));
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
        f.seed(&m, &seed_of(&eff, &eff, &json!({}), &json!({})));
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
        f.seed(&m, &seed_of(&eff, &eff, &json!({}), &json!({})));
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
        f.seed(&m, &seed_of(&eff, &eff, &json!({}), &json!({})));
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
}
