//! 应用外壳 — 无边框窗口 + 标题栏 + 左侧栏导航 + 底部操作栏 + 运行时明暗换肤。
//!
//! 标题栏：渐变 Logo + 标题（可拖动）+ 窗口按钮。
//! 左侧栏：分区导航（pill 选中）。
//! 底部操作栏（搜狗式）：左=连接状态 + 明暗切换；右=恢复本页 / 重新加载 / 应用设置 / 保存并退出。
//! 明暗切换通过 `ThemeHandle::set(theme_for(dark))` 整窗换肤（`*_role` 节点自动跟随）。

use std::cell::RefCell;
use std::rc::Rc;

use windui::core::EventCtx;
use windui::prelude::*;

use crate::dialogs::schema_manager::build_schema_manager_dialog;
use crate::dialogs::theme_import_dialog::build_theme_import_dialog;
use crate::dialogs::update_dialog::build_update_dialog;
use crate::pages;
use crate::state::AppState;
use crate::theme_def::theme_for;

/// 构建整窗 UI。`app` 为共享状态，`th` 为换肤句柄。
pub fn build(app: &Rc<RefCell<AppState>>, th: ThemeHandle) -> Element {
    // 借用一次构建页面与对话框，随后释放，便于保存闭包再借 mut。
    let (
        tabs_vec,
        tab_sig,
        dark,
        status,
        feedback,
        dialog,
        dict_dialog,
        theme_import_dialog,
        backup_dialog,
        stats_confirm_dialog,
        reload_confirm,
        reset_confirm,
        conn_ok,
        conn_failed,
        field_dialogs,
    ) = {
        let st = app.borrow();
        // 注意：tabs_vec 必须先求值——all_tabs 构建各 dialog_button 时把对话框登记进
        // st.fields.dialogs，随后才能在本元组里 take 出来挂到根窗口（元组按从左到右求值）。
        let tabs_vec = pages::all_tabs(app, &st);
        // all_tabs 后各「支持全局」的功能键已登记 global_flags，汇总为 keys.global_hotkeys 写回/回填。
        crate::manifest::register_global_hotkeys_collector(&st);
        let field_dialogs = std::mem::take(&mut *st.fields.dialogs.borrow_mut());
        (
            tabs_vec,
            st.tab,
            st.dark,
            st.status,
            st.feedback,
            build_schema_manager_dialog(&st.schema_mgr),
            pages::dict::build_dict_dialogs(&st.dict_mgr),
            build_theme_import_dialog(app),
            pages::backup::build_backup_restore_dialog(&st.backup_mgr),
            pages::stats::build_stats_confirm_dialog(&st.stats_mgr),
            st.reload_confirm,
            st.reset_confirm,
            st.conn_ok,
            st.conn_failed,
            field_dialogs,
        )
    };

    // 静态色控件（图标 tint/渐变/淡底）用它做亮暗双态，须在页面构建前注册。
    crate::widgets::init_dark_sig(dark);

    let titlebar = build_titlebar();

    // 左侧栏：图标导航（选中 = 淡蓝底 + 渐变图标块 + 左缘指示条）。
    let (names, pages): (Vec<String>, Vec<Element>) = tabs_vec.into_iter().unzip();
    let nav = build_nav(&names, tab_sig);
    // 侧栏：Bg 角色 = 左栏精确色（比右内容区 SurfaceAlt 略暗）。
    let sidebar = Element::col()
        .width(184)
        .height_match()
        .bg_role(Role::Bg)
        .padding_xy(8, 10)
        .child(nav);
    let vdivider = Element::leaf()
        .width(1)
        .height_match()
        .bg_role(Role::Divider);

    // 内容区：各分区页叠放，按选中索引切换显示。
    let mut pages_stack = Element::stack().fill().weight(1.0);
    for (i, page) in pages.into_iter().enumerate() {
        pages_stack = pages_stack.child(page.fill().visible_when(move || tab_sig.get() == i));
    }
    // 按键冲突固定顶栏：置于页面滚动区之外，切页/下滚均常驻，无冲突时隐藏不占位。
    let content = Element::col()
        .fill()
        .weight(1.0)
        .child(crate::pages::conflict_bar(app.borrow().conflict))
        .child(pages_stack);
    let body = Element::row()
        .fill()
        .weight(1.0)
        .child(sidebar)
        .child(vdivider)
        .child(content);

    let footer = build_footer(app, th, dark, status, conn_ok);

    let main_col = Element::col()
        .fill()
        .child(titlebar)
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(footer);

    let mut root = Element::stack()
        .fill()
        .bg_role(Role::Bg)
        .child(main_col)
        .child(dialog)
        .child(dict_dialog)
        .child(theme_import_dialog)
        .child(backup_dialog)
        .child(stats_confirm_dialog)
        .child(build_reload_dialog(app, reload_confirm))
        .child(build_reset_dialog(app, reset_confirm))
        .child(build_theme_delete_dialog(app))
        .child(build_update_dialog(app))
        .child(build_conn_failed_dialog(app, conn_failed));
    // dialog_button 的对话框挂到根：全窗模态遮罩 + 居中（否则内联在设置行格内会偏下）。
    for dlg in field_dialogs {
        root = root.child(dlg);
    }
    // 全局 toast 反馈汇：保存/重载/恢复/协议路由等瞬时结果统一在此弹出（底栏 status 只保留连接态）。
    root = root.child(crate::pages::dict::reactive::toast_sink(feedback));
    // 触发键冲突实时监听：每帧刷新 state.conflict，驱动输入页常驻警告条。
    root.child(crate::pages::conflict_watcher(app))
}

/// 侧栏导航：图标+文字项；选中态 = accent 淡底 + 渐变图标块（白图标）+ 左缘指示条。
/// 选中/未选中为两套元素 visible_when 切换（windui 无 hover-only/响应式样式）；
/// 渐变图标块为静态色，按 `widgets::dark_sig()` 再分亮/暗双态。
/// 「高级」项前插分隔线（参考稿 nav-sep）。
fn build_nav(names: &[String], sel: Signal<usize>) -> Element {
    use crate::icons::{nav_icon, tinted};
    use crate::theme_def::{ACCENT_DARK, ACCENT_LIGHT};

    let mut col = Element::col().width_match().spacing(2);
    // 顶部小节标题。
    col = col.child(
        // 高度需容纳字号 + 上下 padding（固定高含 padding，给小了会截半字）。
        Element::label("设置")
            .font_size(11.0)
            .font_weight(600)
            .fg_role(Role::TextDisabled)
            .width_match()
            .max_lines(1)
            .height(26)
            .padding_xy(10, 5),
    );

    for (i, name) in names.iter().enumerate() {
        if name == "高级" {
            col = col.child(
                Element::col().width_match().padding_xy(10, 8).child(
                    Element::leaf()
                        .width_match()
                        .height(1)
                        .bg_role(Role::Divider),
                ),
            );
        }
        let svg = nav_icon(name);

        // ── 选中态（亮/暗双态渐变图标块 + accent 文字 + 左指示条）──
        let sel_chip = |accent: u32, active: u32, on_dark: bool| {
            Element::stack()
                .size(26, 26)
                .corner(7.0)
                .bg_gradient(Gradient::linear(
                    (0.0, 0.0),
                    (1.0, 1.0),
                    vec![(0.0, Color::hex(active)), (1.0, Color::hex(accent))],
                ))
                .child(tinted(svg, 16, Color::WHITE).align(Align::Center))
                .visible_when(move || crate::widgets::dark_sig().get() == on_dark)
        };
        // 左缘指示条：作 stack 覆盖层贴行左缘垂直居中，不占行内宽度——
        // 选中/未选中 padding 一致，切换时图标/文字不发生横向偏移。
        let indicator = Element::row()
            .width_match()
            .height(36)
            .cross(Align::Center)
            .child(
                Element::leaf()
                    .width(3)
                    .height(14)
                    .corner(1.5)
                    .bg_role(Role::Accent),
            )
            .visible_when(move || sel.get() == i);
        let on = Element::row()
            .width_match()
            .height(36)
            .corner(9.0)
            .cross(Align::Center)
            .spacing(9)
            .padding_xy(10, 0)
            .bg(Color::hex(ACCENT_LIGHT).scale_alpha(0.11))
            .child(
                Element::stack()
                    .child(sel_chip(ACCENT_DARK, 0x4A7CE8, true))
                    .child(sel_chip(ACCENT_LIGHT, 0x2159D6, false)),
            )
            .child(
                Element::label(name)
                    .font_size(13.0)
                    .font_weight(600)
                    .fg_role(Role::Accent)
                    .weight(1.0)
                    .max_lines(1)
                    .height(18),
            )
            .visible_when(move || sel.get() == i);

        // ── 未选中态（可点击；图标中性灰，两主题通用）──
        let off = Element::row()
            .clickable()
            .on_click(move |_| sel.set(i))
            .width_match()
            .height(36)
            .corner(9.0)
            .cross(Align::Center)
            .spacing(9)
            .padding_xy(10, 0)
            .child(
                Element::stack()
                    .size(26, 26)
                    .corner(7.0)
                    .bg_role(Role::Bg)
                    .child(tinted(svg, 16, Color::hex(0x8A93A5)).align(Align::Center)),
            )
            .child(
                Element::label(name)
                    .font_size(13.0)
                    .font_weight(500)
                    .fg_role(Role::TextMuted)
                    .weight(1.0)
                    .max_lines(1)
                    .height(18),
            )
            .visible_when(move || sel.get() != i);

        col = col.child(
            Element::stack()
                .width_match()
                .height(36)
                .child(on)
                .child(off)
                .child(indicator),
        );
    }
    Element::scroll().fill().child(col)
}

/// 自定义标题栏（无边框窗口的可拖动区 + 窗口按钮）。状态与操作在底部操作栏。
fn build_titlebar() -> Element {
    // 应用图标（与 exe/任务栏同一张图，PNG 由 build 期从 .ico 提取的 256px 帧）。
    let logo = crate::widgets::app_icon(26);

    Element::row()
        .width_match()
        .height(44)
        .cross(Align::Center)
        .padding_xy(14, 0)
        .spacing(10)
        .bg_role(Role::SurfaceAlt)
        .window_drag()
        .child(logo)
        // 三段式标题：产品名加粗、间隔点与「设置」弱化分层（参考稿 titlebar）。
        .child(
            Element::row()
                .cross(Align::Center)
                .spacing(4)
                .child(
                    Element::label("清风输入法")
                        .font_size(13.0)
                        .font_weight(600)
                        .fg_role(Role::Text),
                )
                .child(
                    Element::label("·")
                        .font_size(13.0)
                        .fg_role(Role::TextDisabled),
                )
                .child(
                    Element::label("设置")
                        .font_size(13.0)
                        .fg_role(Role::TextMuted),
                ),
        )
        // 版本号（build 期取自 git tag/hash，弱化色小字，与标题基线对齐）；dev 变体追加「(开发版)」。
        .child(
            Element::label(format!(
                "{}{}",
                env!("WIND_SETTING_VERSION"),
                crate::mode::version_suffix()
            ))
            .font_size(11.0)
            .fg_role(Role::TextMuted),
        )
        .child(Element::leaf().weight(1.0))
        // 图标随主题明暗（否则默认深色字在暗色标题栏上不可见）。
        .child(Element::window_button(WindowButtonKind::Minimize).fg_role(Role::Text))
        .child(Element::window_button(WindowButtonKind::Maximize).fg_role(Role::Text))
        .child(Element::window_button(WindowButtonKind::Close).fg_role(Role::Text))
}

/// 底部操作栏（搜狗式）：左=连接状态点 + 文字 + 明暗切换；右=恢复本页/重新加载/应用设置/保存并退出。
fn build_footer(
    app: &Rc<RefCell<AppState>>,
    th: ThemeHandle,
    dark: Signal<bool>,
    status: Signal<String>,
    conn_ok: Signal<bool>,
) -> Element {
    // 连接状态点：绿=已连接，琥珀=未连接；外圈 22% alpha 光环（参考稿 status dot）。
    // 响应式（连接在异步加载后才确定，构建期总为未连接）：
    // 用两点 visible_when 切换，连接成功后自动转绿（同明暗切换按钮的双态模式）。
    let halo_dot = |c: u32, when_ok: bool| {
        Element::stack()
            .size(14, 14)
            .corner(7.0)
            .bg(Color::hex(c).scale_alpha(0.22))
            .child(
                Element::leaf()
                    .size(8, 8)
                    .corner(4.0)
                    .bg(Color::hex(c))
                    .align(Align::Center),
            )
            .visible_when(move || conn_ok.get() == when_ok)
    };
    let status_dot = Element::stack()
        .child(halo_dot(0x22C55E, true))
        .child(halo_dot(0xF59E0B, false));

    // 明暗切换：随状态更新的 emoji 图标（暗色显示 ☀ 转浅；亮色显示 🌙 转深）。
    let th_l = th.clone();
    let d_l = dark;
    let to_light = Element::button("☀ 浅色")
        .small()
        .outline_soft()
        .on_click(move |_| {
            d_l.set(false);
            th_l.set(theme_for(false));
        })
        .visible_when(move || d_l.get());
    let th_d = th;
    let d_d = dark;
    let to_dark = Element::button("🌙 深色")
        .small()
        .outline_soft()
        .on_click(move |_| {
            d_d.set(true);
            th_d.set(theme_for(true));
        })
        .visible_when(move || !d_d.get());
    let theme_btn = Element::stack().child(to_light).child(to_dark);

    // 恢复本页：本页改默认值；有未保存改动先弹确认。
    let app_reset = app.clone();
    let reset_btn = Element::button("恢复本页")
        .small()
        .outline_soft()
        .on_click(move |_| {
            if let Ok(s) = app_reset.try_borrow() {
                if s.is_dirty() {
                    s.reset_confirm.set(true);
                } else {
                    s.reset_current_page();
                }
            }
        });

    // 重新加载：从服务端重读，丢弃改动——二次确认。
    let app_reload = app.clone();
    let reload_btn = Element::button("重新加载")
        .small()
        .outline_soft()
        .on_click(move |_| {
            if let Ok(s) = app_reload.try_borrow() {
                s.reload_confirm.set(true);
            }
        });

    // 应用设置：保存当前改动（不退出）。
    let app_apply = app.clone();
    let apply_btn = Element::button("应用设置")
        .small()
        .outline_soft()
        .on_click(move |_| {
            if let Ok(mut s) = app_apply.try_borrow_mut() {
                s.save();
            }
        });

    // 保存并退出：保存成功（无报错）才退出。
    let app_save_exit = app.clone();
    let save_exit_btn = Element::button("保存并退出").small().on_click(move |ctx| {
        if let Ok(mut s) = app_save_exit.try_borrow_mut()
            && s.save()
        {
            ctx.request_close();
        }
    });

    // 底栏与标题栏同色(SurfaceAlt)，与内容区卡片(Surface)区分；上方分割线由 main_col 提供层次。
    Element::row()
        .width_match()
        .height(48)
        .cross(Align::Center)
        .padding_xy(14, 0)
        .spacing(8)
        .bg_role(Role::SurfaceAlt)
        .child(status_dot)
        .child(
            Element::label_rc(status)
                .font_size(11.5)
                .fg_role(Role::TextMuted)
                .max_lines(1),
        )
        .child(Element::leaf().width(6))
        .child(theme_btn)
        .child(Element::leaf().weight(1.0))
        .child(reset_btn)
        .child(reload_btn)
        .child(apply_btn)
        .child(save_exit_btn)
}

/// 「重新加载」二次确认对话框。
fn build_reload_dialog(app: &Rc<RefCell<AppState>>, visible: Signal<bool>) -> Element {
    let app_c = app.clone();
    confirm_dialog(
        visible,
        "重新加载",
        "将丢弃所有未保存的修改，从服务端重新读取配置。确定继续？",
        "重新加载",
        move |_| {
            if let Ok(mut s) = app_c.try_borrow_mut() {
                s.reload();
            }
            visible.set(false);
        },
    )
}

/// 「恢复本页」确认对话框（仅在有未保存改动时弹出）。
fn build_reset_dialog(app: &Rc<RefCell<AppState>>, visible: Signal<bool>) -> Element {
    let app_c = app.clone();
    confirm_dialog(
        visible,
        "恢复本页为默认值",
        "本页存在未保存的修改，恢复默认会丢弃这些修改。确定恢复为系统默认值？",
        "恢复默认",
        move |_| {
            if let Ok(s) = app_c.try_borrow() {
                s.reset_current_page();
            }
            visible.set(false);
        },
    )
}

/// 「删除主题」确认对话框：正文含主题名（`theme_delete_msg` 动态填充），确认按钮为危险色。
/// 由主题下拉里外部主题的删除图标触发显示，确认后调 `theme.delete` 并刷新列表。
fn build_theme_delete_dialog(app: &Rc<RefCell<AppState>>) -> Element {
    let st = app.borrow();
    let visible = st.theme_delete_visible;
    let msg = st.theme_delete_msg;
    drop(st);
    let app_c = app.clone();
    let frame = Element::col()
        .width(360)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("删除主题")
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(22)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label_rc(msg)
                .font_size(13.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(4)
                .padding_xy(18, 16),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| visible.set(false)),
                )
                .child(Element::button("删除").small().danger().on_click(move |_| {
                    if let Ok(mut s) = app_c.try_borrow_mut() {
                        s.confirm_delete_theme();
                    }
                })),
        );
    Element::dialog(visible, frame)
}

/// 连接失败阻断对话框：[重连] 重跑后台加载；[退出] 关闭应用。无取消项（阻断）。
fn build_conn_failed_dialog(app: &Rc<RefCell<AppState>>, visible: Signal<bool>) -> Element {
    let app_retry = app.clone();
    let frame = Element::col()
        .width(360)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("无法连接输入法服务")
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(22)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label("设置应用需要输入法服务在运行。请确认服务已启动后重连，或退出。")
                .font_size(13.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3)
                .padding_xy(18, 16),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("退出")
                        .small()
                        .outline_soft()
                        .on_click(|ctx| ctx.request_close()),
                )
                .child(Element::button("重连").small().on_click(move |_| {
                    if let Ok(s) = app_retry.try_borrow() {
                        s.status.set("正在重连...".to_string());
                    }
                    crate::main_reconnect(&app_retry);
                })),
        );
    Element::dialog(visible, frame)
}

/// 通用二次确认对话框：标题 + 说明 + [取消][确认]。`on_confirm` 执行后请自行关闭。
fn confirm_dialog(
    visible: Signal<bool>,
    title: &str,
    message: &str,
    confirm_label: &str,
    on_confirm: impl FnMut(&mut EventCtx) + 'static,
) -> Element {
    let cancel = visible;
    let frame = Element::col()
        .width(360)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(22)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(message)
                .font_size(13.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(4)
                .padding_xy(18, 16),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| cancel.set(false)),
                )
                .child(Element::button(confirm_label).small().on_click(on_confirm)),
        );
    Element::dialog(visible, frame)
}
