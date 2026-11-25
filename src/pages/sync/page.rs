//! 同步页的卡片：未配置只有「数据同步」一张；已配置是状态、本机、设备、存储、安全五张与底部「停用同步」。

use std::cell::RefCell;
use std::rc::Rc;

use windui::prelude::*;

use super::dialogs::{btn, btn2, muted};
use super::{ANCHOR, SyncUi, model::*};
use crate::search::anchor;
use crate::state::AppState;
use crate::widgets::{
    CardItem, Vis, card_pad, list_heading_ext, list_row, section_items, setting_row,
    setting_row_desc_signal,
};

/// 一张卡：`(二级导航名, 卡片, 何时显示)`；导航名为空的不进二级导航。
pub type Card = (Option<&'static str>, Element, Vis);

/// 同步页的卡片。
pub fn build_page(_app: &Rc<RefCell<AppState>>, state: &AppState) -> Vec<Card> {
    let ui = state.sync.clone();
    ui.load_once();
    super::dialogs::mount(&ui, state);

    let cfg = ui.configured;
    let on: Vis = Rc::new(move || cfg.get());
    let off: Vis = Rc::new(move || !cfg.get());
    vec![
        (
            Some("数据同步"),
            anchor::wrap(setup_card(&ui), ANCHOR, state.jump),
            off,
        ),
        (
            Some("同步状态"),
            anchor::wrap(status_card(&ui), ANCHOR, state.jump),
            on.clone(),
        ),
        (Some("本机"), self_card(&ui), on.clone()),
        (Some("设备"), devices_card(&ui), on.clone()),
        (Some("存储"), storage_card(&ui), on.clone()),
        (Some("安全"), security_card(&ui), on.clone()),
        (None, disable_row(&ui), on),
    ]
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

/// 未配置：一句介绍 + 「开始配置…」。
fn setup_card(ui: &SyncUi) -> Element {
    let (installed, busy) = (ui.installed, ui.busy);
    let u = ui.clone();
    section_items(
        "数据同步",
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
            CardItem::Row(setting_row(
                "用 WebDAV 或 S3 在多台设备间同步配置、方案和词库",
                Some("数据先在本机加密再上传"),
                btn("开始配置…")
                    .enabled_when(move || installed.get() && !busy.get())
                    .on_click(move |_| u.open_wizard()),
            )),
        ],
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
    section_items(
        "同步状态",
        vec![
            CardItem::Row(setting_row_desc_signal(
                "状态",
                ui.pending,
                value_and(
                    ui.phase,
                    btn("立即同步")
                        .enabled_when(move || can_sync.get() && !busy.get())
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
                Some("分钟；本机文件有改动时会很快同步"),
                Element::row()
                    .cross(Align::Center)
                    .child(Element::number_input(interval, 1.0, 1440.0, 1.0))
                    .child(interval_saver),
            )),
        ],
    )
}

/// 本机卡：设备号、显示名称、所在组；改号与更换组走各自的对话框。
fn self_card(ui: &SyncUi) -> Element {
    let (u_rename, u_group) = (ui.clone(), ui.clone());
    let name = ui.device_name.map(|n| {
        if n.trim().is_empty() {
            "未设置".to_string()
        } else {
            n.clone()
        }
    });
    section_items(
        "本机",
        vec![
            CardItem::Row(setting_row(
                "设备号",
                None,
                value_and(
                    ui.device,
                    btn2("改…").on_click(move |_| u_rename.open_rename_dialog()),
                ),
            )),
            CardItem::Row(setting_row("显示名称", None, value(name))),
            CardItem::Row(setting_row(
                "所在组",
                None,
                value_and(
                    ui.group_line,
                    btn2("更换组…").on_click(move |_| u_group.open_group_dialog()),
                ),
            )),
        ],
    )
}

/// 设备卡：按组分列，本组在前；本机标「本机」，其他设备可移除（二次确认）。
fn devices_card(ui: &SyncUi) -> Element {
    let (u, group, devices_err) = (ui.clone(), ui.group, ui.devices_err);
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
        "设备",
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

fn storage_card(ui: &SyncUi) -> Element {
    let (u, busy) = (ui.clone(), ui.busy);
    section_items(
        "存储",
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
    let (u_pass, u_exp, u_reset) = (ui.clone(), ui.clone(), ui.clone());
    section_items(
        "安全",
        vec![
            CardItem::Row(setting_row(
                "同步口令",
                Some("其他设备加入时要输入；修改不影响已导出的密钥"),
                btn2("修改口令…")
                    .enabled_when(move || !busy.get())
                    .on_click(move |_| u_pass.open_passphrase_dialog()),
            )),
            CardItem::Row(setting_row(
                "数据密钥",
                Some("可代替口令加入新设备，也能在忘记口令时找回数据"),
                btn2("导出密钥…")
                    .enabled_when(move || !busy.get())
                    .on_click(move |_| u_exp.open_export()),
            )),
            CardItem::Row(setting_row(
                "重置远端",
                Some("清空远端全部同步数据，以本机为准重新上传；其他设备需重新加入"),
                Element::button("重置远端…")
                    .small()
                    .danger()
                    .enabled_when(move || !busy.get())
                    .on_click(move |_| u_reset.open_reset_dialog()),
            )),
        ],
    )
}

/// 页底「停用同步」：确认后只关开关，不删远端。
fn disable_row(ui: &SyncUi) -> Element {
    let (u, busy) = (ui.clone(), ui.busy);
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(12)
        .child(
            btn2("停用同步")
                .enabled_when(move || !busy.get())
                .on_click(move |_| u.disable_dialog.set(true)),
        )
        .child(muted("本机不再同步，远端数据保留"))
}
