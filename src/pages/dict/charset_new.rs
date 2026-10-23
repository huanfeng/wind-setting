//! 「字符分类」的新建：工具栏「+ 添加」→ 填文件名、显示名、默认判定与优先级 → 列表里多一条
//! 空分类。成员与码位范围之后在那一行用「编辑文件」补（`file_edit`）。
//!
//! 取代原先「导出模板 → 在编辑器里填 → 回读试算 → 加载」那套外部编辑对话框：新建只需要几项
//! 基本信息，细改交给直接编辑文件。

use serde_json::json;
use windui::prelude::*;

use super::state::DictManagerState;
use crate::rpc;
use crate::widgets::flat_panel;

/// 默认判定下拉的选项，与 `charset.create` 的 `default` 一一对应。
const DEFAULT_LABELS: [&str; 3] = ["不表态", "常用", "生僻"];
const DEFAULT_VALUES: [&str; 3] = ["inherit", "common", "rare"];

/// 新建对话框的状态（Signal 为 Copy，整体可 Clone 进闭包）。
#[derive(Clone)]
pub struct CharsetNewState {
    pub visible: Signal<bool>,
    /// 文件名（即 key，落盘为 `charsets/<key>.yaml`）。
    pub key: Signal<String>,
    pub name: Signal<String>,
    /// 默认判定下拉（[`DEFAULT_LABELS`] 的下标）。
    pub default: Signal<usize>,
    pub default_items: Signal<Vec<String>>,
    /// 优先级（空 = 不设）。
    pub order: Signal<String>,
    /// 校验或 core 的报错；空 = 没有。
    pub error: Signal<String>,
}

impl CharsetNewState {
    pub fn new() -> Self {
        Self {
            visible: signal(false),
            key: signal(String::new()),
            name: signal(String::new()),
            default: signal(0),
            default_items: signal(DEFAULT_LABELS.iter().map(|s| s.to_string()).collect()),
            order: signal(String::new()),
            error: signal(String::new()),
        }
    }
}

impl Default for CharsetNewState {
    fn default() -> Self {
        Self::new()
    }
}

/// 文件名的即时校验（与 core `is_valid_key` 同规则；core 仍是最终判据）。
pub(crate) fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("请填文件名".into());
    }
    if key.trim() != key {
        return Err("文件名首尾不能有空格".into());
    }
    if key.len() > 64 {
        return Err("文件名太长".into());
    }
    // Windows 保留的设备名当不了文件名（同 core `is_valid_key`）。
    let lower = key.to_ascii_lowercase();
    let b = lower.as_bytes();
    if ["con", "prn", "aux", "nul"].contains(&lower.as_str())
        || ((lower.starts_with("com") || lower.starts_with("lpt"))
            && b.len() == 4
            && (b'1'..=b'9').contains(&b[3]))
    {
        return Err("这个文件名是 Windows 保留的，换一个".into());
    }
    if key
        .chars()
        .any(|c| "/\\.:*?\"<>|".contains(c) || c.is_control())
    {
        return Err("文件名不能含 / \\ . : * ? \" < > |".into());
    }
    Ok(())
}

impl DictManagerState {
    /// 打开新建对话框（清空上次填的）。
    pub fn open_charset_create(&self) {
        let n = &self.charset_new;
        n.key.set(String::new());
        n.name.set(String::new());
        n.default.set(0);
        n.order.set(String::new());
        n.error.set(String::new());
        n.visible.set(true);
    }

    pub fn cancel_charset_create(&self) {
        self.charset_new.visible.set(false);
    }

    /// `charset.create` 的参数；校验不过返回错误文案。
    pub(crate) fn charset_create_params(&self) -> Result<serde_json::Value, String> {
        let n = &self.charset_new;
        let key = n.key.get();
        validate_key(&key)?;
        let mut p = json!({
            "key": key,
            "name": n.name.get().trim(),
            "default": DEFAULT_VALUES[n.default.get().min(DEFAULT_VALUES.len() - 1)],
        });
        let order = n.order.get();
        if !order.trim().is_empty() {
            let v: i64 = order
                .trim()
                .parse()
                .map_err(|_| "优先级需为整数".to_string())?;
            p["order"] = json!(v);
        }
        Ok(p)
    }

    /// 「确定」：建好就关对话框、列表里多一条；失败留着对话框显示原因。
    pub fn submit_charset_create(&self) {
        let p = match self.charset_create_params() {
            Ok(p) => p,
            Err(e) => return self.charset_new.error.set(e),
        };
        match rpc::call_core("charset.create", &p) {
            Ok(_) => {
                self.charset_new.visible.set(false);
                let name = p["name"].as_str().unwrap_or_default();
                let shown = if name.is_empty() {
                    p["key"].as_str().unwrap_or_default()
                } else {
                    name
                };
                self.notify_ok(&format!(
                    "已新建字符分类「{shown}」，成员与范围用这一行的「编辑文件」添加"
                ));
                self.reload();
            }
            Err(e) => self.charset_new.error.set(e.to_string()),
        }
    }
}

/// 新建对话框。
pub fn build_charset_create_dialog(mgr: &DictManagerState) -> Element {
    let n = mgr.charset_new.clone();
    let field = |label: &'static str, input: Element| {
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
            .child(input.weight(1.0))
    };
    let err = n.error;
    let (m_cancel, m_ok) = (mgr.clone(), mgr.clone());
    let content = flat_panel()
        .width(440)
        .padding(20)
        .spacing(12)
        .child(
            Element::label("新建字符分类")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(24)
                .width_match(),
        )
        .child(Element::divider())
        .child(field(
            "文件名",
            Element::text_input(n.key, "如 my_symbols，保存为 charsets/<文件名>.yaml"),
        ))
        .child(field(
            "显示名",
            Element::text_input(n.name, "列表里显示的名字，留空同文件名"),
        ))
        .child(field(
            "默认判定",
            Element::dropdown_signal(n.default_items, n.default),
        ))
        .child(field(
            "优先级",
            Element::text_input(n.order, "数字小的先判，留空为 5（排在出厂分类前）"),
        ))
        .child(
            Element::label("新建的是空分类。成员与码位范围之后用这一行的「编辑文件」添加。")
                .font_size(11.5)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(2),
        )
        .child(
            Element::label_signal(err)
                .font_size(12.0)
                .fg_role(Role::Danger)
                .width_match()
                .max_lines(2)
                .visible_when(move || !err.get().is_empty()),
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
                        .on_click(move |_| m_cancel.cancel_charset_create()),
                )
                .child(
                    Element::button("确定")
                        .small()
                        .on_click(move |_| m_ok.submit_charset_create()),
                ),
        );
    Element::dialog(n.visible, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_key_follows_core_rules() {
        assert!(validate_key("my_symbols").is_ok());
        assert!(validate_key("我的符号").is_ok());
        assert_eq!(validate_key("").unwrap_err(), "请填文件名");
        assert_eq!(validate_key(" a").unwrap_err(), "文件名首尾不能有空格");
        assert!(validate_key("a.b").is_err());
        assert!(validate_key("a/b").is_err());
        assert!(validate_key(&"a".repeat(65)).is_err());
        assert!(validate_key("CON").is_err());
        assert!(validate_key("com1").is_err());
        assert!(validate_key("com0").is_ok());
        assert!(validate_key("console").is_ok());
    }

    fn with_mock<R>(f: impl FnOnce() -> R) -> R {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        crate::rpc::take_mock_calls();
        let r = f();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        r
    }

    /// 填好 → charset.create 带上四项 → 对话框关掉；优先级不是整数就留着对话框报错。
    #[test]
    fn create_sends_the_basics_and_closes() {
        with_mock(|| {
            let st = DictManagerState::new();
            st.open_charset_create();
            let n = &st.charset_new;
            n.key.set("my_symbols".into());
            n.name.set(" 我的符号 ".into());
            n.default.set(2);
            n.order.set("x".into());
            st.submit_charset_create();
            assert!(n.visible.get());
            assert_eq!(n.error.get(), "优先级需为整数");

            n.order.set("7".into());
            st.submit_charset_create();
            let calls: Vec<_> = crate::rpc::take_mock_calls()
                .into_iter()
                .filter(|(m, _)| m == "charset.create")
                .map(|(_, p)| p)
                .collect();
            assert_eq!(
                calls,
                vec![
                    json!({"key": "my_symbols", "name": "我的符号", "default": "rare", "order": 7})
                ]
            );
            assert!(!n.visible.get());
        });
    }
}
