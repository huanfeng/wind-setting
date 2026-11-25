//! 同步页的对话框。

use windui::prelude::*;

use super::{SyncUi, model::*};
use crate::state::AppState;

/// 本页全部对话框挂到字段注册表的 `dialogs`（同扩展程序卡），随页面生命周期。
pub(super) fn mount(ui: &SyncUi, state: &AppState) {
    let all = [
        super::wizard::wizard_dialog(ui),
        super::wizard::identity_dialog(ui),
        storage_dialog(ui),
        export_dialog(ui),
        passphrase_dialog(ui),
        conflicts_dialog(ui),
        mass_delete_dialog(ui),
        reset_dialog(ui),
        remove_device_dialog(ui),
        rename_dialog(ui),
        group_dialog(ui),
    ];
    state.fields.dialogs.borrow_mut().extend(all);
}

fn dialog_frame(title: &str, body: Element, footer: Element) -> Element {
    dialog_frame_w(460, title, body, footer)
}

pub(super) fn dialog_frame_w(width: i32, title: &str, body: Element, footer: Element) -> Element {
    Element::col()
        .width(width)
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
                .child(Element::leaf().weight(1.0))
                .child(footer),
        )
}

fn passphrase_dialog(ui: &SyncUi) -> Element {
    let joined = ui.joined;
    let body = Element::col()
        .width_match()
        .padding_xy(18, 14)
        .spacing(8)
        .child(
            Element::label(
                "口令用于加密同步数据。请使用足够长的口令，并牢记；另可用「导出密钥」做备份。",
            )
            .font_size(12.5)
            .fg_role(Role::TextMuted)
            .width_match()
            .max_lines(4),
        )
        .child(
            Element::text_input(ui.old_pass, "旧口令（可选，忘记可留空）")
                .password()
                .width_match()
                .visible_when(move || joined.get()),
        )
        .child(
            Element::text_input(ui.pass1, "口令")
                .password()
                .width_match(),
        )
        .child(
            Element::text_input(ui.pass2, "再输入一次")
                .password()
                .width_match(),
        )
        .child(
            Element::label_signal(ui.pass_err)
                .font_size(12.5)
                .fg_role(Role::Danger)
                .width_match(),
        );
    let (u, u2, busy, visible) = (ui.clone(), ui.clone(), ui.busy, ui.pass_dialog);
    let footer = Element::row()
        .spacing(8)
        .child(
            Element::button("取消")
                .small()
                .outline_soft()
                .on_click(move |_| u2.close_passphrase_dialog()),
        )
        .child(
            Element::button("确定")
                .small()
                .enabled_when(move || !busy.get())
                .on_click(move |_| u.submit_passphrase()),
        );
    let u3 = ui.clone();
    Element::dialog_closable(
        visible,
        dialog_frame("修改口令", body, footer),
        move |_| u3.close_passphrase_dialog(),
    )
}

fn mass_delete_dialog(ui: &SyncUi) -> Element {
    let visible = ui.mass_dialog;
    let body = Element::col().width_match().padding_xy(18, 14).child(
        Element::label(
            "这些文件在本机看似被删除。确认后，这些删除会同步到所有设备，其他设备上对应的文件也会被删掉。请确认不是误操作。",
        )
        .font_size(12.5)
        .fg_role(Role::Warning)
        .width_match()
        .max_lines(5),
    );
    let (u, busy, g) = (ui.clone(), ui.busy, ui.mass_guard.clone());
    let footer = Element::row()
        .spacing(8)
        .child(
            Element::button("取消")
                .small()
                .outline_soft()
                .on_click(move |_| {
                    g.disarm();
                    visible.set(false);
                }),
        )
        .child(
            Element::button("确认删除并同步")
                .small()
                .danger()
                .enabled_when(move || !busy.get())
                .on_click(move |_| u.confirm_mass_delete()),
        );
    Element::dialog(visible, dialog_frame("确认大批量删除", body, footer))
}

fn remove_device_dialog(ui: &SyncUi) -> Element {
    let visible = ui.remove_dialog;
    let body = Element::col()
        .width_match()
        .padding_xy(18, 14)
        .spacing(8)
        .child(
            Element::label_signal(ui.remove_name.map(|n| format!("移除设备：{n}")))
                .font_size(13.0)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(2),
        )
        .child(
            Element::label(REMOVE_DEVICE_WARN)
                .font_size(12.5)
                .fg_role(Role::Warning)
                .width_match()
                .max_lines(6),
        );
    let (u, busy, g) = (ui.clone(), ui.busy, ui.remove_guard.clone());
    let footer = Element::row()
        .spacing(8)
        .child(
            Element::button("取消")
                .small()
                .outline_soft()
                .on_click(move |_| {
                    g.disarm();
                    visible.set(false);
                }),
        )
        .child(
            Element::button("移除")
                .small()
                .danger()
                .enabled_when(move || !busy.get())
                .on_click(move |_| u.confirm_remove()),
        );
    Element::dialog(visible, dialog_frame("移除设备", body, footer))
}

/// 改号与显示名称：两项都不变时保存只关框。
fn rename_dialog(ui: &SyncUi) -> Element {
    let visible = ui.rename_dialog;
    let err = ui.rename_err;
    let body = Element::col()
        .width_match()
        .padding_xy(18, 14)
        .spacing(12)
        .child(field(
            "设备号",
            Element::text_input(ui.rename_id, "小写字母、数字、短横线，最长 24 个字符")
                .width_match(),
        ))
        .child(muted(
            "设备号用来区分同一存储里的各台设备；与另一台设备撞号时，给本机换一个就行。",
        ))
        .child(field(
            "显示名称",
            Element::text_input(ui.rename_name, "其他设备上看到的名字").width_match(),
        ))
        .child(
            Element::label_signal(err)
                .font_size(12.5)
                .fg_role(Role::Danger)
                .width_match()
                .max_lines(3)
                .visible_when(move || !err.get().is_empty()),
        );
    let (u, busy) = (ui.clone(), ui.busy);
    let footer = Element::row()
        .spacing(8)
        .child(btn2("取消").on_click(move |_| visible.set(false)))
        .child(
            btn("保存")
                .enabled_when(move || !busy.get())
                .on_click(move |_| u.save_rename()),
        );
    Element::dialog_closable(
        visible,
        dialog_frame("修改设备名称与设备号", body, footer),
        move |_| visible.set(false),
    )
}

/// 更换组：加入已有组（带替换方向与确认文案）/ 新建独立组 / 重命名本组。
fn group_dialog(ui: &SyncUi) -> Element {
    let visible = ui.group_dialog;
    let (mode, pick, mine, choices) = (
        ui.group_mode,
        ui.group_pick,
        ui.group_mine,
        ui.group_choices,
    );
    let err = ui.group_err;
    let cap = |t: &str| muted(t).padding_xy(22, 0);
    let list = Element::switcher(choices, move |cs: Vec<(String, String)>| {
        let mut col = Element::col().width_match().spacing(6).padding_xy(22, 0);
        match cs.as_slice() {
            [] => col.child(muted("没有可加入的其他组")),
            [(_, label)] => col.child(
                Element::label(format!("加入「{label}」"))
                    .font_size(13.0)
                    .fg_role(Role::Text),
            ),
            _ => {
                for (i, (_, label)) in cs.iter().enumerate() {
                    col = col.child(Element::radio(label.as_str(), pick, i));
                }
                col
            }
        }
    });
    let join = Element::col()
        .width_match()
        .spacing(6)
        .padding_xy(0, 2)
        .child(cap("加入哪个组"))
        .child(list)
        .child(
            Element::col()
                .width_match()
                .spacing(6)
                .child(Element::leaf().height(6))
                .child(cap("以哪边的设置为准"))
                .child(
                    Element::col()
                        .width_match()
                        .spacing(6)
                        .padding_xy(22, 0)
                        .child(Element::radio("用组内设置替换本机", mine, 0))
                        .child(Element::radio("用本机设置替换组内", mine, 1))
                        .child(
                            Element::label_signal(
                                mine.map(|m| join_confirm_text(*m == 0).to_string()),
                            )
                            .font_size(12.5)
                            .fg_role(Role::Warning)
                            .width_match()
                            .max_lines(3),
                        ),
                )
                .visible_when(move || !choices.get().is_empty()),
        )
        .visible_when(move || mode.get() == 0);
    let body = Element::col()
        .width_match()
        .padding_xy(18, 14)
        .spacing(12)
        .child(Element::radio("加入已有组", mode, 0))
        .child(join)
        .child(Element::radio("新建独立组", mode, 1))
        .child(
            Element::col()
                .width_match()
                .spacing(4)
                .child(Element::text_input(ui.group_new_name, "组名（可不填）").width_match())
                .child(cap("本机退出当前组，单独成组"))
                .visible_when(move || mode.get() == 1),
        )
        .child(Element::radio("重命名本组", mode, 2))
        .child(
            Element::col()
                .width_match()
                .spacing(4)
                .child(Element::text_input(ui.group_rename, "新的组名").width_match())
                .child(cap("只改组的显示名，组内设备不变"))
                .visible_when(move || mode.get() == 2),
        )
        .child(
            Element::label_signal(err)
                .font_size(12.5)
                .fg_role(Role::Danger)
                .width_match()
                .max_lines(3)
                .visible_when(move || !err.get().is_empty()),
        );
    let (u, busy) = (ui.clone(), ui.busy);
    let footer = Element::row()
        .spacing(8)
        .child(btn2("取消").on_click(move |_| visible.set(false)))
        .child(
            btn("确定")
                .enabled_when(move || !busy.get())
                .on_click(move |_| u.save_group()),
        );
    Element::dialog_closable(
        visible,
        dialog_frame("修改同步组", body, footer),
        move |_| visible.set(false),
    )
}

fn reset_dialog(ui: &SyncUi) -> Element {
    let visible = ui.reset_dialog;
    let ack = ui.reset_ack;
    let body = Element::col()
        .width_match()
        .padding_xy(18, 14)
        .spacing(10)
        .child(
            Element::label(
                "这会删除远端存储里全部同步数据（含其他设备上传的内容），并以本机当前数据重新上传。其他设备需要重新加入同步。此操作不可撤销。",
            )
            .font_size(12.5)
            .fg_role(Role::Danger)
            .width_match()
            .max_lines(5),
        )
        .child(
            Element::text_input(ui.reset_pass1, "新口令")
                .password()
                .width_match(),
        )
        .child(
            Element::text_input(ui.reset_pass2, "再输入一次新口令")
                .password()
                .width_match(),
        )
        .child(
            Element::label_signal(ui.reset_err)
                .font_size(12.5)
                .fg_role(Role::Danger)
                .width_match(),
        )
        .child(Element::checkbox("我了解后果，确认重置远端", ack));
    let (u, guard, busy) = (ui.clone(), ui.reset_guard.clone(), ui.busy);
    let footer = Element::row()
        .spacing(8)
        .child(Element::button("取消").small().outline_soft().on_click({
            let (guard, u2) = (guard.clone(), ui.clone());
            move |_| {
                guard.disarm();
                u2.reset_pass1.set(String::new());
                u2.reset_pass2.set(String::new());
                visible.set(false);
            }
        }))
        .child(
            Element::button("重置远端")
                .small()
                .danger()
                .enabled_when(move || ack.get() && !busy.get())
                .on_click(
                    move |_| match guard.take(&u.reset_pass1.get(), &u.reset_pass2.get()) {
                        Ok(args) => {
                            u.reset_err.set(String::new());
                            u.run("reset_remote", vec![("reset_remote", args)]);
                        }
                        Err(e) => u.reset_err.set(e),
                    },
                ),
        );
    Element::dialog(visible, dialog_frame("重置远端", body, footer))
}

pub(super) fn muted(text: &str) -> Element {
    Element::label(text)
        .font_size(12.5)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(5)
}

pub(super) fn tone(text: &str, role: Role) -> Element {
    Element::label(text)
        .font_size(12.5)
        .fg_role(role)
        .width_match()
        .max_lines(5)
}

pub(super) fn field(label: &str, control: Element) -> Element {
    Element::col()
        .width_match()
        .spacing(4)
        .child(Element::label(label).font_size(12.5).fg_role(Role::Text))
        .child(control)
}

pub(super) fn btn(text: &str) -> Element {
    Element::button(text).small()
}

pub(super) fn btn2(text: &str) -> Element {
    Element::button(text).small().outline_soft()
}

/// 存储类型与参数（更换存储与向导共用）。
pub(super) fn storage_fields(ui: &SyncUi) -> Element {
    let b = ui.backend;
    let is_wd = move || b.get() == 0;
    let is_s3 = move || b.get() == 1;
    let inp = |s: Signal<String>, ph: &str| Element::text_input(s, ph).width_match();
    let hint = ui.secret_hint;
    let secret = move |s: Signal<String>| {
        Element::switcher(hint, move |h| {
            Element::text_input(s, h).password().width_match()
        })
    };
    let two = |a: Element, b: Element| {
        Element::row()
            .width_match()
            .spacing(12)
            .child(a.weight(1.0))
            .child(b.weight(1.0))
    };
    Element::col()
        .width_match()
        .spacing(12)
        .child(field(
            "存储类型",
            Element::segmented(vec!["WebDAV", "S3 兼容存储"], ui.backend),
        ))
        .child(
            Element::col()
                .width_match()
                .spacing(12)
                .child(field(
                    "WebDAV 地址",
                    inp(ui.wd_url, "https://dav.example.com/dav/"),
                ))
                .child(two(
                    field("用户名", inp(ui.wd_user, "")),
                    field("密码", secret(ui.wd_password)),
                ))
                .visible_when(is_wd),
        )
        .child(
            Element::col()
                .width_match()
                .spacing(12)
                .child(field(
                    "Endpoint",
                    inp(ui.s3_endpoint, "https://s3.example.com"),
                ))
                .child(two(
                    field("Bucket", inp(ui.s3_bucket, "")),
                    field("Region", inp(ui.s3_region, "auto")),
                ))
                .child(two(
                    field("Access Key ID", inp(ui.s3_access_key_id, "")),
                    field("Secret", secret(ui.s3_secret)),
                ))
                .child(two(
                    field("路径前缀（可留空）", inp(ui.s3_prefix, "windinput/")),
                    field(
                        "寻址方式",
                        Element::checkbox("Path-style（MinIO 等自建服务）", ui.s3_path_style),
                    ),
                ))
                .visible_when(is_s3),
        )
}

/// 「更换存储…」：沿用存储表单；位置变了要重填密码，并用口令或导出的密钥重新解开数据。
fn storage_dialog(ui: &SyncUi) -> Element {
    let use_key = ui.storage_use_key;
    let pwd = |s: Signal<String>, ph: &str| Element::text_input(s, ph).password().width_match();
    let unlock = Element::col()
        .width_match()
        .spacing(8)
        .child(
            Element::col()
                .width_match()
                .spacing(8)
                .child(field("同步口令", pwd(ui.pass1, "新位置上同步数据的口令")))
                .child(field("再输入一次", pwd(ui.pass2, "")))
                .child(
                    Element::link("改用导出的密钥")
                        .font_size(12.5)
                        .on_click(move |_| use_key.set(true)),
                )
                .visible_when(move || !use_key.get()),
        )
        .child(
            Element::col()
                .width_match()
                .spacing(8)
                .child(field(
                    "密钥",
                    pwd(ui.import_key, "粘贴在其他设备上导出的密钥"),
                ))
                .child(
                    Element::link("改用口令")
                        .font_size(12.5)
                        .on_click(move |_| use_key.set(false)),
                )
                .visible_when(move || use_key.get()),
        );
    let body = Element::col()
        .width_match()
        .padding_xy(20, 16)
        .spacing(14)
        .child(muted(
            "换到别的存储位置时，要重填该位置的密码，并用同步口令或导出的密钥解开那里的数据；新位置还没有数据时会用这个口令新建。只改密码则下面的口令可以不填。原存储里的数据不会被删除。",
        ))
        .child(storage_fields(ui))
        .child(Element::divider())
        .child(unlock)
        .child({
            let err = ui.storage_err;
            Element::label_signal(err)
                .font_size(12.5)
                .fg_role(Role::Danger)
                .width_match()
                .max_lines(3)
                .visible_when(move || !err.get().is_empty())
        });
    // S3 表单较高：小窗口里正文滚动，标题与按钮不出框。
    let body = Element::scroll().width_match().max_height(460).child(body);
    let (u, u2, busy) = (ui.clone(), ui.clone(), ui.busy);
    let footer = Element::row()
        .spacing(8)
        .child(btn2("取消").on_click(move |_| u2.close_storage_dialog()))
        .child(
            btn("保存")
                .enabled_when(move || !busy.get())
                .on_click(move |_| u.save_storage()),
        );
    let visible = ui.storage_dialog;
    let u3 = ui.clone();
    Element::dialog_closable(
        visible,
        dialog_frame_w(560, "更换存储", body, footer),
        move |_| u3.close_storage_dialog(),
    )
}

/// 「导出密钥…」：密钥回来前显示「正在读取…」；关框即清掉。
fn export_dialog(ui: &SyncUi) -> Element {
    let key = ui.exported_key;
    let body = Element::col()
        .width_match()
        .padding_xy(18, 14)
        .spacing(10)
        .child(muted(
            "这串密钥可以直接解开同步数据，作用等同口令。可用来在新设备上免输口令加入，或在忘记口令时找回数据。",
        ))
        .child(muted("正在读取…").visible_when(move || key.get().is_empty()))
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(6)
                .child(Element::text_input(key, "").read_only().weight(1.0))
                .child(btn2("复制").on_click(move |ctx| {
                    ctx.clipboard_set(&key.get());
                    ctx.toast_ok("已复制密钥");
                }))
                .visible_when(move || !key.get().is_empty()),
        )
        .child(tone(
            "请离线保存（如密码管理器），不要发到聊天或网盘。任何拿到它的人都能读取你的同步数据。",
            Role::Warning,
        ));
    let (u, u2) = (ui.clone(), ui.clone());
    Element::dialog_closable(
        ui.export_dialog,
        dialog_frame(
            "导出密钥",
            body,
            btn("完成").on_click(move |_| u.close_export()),
        ),
        move |_| u2.close_export(),
    )
}

/// 冲突副本列表（同步进程给的路径，新到旧）。
fn conflicts_dialog(ui: &SyncUi) -> Element {
    let visible = ui.conflicts_dialog;
    let body = Element::col().width_match().padding_xy(18, 14).child(
        Element::label_signal(ui.conflicts)
            .font_size(12.5)
            .fg_role(Role::Text)
            .width_match()
            .max_lines(14),
    );
    let footer = Element::row()
        .spacing(8)
        .child(btn2("打开用户目录").on_click(|ctx| {
            if let Err(e) = crate::log_tools::open_dir(crate::log_tools::DirKind::UserData) {
                ctx.toast_err(format!("打开用户目录失败：{e}"));
            }
        }))
        .child(btn("关闭").on_click(move |_| visible.set(false)));
    Element::dialog(visible, dialog_frame("冲突副本", body, footer))
}
