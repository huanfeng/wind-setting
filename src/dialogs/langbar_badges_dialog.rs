//! 「角标规则」对话框：管理 `[[ui.langbar.badges]]` —— 任务栏输入指示器上叠加的状态角标。
//!
//! 一条规则说的是「某状态成立时，在某个角落用某色画一个角标」。core 侧只有一种形状
//! （角落直角三角），可配的是**状态、位置、亮暗两色、大小**——这几个自由度组合起来
//! 必须是一张表，摊成扁平键要铺十几个，且状态集合一变就得整段重排。
//!
//! # 顺序即优先级，所以必须能排序
//!
//! 位置成了规则的属性之后，两条规则可以落在同一个角落（如把「全角」也挪到右下），
//! 而 16px 上叠两个三角只会糊成一片——core 的规矩是**一个角落只画最靠前命中的那条**。
//! 列表因此用 `reorder_list_signal` 拖拽排序：不给排序手段，用户就没有任何办法表达
//! 「我要标点优先」。与注释词库那屏同一条理由。
//!
//! # ★ 启用态与条目定义分开存
//!
//! `rows` 只装条目定义，启用开关另存在 `switches`。合在一起的话，每次重建行都要现造
//! 一个 `Signal<bool>`，而行是随 `rows` 变化重建的 ⇒ 开关信号每次都换一个新的，绑在
//! 上面的 `on_toggle` 写回旧信号。与注释词库、工具栏「显示内容」两屏同构。
//!
//! ⚠️ 索引开关的键是**行内生成的 `uid`，不是状态 id 也不是下标**：状态可以重复
//! （同一个状态配到两个角落是合法的），下标又会被拖拽改掉——两者都做不了稳定键。
//!
//! # ★ 「不受打扰」是关掉这条，不是删掉它
//!
//! 用户最常见的诉求是「中文标点是常态，别提醒我；切到英文标点时才亮一下」。做成开关
//! 而不是让他删掉整条：关掉保留颜色与位置，想回来时勾上就行，反复比选时差别很大。
//!
//! # ★ 写回在原对象上 insert，不重建
//!
//! [`BadgeRow::raw`] 留着读进来的整个 JSON 对象。core 的条目结构将来加字段时，用户手写
//! 的那些值不会被这一屏静默抹掉——`[overlay]` 那节踩过这个坑（用字面量重建对象，把段里
//! 本 UI 不管的项一并吃掉）。
//!
//! # ★ 颜色格与透明度格分开（R8.4，整改项 F7）
//!
//! core 侧 `LangBarBadge` 把「颜色」（`color_light`/`color_dark`，`""` = 与主字同色）与
//! 「这一条自己的不透明度」（`alpha_light`/`alpha_dark`，`Option<f32>`，`None` = 跟随全局
//! `badge_alpha`）拆成了两个独立字段。这一屏跟着拆：颜色仍是取色器 + 「与主字同色」
//! 按钮；透明度换成**勾选行**（未勾 = 跟随全局，勾上才露出滑块），不再靠旧版那种
//! 「八位色值末两位当透明度」的编码——两个自由度一旦揉进同一个字符串，「六位＝跟随
//! 全局、八位 FF＝这一条自己钉成实心挖空」这条规则只能靠文档记着，改错一位就悄悄
//! 切到另一档而界面毫无察觉。
//!
//! 旧写法（`auto` / `#RRGGBBAA`）只在**读**这一侧兼容：[`rows_from_value`] 把它们拆成
//! 新形态（`""` / `#RRGGBB` + 对应 `alpha_x`），即便用户层还没被 core 迁移过也能正确
//! 显示；保存时这一屏只写新形态，两种写法不会同时出现在同一份配置里。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

use crate::dialogs::langbar_text_colors_dialog::fallback_color;
use crate::widgets::{
    PickedColor, color_from_value, color_picker_cell, color_swatch, override_control_signal,
    parse_color,
};

/// 可选的状态，`(落盘 id, 界面名)`。**必须与 core 的 `BadgeState::as_id()` 逐字一致。**
///
/// 只有这三个：它们在 core 的 `IconSpec` 里都已有对应数据。半角与英文态刻意不开放——
/// 半角是常态，给它画标记等于在 16×16 上常驻一个永不变化的点；英文态下主字本身已经
/// 是「英」，角标只是重复。
pub const STATES: &[(&str, &str)] = &[
    ("punct_cn", "中文标点"),
    ("punct_en", "英文标点"),
    ("full_width", "全角"),
];

/// 可选的角落，`(落盘 id, 界面名)`。**必须与 core 的 `Corner::as_id()` 逐字一致。**
pub const CORNERS: &[(&str, &str)] = &[
    ("top_left", "左上角"),
    ("top_right", "右上角"),
    ("bottom_right", "右下角"),
    ("bottom_left", "左下角"),
];

/// 未知角落的回落目标，与 core 的 `Corner::from_id` 一致。
const DEFAULT_CORNER: &str = "bottom_right";

/// 大小倍率的步进与上限。步进与设置页那个全局倍率滑块保持一致——同一个量在两处
/// 用不同的粒度调，用户会以为其中一处"调不准"。
const SCALE_STEP: f64 = 0.1;
const MAX_SCALE: f64 = 3.0;

/// 单条透明度覆盖的步进，与全局 `ui.langbar.badge_alpha` 滑块一致（见 manifest 里的注释：
/// 出厂值 0.88 必须落在格子上）。
const ALPHA_STEP: f64 = 0.02;

/// 单条透明度覆盖的下限，与全局 `ui.langbar.badge_alpha` 滑块的 `min = 0.3` 一致
/// （控制者裁定）：同一个量两处范围不同，用户在这里拖到 0.1 会以为全局滑块「拖不到底」。
/// 读入钳制也用它——stepper 装不下界外值，读进来就得落在它的范围里。
const MIN_BADGE_ALPHA: f32 = 0.3;

/// 一条角标规则（对应一个 `[[ui.langbar.badges]]`）。
///
/// **不含 `enabled`**：那一位存在 [`LangBarBadgesUi::switches`] 里，理由见模块文档。
#[derive(Clone, PartialEq)]
pub struct BadgeRow {
    /// 仅进程内有效的稳定标识，用来索引启用开关。**不落盘**——见模块文档里那条 ⚠️。
    pub uid: u32,
    /// 状态 id，取值见 [`STATES`]。
    pub state: String,
    /// 角落 id，取值见 [`CORNERS`]。
    pub corner: String,
    /// 浅色任务栏上的颜色：`#RRGGBB`，或 `""`（与主字同色）。不透明度见 [`Self::alpha_light`]。
    pub color_light: String,
    /// 深色任务栏上的颜色，语义同 [`Self::color_light`]。
    pub color_dark: String,
    /// 本条在浅色任务栏上自己的不透明度（0~1）。`None` = 跟随全局 `badge_alpha`。
    /// `Some(1.0)` 是「实心 + 挖空」档，语义同旧写法八位色值末两位 `FF`。
    pub alpha_light: Option<f32>,
    /// 本条在深色任务栏上自己的不透明度，语义同 [`Self::alpha_light`]。
    pub alpha_dark: Option<f32>,
    /// 相对整体大小的额外倍率。
    pub scale: f32,
    /// 读进来的原始对象，写回时在它上面 insert。见模块文档。
    pub raw: Value,
}

/// 新建态的 `editing` 哨兵。
pub const NEW_BADGE: usize = usize::MAX;

/// 新建 / 编辑一条规则的表单。**两者共用**——差别只在确认时是追加还是替换。
#[derive(Clone)]
pub struct BadgeForm {
    pub visible: Signal<bool>,
    /// 正在编辑第几条；[`NEW_BADGE`] = 新建。
    pub editing: Signal<usize>,
    /// 状态在 [`STATES`] 里的下标。
    pub state_idx: Signal<usize>,
    /// 角落在 [`CORNERS`] 里的下标。
    pub corner_idx: Signal<usize>,
    /// 「亮暗用同一个颜色」。勾上时深色框跟着浅色走，用户只需填一个。
    ///
    /// 出厂三条都是这个形态——那三色本就是按「深浅两种任务栏上都立得住」挑的。
    /// 真要分开配是自由度，不该是每次新建都得填两遍的负担。
    pub same_color: Signal<bool>,
    /// 浅 / 深两色的取色绑定。`value` 是配置字符串（`""` / `#RRGGBB`）。
    pub color_light: PickedColor,
    pub color_dark: PickedColor,
    /// 浅 / 深两侧「这一条自己的透明度」勾选行：未勾 = 跟随全局，勾上才用后面的值。
    pub alpha_light_over: Signal<bool>,
    pub alpha_light: Signal<f64>,
    pub alpha_dark_over: Signal<bool>,
    pub alpha_dark: Signal<f64>,
    /// 全局 `ui.langbar.badge_alpha` 的当前值——未勾时勾选行显示「跟随全局（这个值）」，
    /// 勾上瞬间控件也播成这个值（同 `theme_follow` 那套「勾选瞬间必须已是跟随值」的裁决）。
    /// 由外部（`manifest.rs`）灌入，构造时先给 core 的出厂值占位。
    ///
    /// ⚠️ 它是**打开表单那一刻**的全局值：全局滑块与本对话框在同一页，用户可以先拖滑块
    /// 再开表单，此时配置还没保存、`badge_alpha` 仍是加载时的数。所以每次打开都经
    /// [`Self::badge_alpha_source`] 重取一次「当前编辑中」的滑块值。
    pub badge_alpha: Signal<f32>,
    /// 取全局滑块**当前编辑中**的值；`None` = 拿不到（滑块尚未建出等），保留原值。
    /// 由 `manifest.rs` 接到 `FieldRegistry::getters` 上，见 [`Self::sync_badge_alpha`]。
    pub badge_alpha_source: Rc<RefCell<Option<Box<dyn Fn() -> Option<f32>>>>>,
    /// 大小倍率。用 [`Element::stepper`] 而非文本框：倍率是个连续量，按 0.1 步进点
    /// 两下就到位，而文本框还得自己校验"填了个字母"。
    pub scale: Signal<f64>,
    /// 校验失败原因；空 = 无。就地显示，不再弹一层。
    pub error: Signal<String>,
    pub title: Signal<String>,
}

impl BadgeForm {
    pub fn new() -> Self {
        Self {
            visible: signal(false),
            editing: signal(NEW_BADGE),
            state_idx: signal(0),
            corner_idx: signal(2), // 右下角：最不挡主字的一角
            same_color: signal(true),
            color_light: PickedColor::new(
                DEFAULT_COLOR,
                color_from_value(DEFAULT_COLOR, fallback_color(0, None)),
            ),
            color_dark: PickedColor::new(
                DEFAULT_COLOR,
                color_from_value(DEFAULT_COLOR, fallback_color(1, None)),
            ),
            alpha_light_over: signal(false),
            alpha_light: signal(DEFAULT_BADGE_ALPHA as f64),
            alpha_dark_over: signal(false),
            alpha_dark: signal(DEFAULT_BADGE_ALPHA as f64),
            badge_alpha: signal(DEFAULT_BADGE_ALPHA),
            badge_alpha_source: Rc::new(RefCell::new(None)),
            scale: signal(1.0),
            error: signal(String::new()),
            title: signal("添加角标规则".to_string()),
        }
    }

    /// 从 [`Self::badge_alpha_source`] 把全局滑块当前值同步进 `badge_alpha`。
    /// 「跟随全局（x）」文案对 `badge_alpha` 做了响应，这里一设就跟着变。
    pub fn sync_badge_alpha(&self) {
        let cur = self.badge_alpha_source.borrow().as_ref().and_then(|f| f());
        if let Some(v) = cur {
            self.badge_alpha.set(v);
        }
    }

    pub fn open_new(&self) {
        self.sync_badge_alpha();
        self.editing.set(NEW_BADGE);
        self.state_idx.set(0);
        self.corner_idx.set(index_of(CORNERS, DEFAULT_CORNER));
        self.same_color.set(true);
        set_badge_color(&self.color_light, DEFAULT_COLOR, fallback_color(0, None));
        set_badge_color(&self.color_dark, DEFAULT_COLOR, fallback_color(1, None));
        self.alpha_light_over.set(false);
        self.alpha_light.set(self.badge_alpha.get() as f64);
        self.alpha_dark_over.set(false);
        self.alpha_dark.set(self.badge_alpha.get() as f64);
        self.scale.set(1.0);
        self.error.set(String::new());
        self.title.set("添加角标规则".to_string());
        self.visible.set(true);
    }

    pub fn open_edit(&self, index: usize, row: &BadgeRow) {
        self.sync_badge_alpha();
        self.editing.set(index);
        self.state_idx.set(index_of(STATES, &row.state));
        self.corner_idx.set(index_of(CORNERS, &row.corner));
        self.same_color
            .set(row.color_light.eq_ignore_ascii_case(&row.color_dark));
        set_badge_color(&self.color_light, &row.color_light, fallback_color(0, None));
        set_badge_color(&self.color_dark, &row.color_dark, fallback_color(1, None));
        self.alpha_light_over.set(row.alpha_light.is_some());
        self.alpha_light
            .set(row.alpha_light.unwrap_or(self.badge_alpha.get()) as f64);
        self.alpha_dark_over.set(row.alpha_dark.is_some());
        self.alpha_dark
            .set(row.alpha_dark.unwrap_or(self.badge_alpha.get()) as f64);
        self.scale.set(row.scale as f64);
        self.error.set(String::new());
        self.title.set("编辑角标规则".to_string());
        self.visible.set(true);
    }
}

impl Default for BadgeForm {
    fn default() -> Self {
        Self::new()
    }
}

/// 「角标规则」对话框的全部可变状态。
#[derive(Clone)]
pub struct LangBarBadgesUi {
    pub visible: Signal<bool>,
    pub rows: Signal<Vec<BadgeRow>>,
    /// uid → 启用开关。
    pub switches: Rc<RefCell<HashMap<u32, Signal<bool>>>>,
    /// 下一个可用 uid。只增不减，删了再加也不会撞上还没清干净的旧开关。
    next_uid: Rc<Cell<u32>>,
    pub form: BadgeForm,
}

impl LangBarBadgesUi {
    pub fn new() -> Self {
        Self {
            visible: signal(false),
            rows: signal(Vec::new()),
            switches: Rc::new(RefCell::new(HashMap::new())),
            next_uid: Rc::new(Cell::new(0)),
            form: BadgeForm::new(),
        }
    }

    fn take_uid(&self) -> u32 {
        let id = self.next_uid.get();
        self.next_uid.set(id.wrapping_add(1));
        id
    }

    /// 取某条的启用开关；不存在时**当场建一个**（新建那一刻还没有）。
    pub fn switch_of(&self, uid: u32, default_on: bool) -> Signal<bool> {
        if let Some(s) = self.switches.borrow().get(&uid) {
            return *s;
        }
        let s = signal(default_on);
        self.switches.borrow_mut().insert(uid, s);
        s
    }

    /// 用配置里的数组播种（打开对话框时调用）。
    pub fn seed(&self, items: &[Value]) {
        self.switches.borrow_mut().clear();
        let mut rows = Vec::with_capacity(items.len());
        for (r, on) in rows_from_value(items).into_iter().zip(enabled_flags(items)) {
            let uid = self.take_uid();
            self.switches.borrow_mut().insert(uid, signal(on));
            rows.push(BadgeRow { uid, ..r });
        }
        self.rows.set(rows);
    }

    /// 提交表单：新建则追加，编辑则替换。
    pub fn commit_form(
        &self,
        index: usize,
        state: String,
        corner: String,
        color_light: String,
        color_dark: String,
        alpha_light: Option<f32>,
        alpha_dark: Option<f32>,
        scale: f32,
    ) {
        if index == NEW_BADGE {
            let uid = self.take_uid();
            self.switch_of(uid, true); // 新建即启用
            self.rows.update(|v| {
                v.push(BadgeRow {
                    uid,
                    state,
                    corner,
                    color_light,
                    color_dark,
                    alpha_light,
                    alpha_dark,
                    scale,
                    raw: json!({}),
                })
            });
            return;
        }
        self.rows.update(|v| {
            if let Some(slot) = v.get_mut(index) {
                // uid 与 raw 原样留着：前者绑着启用开关（换掉等于把这条的启用态清零），
                // 后者装着本屏不认识的字段（重建等于把用户手写的值悄悄吃掉）。
                slot.state = state;
                slot.corner = corner;
                slot.color_light = color_light;
                slot.color_dark = color_dark;
                slot.alpha_light = alpha_light;
                slot.alpha_dark = alpha_dark;
                slot.scale = scale;
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
        self.switches.borrow_mut().remove(&r.uid);
    }

    /// 当前工作态 → 回写给 core 的数组。
    pub fn to_value(&self) -> Value {
        let sw = self.switches.borrow();
        Value::Array(
            self.rows
                .get()
                .iter()
                .map(|r| {
                    let on = sw.get(&r.uid).map(|s| s.get()).unwrap_or(true);
                    row_to_value(r, on)
                })
                .collect(),
        )
    }
}

impl Default for LangBarBadgesUi {
    fn default() -> Self {
        Self::new()
    }
}

/// 在 `(id, label)` 表里查 id 的下标；未知回落 0。
fn index_of(table: &[(&str, &str)], id: &str) -> usize {
    table.iter().position(|(k, _)| *k == id).unwrap_or(0)
}

/// 查 id 对应的界面名；未知原样返回（手写配置里可能有本屏不认识的值）。
fn label_of(table: &[(&str, &str)], id: &str) -> String {
    table
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, l)| l.to_string())
        .unwrap_or_else(|| id.to_string())
}

/// 倍率 → 文本。整数不带小数点（`1` 而不是 `1.00`），免得输入框里一片零。
pub fn fmt_scale(v: f32) -> String {
    if (v - v.round()).abs() < f32::EPSILON {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.2}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

/// 拆一个色格的旧写法：`auto`（不分大小写）→ `("", None)`；`#RRGGBBAA` → 拆成
/// `(#RRGGBB, Some(AA/255))`；其余（新写法的 `""`/`#RRGGBB`，或非法值）原样返回、
/// 不带出透明度——非法值留给 [`validate`] 去报错，这里不替用户猜。
///
/// ★ 只在**读**这一侧兼容：core 迁移过的用户层不会再产出这两种写法，但设置端拿到的
/// 可能是尚未迁移的定制层值，或干脆是老版本 core 直接吐回来的存量配置。
fn split_legacy_color(raw: &str) -> (String, Option<f32>) {
    let t = raw.trim();
    if t.eq_ignore_ascii_case("auto") {
        return (String::new(), None);
    }
    // 8 位旧写法：末两位是这一条自己的透明度。`#` 可省——手写配置里两种写法都见过，
    // 与 core 的 `parse_hex` 认定一致（不写 `#` 也认）。
    let hex8 = t.strip_prefix('#').unwrap_or(t);
    if hex8.len() == 8 && hex8.bytes().all(|c| c.is_ascii_hexdigit()) {
        let aa = u8::from_str_radix(&hex8[6..], 16).unwrap_or(0xFF);
        let alpha = (f32::from(aa) / 255.0).clamp(MIN_BADGE_ALPHA, 1.0);
        return (format!("#{}", &hex8[..6]), Some(alpha));
    }
    // 其余（新写法的 `""` / `#RRGGBB`，不拘大小写，或非法值）**原样**返回，不规范大小写。
    // 这是读入路径：脏检查比的是「写回值与原始配置逐字是否相同」（`state.rs` 的
    // base_config diff），读进来就把大小写改了，手写小写色的存量配置一打开就会被判成
    // 「有未保存的改动」。大小写规范化只在用户真的在面板上选了色、要写出去的那条路径上
    // 做（见 `normalize_color` 在表单确认里的用法），读这一侧只拆两种旧写法，不多做。
    (t.to_string(), None)
}

/// 解析配置数组 → 条目定义（纯函数，供测试）。`uid` 由调用方补，这里恒填 0。
///
/// ★ 不认识的状态 / 角落**原样留着**而不是丢弃或改写：core 那边状态不认识会丢掉整条
/// （并记警告），但那是 core 的事；这一屏若也丢弃，用户会看到「我明明配了三条，界面
/// 里只有两条」，而那两条一保存就把第三条抹掉了。同注释词库那屏的判据。
///
/// 色值兼容旧写法（见 [`split_legacy_color`]）；`alpha_x` 键若已显式写着，以它为准
/// （新键比旧色值末两位更可信——它可能是用户后来手动改的）。
pub fn rows_from_value(items: &[Value]) -> Vec<BadgeRow> {
    items
        .iter()
        .map(|it| {
            let s = |k: &str, d: &str| {
                it.get(k)
                    .and_then(|v| v.as_str())
                    .filter(|x| !x.is_empty())
                    .unwrap_or(d)
                    .to_string()
            };
            let (color_light, legacy_alpha_light) = split_legacy_color(&s("color_light", ""));
            let (color_dark, legacy_alpha_dark) = split_legacy_color(&s("color_dark", ""));
            // 钳到 0.3..=1（[`MIN_BADGE_ALPHA`]）：手写配置里的越界值（负数、大于 1）
            // 没有意义，core 侧 `wind_ui::langbar_icon::IconRenderer::badge_alpha` 按
            // 不透明度用；下限取 0.3 与全局滑块、本表单 stepper 同一范围。
            let alpha = |k: &str, legacy: Option<f32>| {
                it.get(k)
                    .and_then(|v| v.as_f64())
                    .map(|v| (v as f32).clamp(MIN_BADGE_ALPHA, 1.0))
                    .or(legacy)
            };
            BadgeRow {
                uid: 0,
                state: s("state", ""),
                corner: s("corner", DEFAULT_CORNER),
                color_light,
                color_dark,
                alpha_light: alpha("alpha_light", legacy_alpha_light),
                alpha_dark: alpha("alpha_dark", legacy_alpha_dark),
                scale: it
                    .get("scale")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as f32)
                    .unwrap_or(1.0),
                raw: it.clone(),
            }
        })
        .collect()
}

/// 各条目的启用位（缺省视为启用，与 core 的 `default_true` 同源）。
pub fn enabled_flags(items: &[Value]) -> Vec<bool> {
    items
        .iter()
        .map(|it| it.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true))
        .collect()
}

/// 颜色这一维要写回的值：跟随主字时（`follow`）恒为 `""`，不管 `color` 参数本身是什么
/// ——调用方多半已经保证它是 `""`，这里再钉一遍是防御性的，与 [`badge_alpha_write`]
/// 同一套「未勾 / 跟随就不信任残留值」的写法。
pub fn badge_color_value(follow: bool, color: &str) -> Value {
    if follow { json!("") } else { json!(color) }
}

/// 一条透明度勾选行要写回的值：未勾（`over = false`）= 跟随全局，写 `Value::Null`
/// （调用方据此从对象里**删掉**这个键，TOML 没有 null，不能真写个 null 进去）；
/// 勾上则写这一条自己的值。
pub fn badge_alpha_write(over: bool, v: f32) -> Value {
    if over { json!(v) } else { Value::Null }
}

/// 一条 → JSON。在 `raw` 上 insert，本 UI 不管的字段原样保留（见模块文档）。
///
/// `alpha_light` / `alpha_dark` 走 [`badge_alpha_write`]：`Value::Null` 时**删掉**那个键
/// 而不是写 `null`——`raw` 里可能残留着上次「勾上过」时插的旧值，跟随全局却让它们
/// 复活是最容易让人以为「取消勾选没生效」的一种。
pub fn row_to_value(r: &BadgeRow, enabled: bool) -> Value {
    let mut v = r.raw.clone();
    if !v.is_object() {
        v = json!({});
    }
    let obj = v.as_object_mut().expect("上面刚保证是对象");
    obj.insert("enabled".into(), json!(enabled));
    obj.insert("state".into(), json!(r.state));
    obj.insert("corner".into(), json!(r.corner));
    obj.insert(
        "color_light".into(),
        badge_color_value(r.color_light.trim().is_empty(), &r.color_light),
    );
    obj.insert(
        "color_dark".into(),
        badge_color_value(r.color_dark.trim().is_empty(), &r.color_dark),
    );
    for (key, alpha) in [("alpha_light", r.alpha_light), ("alpha_dark", r.alpha_dark)] {
        match badge_alpha_write(alpha.is_some(), alpha.unwrap_or(0.0)) {
            Value::Null => {
                obj.remove(key);
            }
            val => {
                obj.insert(key.to_string(), val);
            }
        }
    }
    obj.insert("scale".into(), json!(r.scale));
    v
}

/// 一个色值输入是否合法：`""`（与主字同色）或 `#RRGGBB`。
///
/// 八位写法（旧版内嵌透明度）**不再合法**——透明度走独立字段，这一维只认颜色。
pub fn color_is_valid(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return true;
    }
    let hex = t.strip_prefix('#').unwrap_or(t);
    hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// 归一化色值文本：十六进制一律大写、补回 `#`，免得同一个色在列表里长得两样。
/// 非法值原样留着交给 [`validate`] 去报错，不在这里悄悄改写。
pub fn normalize_color(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        return String::new();
    }
    let hex = t.strip_prefix('#').unwrap_or(t);
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("#{}", hex.to_uppercase())
    } else {
        t.to_string()
    }
}

/// 校验表单，返回解析出的倍率。纯函数，供测试。
///
/// 倍率来自 stepper（值域由控件保证），这里仍复核一遍：控件的 min/max 是 UI 约定，
/// 而写回的是配置文件，值域该由写侧自己守住。
pub fn validate(color_light: &str, color_dark: &str, scale: f64) -> Result<f32, &'static str> {
    if !color_is_valid(color_light) {
        return Err("浅色任务栏的颜色要填 #RRGGBB 或留空（与主字同色）");
    }
    if !color_is_valid(color_dark) {
        return Err("深色任务栏的颜色要填 #RRGGBB 或留空（与主字同色）");
    }
    if !scale.is_finite() || scale <= 0.0 {
        return Err("大小倍率要大于 0");
    }
    if scale > MAX_SCALE {
        return Err("大小倍率最大 3");
    }
    Ok(scale as f32)
}

/// 一条的副标题：位置 · 颜色 · 透明度 · 倍率。亮暗同色 / 同透明度时只写一遍，
/// 免得一行里两遍同一个数。
pub fn row_desc(r: &BadgeRow) -> String {
    let mut parts = vec![label_of(CORNERS, &r.corner)];
    if r.color_light.eq_ignore_ascii_case(&r.color_dark) {
        parts.push(color_text(&r.color_light));
    } else {
        parts.push(format!(
            "浅 {} / 深 {}",
            color_text(&r.color_light),
            color_text(&r.color_dark)
        ));
    }
    // 两侧都跟随全局透明度是常态，不占一段；有一侧自己钉了值才提。
    if let Some(seg) = alpha_desc(r.alpha_light, r.alpha_dark) {
        parts.push(seg);
    }
    // 倍率 1 是常态，不占一段——只在真被调过时才提。
    if (r.scale - 1.0).abs() > f32::EPSILON {
        parts.push(format!("×{}", fmt_scale(r.scale)));
    }
    parts.join(" · ")
}

fn color_text(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        return "与主字同色".to_string();
    }
    match parse_color(t) {
        Some((rgb, _)) => format!("#{rgb:06X}"),
        None => t.to_uppercase(),
    }
}

/// 副标题里的透明度小节；两侧都跟随全局时返回 `None`（不占一段）。
fn alpha_desc(light: Option<f32>, dark: Option<f32>) -> Option<String> {
    if light.is_none() && dark.is_none() {
        return None;
    }
    let seg = |a: Option<f32>| match a {
        Some(v) if v >= 1.0 => "实心·挖空".to_string(),
        Some(v) => format!("不透明度{}%", (v * 100.0).round() as i32),
        None => "跟随全局透明度".to_string(),
    };
    if light == dark {
        Some(seg(light))
    } else {
        Some(format!("浅 {} / 深 {}", seg(light), seg(dark)))
    }
}

/// 新建规则的默认色（出厂「中文标点」那条的蓝）。取色器里显示这个色本身即可
/// （不再有 `auto` 那档要额外回落）。
const DEFAULT_COLOR: &str = "#2288E0";

/// 全局 `ui.langbar.badge_alpha` 拿不到时的占位（与 core 出厂值一致）。
pub const DEFAULT_BADGE_ALPHA: f32 = 0.88;

/// 程序侧设置一色（播种 / 与主字同色）：`""` / 非法值在取色器里显示 `fallback`。
pub fn set_badge_color(pc: &PickedColor, value: &str, fallback: Color) {
    pc.set(value, color_from_value(value, fallback));
}

/// 取色器选出的颜色 → 配置值。这一维只有颜色，没有透明度（见模块文档），
/// 故直接是 6 位色号，不必判断「动没动过透明度」。
///
/// ★ 调用方（[`color_row`]）必须把取色面板的 `alpha` 选项关掉——面板一旦带透明度条，
/// 用户拖它时色块会跟着变淡，而这里却把 `c.a` 直接扔掉，「拖了条却没存」会被当成控件
/// 坏了。这条 `debug_assert` 钉住这个前提：面板真带了透明度条、用户又确实调过（`a`
/// 就不会是满值），dev/test 构建下会立刻炸出来，而不是悄悄丢一个用户以为生效了的值。
pub fn badge_value_of(_prev: &str, c: Color) -> String {
    debug_assert_eq!(
        c.a, 0xFF,
        "badge_value_of 收到了带透明度的颜色——取色面板是不是忘了关 alpha？"
    );
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

/// 表单格内文字：颜色本身（这一维不再带透明度说明，那部分挪到 [`alpha_row`]）。
pub fn badge_color_text(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        return "与主字同色".to_string();
    }
    match parse_color(t) {
        Some((rgb, _)) => format!("#{rgb:06X}"),
        None => format!("{t}（无效）"),
    }
}

/// 表单里的一行：`标签 [控件]`。
fn form_row(label: &str, ctrl: Element) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(Element::label(label).font_size(12.5).width(96))
        .child(ctrl)
}

/// 表单里的一行取色：色块 + 当前值 + 「与主字同色」按钮。
fn color_row(label: &str, pc: &PickedColor, fallback: Color) -> Element {
    let value = pc.value;
    let pa = pc.clone();
    form_row(
        label,
        color_picker_cell(
            pc,
            |s| badge_color_text(s),
            92,
            badge_value_of,
            false, // 透明度走独立勾选行（alpha_row），面板里不重复放一条
            vec![
                Element::button("与主字同色")
                    .small()
                    .outline_soft()
                    .enabled_when(move || !value.get().trim().is_empty())
                    .on_click(move |_| set_badge_color(&pa, "", fallback)),
            ],
        ),
    )
}

/// 表单里的一行透明度：勾选行——未勾 = 跟随全局 `badge_alpha`（文案随其值实时更新），
/// 勾上后拖动滑块给这一条自己的不透明度（1.0 = 实心 + 挖空，见 [`BadgeRow::alpha_light`]）。
fn alpha_row(
    label: &str,
    over: Signal<bool>,
    value: Signal<f64>,
    follow_text: Signal<String>,
) -> Element {
    let editor = Element::stepper(value, MIN_BADGE_ALPHA as f64, 1.0, ALPHA_STEP).width(90);
    let control = override_control_signal(over, Rc::new(|| true), editor, follow_text, "", 150);
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        // 空标签的勾选框：标签文字在下一列，同 `widgets::setting_row_override` 的排布。
        .child(Element::checkbox("", over).small().fg_role(Role::Text))
        .child(
            Element::label(label)
                .font_size(12.5)
                .fg_role(Role::Text)
                .width(80),
        )
        .child(control)
}

fn labels_of(table: &[(&str, &str)]) -> Vec<String> {
    table.iter().map(|(_, l)| l.to_string()).collect()
}

/// 构建「角标规则」对话框（含二级的新建/编辑表单）。
pub fn build_langbar_badges_dialog(ui: &LangBarBadgesUi) -> Element {
    let visible = ui.visible;
    let row_ui = ui.clone();

    let list = Element::col().width_match().padding_xy(16, 2).child(
        Element::reorder_list_signal(ui.rows, move |r: BadgeRow, handle| {
            let idx = row_ui
                .rows
                .get()
                .iter()
                .position(|x| x.uid == r.uid)
                .unwrap_or(0);
            let sig = row_ui.switch_of(r.uid, true);
            let e_ui = row_ui.clone();
            let d_ui = row_ui.clone();
            let title = label_of(STATES, &r.state);
            let ctrl = Element::row()
                .cross(Align::Center)
                .spacing(6)
                .child(color_swatch(&r.color_light))
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
                            crate::widgets::setting_row(&title, Some(&row_desc(&r)), ctrl)
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
        .width(600)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("角标规则")
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(
                "不想被某个状态打扰，关掉那一条即可，颜色和位置都留着。\n\
                 两条规则落在同一个角上时只画靠前的那条，可拖动左侧手柄调整顺序。",
            )
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(3)
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
                    Element::button("添加规则…")
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
        .child(build_badge_form_dialog(ui))
}

/// 二级弹层：新建 / 编辑一条规则。
fn build_badge_form_dialog(ui: &LangBarBadgesUi) -> Element {
    let form = ui.form.clone();
    let fc = form.clone();
    let ui2 = ui.clone();
    let visible = form.visible;
    let same = form.same_color;

    // 「跟随全局（0.88）」文案随全局 `badge_alpha` 实时变化——浅、深两行共用一个信号。
    let badge_alpha_sig = form.badge_alpha;
    let follow_alpha_text = signal(format!("跟随全局（{:.2}）", badge_alpha_sig.get()));
    let watch = crate::pages::dict::reactive::reactive_effect(
        move || badge_alpha_sig.version(),
        move || follow_alpha_text.set(format!("跟随全局（{:.2}）", badge_alpha_sig.get())),
    );

    let frame = Element::col()
        .width(540)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(watch)
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
                "点色块选色，或点「与主字同色」跟主字保持一致。\n\
                 透明度默认跟随全局；勾上后是这一条自己的，调到 100% 会变实心，\n\
                 并挖掉角标周围一圈主字。",
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
                .child(form_row(
                    "什么状态",
                    Element::dropdown(labels_of(STATES), form.state_idx).min_width(160),
                ))
                .child(form_row(
                    "画在哪个角",
                    Element::dropdown(labels_of(CORNERS), form.corner_idx).min_width(160),
                ))
                .child(form_row(
                    "亮暗同色",
                    Element::checkbox("", form.same_color).fg_role(Role::Text),
                ))
                .child(color_row(
                    "浅色任务栏",
                    &form.color_light,
                    fallback_color(0, None),
                ))
                .child(alpha_row(
                    "浅色不透明度",
                    form.alpha_light_over,
                    form.alpha_light,
                    follow_alpha_text,
                ))
                .child(
                    color_row("深色任务栏", &form.color_dark, fallback_color(1, None))
                        // 勾了「亮暗同色」就置灰：留着可编辑而值会被浅色覆盖，
                        // 是最容易让人以为「填了没生效」的一种。
                        .enabled_when(move || !same.get()),
                )
                .child(alpha_row(
                    "深色不透明度",
                    form.alpha_dark_over,
                    form.alpha_dark,
                    follow_alpha_text,
                ))
                .child(form_row(
                    "大小倍率",
                    Element::stepper(form.scale, SCALE_STEP, MAX_SCALE, SCALE_STEP).width(160),
                )),
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
                    let light = normalize_color(&fc.color_light.value.get());
                    // 勾了同色就以浅色为准，不去读深色那个框——它此刻是灰的，
                    // 里面留着的是上一次编辑的残值。
                    let dark = if fc.same_color.get() {
                        light.clone()
                    } else {
                        normalize_color(&fc.color_dark.value.get())
                    };
                    let scale = match validate(&light, &dark, fc.scale.get()) {
                        Ok(v) => v,
                        Err(msg) => {
                            fc.error.set(msg.to_string());
                            return;
                        }
                    };
                    fc.error.set(String::new());
                    let state = STATES
                        .get(fc.state_idx.get())
                        .map(|(k, _)| k.to_string())
                        .unwrap_or_default();
                    let corner = CORNERS
                        .get(fc.corner_idx.get())
                        .map(|(k, _)| k.to_string())
                        .unwrap_or_else(|| DEFAULT_CORNER.to_string());
                    let alpha_light = fc
                        .alpha_light_over
                        .get()
                        .then(|| fc.alpha_light.get() as f32);
                    let alpha_dark = fc.alpha_dark_over.get().then(|| fc.alpha_dark.get() as f32);
                    ui2.commit_form(
                        fc.editing.get(),
                        state,
                        corner,
                        light,
                        dark,
                        alpha_light,
                        alpha_dark,
                        scale,
                    );
                    fc.visible.set(false);
                })),
        );
    Element::dialog(visible, frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_rows() -> Vec<Value> {
        vec![
            json!({"enabled": true, "state": "punct_cn", "corner": "bottom_right",
                   "color_light": "#2288E0", "color_dark": "#2288E0", "scale": 1.0}),
            json!({"enabled": false, "state": "full_width", "corner": "top_right",
                   "color_light": "#E0447A", "color_dark": "#E0447A", "scale": 1.0}),
        ]
    }

    /// 界面上的 id 必须与 core 的枚举逐字一致——它们是跨仓的字符串契约，
    /// 拼错一个字母的表现是「设置页选了、装到任务栏上没反应」，两边都不报错。
    #[test]
    fn ids_match_core_spelling() {
        let states: Vec<&str> = STATES.iter().map(|(k, _)| *k).collect();
        assert_eq!(states, ["punct_cn", "punct_en", "full_width"]);
        let corners: Vec<&str> = CORNERS.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            corners,
            ["top_left", "top_right", "bottom_right", "bottom_left"]
        );
    }

    /// 一个完整来回：读进来 → 原样写回去，条目与启用位都不能变形。
    #[test]
    fn round_trip_preserves_rows_and_switches() {
        let items = cfg_rows();
        let rows = rows_from_value(&items);
        let flags = enabled_flags(&items);
        assert_eq!(rows.len(), 2);
        assert_eq!(flags, vec![true, false]);
        assert_eq!(rows[1].corner, "top_right");
        assert_eq!(rows[1].scale, 1.0);
        assert_eq!((rows[1].alpha_light, rows[1].alpha_dark), (None, None));

        let back: Vec<Value> = rows
            .iter()
            .zip(&flags)
            .map(|(r, on)| row_to_value(r, *on))
            .collect();
        for (a, b) in items.iter().zip(&back) {
            for k in ["enabled", "state", "corner", "color_light", "color_dark"] {
                assert_eq!(a.get(k), b.get(k), "字段 {k} 在来回后变了");
            }
            assert_eq!(
                b.get("alpha_light"),
                None,
                "跟随全局不该写出 alpha_light 键"
            );
        }
    }

    /// 本屏不认识的字段必须原样留着——core 加字段时不该被这一屏静默吃掉。
    #[test]
    fn unknown_fields_survive_a_round_trip() {
        let items = vec![json!({"state": "punct_cn", "corner": "top_left", "blink": true})];
        let rows = rows_from_value(&items);
        let back = row_to_value(&rows[0], true);
        assert_eq!(back.get("blink"), Some(&json!(true)), "未知字段被吃掉了");
    }

    /// 状态不认识的条目要**留在列表里**，不能悄悄丢——丢了就会在保存时被抹掉。
    #[test]
    fn unknown_state_is_kept_not_dropped() {
        let items = vec![json!({"state": "half_width", "corner": "top_left"})];
        let rows = rows_from_value(&items);
        assert_eq!(rows.len(), 1, "不认识的状态被丢弃了");
        assert_eq!(rows[0].state, "half_width", "不认识的状态被改写了");
        // 界面名回落成原值，而不是显示成三个已知状态之一。
        assert_eq!(label_of(STATES, &rows[0].state), "half_width");
    }

    /// 缺字段的条目按 core 的默认值补齐，不能变成空串或 0 倍率。
    #[test]
    fn missing_fields_fall_back_to_core_defaults() {
        let rows = rows_from_value(&[json!({"state": "punct_en"})]);
        assert_eq!(rows[0].corner, "bottom_right");
        assert_eq!(rows[0].color_light, "");
        assert_eq!(rows[0].color_dark, "");
        assert_eq!(rows[0].alpha_light, None);
        assert_eq!(rows[0].alpha_dark, None);
        assert_eq!(rows[0].scale, 1.0);
    }

    /// 旧写法 `auto`（不分大小写）读进来要视为新写法的 `""`（与主字同色），
    /// 不带出任何透明度。
    #[test]
    fn legacy_auto_color_reads_as_follow() {
        let items = vec![json!({"state": "punct_cn", "color_light": "auto", "color_dark": "AUTO"})];
        let rows = rows_from_value(&items);
        assert_eq!(rows[0].color_light, "");
        assert_eq!(rows[0].color_dark, "");
        assert_eq!((rows[0].alpha_light, rows[0].alpha_dark), (None, None));
    }

    /// 旧写法八位色值：末两位拆成独立的 `alpha_x`，颜色本身归一成六位。
    /// 浅、深两侧的透明度不同时，各自保留。
    #[test]
    fn legacy_rgba8_color_splits_into_color_and_alpha() {
        let items = vec![
            json!({"state": "punct_cn", "color_light": "#2288E080", "color_dark": "#2288E0FF"}),
        ];
        let rows = rows_from_value(&items);
        assert_eq!(rows[0].color_light, "#2288E0");
        assert_eq!(rows[0].color_dark, "#2288E0");
        assert!((rows[0].alpha_light.unwrap() - 128.0 / 255.0).abs() < 1e-3);
        assert_eq!(rows[0].alpha_dark, Some(1.0));
    }

    /// 旧写法的 `#` 可省——手写配置里两种写法都见过，与 core 的 `parse_hex` 认定一致。
    #[test]
    fn legacy_rgba8_color_without_hash_prefix_also_splits() {
        let items = vec![json!({"state": "punct_cn", "color_light": "2288E080"})];
        let rows = rows_from_value(&items);
        assert_eq!(rows[0].color_light, "#2288E0");
        assert!((rows[0].alpha_light.unwrap() - 128.0 / 255.0).abs() < 1e-3);
    }

    /// ★ 读入不是写出：新写法的六位色（不拘大小写）原样穿过去，不改大小写——
    /// 脏检查比的是写回值与原始配置逐字是否相同，读进来就改大小写，手写小写色的
    /// 存量配置一打开就会被误判成「有未保存的改动」。
    #[test]
    fn lowercase_six_digit_color_round_trips_verbatim() {
        let items =
            vec![json!({"state": "punct_cn", "color_light": "#2288e0", "color_dark": "#2288e0"})];
        let rows = rows_from_value(&items);
        assert_eq!(rows[0].color_light, "#2288e0", "读入阶段不该改写大小写");
        let back = row_to_value(&rows[0], true);
        assert_eq!(back.get("color_light"), Some(&json!("#2288e0")));
    }

    /// 手写配置里的越界透明度（负数 / 低于全局滑块下限 / 大于 1）读入时钳到 0.3..=1，
    /// 与全局滑块、表单 stepper 同一范围。
    #[test]
    fn explicit_alpha_out_of_range_is_clamped() {
        let items = vec![
            json!({"state": "punct_cn", "alpha_light": -0.5, "alpha_dark": 1.5}),
            json!({"state": "punct_en", "alpha_light": 0.1, "alpha_dark": 0.3}),
        ];
        let rows = rows_from_value(&items);
        assert_eq!(rows[0].alpha_light, Some(0.3));
        assert_eq!(rows[0].alpha_dark, Some(1.0));
        assert_eq!(rows[1].alpha_light, Some(0.3), "低于下限的也钳到 0.3");
        assert_eq!(rows[1].alpha_dark, Some(0.3), "恰为下限原样通过");
    }

    /// 旧写法末两位很小（`#…10` ≈ 0.06）时同样钳到下限，不绕过读入钳制。
    #[test]
    fn legacy_rgba8_low_alpha_is_clamped_to_min() {
        let items = vec![json!({"state": "punct_cn", "color_light": "#2288E010"})];
        let rows = rows_from_value(&items);
        assert_eq!(rows[0].alpha_light, Some(0.3));
    }

    /// 新键（`alpha_x`）比旧色值末两位更可信：两者都出现时以新键为准。
    #[test]
    fn explicit_alpha_key_overrides_legacy_suffix() {
        let items =
            vec![json!({"state": "punct_cn", "color_light": "#2288E080", "alpha_light": 0.9})];
        let rows = rows_from_value(&items);
        assert_eq!(rows[0].color_light, "#2288E0");
        assert_eq!(rows[0].alpha_light, Some(0.9));
    }

    #[test]
    fn factory_badges_have_alpha_none_and_no_legacy_color() {
        // 出厂三条不在这一屏，但这一屏解析同形态的输入应得到同样干净的结果。
        let items =
            vec![json!({"state": "punct_cn", "color_light": "#2288E0", "color_dark": "#2288E0"})];
        for b in rows_from_value(&items) {
            assert!(b.color_light != "auto" && b.color_dark != "auto");
            assert_eq!((b.alpha_light, b.alpha_dark), (None, None));
        }
    }

    #[test]
    fn color_validation_accepts_empty_and_six_digit_hex() {
        assert!(color_is_valid(""));
        assert!(color_is_valid("  "));
        assert!(color_is_valid("#2288E0"));
        assert!(color_is_valid("  #2288e0 "));
        assert!(color_is_valid("2288E0"));
        // 八位写法（旧版内嵌透明度）不再是这一维的合法值——透明度走独立字段。
        assert!(!color_is_valid("#2288E0CC"));
        assert!(!color_is_valid("#28E"));
        assert!(!color_is_valid("#2288EZ"));
    }

    #[test]
    fn scale_validation_rejects_zero_and_out_of_range() {
        assert_eq!(validate("", "", 1.0), Ok(1.0));
        assert_eq!(validate("#2288E0", "#EE9922", 0.8), Ok(0.8));
        assert!(validate("", "", 0.0).is_err());
        assert!(validate("", "", -1.0).is_err());
        assert!(validate("", "", 9.0).is_err());
        assert!(validate("#28E", "", 1.0).is_err());
        assert!(validate("", "#2288E0CC", 1.0).is_err());
    }

    /// 倍率显示不带多余的零：输入框里一片 `1.00` 会让人以为这是个高精度参数。
    #[test]
    fn scale_is_formatted_without_trailing_zeros() {
        assert_eq!(fmt_scale(1.0), "1");
        assert_eq!(fmt_scale(0.82), "0.82");
        assert_eq!(fmt_scale(0.5), "0.5");
    }

    /// 副标题在亮暗同色时只写一遍颜色，不同色时两个都写。
    #[test]
    fn row_desc_collapses_identical_colors() {
        let mut r = rows_from_value(&cfg_rows())[0].clone();
        assert_eq!(r.color_light, r.color_dark);
        assert_eq!(r.color_light, "#2288E0");
        assert!(!row_desc(&r).contains('/'), "同色时不该出现「浅/深」两段");
        r.color_dark = "#EE9922".to_string();
        assert!(row_desc(&r).contains('/'), "异色时必须两个都写出来");
        // 倍率 1 是常态，不占一段。
        assert!(!row_desc(&r).contains('×'));
        r.scale = 0.8;
        assert!(row_desc(&r).contains("×0.8"));
    }

    /// 与主字同色在界面上说人话，不直接把内部哨兵甩给用户。
    #[test]
    fn follow_color_reads_as_plain_chinese() {
        let r = rows_from_value(&[json!({"state": "punct_cn"})]);
        assert!(row_desc(&r[0]).contains("与主字同色"));
    }

    /// 编辑一条不换 uid，否则那条的启用态会退回默认——用户改了个颜色，
    /// 结果关掉的规则自己开了。同注释词库那屏「改 id 要把开关一起改名」的教训。
    #[test]
    fn editing_a_row_keeps_its_uid_and_switch() {
        let ui = LangBarBadgesUi::new();
        ui.seed(&cfg_rows());
        let uid = ui.rows.get()[1].uid;
        let sw = ui.switch_of(uid, true);
        assert!(!sw.get(), "第二条出厂就是关着的");

        ui.commit_form(
            1,
            "full_width".into(),
            "top_left".into(),
            "#123456".into(),
            "#123456".into(),
            None,
            None,
            1.0,
        );
        assert_eq!(ui.rows.get()[1].uid, uid, "编辑后 uid 变了");
        assert!(!sw.get(), "编辑后启用态被重置了");

        let out = ui.to_value();
        let arr = out.as_array().expect("数组");
        assert_eq!(arr[1].get("enabled"), Some(&json!(false)));
        assert_eq!(arr[1].get("corner"), Some(&json!("top_left")));
    }

    /// 删了再加不会捡到旧开关：uid 只增不减。
    #[test]
    fn a_new_row_does_not_inherit_a_deleted_rows_switch() {
        let ui = LangBarBadgesUi::new();
        ui.seed(&cfg_rows());
        ui.delete(1); // 那条是关着的
        ui.commit_form(
            NEW_BADGE,
            "full_width".into(),
            "top_right".into(),
            "".into(),
            "".into(),
            None,
            None,
            1.0,
        );
        let arr = ui.to_value();
        let arr = arr.as_array().expect("数组");
        assert_eq!(arr.len(), 2);
        assert_eq!(
            arr[1].get("enabled"),
            Some(&json!(true)),
            "新建的规则捡到了被删那条的关闭态"
        );
    }

    /// 拖拽排序后写回的数组顺序必须跟着变——顺序即优先级，写回丢了就等于没排。
    #[test]
    fn reordering_changes_the_written_order() {
        let ui = LangBarBadgesUi::new();
        ui.seed(&cfg_rows());
        ui.rows.update(|v| {
            let x = v.remove(0);
            v.push(x);
        });
        let arr = ui.to_value();
        let arr = arr.as_array().expect("数组");
        assert_eq!(arr[0].get("state"), Some(&json!("full_width")));
        assert_eq!(arr[1].get("state"), Some(&json!("punct_cn")));
        // 启用位跟着各自的行走，不是按位置固定的。
        assert_eq!(arr[0].get("enabled"), Some(&json!(false)));
        assert_eq!(arr[1].get("enabled"), Some(&json!(true)));
    }

    /// 色号一律归一成大写六位，免得同一个颜色在列表里长得两样。
    #[test]
    fn colors_are_normalized() {
        assert_eq!(normalize_color(" #2288e0 "), "#2288E0");
        assert_eq!(normalize_color(""), "");
        assert_eq!(normalize_color("  "), "");
        assert_eq!(normalize_color("2288e0"), "#2288E0");
        // 非法值原样留着交给校验去报错，不在这里悄悄改写。
        assert_eq!(normalize_color("#28E"), "#28E");
    }

    /// 副标题把不透明度换算成百分比——没人能一眼看出 `0.8` 是多淡；
    /// 两侧都跟随全局是常态，不该凭空显示百分比。
    #[test]
    fn subtitle_spells_out_the_alpha_as_a_percentage() {
        let mut r = rows_from_value(&cfg_rows())[0].clone();
        assert!(
            !row_desc(&r).contains('%'),
            "跟随全局透明度时不该凭空显示百分比"
        );
        r.alpha_light = Some(0.8);
        r.alpha_dark = Some(0.8);
        assert!(row_desc(&r).contains("80%"), "0.8 应显示成 80%");
    }

    /// 透明度钉到 1.0 是「实心 + 挖空」档，副标题要点出来，不能只显示「100%」
    /// 让人以为只是普通的浅透明。
    #[test]
    fn subtitle_marks_alpha_one_as_solid_cutout() {
        let mut r = rows_from_value(&cfg_rows())[0].clone();
        r.alpha_light = Some(1.0);
        r.alpha_dark = Some(1.0);
        assert!(row_desc(&r).contains("挖空"));
    }

    /// 颜色这一维的「跟随」恒是空串——即便 `color` 参数本身传了别的值。
    #[test]
    fn badge_color_follow_is_empty_string() {
        assert_eq!(badge_color_value(true, "#112233"), json!(""));
        assert_eq!(badge_color_value(false, "#112233"), json!("#112233"));
    }

    /// 透明度勾选行未勾时写 `Value::Null`（调用方据此删键，不是写字面 null）。
    #[test]
    fn badge_alpha_unchecked_writes_null() {
        assert_eq!(badge_alpha_write(false, 0.5), Value::Null);
        assert_eq!(badge_alpha_write(true, 0.5), json!(0.5));
    }

    /// `row_to_value` 遇到未勾的一侧要把 `alpha_x` 键**删掉**，不是写 null——
    /// TOML 没有 null，且 raw 里可能残留着上次勾上过时插的旧值。
    #[test]
    fn row_to_value_omits_alpha_key_when_none_and_writes_when_some() {
        let mut r = rows_from_value(&cfg_rows())[0].clone();
        r.alpha_light = Some(0.5);
        r.alpha_dark = None;
        let v = row_to_value(&r, true);
        assert_eq!(v.get("alpha_light"), Some(&json!(0.5)));
        assert_eq!(v.get("alpha_dark"), None, "未勾的一侧不写 alpha_dark 键");
    }

    /// 编辑时把某一侧的透明度从「自己的」改回「跟随全局」，raw 里残留的旧键要
    /// 被清掉，不能悄悄复活成上次的数值。
    #[test]
    fn row_to_value_clears_stale_alpha_key_when_reverted_to_follow() {
        let mut r = rows_from_value(&[json!({"state": "punct_cn", "alpha_light": 0.7})])[0].clone();
        assert_eq!(r.alpha_light, Some(0.7));
        r.alpha_light = None;
        let v = row_to_value(&r, true);
        assert_eq!(v.get("alpha_light"), None);
    }

    /// 「与主字同色」按钮写回 `""`，取色器显示主字回落色；该值不会被同步成色号。
    #[test]
    fn follow_button_writes_empty_and_is_not_synced_back() {
        let f = BadgeForm::new();
        set_badge_color(&f.color_dark, "", fallback_color(1, None));
        assert_eq!(f.color_dark.value.get(), "");
        assert_eq!(f.color_dark.color.get(), Color::hex(0xFFFFFF));
        f.color_dark.sync(badge_value_of);
        assert_eq!(f.color_dark.value.get(), "", "与主字同色被回写成了色号");
        assert_eq!(badge_color_text(""), "与主字同色");
        // 从「与主字同色」选一个具体色：写回色号。
        f.color_dark.color.set(Color::hex(0x336699));
        f.color_dark.sync(badge_value_of);
        assert_eq!(f.color_dark.value.get(), "#336699");
    }

    /// 编辑已有规则时程序播种的颜色 / 透明度不回写；`same_color`、透明度勾选状态
    /// 都按行的实际数据来判。
    #[test]
    fn open_edit_seeds_without_writing_back() {
        let f = BadgeForm::new();
        let mut r = rows_from_value(&cfg_rows())[0].clone();
        r.color_light = "#2288E0".into();
        r.color_dark = "#EE9922".into();
        r.alpha_light = Some(1.0);
        r.alpha_dark = None;
        f.open_edit(0, &r);
        f.color_light.sync(badge_value_of);
        f.color_dark.sync(badge_value_of);
        assert_eq!(f.color_light.value.get(), "#2288E0");
        assert_eq!(f.color_dark.value.get(), "#EE9922");
        assert!(!f.same_color.get(), "浅深不同色应判为不同");
        assert!(f.alpha_light_over.get(), "带自己透明度的一侧应勾上");
        assert_eq!(f.alpha_light.get(), 1.0);
        assert!(!f.alpha_dark_over.get(), "跟随全局的一侧不该勾上");
    }

    /// 未勾透明度时，勾选瞬间控件里应已经是全局当前值（同 `theme_follow` 那套
    /// 「勾选瞬间必须已是跟随值」的裁决），不是某个固定占位。
    #[test]
    fn open_new_seeds_alpha_with_current_global_value() {
        let f = BadgeForm::new();
        f.badge_alpha.set(0.42);
        f.open_new();
        assert!(!f.alpha_light_over.get());
        assert_eq!(f.alpha_light.get(), 0.42_f32 as f64);
        assert!(!f.alpha_dark_over.get());
        assert_eq!(f.alpha_dark.get(), 0.42_f32 as f64);
    }

    /// T9：同页先拖全局滑块、再开表单，未勾时的跟随值与勾上瞬间的初值都要是滑块
    /// **当前编辑中**的值，不是加载时灌进来的旧值。新建、编辑两个入口都要取。
    #[test]
    fn open_pulls_badge_alpha_from_live_source() {
        let f = BadgeForm::new();
        f.badge_alpha.set(0.88);
        let live = Rc::new(Cell::new(0.5_f32));
        let l = live.clone();
        *f.badge_alpha_source.borrow_mut() = Some(Box::new(move || Some(l.get())));

        f.open_new();
        assert_eq!(f.badge_alpha.get(), 0.5);
        assert_eq!(f.alpha_light.get(), 0.5_f32 as f64);

        live.set(0.6);
        let mut r = rows_from_value(&cfg_rows())[0].clone();
        r.alpha_light = None;
        r.alpha_dark = None;
        f.open_edit(0, &r);
        assert_eq!(f.badge_alpha.get(), 0.6);
        assert_eq!(f.alpha_dark.get(), 0.6_f32 as f64);
    }

    /// 取不到滑块值（源返回 `None`）时保留原值，不能把它冲成某个占位。
    #[test]
    fn open_keeps_badge_alpha_when_source_unavailable() {
        let f = BadgeForm::new();
        f.badge_alpha.set(0.7);
        *f.badge_alpha_source.borrow_mut() = Some(Box::new(|| None));
        f.open_new();
        assert_eq!(f.badge_alpha.get(), 0.7);
    }

    /// ★ 颜色格的取色面板必须关掉透明度条——这条钉住 `badge_value_of` 那侧的前提：
    /// 一旦面板哪天又被打开（`color_row` 传 `alpha: true`），用户拖出的半透明色传进来，
    /// 这里应该立刻炸而不是悄悄丢掉，免得用户以为调的透明度生效了。
    #[test]
    #[should_panic(expected = "alpha")]
    fn badge_value_of_rejects_translucent_input() {
        badge_value_of("", Color::rgba(0x11, 0x22, 0x33, 0x80));
    }

    /// 用户恰好选中回落色（跟随主字时显示的黑）也照常写回，不当成「没选」。
    #[test]
    fn user_pick_equal_to_fallback_is_written() {
        let f = BadgeForm::new();
        set_badge_color(&f.color_light, "", fallback_color(0, None));
        f.color_light.color.set(fallback_color(0, None));
        f.color_light.sync(badge_value_of);
        assert_eq!(f.color_light.value.get(), "#000000");
    }
}
