//! 「显示内容」对话框：内置格与自定义按钮同列，可勾选、可拖拽排序、可增删按钮。
//!
//! # 为什么不复用 `build_toggle_rows_dialog`
//!
//! 那个组件的行是**同构且静态**的（构建时给定 `Vec<ToggleRow>`）。这里两样都不成立：
//!
//! - 行**异构**：内置格只有开关，自定义按钮还要能改和删；
//! - 行**动态**：「新建」当场多一行。
//!
//! 把这两件事塞进通用组件，快捷输入那个调用方（同构静态）就要跟着背上复杂度，
//! 而它一项也用不上。本仓 `field_dialogs.rs` 里 9 个对话框本就各写各的外壳，
//! 按这个惯例开一个专用的，比撑大通用组件诚实。
//!
//! # 状态
//!
//! - `switches`：**每行一个** `Signal<bool>`，按行 value 索引。自定义按钮新建时往里插。
//! - `buttons`：自定义按钮的定义，新建/编辑/删除都改它，行区随之重建。
//! - `order`：行序真相源（元素是行 value：内置键或 `custom:<id>`）。
//!
//! ⚠️ 用户手写的、`buttons` 里没有对应定义的引用（如拼错的 `custom:smy`）**不进这三者**
//! ——界面显示不了它。它由写回时的 `merge_preserving_foreign` 原样保住，见那里。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use windui::prelude::*;

use super::toolbar_button_edit::{ButtonForm, NEW_BUTTON, build_button_form_dialog};

/// 自定义按钮的界面态（对应 core 的 `[[ui.toolbar.buttons]]` 一项）。
#[derive(Clone, PartialEq)]
pub struct ButtonRow {
    pub id: String,
    pub label: String,
    pub action: String,
}

/// 行 value：自定义按钮在 `items` 里的引用形态。
pub fn custom_value(id: &str) -> String {
    format!("custom:{id}")
}

/// 「显示内容」对话框的全部可变状态。
#[derive(Clone)]
pub struct ToolbarItemsUi {
    pub visible: Signal<bool>,
    /// 行 value → 开关。内置项建好后不变，自定义按钮随增删进出。
    pub switches: Rc<RefCell<HashMap<String, Signal<bool>>>>,
    pub buttons: Signal<Vec<ButtonRow>>,
    pub order: Signal<Vec<String>>,
    /// 按钮新建/编辑表单。
    pub form: ButtonForm,
}

impl ToolbarItemsUi {
    /// 取某行的开关；不存在时**当场建一个**（新建按钮那一刻行还没有开关）。
    pub fn switch_of(&self, value: &str, default_on: bool) -> Signal<bool> {
        if let Some(s) = self.switches.borrow().get(value) {
            return *s;
        }
        let s = signal(default_on);
        self.switches.borrow_mut().insert(value.to_string(), s);
        s
    }

    /// 当前开着的行 value（写回用）。
    pub fn enabled_values(&self) -> Vec<String> {
        self.switches
            .borrow()
            .iter()
            .filter(|(_, s)| s.get())
            .map(|(v, _)| v.clone())
            .collect()
    }

    /// 除第 `skip` 个之外的全部按钮 id（供表单挡重名；`NEW_BUTTON` = 一个都不跳）。
    pub fn other_ids(&self, skip: usize) -> Vec<String> {
        self.buttons
            .get()
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != skip)
            .map(|(_, b)| b.id.clone())
            .collect()
    }

    /// 提交表单：新建则追加并上架，编辑则替换。
    ///
    /// ⚠️ **改 id 等于换了一个引用目标**：`items` 里那条 `custom:<旧id>` 必须跟着改名，
    /// 否则它会变成一条指不到任何按钮的悬空引用，而那个按钮又没有任何一行引用它
    /// ——表现是「改了个名字，按钮就从工具栏上消失了」。
    pub fn commit_form(&self, index: usize, id: String, label: String, action: String) {
        let row = ButtonRow {
            id: id.clone(),
            label,
            action,
        };
        let value = custom_value(&id);
        if index == NEW_BUTTON {
            self.buttons.update(|v| v.push(row));
            self.switch_of(&value, true); // 新建即上架
            self.order.update(|v| v.push(value));
            return;
        }
        let old_id = self.buttons.get().get(index).map(|b| b.id.clone());
        self.buttons.update(|v| {
            if let Some(slot) = v.get_mut(index) {
                *slot = row;
            }
        });
        if let Some(old) = old_id.filter(|o| *o != id) {
            let old_value = custom_value(&old);
            // 开关连同它的开/关状态一起改名，别让用户新填的名字默认变成"开着"。
            let was_on = self
                .switches
                .borrow()
                .get(&old_value)
                .map(|s| s.get())
                .unwrap_or(true);
            self.switches.borrow_mut().remove(&old_value);
            self.switch_of(&value, was_on);
            self.order.update(|v| {
                for x in v.iter_mut() {
                    if x == &old_value {
                        *x = value.clone();
                    }
                }
            });
        }
    }

    /// 删除第 `index` 个按钮：定义、开关、行序里的引用一并去掉。
    ///
    /// ⚠️ **只删这一条 id 的引用**，不做「清理所有指不到按钮的引用」——后者会把用户
    /// 手写的、界面不认识的条目一并吃掉，那正是刚修好的那个数据丢失。
    pub fn delete_button(&self, index: usize) {
        let Some(b) = self.buttons.get().get(index).cloned() else {
            return;
        };
        let value = custom_value(&b.id);
        self.buttons.update(|v| {
            if index < v.len() {
                v.remove(index);
            }
        });
        self.switches.borrow_mut().remove(&value);
        self.order.update(|v| v.retain(|x| x != &value));
    }
}

/// 内置格的显示信息（value / 标签 / 说明），由调用方按 manifest 的 options 给出。
#[derive(Clone)]
pub struct BuiltinRow {
    pub value: String,
    pub label: String,
    pub desc: String,
}

/// 构建对话框。
///
/// `min_enabled` = 至少要留几项开着（越界时弹 toast，不置灰——灰掉的开关看不出来，
/// 点下去没反应反而更像卡住，见 `field_dialogs::switch_with_floor` 的同款结论）。
pub fn build_toolbar_items_dialog(
    ui: &ToolbarItemsUi,
    title: &str,
    hint: &str,
    builtins: Vec<BuiltinRow>,
    defaults: Vec<String>,
    min_enabled: usize,
) -> Element {
    let meta: Rc<Vec<BuiltinRow>> = Rc::new(builtins);
    let declared: Vec<String> = meta.iter().map(|b| b.value.clone()).collect();
    let visible = ui.visible;

    let row_ui = ui.clone();
    let row_meta = meta.clone();
    let grid = Element::col().width_match().padding_xy(16, 2).child(
        Element::reorder_list_signal(ui.order, move |value: String, handle| {
            let is_custom = value.starts_with("custom:");
            let sig = row_ui.switch_of(&value, true);
            // 标签/说明：内置项查 meta，自定义按钮查 buttons（它可能刚被改过名）。
            let (label, desc, btn_index) = if is_custom {
                let id = value.trim_start_matches("custom:").to_string();
                match row_ui.buttons.get().iter().position(|b| b.id == id) {
                    Some(i) => {
                        let b = row_ui.buttons.get()[i].clone();
                        // 说明显示**人话摘要**而非表达式源码：源码里的双反斜杠是给 cmdbar
                        // 看的，摆在界面上只会让用户以为自己的路径写错了。
                        (
                            format!("{}（自定义）", b.label),
                            super::action_expr::summary(&b.action),
                            Some(i),
                        )
                    }
                    // order 里有、buttons 里没有：不该发生（两者同源增删），但也不能
                    // 让整行塌成空白。
                    None => (value.clone(), String::new(), None),
                }
            } else {
                match row_meta.iter().find(|b| b.value == value) {
                    Some(b) => (b.label.clone(), b.desc.clone(), None),
                    None => (value.clone(), String::new(), None),
                }
            };

            let mut ctrl = Element::row().cross(Align::Center).spacing(6);
            if let Some(i) = btn_index {
                let e_ui = row_ui.clone();
                let d_ui = row_ui.clone();
                ctrl =
                    ctrl.child(Element::button("编辑").small().outline_soft().on_click(
                        move |_| {
                            if let Some(b) = e_ui.buttons.get().get(i) {
                                e_ui.form.open_edit(i, &b.id, &b.label, &b.action);
                            }
                        },
                    ))
                    .child(
                        Element::button("删除")
                            .small()
                            .outline_soft()
                            .on_click(move |_| d_ui.delete_button(i)),
                    );
            }
            ctrl = ctrl.child(switch_with_floor(sig, &row_ui, min_enabled));

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
                            crate::widgets::setting_row(
                                &label,
                                (!desc.is_empty()).then_some(desc.as_str()),
                                ctrl,
                            )
                            .weight(1.0),
                        ),
                )
        })
        .on_reorder({
            let order = ui.order;
            move |_ctx, from, to| {
                order.update(|v| {
                    let x = v.remove(from);
                    v.insert(to.min(v.len()), x);
                })
            }
        }),
    );

    let reset_ui = ui.clone();
    let new_ui = ui.clone();
    let frame = Element::col()
        .width(500)
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
                    Element::button("新建按钮…")
                        .small()
                        .outline_soft()
                        .on_click(move |_| new_ui.form.open_new()),
                )
                .child(
                    Element::button("恢复默认")
                        .small()
                        .outline_soft()
                        .on_click(move |_| {
                            // 只复位内置格的开关与顺序。**自定义按钮不动**：它们是用户
                            // 创建的内容，不是"设置项"，一并清掉等于替他删数据。
                            for v in &declared {
                                let on = defaults.iter().any(|d| d == v);
                                reset_ui.switch_of(v, on).set(on);
                            }
                            let customs: Vec<String> = reset_ui
                                .buttons
                                .get()
                                .iter()
                                .map(|b| custom_value(&b.id))
                                .collect();
                            let mut next = declared.clone();
                            next.extend(customs);
                            reset_ui.order.set(next);
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

    // 按钮表单挂在同一棵子树上：它是本对话框的二级弹层（页面 → 显示内容 → 编辑按钮，
    // 深度 2，正好是仓规的上限）。
    let form_ui = ui.clone();
    let form_ui2 = ui.clone();
    let form_dlg = build_button_form_dialog(
        &ui.form,
        move |skip| form_ui.other_ids(skip),
        move |idx, id, label, action| form_ui2.commit_form(idx, id, label, action),
    );

    Element::stack()
        .child(Element::dialog(visible, frame))
        .child(form_dlg)
}

/// 带下限守卫的行开关：越界时弹 toast 而不是置灰（理由同 `field_dialogs` 那处）。
fn switch_with_floor(sig: Signal<bool>, ui: &ToolbarItemsUi, min_enabled: usize) -> Element {
    if min_enabled == 0 {
        return Element::switch(sig);
    }
    let ui = ui.clone();
    Element::switch(sig).on_toggle(move |ctx| {
        if sig.get() && ui.enabled_values().len() <= min_enabled {
            ctx.toast_err(if min_enabled == 1 {
                "至少要保留一项".to_string()
            } else {
                format!("至少要保留 {min_enabled} 项")
            });
            return;
        }
        sig.set(!sig.get());
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_value_round_trips() {
        assert_eq!(custom_value("sym"), "custom:sym");
        assert_eq!(custom_value("sym").trim_start_matches("custom:"), "sym");
    }
}
