//! 同步页的对话框。

use windui::prelude::*;

use super::{SyncUi, model::*};
use crate::state::AppState;

/// 本页全部对话框挂到字段注册表的 `dialogs`（同扩展程序卡），随页面生命周期。
pub(super) fn mount(ui: &SyncUi, state: &AppState) {
    let all = [
        wizard_dialog(ui),
        storage_dialog(ui),
        export_dialog(ui),
        passphrase_dialog(ui),
        conflicts_dialog(ui),
        disable_dialog(ui),
        mass_delete_dialog(ui),
        reset_dialog(ui),
        remove_device_dialog(ui),
    ];
    state.fields.dialogs.borrow_mut().extend(all);
}

fn dialog_frame(title: &str, body: Element, footer: Element) -> Element {
    dialog_frame_w(460, title, body, footer)
}

fn dialog_frame_w(width: i32, title: &str, body: Element, footer: Element) -> Element {
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
    let visible = ui.pass_dialog;
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
    let (u, busy) = (ui.clone(), ui.busy);
    let footer = Element::row()
        .spacing(8)
        .child(
            Element::button("取消")
                .small()
                .outline_soft()
                .on_click(move |_| visible.set(false)),
        )
        .child(
            Element::button("确定")
                .small()
                .enabled_when(move || !busy.get())
                .on_click(move |_| u.submit_passphrase()),
        );
    Element::dialog(visible, dialog_frame("修改口令", body, footer))
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

fn step_header(step: Signal<usize>) -> Element {
    let names = ["连接存储", "加密口令", "同步内容"];
    let mut row = Element::row().width_match().cross(Align::Center).spacing(8);
    for (i, n) in names.iter().enumerate() {
        if i > 0 {
            row = row.child(Element::leaf().width(24).height(1).bg_role(Role::Divider));
        }
        let cur = move || step.get() == i;
        let done = move || step.get() > i;
        let dot = |role: Role, txt: String| {
            Element::stack()
                .size(20, 20)
                .corner(10.0)
                .bg_role(role)
                .child(
                    Element::label(txt)
                        .font_size(11.5)
                        .font_weight(600)
                        .fg_role(Role::OnAccent)
                        .align(Align::Center),
                )
        };
        row = row
            .child(dot(Role::Accent, format!("{}", i + 1)).visible_when(move || cur() || done()))
            .child(
                dot(Role::TextSubtle, format!("{}", i + 1)).visible_when(move || !cur() && !done()),
            )
            .child(
                Element::label(*n)
                    .font_size(12.5)
                    .font_weight(600)
                    .fg_role(Role::Text)
                    .visible_when(cur),
            )
            .child(
                Element::label(*n)
                    .font_size(12.5)
                    .fg_role(Role::TextMuted)
                    .visible_when(move || !cur()),
            );
    }
    row
}

/// 存储类型与参数（更换存储与向导共用）。
fn storage_fields(ui: &SyncUi) -> Element {
    let b = ui.backend;
    let is_wd = move || b.get() == 0;
    let is_s3 = move || b.get() == 1;
    let inp = |s: Signal<String>, ph: &str| Element::text_input(s, ph).width_match();
    let has = ui.has_secret;
    let secret = move |s: Signal<String>| {
        Element::switcher(has, move |h| {
            Element::text_input(s, secret_placeholder(h))
                .password()
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
            "换到别的存储位置时，要重填该位置的密码，并用同步口令或导出的密钥解开那里的数据；只改密码则下面的口令可以不填。原存储里的数据不会被删除。",
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

/// 「停用同步」确认：只关开关，不删远端。
fn disable_dialog(ui: &SyncUi) -> Element {
    let visible = ui.disable_dialog;
    let body = Element::col().width_match().padding_xy(18, 14).child(muted(
        "停用后本机不再同步。存储设置、口令和本机数据都会保留，远端存储里的数据不会被删除；之后可以随时重新开始配置。",
    ));
    let (u, busy) = (ui.clone(), ui.busy);
    let footer = Element::row()
        .spacing(8)
        .child(btn2("取消").on_click(move |_| visible.set(false)))
        .child(
            btn("停用")
                .enabled_when(move || !busy.get())
                .on_click(move |_| u.confirm_disable()),
        );
    Element::dialog(visible, dialog_frame("停用同步", body, footer))
}

/// 配置向导的步骤与表单（占位：Task 4 接上测试连接、口令与身份确认）。
fn wizard_dialog(ui: &SyncUi) -> Element {
    let (step, remote, use_key) = (ui.wiz_step, ui.wiz_remote, ui.wiz_use_key);
    let pwd = |s: Signal<String>, ph: &str| Element::text_input(s, ph).password().width_match();

    let s1 = storage_fields(ui);

    let s2_new = Element::col()
        .width_match()
        .spacing(12)
        .child(muted(
            "设置一个口令。同步数据在本机加密后才上传，存储服务看不到内容；其他设备加入时需要输入同一个口令。",
        ))
        .child(field("口令", pwd(ui.pass1, "")))
        .child(field("再输入一次", pwd(ui.pass2, "")))
        .child(tone(
            "请牢记口令。忘记口令且没有备份密钥时，存储里的数据无法解开。设置完成后可在「安全」里导出密钥备份。",
            Role::Warning,
        ))
        .visible_when(move || remote.get() != 2);
    let s2_join = Element::col()
        .width_match()
        .spacing(12)
        .child(muted("存储里已有同步数据，可以把本机加入进去。"))
        .child(
            Element::col()
                .width_match()
                .spacing(12)
                .child(field("口令", pwd(ui.pass1, "创建同步库时设置的口令")))
                .child(
                    Element::link("没有口令？改用导出的密钥加入")
                        .font_size(12.5)
                        .on_click(move |_| use_key.set(true)),
                )
                .visible_when(move || !use_key.get()),
        )
        .child(
            Element::col()
                .width_match()
                .spacing(12)
                .child(field(
                    "密钥",
                    pwd(ui.import_key, "粘贴在其他设备上导出的密钥"),
                ))
                .child(
                    Element::link("改用口令加入")
                        .font_size(12.5)
                        .on_click(move |_| use_key.set(false)),
                )
                .visible_when(move || use_key.get()),
        )
        .visible_when(move || remote.get() == 2);
    let s2 = Element::col().width_match().child(s2_new).child(s2_join);

    let s3 = Element::col().width_match().spacing(12).child(field(
        "本机名称",
        Element::text_input(ui.device_name, "其他设备上看到的名字").width_match(),
    ));

    let body = Element::col()
        .width_match()
        .padding_xy(20, 16)
        .spacing(16)
        .child(step_header(step))
        .child(Element::divider())
        .child(s1.visible_when(move || step.get() == 0))
        .child(s2.visible_when(move || step.get() == 1))
        .child(s3.visible_when(move || step.get() == 2));

    let wiz = ui.wizard;
    let footer = Element::row()
        .spacing(8)
        .child(btn2("取消").on_click(move |_| wiz.set(false)))
        .child(
            btn2("上一步")
                .visible_when(move || step.get() > 0)
                .on_click(move |_| step.set(step.get().saturating_sub(1))),
        )
        .child(
            btn("下一步")
                .visible_when(move || step.get() < 2)
                .on_click(move |_| step.set(step.get() + 1)),
        )
        .child(
            btn("完成")
                .visible_when(move || step.get() == 2)
                .on_click(move |_| wiz.set(false)),
        );
    Element::dialog(wiz, dialog_frame_w(560, "设置数据同步", body, footer))
}
