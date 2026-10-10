//! 「Emoji/OpenCC 附加模块」对话框：管理 `input.emoji.modules`（模块 id → 是否开启）。
//!
//! 模块清单来自 core 的 `emoji.modules` RPC：内置模块（白霜拼音的 emoji / 符号表，按分组拆开）
//! 加上用户放进用户目录 `emoji\` 的 OpenCC 表（`*.txt`，id 即文件名）。条数不定，不能像固定
//! 设置项那样在清单里逐个写开关，故走对话框——与方案设置里的「扩展词库」同一形态：标题旁
//! 一个打开用户目录的按钮，下面每个模块一行勾选。
//!
//! # 开关状态的真相源
//!
//! 状态在配置 `input.emoji.modules` 里，本对话框按 `配置值 ?? defaultOn` 算初值；RPC 只给
//! 清单不给状态（见 core `web_emoji_modules` 的说明）。写回时以读进来的那张表为底、只覆盖
//! 「表里本来就有这个键」或「与出厂值不同」的项——什么都没动时写出去的与读进来的逐字相同，
//! 不制造假的「未保存改动」；用户删掉文件后表里残留的键也原样留着，文件放回来开关还在。

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::{Map, Value, json};
use windui::prelude::*;

/// 一个模块（对应 core 的 `EmojiModuleEntry`）。
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleRow {
    pub id: String,
    pub label: String,
    pub description: String,
    /// 相对数据根的源文件（用户模块显示它，内置模块不显示）。
    pub file: String,
    pub user: bool,
    pub default_on: bool,
}

#[derive(Clone)]
pub struct EmojiModulesUi {
    pub visible: Signal<bool>,
    pub rows: Signal<Vec<ModuleRow>>,
    /// 用户目录名（相对用户配置目录），来自 RPC，缺省 `emoji`。
    user_dir: Rc<RefCell<String>>,
    switches: Rc<RefCell<BTreeMap<String, Signal<bool>>>>,
    /// 读进来的 `input.emoji.modules` 原表，写回的底。
    base: Rc<RefCell<Map<String, Value>>>,
    /// 全局 toast（`AppState.feedback`）：打开目录失败时报错用。
    feedback: Signal<Option<(ToastKind, String)>>,
}

impl EmojiModulesUi {
    pub fn new(feedback: Signal<Option<(ToastKind, String)>>) -> Self {
        Self {
            visible: signal(false),
            rows: signal(Vec::new()),
            user_dir: Rc::new(RefCell::new("emoji".to_string())),
            switches: Rc::new(RefCell::new(BTreeMap::new())),
            base: Rc::new(RefCell::new(Map::new())),
            feedback,
        }
    }

    /// 向 core 拉模块清单。拉不到（core 未连上）时清单为空，写回仍按原表原样，不丢配置。
    pub fn refresh_list(&self) {
        let v = crate::rpc::call_core("emoji.modules", &json!({})).unwrap_or(json!({}));
        if let Some(d) = v.get("userDir").and_then(Value::as_str) {
            *self.user_dir.borrow_mut() = d.to_string();
        }
        self.rows.set(rows_from_value(&v));
    }

    /// 按配置表算各行初值（`配置值 ?? defaultOn`）。先 `refresh_list` 再 `seed`。
    pub fn seed(&self, cfg: Option<&Map<String, Value>>) {
        let base = cfg.cloned().unwrap_or_default();
        let mut sw = self.switches.borrow_mut();
        for r in self.rows.get() {
            let on = base
                .get(&r.id)
                .and_then(Value::as_bool)
                .unwrap_or(r.default_on);
            match sw.get(&r.id) {
                Some(s) => s.set(on),
                None => {
                    sw.insert(r.id.clone(), signal(on));
                }
            }
        }
        *self.base.borrow_mut() = base;
    }

    pub fn switch_of(&self, id: &str) -> Signal<bool> {
        *self
            .switches
            .borrow_mut()
            .entry(id.to_string())
            .or_insert_with(|| signal(false))
    }

    /// 写回值：见模块文档「开关状态的真相源」。
    pub fn to_value(&self) -> Value {
        let mut out = self.base.borrow().clone();
        let sw = self.switches.borrow();
        for r in self.rows.get() {
            let Some(s) = sw.get(&r.id) else { continue };
            let on = s.get();
            if out.contains_key(&r.id) || on != r.default_on {
                out.insert(r.id.clone(), Value::Bool(on));
            }
        }
        Value::Object(out)
    }

    /// 打开用户目录下的模块目录（不存在就先建）。
    fn open_user_dir(&self) -> Result<(), String> {
        let root = crate::user_dir::user_config_dir_for_mode(crate::mode::get())
            .ok_or_else(|| "找不到用户配置目录".to_string())?;
        let path = root.join(self.user_dir.borrow().as_str());
        std::fs::create_dir_all(&path)
            .map_err(|e| format!("创建目录失败 {}: {e}", path.display()))?;
        crate::open_external::open_with_default_app(&path)
    }
}

/// RPC 结果 → 行。字段缺失的条目跳过（没有 id 就没法开关）。
pub fn rows_from_value(v: &Value) -> Vec<ModuleRow> {
    v.get("modules")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let id = m.get("id")?.as_str()?.to_string();
                    let s = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    Some(ModuleRow {
                        label: {
                            let l = s("label");
                            if l.is_empty() { id.clone() } else { l }
                        },
                        description: s("description"),
                        file: s("file"),
                        user: m.get("user").and_then(Value::as_bool).unwrap_or(false),
                        default_on: m.get("defaultOn").and_then(Value::as_bool).unwrap_or(false),
                        id,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 行说明：内置模块给例子；用户模块给文件位置（用户目录下），说明它从哪来。
fn row_desc(r: &ModuleRow) -> String {
    if r.user {
        format!("用户文件 {}", r.file.replace('/', "\\"))
    } else {
        r.description.clone()
    }
}

pub fn build_emoji_modules_dialog(ui: &EmojiModulesUi) -> Element {
    // 开关信号在 `seed` 里先造好，行构建器里只取：`host_signal` 会把行构建器里现造的信号
    // 圈进那一行的作用域，行一重建就丢（同 comment_dicts_dialog 的说明）。
    let row_ui = ui.clone();
    let list = Element::col()
        .width_match()
        .padding_xy(16, 2)
        .child(Element::host_signal(ui.rows, move |r: ModuleRow| {
            let desc = row_desc(&r);
            Element::col()
                .width_match()
                .child(Element::divider())
                .child(crate::widgets::setting_row(
                    &r.label,
                    (!desc.is_empty()).then_some(desc.as_str()),
                    Element::checkbox("启用", row_ui.switch_of(&r.id))
                        .small()
                        .fg_role(Role::Text),
                ))
        }));
    let ui_open = ui.clone();
    let open_btn = Element::button("打开用户目录")
        .small()
        .outline_soft()
        .tooltip(
            "打开用户目录下的 emoji 文件夹。放进 OpenCC 格式的 .txt（每行「词<Tab>符号1 符号2…」），\
             重开本窗口后出现在列表里，勾选生效；内置模块不在这里",
        )
        .on_click(move |_| {
            if let Err(e) = ui_open.open_user_dir() {
                ui_open.feedback.set(Some((ToastKind::Error, e)));
            }
        });
    crate::dialogs::field_dialogs::build_group_dialog_shell(
        ui.visible,
        "Emoji/OpenCC 附加模块",
        list,
        vec![open_btn],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<ModuleRow> {
        rows_from_value(&json!({ "modules": [
            { "id": "frost_emoji", "label": "白霜 Emoji", "defaultOn": true },
            { "id": "frost_greek", "label": "希腊字母", "defaultOn": false },
            { "id": "猫猫", "label": "猫猫", "file": "emoji/猫猫.txt", "user": true },
            { "label": "缺 id" },
        ]}))
    }

    /// 什么都没动：写出去的与读进来的逐字相同（不制造假的未保存改动）。
    #[test]
    fn untouched_round_trips_the_config_table() {
        let ui = EmojiModulesUi::new(signal(None));
        ui.rows.set(rows());
        let cfg = json!({ "frost_emoji": true, "frost_greek": false, "已删的文件": true });
        ui.seed(cfg.as_object());
        assert_eq!(ui.to_value(), cfg);
    }

    /// 勾上用户模块 → 写入该键；表里残留的键（文件已删）原样保留。
    #[test]
    fn toggling_writes_only_what_changed() {
        let ui = EmojiModulesUi::new(signal(None));
        ui.rows.set(rows());
        ui.seed(json!({ "frost_emoji": true, "已删的文件": true }).as_object());
        ui.switch_of("猫猫").set(true);
        assert_eq!(
            ui.to_value(),
            json!({ "frost_emoji": true, "已删的文件": true, "猫猫": true })
        );
    }

    /// 配置里没写的模块按出厂值；缺 id 的条目跳过。
    #[test]
    fn seed_falls_back_to_default_on() {
        let ui = EmojiModulesUi::new(signal(None));
        let r = rows();
        assert_eq!(r.len(), 3);
        ui.rows.set(r);
        ui.seed(None);
        assert!(ui.switch_of("frost_emoji").get());
        assert!(!ui.switch_of("frost_greek").get());
        assert_eq!(row_desc(&ui.rows.get()[2]), "用户文件 emoji\\猫猫.txt");
    }
}
