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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

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

/// 「与主字同色」的色值标记，与 core 的 `color_light = "auto"` 同义。
pub const AUTO_COLOR: &str = "auto";

/// 未知角落的回落目标，与 core 的 `Corner::from_id` 一致。
const DEFAULT_CORNER: &str = "bottom_right";

/// 大小倍率的步进与上限。步进与设置页那个全局倍率滑块保持一致——同一个量在两处
/// 用不同的粒度调，用户会以为其中一处"调不准"。
const SCALE_STEP: f64 = 0.1;
const MAX_SCALE: f64 = 3.0;

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
    /// 浅色任务栏上的颜色：`#RRGGBB` 或 [`AUTO_COLOR`]。
    pub color_light: String,
    /// 深色任务栏上的颜色，同上。
    pub color_dark: String,
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
    pub color_light: Signal<String>,
    pub color_dark: Signal<String>,
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
            color_light: signal("#2288E0".to_string()),
            color_dark: signal("#2288E0".to_string()),
            scale: signal(1.0),
            error: signal(String::new()),
            title: signal("添加角标规则".to_string()),
        }
    }

    pub fn open_new(&self) {
        self.editing.set(NEW_BADGE);
        self.state_idx.set(0);
        self.corner_idx.set(index_of(CORNERS, DEFAULT_CORNER));
        self.same_color.set(true);
        self.color_light.set("#2288E0".to_string());
        self.color_dark.set("#2288E0".to_string());
        self.scale.set(1.0);
        self.error.set(String::new());
        self.title.set("添加角标规则".to_string());
        self.visible.set(true);
    }

    pub fn open_edit(&self, index: usize, row: &BadgeRow) {
        self.editing.set(index);
        self.state_idx.set(index_of(STATES, &row.state));
        self.corner_idx.set(index_of(CORNERS, &row.corner));
        self.same_color
            .set(row.color_light.eq_ignore_ascii_case(&row.color_dark));
        self.color_light.set(row.color_light.clone());
        self.color_dark.set(row.color_dark.clone());
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

/// 解析配置数组 → 条目定义（纯函数，供测试）。`uid` 由调用方补，这里恒填 0。
///
/// ★ 不认识的状态 / 角落**原样留着**而不是丢弃或改写：core 那边状态不认识会丢掉整条
/// （并记警告），但那是 core 的事；这一屏若也丢弃，用户会看到「我明明配了三条，界面
/// 里只有两条」，而那两条一保存就把第三条抹掉了。同注释词库那屏的判据。
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
            BadgeRow {
                uid: 0,
                state: s("state", ""),
                corner: s("corner", DEFAULT_CORNER),
                color_light: s("color_light", AUTO_COLOR),
                color_dark: s("color_dark", AUTO_COLOR),
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

/// 一条 → JSON。在 `raw` 上 insert，本 UI 不管的字段原样保留（见模块文档）。
pub fn row_to_value(r: &BadgeRow, enabled: bool) -> Value {
    let mut v = r.raw.clone();
    if !v.is_object() {
        v = json!({});
    }
    let obj = v.as_object_mut().expect("上面刚保证是对象");
    obj.insert("enabled".into(), json!(enabled));
    obj.insert("state".into(), json!(r.state));
    obj.insert("corner".into(), json!(r.corner));
    obj.insert("color_light".into(), json!(r.color_light));
    obj.insert("color_dark".into(), json!(r.color_dark));
    obj.insert("scale".into(), json!(r.scale));
    v
}

/// 色值文本 → `(RGB, 不透明度)`。`auto` 与非法值返回 `None`。
///
/// 第二项 `None` = **没写末两位**，这一条的不透明度跟随全局 `badge_alpha`。
/// 这个区别必须在这里就分出来：写了 `FF` 与没写不是一回事，前者会把这一条切到
/// 挖空档（角标实心 + 周围切掉一圈主字），core 侧的判据同样是原串长度。
///
/// 只认 `#RRGGBB` / `#RRGGBBAA` 两种长度，与 core 的 `parse_hex` 逐字对齐：这里放宽
/// （比如认 `#RGB` 短写）会让用户在设置页看着好好的、装到任务栏上却退回了 auto。
pub fn parse_color(s: &str) -> Option<(u32, Option<u8>)> {
    let hex = s.trim().strip_prefix('#')?;
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok().map(|v| (v, None)),
        8 => Some((
            u32::from_str_radix(&hex[0..6], 16).ok()?,
            Some(u8::from_str_radix(&hex[6..8], 16).ok()?),
        )),
        _ => None,
    }
}

/// 一个色值输入是否合法：`auto`、`#RRGGBB` 或 `#RRGGBBAA`。
pub fn color_is_valid(s: &str) -> bool {
    s.trim().eq_ignore_ascii_case(AUTO_COLOR) || parse_color(s).is_some()
}

/// 归一化色值文本：`auto` 一律小写，十六进制一律大写，免得同一个色在列表里长得两样。
///
/// **不补也不删末两位**：补上等于替用户把这一条钉死成某个不透明度，删掉等于把他刚
/// 配的透明度吃掉。长度本身是有含义的一位信息（见 [`parse_color`]）。
pub fn normalize_color(s: &str) -> String {
    let t = s.trim();
    if t.eq_ignore_ascii_case(AUTO_COLOR) {
        return AUTO_COLOR.to_string();
    }
    match parse_color(t) {
        Some((rgb, None)) => format!("#{rgb:06X}"),
        Some((rgb, Some(a))) => format!("#{rgb:06X}{a:02X}"),
        None => t.to_string(),
    }
}

/// 校验表单，返回解析出的倍率。纯函数，供测试。
///
/// 倍率来自 stepper（值域由控件保证），这里仍复核一遍：控件的 min/max 是 UI 约定，
/// 而写回的是配置文件，值域该由写侧自己守住。
pub fn validate(color_light: &str, color_dark: &str, scale: f64) -> Result<f32, &'static str> {
    if !color_is_valid(color_light) {
        return Err("浅色任务栏的颜色要填 #RRGGBB、#RRGGBBAA 或 auto");
    }
    if !color_is_valid(color_dark) {
        return Err("深色任务栏的颜色要填 #RRGGBB、#RRGGBBAA 或 auto");
    }
    if !scale.is_finite() || scale <= 0.0 {
        return Err("大小倍率要大于 0");
    }
    if scale > MAX_SCALE {
        return Err("大小倍率最大 3");
    }
    Ok(scale as f32)
}

/// 一条的副标题：位置 · 颜色 · 倍率。亮暗同色时只写一个，免得一行里两遍同一个色号。
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
    // 倍率 1 是常态，不占一段——只在真被调过时才提。
    if (r.scale - 1.0).abs() > f32::EPSILON {
        parts.push(format!("×{}", fmt_scale(r.scale)));
    }
    parts.join(" · ")
}

fn color_text(s: &str) -> String {
    let t = s.trim();
    if t.eq_ignore_ascii_case(AUTO_COLOR) {
        return "与主字同色".to_string();
    }
    // 末两位换算成百分比：没人能一眼看出 CC 是多淡，而这正是它存在的意义。
    match parse_color(t) {
        Some((rgb, Some(a))) => {
            format!("#{rgb:06X} {}%", (a as f32 / 2.55).round() as i32)
        }
        _ => t.to_uppercase(),
    }
}

/// 一小块色样。`auto` 与非法值画成描边空块——颜色是这一屏最该一眼看清的东西。
///
/// **按色值自带的不透明度半透明绘制**：透明度是这次新加的自由度，只写在副标题里
/// 用户得自己把 `CC` 换算成百分比才知道多淡，而色样直接把结果摆出来。没写末两位的
/// 画成不透明——那种情况的实际淡度由全局值决定，这一屏没有权威可依。
fn swatch(color: &str) -> Element {
    let base = Element::leaf().width(14).height(14).corner(3.0);
    match parse_color(color) {
        Some((v, a)) => base.bg(Color::rgba(
            ((v >> 16) & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
            a.unwrap_or(0xFF),
        )),
        None => base.border_role(Role::Border, 1),
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

/// 表单里的文本输入行。
fn field_row(label: &str, sig: Signal<String>, placeholder: &str) -> Element {
    form_row(
        label,
        Element::text_input(sig, placeholder)
            .width_match()
            .height(32)
            .weight(1.0),
    )
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
                .child(swatch(&r.color_light))
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

    let frame = Element::col()
        .width(500)
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
                "颜色填 #RRGGBB，或 auto = 与主字同色；再补两位就是这一条自己的透明度。\n\
                 不补两位用全局透明度；补成 FF 会变实心，并挖掉角标周围一圈主字。",
            )
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(3)
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
                .child(field_row(
                    "浅色任务栏",
                    form.color_light,
                    "#2288E0 或 #2288E0CC",
                ))
                .child(
                    field_row("深色任务栏", form.color_dark, "#2288E0 或 #2288E0CC")
                        // 勾了「亮暗同色」就置灰：留着可编辑而值会被浅色覆盖，
                        // 是最容易让人以为「填了没生效」的一种。
                        .enabled_when(move || !same.get()),
                )
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
                    let light = normalize_color(&fc.color_light.get());
                    // 勾了同色就以浅色为准，不去读深色那个框——它此刻是灰的，
                    // 里面留着的是上一次编辑的残值。
                    let dark = if fc.same_color.get() {
                        light.clone()
                    } else {
                        normalize_color(&fc.color_dark.get())
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
                    ui2.commit_form(fc.editing.get(), state, corner, light, dark, scale);
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

        let back: Vec<Value> = rows
            .iter()
            .zip(&flags)
            .map(|(r, on)| row_to_value(r, *on))
            .collect();
        for (a, b) in items.iter().zip(&back) {
            for k in ["enabled", "state", "corner", "color_light", "color_dark"] {
                assert_eq!(a.get(k), b.get(k), "字段 {k} 在来回后变了");
            }
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
        assert_eq!(rows[0].color_light, AUTO_COLOR);
        assert_eq!(rows[0].color_dark, AUTO_COLOR);
        assert_eq!(rows[0].scale, 1.0);
    }

    #[test]
    fn color_validation_accepts_auto_and_six_or_eight_digit_hex() {
        assert!(color_is_valid("auto"));
        assert!(color_is_valid("AUTO"));
        assert!(color_is_valid("#2288E0"));
        assert!(color_is_valid("  #2288e0 "));
        assert!(color_is_valid("#2288E0CC"), "带透明度的八位写法应放行");
        // `#RGB` / `#RGBA` 短写在 core 侧解析不了，这里放行会得到「设置页看着好好的、
        // 装上去却退回了 auto」。
        assert!(!color_is_valid("#28E"));
        assert!(!color_is_valid("#28EF"));
        assert!(!color_is_valid("2288E0"));
        assert!(!color_is_valid("#2288EZ"));
        assert!(!color_is_valid("#2288E0CCC"));
        assert!(!color_is_valid(""));
    }

    /// ★ 「没写末两位」与「写了 FF」必须分得开。
    ///
    /// 前者 = 跟随全局透明度，后者 = 这一条自己钉成实心（core 侧还会因此切到挖空档）。
    /// 若解析时把六位补成 `Some(255)`，两者在配置里就再也区分不出来了。
    #[test]
    fn six_digit_means_follow_global_eight_digit_means_override() {
        assert_eq!(parse_color("#2288E0"), Some((0x2288E0, None)));
        assert_eq!(parse_color("#2288E0FF"), Some((0x2288E0, Some(0xFF))));
        assert_eq!(parse_color("#2288E0CC"), Some((0x2288E0, Some(0xCC))));
        assert_eq!(parse_color("auto"), None);
    }

    #[test]
    fn scale_validation_rejects_zero_and_out_of_range() {
        assert_eq!(validate("auto", "auto", 1.0), Ok(1.0));
        assert_eq!(validate("#2288E0", "#EE9922", 0.8), Ok(0.8));
        assert!(validate("auto", "auto", 0.0).is_err());
        assert!(validate("auto", "auto", -1.0).is_err());
        assert!(validate("auto", "auto", 9.0).is_err());
        assert!(validate("#28E", "auto", 1.0).is_err());
        assert!(validate("auto", "#2288E0CCC", 1.0).is_err());
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

    /// `auto` 在界面上说人话，不直接把关键字甩给用户。
    #[test]
    fn auto_color_reads_as_plain_chinese() {
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
            "auto".into(),
            "auto".into(),
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
        assert_eq!(normalize_color("AUTO"), "auto");
        // ★ 长度是有含义的一位信息：既不给六位补上 FF，也不把八位的末两位删掉。
        assert_eq!(normalize_color("#2288e0"), "#2288E0");
        assert_eq!(normalize_color("#2288e0cc"), "#2288E0CC");
        // 非法值原样留着交给校验去报错，不在这里悄悄改写。
        assert_eq!(normalize_color("#28E"), "#28E");
    }

    /// 副标题把末两位换算成百分比——没人能一眼看出 `CC` 是多淡。
    #[test]
    fn subtitle_spells_out_the_alpha_as_a_percentage() {
        let mut r = rows_from_value(&cfg_rows())[0].clone();
        assert!(
            !row_desc(&r).contains('%'),
            "没写透明度时不该凭空显示百分比"
        );
        r.color_light = "#2288E0CC".to_string();
        r.color_dark = "#2288E0CC".to_string();
        assert!(row_desc(&r).contains("80%"), "0xCC 应显示成 80%");
    }
}
