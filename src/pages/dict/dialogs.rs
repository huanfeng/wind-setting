//! 词库页浮层对话框 — 破坏性操作确认（删除/清空/恢复默认）。
//!
//! 挂到根窗口（见 `shell::build`）以获得全窗模态遮罩 + 居中，模式同 `schema_manager`。
//! 加/改词对话框在阶段 5 追加。

use windui::prelude::*;

use crate::auto_encode::auto_encode_node;
use crate::widgets::{ENCODE_BTN_W, clear_text_button, flat_panel, paste_button};

use super::state::DictManagerState;

/// 导入/导出对话框宽度。比其余对话框宽一档：这里要同时容纳目标方案下拉、四个类型勾选、
/// 多行预览与提示，窄了会把每一条都挤成两三行，读起来全是折行。
const IO_DIALOG_W: i32 = 540;

/// 导入对话框里「提示 + 预览」区的固定视口高度。
///
/// 刻意写死而非按内容自适应：这块的行数随目标方案与文件内容大幅波动（混输目标多两行
/// 作用域说明、自动匹配又多一行、预览本身 1~5 行），自适应会让对话框每选一次文件、
/// 每换一次目标就跳一次高度，按钮位置跟着乱动。固定视口 + 内部滚动，尺寸自始至终不变。
const IO_NOTE_VIEWPORT_H: i32 = 132;

/// 构建词库页所有浮层对话框：确认 + 加/改词 + 候选调整添加 + 系统短语只读查看（叠在一个 stack）。
pub fn build_dict_dialogs(mgr: &DictManagerState) -> Element {
    Element::stack()
        .fill()
        .child(build_confirm_dialog(mgr))
        .child(build_edit_dialog(mgr))
        .child(build_auto_encode(mgr))
        .child(build_quick_dialog(mgr))
        .child(build_common_dialog(mgr))
        .child(build_shadow_dialog(mgr))
        .child(build_view_dialog(mgr))
        .child(build_export_dialog(mgr))
        .child(build_import_dialog(mgr))
}

/// 目标方案选择行（导入导出共用）：`verb` 为「导入到」/「导出自」。
///
/// 目标独立于页面导航的数据域——混输方案自身只有候选调整，用户却常在混输域点导入，
/// 期望的是"把五笔词库装进去"。给一个显式的目标选择器，配合选文件后的自动匹配
/// （见 `DictManagerState::run_import_preview`），用户不必先理解"域"才能导对。
fn io_target_row(mgr: &DictManagerState, verb: &'static str) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::label(verb)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width(52),
        )
        .child(Element::dropdown_items_signal(mgr.io_target_items, mgr.io_target).weight(1.0))
}

/// 格式行：格式下拉 + 识别结果回显。
///
/// 下拉只给「自动识别 / 纯词列表」两项。强制指定 WindDict/Rime/TSV 需要 core 的
/// preview/import 接受 format 参数（现在一律按内容自动识别），给了也不生效；
/// 而纯词列表的强制指定是纯客户端能力，立即可用，也正是严格判据误伤时的救济通道。
fn io_format_row(mgr: &DictManagerState) -> Element {
    let note = mgr.io_format_note;
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::label("格式")
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width(52),
        )
        .child(
            Element::dropdown(
                vec!["自动识别".to_string(), "纯词列表（自动出码）".to_string()],
                mgr.io_format,
            )
            .min_width(200),
        )
        .child(
            Element::label_signal(note)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .weight(1.0)
                .max_lines(1)
                .visible_when(move || !note.get().is_empty()),
        )
}

/// 权重行：仅纯词列表出现。
///
/// 其余格式的权重来自文件本身，摆一个输入框会造成「能覆盖」的误导；而纯词列表
/// 压根没有权重列，不给用户一个入口就只能由代码替他拍板。
fn io_weight_row(mgr: &DictManagerState) -> Element {
    let vis = mgr.clone();
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::label("权重")
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width(52),
        )
        .child(
            Element::text_input(mgr.io_weight, "1200")
                .width(96)
                .height(28),
        )
        .child(
            Element::label("越大越靠前；纯词列表没有权重列，全部按此值入库")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .weight(1.0)
                .max_lines(1),
        )
        .visible_when(move || vis.is_word_list_import())
}

/// 音节分隔补齐的二选一。**只在预览报出「可补齐」行时才出现**——没有这类词的导入
/// （码表词库、本就带分隔的拼音词库）不该看到一行读不懂的选项。
///
/// 用勾选框而非两个单选：这个对话框里的类型选择已经是勾选框，形态一致；
/// 且默认项（补充）有实测支撑，勾着不动就是推荐路径。
fn io_boundary_row(mgr: &DictManagerState) -> Element {
    let count = mgr.import_boundary_pending;
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::label("音节分隔")
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width(52),
        )
        .child({
            // ⚠️ `Element::checkbox` 只负责显示，**自己不会翻转**——必须配 on_toggle，
            // 否则勾选框渲染正常、点了毫无反应（同 io_type_row 的写法）。
            let sig = mgr.io_fill_boundary;
            Element::checkbox("由程序补充（推荐）", sig)
                .small()
                .on_toggle(move |_| sig.set(!sig.get()))
        })
        .child(
            Element::label("不勾选则跳过这些词，不会以缺分隔的形式入库")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .weight(1.0)
                .max_lines(1),
        )
        .visible_when(move || count.get() > 0)
}

/// 提示条：内容为空时整行隐藏（自动匹配说明 / 目标方案作用域说明共用）。
fn io_note(sig: Signal<String>, role: Role) -> Element {
    Element::label_signal(sig)
        .font_size(12.0)
        .fg_role(role)
        .width_match()
        .max_lines(3)
        .visible_when(move || !sig.get().is_empty())
}

/// 数据类型勾选行（导入导出共用；仅目标方案适用的类型显示）。
fn io_type_row(mgr: &DictManagerState) -> Element {
    let cb = |label: &'static str, sig: Signal<bool>, key: &'static str, m: DictManagerState| {
        Element::checkbox(label, sig)
            .small()
            .on_toggle(move |_| sig.set(!sig.get()))
            .visible_when(move || m.io_type_applicable(key))
    };
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(18)
        .child(cb("用户词库", mgr.io_user_words, "userWords", mgr.clone()))
        .child(cb("临时词库", mgr.io_temp_words, "tempWords", mgr.clone()))
        .child(cb("词频", mgr.io_freq, "freq", mgr.clone()))
        .child(cb("候选调整", mgr.io_shadow, "shadow", mgr.clone()))
}

/// 导出对话框（方案级）：标题含方案说明 + 数据类型勾选 + 导出到文件。
fn build_export_dialog(mgr: &DictManagerState) -> Element {
    let exp = mgr.clone();
    let cancel = mgr.clone();
    let content = flat_panel()
        .width(IO_DIALOG_W)
        .padding(20)
        .spacing(12)
        .child(
            Element::label_signal(mgr.io_title)
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(24)
                .width_match(),
        )
        .child(Element::divider())
        .child(io_target_row(mgr, "导出自"))
        .child(io_note(mgr.io_scope_note, Role::TextMuted))
        .child(
            Element::label("选择要导出的数据类型")
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width_match(),
        )
        .child(io_type_row(mgr))
        .child(
            Element::label("勾选的类型将导出为一个 .wdict 文件")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(1),
        )
        // 引擎标记的提示放在按钮正上方：这是点「导出到文件…」之前最后看到的一句。
        .child(io_note(mgr.io_engine_note, Role::TextMuted))
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("关闭")
                        .small()
                        .outline_soft()
                        .on_click(move |_| cancel.cancel_export()),
                )
                .child(
                    Element::button("导出到文件…")
                        .small()
                        .on_click(move |ctx| exp.do_export_io(ctx)),
                ),
        );
    Element::dialog(mgr.export_visible, content)
}

/// 导入对话框（方案级）：标题含方案说明 + 数据类型勾选（选文件后自动勾选文件所含段）+
/// 选文件·自动识别·只显数量预览·合并/替换（不兼容或未预览时禁用）。
fn build_import_dialog(mgr: &DictManagerState) -> Element {
    let pick = mgr.clone();
    let merge = mgr.clone();
    let replace = mgr.clone();
    let merge_ready = mgr.clone();
    let replace_ready = mgr.clone();
    let cancel = mgr.clone();
    let file_row = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::button("选择文件…")
                .small()
                .outline_soft()
                .on_click(move |ctx| pick.pick_import_file(ctx)),
        )
        .child(
            Element::label_signal(mgr.import_file_name)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .weight(1.0)
                .max_lines(1),
        );
    let content = flat_panel()
        .width(IO_DIALOG_W)
        .padding(20)
        .spacing(12)
        .child(
            Element::label_signal(mgr.io_title)
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(24)
                .width_match(),
        )
        .child(Element::divider())
        .child(
            Element::label("从 WindDict / Rime / TSV / 纯词列表导入（按内容自动识别）")
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(1),
        )
        .child(file_row)
        .child(io_target_row(mgr, "导入到"))
        .child(io_format_row(mgr))
        .child(io_weight_row(mgr))
        .child(io_type_row(mgr))
        .child(io_boundary_row(mgr))
        // 提示与预览同处一个固定视口：三条的行数各自随目标/文件变动，分开放会让对话框
        // 高度跟着跳。放不下时内部滚动，外框尺寸恒定（见 IO_NOTE_VIEWPORT_H）。
        // 预览不再 max_lines 截断——溢出量交给滚动，长文案不会看不全。
        .child(
            Element::scroll()
                .width_match()
                .height(IO_NOTE_VIEWPORT_H)
                .child(
                    Element::col()
                        .width_match()
                        .spacing(8)
                        .child(io_note(mgr.io_auto_note, Role::Accent))
                        .child(io_note(mgr.io_scope_note, Role::TextMuted))
                        .child(
                            Element::label_signal(mgr.import_preview_msg)
                                .font_size(13.0)
                                .fg_role(Role::Text)
                                .width_match(),
                        ),
                ),
        )
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("关闭")
                        .small()
                        .outline_soft()
                        .on_click(move |_| cancel.cancel_import()),
                )
                .child(
                    Element::button("替换导入")
                        .small()
                        .danger()
                        .enabled_when(move || replace_ready.import_enabled())
                        .on_click(move |_| replace.confirm_import("replace")),
                )
                .child(
                    Element::button("合并导入")
                        .small()
                        .enabled_when(move || merge_ready.import_enabled())
                        .on_click(move |_| merge.confirm_import("merge")),
                ),
        );
    Element::dialog(mgr.import_confirm_visible, content)
}

/// 加/改词对话框（用户词库 / 短语）：编码/缩写（+出码）· 词条/短语 · 权重。
/// 标签与标题随类别经 `label_rc` 响应式切换；保存逻辑见 `DictManagerState::submit_word`。
/// 「标签 + 控件」一行（编辑与查看两个对话框同款，标签宽度一致才能让两处字段左边线对齐）。
fn labeled_field(label: &'static str, input: Element) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::label(label)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width(48),
        )
        .child(input)
}

/// 次序字段的说明行：主程序的短语层排序是「weight 降序 → position 升序」，**权重不同时
/// position 完全不参与比较**。不写清楚的话，用户会以为它是全局排序位而反复调它却没反应。
/// 编辑与查看两处共用同一句话，避免日后只改一处造成口径分裂。
fn position_hint() -> Element {
    Element::label("仅在同编码、同权重时决定先后（数值小的靠前）")
        .font_size(11.0)
        .fg_role(Role::TextMuted)
        .width_match()
        .padding_xy(56, 0)
}

/// 编辑对话框的「次序」字段（用户词库无此维度，整块隐藏）。
fn position_field(mgr: &DictManagerState) -> Element {
    let vis = mgr.clone();
    let is_phrase = move || {
        matches!(
            vis.current_category(),
            super::spec::WordCategory::PhraseUser
        )
    };
    let hint_vis = is_phrase.clone();
    Element::col()
        .width_match()
        .spacing(4)
        .child(labeled_field(
            "次序",
            Element::text_input(mgr.edit_position, "")
                .width(100)
                .height(34),
        ))
        .child(position_hint().visible_when(hint_vis))
        .visible_when(is_phrase)
}

fn build_edit_dialog(mgr: &DictManagerState) -> Element {
    let submit = mgr.clone();
    let cancel = mgr.clone();
    let encode = mgr.clone();
    let enc_vis = mgr.clone();

    // 编码/词条两个标签随类别变化（`label_rc` 绑信号），故与静态标签的 `labeled_field`
    // 各留一份——共用得把标签参数抽象成两种来源，反而更绕。样式保持一致（同宽同色号）。
    let field = |label: Signal<String>, input: Element| {
        Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(8)
            .child(
                Element::label_signal(label)
                    .font_size(12.5)
                    .fg_role(Role::TextMuted)
                    .width(48),
            )
            .child(input)
    };

    let code_input = Element::row()
        .weight(1.0)
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::text_input(mgr.edit_code, "")
                .height(34)
                .weight(1.0),
        )
        .child(
            Element::button("生成编码")
                .small()
                .outline_soft()
                .min_width(ENCODE_BTN_W)
                .on_click(move |_| encode.encode_current())
                .visible_when(move || enc_vis.current_spec().ops.encode),
        );

    // 正文一律多行——用户词库与短语用的是同一套文本域，两边都可能是多行内容
    // （词条里塞一段签名、一段地址都属常见用法）。此前用户词库锁单行，回车被吞掉，
    // 用户只能手打两个字符 `\n`，而那串在单行框里既看不出效果也极易写成 `\\n`。
    //
    // 曾经这里是 `text_multi`/`text_single` 两个元素靠 `visible_when` 二选一——
    // `multiline()` 是构建期属性，接不了信号，想按类别切就只能摆两份。两类都多行之后
    // 这个变通没有存在理由，整块收敛掉。
    //
    // 框里敲下的真换行由 `save_calls` 统一投影成字面 `\n` 再发给 core（core 收的是
    // 转义形态，见 `wind-webdata` 的 `ui_text`/`store_text` 一对）。
    //
    // 尾部「粘贴 / 清空」与编码行的「生成编码」等宽对齐（见 `widgets::ENCODE_BTN_W`）。
    // 这一行顶对齐而非居中：正文框有 88 高，两个小按钮吊在正中间会离标签很远。
    let text_field = Element::row()
        .weight(1.0)
        .cross(Align::Start)
        .spacing(8)
        .child(
            Element::text_input(mgr.edit_text, "")
                .multiline()
                .wrap(true)
                .height(88)
                .weight(1.0),
        )
        .child(paste_button(mgr.edit_text, false))
        .child(clear_text_button(mgr.edit_text));

    let body = Element::col()
        .width_match()
        .padding_xy(18, 12)
        .spacing(10)
        .child(field(mgr.edit_l_code, code_input))
        .child(field(mgr.edit_l_text, text_field))
        .child(labeled_field(
            "权重",
            Element::text_input(mgr.edit_weight, "")
                .width(100)
                .height(34),
        ))
        .child(position_field(mgr));

    let footer = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 12)
        .spacing(8)
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("取消")
                .small()
                .outline_soft()
                .on_click(move |_| cancel.cancel_edit()),
        )
        .child(
            Element::button("确定")
                .small()
                .on_click(move |_| submit.submit_word()),
        );

    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label_signal(mgr.edit_title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(footer);
    Element::dialog(mgr.edit_visible, frame)
}

/// 加/改词对话框的自动出码叶子（0×0 不可见）：词条一变即反查编码填进编码框。
///
/// 挂在对话框**外面**的同一个 stack 里，而不是塞进 `build_edit_dialog` 的面板：那是个带
/// `spacing(10)` 的 col，多一个零尺寸子节点就多一道 10px 空隙。
///
/// `active` 两条缺一不可：
/// - 对话框关着时不能跑——`edit_code`/`edit_text` 被快捷输入、常用字两个对话框**复用**成
///   别的字段（前者存类别、后者存那个字），跑起来就是往它们身上写编码。
/// - 当前类别不支持出码时不能跑——短语的「编码」是用户自定的缩写，没有反查这回事。
fn build_auto_encode(mgr: &DictManagerState) -> Element {
    let act = mgr.clone();
    let enc = mgr.clone();
    auto_encode_node(
        mgr.auto_encode,
        mgr.edit_text,
        mgr.edit_code,
        move || act.edit_visible.get() && act.current_spec().ops.encode,
        move |t| enc.encode_text_silent(t),
    )
}

/// 快捷输入格式的添加 / 编辑：类别 + 模板 + 本类可用变量提示。
///
/// 与 [`build_edit_dialog`] 分开而不是塞进它的 `visible_when` 分支：这个对话框没有编码、
/// 权重、次序（快捷输入没有这些维度），多出来的是类别选择与一整块变量提示——共用一个
/// 布局会变成一堆互斥的显隐条件，而每加一个条件都要在另一个类别上验一遍。
///
/// **编辑时类别锁定**：类别决定了可用变量与出厂分组，换类别本就是另一条格式（想换就删掉
/// 重加）。锁定的做法是禁用选择器而非隐藏它——用户仍需看到「这条属于哪一类」。
fn build_quick_dialog(mgr: &DictManagerState) -> Element {
    let submit = mgr.clone();
    let cancel = mgr.clone();
    let kind_enabled = mgr.clone();

    let field = |label: &str, input: Element| {
        Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(8)
            .child(
                Element::label(label)
                    .font_size(12.5)
                    .fg_role(Role::TextMuted)
                    .width(48),
            )
            .child(input)
    };

    let body = Element::col()
        .width_match()
        .padding_xy(18, 12)
        .spacing(10)
        .child(field(
            "类别",
            // ⚠️ 选项走 `dropdown_signal`（绑信号）而**不是**在这里算好一个 Vec：对话框只
            // 构建一次，而那一刻列表数据还没加载，算出来的是空列表并被永久冻结。
            // 选项在 `open_quick_add`/`open_quick_edit` 里填入 `quick_kind_labels`。
            Element::dropdown_signal(mgr.quick_kind_labels, mgr.quick_kind_idx)
                .width(160)
                // 编辑已有条目时锁定：`editing_orig` 有值即为编辑。
                .enabled_when(move || !kind_enabled.is_editing_quick()),
        ))
        .child(field(
            "模板",
            Element::text_input(mgr.edit_text, "如 $Y年$M月$D日")
                .height(34)
                .weight(1.0),
        ))
        .child(
            Element::label("可用变量（可选中复制）")
                .font_size(11.5)
                .font_weight(600)
                .fg_role(Role::TextMuted)
                .width_match(),
        )
        .child(
            // ★★ 用多行 `text_input` 承载而不是 label 或 RichText，判据是**选区粒度**：
            // 用户要做的事是「把 $LMD 照抄进模板」，也就是选中**一个短变量名**。
            //   - label 不可选中，只能手打；
            //   - RichText 有划选与右键复制，但选区是碎片级、Latin 按空格切 ⇒ `$LMD`
            //     整个是一个碎片，而选区要求「拖出锚点碎片」才成立——在变量名内部
            //     怎么拖都选不中，恰好挡死了主用例（其 doc 把「Latin 词内字符级选区」
            //     列在已知限制里）；
            //   - text_input 是字符级选区，与用户在别处的选中习惯一致。
            //
            // 它可编辑，但内容每次打开对话框都重设，且没有任何地方读这个信号——
            // 改了不会有任何后果，代价换来的是确定可用的复制。
            //
            // 高度写死 + 控件自带滚动：全部类别合计二十余行，铺开会把对话框顶出屏幕。
            Element::text_input(mgr.quick_var_text, "")
                .multiline()
                .wrap(false)
                .height(150)
                .width_match(),
        );

    let footer = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 12)
        .spacing(8)
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("取消")
                .small()
                .outline_soft()
                .on_click(move |_| cancel.cancel_quick_edit()),
        )
        .child(
            Element::button("确定")
                .small()
                .on_click(move |_| submit.submit_word()),
        );

    let frame = Element::col()
        .width(460)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label_signal(mgr.edit_title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(footer);
    Element::dialog(mgr.quick_edit_visible, frame)
}

/// 常用字的**添加**：一个字 + 判为哪边。
///
/// 只服务「添加」这一条路径：改已有的字走列表里的一键切换（`toggle_common_row`），
/// 为一个二值属性开模态、再点确定，比操作本身还重。这里之所以还要对话框，是因为
/// 要添加的字**不在列表里**（默认字表没收录），搜也搜不到，只能手填。
///
/// 与 [`build_edit_dialog`] 分开的理由同 [`build_quick_dialog`]：这张表没有编码、权重、
/// 次序，共用一个布局会变成一堆互斥的显隐条件，而每加一个条件都要在别的类别上验一遍。
fn build_common_dialog(mgr: &DictManagerState) -> Element {
    let submit = mgr.clone();
    let cancel = mgr.clone();

    let field = |label: &str, input: Element| {
        Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(8)
            .child(
                Element::label(label)
                    .font_size(12.5)
                    .fg_role(Role::TextMuted)
                    .width(48),
            )
            .child(input)
    };

    let body = Element::col()
        .width_match()
        .padding_xy(18, 12)
        .spacing(10)
        .child(field(
            "字",
            Element::text_input(mgr.edit_text, "").width(80).height(34),
        ))
        .child(field(
            "判为",
            // 两个互斥选项用分段控件：下拉要点两下才看得到另一个选项，而这里的全部信息
            // 就是「二选一」，值得直接摊开。
            Element::segmented(super::spec::common_choices(), mgr.common_choice).width(160),
        ))
        .child(
            Element::label(
                "任何字符都可以设，汉字之外的字根、间架结构符、注音、假名也行。设为生僻后，\
                 该字在「智能」「常用字」两档检索范围下不再出现在候选里，切到「全部字符」\
                 档仍可打出；设为常用则反之。此设置对所有方案生效。",
            )
            .font_size(11.5)
            .fg_role(Role::TextMuted)
            .width_match()
            // 说明有三行左右，给足行数上限；`wrap()` 只对 text_input 有效，label 靠
            // width_match + max_lines 折行。
            .max_lines(4),
        );

    let footer = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 12)
        .spacing(8)
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("取消")
                .small()
                .outline_soft()
                .on_click(move |_| cancel.cancel_common_edit()),
        )
        .child(
            Element::button("确定")
                .small()
                .on_click(move |_| submit.submit_word()),
        );

    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label_signal(mgr.edit_title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(footer);
    Element::dialog(mgr.common_edit_visible, frame)
}

/// 候选调整手动添加：编码 / 词条 / 类型(固定位置|隐藏) / 目标位置(仅固定位置)。
fn build_shadow_dialog(mgr: &DictManagerState) -> Element {
    let submit = mgr.clone();
    let cancel = mgr.clone();
    let pos_enabled = mgr.clone();

    let field = |label: &str, input: Element| {
        Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(8)
            .child(
                Element::label(label)
                    .font_size(12.5)
                    .fg_role(Role::TextMuted)
                    .width(64),
            )
            .child(input)
    };

    let body = Element::col()
        .width_match()
        .padding_xy(18, 12)
        .spacing(10)
        .child(field(
            "编码",
            Element::text_input(mgr.shadow_code, "")
                .height(34)
                .weight(1.0),
        ))
        .child(field(
            "词条",
            Element::text_input(mgr.shadow_word, "")
                .height(34)
                .weight(1.0),
        ))
        .child(field(
            "类型",
            Element::segmented(
                vec!["固定位置".to_string(), "隐藏".to_string()],
                mgr.shadow_type,
            )
            .width(200),
        ))
        .child(field(
            "目标位置",
            Element::text_input(mgr.shadow_pos, "")
                .width(100)
                .height(34)
                .enabled_when(move || pos_enabled.shadow_type.get() == 0),
        ));

    let footer = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 12)
        .spacing(8)
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("取消")
                .small()
                .outline_soft()
                .on_click(move |_| cancel.cancel_shadow_add()),
        )
        .child(
            Element::button("确定")
                .small()
                .on_click(move |_| submit.submit_shadow()),
        );

    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label_signal(mgr.shadow_title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(footer);
    Element::dialog(mgr.shadow_add_visible, frame)
}

/// 系统短语只读查看对话框：编码 / 内容 / 权重 / 次序用 text_input 展示（可选中复制）。
///
/// 字段构成与用户短语的编辑对话框一致（同一套 `labeled_field` + `position_hint`），差别只在
/// 不可改、且多一个「转为用户短语」出口。**未与编辑对话框合并**：合并要引入 readonly 维度，
/// 且得共用 `edit_*` 信号——查看系统短语会覆写用户尚未提交的编辑内容。
///
/// 「转为用户短语」放在此处而非列表操作列：系统短语行本已有「启用开关 + 查看」，默认列宽下
/// 再加一枚按钮会把操作列挤爆（`ACTIONS_WEIGHT` 只留得下两枚小按钮）。
fn build_view_dialog(mgr: &DictManagerState) -> Element {
    let cancel = mgr.clone();
    let claim = mgr.clone();

    let body = Element::col()
        .width_match()
        .padding_xy(18, 12)
        .spacing(10)
        .child(labeled_field(
            "编码",
            Element::text_input(mgr.view_code, "").weight(1.0),
        ))
        .child(labeled_field(
            "内容",
            Element::text_input(mgr.view_text, "")
                .multiline()
                .wrap(true)
                .height(88)
                .weight(1.0),
        ))
        .child(labeled_field(
            "权重",
            Element::text_input(mgr.view_weight, "").width(100),
        ))
        .child(
            Element::col()
                .width_match()
                .spacing(4)
                .child(labeled_field(
                    "次序",
                    Element::text_input(mgr.view_position, "").width(100),
                ))
                .child(position_hint()),
        );

    let footer = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 12)
        .spacing(8)
        // 转为用户短语：原样复制归属后即可编辑（原条目从系统列表隐去）。左置以与「关闭」分区。
        .child(
            Element::button("转为用户短语")
                .small()
                .outline_soft()
                .on_click(move |_| claim.claim_viewing_as_user()),
        )
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("关闭")
                .small()
                .outline_soft()
                .on_click(move |_| cancel.cancel_view()),
        );

    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("查看系统短语")
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(footer);
    Element::dialog(mgr.view_visible, frame)
}

/// 破坏性操作二次确认：正文由 `confirm_msg` 驱动，确认执行 `confirm_pending`。
fn build_confirm_dialog(mgr: &DictManagerState) -> Element {
    let confirm = mgr.clone();
    let cancel = mgr.clone();
    let content = flat_panel()
        .width(380)
        .padding(20)
        .spacing(14)
        .child(
            Element::label("确认操作")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(24)
                .width_match(),
        )
        .child(Element::divider())
        .child(
            Element::label_signal(mgr.confirm_msg)
                .font_size(13.0)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(4),
        )
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| cancel.cancel_confirm()),
                )
                .child(
                    Element::button("确定")
                        .small()
                        .danger()
                        .on_click(move |_| confirm.confirm_pending()),
                ),
        );
    Element::dialog(mgr.confirm_visible, content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SchemaInfo;
    use windui::core::Tree;
    use windui::geometry::Size;

    /// 建一个「五笔 · 用户词库」的场（同 `state.rs` 各用例的建场方式）。
    fn user_dict_state() -> DictManagerState {
        let st = DictManagerState::new();
        *st.schemas.borrow_mut() = vec![SchemaInfo {
            id: "wb".into(),
            name: "五笔".into(),
            kind: "codetable".into(),
            ..Default::default()
        }];
        st.domain.set(super::super::state::GLOBAL_DOMAINS);
        st.sub_tab.set(0); // 用户词库
        st
    }

    fn frame(tree: &mut Tree) {
        tree.layout_root(Size::new(900, 700), &mut windui::text::NullTextEngine);
    }

    /// 自动出码在**对话框这棵真树上**跑得起来：词条一变，编码跟着出；
    /// 用户手改编码之后不再自动覆盖。
    ///
    /// 走真实 `layout_root`（`dispatch_reactive_updates` 是 `on_update` 唯一的驱动点）：
    /// 只测 `encode_text` 的话，「叶子挂没挂进 stack、`active` 判据写没写对」这一半
    /// 照样全绿——而那一半才是这个功能会坏的地方。
    #[test]
    fn edit_dialog_auto_encodes_and_yields_to_manual_edit() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let st = user_dict_state();
        st.open_add();
        let mut tree = Tree::new();
        let id = build_dict_dialogs(&st).build(&mut tree);
        tree.root = Some(id);
        frame(&mut tree); // 基线帧（open_add 的预填在此被 reseed 吸收）

        st.edit_text.set("工人".into());
        frame(&mut tree);
        assert_eq!(
            st.edit_code.get(),
            "ab",
            "词条变化后自动出码没把编码填进去 ⇒ 叶子没挂上或 active 判据不成立"
        );

        // 再变一次：首次自动写回若被自己判成「用户手改」，这一步就不会再动。
        st.edit_text.set("工人们".into());
        frame(&mut tree);
        assert_eq!(
            st.edit_code.get(),
            "abc",
            "自动出码只生效一次 ⇒ 写回没记进基线"
        );

        // 用户手改编码 → 让位于手动。
        st.edit_code.set("mine".into());
        frame(&mut tree);
        st.edit_text.set("工人阶级".into());
        frame(&mut tree);
        assert_eq!(st.edit_code.get(), "mine", "手改过编码后不得再被自动覆盖");

        // 点「生成编码」（= `encode_current`）恢复自动。
        st.encode_current();
        frame(&mut tree);
        assert_eq!(st.edit_code.get(), "abcd", "「生成编码」应按当前词条重算");
        st.edit_text.set("工人".into());
        frame(&mut tree);
        assert_eq!(st.edit_code.get(), "ab", "「生成编码」之后应恢复自动生成");
    }

    /// 出不了码时保留原编码。mock 里 BMP 外字符出不了码（同真实的「方案未配拆字」）。
    #[test]
    fn unencodable_text_keeps_existing_code_in_dialog() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let st = user_dict_state();
        st.open_add();
        let mut tree = Tree::new();
        let id = build_dict_dialogs(&st).build(&mut tree);
        tree.root = Some(id);
        frame(&mut tree);

        st.edit_text.set("工人".into());
        frame(&mut tree);
        assert_eq!(st.edit_code.get(), "ab");

        st.edit_text.set("𠀀".into()); // 扩展区生僻字：出不了码
        frame(&mut tree);
        assert_eq!(
            st.edit_code.get(),
            "ab",
            "出不了码时必须保留原编码，不能清空"
        );
    }

    /// 对话框关着时不许跑：`edit_code`/`edit_text` 被快捷输入、常用字两个对话框复用成
    /// 别的字段（类别 / 那个字），跑起来就是往它们身上写编码。
    #[test]
    fn auto_encode_is_inert_while_dialog_hidden() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };

        let st = user_dict_state();
        let mut tree = Tree::new();
        let id = build_dict_dialogs(&st).build(&mut tree);
        tree.root = Some(id);
        frame(&mut tree);

        // 快捷输入对话框那套用法：edit_code 存类别、edit_text 存模板。
        st.edit_code.set("date".into());
        st.edit_text.set("今天是 ${date}".into());
        frame(&mut tree);
        assert_eq!(st.edit_code.get(), "date", "对话框关着时不得改写 edit_code");
    }
}
