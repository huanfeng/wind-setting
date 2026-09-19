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
    let visible = state.theme_picker_visible;
    let url = state.theme_picker_url;
    let text = state.theme_picker_text;
    let ov_src = state.theme_overwrite_source;
    let ov_msg = state.theme_overwrite_msg;
    let ov_visible = state.theme_overwrite_visible;
    let pkg = ThemePkgConfirmSignals::of(&state);
    drop(state);

    let (s_file, s_url, s_text, s_cancel) = (
        app_state.clone(),
        app_state.clone(),
        app_state.clone(),
        app_state.clone(),
    );

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
                                    let Some(path) = path else { return };
                                    import_picked_file(
                                        &app, &path, ov_src, ov_msg, ov_visible, pkg, visible,
                                    );
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
                .child(Element::button("下载并导入").small().on_click(move |_| {
                    let u = url.get();
                    if let Err(e) = crate::theme_import::validate_https_url(&u) {
                        s_url.borrow().notify_err(e.user_message());
                        return;
                    }
                    // 下载与导入分两步：撞名时要拿**已下载的原文**去弹覆盖确认，
                    // 串成一条链的话 text 就丢了，确认之后只能重下一次。
                    match crate::theme_import::download_https_text(&u) {
                        Ok(t) => finish(
                            &s_url,
                            ThemeSource::Text(t),
                            ov_src,
                            ov_msg,
                            ov_visible,
                            visible,
                        ),
                        Err(e) => s_url.borrow().notify_err(e.user_message()),
                    }
                })),
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
                    finish(
                        &s_text,
                        ThemeSource::Text(t),
                        ov_src,
                        ov_msg,
                        ov_visible,
                        visible,
                    );
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
                        .on_click(move |_| {
                            close(&s_cancel, url, text, visible);
                        }),
                ),
        );

    Element::dialog(visible, frame)
}

/// 选中文件后按扩展名分派。
///
/// `.wtheme` 不在这里落盘，而是转去包确认框——主题包多半是从网上下回来的，装之前得让
/// 用户看清包里是什么（主题名、作者、几个资源文件），这与方案包双击的体验是同一套。
fn import_picked_file(
    app: &Rc<RefCell<AppState>>,
    path: &std::path::Path,
    ov_src: Signal<Option<ThemeSource>>,
    ov_msg: Signal<String>,
    ov_visible: Signal<bool>,
    pkg: ThemePkgConfirmSignals,
    visible: Signal<bool>,
) {
    if is_theme_package(path) {
        match crate::theme_import::preview_package(&path.to_string_lossy()) {
            Ok(p) => {
                pkg.msg.set(p.confirm_text());
                pkg.path.set(path.to_string_lossy().into_owned());
                // 先收本框再弹确认框：两个对话框同时可见会互相遮挡。
                visible.set(false);
                pkg.visible.set(true);
            }
            Err(e) => app.borrow().notify_err(e.user_message()),
        }
        return;
    }

    match crate::theme_import::read_local_toml(path) {
        Ok(t) => finish(
            app,
            ThemeSource::Text(t),
            ov_src,
            ov_msg,
            ov_visible,
            visible,
        ),
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
fn finish(
    app: &Rc<RefCell<AppState>>,
    source: ThemeSource,
    ov_src: Signal<Option<ThemeSource>>,
    ov_msg: Signal<String>,
    ov_visible: Signal<bool>,
    visible: Signal<bool>,
) {
    // 撞名不是「导入不了」而是「要不要更新」，那条判断几个入口共用一处
    // （见 `AppState::try_import_theme`）。
    match AppState::try_import_theme(source, ov_src, ov_msg, ov_visible) {
        // 已转覆盖确认：先收本框再让它弹，两个框同时可见会互相遮挡。
        None => visible.set(false),
        Some(Ok(r)) => {
            let mut st = app.borrow_mut();
            st.refresh_theme_names();
            st.notify_ok(format!("已导入主题：{}", r.display_name));
            visible.set(false);
        }
        // 失败保留对话框：用户可以改链接、换文件或改那段文本再来一次。
        Some(Err(e)) => app.borrow().notify_err(e.user_message()),
    }
}

/// 关框并清空输入——留着上次的链接与整段文本，下次打开会让人以为那是待导入的内容。
fn close(
    _app: &Rc<RefCell<AppState>>,
    url: Signal<String>,
    text: Signal<String>,
    visible: Signal<bool>,
) {
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
