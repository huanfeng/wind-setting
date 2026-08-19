//! 桌面集成（高级页卡片 + 四个确认对话框）。
//!
//! 管两条关联：`windinput://` 协议（一键导入链接靠它从浏览器唤起本程序）与
//! `.wpkg` 文件关联（双击方案包直接进导入确认）。两者同一套规则与门控，
//! 只是注册表形状不同（见 [`crate::file_assoc`]）。
//!
//! 状态**每次现读注册表**（[`crate::protocol::current_registration`] /
//! [`crate::file_assoc::current_registration`]），不缓存判断结果：
//! 关联可能被别的程序、或另一份清风输入法在我们运行期间改掉，缓存下来只会让界面
//! 显示一个早已不成立的结论。`refresh` 在建卡片时与每次操作后各调一次。

use std::cell::RefCell;
use std::rc::Rc;

use windui::prelude::*;

use crate::pages::dict::reactive::Feedback;
use crate::protocol::Registration;
use crate::state::AppState;
use crate::widgets::{section, setting_row_desc_signal};

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
    /// `.wpkg` 关联状态的展示文案。
    pub assoc_status: Signal<String>,
    /// `.wpkg`「注册」是否可点。
    pub assoc_can_register: Signal<bool>,
    /// `.wpkg`「取消注册」是否可点。
    pub assoc_can_unregister: Signal<bool>,
    /// 覆盖他人 `.wpkg` 关联的确认框。
    pub assoc_overwrite_confirm: Signal<bool>,
    /// 解除他人 `.wpkg` 关联的确认框。
    pub assoc_remove_confirm: Signal<bool>,
    /// 四个确认框共用的正文（含占用者命令行，由触发方填好再开框；同刻只开一个框）。
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
            assoc_status: signal(String::new()),
            assoc_can_register: signal(false),
            assoc_can_unregister: signal(false),
            assoc_overwrite_confirm: signal(false),
            assoc_remove_confirm: signal(false),
            confirm_msg: signal(String::new()),
            feedback: signal(None),
        };
        s.refresh();
        s
    }

    /// 现读注册表并刷新展示信号（协议与 `.wpkg` 各读各的）。
    pub fn refresh(&self) {
        let portable = crate::mode::is_portable();
        let reg = crate::protocol::current_registration();
        self.protocol_status.set(status_text(&reg, portable));
        self.can_register.set(can_register(&reg));
        self.can_unregister.set(can_unregister(&reg));
        let assoc = crate::file_assoc::current_registration();
        self.assoc_status.set(assoc_status_text(&assoc, portable));
        self.assoc_can_register.set(can_register(&assoc));
        self.assoc_can_unregister.set(can_unregister(&assoc));
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

    /// `.wpkg`「注册」按钮：与协议同规则 —— 点击这一刻现读，指向他人先确认。
    pub fn on_assoc_register_clicked(&self, app: &Rc<RefCell<AppState>>) {
        match foreign_command(&crate::file_assoc::current_registration()) {
            Some(cur) => {
                self.confirm_msg.set(assoc_overwrite_msg(cur));
                self.assoc_overwrite_confirm.set(true);
            }
            None => apply(app, Action::RegisterAssoc),
        }
    }

    /// `.wpkg`「取消注册」按钮：解除的若是他人关联，同样先确认。
    pub fn on_assoc_unregister_clicked(&self, app: &Rc<RefCell<AppState>>) {
        match foreign_command(&crate::file_assoc::current_registration()) {
            Some(cur) => {
                self.confirm_msg.set(assoc_remove_msg(cur));
                self.assoc_remove_confirm.set(true);
            }
            None => apply(app, Action::UnregisterAssoc),
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

fn assoc_overwrite_msg(cur: &str) -> String {
    format!(
        ".wpkg 文件当前由以下程序打开：\n{cur}\n\n\
         继续将改为由本程序打开。若上面是另一份清风输入法（例如旧的安装路径），覆盖是安全的。"
    )
}

fn assoc_remove_msg(cur: &str) -> String {
    format!(
        ".wpkg 文件当前由以下程序打开：\n{cur}\n\n\
         这不是本程序写入的关联。继续将解除它，之后双击 .wpkg 文件不再打开对应程序。"
    )
}

/// 一次注册表写操作。抽成枚举是为了让四个确认对话框的回调只差一个值。
#[derive(Clone, Copy)]
pub enum Action {
    Register,
    Unregister,
    RegisterAssoc,
    UnregisterAssoc,
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
        Action::RegisterAssoc => st.register_file_assoc(),
        Action::UnregisterAssoc => st.unregister_file_assoc(),
    }
}

/// 状态文案 —— 它同时是设置行的副标题，所以要独立成句、不依赖别处的补充说明。
///
/// 注册表原样回显，不加"被占用"这类判断：这里最常见的"他人"其实是用户自己的
/// 另一份安装（旧安装路径、移动过的便携包）。
///
/// 便携版的"要手动注册"提示并进**未注册**这一支，而不是常驻单独一行：
/// 已经注册好的便携用户不需要再被告知一遍怎么注册。
fn status_text(reg: &Registration, portable: bool) -> String {
    if !cfg!(windows) {
        return "本平台的协议关联由应用包声明，无需在此管理".to_string();
    }
    match reg {
        Registration::Ours => "已注册，由本程序处理".to_string(),
        Registration::Unregistered if portable => {
            "未注册 —— 便携版启动不会自动注册，需要一键导入请点右侧「注册」".to_string()
        }
        Registration::Unregistered => "未注册 —— 网页上的一键导入链接暂时唤不起本程序".to_string(),
        Registration::Other(cur) => format!("已注册，但指向其它程序：{cur}"),
    }
}

/// `.wpkg` 关联的状态文案。规则同 [`status_text`]：独立成句、注册表原样回显、
/// 便携提示只在未注册时给。
fn assoc_status_text(reg: &Registration, portable: bool) -> String {
    if !cfg!(windows) {
        return "本平台的文件关联由应用包声明，无需在此管理".to_string();
    }
    match reg {
        Registration::Ours => "已注册，双击 .wpkg 方案包会用本程序打开".to_string(),
        Registration::Unregistered if portable => {
            "未注册 —— 便携版启动不会自动注册，需要双击导入请点右侧「注册」".to_string()
        }
        Registration::Unregistered => "未注册 —— 双击 .wpkg 方案包暂时不会唤起本程序".to_string(),
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

/// 高级页「系统集成」卡片。
///
/// 与日志设置/备份还原同一形态（`section` + 设置行），只是副标题走信号版行型 ——
/// 这一行的说明就是**当前关联状态**，随注册表变化。
pub fn build_integration_section(app: &Rc<RefCell<AppState>>, state: &AppState) -> Element {
    let s = state.integration.clone();
    let can_reg = s.can_register;
    let can_unreg = s.can_unregister;

    let app_reg = app.clone();
    let s_reg = s.clone();
    let app_unreg = app.clone();
    let s_unreg = s.clone();

    let buttons = Element::row()
        .cross(Align::Center)
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
        );

    let assoc_can_reg = s.assoc_can_register;
    let assoc_can_unreg = s.assoc_can_unregister;
    let app_areg = app.clone();
    let s_areg = s.clone();
    let app_aunreg = app.clone();
    let s_aunreg = s.clone();
    let assoc_buttons = Element::row()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::button("注册")
                .small()
                .outline_soft()
                .enabled_signal(assoc_can_reg)
                .on_click(move |_| s_areg.on_assoc_register_clicked(&app_areg)),
        )
        .child(
            Element::button("取消注册")
                .small()
                .outline_soft()
                .enabled_signal(assoc_can_unreg)
                .on_click(move |_| s_aunreg.on_assoc_unregister_clicked(&app_aunreg)),
        );

    section(
        "系统集成",
        vec![
            setting_row_desc_signal("windinput:// 协议（一键导入）", s.protocol_status, buttons),
            setting_row_desc_signal(
                ".wpkg 文件关联（双击导入方案包）",
                s.assoc_status,
                assoc_buttons,
            ),
            crate::pages::dict::reactive::toast_sink(s.feedback),
        ],
    )
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
        let text = status_text(&Registration::Other(cmd.to_string()), false);
        assert!(text.contains(cmd), "{text}");
    }

    /// 便携版没有启动自愈兜底，未注册时这行字得把"要自己点注册"说清楚 ——
    /// 它是副标题，用户看不到别处的补充说明。
    #[test]
    fn portable_unregistered_status_tells_user_to_register() {
        if !cfg!(windows) {
            return;
        }
        let portable = status_text(&Registration::Unregistered, true);
        let installed = status_text(&Registration::Unregistered, false);
        assert_ne!(portable, installed);
        assert!(portable.contains("便携版"), "{portable}");
        assert!(portable.contains("注册"), "{portable}");
    }

    /// 已经注册好的便携用户不该再被念一遍"便携版要手动注册"。
    #[test]
    fn portable_hint_only_shows_when_unregistered() {
        if !cfg!(windows) {
            return;
        }
        assert!(!status_text(&Registration::Ours, true).contains("便携版"));
        assert!(
            !status_text(&Registration::Other("x".into()), true).contains("便携版"),
            "指向他人时该说的是占用者，不是便携提示"
        );
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

    /// `.wpkg` 状态文案与协议同规则：三态各自独立成句、他人命令行原样回显、
    /// 便携提示只在未注册时给。
    #[test]
    fn assoc_status_text_covers_three_states() {
        if !cfg!(windows) {
            return;
        }
        let cmd = r#""D:\old\wind_setting.exe" "%1""#;
        assert!(assoc_status_text(&Registration::Other(cmd.to_string()), false).contains(cmd));
        let ours = assoc_status_text(&Registration::Ours, false);
        let none = assoc_status_text(&Registration::Unregistered, false);
        assert_ne!(ours, none);
        assert!(none.contains("未注册"), "{none}");
        let portable = assoc_status_text(&Registration::Unregistered, true);
        assert!(portable.contains("便携版"), "{portable}");
        assert!(
            !assoc_status_text(&Registration::Ours, true).contains("便携版"),
            "已注册的便携用户不该再被念注册提示"
        );
    }

    /// `.wpkg` 的两个确认框正文都要带占用者命令行，且措辞区分「覆盖」与「解除」。
    #[test]
    fn assoc_confirm_messages_carry_command_and_differ() {
        let cmd = r#""D:\other\tool.exe" "%1""#;
        let ow = assoc_overwrite_msg(cmd);
        let rm = assoc_remove_msg(cmd);
        assert!(ow.contains(cmd), "{ow}");
        assert!(rm.contains(cmd), "{rm}");
        assert_ne!(ow, rm);
        assert!(rm.contains("解除"), "解除关联的后果要说明白：{rm}");
    }

    #[test]
    fn ours_and_unregistered_have_distinct_text() {
        if !cfg!(windows) {
            return;
        }
        let ours = status_text(&Registration::Ours, false);
        let none = status_text(&Registration::Unregistered, false);
        assert_ne!(ours, none);
        assert!(none.contains("未注册"), "{none}");
    }
}
