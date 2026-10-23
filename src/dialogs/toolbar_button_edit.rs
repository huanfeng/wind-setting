//! 工具栏自定义按钮的新建 / 编辑对话框（id + 显示文字 + 动作）。
//!
//! 动作那一栏走 [`crate::dialogs::action_expr`]：下拉选类型 + 一个参数框，用户填**真实
//! 路径**，转义由那边负责。「自定义表达式」是下拉里的一项，切过去就是一个自由文本框
//! ——常用几种照顾多数人，复杂的不挡路。
//!
//! # 复用余地
//!
//! 动作编辑那半截（下拉 + 参数 + 浏览）抽在 [`action_row`]，不认识「按钮」这个概念。
//! 短语的动作是同一套语法，将来那边要做编辑器时可以直接拿走它，只换外壳。

use windui::prelude::*;

use super::action_expr::{ActionExpr, ActionKind, parse};

/// 编辑中的按钮表单状态。**新建与编辑共用**——两者的差别只有「确认时是追加还是替换」，
/// 表单本身没有任何不同，分成两套只会变成两处要同步的校验。
#[derive(Clone)]
pub struct ButtonForm {
    pub visible: Signal<bool>,
    /// 正在编辑第几个按钮；`usize::MAX` = 新建。
    pub editing: Signal<usize>,
    pub id: Signal<String>,
    pub label: Signal<String>,
    /// 动作类型在 [`ActionKind::ALL`] 里的下标。
    pub kind_idx: Signal<usize>,
    /// 动作参数（真实值，未转义）。
    pub arg: Signal<String>,
    /// 校验失败的原因；空 = 无。就地显示在按钮上方，不弹二次对话框。
    pub error: Signal<String>,
    /// 对话框标题（「新建按钮」/「编辑按钮」）。做成 signal 而非按 `editing` 现算：
    /// `label_signal` 要的是 `Signal<String>`，不收闭包。
    pub title: Signal<String>,
}

/// 新建态的 `editing` 哨兵。
pub const NEW_BUTTON: usize = usize::MAX;

impl ButtonForm {
    pub fn new() -> Self {
        Self {
            visible: signal(false),
            editing: signal(NEW_BUTTON),
            id: signal(String::new()),
            label: signal(String::new()),
            kind_idx: signal(0),
            arg: signal(String::new()),
            error: signal(String::new()),
            title: signal("新建按钮".to_string()),
        }
    }

    /// 打开「新建」：清空所有字段。
    pub fn open_new(&self) {
        self.editing.set(NEW_BUTTON);
        self.id.set(String::new());
        self.label.set(String::new());
        self.kind_idx.set(0);
        self.arg.set(String::new());
        self.error.set(String::new());
        self.title.set("新建按钮".to_string());
        self.visible.set(true);
    }

    /// 打开「编辑第 i 个」：把现有值填进表单。
    pub fn open_edit(&self, index: usize, id: &str, label: &str, action: &str) {
        let a = parse(action);
        self.editing.set(index);
        self.id.set(id.to_string());
        self.label.set(label.to_string());
        self.kind_idx.set(
            ActionKind::ALL
                .iter()
                .position(|k| *k == a.kind)
                .unwrap_or(0),
        );
        self.arg.set(a.arg);
        self.error.set(String::new());
        self.title.set("编辑按钮".to_string());
        self.visible.set(true);
    }

    /// 当前选中的动作类型。
    pub fn kind(&self) -> ActionKind {
        ActionKind::ALL
            .get(self.kind_idx.get())
            .copied()
            .unwrap_or(ActionKind::Custom)
    }

    /// 表单 → 动作表达式源码。
    pub fn action_source(&self) -> String {
        ActionExpr {
            kind: self.kind(),
            arg: self.arg.get(),
        }
        .to_source()
    }
}

impl Default for ButtonForm {
    fn default() -> Self {
        Self::new()
    }
}

/// 校验表单，返回错误原因（`None` = 可以保存）。
///
/// `existing_ids` 是**除本条之外**已有的 id，用来挡重名——id 是 `items` 里
/// `custom:<id>` 的引用目标，重了就有一条永远指不到自己那个按钮。
///
/// 纯函数，与 UI 无关，故可单测。
pub fn validate(
    id: &str,
    label: &str,
    action: &str,
    existing_ids: &[String],
) -> Option<&'static str> {
    let id = id.trim();
    if id.is_empty() {
        return Some("请填写标识（id）");
    }
    // id 会拼进 `custom:<id>` 写进配置数组，含空白或 `-` 开头都会与那套编码打架。
    if id.contains(char::is_whitespace) {
        return Some("标识不能含空格");
    }
    if id.starts_with('-') {
        return Some("标识不能以减号开头");
    }
    if existing_ids.iter().any(|x| x == id) {
        return Some("标识重复，换一个");
    }
    if label.trim().is_empty() {
        return Some("请填写显示文字");
    }
    if action.trim().is_empty() {
        return Some("请填写动作");
    }
    None
}

/// 「启动程序」的文件选择框。`.exe` 过滤只在 Windows 有意义：Linux / macOS 的可执行文件
/// 没有扩展名，加了过滤就几乎什么都选不到。
fn program_pick_dialog() -> PickDialog {
    let dlg = PickDialog::new().title("选择程序");
    match program_filter_exts() {
        Some(exts) => dlg.filter("程序", exts),
        None => dlg,
    }
}

fn program_filter_exts() -> Option<&'static [&'static str]> {
    cfg!(windows).then_some(&["exe"][..])
}

/// 动作编辑行：类型下拉 + 参数框（路径类附「浏览」）。
///
/// 抽出来是为了**将来短语编辑器能直接复用**——它不认识按钮，只认识「一条动作」。
fn action_row(form: &ButtonForm) -> Element {
    let kind_labels: Vec<String> = ActionKind::ALL.iter().map(|k| k.label().into()).collect();
    let f2 = form.clone();
    let f3 = form.clone();

    // 占位符是静态的（`text_input` 不收响应式占位），各类型该填什么写在对话框顶部的
    // 说明里一次说清——比让占位符跟着下拉变更可靠，也不必为此引一条 reactive。
    let arg_row = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::text_input(form.arg, "按所选类型填写，见上方说明")
                .width_match()
                .weight(1.0),
        );
    // 「浏览」只对本机路径类出现：给网址栏配一个文件选择框是误导。
    let arg_row = arg_row.child(
        Element::button("浏览…")
            .small()
            .outline_soft()
            .enabled_when(move || f2.kind().takes_path())
            .on_click(move |ctx| {
                let arg = f3.arg;
                let error = f3.error;
                ctx.request_pick_file(program_pick_dialog(), move |path| match path {
                    Some(p) => arg.set(p.display().to_string()),
                    None => crate::file_pick::report_failure(|m| error.set(m.to_string())),
                });
            }),
    );

    Element::col()
        .width_match()
        .spacing(6)
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::label("动作").font_size(12.5).width(56))
                .child(Element::dropdown(kind_labels, form.kind_idx).weight(1.0)),
        )
        .child(arg_row)
}

/// 一个「标签 + 输入框」行。
fn field_row(label: &str, sig: Signal<String>, placeholder: &str) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(Element::label(label).font_size(12.5).width(56))
        .child(
            Element::text_input(sig, placeholder)
                .width_match()
                .weight(1.0),
        )
}

/// 构建按钮编辑对话框。`on_commit(index, id, label, action)` 在校验通过后调用，
/// `index == NEW_BUTTON` 表示新建。
pub fn build_button_form_dialog(
    form: &ButtonForm,
    existing_ids: impl Fn(usize) -> Vec<String> + 'static,
    on_commit: impl Fn(usize, String, String, String) + 'static,
) -> Element {
    let fc = form.clone();
    let visible = form.visible;

    let frame = Element::col()
        .width(460)
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
                "标识只用于内部引用（不显示）；显示文字建议一个汉字或两个字母，\
                 太长会被截断以保持各格等宽。\n\
                 动作参数按所选类型填：启动程序→程序路径；打开网址/文件→网址或路径；\
                 输入法命令→如 schema switch wubi86；模拟按键→如 Ctrl+Shift+P；\
                 自定义表达式→整条表达式。",
            )
            .font_size(12.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(5)
            .padding_xy(18, 8),
        )
        .child(
            Element::col()
                .width_match()
                .spacing(8)
                .padding_xy(18, 6)
                .child(field_row("标识", form.id, "sym"))
                .child(field_row("显示文字", form.label, "符"))
                .child(action_row(form)),
        )
        // 校验失败就地显示：为一句「id 不能为空」再弹一层对话框，深度就三层了。
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
                    let label = fc.label.get().trim().to_string();
                    let action = fc.action_source();
                    let idx = fc.editing.get();
                    if let Some(msg) = validate(&id, &label, &action, &existing_ids(idx)) {
                        fc.error.set(msg.to_string());
                        return;
                    }
                    fc.error.set(String::new());
                    on_commit(idx, id, label, action);
                    fc.visible.set(false);
                })),
        );
    Element::dialog(visible, frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_filter_only_on_windows() {
        assert_eq!(program_filter_exts().is_some(), cfg!(windows));
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn rejects_empty_and_duplicate_id() {
        assert!(validate("", "符", "open(\"x\")", &[]).is_some());
        assert_eq!(
            validate("sym", "符", "open(\"x\")", &ids(&["sym"])),
            Some("标识重复，换一个")
        );
        // 不与自己重名（调用方传的是「除本条之外」的 id）。
        assert!(validate("sym", "符", "open(\"x\")", &ids(&["other"])).is_none());
    }

    /// id 会拼进 `custom:<id>` 并进入带 `-` 前缀编码的数组，两种写法会与那套编码打架。
    #[test]
    fn rejects_ids_that_break_the_items_encoding() {
        assert_eq!(
            validate("a b", "符", "open(\"x\")", &[]),
            Some("标识不能含空格")
        );
        assert_eq!(
            validate("-sym", "符", "open(\"x\")", &[]),
            Some("标识不能以减号开头")
        );
    }

    #[test]
    fn rejects_empty_label_and_action() {
        assert!(validate("sym", "  ", "open(\"x\")", &[]).is_some());
        assert!(validate("sym", "符", "   ", &[]).is_some());
    }
}
