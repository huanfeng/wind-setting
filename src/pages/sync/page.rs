//! 同步页的卡片：向导、状态、本机、设备、内容、存储、安全；未配置时只有向导卡，其余连同二级导航一起隐藏。

use std::cell::RefCell;
use std::rc::Rc;

use windui::prelude::*;

use super::dialogs::{btn, btn2};
use super::{ANCHOR, SyncUi, model::*};
use crate::search::anchor;
use crate::state::AppState;
use crate::widgets::{
    CardItem, Vis, card_pad, list_heading_ext, list_row, section_items, setting_row,
    setting_row_desc_signal,
};

/// 一张卡：`(二级导航名, 卡片, 何时显示)`；导航名为空的不进二级导航。
pub type Card = (Option<&'static str>, Element, Vis);

/// 二级导航与卡片标题；未配置时只显示第一项。
pub(super) const NAV: [&str; 7] = ["向导", "状态", "本机", "设备", "内容", "存储", "安全"];
pub(super) const BTN_MODIFY: &str = "修改…";
pub(super) const ROW_GROUP: &str = "同步组";

/// 向导卡的说明与按钮，随是否已配置。
pub(super) fn wizard_text(configured: bool) -> &'static str {
    if configured {
        "已配置；要换一个同步库可重新配置"
    } else {
        "用 WebDAV 或 S3 在多台设备间同步，数据先在本机加密"
    }
}

pub(super) fn wizard_button(configured: bool) -> &'static str {
    if configured {
        "重新配置…"
    } else {
        "开始配置…"
    }
}

/// 第 `i` 张卡是否显示：未配置时只有向导卡，其余卡里没有能操作的东西。
pub(super) fn card_shown(i: usize, configured: bool) -> bool {
    i == 0 || configured
}

/// 同步页的卡片。
pub fn build_page(_app: &Rc<RefCell<AppState>>, state: &AppState) -> Vec<Card> {
    let ui = state.sync.clone();
    ui.load_once();
    super::dialogs::mount(&ui, state);

    let cfg = ui.configured;
    // 定长数组：卡数与 NAV 不一致时编译不过，zip 不会悄悄截断。
    let cards: [Element; NAV.len()] = [
        anchor::wrap(wizard_card(&ui), ANCHOR, state.jump),
        status_card(&ui),
        self_card(&ui),
        devices_card(&ui),
        content_card(&ui),
        storage_card(&ui),
        security_card(&ui),
    ];
    NAV.iter()
        .zip(cards)
        .enumerate()
        .map(|(i, (name, el))| (Some(*name), el, vis(move || card_shown(i, cfg.get()))))
        .collect()
}

fn vis(f: impl Fn() -> bool + 'static) -> Vis {
    Rc::new(f)
}

/// 右侧的值文字（设备号、时间等）。
fn value(text: Signal<String>) -> Element {
    Element::label_signal(text)
        .font_size(13.0)
        .fg_role(Role::TextMuted)
        .max_lines(1)
}

fn value_and(text: Signal<String>, button: Element) -> Element {
    Element::row()
        .cross(Align::Center)
        .spacing(12)
        .child(value(text))
        .child(button)
}

/// 向导卡：未配置是一句介绍 + 「开始配置…」，已配置是「重新配置…」。
fn wizard_card(ui: &SyncUi) -> Element {
    let installed = ui.installed;
    section_items(
        NAV[0],
        vec![
            CardItem::RowWhen(
                Element::label(
                    "未安装同步组件（wind_input_sync），暂不能使用。请重新运行安装程序并选择包含数据同步。",
                )
                .font_size(12.5)
                .fg_role(Role::Warning)
                .width_match()
                .max_lines(3)
                .padding_xy(0, 10),
                vis(move || !installed.get()),
            ),
            wizard_row(ui, false),
            wizard_row(ui, true),
        ],
    )
}

fn wizard_row(ui: &SyncUi, configured: bool) -> CardItem {
    let (installed, busy, cfg) = (ui.installed, ui.busy, ui.configured);
    let u = ui.clone();
    CardItem::RowWhen(
        setting_row(
            wizard_text(configured),
            None,
            if configured {
                btn2(wizard_button(true))
            } else {
                btn(wizard_button(false))
            }
            .enabled_when(move || installed.get() && !busy.get())
            .on_click(move |_| u.open_wizard()),
        ),
        vis(move || cfg.get() == configured),
    )
}

fn status_card(ui: &SyncUi) -> Element {
    let busy = ui.busy;
    let (u_now, u_id, u_dup, u_mass, u_conf) =
        (ui.clone(), ui.clone(), ui.clone(), ui.clone(), ui.clone());
    let (needs_identity, dup) = (ui.needs_identity, ui.dup_device);
    let can_sync = ui.can_sync;
    let (error, notice, mass) = (ui.error, ui.notice, ui.mass_text);
    let conflicts = ui.conflict_summary;
    let u_int = ui.clone();
    let interval = ui.interval;
    let interval_saver = crate::pages::dict::reactive::reactive_effect(
        move || interval.version(),
        move || u_int.save_interval(),
    );
    let (u_master, enabled) = (ui.clone(), ui.enabled);
    section_items(
        NAV[1],
        vec![
            CardItem::Row(setting_row(
                "数据同步",
                Some("关闭后暂停同步，配置都保留"),
                Element::switch(enabled).on_switch_change(move |_ctx, on| u_master.set_master(on)),
            )),
            CardItem::Row(setting_row_desc_signal(
                "状态",
                ui.pending,
                value_and(
                    ui.phase,
                    btn("立即同步")
                        .enabled_when(move || can_sync.get() && enabled.get() && !busy.get())
                        .on_click(move |_| u_now.sync_now()),
                ),
            )),
            CardItem::RowWhen(
                setting_row(
                    "需要确认本机身份",
                    None,
                    btn("确认身份…")
                        .enabled_when(move || !busy.get())
                        .on_click(move |_| u_id.open_identity_dialog()),
                ),
                vis(move || needs_identity.get()),
            ),
            CardItem::RowWhen(
                Element::row()
                    .width_match()
                    .cross(Align::Center)
                    .spacing(12)
                    .padding_xy(0, 10)
                    .child(
                        Element::label_signal(error)
                            .font_size(12.5)
                            .fg_role(Role::Danger)
                            .max_lines(4)
                            .weight(1.0),
                    )
                    .child(
                        btn2("给本机改号…")
                            .visible_when(move || dup.get())
                            .on_click(move |_| u_dup.open_rename_dialog()),
                    ),
                vis(move || !error.get().is_empty()),
            ),
            CardItem::RowWhen(
                Element::label_signal(notice)
                    .font_size(12.5)
                    .fg_role(Role::TextMuted)
                    .width_match()
                    .max_lines(4)
                    .padding_xy(0, 10),
                vis(move || !notice.get().is_empty()),
            ),
            CardItem::RowWhen(
                setting_row_desc_signal(
                    "同步已暂停",
                    mass,
                    Element::button("确认删除…")
                        .small()
                        .danger()
                        .enabled_when(move || !busy.get())
                        .on_click(move |_| u_mass.request_mass_delete()),
                ),
                vis(move || !mass.get().is_empty()),
            ),
            CardItem::Row(setting_row("上次成功", None, value(ui.last_ok))),
            CardItem::RowWhen(
                setting_row(
                    "冲突",
                    Some("两台设备改了同一个文件，需要你决定保留哪份"),
                    value_and(
                        conflicts,
                        btn2("查看…").on_click(move |_| u_conf.conflicts_dialog.set(true)),
                    ),
                ),
                vis(move || !conflicts.get().is_empty()),
            ),
            CardItem::Row(setting_row(
                "同步间隔",
                Some(INTERVAL_DESC),
                Element::row()
                    .cross(Align::Center)
                    .child(
                        Element::number_input(interval, 1.0, MAX_INTERVAL_MIN as f64, 1.0)
                            .unit("分钟"),
                    )
                    .child(interval_saver),
            )),
        ],
    )
}

/// 本机卡：设备号、显示名称、同步组；改号与更换组走各自的对话框。
fn self_card(ui: &SyncUi) -> Element {
    let (u_rename, u_group, needs_identity) = (ui.clone(), ui.clone(), ui.needs_identity);
    let name = ui.device_name.map(|n| {
        if n.trim().is_empty() {
            "未设置".to_string()
        } else {
            n.clone()
        }
    });
    let u_rename2 = u_rename.clone();
    section_items(
        NAV[2],
        vec![
            CardItem::Row(setting_row(
                "显示名称",
                None,
                value_and(
                    name,
                    btn2(BTN_MODIFY).on_click(move |_| u_rename2.open_rename_dialog()),
                ),
            )),
            CardItem::Row(setting_row(
                "设备号",
                None,
                value_and(
                    ui.device,
                    btn2(BTN_MODIFY).on_click(move |_| u_rename.open_rename_dialog()),
                ),
            )),
            CardItem::Row(setting_row(
                ROW_GROUP,
                None,
                value_and(
                    ui.group_line,
                    // 身份未确认时 core 拒绝换组，先置灰
                    btn2(BTN_MODIFY)
                        .enabled_when(move || !needs_identity.get())
                        .on_click(move |_| u_group.open_group_dialog()),
                ),
            )),
        ],
    )
}

/// 设备卡：按组分列，本组在前；本机标「本机」，其他设备可移除（二次确认）。
fn devices_card(ui: &SyncUi) -> Element {
    let (u, group, devices_err) = (ui.clone(), ui.group, ui.devices_err);
    let (u_purge, purge_text, busy) = (ui.clone(), ui.purge_text, ui.busy);
    let list = Element::switcher(ui.devices, move |devs: Vec<DeviceView>| {
        let today = chrono::Local::now().date_naive();
        let mut col = Element::col().width_match();
        for (gi, g) in group_devices(&devs, &group.get()).into_iter().enumerate() {
            let title = format!("{} · {} 台", g.name, g.devices.len());
            col = col.child(list_heading_ext(&title, None, gi == 0));
            for (i, d) in g.devices.into_iter().enumerate() {
                let row = device_row(&u, &d, today);
                col = col.child(if i == 0 { card_pad(row) } else { list_row(row) });
            }
        }
        col
    });
    section_items(
        NAV[3],
        vec![
            CardItem::RowWhen(
                Element::label_signal(devices_err)
                    .font_size(12.5)
                    .fg_role(Role::Danger)
                    .width_match()
                    .max_lines(3)
                    .padding_xy(0, 10),
                vis(move || !devices_err.get().is_empty()),
            ),
            CardItem::Bare(list),
            CardItem::RowWhen(
                Element::row()
                    .width_match()
                    .cross(Align::Center)
                    .spacing(12)
                    .padding_xy(0, 10)
                    .child(
                        Element::label_signal(purge_text)
                            .font_size(13.0)
                            .fg_role(Role::TextMuted)
                            .max_lines(1)
                            .weight(1.0),
                    )
                    .child(
                        btn2("清除…")
                            .enabled_when(move || !busy.get())
                            .on_click(move |_| u_purge.request_purge()),
                    ),
                vis(move || !purge_text.get().is_empty()),
            ),
        ],
    )
}

fn device_row(ui: &SyncUi, d: &DeviceView, today: chrono::NaiveDate) -> Element {
    let mut title = Element::row().cross(Align::Center).spacing(8).child(
        Element::label(d.title())
            .font_size(13.5)
            .fg_role(Role::Text),
    );
    if d.is_self {
        title = title.child(Element::badge("本机"));
    }
    let left = Element::col().weight(1.0).spacing(3).child(title).child(
        Element::label(d.detail(today))
            .font_size(12.0)
            .fg_role(Role::TextMuted),
    );
    let mut row = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(0, 10)
        .child(left);
    if d.removable() {
        let (u, d2, busy) = (ui.clone(), d.clone(), ui.busy);
        row = row.child(
            btn2("移除…")
                .enabled_when(move || !busy.get())
                .on_click(move |_| u.request_remove(&d2)),
        );
    }
    row
}

/// 同步内容卡：三类内容各一个开关，改了即存。
fn content_card(ui: &SyncUi) -> Element {
    let (u1, u2, u3) = (ui.clone(), ui.clone(), ui.clone());
    section_items(
        NAV[4],
        vec![
            CardItem::Row(setting_row(
                "学习数据",
                Some("临时词、选词计数、联想历史"),
                Element::switch(ui.content_learned)
                    .on_switch_change(move |_ctx, on| u1.set_content(|c| c.learned = on)),
            )),
            CardItem::Row(setting_row(
                "补全历史",
                Some("邮箱后缀、网址、快捷输入历史"),
                Element::switch(ui.content_completion)
                    .on_switch_change(move |_ctx, on| u2.set_content(|c| c.completion = on)),
            )),
            CardItem::Row(setting_row(
                "打字统计",
                Some("本机统计上传、其他设备统计下载"),
                Element::switch(ui.content_stats)
                    .on_switch_change(move |_ctx, on| u3.set_content(|c| c.stats = on)),
            )),
        ],
    )
}

fn storage_card(ui: &SyncUi) -> Element {
    let (u, busy) = (ui.clone(), ui.busy);
    section_items(
        NAV[5],
        vec![CardItem::Row(setting_row_desc_signal(
            "存储位置",
            ui.storage_summary,
            btn2("更换存储…")
                .enabled_when(move || !busy.get())
                .on_click(move |_| u.open_storage_dialog()),
        ))],
    )
}

fn security_card(ui: &SyncUi) -> Element {
    let busy = ui.busy;
    let (u_key, u_reset) = (ui.clone(), ui.clone());
    section_items(
        NAV[6],
        vec![
            CardItem::Row(setting_row(
                "同步密钥",
                Some("其他设备加入时要粘贴它；请离线保存"),
                btn2("查看…")
                    .enabled_when(move || !busy.get())
                    .on_click(move |_| u_key.open_export()),
            )),
            CardItem::Row(setting_row(
                "重置远端",
                Some(RESET_DESC),
                Element::button("重置远端…")
                    .small()
                    .danger()
                    .enabled_when(move || !busy.get())
                    .on_click(move |_| u_reset.open_reset_dialog()),
            )),
        ],
    )
}
