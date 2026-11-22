//! 软键盘编辑对话框：顶部下拉选面，中间一整块键盘，下方编辑选中的键。
//!
//! 数据住在 [`SoftKbdData`]（`AppState` 里，比设置树长命：主窗重建后未提交的改动还在）；
//! 本文件里的 `Signal` 都是界面态，随设置树一起建、一起回收，构建时从数据同步一次。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use windui::prelude::*;

use super::SoftKbdData;
use super::model::{SLOT_ROWS, all_slot_names, slot_caption};
use crate::pages::dict::reactive::reactive_effect;

/// 「没有选中键」的哨兵。
const NONE: usize = usize::MAX;
/// 一个键位单元（含间距）的宽度。整行 15u。
const UNIT: f32 = 46.0;
const GAP: i32 = 4;
const CAP_H: i32 = 54;
/// 键盘区宽度：15u 减去最后一个间距。对话框宽度由它推出。
const BOARD_W: i32 = (15.0 * UNIT) as i32 - GAP;
const PAD: i32 = 20;

/// 对话框的界面态。
#[derive(Clone)]
pub struct EditorUi {
    pub visible: Signal<bool>,
    data: SoftKbdData,
    /// 打开时的数据快照：「取消」退回这里。
    snapshot: Rc<RefCell<Option<super::model::Model>>>,
    page_names: Signal<Vec<String>>,
    page_sel: Signal<usize>,
    page_info: Signal<String>,
    can_delete: Signal<bool>,
    can_reset: Signal<bool>,
    editable: Signal<bool>,
    caps_base: Rc<Vec<Signal<String>>>,
    caps_shift: Rc<Vec<Signal<String>>>,
    caps_mod: Rc<Vec<Signal<bool>>>,
    selected: Signal<usize>,
    sel_title: Signal<String>,
    sel_default: Signal<String>,
    sel_modified: Signal<bool>,
    edit_base: Signal<String>,
    edit_shift: Signal<String>,
    /// 程序回填输入框期间为真：输入框的 effect 据此不把回填当成用户编辑。
    syncing: Rc<Cell<bool>>,
    form: NameForm,
}

/// 新建 / 改名小表单。
#[derive(Clone)]
struct NameForm {
    visible: Signal<bool>,
    title: Signal<String>,
    name: Signal<String>,
    copy: Signal<bool>,
    /// true = 新建，false = 改名。
    creating: Signal<bool>,
    error: Signal<String>,
}

impl EditorUi {
    pub fn new(data: SoftKbdData) -> Self {
        let n = all_slot_names().len();
        let ui = Self {
            visible: signal(false),
            data,
            snapshot: Rc::new(RefCell::new(None)),
            page_names: signal(Vec::new()),
            page_sel: signal(0),
            page_info: signal(String::new()),
            can_delete: signal(false),
            can_reset: signal(false),
            editable: signal(true),
            caps_base: Rc::new((0..n).map(|_| signal(String::new())).collect()),
            caps_shift: Rc::new((0..n).map(|_| signal(String::new())).collect()),
            caps_mod: Rc::new((0..n).map(|_| signal(false)).collect()),
            selected: signal(NONE),
            sel_title: signal(String::new()),
            sel_default: signal(String::new()),
            sel_modified: signal(false),
            edit_base: signal(String::new()),
            edit_shift: signal(String::new()),
            syncing: Rc::new(Cell::new(false)),
            form: NameForm {
                visible: signal(false),
                title: signal(String::new()),
                name: signal(String::new()),
                copy: signal(false),
                creating: signal(true),
                error: signal(String::new()),
            },
        };
        ui.sync_all();
        ui
    }

    /// 打开对话框。没有未提交改动时先从 core 现拉一次：别处（手写文件、另一个设置窗）
    /// 可能改过；有未提交改动时不拉，免得冲掉用户还没保存的编辑。
    pub fn open(&self) {
        if !self.data.is_dirty() {
            self.data.reload();
        }
        *self.snapshot.borrow_mut() = Some(self.data.model.borrow().clone());
        self.sync_all();
        self.visible.set(true);
    }

    /// 确认：收下这次的编辑（仍待「应用设置」提交）。先丢快照再关——关窗 effect 据快照
    /// 还在不在区分「确认」与 Esc（Esc 只置 `visible = false`，按取消处理）。
    fn confirm(&self) {
        self.snapshot.borrow_mut().take();
        self.visible.set(false);
    }

    /// 关窗 effect：对话框被关掉而快照还在 ⇒ 不是经「确认」关的（Esc），按取消退回。
    fn on_visibility_change(&self) {
        if !self.visible.get() && self.snapshot.borrow().is_some() {
            self.cancel();
        }
    }

    /// 取消（底栏按钮、右上角 ×、Esc）：退回打开时的样子。
    fn cancel(&self) {
        if let Some(m) = self.snapshot.borrow_mut().take() {
            *self.data.model.borrow_mut() = m;
        }
        self.sync_all();
        self.visible.set(false);
    }

    fn page(&self) -> usize {
        self.page_sel.get()
    }

    /// 数据 → 全部界面态（面列表 + 当前面）。
    fn sync_all(&self) {
        let m = self.data.model.borrow();
        let names: Vec<String> = m
            .pages
            .iter()
            .map(|p| {
                if p.user_created {
                    format!("{}（自建）", p.name)
                } else if p.modified() {
                    format!("{}（已修改）", p.name)
                } else {
                    p.name.clone()
                }
            })
            .collect();
        let n = names.len();
        drop(m);
        self.page_names.set(names);
        if self.page_sel.get() >= n {
            self.page_sel.set(n.saturating_sub(1));
        }
        self.sync_page();
    }

    /// 当前面 → 键帽、按钮状态、选中键的编辑区。
    fn sync_page(&self) {
        let m = self.data.model.borrow();
        let Some(p) = m.pages.get(self.page()) else {
            for (i, _) in all_slot_names().iter().enumerate() {
                self.caps_base[i].set(String::new());
                self.caps_shift[i].set(String::new());
                self.caps_mod[i].set(false);
            }
            let err = self.data.load_error.borrow().clone();
            self.page_info.set(if err.is_empty() {
                "没有可编辑的面".into()
            } else {
                format!("读取软键盘失败：{err}")
            });
            self.can_delete.set(false);
            self.can_reset.set(false);
            self.editable.set(false);
            drop(m);
            self.selected.set(NONE);
            self.sync_selection();
            return;
        };
        for (i, slot) in all_slot_names().iter().enumerate() {
            let k = p.key(slot);
            self.caps_base[i].set(k.and_then(|k| k.base.clone()).unwrap_or_default());
            self.caps_shift[i].set(k.and_then(|k| k.shift.clone()).unwrap_or_default());
            self.caps_mod[i].set(!p.user_created && k.is_some_and(|k| k.modified()));
        }
        self.page_info.set(if p.send_keys {
            "键盘面：点击发送真实按键，不可修改。新建时复制它，可得到一张能改的符号面".into()
        } else if p.user_created {
            format!("自建面（id：{}）", p.id)
        } else if p.modified() {
            "预置面 · 蓝框是改过的键".into()
        } else {
            "预置面".into()
        });
        self.can_delete.set(p.user_created);
        self.can_reset.set(!p.user_created && p.modified());
        self.editable.set(!p.send_keys);
        drop(m);
        if !self.editable.get() {
            self.selected.set(NONE);
        }
        self.sync_selection();
    }

    /// 选中键 → 编辑区（回填输入框时挂 `syncing`，不让 effect 当成用户编辑）。
    fn sync_selection(&self) {
        let i = self.selected.get();
        let slots = all_slot_names();
        let m = self.data.model.borrow();
        let key = slots
            .get(i)
            .and_then(|slot| m.pages.get(self.page())?.key(slot).cloned());
        let user_created = m.pages.get(self.page()).is_some_and(|p| p.user_created);
        drop(m);
        // ★ 编辑区三种状态（选中 / 未选中 / 键盘面）都占同样两行：对话框高度不随状态跳。
        // 未选中时输入框置灰、清空，第二行换成操作提示。
        let Some(k) = key else {
            self.sel_title.set("未选择".into());
            self.sel_default.set(if self.editable.get() {
                "点选上方的键帽进行修改。Shift 符号是按住 Shift 时输出的字符".into()
            } else {
                "这一面不可修改".into()
            });
            self.sel_modified.set(false);
            self.syncing.set(true);
            self.edit_base.set(String::new());
            self.edit_shift.set(String::new());
            self.syncing.set(false);
            return;
        };
        self.sel_title
            .set(format!("键位 {}", slot_caption(&k.slot)));
        // 不给符号套括号：符号本身常常就是引号或括号，套上之后反而认不出来。
        let shown = |v: Option<&str>| v.unwrap_or("（空）").to_string();
        self.sel_default.set(if user_created {
            "自建面没有默认值；「清空」后按下这个键不输出任何字符".into()
        } else {
            format!(
                "默认：符号 {}　　Shift 符号 {}",
                shown(k.default_of(false)),
                shown(k.default_of(true))
            )
        });
        self.sel_modified.set(!user_created && k.modified());
        self.syncing.set(true);
        self.edit_base.set(k.base.clone().unwrap_or_default());
        self.edit_shift.set(k.shift.clone().unwrap_or_default());
        self.syncing.set(false);
    }

    fn select(&self, i: usize) {
        if !self.editable.get() {
            return;
        }
        self.selected
            .set(if self.selected.get() == i { NONE } else { i });
        self.sync_selection();
    }

    /// 输入框里的编辑 → 数据。
    fn on_edit(&self, shift: bool) {
        if self.syncing.get() {
            return;
        }
        let i = self.selected.get();
        let Some(slot) = all_slot_names().get(i).copied() else {
            return;
        };
        let v = if shift {
            self.edit_shift.get()
        } else {
            self.edit_base.get()
        };
        let page = self.page();
        self.data
            .model
            .borrow_mut()
            .set_key(page, slot, shift, Some(v));
        self.after_key_change(i);
    }

    /// 改了一个键之后只刷新受影响的那几处（不整页重同步：那会把输入框的光标冲掉）。
    fn after_key_change(&self, i: usize) {
        let m = self.data.model.borrow();
        if let Some(p) = m.pages.get(self.page())
            && let Some(k) = p.key(all_slot_names()[i])
        {
            self.caps_base[i].set(k.base.clone().unwrap_or_default());
            self.caps_shift[i].set(k.shift.clone().unwrap_or_default());
            let modded = !p.user_created && k.modified();
            self.caps_mod[i].set(modded);
            self.sel_modified.set(modded);
        }
        drop(m);
        self.refresh_page_flags();
    }

    /// 面列表的「（已修改）」标记与「恢复默认」按钮。
    fn refresh_page_flags(&self) {
        let m = self.data.model.borrow();
        let Some(p) = m.pages.get(self.page()) else {
            return;
        };
        let label = if p.user_created {
            format!("{}（自建）", p.name)
        } else if p.modified() {
            format!("{}（已修改）", p.name)
        } else {
            p.name.clone()
        };
        let reset = !p.user_created && p.modified();
        drop(m);
        let idx = self.page();
        self.page_names.update(|v| {
            if let Some(slot) = v.get_mut(idx)
                && *slot != label
            {
                *slot = label;
            }
        });
        self.can_reset.set(reset);
    }

    fn reset_selected_key(&self) {
        let i = self.selected.get();
        let Some(slot) = all_slot_names().get(i).copied() else {
            return;
        };
        let page = self.page();
        self.data.model.borrow_mut().reset_key(page, slot);
        self.after_key_change(i);
        self.sync_selection();
    }

    fn clear_selected_key(&self) {
        let i = self.selected.get();
        let Some(slot) = all_slot_names().get(i).copied() else {
            return;
        };
        let page = self.page();
        {
            let mut m = self.data.model.borrow_mut();
            m.set_key(page, slot, false, None);
            m.set_key(page, slot, true, None);
        }
        self.after_key_change(i);
        self.sync_selection();
    }

    fn open_new_form(&self) {
        let f = &self.form;
        f.creating.set(true);
        f.title.set("新建键盘面".into());
        f.name.set(String::new());
        f.copy.set(false);
        f.error.set(String::new());
        f.visible.set(true);
    }

    fn open_rename_form(&self) {
        let name = self
            .data
            .model
            .borrow()
            .pages
            .get(self.page())
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let f = &self.form;
        f.creating.set(false);
        f.title.set("重命名".into());
        f.name.set(name);
        f.error.set(String::new());
        f.visible.set(true);
    }

    fn submit_form(&self) {
        let f = &self.form;
        let name = f.name.get().trim().to_string();
        if name.is_empty() {
            f.error.set("请填写名称".into());
            return;
        }
        let page = self.page();
        if f.creating.get() {
            let copy = f.copy.get().then_some(page);
            let idx = self.data.model.borrow_mut().create_page(&name, copy);
            self.selected.set(NONE);
            // 先切下标再同步：不靠下一帧 effect 补，界面态在这一刻就与数据一致。
            self.page_sel.set(idx);
            self.sync_all();
        } else {
            self.data.model.borrow_mut().rename_page(page, &name);
            self.sync_all();
        }
        f.visible.set(false);
    }

    fn delete_page(&self) {
        let page = self.page();
        if self.data.model.borrow_mut().delete_page(page) {
            self.selected.set(NONE);
            self.page_sel.set(page.saturating_sub(1));
            self.sync_all();
        }
    }

    fn reset_page(&self) {
        let page = self.page();
        if self.data.model.borrow_mut().reset_page(page) {
            self.sync_all();
        }
    }
}

// ───────────────────────── 构建 ─────────────────────────

/// 键帽描边/底色。
fn ring_color(selected: bool, modified: bool) -> Color {
    let t = windui::theme::current();
    if selected || modified {
        Role::Accent.resolve(&t)
    } else {
        Role::Border.resolve(&t)
    }
}

fn cap_bg(selected: bool) -> Color {
    let t = windui::theme::current();
    if selected {
        let (a, b) = (Role::Surface.resolve(&t), Role::Accent.resolve(&t));
        mix(a, b, 0.18)
    } else {
        Role::Surface.resolve(&t)
    }
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::rgba(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b), a.a)
}

fn cap_width(units: f32) -> i32 {
    (units * UNIT).round() as i32 - GAP
}

/// 一个可编辑键帽：左上角原键、右上角 Shift 层、正中基础层。
fn key_cap(ui: &EditorUi, i: usize, slot: &str, units: f32) -> Element {
    let (sel, modded) = (ui.selected, ui.caps_mod[i]);
    let editable = ui.editable;
    let inner = Element::col()
        .width_match()
        .weight(1.0)
        .padding_xy(5, 3)
        .corner(5.0)
        .bg_when(move || cap_bg(sel.get() == i))
        .child(
            Element::row()
                .width_match()
                .child(
                    Element::label(slot_caption(slot))
                        .font_size(10.0)
                        .fg_role(Role::TextSubtle),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::label_signal(ui.caps_shift[i])
                        .font_size(11.0)
                        .fg_role(Role::TextMuted)
                        .max_lines(1),
                ),
        )
        .child(
            Element::label_signal(ui.caps_base[i])
                .font_size(16.0)
                .fg_role(Role::Text)
                .width_match()
                .text_align(Align::Center)
                .max_lines(1),
        );
    let click = ui.clone();
    Element::col()
        .width(cap_width(units))
        .height(CAP_H)
        .padding(1)
        .corner(6.0)
        .bg_when(move || ring_color(sel.get() == i, modded.get()))
        .clickable()
        .enabled_when(move || editable.get())
        .on_click(move |_| click.select(i))
        .child(inner)
}

/// 不可编辑的功能键（Tab / Enter / Shift…）：只为让它看起来是块键盘。
fn fn_cap(label: &str, units: f32) -> Element {
    Element::col()
        .width(cap_width(units))
        .height(CAP_H)
        .corner(6.0)
        .bg_role(Role::SurfaceAlt)
        .padding_xy(6, 4)
        .child(
            Element::label(label)
                .font_size(10.5)
                .fg_role(Role::TextDisabled),
        )
}

fn keyboard(ui: &EditorUi) -> Element {
    let slots = all_slot_names();
    let mut idx = 0usize;
    let mut row_of = |r: usize, lead: Option<(&str, f32)>, tail: Option<(&str, f32)>| {
        let mut row = Element::row().spacing(GAP);
        if let Some((l, u)) = lead {
            row = row.child(fn_cap(l, u));
        }
        for (c, slot) in SLOT_ROWS[r].iter().enumerate() {
            // 第二行最后一个键（\）是 1.5u，与真实键盘一致，行宽才凑得满 15u。
            let units = if r == 1 && c == SLOT_ROWS[1].len() - 1 {
                1.5
            } else {
                1.0
            };
            debug_assert_eq!(slots[idx], *slot);
            row = row.child(key_cap(ui, idx, slot, units));
            idx += 1;
        }
        if let Some((l, u)) = tail {
            row = row.child(fn_cap(l, u));
        }
        row
    };
    Element::col()
        .width(BOARD_W)
        .spacing(GAP)
        .child(row_of(0, None, Some(("Backspace", 2.0))))
        .child(row_of(1, Some(("Tab", 1.5)), None))
        .child(row_of(2, Some(("Caps", 1.75)), Some(("Enter", 2.25))))
        .child(row_of(3, Some(("Shift", 2.25)), Some(("Shift", 2.75))))
}

fn edit_area(ui: &EditorUi) -> Element {
    let sel = ui.selected;
    let (u_reset, u_clear) = (ui.clone(), ui.clone());
    let modded = ui.sel_modified;
    let user_page = ui.can_delete;
    let editing = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(10)
        .child(
            Element::label_signal(ui.sel_title)
                .font_size(13.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width(70),
        )
        .child(
            Element::label("符号")
                .font_size(12.5)
                .fg_role(Role::TextMuted),
        )
        .child(
            Element::text_input(ui.edit_base, "空")
                .width(120)
                .enabled_when(move || sel.get() != NONE),
        )
        .child(
            Element::label("Shift 符号")
                .font_size(12.5)
                .fg_role(Role::TextMuted),
        )
        .child(
            Element::text_input(ui.edit_shift, "空")
                .width(120)
                .enabled_when(move || sel.get() != NONE),
        )
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("清空")
                .small()
                .outline_soft()
                .tooltip("两层都设为空：按下这个键什么都不输出")
                .enabled_when(move || sel.get() != NONE)
                .on_click(move |_| u_clear.clear_selected_key()),
        )
        .child(
            Element::button("恢复默认")
                .small()
                .outline_soft()
                // 自建面没有默认值可回，整个按钮不出现（置灰会让人以为是条件没满足）。
                .visible_when(move || !user_page.get())
                .enabled_when(move || modded.get())
                .on_click(move |_| u_reset.reset_selected_key()),
        );
    let default_line = Element::label_signal(ui.sel_default)
        .font_size(12.0)
        .fg_role(Role::TextSubtle)
        .max_lines(1);
    Element::col()
        .width_match()
        .spacing(6)
        .padding_xy(PAD, 12)
        .child(editing)
        .child(default_line)
}

fn name_form(ui: &EditorUi) -> Element {
    let f = ui.form.clone();
    let (submit, submit2) = (ui.clone(), ui.clone());
    let visible = f.visible;
    let creating = f.creating;
    let error = f.error;
    let frame = Element::col()
        .width(360)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label_signal(f.title)
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
                .padding_xy(18, 14)
                .spacing(10)
                .child(
                    Element::text_input(f.name, "键盘面的名称，显示在面板标签上")
                        .width_match()
                        .on_submit(move |_| submit2.submit_form()),
                )
                .child(
                    Element::checkbox("复制当前面的全部键位", f.copy)
                        .small()
                        .fg_role(Role::Text)
                        .visible_when(move || creating.get()),
                )
                .child(
                    Element::label_signal(error)
                        .font_size(12.0)
                        .fg_role(Role::Danger)
                        .visible_when(move || error.with(|e| !e.is_empty())),
                ),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .padding_xy(16, 12)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| visible.set(false)),
                )
                .child(
                    Element::button("确定")
                        .small()
                        .on_click(move |_| submit.submit_form()),
                ),
        );
    Element::dialog(visible, frame)
}

/// 构建对话框（经 `register_dialog` 挂到根层）。
pub fn build_dialog(ui: &EditorUi) -> Element {
    let visible = ui.visible;
    let (u_new, u_ren, u_del, u_rst, u_cancel) =
        (ui.clone(), ui.clone(), ui.clone(), ui.clone(), ui.clone());
    let (can_delete, can_reset) = (ui.can_delete, ui.can_reset);
    let u_ok = ui.clone();
    let uv = ui.clone();
    let visible_effect =
        reactive_effect(move || visible.version(), move || uv.on_visibility_change());
    let has_page = ui.page_names;

    let sel_page = ui.clone();
    let page_effect = reactive_effect(
        {
            let s = ui.page_sel;
            move || s.version()
        },
        move || {
            sel_page.selected.set(NONE);
            sel_page.sync_page();
        },
    );
    let eb = ui.clone();
    let base_effect = reactive_effect(
        {
            let s = ui.edit_base;
            move || s.version()
        },
        move || eb.on_edit(false),
    );
    let es = ui.clone();
    let shift_effect = reactive_effect(
        {
            let s = ui.edit_shift;
            move || s.version()
        },
        move || es.on_edit(true),
    );

    let toolbar = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(PAD, 12)
        .child(
            Element::label("键盘面")
                .font_size(12.5)
                .fg_role(Role::TextMuted),
        )
        .child(Element::dropdown_signal(ui.page_names, ui.page_sel).width(220))
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("新建…")
                .small()
                .outline_soft()
                .on_click(move |_| u_new.open_new_form()),
        )
        .child(
            Element::button("重命名…")
                .small()
                .outline_soft()
                .enabled_when(move || has_page.with(|v| !v.is_empty()))
                .on_click(move |_| u_ren.open_rename_form()),
        )
        .child(
            Element::button("删除")
                .small()
                .outline_soft()
                .visible_when(move || can_delete.get())
                .on_click(move |_| u_del.delete_page()),
        )
        .child(
            Element::button("恢复默认")
                .small()
                .outline_soft()
                .visible_when(move || !can_delete.get())
                .enabled_when(move || can_reset.get())
                .tooltip("撤销对这一面的全部修改（名称与所有键位）")
                .on_click(move |_| u_rst.reset_page()),
        );

    let frame = Element::col()
        .width(BOARD_W + PAD * 2)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("自定义软键盘")
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(toolbar)
        .child(
            Element::label_signal(ui.page_info)
                .max_lines(1)
                .font_size(12.0)
                .fg_role(Role::TextSubtle)
                .width_match()
                .padding_edges(Insets::new(PAD, 0, PAD, 8)),
        )
        .child(
            Element::col()
                .width_match()
                .padding_xy(PAD, 0)
                .child(keyboard(ui)),
        )
        .child(edit_area(ui))
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    Element::label("改动随底栏「应用设置」保存，保存后立即生效")
                        .font_size(12.0)
                        .fg_role(Role::TextSubtle)
                        .padding_xy(4, 0),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| u_cancel.cancel()),
                )
                .child(
                    Element::button("确认")
                        .small()
                        .on_click(move |_| u_ok.confirm()),
                ),
        )
        .child(page_effect)
        .child(visible_effect)
        .child(base_effect)
        .child(shift_effect);

    let u_x = ui.clone();
    Element::stack()
        .child(Element::dialog_closable(visible, frame, move |_| {
            u_x.cancel()
        }))
        .child(name_form(ui))
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
