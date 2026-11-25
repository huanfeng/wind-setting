//! 同步页的对话框。

use windui::prelude::*;

use super::{PageUi, SyncUi, model::*};
use crate::state::AppState;

/// 两个对话框挂到字段注册表的 `dialogs`（同扩展程序卡），随页面生命周期。
pub(super) fn ui_dialogs(ui: &SyncUi, state: &AppState) {
    state
        .fields
        .dialogs
        .borrow_mut()
        .push(passphrase_dialog(ui));
    state
        .fields
        .dialogs
        .borrow_mut()
        .push(mass_delete_dialog(ui));
    state.fields.dialogs.borrow_mut().push(reset_dialog(ui));
    state
        .fields
        .dialogs
        .borrow_mut()
        .push(remove_device_dialog(ui));
}

fn dialog_frame(title: &str, body: Element, footer: Element) -> Element {
    Element::col()
        .width(460)
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
    Element::dialog(visible, dialog_frame("同步口令", body, footer))
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

fn wizard_dialog(ui: &SyncUi, p: &PageUi) -> Element {
    let (step, remote, use_key, rewire) = (p.step, p.remote, p.use_key, p.rewire);
    let is_wd = {
        let b = ui.backend;
        move || b.get() == 0
    };
    let is_s3 = {
        let b = ui.backend;
        move || b.get() == 1
    };
    let inp = |s: Signal<String>, ph: &str| Element::text_input(s, ph).width_match();
    let pwd = |s: Signal<String>, ph: &str| Element::text_input(s, ph).password().width_match();
    let two = |a: Element, b: Element| {
        Element::row()
            .width_match()
            .spacing(12)
            .child(a.weight(1.0))
            .child(b.weight(1.0))
    };

    // ── 第 1 步：存储 ──
    let s1 = Element::col()
        .width_match()
        .spacing(12)
        .child(
            tone(
                "更换存储位置相当于重新设置：本机会离开当前同步库，按下面的新位置重新连接和加入。原存储里的数据不会被删除。",
                Role::Warning,
            )
            .visible_when(move || rewire.get()),
        )
        .child(field(
            "存储类型",
            Element::segmented(vec!["WebDAV", "S3 兼容存储"], ui.backend),
        ))
        .child(
            Element::col()
                .width_match()
                .spacing(12)
                .child(field("WebDAV 地址", inp(ui.wd_url, "https://dav.example.com/dav/")))
                .child(muted("坚果云等服务请填写其「第三方应用」里给出的地址，并使用应用专用密码。"))
                .child(two(
                    field("用户名", inp(ui.wd_user, "")),
                    field("密码", pwd(ui.wd_password, "")),
                ))
                .visible_when(is_wd),
        )
        .child(
            Element::col()
                .width_match()
                .spacing(12)
                .child(field("Endpoint", inp(ui.s3_endpoint, "https://s3.example.com")))
                .child(two(
                    field("Bucket", inp(ui.s3_bucket, "")),
                    field("Region", inp(ui.s3_region, "auto")),
                ))
                .child(two(
                    field("Access Key ID", inp(ui.s3_access_key_id, "")),
                    field("Secret", pwd(ui.s3_secret, "")),
                ))
                .child(two(
                    field("路径前缀（可留空）", inp(ui.s3_prefix, "windinput/")),
                    field(
                        "寻址方式",
                        Element::checkbox("Path-style（MinIO 等自建服务）", ui.s3_path_style),
                    ),
                ))
                .visible_when(is_s3),
        );

    // ── 第 2 步：口令 ──
    let s2_new = Element::col()
        .width_match()
        .spacing(12)
        .child(
            Element::row()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::badge_intent("连接成功", Intent::Success))
                .child(muted("这是一个新的同步库，本机将作为第一台设备。")),
        )
        .child(muted(
            "设置一个口令。同步数据在本机加密后才上传，存储服务看不到内容；其他设备加入时需要输入同一个口令。",
        ))
        .child(field("口令", pwd(ui.pass1, "至少 8 个字符")))
        .child(field("再输入一次", pwd(ui.pass2, "")))
        .child(tone(
            "请牢记口令。忘记口令且没有备份密钥时，存储里的数据无法解开。设置完成后可在「存储与安全」里导出密钥备份。",
            Role::Warning,
        ))
        .visible_when(move || remote.get() != 2);
    let s2_join = Element::col()
        .width_match()
        .spacing(12)
        .child(
            Element::row()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::badge_intent("连接成功", Intent::Success))
                .child(muted("存储里已有同步数据，可以把本机加入进去。")),
        )
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
        .child(muted(
            "加入后，本机与存储里的数据会合并；不会删除任何一边已有的词。",
        ))
        .visible_when(move || remote.get() == 2);
    let s2 = Element::col().width_match().child(s2_new).child(s2_join);

    // ── 第 3 步：内容 ──
    let s3 = Element::col()
        .width_match()
        .spacing(12)
        .child(field(
            "本机名称",
            inp(ui.device_name, "其他设备上看到的名字"),
        ))
        .child(field(
            "自动同步间隔（分钟）",
            Element::number_input(ui.interval, 1.0, 1440.0, 1.0),
        ))
        .child(muted("本机文件有改动时会很快同步，不必等到间隔。"));

    let body = Element::col()
        .width_match()
        .padding_xy(20, 16)
        .spacing(16)
        .child(step_header(step))
        .child(Element::divider())
        .child(s1.visible_when(move || step.get() == 0))
        .child(s2.visible_when(move || step.get() == 1))
        .child(s3.visible_when(move || step.get() == 2));

    let wiz = p.wiz;
    let cfg = p.configured;
    let footer = Element::row()
        .spacing(8)
        .child(btn2("取消").on_click(move |_| wiz.set(false)))
        .child(
            btn2("上一步")
                .visible_when(move || step.get() > 0)
                .on_click(move |_| step.set(step.get().saturating_sub(1))),
        )
        .child(
            btn("测试连接并继续")
                .visible_when(move || step.get() == 0)
                .on_click(move |_| {
                    step.set(1);
                }),
        )
        .child(
            btn("下一步")
                .visible_when(move || step.get() == 1)
                .on_click(move |_| step.set(2)),
        )
        .child(
            btn("完成并开始同步")
                .visible_when(move || step.get() == 2)
                .on_click(move |_| {
                    wiz.set(false);
                    cfg.set(true);
                }),
        );
    let title = rewire.map(|r| {
        if *r {
            "更换存储".to_string()
        } else {
            "设置数据同步".to_string()
        }
    });
    let frame = Element::col()
        .width(560)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label_signal(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(20, 14),
        )
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .child(Element::leaf().weight(1.0))
                .child(footer),
        );
    Element::dialog(wiz, frame)
}

fn simple_dialog(visible: Signal<bool>, title: &str, body: Element, ok: Element) -> Element {
    let footer = Element::row()
        .spacing(8)
        .child(btn2("取消").on_click(move |_| visible.set(false)))
        .child(ok);
    Element::dialog(
        visible,
        dialog_frame(title, body.padding_xy(18, 14), footer),
    )
}

pub(super) fn spike_dialogs(ui: &SyncUi, p: &PageUi) -> Vec<Element> {
    let rename = p.rename;
    let rename_d = simple_dialog(
        rename,
        "本机名称",
        Element::col()
            .width_match()
            .spacing(8)
            .child(muted(
                "其他设备的「我的设备」里显示这个名字。只保存在本机，不随同步传播。",
            ))
            .child(Element::text_input(ui.device_name, "留空使用计算机名").width_match()),
        btn("保存").on_click(move |_| rename.set(false)),
    );
    let export = p.export;
    let key = ui.exported_key;
    let export_d = Element::dialog(
        export,
        dialog_frame(
            "导出密钥",
            Element::col()
                .width_match()
                .padding_xy(18, 14)
                .spacing(10)
                .child(muted(
                    "这串密钥可以直接解开同步数据，作用等同口令。可用来在新设备上免输口令加入，或在忘记口令时找回数据。",
                ))
                .child(
                    Element::row()
                        .width_match()
                        .cross(Align::Center)
                        .spacing(6)
                        .child(Element::text_input(key, "").read_only().weight(1.0))
                        .child(btn2("复制").on_click(move |ctx| {
                            ctx.clipboard_set(&key.get());
                            ctx.toast_ok("已复制密钥");
                        })),
                )
                .child(tone(
                    "请离线保存（如密码管理器），不要发到聊天或网盘。任何拿到它的人都能读取你的同步数据。",
                    Role::Warning,
                )),
            btn("完成").on_click(move |_| export.set(false)),
        ),
    );
    let creds = p.creds;
    let creds_d = simple_dialog(
        creds,
        "更新存储密码",
        Element::col()
            .width_match()
            .spacing(8)
            .child(muted(
                "存储服务那边改了密码或 Secret 时，在这里填入新的。保存前会先测试连接。",
            ))
            .child(
                Element::text_input(ui.wd_password, "新密码")
                    .password()
                    .width_match(),
            ),
        btn("测试并保存").on_click(move |_| creds.set(false)),
    );
    let conflicts = p.conflicts;
    let paths = [
        "themes/dark.conflict-laptop-2026-11-23.json",
        "data/phrases.dict.yaml.conflict-laptop-2026-11-22",
    ];
    let mut list = Element::col().width_match().spacing(6);
    for x in paths {
        list = list.child(
            Element::label(x)
                .font_size(12.5)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(10, 6)
                .bg_role(Role::SurfaceAlt)
                .corner(6.0),
        );
    }
    let conflicts_d = Element::dialog(
        conflicts,
        dialog_frame(
            "冲突副本",
            Element::col()
                .width_match()
                .padding_xy(18, 14)
                .spacing(10)
                .child(muted(
                    "两台设备同时改了同一个文件。本机版本保留原名，另一台的版本另存为下面的副本。请比对后保留需要的内容，并删除副本。",
                ))
                .child(list),
            Element::row()
                .spacing(8)
                .child(btn2("打开用户目录"))
                .child(btn("关闭").on_click(move |_| conflicts.set(false))),
        ),
    );
    let disable = p.disable;
    let disable_d = simple_dialog(
        disable,
        "停用同步",
        Element::col().width_match().spacing(8).child(muted(
            "本机停止同步。存储设置、口令和存储里的数据都会保留，之后可以随时重新启用。",
        )),
        btn("停用").on_click(move |_| disable.set(false)),
    );
    vec![
        wizard_dialog(ui, p),
        rename_d,
        export_d,
        creds_d,
        conflicts_d,
        disable_d,
    ]
}
