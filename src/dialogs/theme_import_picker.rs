//! 「导入主题」对话框：用户**主动**发起的导入入口，三条路收在一处。
//!
//! 与 [`crate::dialogs::theme_import_dialog`] 是两回事，别合并：那个由
//! `windinput://import/theme` 从浏览器唤起，问的是「这个来源可信吗」，主语是外部请求；
//! 这个是用户自己在外观页点开的，问的是「你要从哪导」，来源本就是他自己选的。
//!
//! 收进来的三条路此前散在三处：本地 `.toml` 在外观页的按钮上，`.wtheme` 只能双击文件
//! （要先注册关联）或翻到「高级 → 导入」，链接则只有协议唤起那一条。**不设文件关联的
//! 用户因此没有任何在设置里装主题包的入口**——这个对话框就是为补上它而来。

use std::cell::RefCell;
use std::rc::Rc;

use windui::prelude::*;

use crate::pages::config_import::ThemePkgConfirmSignals;
use crate::state::AppState;
use crate::theme_import::ThemeSource;

/// 这个对话框要用的一束信号。
///
/// 打成一束而不是散着传：三条导入路径都要收框、清输入、转覆盖确认、转包确认，散着传的话
/// 每个函数签名都是七八个同类型参数，下一个读它的人得先数清楚哪个是哪个（取舍同
/// [`ThemePkgConfirmSignals`]，那里也是这么收的）。
#[derive(Clone, Copy)]
struct PickerSignals {
    visible: Signal<bool>,
    url: Signal<String>,
    text: Signal<String>,
    /// 撞名覆盖确认：待导入的来源、正文、可见性。
    ov_src: Signal<Option<ThemeSource>>,
    ov_msg: Signal<String>,
    ov_visible: Signal<bool>,
    /// 主题包「要装这个吗」确认框。
    pkg: ThemePkgConfirmSignals,
}

/// 小节标题：三条路各起一段，免得挤成一团分不清哪个按钮配哪个输入。
fn head(text: &str) -> Element {
    Element::label(text)
        .font_size(12.0)
        .font_weight(600)
        .fg_role(Role::Text)
        .width_match()
        .padding_xy(18, 6)
}

fn hint(text: &str) -> Element {
    Element::label(text)
        .font_size(11.5)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(2)
        .padding_xy(18, 2)
}

pub fn build_theme_import_picker(app_state: &Rc<RefCell<AppState>>) -> Element {
    let state = app_state.borrow();
    let sig = PickerSignals {
        visible: state.theme_picker_visible,
        url: state.theme_picker_url,
        text: state.theme_picker_text,
        ov_src: state.theme_overwrite_source,
        ov_msg: state.theme_overwrite_msg,
        ov_visible: state.theme_overwrite_visible,
        pkg: ThemePkgConfirmSignals::of(&state),
    };
    let (visible, url, text) = (sig.visible, sig.url, sig.text);
    let download_idle = state.theme_download_idle;
    drop(state);
    let s_cancel = app_state.clone();

    let (s_file, s_url, s_text) = (app_state.clone(), app_state.clone(), app_state.clone());

    let frame = Element::col()
        .width(560)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("导入主题")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        // ── 1. 本地文件 ──────────────────────────────────────────
        .child(head("从文件"))
        .child(hint(
            "支持 .toml 主题文件与 .wtheme 主题包；主题包带背景图等资源，装它不必先注册文件关联",
        ))
        .child(
            Element::row()
                .width_match()
                .padding_xy(18, 8)
                .child(
                    Element::button("选择文件…")
                        .small()
                        .outline_soft()
                        .on_click(move |ctx| {
                            let app = s_file.clone();
                            ctx.request_pick_file(
                                PickDialog::new()
                                    .title("导入主题")
                                    // 两种扩展名同框可选：分成两个按钮的话，用户得先知道
                                    // 自己手上那个文件算哪一类，而 .wtheme 正是他没见过的
                                    // 那一个。
                                    .filter("主题文件", &["toml", "wtheme"]),
                                move |path| {
                                    let Some(path) = path else {
                                        return crate::file_pick::report_failure(|m| {
                                            app.borrow().notify_err(m)
                                        });
                                    };
                                    import_picked_file(&app, &path, sig);
                                },
                            );
                        }),
                )
                .child(Element::leaf().weight(1.0)),
        )
        .child(Element::divider())
        // ── 2. HTTPS 链接 ────────────────────────────────────────
        .child(head("从链接"))
        .child(hint("必须是 https:// 开头；下载后交给 core 校验再落盘"))
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .padding_xy(18, 8)
                .child(
                    Element::text_input(url, "https://example.com/theme.toml")
                        .width_match()
                        .weight(1.0),
                )
                .child(
                    Element::button("下载并导入")
                        .small()
                        // 下载中置灰：慢网下用户以为没点着、反复点，会排出一串重复导入。
                        .enabled_signal(download_idle)
                        .on_click(move |_| {
                            let u = url.get();
                            if let Err(e) = crate::theme_import::validate_https_url(&u) {
                                s_url.borrow().notify_err(e.user_message());
                                return;
                            }
                            // 下载在后台线程跑，点击立即返回。下载与导入分两步：撞名时要拿
                            // **已下载的原文**去弹覆盖确认，串成一条链的话 text 就丢了，
                            // 确认之后只能重下一次。
                            AppState::start_theme_download(
                                &s_url,
                                crate::state::ThemeDlOwner::Picker,
                                u,
                                visible,
                                move |app, t| finish(app, ThemeSource::Text(t), sig),
                            );
                        }),
                ),
        )
        .child(Element::divider())
        // ── 3. 粘贴原文 ──────────────────────────────────────────
        .child(head("粘贴主题原文"))
        .child(hint("从聊天记录或论坛帖里拷来的 TOML 文本，直接贴在这里"))
        .child(
            Element::col().width_match().padding_xy(18, 8).child(
                Element::text_input(text, "[meta]\nname = \"我的主题\"\n…")
                    .multiline()
                    .width_match()
                    .height(120),
            ),
        )
        .child(
            Element::row()
                .width_match()
                .padding_xy(18, 4)
                .child(Element::leaf().weight(1.0))
                .child(Element::button("导入这段文本").small().on_click(move |_| {
                    let t = text.get();
                    if t.trim().is_empty() {
                        s_text.borrow().notify_err("请先粘贴主题原文");
                        return;
                    }
                    // 与下载那条同一条闸（`download_https_text` 有 MAX_THEME_BYTES）：
                    // 三条路汇到同一个 finish，只有一条有输入上限的话，粘错东西（比如
                    // 整个词库文件）要等 core 才报，而它说的是 TOML 解析失败，指不到
                    // 「你粘的东西太大了」。
                    if t.len() > crate::theme_import::MAX_THEME_BYTES {
                        s_text
                            .borrow()
                            .notify_err("主题内容过大（上限 2 MiB），请确认粘对了东西");
                        return;
                    }
                    finish(&s_text, ThemeSource::Text(t), sig);
                })),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("关闭")
                        .small()
                        .outline_soft()
                        .on_click(move |_| close(url, text, visible)),
                ),
        )
        // 框一收（关闭按钮、导入成功、转确认框、ESC 都算）就取消在途下载。挂在可见性信号
        // 上而不是关闭按钮上，理由同 `close` 的注释：能让这个框消失的路不止一条。
        .child(crate::pages::dict::reactive::reactive_effect(
            move || visible.version(),
            move || {
                if !visible.get() {
                    s_cancel
                        .borrow()
                        .cancel_theme_download(crate::state::ThemeDlOwner::Picker);
                }
            },
        ));

    Element::dialog(visible, frame)
}

/// 选中文件后按扩展名分派。
///
/// `.wtheme` 不在这里落盘，而是转去包确认框——主题包多半是从网上下回来的，装之前得让
/// 用户看清包里是什么（主题名、作者、几个资源文件），这与方案包双击的体验是同一套。
fn import_picked_file(app: &Rc<RefCell<AppState>>, path: &std::path::Path, sig: PickerSignals) {
    if is_theme_package(path) {
        match crate::theme_import::preview_package(&path.to_string_lossy()) {
            Ok(p) => {
                sig.pkg.msg.set(p.confirm_text());
                sig.pkg.path.set(path.to_string_lossy().into_owned());
                // 先收本框再弹确认框。**这是单向遮挡**：包确认框在根 stack 里排在
                // 本框之前（见 `shell::build` 的 `.child()` 次序），两者同时可见时它会落在
                // 本框的遮罩底下——看不见也点不到，而遮罩吞掉全部指针事件，现象是「设置窗
                // 不响应了」。调整那处 stack 次序的人需要知道这条依赖。
                close(sig.url, sig.text, sig.visible);
                sig.pkg.visible.set(true);
            }
            Err(e) => app.borrow().notify_err(e.user_message()),
        }
        return;
    }

    match crate::theme_import::read_local_toml(path) {
        Ok(t) => finish(app, ThemeSource::Text(t), sig),
        Err(e) => app.borrow().notify_err(e.user_message()),
    }
}

/// 选中的是主题**包**还是单文件主题。
///
/// 抽成纯函数是为了测得到：埋在对话框回调里的判断一行都跑不到测试，而这条判断错了的
/// 现象是「选了 .wtheme 却走文本导入」——core 会报一句 TOML 解析失败，指不到真正的原因。
///
/// 大小写不敏感：资源管理器里改过名的文件常常是 `.WTHEME`。
fn is_theme_package(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("wtheme"))
}

/// 三条路共用的收尾：撞名转覆盖确认、成功刷列表报 toast、失败留框让用户改。
fn finish(app: &Rc<RefCell<AppState>>, source: ThemeSource, sig: PickerSignals) {
    // 撞名不是「导入不了」而是「要不要更新」，那条判断几个入口共用一处
    // （见 `AppState::try_import_theme`）。
    match AppState::try_import_theme(source, sig.ov_src, sig.ov_msg, sig.ov_visible) {
        // 已转覆盖确认：先收本框再让它弹（遮挡关系同 `import_picked_file` 那段注释）。
        None => close(sig.url, sig.text, sig.visible),
        Some(Ok(r)) => {
            let mut st = app.borrow_mut();
            st.refresh_theme_names();
            st.notify_ok(format!("已导入主题：{}", r.display_name));
            drop(st);
            close(sig.url, sig.text, sig.visible);
        }
        // 失败保留对话框：用户可以改链接、换文件或改那段文本再来一次。
        Some(Err(e)) => app.borrow().notify_err(e.user_message()),
    }
}

/// 关框并清空输入。
///
/// **收框的每一条路都要走这里**，而不是只挂在「关闭」按钮上：能让这个框消失的路有五条
/// （关闭按钮、导入成功、撞名转覆盖确认、选到包转包确认、以及按 **ESC**——windui 把遮罩
/// 登记进 `Tree::modals`，ESC 直接置 `show=false`）。清空只绑住其中一条的话，用户粘一大段
/// 原文、改主意按 ESC，下次打开那段还原样躺在框里，看上去像是「待导入的内容」。
fn close(url: Signal<String>, text: Signal<String>, visible: Signal<bool>) {
    url.set(String::new());
    text.set(String::new());
    visible.set(false);
}

#[cfg(test)]
mod tests {
    use super::is_theme_package;
    use std::path::Path;

    #[test]
    fn theme_package_is_told_apart_from_a_plain_toml_theme() {
        assert!(is_theme_package(Path::new(r"C:\down\水墨.wtheme")));
        // 资源管理器里改过名的常常是大写扩展名。
        assert!(is_theme_package(Path::new("/tmp/ink.WTheme")));

        assert!(!is_theme_package(Path::new(r"C:\down\我的主题.toml")));
        assert!(
            !is_theme_package(Path::new("/tmp/theme")),
            "没有扩展名不算包"
        );
        // 名字里带 wtheme 但扩展名不是它 —— 判据看的是扩展名而非文件名。
        assert!(!is_theme_package(Path::new("/tmp/wtheme.toml")));
    }
}
