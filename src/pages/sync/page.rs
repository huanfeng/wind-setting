//! 同步页的卡片。

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::json;
use windui::prelude::*;

use super::dialogs::*;
use super::{PageUi, SECTION, model::*};
use crate::state::AppState;
use crate::widgets::{CardItem, Vis, section, section_items, setting_row, setting_row_desc_signal};

// ───────────────────────────── 界面 ─────────────────────────────

pub fn build_section(_app: &Rc<RefCell<AppState>>, state: &AppState) -> Element {
    let ui = state.sync.clone();
    ui.load_once();
    let gate = ui.installed;
    let busy = ui.busy;
    let can = move || gate.get() && !busy.get();

    let note = |text: &str, role: Role| {
        Element::label(text)
            .font_size(12.0)
            .fg_role(role)
            .width_match()
            .max_lines(6)
    };

    // ── 状态 ──
    let (conflicts, error) = (ui.conflicts, ui.error);
    let (notice, mass_text) = (ui.notice, ui.mass_text);
    let u_mass = ui.clone();
    let u = ui.clone();
    let sync_btn = Element::button("立即同步")
        .small()
        .enabled_when(can)
        .on_click(move |_| u.run("sync_now", vec![("sync_now", json!({}))]));
    let status_card = section(
        SECTION,
        vec![
            Element::label(
                "未安装同步组件（wind_input_sync），下面的设置暂不可用。请重新运行安装程序并选择包含数据同步。",
            )
            .font_size(12.5)
            .fg_role(Role::Warning)
            .width_match()
            .max_lines(4)
            .padding_xy(16, 8)
            .visible_when(move || !gate.get()),
            setting_row_desc_signal("同步状态", ui.phase, sync_btn),
            setting_row_desc_signal("上次成功", ui.last_ok, Element::leaf()),
            setting_row_desc_signal("待上传", ui.pending, Element::leaf()),
            Element::label_signal(ui.conflicts)
                .font_size(12.5)
                .fg_role(Role::Warning)
                .width_match()
                .max_lines(12)
                .padding_xy(16, 8)
                .visible_when(move || !conflicts.get().is_empty()),
            Element::label_signal(ui.error)
                .font_size(12.5)
                .fg_role(Role::Danger)
                .width_match()
                .max_lines(4)
                .padding_xy(16, 8)
                .visible_when(move || !error.get().is_empty()),
            Element::label_signal(ui.notice)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(4)
                .padding_xy(16, 8)
                .visible_when(move || !notice.get().is_empty()),
            Element::col()
                .width_match()
                .spacing(6)
                .padding_xy(16, 8)
                .child(
                    Element::label_signal(ui.mass_text)
                        .font_size(12.5)
                        .fg_role(Role::Warning)
                        .width_match()
                        .max_lines(4),
                )
                .child(
                    Element::button("确认这些删除并继续同步")
                        .small()
                        .danger()
                        .enabled_when(can)
                        .on_click(move |_| u_mass.request_mass_delete()),
                )
                .visible_when(move || !mass_text.get().is_empty()),
        ],
    );

    // ── 后端 ──
    let u = ui.clone();
    let on_switch = Element::switch(ui.enabled)
        .enabled_when(move || gate.get())
        .on_switch_change(move |ctx, on| {
            if let Err(e) = u.save_settings() {
                ctx.toast_err(format!("无法{}同步：{e}", if on { "启用" } else { "停用" }));
                u.enabled.set(!on);
            } else {
                u.refresh();
            }
        });
    let is_s3 = {
        let b = ui.backend;
        move || b.get() == 1
    };
    let is_wd = {
        let b = ui.backend;
        move || b.get() == 0
    };
    let input = |s: Signal<String>, ph: &str| {
        Element::text_input(s, ph)
            .width(280)
            .enabled_when(move || gate.get())
    };
    let secret_input = |s: Signal<String>| {
        let has = ui.has_secret;
        Element::switcher(has, move |h| {
            Element::text_input(s, secret_placeholder(h))
                .password()
                .width(280)
                .enabled_when(move || gate.get())
        })
    };
    let (u1, u2) = (ui.clone(), ui.clone());
    let backend_card = section(
        "同步存储",
        vec![
            setting_row(
                "启用同步",
                Some("设置保存在本机，不会随同步上传"),
                on_switch,
            ),
            setting_row(
                "存储类型",
                None,
                Element::segmented(vec!["WebDAV", "S3 兼容"], ui.backend),
            ),
            setting_row(
                "WebDAV 地址",
                None,
                input(ui.wd_url, "https://dav.example.com/dav/"),
            )
            .visible_when(is_wd),
            setting_row("用户名", None, input(ui.wd_user, "")).visible_when(is_wd),
            setting_row("密码", None, secret_input(ui.wd_password)).visible_when(is_wd),
            setting_row(
                "Endpoint",
                None,
                input(ui.s3_endpoint, "https://s3.example.com"),
            )
            .visible_when(is_s3),
            setting_row("Region", None, input(ui.s3_region, "auto")).visible_when(is_s3),
            setting_row("Bucket", None, input(ui.s3_bucket, "")).visible_when(is_s3),
            setting_row(
                "路径前缀",
                Some("可留空"),
                input(ui.s3_prefix, "windinput/"),
            )
            .visible_when(is_s3),
            setting_row("Access Key ID", None, input(ui.s3_access_key_id, "")).visible_when(is_s3),
            setting_row("Secret", None, secret_input(ui.s3_secret)).visible_when(is_s3),
            setting_row(
                "Path-style 寻址",
                Some("MinIO 等自建服务通常需要勾选"),
                Element::checkbox("使用 path-style", ui.s3_path_style)
                    .enabled_when(move || gate.get()),
            )
            .visible_when(is_s3),
            setting_row(
                "保存与测试",
                Some("密码 / Secret 只写不读：保存后输入框清空，界面不会再显示它"),
                Element::row()
                    .cross(Align::Center)
                    .spacing(6)
                    .child(
                        Element::button("保存")
                            .small()
                            .enabled_when(can)
                            .on_click(move |ctx| u1.save(ctx, false)),
                    )
                    .child(
                        Element::button("测试连接")
                            .small()
                            .outline_soft()
                            .enabled_when(can)
                            .on_click(move |ctx| u2.save(ctx, true)),
                    ),
            ),
        ],
    );

    // ── 我的设备 ──
    let joined = ui.joined;
    let ready = move || gate.get() && joined.get();
    let (u_name, u_ref) = (ui.clone(), ui.clone());
    let (devices, devices_err) = (ui.devices, ui.devices_err);
    let u_list = ui.clone();
    let devices_card = section(
        "我的设备",
        vec![
            setting_row(
                "本机名称",
                Some("其他设备看到的本机名称；随同步设置保存在本机"),
                Element::row()
                    .cross(Align::Center)
                    .spacing(6)
                    .child(
                        Element::text_input(ui.device_name, "留空使用计算机名")
                            .width(220)
                            .enabled_when(move || gate.get()),
                    )
                    .child(
                        Element::button("保存名称")
                            .small()
                            .enabled_when(can)
                            .on_click(move |ctx| match u_name.save_settings() {
                                Ok(()) => {
                                    ctx.toast_ok("已保存设备名称");
                                    u_name.refresh_devices();
                                }
                                Err(e) => ctx.toast_err(format!("保存失败：{e}")),
                            }),
                    ),
            ),
            setting_row(
                "设备列表",
                Some("已加入同步的设备；移除只清理存储里的记录"),
                Element::button("刷新")
                    .small()
                    .outline_soft()
                    .enabled_when(ready)
                    .on_click(move |_| u_ref.refresh_devices()),
            ),
            note(
                "未加入同步（需先设置口令或导入密钥）时无法查看设备列表。",
                Role::TextMuted,
            )
            .padding_xy(16, 8)
            .visible_when(move || !ready()),
            Element::label_signal(devices_err)
                .font_size(12.5)
                .fg_role(Role::Danger)
                .width_match()
                .max_lines(3)
                .padding_xy(16, 8)
                .visible_when(move || !devices_err.get().is_empty()),
            Element::switcher(devices, move |list: Vec<DeviceView>| {
                let today = chrono::Local::now().date_naive();
                let mut col = Element::col().width_match();
                for d in list {
                    let control = if d.removable() {
                        let (u, d2) = (u_list.clone(), d.clone());
                        Element::button("移除")
                            .small()
                            .danger()
                            .enabled_when(can)
                            .on_click(move |_| u.request_remove(&d2))
                    } else {
                        Element::label("本机")
                            .font_size(12.5)
                            .fg_role(Role::AccentText)
                    };
                    col = col.child(setting_row(&d.title(), Some(&d.detail(today)), control));
                }
                col
            })
            .visible_when(ready),
        ],
    );

    // ── 范围 ──
    let u = ui.clone();
    let scope_card = section(
        "同步范围",
        vec![
            setting_row(
                "同步间隔",
                Some("分钟，1 到 1440；本机文件有改动时也会很快同步"),
                Element::number_input(ui.interval, 1.0, 1440.0, 1.0)
                    .enabled_when(move || gate.get()),
            ),
            setting_row(
                "应用范围",
                None,
                Element::button("保存范围")
                    .small()
                    .enabled_when(can)
                    .on_click(move |ctx| match u.save_settings() {
                        Ok(()) => {
                            ctx.toast_ok("已保存同步范围");
                            u.refresh();
                        }
                        Err(e) => ctx.toast_err(format!("保存失败：{e}")),
                    }),
            ),
        ],
    );

    // ── 加密 ──
    let u_pass = ui.clone();
    let u_imp = ui.clone();
    let u_exp = ui.clone();
    let u_copy = ui.clone();
    let u_reset = ui.clone();
    let key_out = ui.exported_key;
    let crypto_card = section(
        "加密与密钥",
        vec![
            note(
                "同步内容在本机加密后才上传，服务端看不到文件内容与路径。口令用于在新设备上解开数据密钥。",
                Role::TextMuted,
            ),
            setting_row(
                "口令",
                Some("未加入时设置口令（远端已有数据则用同一口令加入）；已加入可修改，不影响已导出的密钥"),
                Element::switcher(ui.joined, move |joined| {
                    let u = u_pass.clone();
                    Element::button(if joined {
                        "修改口令…"
                    } else {
                        "设置口令…"
                    })
                    .small()
                    .enabled_when(can)
                    .on_click(move |_| {
                        u.pass_err.set(String::new());
                        u.pass_dialog.set(true);
                    })
                }),
            ),
            setting_row(
                "导入密钥",
                Some("在新设备上粘贴导出的密钥字符串，可免输口令"),
                Element::row()
                    .cross(Align::Center)
                    .spacing(6)
                    .child(
                        Element::text_input(ui.import_key, "粘贴 base64 密钥")
                            .password()
                            .width(200)
                            .enabled_when(move || gate.get()),
                    )
                    .child(
                        Element::button("导入")
                            .small()
                            .outline_soft()
                            .enabled_when({
                                let (c, k) = (can, ui.import_key);
                                move || c() && !k.get().trim().is_empty()
                            })
                            .on_click(move |_| {
                                let key = u_imp.import_key.get().trim().to_string();
                                u_imp.run("import_key", vec![("import_key", json!({ "key": key }))]);
                            }),
                    ),
            ),
            setting_row(
                "导出密钥",
                Some("显示本机的数据密钥，请自行备份"),
                Element::button("导出…")
                    .small()
                    .outline_soft()
                    .enabled_when(can)
                    .on_click(move |_| u_exp.run("export_key", vec![("export_key", json!({}))])),
            ),
            Element::col()
                .width_match()
                .spacing(6)
                .child(
                    Element::label(
                        "请妥善保管这串密钥。忘记口令且没有导出密钥时，数据无法恢复，只能重置远端。",
                    )
                    .font_size(12.5)
                    .fg_role(Role::Warning)
                    .width_match()
                    .max_lines(3),
                )
                .child(
                    Element::row()
                        .width_match()
                        .cross(Align::Center)
                        .spacing(6)
                        .child(
                            Element::text_input(key_out, "")
                                .read_only()
                                .weight(1.0),
                        )
                        .child(Element::button("复制").small().outline_soft().on_click(
                            move |ctx| {
                                ctx.clipboard_set(&u_copy.exported_key.get());
                                ctx.toast_ok("已复制密钥");
                            },
                        )),
                )
                .visible_when(move || !key_out.get().is_empty()),
            setting_row(
                "重置远端",
                Some("清空远端全部同步数据，以本机为准重新上传。其他设备需重新加入"),
                Element::button("重置远端…")
                    .small()
                    .danger()
                    .enabled_when(can)
                    .on_click(move |_| {
                        u_reset.reset_ack.set(false);
                        u_reset.reset_err.set(String::new());
                        u_reset.reset_guard.arm();
                        u_reset.reset_dialog.set(true);
                    }),
            ),
        ],
    );

    ui_dialogs(&ui, state);

    Element::col()
        .width_match()
        .spacing(12)
        .child(status_card)
        .child(backend_card)
        .child(devices_card)
        .child(scope_card)
        .child(crypto_card)
}

/// 截图用：`WIND_SPIKE_SYNC=unset` 强制显示未设置态；`WIND_SPIKE_DIALOG` 启动即开某个对话框。
fn spike_env(k: &str) -> String {
    std::env::var(k).unwrap_or_default()
}

/// 同步页的卡片：`(二级导航名, 卡片)`。
pub fn build_page(
    _app: &Rc<RefCell<AppState>>,
    state: &AppState,
) -> Vec<(Option<&'static str>, Element)> {
    let ui = state.sync.clone();
    ui.load_once();
    let unset = spike_env("WIND_SPIKE_SYNC") == "unset";
    let p = PageUi {
        configured: signal(!unset),
        wiz: signal(false),
        step: signal(0),
        rewire: signal(false),
        remote: signal(0),
        use_key: signal(false),
        rename: signal(false),
        export: signal(false),
        creds: signal(false),
        conflicts: signal(false),
        disable: signal(false),
    };
    match spike_env("WIND_SPIKE_DIALOG").as_str() {
        "wizard1" => {
            p.wiz.set(true);
            ui.wd_url.set(String::new());
            ui.wd_user.set(String::new());
            ui.device_name.set("书房台式机".into());
        }
        "wizard1s3" => {
            p.wiz.set(true);
            ui.backend.set(1);
        }
        "wizard2new" => {
            p.wiz.set(true);
            p.step.set(1);
            p.remote.set(1);
        }
        "wizard2join" => {
            p.wiz.set(true);
            p.step.set(1);
            p.remote.set(2);
        }
        "wizard2key" => {
            p.wiz.set(true);
            p.step.set(1);
            p.remote.set(2);
            p.use_key.set(true);
        }
        "wizard3" => {
            p.wiz.set(true);
            p.step.set(2);
            ui.device_name.set("书房台式机".into());
        }
        "rewire" => {
            p.wiz.set(true);
            p.rewire.set(true);
        }
        "rename" => p.rename.set(true),
        "export" => {
            ui.exported_key
                .set("bW9jay1kYXRhLWVuY3J5cHRpb24ta2V5LTAxMjM0NTY3ODk=".into());
            p.export.set(true);
        }
        "creds" => p.creds.set(true),
        "conflicts" => p.conflicts.set(true),
        "disable" => p.disable.set(true),
        _ => {}
    }
    for d in spike_dialogs(&ui, &p) {
        state.fields.dialogs.borrow_mut().push(d);
    }

    let cfg = p.configured;
    let busy = ui.busy;
    let can = move || !busy.get();

    // ── 未设置：引导卡 ──
    let (wiz, step, rewire) = (p.wiz, p.step, p.rewire);
    let hero = section_items(
        "数据同步",
        vec![CardItem::Bare(
            Element::col()
                .width_match()
                .padding_xy(20, 18)
                .spacing(10)
                .child(
                    Element::label("把词库、短语、方案和设置同步到你的其他设备")
                        .font_size(15.0)
                        .font_weight(600)
                        .fg_role(Role::Text)
                        .width_match(),
                )
                .child(muted(
                    "使用你自己的 WebDAV（如坚果云、Nextcloud）或 S3 兼容存储（如 Cloudflare R2、MinIO）。数据在本机加密后才上传，存储服务看不到内容。",
                ))
                .child(muted("已经在其他设备上设置过？用同一个存储和口令即可加入。"))
                .child(
                    Element::row().padding_xy(0, 6).child(btn("开始设置…").on_click(move |_| {
                        rewire.set(false);
                        step.set(0);
                        wiz.set(true);
                    })),
                ),
        )],
    )
    .visible_when(move || !cfg.get());

    // ── 已设置：状态 ──
    let u = ui.clone();
    let phase = ui.phase;
    let last_ok = ui.last_ok;
    let status_desc = last_ok.map(|t| format!("上次成功 {t}"));
    let conflicts_on: Vis = Rc::new({
        let c = ui.conflicts;
        move || !c.get().is_empty()
    });
    let err_on: Vis = Rc::new({
        let e = ui.error;
        move || !e.get().is_empty()
    });
    let mass_on: Vis = Rc::new({
        let m = ui.mass_text;
        move || !m.get().is_empty()
    });
    let pc = p.conflicts;
    let u_mass = ui.clone();
    let status = section_items(
        "同步状态",
        vec![
            CardItem::Row(
                Element::row()
                    .width_match()
                    .cross(Align::Center)
                    .spacing(12)
                    .padding_xy(0, 10)
                    .child(
                        Element::col()
                            .weight(1.0)
                            .spacing(3)
                            .child(
                                Element::label_signal(phase)
                                    .font_size(14.0)
                                    .font_weight(600)
                                    .fg_role(Role::Text),
                            )
                            .child(
                                Element::label_signal(status_desc)
                                    .font_size(12.0)
                                    .fg_role(Role::TextMuted),
                            ),
                    )
                    .child(
                        btn("立即同步")
                            .enabled_when(can)
                            .on_click(move |_| u.run("sync_now", vec![("sync_now", json!({}))])),
                    ),
            ),
            CardItem::RowWhen(
                setting_row(
                    "有 2 个冲突副本",
                    Some("两台设备同时改了同一个文件，需要你来决定保留哪份"),
                    btn2("查看…").on_click(move |_| pc.set(true)),
                ),
                conflicts_on,
            ),
            CardItem::RowWhen(
                Element::label_signal(ui.error)
                    .font_size(12.5)
                    .fg_role(Role::Danger)
                    .width_match()
                    .max_lines(4),
                err_on,
            ),
            CardItem::RowWhen(
                setting_row_desc_signal(
                    "同步已暂停",
                    ui.mass_text,
                    Element::button("确认删除…")
                        .small()
                        .danger()
                        .on_click(move |_| u_mass.request_mass_delete()),
                ),
                mass_on,
            ),
        ],
    )
    .visible_when(move || cfg.get());

    // ── 我的设备 ──
    let (rename, u_list) = (p.rename, ui.clone());
    let devices = section_items(
        "我的设备",
        vec![CardItem::Bare(Element::switcher(
            ui.devices,
            move |list: Vec<DeviceView>| {
                let today = chrono::Local::now().date_naive();
                let mut col = Element::col().width_match();
                for (i, d) in list.into_iter().enumerate() {
                    if i > 0 {
                        col = col.child(crate::widgets::row_divider());
                    }
                    let title = Element::row()
                        .cross(Align::Center)
                        .spacing(8)
                        .child(
                            Element::label(d.title())
                                .font_size(13.5)
                                .fg_role(Role::Text),
                        )
                        .child(Element::badge("本机").visible_when({
                            let s = d.is_self;
                            move || s
                        }));
                    let left = Element::col().weight(1.0).spacing(3).child(title).child(
                        Element::label(d.detail(today))
                            .font_size(12.0)
                            .fg_role(Role::TextMuted),
                    );
                    let control = if d.is_self {
                        btn2("改名…").on_click(move |_| rename.set(true))
                    } else {
                        let (u, d2) = (u_list.clone(), d.clone());
                        btn2("移除…").on_click(move |_| u.request_remove(&d2))
                    };
                    col = col.child(crate::widgets::card_pad(
                        Element::row()
                            .width_match()
                            .cross(Align::Center)
                            .padding_xy(0, 10)
                            .child(left)
                            .child(control),
                    ));
                }
                col
            },
        ))],
    )
    .visible_when(move || cfg.get());

    // ── 同步选项（改了即存） ──
    let options = section(
        "同步选项",
        vec![setting_row(
            "自动同步间隔",
            Some("分钟；本机文件有改动时会很快同步"),
            Element::number_input(ui.interval, 1.0, 1440.0, 1.0),
        )],
    )
    .visible_when(move || cfg.get());

    // ── 存储与安全 ──
    let (wd_url, wd_user) = (ui.wd_url, ui.wd_user);
    let _ = wd_user;
    let where_ = wd_url.map(|u| format!("WebDAV · {u} · 用户 me"));
    let (wiz2, step2, rewire2) = (p.wiz, p.step, p.rewire);
    let (creds, export, disable) = (p.creds, p.export, p.disable);
    let (u_pass, u_exp, u_reset) = (ui.clone(), ui.clone(), ui.clone());
    let security = section_items(
        "存储与安全",
        vec![
            CardItem::Row(setting_row_desc_signal(
                "存储位置",
                where_,
                btn2("更换…").on_click(move |_| {
                    rewire2.set(true);
                    step2.set(0);
                    wiz2.set(true);
                }),
            )),
            CardItem::Row(setting_row(
                "存储密码",
                Some("已保存在本机（加密存放）"),
                btn2("更新…").on_click(move |_| creds.set(true)),
            )),
            CardItem::Row(setting_row(
                "同步口令",
                Some("其他设备加入时需要输入；修改不影响已导出的密钥"),
                btn2("修改…").on_click(move |_| {
                    u_pass.pass_err.set(String::new());
                    u_pass.pass_dialog.set(true);
                }),
            )),
            CardItem::Row(setting_row(
                "备份密钥",
                Some("可替代口令加入新设备，也能在忘记口令时找回数据"),
                btn2("导出…").on_click(move |_| {
                    u_exp
                        .exported_key
                        .set("bW9jay1kYXRhLWVuY3J5cHRpb24ta2V5LTAxMjM0NTY3ODk=".into());
                    export.set(true);
                }),
            )),
            CardItem::Heading("停用与重置".into()),
            CardItem::Row(setting_row(
                "停用同步",
                Some("本机停止同步，保留设置与存储里的数据"),
                btn2("停用…").on_click(move |_| disable.set(true)),
            )),
            CardItem::Row(setting_row(
                "重置同步库",
                Some("清空存储里的全部同步数据，以本机为准重新开始；其他设备需重新加入"),
                Element::button("重置…")
                    .small()
                    .danger()
                    .on_click(move |_| {
                        u_reset.reset_ack.set(false);
                        u_reset.reset_err.set(String::new());
                        u_reset.reset_guard.arm();
                        u_reset.reset_dialog.set(true);
                    }),
            )),
        ],
    )
    .visible_when(move || cfg.get());

    ui_dialogs(&ui, state);

    vec![
        (None, hero),
        (Some("同步状态"), status),
        (Some("我的设备"), devices),
        (Some("同步选项"), options),
        (Some("存储与安全"), security),
    ]
}
