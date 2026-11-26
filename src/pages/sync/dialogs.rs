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
        key_view_dialog(ui),
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

/// 「重置远端…」：先二次确认；成功后在同一个框里显示新的同步密钥，勾选已保存才能关。
fn reset_dialog(ui: &SyncUi) -> Element {
    let (visible, ack, key, busy) = (ui.reset_dialog, ui.reset_ack, ui.new_key, ui.busy);
    let confirm = Element::col()
        .width_match()
        .spacing(10)
        .child(tone(
            "这会删除远端存储里全部同步数据（含其他设备上传的内容），以本机当前数据重新上传，并生成新的同步密钥。其他设备需要用新密钥重新加入。此操作不可撤销。",
            Role::Danger,
        ))
        .child(Element::checkbox("我了解后果，确认重置远端", ack))
        .visible_when(move || key.get().is_empty());
    let done = Element::col()
        .width_match()
        .spacing(10)
        .child(tone(
            "远端已重置，将以本机数据为准重新上传。新的同步密钥：",
            Role::Success,
        ))
        .child(new_key_block(ui))
        .visible_when(move || !key.get().is_empty());
    let body = Element::col()
        .width_match()
        .padding_xy(18, 14)
        .child(confirm)
        .child(done);
    let (u, u2, u3) = (ui.clone(), ui.clone(), ui.clone());
    let key_ack = ui.key_ack;
    let footer = Element::row()
        .spacing(8)
        .child(
            btn2("取消")
                .enabled_when(move || !busy.get())
                .on_click(move |_| u2.close_reset_dialog())
                .visible_when(move || key.get().is_empty()),
        )
        .child(
            Element::button("重置远端")
                .small()
                .danger()
                .enabled_when(move || ack.get() && !busy.get())
                .on_click(move |_| u.confirm_reset())
                .visible_when(move || key.get().is_empty()),
        )
        .child(
            btn("完成")
                .enabled_when(move || key_ack.get())
                .on_click(move |_| u3.close_reset_dialog())
                .visible_when(move || !key.get().is_empty()),
        );
    Element::dialog(visible, dialog_frame("重置远端", body, footer))
}

/// 显示同步密钥：等宽两行 + 「复制」（复制的是原串）。字体缺失时 windui 回退系统默认。
pub(super) fn key_box(key: Signal<String>) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(10)
        .child(
            Element::label_signal(key.map(|k| key_lines(k)))
                .font_family("Consolas")
                .font_size(14.0)
                .fg_role(Role::Text)
                .max_lines(2)
                .bg_role(Role::InputBg)
                .border_role(Role::InputBorder, 1)
                .corner(6.0)
                .padding_xy(12, 8)
                .weight(1.0),
        )
        .child(btn2("复制").on_click(move |ctx| {
            ctx.clipboard_set(&key.get());
            ctx.toast_ok("已复制同步密钥");
        }))
}

/// 刚生成的同步密钥：密钥框 + 生成后提示 + 「我已保存同步密钥」。
pub(super) fn new_key_block(ui: &SyncUi) -> Element {
    Element::col()
        .width_match()
        .spacing(8)
        .child(key_box(ui.new_key))
        .child(tone(KEY_SAVED_HINT, Role::Warning))
        .child(Element::checkbox(KEY_ACK, ui.key_ack))
}

/// 粘贴同步密钥的输入框（明文，便于核对）。
pub(super) fn key_input(key: Signal<String>) -> Element {
    Element::text_input(key, KEY_PLACEHOLDER)
        .font_family("Consolas")
        .width_match()
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
    let skip = ui.skip_cert_verify;
    let secret = move |s: Signal<String>| {
        Element::switcher(hint, move |h| {
            Element::text_input(s, h)
                .password()
                .reveal_toggle()
                .width_match()
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
        .child(
            Element::col()
                .width_match()
                .spacing(6)
                .child(Element::checkbox(
                    "跳过证书验证（自签名证书的自建服务）",
                    ui.skip_cert_verify,
                ))
                .child(
                    Element::label(SKIP_CERT_WARN)
                        .font_size(12.0)
                        .fg_role(Role::Warning)
                        .width_match()
                        .max_lines(3)
                        .visible_when(move || skip.get()),
                ),
        )
}

/// 「更换存储…」：先填存储参数并测试新位置；新位置没有同步库就给「在新位置创建同步库」并显示新密钥，
/// 已有就粘贴同步密钥加入。
fn storage_dialog(ui: &SyncUi) -> Element {
    let stage = ui.storage_stage;
    let at = move |s: StorageStage| move || stage.get() == s;
    let form = Element::col()
        .width_match()
        .spacing(14)
        .child(muted(
            "换到别的存储位置时要重填该位置的密码。保存后会检查新位置：还没有同步库就在那里新建，已有就粘贴它的同步密钥加入。原存储里的数据不会被删除。",
        ))
        .child(storage_fields(ui))
        .visible_when(at(StorageStage::Form));
    let create = muted(
        "设置已保存。新位置还没有同步库：创建后本机的数据会上传到这里，并生成新的同步密钥，其他设备要用新密钥加入。",
    )
    .visible_when(at(StorageStage::Create));
    let join = Element::col()
        .width_match()
        .spacing(10)
        .child(muted(
            "设置已保存。新位置已有同步库：粘贴它的同步密钥加入（可在已加入的设备上「安全」→「同步密钥」→「查看…」）。",
        ))
        .child(field("同步密钥", key_input(ui.import_key)))
        .visible_when(at(StorageStage::Join));
    let created = Element::col()
        .width_match()
        .spacing(10)
        .child(tone("已在新位置创建同步库。新的同步密钥：", Role::Success))
        .child(new_key_block(ui))
        .visible_when(at(StorageStage::Created));
    let body = Element::col()
        .width_match()
        .padding_xy(20, 16)
        .spacing(14)
        .child(form)
        .child(create)
        .child(join)
        .child(created)
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
    let (busy, ack) = (ui.busy, ui.key_ack);
    let (u_save, u_create, u_join, u_cancel, u_done) =
        (ui.clone(), ui.clone(), ui.clone(), ui.clone(), ui.clone());
    let primary = |text: &str, s: StorageStage, f: Box<dyn Fn()>| {
        btn(text)
            .enabled_when(move || !busy.get())
            .on_click(move |_| f())
            .visible_when(at(s))
    };
    let footer = Element::row()
        .spacing(8)
        .child(
            btn2("取消")
                .on_click(move |_| u_cancel.close_storage_dialog())
                .visible_when(move || stage.get() != StorageStage::Created),
        )
        .child(primary(
            "保存",
            StorageStage::Form,
            Box::new(move || u_save.save_storage()),
        ))
        .child(primary(
            "在新位置创建同步库",
            StorageStage::Create,
            Box::new(move || u_create.create_storage_library()),
        ))
        .child(primary(
            "加入",
            StorageStage::Join,
            Box::new(move || u_join.join_storage()),
        ))
        .child(
            btn("完成")
                .enabled_when(move || ack.get())
                .on_click(move |_| u_done.close_storage_dialog())
                .visible_when(at(StorageStage::Created)),
        );
    let visible = ui.storage_dialog;
    let u3 = ui.clone();
    Element::dialog_closable(
        visible,
        dialog_frame_w(560, "更换存储", body, footer),
        move |_| u3.close_storage_dialog(),
    )
}

/// 「查看…」：同步密钥回来前显示「正在读取…」；关框即清掉。
fn key_view_dialog(ui: &SyncUi) -> Element {
    let key = ui.exported_key;
    let body = Element::col()
        .width_match()
        .padding_xy(18, 14)
        .spacing(10)
        .child(muted(
            "其他设备加入同步时粘贴这串同步密钥。它能解开全部同步数据。",
        ))
        .child(muted("正在读取…").visible_when(move || key.get().is_empty()))
        .child(key_box(key).visible_when(move || !key.get().is_empty()))
        .child(tone(
            "请离线保存（如密码管理器），不要发到聊天或网盘。任何拿到它的人都能读取你的同步数据。",
            Role::Warning,
        ));
    let (u, u2) = (ui.clone(), ui.clone());
    Element::dialog_closable(
        ui.export_dialog,
        dialog_frame(
            "同步密钥",
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
