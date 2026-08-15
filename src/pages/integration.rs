//! 桌面集成（高级页卡片 + 两个确认对话框）。
//!
//! 眼下只管 `windinput://` 协议关联 —— 一键导入链接（主题，后续还有方案/词库）
//! 靠它从浏览器唤起本程序。协议没注册，市场页上的「一键导入」按钮就是个死链。
//!
//! 状态**每次现读注册表**（[`crate::protocol::current_registration`]），不缓存判断结果：
//! 关联可能被别的程序、或另一份清风输入法在我们运行期间改掉，缓存下来只会让界面
//! 显示一个早已不成立的结论。`refresh` 在建卡片时与每次操作后各调一次。

use std::cell::RefCell;
use std::rc::Rc;

use windui::prelude::*;

use crate::pages::dict::reactive::Feedback;
use crate::protocol::Registration;
use crate::state::AppState;

/// 桌面集成状态（Signal 为 Copy，可直接 Clone 进闭包）。
///
/// 只存**展示用**的派生值：注册表本身是唯一真相，这些信号是它在某一刻的投影。
#[derive(Clone)]
pub struct IntegrationState {
    /// 协议注册状态的展示文案。
    pub protocol_status: Signal<String>,
    /// 「注册」是否可点（已经指向本程序时无事可做）。
    pub can_register: Signal<bool>,
    /// 「取消注册」是否可点（本就没注册时无事可做）。
    pub can_unregister: Signal<bool>,
    /// 覆盖他人注册的确认框。
    pub overwrite_confirm: Signal<bool>,
    /// 清除他人注册的确认框。
    pub remove_confirm: Signal<bool>,
    /// 两个确认框共用的正文（含占用者命令行，由触发方填好再开框）。
    pub confirm_msg: Signal<String>,
    pub feedback: Feedback,
}

impl Default for IntegrationState {
    fn default() -> Self {
        Self::new()
    }
}

impl IntegrationState {
    pub fn new() -> Self {
        let s = Self {
            protocol_status: signal(String::new()),
            can_register: signal(false),
            can_unregister: signal(false),
            overwrite_confirm: signal(false),
            remove_confirm: signal(false),
            confirm_msg: signal(String::new()),
            feedback: signal(None),
        };
        s.refresh();
        s
    }

    /// 现读注册表并刷新展示信号。
    pub fn refresh(&self) {
        let reg = crate::protocol::current_registration();
        self.protocol_status.set(status_text(&reg));
        self.can_register.set(can_register(&reg));
        self.can_unregister.set(can_unregister(&reg));
    }

    /// 「注册」按钮：指向他人时先征得同意，其余情况直接执行。
    ///
    /// 状态是**点击这一刻现读**的，不用卡片上那份 —— 卡片可能已经开了几分钟，
    /// 期间关联被别的程序改掉了，照着旧状态直接覆盖就成了无声的抢占。
    pub fn on_register_clicked(&self, app: &Rc<RefCell<AppState>>) {
        match foreign_command(&crate::protocol::current_registration()) {
            Some(cur) => {
                self.confirm_msg.set(overwrite_msg(cur));
                self.overwrite_confirm.set(true);
            }
            None => apply(app, Action::Register),
        }
    }

    /// 「取消注册」按钮：删的若是别人写的关联，同样要先确认 ——
    /// 这一步会动到不属于本程序的注册表项，静默执行不合适。
    pub fn on_unregister_clicked(&self, app: &Rc<RefCell<AppState>>) {
        match foreign_command(&crate::protocol::current_registration()) {
            Some(cur) => {
                self.confirm_msg.set(remove_msg(cur));
                self.remove_confirm.set(true);
            }
            None => apply(app, Action::Unregister),
        }
    }
}

/// 需要确认时返回占用者的命令行，否则 `None`。
///
/// 判定与取值合并在一个出口：只有「指向别的程序」才需要确认（注册一个空关联、
/// 或解除自己写的关联，都是随时可撤销的无风险操作，加弹窗只会烦人），
/// 而确认框的正文又必须带上这条命令行 —— 分成两步写迟早会漂移成
/// 「判了要确认，却按另一个分支取正文」。
fn foreign_command(reg: &Registration) -> Option<&str> {
    match reg {
        Registration::Other(cur) => Some(cur),
        _ => None,
    }
}

fn overwrite_msg(cur: &str) -> String {
    format!(
        "windinput:// 协议当前指向：\n{cur}\n\n\
         继续将改为由本程序处理。若上面是另一份清风输入法（例如旧的安装路径），覆盖是安全的。"
    )
}

fn remove_msg(cur: &str) -> String {
    format!(
        "windinput:// 协议当前指向：\n{cur}\n\n\
         这不是本程序写入的关联。继续将直接删除它，对应程序的一键导入功能也会随之失效。"
    )
}

/// 一次注册表写操作。抽成枚举是为了让两个确认对话框的回调只差一个值。
#[derive(Clone, Copy)]
pub enum Action {
    Register,
    Unregister,
}

/// 执行操作 + 持久化用户意愿 + 刷新展示。
///
/// 持久化必须经 `AppState`：`setting_config` 在内存里有一份副本，别处（如升级偏好）
/// 保存时整份写盘 —— 这里若自行 load/save，那次写盘会把本次改动覆盖回去。
fn apply(app: &Rc<RefCell<AppState>>, action: Action) {
    let Ok(mut st) = app.try_borrow_mut() else {
        return;
    };
    match action {
        Action::Register => st.register_protocol(),
        Action::Unregister => st.unregister_protocol(),
    }
}

/// 状态文案。注册表原样回显，不加"被占用"这类判断 ——
/// 这里最常见的"他人"其实是用户自己的另一份安装。
fn status_text(reg: &Registration) -> String {
    if !cfg!(windows) {
        return "本平台的协议关联由应用包声明，无需在此管理".to_string();
    }
    match reg {
        Registration::Ours => "已注册，由本程序处理".to_string(),
        Registration::Unregistered => "未注册 —— 网页上的一键导入链接暂时唤不起本程序".to_string(),
        Registration::Other(cur) => format!("已注册，但指向其它程序：{cur}"),
    }
}

/// 已经指向本程序时没有可做的事，禁用按钮免得用户以为点了没反应。
fn can_register(reg: &Registration) -> bool {
    cfg!(windows) && !matches!(reg, Registration::Ours)
}

fn can_unregister(reg: &Registration) -> bool {
    cfg!(windows) && !matches!(reg, Registration::Unregistered)
}

/// 高级页「桌面集成」卡片。
///
/// 不用 `section` + `setting_row`：状态文案是响应式的（`label_signal`），
/// 而 `setting_row` 的副标题只收 `&str`，套不进去。
pub fn build_integration_section(app: &Rc<RefCell<AppState>>, state: &AppState) -> Element {
    let s = state.integration.clone();
    let status = s.protocol_status;
    let can_reg = s.can_register;
    let can_unreg = s.can_unregister;

    let app_reg = app.clone();
    let s_reg = s.clone();
    let app_unreg = app.clone();
    let s_unreg = s.clone();

    Element::col()
        .width_match()
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .padding_xy(16, 14)
        .spacing(8)
        .child(Element::label("桌面集成").font_size(14.0).font_weight(600))
        .child(
            Element::label(
                "网页上的一键导入（主题等）通过 windinput:// 链接唤起本程序处理。\
                 关联写在当前用户下，不需要管理员权限。",
            )
            .font_size(12.0)
            .fg_role(Role::TextMuted),
        )
        // 便携版没有启动自愈这层兜底，用户得知道关联要自己挂上。
        .child(
            Element::label("便携版启动时不会自动注册，需要一键导入时请在此手动注册。")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .visible_when(crate::mode::is_portable),
        )
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(
                    Element::label("状态：")
                        .font_size(12.0)
                        .fg_role(Role::TextMuted),
                )
                .child(
                    Element::label_signal(status)
                        .font_size(12.0)
                        .fg_role(Role::TextMuted)
                        .weight(1.0)
                        // 他人注册时要连命令行一起显示，一行放不下。
                        .max_lines(3),
                ),
        )
        .child(
            Element::row()
                .spacing(8)
                .child(
                    Element::button("注册")
                        .small()
                        .outline_soft()
                        .enabled_signal(can_reg)
                        .on_click(move |_| s_reg.on_register_clicked(&app_reg)),
                )
                .child(
                    Element::button("取消注册")
                        .small()
                        .outline_soft()
                        .enabled_signal(can_unreg)
                        .on_click(move |_| s_unreg.on_unregister_clicked(&app_unreg)),
                ),
        )
        .child(crate::pages::dict::reactive::toast_sink(s.feedback))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 已指向本程序时「注册」无事可做；未注册时「取消注册」无事可做。
    /// 两个按钮的可用性必须互补覆盖三种状态，否则会出现"两个都灰着"的死局。
    #[test]
    fn button_availability_covers_every_state() {
        let cases = [
            Registration::Unregistered,
            Registration::Ours,
            Registration::Other("x".into()),
        ];
        for reg in cases {
            let (r, u) = (can_register(&reg), can_unregister(&reg));
            if cfg!(windows) {
                assert!(r || u, "{reg:?} 下两个按钮不能同时不可用");
            } else {
                assert!(!r && !u, "非 Windows 平台不提供注册表操作");
            }
        }
        if cfg!(windows) {
            assert!(!can_register(&Registration::Ours));
            assert!(!can_unregister(&Registration::Unregistered));
            assert!(can_register(&Registration::Other("x".into())));
            assert!(can_unregister(&Registration::Other("x".into())));
        }
    }

    /// 他人注册时，文案必须**原样带出命令行** —— 用户就是靠这行字分辨
    /// "是别的软件抢了"还是"我自己另一份安装"，含糊其辞等于没给信息。
    #[test]
    fn other_status_shows_the_actual_command() {
        if !cfg!(windows) {
            return;
        }
        let cmd = r#""D:\old\wind_setting.exe" "%1""#;
        let text = status_text(&Registration::Other(cmd.to_string()));
        assert!(text.contains(cmd), "{text}");
    }

    /// 确认只加在"动了别人的东西"这一种情况上，且拿到的正是那条命令行。
    #[test]
    fn confirm_required_only_for_foreign_registration() {
        let cmd = r#""D:\other\tool.exe" "%1""#;
        assert_eq!(
            foreign_command(&Registration::Other(cmd.into())),
            Some(cmd),
            "确认框要靠这条命令行填正文"
        );
        assert_eq!(foreign_command(&Registration::Ours), None);
        assert_eq!(foreign_command(&Registration::Unregistered), None);
    }

    /// 两个确认框的正文都必须把占用者命令行摆出来 —— 用户凭它决定要不要继续；
    /// 且两者措辞必须不同：一个是"改为本程序"，一个是"直接删掉"，后果完全不同。
    #[test]
    fn confirm_messages_carry_command_and_differ() {
        let cmd = r#""D:\other\tool.exe" "%1""#;
        let ow = overwrite_msg(cmd);
        let rm = remove_msg(cmd);
        assert!(ow.contains(cmd), "{ow}");
        assert!(rm.contains(cmd), "{rm}");
        assert_ne!(ow, rm);
        assert!(rm.contains("删除"), "解除关联的后果要说明白：{rm}");
    }

    #[test]
    fn ours_and_unregistered_have_distinct_text() {
        if !cfg!(windows) {
            return;
        }
        let ours = status_text(&Registration::Ours);
        let none = status_text(&Registration::Unregistered);
        assert_ne!(ours, none);
        assert!(none.contains("未注册"), "{none}");
    }
}
