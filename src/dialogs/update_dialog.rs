//! 在线升级对话框。
//!
//! 七个阶段（[`UpdatePhase`]）共用一个框架，正文与按钮行各自按阶段 `visible_when` 切换。
//!
//! 所有耗时动作都经 `AppState::spawn_*` 起后台线程，结果由 `App::channel` 回到 UI 线程
//! 写 Signal —— 对话框本身不碰网络，也就不会阻塞渲染。

use std::cell::RefCell;
use std::rc::Rc;

use windui::prelude::*;

use crate::state::AppState;
use crate::update::{self, UpdatePhase};

/// 没有自动安装通路的平台（Linux）在「发现新版本」时打开的页面。
const DOWNLOAD_PAGE_URL: &str = "https://windinput.com/download";

/// 对话框宽度。发布说明常有较长条目，比常规对话框宽一档可减少折行。
const DIALOG_W: i32 = 620;
/// 发布说明区高度上限：超出即滚动。
///
/// 用 `max_height` 而非固定高，短说明（多数版本只有两三条）时对话框自然收缩，
/// 不会留一大片空白；长说明则封顶在此高度，按钮行始终留在视野内。
const NOTES_MAX_H: i32 = 220;

pub fn build_update_dialog(app_state: &Rc<RefCell<AppState>>) -> Element {
    let st = app_state.borrow();
    let visible = st.update_dialog_visible;
    let phase = st.update_phase;
    let message = st.update_message;
    let size_text = st.update_size_text;
    let notes = st.update_notes;
    let progress = st.update_progress;
    let progress_text = st.update_progress_text;
    drop(st);

    let is = move |p: UpdatePhase| move || phase.get() == p;

    let frame = Element::col()
        .width(DIALOG_W)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(title_row())
        .child(Element::divider())
        .child(consent_body().visible_when(is(UpdatePhase::Consent)))
        .child(checking_body().visible_when(is(UpdatePhase::Checking)))
        .child(has_update_body(message, size_text, notes).visible_when(is(UpdatePhase::HasUpdate)))
        .child(no_update_body(message, notes).visible_when(is(UpdatePhase::NoUpdate)))
        .child(
            downloading_body(message, progress, progress_text)
                .visible_when(is(UpdatePhase::Downloading)),
        )
        .child(simple_body(message).visible_when(is(UpdatePhase::Ready)))
        .child(error_body(message).visible_when(is(UpdatePhase::Error)))
        .child(Element::divider())
        .child(footer(app_state, phase, visible))
        // 不可见叶子：checkbox 只写信号无回调，靠它把变更落盘
        .child(auto_check_persister(app_state));

    Element::dialog(visible, frame)
}

/// 把「自动检查」勾选框的变更写进 wind_setting.toml。
///
/// windui 的 checkbox 只写绑定信号、不提供 on_change，项目既有做法是挂一个不可见的
/// `reactive_effect` 叶子监听信号版本（同 `pages/dict/reactive.rs` 的注释）。
/// 它以构建帧为基线，故打开对话框本身不会触发一次多余的写盘。
fn auto_check_persister(app_state: &Rc<RefCell<AppState>>) -> Element {
    let sig = app_state.borrow().update_auto_check_sig;
    let st = app_state.clone();
    crate::pages::dict::reactive::reactive_effect(
        move || sig.version(),
        move || st.borrow_mut().save_update_auto_check(sig.get()),
    )
}

/// 标题行。Dev 变体加徽章 —— 用户须一眼看出自己在升级哪条产品线。
fn title_row() -> Element {
    let mut row = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(18, 14)
        .child(
            Element::label("检查更新")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text),
        );
    if crate::mode::get() == crate::mode::AppMode::Dev {
        row = row.child(Element::badge("Dev"));
    }
    row
}

/// 首次联网同意页。输入法产品不得默认联网，须明示将访问哪些地址。
fn consent_body() -> Element {
    let host = if crate::mode::get() == crate::mode::AppMode::Dev {
        "dl.windinput.com/latest-dev.json（开发版更新源）"
    } else {
        "dl.windinput.com/latest.json（正式版更新源）"
    };
    Element::col()
        .width_match()
        .spacing(6)
        .padding_xy(18, 12)
        .child(
            Element::label("检查更新需要联网，这是清风输入法首次使用网络功能。")
                .font_size(12.0)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(2),
        )
        .child(
            Element::label(format!("将访问：{host}"))
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(2),
        )
        .child(
            Element::label("不会收集任何个人信息或输入内容。")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(1),
        )
}

fn checking_body() -> Element {
    Element::col()
        .width_match()
        .spacing(10)
        .padding_xy(18, 16)
        .child(
            Element::label("正在检查更新…")
                .font_size(12.0)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(1),
        )
        .child(Element::progress_indeterminate().width_match())
}

fn has_update_body(
    message: Signal<String>,
    size_text: Signal<String>,
    notes: Signal<String>,
) -> Element {
    Element::col()
        .width_match()
        .spacing(6)
        .padding_xy(18, 12)
        .child(
            Element::label_signal(message)
                .font_size(13.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(2),
        )
        .child(
            Element::label_signal(size_text)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(1),
        )
        .child(notes_area(notes))
}

/// 发布说明区：内容超过 [`NOTES_MAX_H`] 即可滚动。
///
/// 此前是一个 `max_lines(8)` 的裸 label——超过八行的说明后半段既看不到也滚不动。
/// 现在交给滚动容器：label 不再限行，按完整高度测量，溢出部分成为可滚动量。
fn notes_area(notes: Signal<String>) -> Element {
    Element::scroll()
        .width_match()
        .max_height(NOTES_MAX_H)
        .child(
            Element::label_signal(notes)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match(),
        )
}

/// 「已是最新」正文：结论 + 最新版的更新记录。
///
/// 展示更新记录不是多余的——用户点开"检查更新"，除了想知道有没有新版，也想知道
/// 手上这个版本都带了什么。说明拉取失败或为空时整段隐藏，不留一个空框。
fn no_update_body(message: Signal<String>, notes: Signal<String>) -> Element {
    let has_notes = move || !notes.get().trim().is_empty();
    Element::col()
        .width_match()
        .spacing(8)
        .padding_xy(18, 14)
        .child(
            Element::label_signal(message)
                .font_size(12.0)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(2),
        )
        .child(
            Element::label("本版更新记录")
                .font_size(12.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(1)
                .visible_when(has_notes),
        )
        .child(notes_area(notes).visible_when(has_notes))
}

fn simple_body(message: Signal<String>) -> Element {
    Element::col().width_match().padding_xy(18, 16).child(
        Element::label_signal(message)
            .font_size(12.0)
            .fg_role(Role::Text)
            .width_match()
            .max_lines(3),
    )
}

fn error_body(message: Signal<String>) -> Element {
    Element::col().width_match().padding_xy(18, 16).child(
        Element::label_signal(message)
            .font_size(12.0)
            .fg_role(Role::Danger)
            .width_match()
            .max_lines(4),
    )
}

fn downloading_body(
    message: Signal<String>,
    progress: Signal<f32>,
    progress_text: Signal<String>,
) -> Element {
    Element::col()
        .width_match()
        .spacing(8)
        .padding_xy(18, 14)
        .child(
            Element::label_signal(message)
                .font_size(12.0)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(1),
        )
        .child(Element::progress(progress).width_match())
        .child(
            Element::label_signal(progress_text)
                .font_size(11.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(1),
        )
}

/// 按钮行：每个阶段一组，靠 `visible_when` 切换。
fn footer(
    app_state: &Rc<RefCell<AppState>>,
    phase: Signal<UpdatePhase>,
    visible: Signal<bool>,
) -> Element {
    let is = move |p: UpdatePhase| move || phase.get() == p;
    let auto_check = app_state.borrow().update_auto_check_sig;

    let consent_state = app_state.clone();
    let recheck_state = app_state.clone();
    let download_state = app_state.clone();
    let cancel_state = app_state.clone();
    let retry_state = app_state.clone();
    let install_state = app_state.clone();
    let skip_state = app_state.clone();

    // 「自动检查」在有结论的两个阶段（有更新／已最新）显示 —— 用户此刻最容易想到调整它。
    // 检查中/下载中不显示，避免在进行中的操作旁放一个会改行为的开关。
    let show_auto_check =
        move || matches!(phase.get(), UpdatePhase::HasUpdate | UpdatePhase::NoUpdate);

    // 关闭按钮的文案由阶段派生。windui 没有 derived signal，故用本仓惯用的
    // `reactive_effect`（隐藏叶子轮询 version）把 phase 的变化搬到文案信号上。
    // 下载中不更新：那一阶段按钮本就不可见，留着上一次的文案即可。
    let close_label_sig = signal(close_label(phase.get()).unwrap_or("关闭").to_string());
    let label_sync = crate::pages::dict::reactive::reactive_effect(
        move || phase.version(),
        move || {
            if let Some(t) = close_label(phase.get()) {
                close_label_sig.set(t.to_string());
            }
        },
    );

    Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 12)
        .spacing(8)
        .child(
            Element::checkbox("打开设置时自动检查更新", auto_check)
                .font_size(12.0)
                .visible_when(show_auto_check),
        )
        .child(Element::leaf().weight(1.0))
        // 关闭按钮全阶段共用一个：六个阶段里它只是文案不同（暂不／稍后再说／关闭），
        // 行为完全一样。文案绑信号（windui 0.12 起控件文案可收 `Signal<String>`），
        // 于是「哪些阶段有关闭按钮、各叫什么」收进 `close_label` 一处 match——原先是
        // 六个各带 visible_when 的按钮，而"下载中没有关闭按钮"只能靠"没写"来表达。
        .child(
            Element::button(close_label_sig)
                .small()
                .outline_soft()
                .on_click(move |_| visible.set(false))
                .visible_when(move || close_label(phase.get()).is_some()),
        )
        .child(
            Element::button("允许联网并检查")
                .small()
                .on_click(move |_| consent_state.borrow_mut().accept_update_consent())
                .visible_when(is(UpdatePhase::Consent)),
        )
        // HasUpdate
        .child(
            Element::button("跳过此版本")
                .small()
                .outline_soft()
                .on_click(move |_| {
                    // 取远端版本号而非本地：跳过的是"这个新版本"
                    let ver = skip_state
                        .borrow()
                        .update_check
                        .borrow()
                        .as_ref()
                        .map(|c| c.remote_version.clone());
                    if let Some(v) = ver {
                        skip_state.borrow_mut().skip_update_version(v);
                    }
                })
                .visible_when(is(UpdatePhase::HasUpdate)),
        )
        .child(
            Element::button("下载更新")
                .small()
                .on_click(move |_| download_state.borrow().spawn_update_download())
                .visible_when(move || {
                    phase.get() == UpdatePhase::HasUpdate && update::self_update_supported()
                }),
        )
        // 没有自动安装通路的平台（Linux）：引导到下载页，由用户用 apt 覆盖安装
        .child(
            Element::button("打开下载页")
                .small()
                .on_click(|ctx| ctx.open_url(DOWNLOAD_PAGE_URL))
                .visible_when(move || {
                    phase.get() == UpdatePhase::HasUpdate && !update::self_update_supported()
                }),
        )
        // NoUpdate
        .child(
            Element::button("重新检查")
                .small()
                .outline_soft()
                .on_click(move |_| recheck_state.borrow().begin_update_check())
                .visible_when(is(UpdatePhase::NoUpdate)),
        )
        // Downloading —— 取消只置旗，下载线程在下一个数据块边界响应
        .child(
            Element::button("取消下载")
                .small()
                .outline_soft()
                .on_click(move |_| cancel_state.borrow().cancel_update_download())
                .visible_when(is(UpdatePhase::Downloading)),
        )
        // Ready（安装由 P4 接线）
        .child(
            Element::button("立即安装")
                .small()
                .on_click(move |ctx| {
                    let st = install_state.borrow();
                    match st.launch_update_install() {
                        // 安装器随即会终止本进程，主动让出窗口比等它来杀更干净。
                        // 必须绕过未保存守卫（close_window）：安装器要等本进程退出才能
                        // 替换文件，此时弹确认框会变成"安装器等窗口关、窗口等用户回答"。
                        Ok(true) => st.close_window(ctx),
                        // UAC 被拒：留在当前阶段，用户可再点一次
                        Ok(false) => ctx.toast("已取消提权，未开始安装"),
                        Err(e) => ctx.toast_err(e),
                    }
                })
                .visible_when(is(UpdatePhase::Ready)),
        )
        // Error
        .child(
            Element::button("重试")
                .small()
                .outline_soft()
                .on_click(move |_| retry_state.borrow().begin_update_check())
                .visible_when(is(UpdatePhase::Error)),
        )
        .child(label_sync)
}

/// 各阶段「关闭」按钮的文案；`None` = 该阶段不给关闭按钮。
///
/// 下载中刻意没有：此时只能「取消下载」——直接关掉对话框会让下载在后台继续跑而界面
/// 无处可回，而取消是明确的意思表示。这条规则过去分散在六个 `visible_when` 里，
/// "下载中没有"只能靠"没写"来表达，读代码时要把六个分支数一遍才看得出来。
fn close_label(p: UpdatePhase) -> Option<&'static str> {
    match p {
        UpdatePhase::Consent => Some("暂不"),
        UpdatePhase::HasUpdate => Some("稍后再说"),
        UpdatePhase::Checking | UpdatePhase::NoUpdate | UpdatePhase::Ready | UpdatePhase::Error => {
            Some("关闭")
        }
        UpdatePhase::Downloading => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windui::core::Tree;
    use windui::geometry::Size;

    /// 布局 notes 区，返回 (节点占位高, 完整内容高)。
    fn layout_notes(text: &str) -> (i32, i32) {
        let sig = windui::signal::signal(text.to_string());
        let mut tree = Tree::new();
        // 外层给定与对话框一致的宽度，使 label 的换行条件贴近真实。
        let root = Element::col()
            .width(DIALOG_W)
            .child(notes_area(sig))
            .build(&mut tree);
        tree.root = Some(root);
        // 用按行折算的引擎：NullTextEngine 恒返回单行高，测不出"内容变高→溢出→可滚动"。
        tree.layout_root(
            Size::new(DIALOG_W, 800),
            &mut windui::text::LineAwareTextEngine,
        );
        let scroll = tree.get(root).unwrap().children[0];
        let n = tree.get(scroll).unwrap();
        (n.measured.h, n.content_h)
    }

    /// 关闭按钮的阶段规则：**只有下载中没有**，其余六个阶段都给，文案按语境三选一。
    ///
    /// 单独锁住这条，是因为"下载中不给关闭"看起来像遗漏——六个按钮合并成一个之后，
    /// 它在 `close_label` 里是一个显式的 `None` 分支，很容易被后来者顺手"补全"。
    /// 直接关掉对话框会让下载在后台继续跑而界面无处可回；取消才是明确的意思表示。
    #[test]
    fn close_button_is_absent_only_while_downloading() {
        assert_eq!(
            close_label(UpdatePhase::Downloading),
            None,
            "下载中只能取消，不给关闭"
        );
        for p in [
            UpdatePhase::Consent,
            UpdatePhase::Checking,
            UpdatePhase::HasUpdate,
            UpdatePhase::NoUpdate,
            UpdatePhase::Ready,
            UpdatePhase::Error,
        ] {
            assert!(close_label(p).is_some(), "{p:?} 阶段应有关闭按钮");
        }
        // 文案按语境走，不是一律「关闭」——同意页说"暂不"、有更新时说"稍后再说"。
        assert_eq!(close_label(UpdatePhase::Consent), Some("暂不"));
        assert_eq!(close_label(UpdatePhase::HasUpdate), Some("稍后再说"));
        assert_eq!(close_label(UpdatePhase::Error), Some("关闭"));
    }

    /// 长发布说明须封顶并留下可滚动量——回归此前 `max_lines(8)` 直接截断、下半段
    /// 既看不到也滚不动的问题。
    #[test]
    fn long_notes_are_capped_and_scrollable() {
        let long = (1..=60)
            .map(|i| format!("- 第 {i} 条更新内容"))
            .collect::<Vec<_>>()
            .join("\n");
        let (h, content_h) = layout_notes(&long);
        assert_eq!(h, NOTES_MAX_H, "超长说明须封顶在上限高度");
        assert!(
            content_h > NOTES_MAX_H,
            "溢出部分须成为可滚动量（content_h={content_h}）"
        );
    }

    /// 短说明不该被撑到上限——多数版本只有两三条，撑高会留一大片空白。
    #[test]
    fn short_notes_shrink_to_content() {
        let (h, content_h) = layout_notes("- 修了一个小问题");
        assert!(h < NOTES_MAX_H, "短说明应按内容收缩，实际 {h}");
        assert_eq!(h, content_h, "无溢出时占位高即内容高");
    }

    /// 布局「已是最新」正文，返回可见的子节点数。
    fn visible_children_of_no_update(notes_text: &str) -> usize {
        let msg = windui::signal::signal("已是最新版本（0.112.0）".to_string());
        let notes = windui::signal::signal(notes_text.to_string());
        let mut tree = Tree::new();
        let root = Element::col()
            .width(DIALOG_W)
            .child(no_update_body(msg, notes))
            .build(&mut tree);
        tree.root = Some(root);
        tree.layout_root(
            Size::new(DIALOG_W, 800),
            &mut windui::text::LineAwareTextEngine,
        );
        let body = tree.get(root).unwrap().children[0];
        tree.get(body)
            .unwrap()
            .children
            .iter()
            .filter(|c| {
                tree.get(**c)
                    .map(|n| n.effective_visible())
                    .unwrap_or(false)
            })
            .count()
    }

    /// 已是最新时也要展示更新记录（结论 + 小标题 + 说明区）。
    #[test]
    fn up_to_date_shows_release_notes() {
        assert_eq!(visible_children_of_no_update("- 新增了一些东西"), 3);
    }

    /// 说明拉取失败或为空时整段隐藏，不留一个空框。
    #[test]
    fn up_to_date_hides_notes_section_when_empty() {
        assert_eq!(visible_children_of_no_update(""), 1, "只剩结论一行");
        assert_eq!(
            visible_children_of_no_update("   \n  "),
            1,
            "空白同样视为无说明"
        );
    }
}
