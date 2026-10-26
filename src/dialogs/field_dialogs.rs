//! dialog_button 各定制对话框主体（按 key 注册）。

use serde_json::{Map, Value};
use std::rc::Rc;
use windui::prelude::*;

/// strlist → 空格分隔字符串（对话框文本框展示）。
pub fn strlist_to_spaced(items: &[String]) -> String {
    items.join(" ")
}

/// 空格分隔字符串 → strlist（去空白、跳过空项）。
pub fn spaced_to_strlist(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

/// 空格分隔文本编辑对话框：提示 + text_input + [恢复默认 | 取消 确认]。
/// 确认即写入 `text`（writer 随主保存提交）；取消仅关闭，未确认改动仍保留在 text。
pub fn build_spaced_text_dialog(
    visible: Signal<bool>,
    title: &str,
    hint: &str,
    text: Signal<String>,
    default_text: String,
) -> Element {
    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(hint)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3)
                .padding_xy(18, 10),
        )
        .child(
            // padding 包裹：text_input 用 width_match 填满内边距后的宽，避免 margin 外推溢出对话框。
            Element::col().width_match().padding_xy(18, 6).child(
                Element::text_input(text, "按空格分隔")
                    .multiline()
                    .wrap(true)
                    .width_match()
                    .height(96),
            ),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    Element::button("恢复默认")
                        .small()
                        .outline_soft()
                        .on_click(move |_| text.set(default_text.clone())),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| visible.set(false)),
                )
                .child(
                    Element::button("确认")
                        .small()
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// 单行字符串编辑对话框：提示 + 单行 text_input +（可选附加行）+ [恢复默认 | 取消 确认]。
/// 用于 str 类型的「启用 + 设置」项（如智能标点字符集），需要「恢复默认」故走对话框。
///
/// `extra`：追加在输入框下方的附加行，承载 manifest 里声明了 `dialog_extra_for` 的从属项
/// （如「英文标点智能符号」开关寄居在中文智能符号的对话框里）。`None` = 无附加行，
/// 版式与改造前完全一致。注意「恢复默认」只复位文本框，不动附加项——附加项是独立配置键，
/// 各自有自己的默认值，一并复位会造成「我只想恢复字符集」的误伤。
pub fn build_text_dialog(
    visible: Signal<bool>,
    title: &str,
    hint: &str,
    text: Signal<String>,
    default_text: String,
    placeholder: &str,
    extra: Option<Element>,
) -> Element {
    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(hint)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3)
                .padding_xy(18, 10),
        )
        .child(
            Element::col()
                .width_match()
                .padding_xy(18, 6)
                .child(Element::text_input(text, placeholder).width_match()),
        );
    let frame = match extra {
        Some(e) => frame.child(Element::col().width_match().padding_xy(18, 4).child(e)),
        None => frame,
    };
    let frame = frame.child(Element::divider()).child(
        Element::row()
            .width_match()
            .cross(Align::Center)
            .padding_xy(16, 12)
            .spacing(8)
            .child(
                Element::button("恢复默认")
                    .small()
                    .outline_soft()
                    .on_click(move |_| text.set(default_text.clone())),
            )
            .child(Element::leaf().weight(1.0))
            .child(
                Element::button("取消")
                    .small()
                    .outline_soft()
                    .on_click(move |_| visible.set(false)),
            )
            .child(
                Element::button("确认")
                    .small()
                    .on_click(move |_| visible.set(false)),
            ),
    );
    Element::dialog(visible, frame)
}

/// map → multi-line text（每行一对 `原符号=替换`）。
///
/// 不变量：键不含 `=`——`=` 是行内键值分隔符，含 `=` 的键经 [`rows_to_map`]
/// 回读会被错误截断；故此降级编辑器仅适用于键无 `=` 的映射（标点映射满足）。
pub fn map_to_rows(map: &Map<String, Value>) -> String {
    let mut pairs: Vec<String> = map
        .iter()
        .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or("")))
        .collect();
    pairs.sort(); // deterministic order for tests
    pairs.join("\n")
}

/// multi-line text → map（去空白、跳过无 `=` 行）。
///
/// 不变量：键不含 `=`——按首个 `=` 切分（`split_once`），故键侧出现 `=` 会被
/// 截断到分隔符前；与 [`map_to_rows`] 对称，仅适用于键无 `=` 的映射。
pub fn rows_to_map(text: &str) -> Map<String, Value> {
    let mut m = Map::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            m.insert(k.trim().to_string(), Value::String(v.trim().to_string()));
        }
    }
    m
}

// ─────────────────── 字体相关：**一行一项**的编解码 ───────────────────
//
// ⚠️ 字体名**含空格**（`Microsoft YaHei UI`），所以这两项都不能用通用兜底控件：
// `strlist` 的空格分隔编辑会把一个字体拆成三个；`map` 的通用控件按标量读写
// （`v.as_str()` / `Value::String`），而 `ui.font.scripts` 的值是**字体链数组**——
// 读出来是空串、存回去类型都变了，core 解析不了 ⇒ 打开设置页保存一次，整项配置就没了。
// 故两者各配一套按**行**切分的编解码。

/// `["A", "B"]` → 每行一个字体名。
pub fn font_list_to_rows(v: Option<&Value>) -> String {
    v.and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// 每行一个字体名 → `["A", "B"]`。空行丢弃（顺带容忍尾随换行）。
pub fn rows_to_font_list(text: &str) -> Vec<Value> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| Value::String(l.to_string()))
        .collect()
}

/// `{"latin": ["A","B"]}` → 每行 `latin = A, B`，并把 `known` 里**尚未配置**的类别
/// 以空链预填进去。
///
/// 链内用逗号分隔而不是空格：字体名含空格，空格分不开。
///
/// # 为什么要预填
///
/// 类别名（`latin`/`cjk`/…）是**封闭值域**，用户猜不出来也没处查——空白框加一句提示，
/// 等于要求用户先去翻文档。预填后这个对话框自己就是那份文档：想改哪类填哪行，
/// 留空的行在 [`rows_to_font_scripts`] 里被丢弃，什么都不填就等于没配过。
///
/// `known` 来自 core 的 capability（`ui.font.scripts` 的键名值域），**顺序照抄**——
/// core 侧有测试钉住它与 `ScriptClass::ALL` 同序，这里再排一次序就把那条钉子废了，
/// 且用户每次打开对话框会看到行序变化。`known` 为空（快照缺该字段）时退回「只列已配的」，
/// 不会变成一个空框。
///
/// 用户自己写的、不在 `known` 里的键排在最后（按字典序）：core 会忽略并记一条日志，
/// 但设置页**不能**替 core 把它删掉——那是静默丢用户数据。
pub fn font_scripts_to_rows(map: &Map<String, Value>, known: &[String]) -> String {
    let chain_of = |v: &Value| -> String {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    let mut rows: Vec<String> = known
        .iter()
        .map(|k| {
            let chain = map.get(k).map(&chain_of).unwrap_or_default();
            format!("{k} = {chain}")
        })
        .collect();
    let mut extra: Vec<String> = map
        .iter()
        .filter(|(k, _)| !known.iter().any(|n| n == *k))
        .map(|(k, v)| format!("{k} = {}", chain_of(v)))
        .collect();
    extra.sort(); // 未知键之间顺序稳定，避免每次打开对话框都判为脏
    rows.extend(extra);
    rows.join("\n")
}

/// 每行 `latin = A, B` → `{"latin": ["A","B"]}`。
///
/// 无 `=` 的行、空键、空链一律丢弃：空链在 core 侧会被 `FontPlan::new` 归一化掉，
/// 留着只会让「保存后重开，那一行没了」显得像 bug。
pub fn rows_to_font_scripts(text: &str) -> Map<String, Value> {
    let mut m = Map::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim();
        if key.is_empty() {
            continue;
        }
        let chain: Vec<Value> = v
            .split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(|x| Value::String(x.to_string()))
            .collect();
        if chain.is_empty() {
            continue;
        }
        m.insert(key.to_string(), Value::Array(chain));
    }
    m
}

/// 映射表编辑对话框（降级实现：每行一对 `原符号=替换`）。
/// windui 无动态增删行的 table_editable，故用 multiline text_input 替代。
pub fn build_map_dialog(
    visible: Signal<bool>,
    title: &str,
    hint: &str,
    text: Signal<String>,
    default_text: String,
) -> Element {
    build_text_rows_dialog(
        visible,
        title,
        hint,
        "每行一对，格式：原符号=替换符号",
        text,
        default_text,
    )
}

/// 多行文本编辑对话框的通用外壳。
///
/// `format_hint` 是「每行怎么写」那一句，由调用方给——字体那两项的行格式与标点表不同，
/// 共用外壳但不共用说明。
pub fn build_text_rows_dialog(
    visible: Signal<bool>,
    title: &str,
    hint: &str,
    format_hint: &str,
    text: Signal<String>,
    default_text: String,
) -> Element {
    build_text_rows_dialog_with(
        visible,
        title,
        hint,
        format_hint,
        "，=，",
        text,
        default_text,
    )
}

/// 同上，可指定输入框占位符。
///
/// ⚠️ 抽出这个参数是因为占位符**只有截图看得见**：复用外壳时它会静默继承标点表的
/// `，=，`，而字体那两项的行格式完全不同——用户照着占位符写会得到一份空配置。
/// 测试断言不到（它不进任何数据流），`--screenshot` 自查是唯一发现途径。
#[allow(clippy::too_many_arguments)]
pub fn build_text_rows_dialog_with(
    visible: Signal<bool>,
    title: &str,
    hint: &str,
    format_hint: &str,
    placeholder: &str,
    text: Signal<String>,
    default_text: String,
) -> Element {
    // 输入框高度随**初始**内容长短走。写死 120 时，预填了七个类别的字体对话框一打开
    // 就滚到了末行，第一行 `latin` 在视野外——用户第一眼看不到最常用的那一类。
    // 上限防止将来某个键行数暴涨把对话框顶出屏幕；下限保住空内容时的手感。
    let box_h = {
        // 19px/行是实测值（原先写死 120 时刚好露出 6 行）。+24 给内边距与最后一行留余量。
        const LINE_H: i32 = 19;
        let lines = text.get().lines().count().max(1) as i32;
        (lines * LINE_H + 24).clamp(120, 280)
    };
    let full_hint = if hint.is_empty() {
        format_hint.to_string()
    } else {
        format!(
            "{hint}
{format_hint}"
        )
    };
    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(&full_hint)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(4)
                .padding_xy(18, 10),
        )
        .child(
            Element::col().width_match().padding_xy(18, 6).child(
                Element::text_input(text, placeholder)
                    .multiline()
                    .width_match()
                    .height(box_h),
            ),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    Element::button("恢复默认")
                        .small()
                        .outline_soft()
                        .on_click(move |_| text.set(default_text.clone())),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| visible.set(false)),
                )
                .child(
                    Element::button("确认")
                        .small()
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// 一行的开关，带「至少留 N 个开着」的下限守卫。
///
/// # 为什么是「可点 + toast」而不是置灰
///
/// 初版用 `enabled_when` 把最后几个开关置灰，判据是「置灰当场说明这个关不掉」。
/// **实测被用户推翻**：灰掉的开关与正常开关差别太小、看不出来，点下去没反应
/// 反而更像卡住了。改成受控点击——照常可点，越界时**明说为什么**。
///
/// `on_toggle` 会接管翻转：设了它之后点击不再自动改 `sig`，得由本回调决定翻不翻。
/// 这正是「先判断再决定」需要的形态，且渲染始终跟随 `sig`，零闪烁。
///
/// 只拦「关掉最后几个」，已关掉的照常可以打开——否则用户会被锁死在当前组合里。
///
/// `min_enabled == 0` 时不接管，退回普通开关（等价于没有守卫）。
fn switch_with_floor(
    sig: Signal<bool>,
    all: &Rc<Vec<Signal<bool>>>,
    floor: Option<&MinEnabled>,
) -> Element {
    let Some(f) = floor else {
        return Element::switch(sig);
    };
    let all = all.clone();
    let n = f.count;
    let msg = if n == 1 {
        format!("至少要保留一个{}", f.noun)
    } else {
        format!("至少要保留 {n} 个{}", f.noun)
    };
    Element::switch(sig).on_toggle(move |ctx| {
        if sig.get() && all.iter().filter(|s| s.get()).count() <= n {
            ctx.toast_err(msg.clone());
            return; // 不翻转
        }
        sig.set(!sig.get());
    })
}

/// 「至少留 N 项开着」的下限约束（[`build_toggle_rows_dialog`] 的可选参数）。
///
/// 用 `Option<MinEnabled>` 而不是 `usize` 的 0 表示「不限」：这里的 0 与 1 是两种
/// **性质不同**的策略（不接管点击 / 接管并可能拒绝），不是同一量纲上的两个取值。
pub struct MinEnabled {
    /// 至少要有几项开着。
    pub count: usize,
    /// 提示语里的名词，如「格」→「至少要保留一个格」。
    pub noun: String,
}

/// 条目开关对话框的一行：标签 + 说明 + 开关。
pub struct ToggleRow {
    /// 该行代表的值（写回配置用）。
    pub value: String,
    /// 行标签。
    pub label: String,
    /// 行说明（副标题，可留空）。
    pub desc: String,
    /// 开关状态。
    pub sig: Signal<bool>,
}

/// 条目开关对话框：每行「手柄 + 标签 + 说明 + 开关」，**可拖拽调整顺序**。
///
/// 相对复选网格的取舍：网格一行挤两个，只放得下短标签；本对话框一行一项，
/// 给每项留出说明文字的位置——项数不多但每项需要解释时用它。
///
/// `order` 是行序的真相源（元素为 `ToggleRow::value`），拖拽与「恢复默认」都改它，
/// 列表随之重建。用 `reorder_list_signal` 而非 `reorder_list` 正是为了「恢复默认」
/// ——后者把顺序只存在节点树里，应用推不回去，点了恢复默认顺序会原样留在那儿。
///
/// 分隔线做进**每一行的顶部**（含首行）而不是行与行之间：拖动中行会重新堆叠，
/// 若首行特殊、其余带线，动画期间就会出现"顶上多一条、中间少一条"的错位。
/// 每行长得一样，任何排列都对。首行那条顺带把行区与上方提示文字分开。
///
/// `min_enabled` = 可选的「至少要留几项开着」约束（`None` = 不限）。把最后一项也关掉
/// 往往不是用户想要的：core 侧对「全关」只能回落成全显示或空条，两种反馈都与
/// 「我关掉了它们」相反。越界时**照常可点、弹 toast 说明**——见 [`switch_with_floor`]
/// 里为什么不用置灰。
pub fn build_toggle_rows_dialog(
    visible: Signal<bool>,
    title: &str,
    hint: &str,
    rows: Vec<ToggleRow>,
    defaults: Vec<String>,
    order: Signal<Vec<String>>,
    min_enabled: Option<MinEnabled>,
) -> Element {
    let rows_reset: Vec<(String, Signal<bool>)> =
        rows.iter().map(|r| (r.value.clone(), r.sig)).collect();
    let declared: Vec<String> = rows.iter().map(|r| r.value.clone()).collect();
    // 全部行的开关，供 `min_enabled` 的下限判定用（每行都要看"现在总共开着几个"）。
    let all_sigs: Rc<Vec<Signal<bool>>> = Rc::new(rows.iter().map(|r| r.sig).collect());
    // 行元数据按 value 查表——`reorder_list_signal` 的行构建函数只拿得到 value。
    let meta: Rc<Vec<ToggleRow>> = Rc::new(rows);
    // 左右留白用 **padding** 而非 margin：`setting_row` 内部是 `width_match()` 的 row，
    // margin 下它仍按父容器全宽排布，右侧开关会被挤出对话框。padding 缩的是内容盒，
    // 行拿到的可用宽才是扣掉留白后的。数值对齐 `card()` / `build_group_dialog`。
    let grid = Element::col().width_match().padding_xy(0, 2).child(
        Element::reorder_list_signal(order, move |value: String, handle| {
            let Some(r) = meta.iter().find(|r| r.value == value) else {
                // order 里混进了未知 value：不该发生，但也不能让整行塌成空白。
                return Element::label(value).fg_role(Role::TextMuted);
            };
            let desc = (!r.desc.is_empty()).then_some(r.desc.as_str());
            crate::widgets::list_row(
                Element::row()
                    .width_match()
                    .cross(Align::Center)
                    // 行内容左右各留 8：拖动中整行会浮起成一张带底色、投影与圆角的卡片，
                    // 而行的左右两端就是手柄和开关——不留边距时它们会贴死在卡片边框上
                    // （右侧开关尤其明显，圆角处几乎相切）。静止态这点留白也不碍事，
                    // 分隔线画在行外（`list_row`），随「界面细节」铺开，不受这 8px 影响。
                    .padding_xy(8, 0)
                    .child(handle)
                    .child(
                        crate::widgets::setting_row(
                            &r.label,
                            desc,
                            switch_with_floor(r.sig, &all_sigs, min_enabled.as_ref()),
                        )
                        .weight(1.0),
                    ),
            )
        })
        .on_reorder(move |_ctx, from, to| {
            order.update(|v| {
                let x = v.remove(from);
                v.insert(to.min(v.len()), x);
            })
        }),
    );
    let frame = Element::col()
        // 与 build_group_dialog 同宽：带副标题的行比复选网格更需要横向空间。
        .width(460)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(hint)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3)
                .padding_xy(18, 10),
        )
        .child(grid)
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    Element::button("恢复默认")
                        .small()
                        .outline_soft()
                        .on_click(move |_| {
                            for (v, s) in &rows_reset {
                                s.set(defaults.iter().any(|d| d == v));
                            }
                            // 顺序也是配置的一部分：只复位开关会留下用户上次拖出来的
                            // 优先级，看起来"恢复了一半"。声明序即出厂序。
                            order.set(declared.clone());
                        }),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| visible.set(false)),
                )
                .child(
                    Element::button("确认")
                        .small()
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// 配对复选网格对话框（用于 auto_pair *_pairs 类 strlist）。
/// `sigs`: (value, Signal<bool>)；`labels`: 每项对应的展示标签（1:1 with sigs）；
/// `defaults`: capability 默认 strlist（恢复默认用）。
pub fn build_pairs_dialog(
    visible: Signal<bool>,
    title: &str,
    hint: &str,
    sigs: Vec<(String, Signal<bool>)>,
    labels: Vec<String>,
    defaults: Vec<String>,
) -> Element {
    let sigs_reset = sigs.clone();
    let checkboxes: Vec<Element> = sigs
        .iter()
        .zip(labels.iter())
        .map(|((_, s), lbl)| Element::checkbox(lbl, *s).fg_role(Role::Text))
        .collect();
    let grid = Element::grid(2, 10, checkboxes);
    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(hint)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3)
                .padding_xy(18, 10),
        )
        .child(grid.width_match().margin_xy(18, 8))
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    Element::button("恢复默认")
                        .small()
                        .outline_soft()
                        .on_click(move |_| {
                            for (v, s) in &sigs_reset {
                                s.set(defaults.iter().any(|d| d == v));
                            }
                        }),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| visible.set(false)),
                )
                .child(
                    Element::button("确认")
                        .small()
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// 设置组对话框：标题 + 若干设置行（行间分隔线）+ [关闭]。
/// 用于「全局方案配置」码表/拼音对话框：内部控件经各自 writer 随主保存提交，无需独立保存。
/// `reset_fn`：传 Some 时底栏左侧显示「恢复默认」按钮，调用后各字段回落 capability 默认值。
pub fn build_group_dialog(
    visible: Signal<bool>,
    title: &str,
    rows: Vec<Element>,
    reset_fn: Option<Box<dyn Fn() + 'static>>,
) -> Element {
    build_group_dialog_with_link(visible, title, rows, reset_fn, None)
}

/// 同 [`build_group_dialog`]，但按钮栏左侧多一个「查看文档」链接。
///
/// 给的是链接而非按钮：它跳出应用、不改任何设置，与「恢复默认 / 关闭」这类会产生后果的
/// 操作不是一类东西，形态上区分开更不容易误点。
///
/// 高级功能（如候选注释的模板语法）用它替代把说明塞进对话框——语法还会继续长，
/// 200px 的框里既排不下也搜不了，而文档站可以配示例、对照表和跳转。
pub fn build_group_dialog_with_link(
    visible: Signal<bool>,
    title: &str,
    rows: Vec<Element>,
    reset_fn: Option<Box<dyn Fn() + 'static>>,
    doc_url: Option<&'static str>,
) -> Element {
    let (content, leading) = dialog_body_with_link(rows, reset_fn, doc_url);
    build_group_dialog_shell(visible, title, content, leading)
}

/// 同 [`build_group_dialog`]，尺寸由调用方定（行说明长、默认 460 宽会截半句的分组对话框）。
pub fn build_group_dialog_sized(
    visible: Signal<bool>,
    title: &str,
    rows: Vec<Element>,
    reset_fn: Option<Box<dyn Fn() + 'static>>,
    width: i32,
    height: i32,
) -> Element {
    let (content, leading) = dialog_body_with_link(rows, reset_fn, None);
    shell_with_title_sized(
        visible,
        Element::label(title),
        content,
        leading,
        width,
        height,
    )
}

/// [`build_group_dialog_with_link`] 的内容区与底栏左侧按钮组装。
fn dialog_body_with_link(
    rows: Vec<Element>,
    reset_fn: Option<Box<dyn Fn() + 'static>>,
    doc_url: Option<&'static str>,
) -> (Element, Vec<Element>) {
    // 水平内边距由每行自己承担（`card_pad`），分割线才能按「界面细节」贴到边。
    let mut content = Element::col().width_match().padding_xy(0, 2);
    for (i, r) in rows.into_iter().enumerate() {
        if i > 0 {
            content = content.child(crate::widgets::row_divider());
        }
        content = content.child(crate::widgets::card_pad(r));
    }

    let mut leading: Vec<Element> = Vec::new();
    if let Some(reset) = reset_fn {
        leading.push(
            Element::button("恢复默认")
                .small()
                .outline_soft()
                .on_click(move |_| reset()),
        );
    }
    if let Some(url) = doc_url {
        leading.push(Element::link("查看文档").url(url).font_size(12.0));
    }

    (content, leading)
}

/// 配置类对话框的**统一外壳**：标题栏 + 固定高滚动区 + 按钮栏。
///
/// 抽出来是为了让内容动态生成的对话框（方案设置那种 `host_signal` 重建 body）与
/// 清单驱动的静态对话框长得一模一样——两处各写一遍框架的结果，是宽度、滚动高度、
/// 标题字号、按钮栏间距慢慢各走各的，而用户看到的是"同一个设置界面里两种弹窗"。
///
/// `bottom_leading` 是按钮栏左侧的元素（恢复默认 / 查看文档 / 保存…）；「关闭」恒在右侧。
pub fn build_group_dialog_shell(
    visible: Signal<bool>,
    title: &str,
    content: Element,
    bottom_leading: Vec<Element>,
) -> Element {
    shell_with_title(visible, Element::label(title), content, bottom_leading)
}

// ⚠️ 这里曾有一个 `collapsible_group`（「总开关 + 折叠区」），是方案级码表配置的
// 「自定义码表配置」总开关。0.119 改成逐项三态后它没有了使用者，随之删除——
// 方案级覆盖的正确形态是**每项一个勾选框**（见 `dialogs::schema_codetable` 的模块文档），
// 「整段接管」这个模型本身有问题，不该留着给下一个人复用。要做折叠区请另写。

/// [`build_group_dialog_shell_large`] 中部滚动视口的高度。内容想恰好填满视口（自带滚动的
/// 控件，免得外壳再套一层滚动条）时按它算高度。
pub const LARGE_SHELL_BODY_H: i32 = 460;

/// 尺寸放大的变体，供内容分了 TAB、默认 460×380 装不下的浮层用。
pub fn build_group_dialog_shell_large(
    visible: Signal<bool>,
    title: &str,
    content: Element,
    bottom_leading: Vec<Element>,
) -> Element {
    shell_with_title_sized(
        visible,
        Element::label(title),
        content,
        bottom_leading,
        620,
        LARGE_SHELL_BODY_H,
    )
}

/// 尺寸可指定的动态标题外壳（标题随内容变化，如「按键自定义 - 五笔」：同一个浮层服务于
/// 任意方案）。给「默认 460×380 装不下」的那类用——当前是按键绑定编辑器家族，
/// 见 `dialogs::key_binding::build_binding_dialog`。
pub fn build_group_dialog_shell_dyn_sized(
    visible: Signal<bool>,
    title: Signal<String>,
    content: Element,
    bottom_leading: Vec<Element>,
    width: i32,
    height: i32,
) -> Element {
    shell_with_title_sized(
        visible,
        Element::label_signal(title),
        content,
        bottom_leading,
        width,
        height,
    )
}

/// 外壳本体。`title` 传的是**未加样式**的标签元素，样式在此统一施加——
/// 两个入口各自 `.font_size()` 一遍的话，改一处忘另一处就成了「两种弹窗」。
fn shell_with_title(
    visible: Signal<bool>,
    title: Element,
    content: Element,
    bottom_leading: Vec<Element>,
) -> Element {
    shell_with_title_sized(visible, title, content, bottom_leading, 460, 380)
}

/// 带「确定 / 取消」的外壳，供**编辑型**浮层用：改完要确认才生效。
///
/// 与 [`build_group_dialog_shell`] 那套「只有关闭」的形态是两种语义，不能混用：那类浮层
/// 里的控件**改一下就已经进工作态**了，底部只需要一个关掉的出口；而本外壳服务的是
/// 「攒一组值、一起提交或整体丢弃」——没有取消按钮的话，用户点开一行想看看配的是什么，
/// 看完只能被迫接受自己途中的每一次误点。
///
/// `confirm_enabled` 控制「确定」是否可点（如键与功能不合法时）。
pub fn build_confirm_dialog_shell(
    visible: Signal<bool>,
    title: Signal<String>,
    content: Element,
    confirm_enabled: Signal<bool>,
    on_confirm: impl Fn() + 'static,
) -> Element {
    let trailing = vec![
        Element::button("取消")
            .small()
            .outline_soft()
            .on_click(move |_| visible.set(false)),
        Element::button("确定")
            .small()
            .enabled_signal(confirm_enabled)
            .on_click(move |_| {
                on_confirm();
                visible.set(false);
            }),
    ];
    shell_with_buttons(
        visible,
        Element::label_signal(title),
        content,
        Vec::new(),
        trailing,
        FrameWidth::Fixed(460),
        400,
    )
}

/// 尺寸可指定的外壳。默认 460×380 装不下的（如分了 TAB 的方案设置）用这个。
///
/// 单开一个入口而不是把默认值改大：那会一次性放大**所有**用这套外壳的浮层，而它们
/// 绝大多数只有三五行，撑大只是留白。
fn shell_with_title_sized(
    visible: Signal<bool>,
    title: Element,
    content: Element,
    bottom_leading: Vec<Element>,
    width: impl Into<FrameWidth>,
    height: i32,
) -> Element {
    let width = width.into();
    let close = vec![
        Element::button("关闭")
            .small()
            .outline_soft()
            .on_click(move |_| visible.set(false)),
    ];
    shell_with_buttons(
        visible,
        title,
        content,
        bottom_leading,
        close,
        width,
        height,
    )
}

/// 外壳宽度：定宽，或随窗宽伸缩（撑满窗宽减两侧外边距，封顶 `max`）。
#[derive(Debug, Clone, Copy)]
pub enum FrameWidth {
    Fixed(i32),
    Fluid { max: i32 },
}

impl From<i32> for FrameWidth {
    fn from(w: i32) -> Self {
        FrameWidth::Fixed(w)
    }
}

/// 窗宽自适应外壳两侧的外边距。
const FLUID_MARGIN: i32 = 20;

/// 宽度随窗宽伸缩的动态标题外壳：窗口宽时宽到 `max_width`，窄时贴着窗边留 20px。
/// 给列多、定宽装不下的表格类浮层用（候选注释的分配表）。
pub fn build_group_dialog_shell_fluid(
    visible: Signal<bool>,
    title: Signal<String>,
    content: Element,
    bottom_leading: Vec<Element>,
    max_width: i32,
    height: i32,
) -> Element {
    shell_with_title_sized(
        visible,
        Element::label_signal(title),
        content,
        bottom_leading,
        FrameWidth::Fluid { max: max_width },
        height,
    )
}

/// 外壳本体（底部按钮由调用方给全）。抽出这一层是因为编辑型浮层要「确定 / 取消」而不是
/// 「关闭」——两套底栏各写一遍框架，就会重蹈本文件开头那段注释说的「同一个设置界面里
/// 两种弹窗」。
#[allow(clippy::too_many_arguments)]
fn shell_with_buttons(
    visible: Signal<bool>,
    title: Element,
    content: Element,
    bottom_leading: Vec<Element>,
    bottom_trailing: Vec<Element>,
    width: FrameWidth,
    height: i32,
) -> Element {
    // 内容超长时中间区域可滚动；固定视口高度确保标题栏和关闭按钮始终可见。
    let body = Element::scroll()
        .width_match()
        .height(height)
        .child(content);

    let mut bottom = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 12)
        .spacing(8);
    for el in bottom_leading {
        bottom = bottom.child(el);
    }
    bottom = bottom.child(Element::leaf().weight(1.0));
    for el in bottom_trailing {
        bottom = bottom.child(el);
    }

    let frame = match width {
        FrameWidth::Fixed(w) => Element::col().width(w),
        FrameWidth::Fluid { .. } => Element::col().width_match(),
    };
    let frame = frame
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            title
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(bottom);
    // 右上角小 ×：标题行右侧本就是空的（标题 `width_match` 左对齐），叠上去不挤任何东西。
    // 与底栏的「关闭 / 取消」是同一件事，多这个入口是因为浮层拖开之后底栏可能已经被
    // 拖出视野，而标题行按定义还留在窗口内（拖动的钳制保的就是它）。
    match width {
        FrameWidth::Fixed(_) => {
            Element::dialog_closable(visible, frame, move |_| visible.set(false))
        }
        // 自适应宽的外边距要落在「面板 + ×」这一层外面：落在面板上的话，`dialog_closable`
        // 把 × 摆在含外边距的外框角上，× 就跑到面板外头去了。故 × 在这里自己叠。
        FrameWidth::Fluid { max } => {
            let close = Element::icon_button("\u{2715}")
                .size(24, 24)
                .font_size(11.0)
                .fg_role(Role::TextMuted)
                .align_xy(Align::End, Align::Start)
                .margin(windui::theme::current().metrics.space_sm)
                .on_click(move |_| visible.set(false));
            let panel = Element::stack()
                .width_match()
                .max_width(max)
                .margin(FLUID_MARGIN)
                .child(frame)
                .child(close);
            Element::dialog(visible, panel)
        }
    }
}

/// 自定义序号标签对话框：10 个文本框对应序号 1-10（**每框一个完整标签**，可多字符），
/// 2 列排布。`inputs` 为 10 个文本信号（由调用方注册 writer/loader），编辑随主保存提交；
/// 「恢复默认」清空全部（全空 = 默认 1-9）。
///
/// 单框留空**不是**「到此为止」而是「该槽让位主题」——中间留空是合法配置，
/// 收集逻辑见 `manifest::collect_labels`。
pub fn build_index_labels_dialog(
    visible: Signal<bool>,
    title: &str,
    inputs: Vec<Signal<String>>,
) -> Element {
    let cells: Vec<Element> = inputs
        .iter()
        .enumerate()
        .map(|(i, s)| {
            Element::row()
                .cross(Align::Center)
                .spacing(8)
                .child(
                    Element::label(format!("序号 {}", i + 1))
                        .font_size(12.5)
                        .fg_role(Role::TextMuted)
                        .width(48),
                )
                .child(Element::text_input(*s, "").width(96))
        })
        .collect();
    let grid = Element::grid(2, 12, cells);
    let inputs_reset = inputs.clone();
    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(
                "为各序号填写标签，每框一个完整标签（可多字符，如 a、(1)、Ⅰ）；单框留空该序号用主题或默认值，全部留空用默认 1-9。",
            )
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(2)
            .padding_xy(18, 10),
        )
        .child(grid.width_match().margin_xy(18, 8))
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    Element::button("恢复默认")
                        .small()
                        .outline_soft()
                        .on_click(move |_| {
                            for s in &inputs_reset {
                                s.set(String::new());
                            }
                        }),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("关闭")
                        .small()
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// 自定义标点 5 列可编辑表格对话框（原字符 / 英文半角 / 英文全角 / 中文半角 / 中文全角）。
/// 复刻 examples/settings.rs 的表格 + 单元格编辑子对话框：点单元格弹文本框编辑，确认写回。
/// - `src_labels`：每行第 0 列「原字符」展示（只读，点击不弹编辑）；
/// - `cells_data`：每行 4 个可编辑单元格信号，行内下标为 **core 存储序**
///   [中半, 英全, 中全, 英半]（与 core lookup_custom 列语义一致）；UI 展示序
///   （英半/英全/中半/中全）仅在本函数内经 [`PUNCT_DISPLAY_COLS`] 单点映射，
///   这些信号由调用方注册到 writer/loader，编辑即改它们，随主保存提交；
/// - `defaults`：每行 4 列默认值（core 序；单元格清空时回落 + 「恢复默认」用）。
///
/// 返回一个含「表格对话框 + 单元格编辑子对话框」两层的 stack，整体挂根。
///
/// UI 展示列 → core 数组下标：英半=3, 英全=1, 中半=0, 中全=2。
/// 展示/存储换序只许经此常量，禁止在别处再写死下标（防两端再度错位）。
const PUNCT_DISPLAY_COLS: [usize; 4] = [3, 1, 0, 2];

/// 表格上方的额外控件，连同它对整张表的授权。
///
/// ★ `gate` **必填**，不是 `Option`：会插 header 的唯一场景就是「一个开关决定这张表算不算
/// 数」（方案级那处）。做成可选的话，下一个人插一个说明性 header 时会顺手不填，于是又回到
/// 「开关没勾、表照样能改、改完不生效」——那正是本字段要根治的问题。
/// 确实需要一个不管辖表格的 header 时，再加一个字段表达，不要把这一个放软。
pub struct PunctTableHeader {
    /// 插在说明与表格之间的控件（通常就是那个开关本身；它自己不受 `gate` 管辖）。
    pub element: Element,
    /// 授权信号：为 false 时表格与「恢复默认」整体置灰且不接事件（禁用沿父链继承，
    /// 见 windui `Element::enabled_signal`）。「关闭」按钮不在管辖内，永远点得动。
    pub gate: Signal<bool>,
}

/// `header`：表格上方的额外控件 + 授权信号（方案级那处放「本方案使用自己的表」开关）；
/// `None` = 不插，且表格恒可编辑（全局那处的开关在 manifest 里以 `gate_key` 挡在门外，
/// 关掉时连对话框都打不开，故不需要第二道）。
/// `hint`：表格上方的说明文字（两处语义不同——全局那份是「基线是内置默认」，方案那份还要
/// 说清「整份替换全局表」）。
pub fn build_punct_table_dialog(
    visible: Signal<bool>,
    title: &str,
    src_labels: Vec<String>,
    cells_data: Vec<[Signal<String>; 4]>,
    defaults: Vec<[String; 4]>,
    header: Option<PunctTableHeader>,
    hint: &str,
) -> Element {
    // 授权信号：未授权时表格子树整体禁用（事件不入、置灰），编辑入口另有一道显式守卫。
    let gate = header.as_ref().map(|h| h.gate);
    // 单元格编辑子对话框状态
    let edit_show = signal(false);
    let edit_buf = signal(String::new());
    let edit_row = signal(0usize);
    let edit_col = signal(0usize); // 0..3，对应 cells_data 行内列

    // 表格单元格：第 0 列为原字符（只读展示），第 1..4 列为可编辑信号（按展示序重排）。
    let src_sigs: Vec<Signal<String>> = src_labels.iter().map(|s| signal(s.clone())).collect();
    let table_cells: Vec<Vec<Signal<String>>> = src_sigs
        .iter()
        .zip(cells_data.iter())
        .map(|(src, row)| {
            let mut cols = vec![*src];
            cols.extend(PUNCT_DISPLAY_COLS.iter().map(|&i| row[i]));
            cols
        })
        .collect();

    // 点单元格：第 0 列只读跳过；其余映射回 core 下标，载入缓冲并弹编辑子对话框。
    let cells_for_edit = cells_data.clone();
    let table = Element::table_editable(
        vec![
            ("原字符", 0.8f32),
            ("英文半角", 1.0),
            ("英文全角", 1.0),
            ("中文半角", 1.0),
            ("中文全角", 1.0),
        ],
        table_cells,
        move |_ctx, r, c| {
            // 未授权时不弹编辑框。容器禁用已经拦住了事件，这一句是第二道：表格若哪天换成
            // 自绘 widget、走了别的投递路径，这里仍然守得住（同类缺口见方案级标点那轮的
            // 「绕过收窄类型的直调路径」）。
            if gate.is_some_and(|g| !g.get()) {
                return;
            }
            if c == 0 {
                return;
            }
            let ci = PUNCT_DISPLAY_COLS[c - 1];
            edit_buf.set(cells_for_edit[r][ci].get());
            edit_row.set(r);
            edit_col.set(ci);
            edit_show.set(true);
        },
    )
    .height(420);

    // 恢复默认：全部单元格回落默认值。
    let cells_reset = cells_data.clone();
    let defaults_reset = defaults.clone();
    let frame = Element::col()
        .width(720)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(hint)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3)
                .padding_xy(18, 10),
        );
    // 额外控件插在说明与表格之间：它决定整张表生不生效，得在表格之前被看到。
    let frame = match header {
        Some(h) => frame.child(
            Element::col()
                .width_match()
                .padding_xy(18, 0)
                .child(h.element),
        ),
        None => frame,
    };
    // 表格整体挂授权：禁用沿父链继承到每个单元格，于是「点单元格」在事件分发前就被拦下。
    let mut table_wrap = Element::col().width_match().padding_xy(18, 6).child(table);
    if let Some(g) = gate {
        table_wrap = table_wrap.enabled_signal(g);
    }
    let frame = frame.child(table_wrap).child(Element::divider()).child(
        Element::row()
            .width_match()
            .cross(Align::Center)
            .padding_xy(16, 12)
            .spacing(8)
            .child({
                // 「恢复默认」直接写单元格信号，是绕开表格的第二个改数据入口，同受授权管辖。
                let btn = Element::button("恢复默认")
                    .small()
                    .outline_soft()
                    .on_click(move |_| {
                        for (row, def) in cells_reset.iter().zip(defaults_reset.iter()) {
                            for i in 0..4 {
                                row[i].set(def[i].clone());
                            }
                        }
                    });
                match gate {
                    Some(g) => btn.enabled_signal(g),
                    None => btn,
                }
            })
            .child(Element::leaf().weight(1.0))
            .child(
                Element::button("关闭")
                    .small()
                    .on_click(move |_| visible.set(false)),
            ),
    );
    let table_dialog = Element::dialog(visible, frame);

    // 单元格编辑子对话框。
    let cells_commit = cells_data.clone();
    let defaults_commit = defaults.clone();
    let edit_frame = Element::col()
        .width(360)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("编辑标点映射")
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::col()
                .width_match()
                .padding_xy(18, 10)
                .child(Element::text_input(edit_buf, "替换字符（留空恢复默认）").width_match()),
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
                        .on_click(move |_| edit_show.set(false)),
                )
                .child(Element::button("确认").small().on_click(move |_| {
                    let r = edit_row.get();
                    let ci = edit_col.get();
                    let raw = edit_buf.get();
                    // 半角/全角空格为合法映射值，不可 trim 掉；其余去空白后若空则回落默认。
                    let is_space = raw == " " || raw == "\u{3000}";
                    let trimmed = if is_space {
                        raw.clone()
                    } else {
                        raw.trim().to_string()
                    };
                    let val = if trimmed.is_empty() {
                        defaults_commit[r][ci].clone()
                    } else {
                        trimmed
                    };
                    cells_commit[r][ci].set(val);
                    edit_show.set(false);
                })),
        );
    let edit_dialog = Element::dialog(edit_show, edit_frame);

    // 外层 stack 必须带 visible_when：否则它无 visible_when 恒铺满全窗，而 hit_test 在子节点
    // 都不可见时会把容器自身判为命中目标，吞掉全窗指针事件导致鼠标失灵。仅在任一对话框打开时挂载。
    Element::stack()
        .fill()
        .visible_when(move || visible.get() || edit_show.get())
        .child(table_dialog)
        .child(edit_dialog)
}

/// 热键编辑对话框：修饰键勾选 + 主键下拉（+ 可选「全局」）+ [恢复默认 | 取消 确认]。
/// 确认时合成 "ctrl+shift+e" 写回 `raw`（并更新行内显示 `disp`）；恢复默认从 `default` 反填。
#[allow(clippy::too_many_arguments)]
/// 功能快捷键对话框里那条系统占用提示的判定本体。抽出来是为了**可测** —— 留在
/// `build_hotkey_dialog` 的闭包里就只能靠肉眼，而它有一条容易写错的前置条件（见下）。
///
/// ⚠️ **半成品状态必须返回 `None`**：同一行上还挂着「只选修饰键不构成热键」与
/// 「单个按键会被全局吞掉」两条提示，三条靠状态天然互斥才共用一行。这里若在
/// 「没勾修饰键」或「没选主键」时也给出档位，就会两条同时显示、叠在一起。
///
/// 值域与档位不在这里定义，转交 `key_binding::system_reserved_combo_notice` —— 两个
/// 对话框列两张表的结果，是同一个键在一处提示、另一处不提示。
fn hotkey_dialog_system_risk(
    mods: [bool; 4],
    key: &str,
) -> Option<crate::dialogs::key_binding::SystemKeyRisk> {
    if !mods.iter().any(|b| *b) || key.is_empty() {
        return None;
    }
    crate::dialogs::key_binding::system_reserved_combo_notice(&crate::manifest::compose_hotkey(
        mods, key,
    ))
    .map(|(r, _)| r)
}

pub fn build_hotkey_dialog(
    visible: Signal<bool>,
    title: &str,
    raw: Signal<String>,
    disp: Signal<String>,
    default: String,
    mods: [Signal<bool>; 4],
    key_idx: Signal<usize>,
    key_values: Vec<String>,
    key_labels: Vec<String>,
    global: Option<Signal<bool>>,
    enabled: Signal<bool>,
) -> Element {
    use crate::dialogs::key_binding::SystemKeyRisk;
    use crate::manifest::{compose_hotkey, format_hotkey, parse_hotkey};
    let [mc, ms, ma, mw] = mods;

    let mod_row = Element::row()
        .spacing(16)
        .child(Element::checkbox("Ctrl", mc).small().fg_role(Role::Text))
        .child(Element::checkbox("Shift", ms).small().fg_role(Role::Text))
        .child(Element::checkbox("Alt", ma).small().fg_role(Role::Text))
        .child(Element::checkbox("Win", mw).small().fg_role(Role::Text));

    // 当前工作态的两个自由度：勾了修饰键吗、选了主键吗。
    //
    // **合法只有两种：两者都有（真热键），或两者都无（= 解绑）。只有一半都是废配置**，
    // 而两种半成品的坏法还不一样：
    //
    // - 只勾修饰键：`compose_hotkey` 对空主键一律返回 `"none"`（**不管勾了几个修饰键**），
    //   于是被当成解绑。该项原本没绑过的话，解绑等于什么都没做 ⇒ 工作态无变化 ⇒ 保存时
    //   报「没有变更」——用户明明刚配过，只是主键忘了选。
    // - 只选主键：合成出裸主键（`"e"`），而 core 编译这批功能快捷键时只判 `parse_hotkey`
    //   是否 `Some`、**不看有无修饰位**（`wind-config/src/hotkey.rs:366`），照常编进
    //   key_down 表且策略是 `Always`。后果不是「不生效」而是更糟：那个键在所有应用、
    //   中英两种模式下被无条件吞掉（`key_gate.rs:117`），`e` 从此打不出来，且无任何日志。
    //   `open_settings` 这么配坏就连设置窗都打不开，只能手改配置文件。F1-F12 无例外。
    //
    // 核心侧没有任何闸门挡这个，校验只能落在这里。三处（两条提示 + 确认可用性）共用同一
    // 判定：各写一份的结果，是提示已经出来了、确认还点得下去。
    let form: Rc<dyn Fn() -> (bool, bool)> = {
        let kv = key_values.clone();
        Rc::new(move || {
            let has_mod = mc.get() || ms.get() || ma.get() || mw.get();
            let has_key = kv.get(key_idx.get()).is_some_and(|k| !k.is_empty());
            (has_mod, has_key)
        })
    };

    // 当前这个组合键会不会被 Windows 先拿走。只在配全了（有修饰键 + 有主键）时有意义，
    // 半成品状态由上面 `form` 那两条提示负责。
    //
    // 判定与值域的单一真相源在 `key_binding::system_reserved_combo_notice` —— 这里
    // **只取档位**，不复制那张键表：两处各列一份的结果，是同一个键在两个对话框里
    // 一个提示一个不提示，而用户不会知道该信哪个。
    let risk: Rc<dyn Fn() -> Option<SystemKeyRisk>> = {
        let kv = key_values.clone();
        Rc::new(move || {
            let m = [mc.get(), ms.get(), ma.get(), mw.get()];
            let k = kv.get(key_idx.get()).cloned().unwrap_or_default();
            hotkey_dialog_system_risk(m, &k)
        })
    };

    let mut body = Element::col()
        .width_match()
        .spacing(10)
        .padding_xy(18, 12)
        .child(
            Element::label("修饰键")
                .font_size(12.0)
                .fg_role(Role::TextMuted),
        )
        .child(mod_row)
        .child(
            // 「主键」标签与「只勾了修饰键、没选主键」的提示同一行。
            //
            // 提示**不能自成一行**：它出现/消失会改对话框高度，用户每勾一个修饰键窗口就
            // 跳一下。挂在标签行右侧则始终有位置——11px 提示矮于 12px 标签，显隐都不改行高。
            //
            // 提示为什么必须有：`compose_hotkey` 对空主键一律返回 `"none"`（**不管勾了
            // 几个修饰键**），于是这种半成品状态会被当作「解绑」。若该项原本就没绑过，
            // 解绑等于什么都没做 ⇒ 工作态无变化 ⇒ 保存时报「没有变更」——用户明明刚配过，
            // 只是主键忘了选。等到那一步才反馈太迟，且那句提示还把原因指向了错误的地方。
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(
                    Element::label("主键")
                        .font_size(12.0)
                        .fg_role(Role::TextMuted),
                )
                .child(
                    Element::label("只选修饰键不构成热键，请选择主键")
                        .font_size(11.0)
                        .fg_role(Role::Warning)
                        .max_lines(1)
                        .visible_when({
                            let f = form.clone();
                            move || matches!(f(), (true, false))
                        }),
                )
                .child(
                    // 三条提示互斥，同挂这一行：分挂两行的话「修饰键」那行只剩 ~100px，
                    // 塞不下说得清后果的句子，而这句的后果（键被全局吞掉）恰恰最需要说清。
                    Element::label("单个按键会被全局吞掉，请加修饰键")
                        .font_size(11.0)
                        .fg_role(Role::Warning)
                        .max_lines(1)
                        .visible_when({
                            let f = form.clone();
                            move || matches!(f(), (false, true))
                        }),
                )
                .child(
                    // ★ 第三条：键**配全了**（有修饰键 + 有主键），但 Windows 可能先把它
                    // 拿走。与上面两条天然互斥 —— 那两条只在半成品状态出现，这条只在
                    // (true, true) 出现 —— 所以能共用同一行，不额外改对话框高度。
                    //
                    // 为什么这个对话框也要有：起因那次排查里，用户把 `Ctrl+Space` 配成
                    // 工具栏开关后「按下没反应」，而「显示/隐藏工具栏」在设置里的**主路径**
                    // 正是本对话框（`keys.toggle_toolbar` 是个 hotkey 字段）。只在
                    // `key_actions` 那个绑定对话框里提示的话，更容易被走到的这条路仍然
                    // 一个字都看不到。
                    //
                    // 档位判定与文案的单一真相源在 `key_binding::system_reserved_combo_notice`；
                    // 这里只取**档位**、配一句一行放得下的短语。那边有 `max_lines(3)` 的
                    // 空间可以讲清楚「去哪改」，这一行没有，两处信息密度本就该不同 ——
                    // 但事实（哪些键、哪一档）必须同源，不能各列一张表。
                    Element::label("Windows 系统快捷键，输入法收不到")
                        .font_size(11.0)
                        .fg_role(Role::Warning)
                        .max_lines(1)
                        .visible_when({
                            let r = risk.clone();
                            move || r() == Some(SystemKeyRisk::Unreachable)
                        }),
                )
                .child(
                    // 第二档用**中性灰**而不是警告色：它多数情况下仍能正常工作
                    // （出厂 `toggle_punct` 用的就是 `Ctrl+.`），标红会把可用配置说成坏的。
                    // 与本模块「码元首码」那条中性措辞同一取舍。
                    Element::label("Windows 输入法热键，可能被系统占用")
                        .font_size(11.0)
                        .fg_role(Role::TextMuted)
                        .max_lines(1)
                        .visible_when({
                            let r = risk.clone();
                            move || r() == Some(SystemKeyRisk::MayBeTaken)
                        }),
                ),
        )
        .child(Element::dropdown(key_labels, key_idx).width(160));

    if let Some(g) = global {
        body = body.child(
            Element::checkbox("注册为全局热键（其它程序中也生效）", g)
                .small()
                .fg_role(Role::Text),
        );
    }

    let kv_ok = key_values.clone();
    // 半成品一律置灰而不是让它点得下去：点下去要么写成解绑（用户以为配好了、实际什么
    // 都没配上），要么写成裸单键（把那个键全局吞掉）。两者相等 = 都有或都无 = 合法。
    let confirm_form = form.clone();
    let confirm = Element::button("确认")
        .small()
        .enabled_when(move || {
            let (has_mod, has_key) = confirm_form();
            has_mod == has_key
        })
        .on_click(move |_| {
            let m = [mc.get(), ms.get(), ma.get(), mw.get()];
            let k = kv_ok.get(key_idx.get()).cloned().unwrap_or_default();
            let v = compose_hotkey(m, &k);
            // 同步启用态：对话框选「（未设置）」合成 "none" 即视为禁用，避免与行内复选漂移。
            enabled.set(v != "none");
            disp.set(format_hotkey(&v));
            raw.set(v);
            visible.set(false);
        });
    let kv_def = key_values;
    let reset = Element::button("恢复默认")
        .small()
        .outline_soft()
        .on_click(move |_| {
            let (m, k) = parse_hotkey(&default);
            mc.set(m[0]);
            ms.set(m[1]);
            ma.set(m[2]);
            mw.set(m[3]);
            key_idx.set(kv_def.iter().position(|v| *v == k).unwrap_or(0));
            disp.set(format_hotkey(&default));
            raw.set(default.clone());
            // 同步启用态：默认绑定为 "none" 则视为禁用。
            enabled.set(default != "none");
            // 全局热键默认为空（无功能全局），恢复默认同步取消全局勾选。
            if let Some(g) = global {
                g.set(false);
            }
        });

    let frame = Element::col()
        .width(380)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(reset)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| visible.set(false)),
                )
                .child(confirm),
        );
    Element::dialog(visible, frame)
}

/// 候选注释文档地址（模板语法、全部变量、常用示例）。
pub(crate) const COMMENT_DOC_URL: &str =
    "https://windinput.com/docs/settings/appearance/candidate-comment";

#[cfg(test)]
mod font_rows_tests {
    use super::*;
    use serde_json::json;

    /// ★★ 字体名**含空格**时往返必须闭合。
    ///
    /// 这条守的正是「为什么不能用通用兜底控件」：`strlist` 的通用编辑器按空格分隔，
    /// `Microsoft YaHei UI` 会被拆成三个字体，且**保存时不报任何错**——
    /// 用户只会发现候选字体变了，而设置页显示的还是对的。
    #[test]
    fn font_list_round_trips_names_containing_spaces() {
        let v = json!(["Microsoft YaHei UI", "Noto Sans Mongolian"]);
        let rows = font_list_to_rows(Some(&v));
        assert_eq!(rows, "Microsoft YaHei UI\nNoto Sans Mongolian");
        assert_eq!(Value::Array(rows_to_font_list(&rows)), v, "往返未闭合");
    }

    /// ★ 同一行上三条提示的**互斥**，以及互斥真正依赖的那个事实。
    ///
    /// 三条挂在同一行（「只选修饰键不构成热键」/「单个按键会被全局吞掉」/ 系统占用），
    /// 靠状态天然互斥才不会叠在一起。
    ///
    /// ⚠️ **这条测试的判别力有限，别高估它**：把 `hotkey_dialog_system_risk` 里那道
    /// 「没勾修饰键就返回 None」删掉，它**仍然全绿** —— 因为 `compose_hotkey` 在无修饰键时
    /// 产出的是裸键（`"space"`，不含 `+`），而 `system_reserved_combo_notice` 对不含 `+` 的
    /// 串直接返回 `None`。互斥是这两个下游行为共同保证的，不是那道前置判断保证的。
    /// （初版注释声称它守的就是那道判断，动手验了一次才发现不是 —— 假护栏比没有更坏，
    /// 故在此写明。那道判断仍然留着：它显式表达意图，也挡住下游哪天改变行为。）
    ///
    /// 所以下面第二段直接钉住那个**真实依赖**：无修饰键时合成结果不得带 `+`。
    /// 那才是改动后会真的变红的地方。
    #[test]
    fn the_three_row_hints_stay_mutually_exclusive() {
        // 半成品的三种形态，系统占用这条都必须闭嘴。
        assert_eq!(
            hotkey_dialog_system_risk([true, false, false, false], ""),
            None,
            "只勾修饰键时该由「请选择主键」那条说话"
        );
        assert_eq!(
            hotkey_dialog_system_risk([false; 4], "space"),
            None,
            "只选主键时该由「会被全局吞掉」那条说话"
        );
        assert_eq!(
            hotkey_dialog_system_risk([false; 4], ""),
            None,
            "解绑态无提示"
        );

        // ★ 互斥真正依赖的事实：没有修饰键就合成不出组合键形态。
        // 这一行若被改成产出 `"+space"` 之类，上面的互斥立刻破掉。
        assert!(
            !crate::manifest::compose_hotkey([false; 4], "space").contains('+'),
            "无修饰键时 compose_hotkey 不得产出组合键形态，否则三条提示会同时显示"
        );
    }

    /// 配全之后，两个入口对同一个键必须给出**同一档位**。
    ///
    /// 这个对话框（功能快捷键）与 `key_actions` 的绑定对话框是两条路径，用户从哪条进来
    /// 都可能配出 `Ctrl+Space`。两处各列一张键表的话，同一个键会一处提示、一处不提示，
    /// 而用户不会知道该信哪个 —— 故本测试直接对着 `key_binding` 那份判定比。
    #[test]
    fn system_risk_agrees_with_the_binding_dialog() {
        use crate::dialogs::key_binding::{SystemKeyRisk, system_reserved_combo_notice};
        // (修饰键, 主键, 合成后的键串)
        let cases = [
            ([true, false, false, false], "space", "ctrl+space"),
            ([false, true, false, false], "space", "shift+space"),
            ([true, false, false, false], ".", "ctrl+."),
            ([false, false, true, false], "tab", "alt+tab"),
            ([false, false, false, true], "a", "win+a"),
            ([true, true, false, false], "u", "ctrl+shift+u"),
            ([true, false, true, false], "m", "ctrl+alt+m"),
        ];
        for (mods, key, composed) in cases {
            assert_eq!(
                hotkey_dialog_system_risk(mods, key),
                system_reserved_combo_notice(composed).map(|(r, _)| r),
                "{composed} 在两个对话框里的档位不一致"
            );
        }
        // 钉住方向：上面若两边同时退化成恒 None，比较仍会通过。
        assert_eq!(
            hotkey_dialog_system_risk([true, false, false, false], "space"),
            Some(SystemKeyRisk::MayBeTaken),
            "Ctrl+Space 应判为「可能被占」"
        );
        assert_eq!(
            hotkey_dialog_system_risk([false, false, false, true], "a"),
            Some(SystemKeyRisk::Unreachable),
            "Win+A 应判为「收不到」"
        );
    }

    /// 空行与尾随换行不得产生空字体项（空项会让 core 的链首变成空串）。
    #[test]
    fn font_list_drops_blank_lines() {
        assert_eq!(
            rows_to_font_list("A\n\n  \nB\n"),
            vec![json!("A"), json!("B")]
        );
        assert_eq!(rows_to_font_list(""), Vec::<Value>::new());
        assert_eq!(font_list_to_rows(None), "");
    }

    /// ★★ `scripts` 的值是**数组**，往返必须保持数组类型。
    ///
    /// 通用 map 控件按标量读写（`v.as_str()` / `Value::String`）：读出来是空串、
    /// 存回去类型变了 ⇒ core 反序列化失败、整项静默丢失。**这是本轮最该守住的一条**。
    #[test]
    fn font_scripts_round_trips_and_keeps_arrays() {
        let m = json!({
            "latin": ["Segoe UI", "Arial"],
            "cjk": ["Microsoft YaHei UI"],
        });
        let map = m.as_object().unwrap();
        // known 为空 = 快照缺键名域时的退化路径：只列已配的，按字典序。
        let rows = font_scripts_to_rows(map, &[]);
        assert_eq!(rows, "cjk = Microsoft YaHei UI\nlatin = Segoe UI, Arial");
        let back = rows_to_font_scripts(&rows);
        assert_eq!(Value::Object(back.clone()), m, "往返未闭合");
        assert!(
            back["latin"].is_array(),
            "值必须仍是数组——写成标量的话 core 解析不了，整项配置静默丢失"
        );
    }

    /// 畸形输入一律丢弃而不是写进半成品：无 `=`、空键、空链。
    #[test]
    fn font_scripts_drops_malformed_rows() {
        let m = rows_to_font_scripts("没有等号\n = 只有值\nlatin =\nlatin = Segoe UI\n");
        assert_eq!(Value::Object(m), json!({ "latin": ["Segoe UI"] }));
    }

    /// 预填：`known` 里的类别**全部**成行、**照抄顺序**，已配的填上值。
    ///
    /// ★ 顺序不能重排：core 侧有测试钉住 capability 的类别顺序与 `ScriptClass::ALL` 同序，
    /// 这里再 sort 一次就把那条钉子废了，且用户每次打开对话框会看到行序变化。
    #[test]
    fn font_scripts_prefills_every_known_category_in_order() {
        let known: Vec<String> = ["latin", "cjk", "emoji"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let m = json!({ "cjk": ["Microsoft YaHei UI"] });
        let rows = font_scripts_to_rows(m.as_object().unwrap(), &known);
        assert_eq!(rows, "latin = \ncjk = Microsoft YaHei UI\nemoji = ");
        // ★ 空行必须原样往返成「没配过」——否则光是打开一次对话框再确定，
        // 就会给每个类别写一条空数组进配置。
        assert_eq!(Value::Object(rows_to_font_scripts(&rows)), m, "往返未闭合");
    }

    /// 用户手写的、不在值域里的键**不得被吞掉**——设置页替 core 删数据是静默丢用户配置。
    /// 它们排在已知类别之后，彼此按字典序。
    #[test]
    fn font_scripts_keeps_unknown_keys_after_the_known_ones() {
        let known: Vec<String> = vec!["latin".to_string()];
        let m = json!({ "zzz": ["A"], "mystery": ["B"], "latin": ["C"] });
        let rows = font_scripts_to_rows(m.as_object().unwrap(), &known);
        assert_eq!(rows, "latin = C\nmystery = B\nzzz = A");
        assert_eq!(Value::Object(rows_to_font_scripts(&rows)), m, "往返未闭合");
    }

    /// ★★ 跨仓守门：预填用的类别名来自 core 的 capability，本仓**不得**手抄一份。
    ///
    /// 只断言「非空」是不够的——真正要拦的是「设置页列的类别 core 不认」和
    /// 「core 认的类别设置页不列」。故这里比对的是**快照里那一份**，
    /// 而快照由 core 的 `wind_rpc::capabilities::generate` 生成。
    #[test]
    fn font_script_categories_come_from_core_capability() {
        let caps = crate::capabilities::Capabilities::from_embedded();
        let e = caps
            .entries
            .get("ui.font.scripts")
            .expect("capability 应含 ui.font.scripts");
        assert_eq!(e.ty, "map");
        assert!(
            !e.values.is_empty(),
            "键名值域为空 ⇒ 对话框会退化成空框，用户无从知道有哪些类别"
        );
        // 预填行数 = 类别数（没配过任何东西时）。
        let rows = font_scripts_to_rows(&Map::new(), &e.values);
        assert_eq!(rows.lines().count(), e.values.len());
        for k in &e.values {
            assert!(
                rows.lines().any(|l| l.starts_with(&format!("{k} ="))),
                "预填缺类别 {k}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 门控：没勾「使用本方案的标点表」时，表格必须点不动。
    ///
    /// 真机反馈的原话是「上方 checkbox 不勾，下方照样能改，但实际无效」——writer 那侧
    /// 一直是对的（未勾选写 null，见 `schema_manager::punct_map_value`），坏的是界面
    /// 放行了一次注定被丢弃的编辑：用户改完一整页、保存、毫无变化且零提示。
    ///
    /// ★ 断言落在「点表格中央这一下命中的节点启不启用」，而不是「树里有没有禁用节点」：
    /// 后者把 gate 错挂到任意一个角落（比如「关闭」按钮）也照样绿，等于没测。
    #[test]
    fn punct_table_gate_blocks_editing_when_unchecked() {
        use windui::core::Tree;
        use windui::geometry::{Point, Size};

        let gate = signal(false);
        let cells: Vec<[Signal<String>; 4]> = (0..6)
            .map(|_| std::array::from_fn(|_| signal("x".to_string())))
            .collect();
        let defaults: Vec<[String; 4]> = (0..6)
            .map(|_| std::array::from_fn(|_| "d".to_string()))
            .collect();
        let src: Vec<String> = (0..6).map(|i| format!("s{i}")).collect();
        let el = build_punct_table_dialog(
            signal(true),
            "测试标点表",
            src,
            cells,
            defaults,
            Some(PunctTableHeader {
                element: Element::checkbox("开启", gate),
                gate,
            }),
            "说明",
        );

        let mut tree = Tree::new();
        let root = el.build(&mut tree);
        tree.root = Some(root);
        let mut te = windui::text::NullTextEngine;
        let win = Size::new(900, 900);
        tree.layout_root(win, &mut te);
        // 对话框居中、表格占据主体，故窗口中心落在某个单元格上。
        let center = Point::new(450, 450);

        let hit = tree.hit_test(center).expect("窗口中央应命中表格里的节点");
        assert!(
            !tree.node_enabled(hit),
            "没勾选却点得动表格 = 用户改完不生效且零提示"
        );
        assert!(tree.node_enabled(root), "禁的应是表格子树，不是整个对话框");

        gate.set(true);
        tree.layout_root(win, &mut te);
        let hit = tree.hit_test(center).expect("窗口中央应命中表格里的节点");
        assert!(tree.node_enabled(hit), "勾选后表格必须能编辑");
    }

    #[test]
    fn spaced_strlist_roundtrip() {
        let v = vec![
            "http://".to_string(),
            "https://".to_string(),
            "ftp://".to_string(),
        ];
        let s = strlist_to_spaced(&v);
        assert_eq!(s, "http:// https:// ftp://");
        assert_eq!(spaced_to_strlist(&s), v);
    }

    #[test]
    fn spaced_to_strlist_trims_and_skips_empty() {
        assert_eq!(
            spaced_to_strlist("  a   b  "),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(spaced_to_strlist("   ").is_empty());
    }

    #[test]
    fn map_roundtrip() {
        let mut m = serde_json::Map::new();
        m.insert("，".to_string(), Value::String("，".to_string()));
        m.insert("。".to_string(), Value::String("。".to_string()));
        let text = map_to_rows(&m);
        let m2 = rows_to_map(&text);
        assert_eq!(m, m2);
    }

    #[test]
    fn rows_to_map_skips_blank_and_no_eq() {
        let text = "  
，=，
badline
。=。
";
        let m = rows_to_map(text);
        assert_eq!(m.len(), 2);
        assert_eq!(m["，"], Value::String("，".to_string()));
    }

    #[test]
    fn map_to_rows_deterministic() {
        // map_to_rows sorts keys, output must be stable
        let mut m = serde_json::Map::new();
        m.insert("b".to_string(), Value::String("B".to_string()));
        m.insert("a".to_string(), Value::String("A".to_string()));
        let text = map_to_rows(&m);
        assert_eq!(
            text,
            "a=A
b=B"
        );
    }

    /// 两条提示都挂在「主键」标签行右侧，不自成一行——守的是**对话框高度不因它显隐而变**。
    /// 独立一行时用户每勾一个修饰键窗口就跳一下，那正是这轮改动要消灭的。
    ///
    /// 同一个用例连带守住「半成品下确认必须置灰」：两件事同源（`form` 那个闭包），分开写
    /// 会出现「提示已经出来了、确认还点得下去」。四个状态各按一次，因为**两种半成品的
    /// 坏法不同**：只勾修饰键 → 合成 `"none"` = 解绑（保存报「没有变更」）；只选主键 →
    /// 合成裸单键，core 照编不误且策略是 `Always`，那个键在所有应用里被吞掉、打不出来。
    #[test]
    fn hotkey_dialog_height_is_stable_and_confirm_blocks_half_configured_keys() {
        use windui::core::{NodeId, Tree};
        use windui::geometry::Size;

        // 面板宽度固定 380（见 `build_hotkey_dialog` 的 frame）。它的 width_match 子节点
        // 同宽，故取「宽 380 中最高的那个」= 面板自身。
        fn panel_h(t: &Tree, id: NodeId, best: &mut i32) {
            if let Some(n) = t.get(id) {
                if n.bounds.w == 380 {
                    *best = (*best).max(n.bounds.h);
                }
                for c in n.children.clone() {
                    panel_h(t, c, best);
                }
            }
        }
        // 树里唯一挂了启用条件的节点就是「确认」；其余节点 own_enabled 恒真。
        fn has_disabled(t: &Tree, id: NodeId) -> bool {
            match t.get(id) {
                None => false,
                Some(n) => {
                    !n.own_enabled() || n.children.clone().iter().any(|c| has_disabled(t, *c))
                }
            }
        }

        let visible = signal(true);
        let raw = signal(String::new());
        let disp = signal("未设置".to_string());
        let mods = [signal(false), signal(false), signal(false), signal(false)];
        let key_idx = signal(0usize);
        let enabled = signal(false);
        let el = build_hotkey_dialog(
            visible,
            "测试热键",
            raw,
            disp,
            "none".to_string(),
            mods,
            key_idx,
            vec![String::new(), "e".to_string()],
            vec!["（未设置）".to_string(), "E".to_string()],
            None,
            enabled,
        );

        let mut tree = Tree::new();
        let root = el.build(&mut tree);
        tree.root = Some(root);
        let mut te = windui::text::NullTextEngine;
        let mut relayout = |t: &mut Tree| t.layout_root(Size::new(800, 600), &mut te);

        relayout(&mut tree);
        let mut h0 = 0;
        panel_h(&tree, root, &mut h0);
        assert!(h0 > 0, "没量到面板，宽度约定（380）变了就得同步改这个用例");
        assert!(
            !has_disabled(&tree, root),
            "什么都没勾 = 解绑，是合法操作，确认不该置灰"
        );

        // (勾了修饰?, 选了主键?, 该禁用确认吗, 断言说明)
        let steps: [(bool, bool, bool, &str); 4] = [
            (
                true,
                false,
                true,
                "只勾修饰键 → 合成 \"none\" = 解绑，保存会报「没有变更」",
            ),
            (
                false,
                true,
                true,
                "只选主键 → 裸单键被 core 照编不误，那个键从此全局打不出来",
            ),
            (
                true,
                true,
                false,
                "修饰键 + 主键齐全，是合法热键，不该禁用确认",
            ),
            (false, false, false, "全空 = 解绑，是合法操作"),
        ];
        for (has_mod, has_key, should_block, why) in steps {
            mods[0].set(has_mod);
            key_idx.set(usize::from(has_key));
            relayout(&mut tree);
            let mut h = 0;
            panel_h(&tree, root, &mut h);
            assert_eq!(
                h, h0,
                "({has_mod}, {has_key}) 下高度变了 ⇒ 提示又自成一行了"
            );
            assert_eq!(has_disabled(&tree, root), should_block, "{why}");
        }
    }
}
