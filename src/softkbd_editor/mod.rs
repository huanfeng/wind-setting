//! 「输入 → 软键盘」：图形化编辑软键盘的键盘面（core 仓 `docs/design/soft-keyboard.md` §2.2.1）。
//!
//! 卡片上一行入口，点开大对话框编辑。改动是**本地工作副本 + 操作队列**，登记成
//! `SideCommitter`，随底栏「应用设置」整批交给 core 的 `softkeyboard.apply`——不另设保存按钮
//! （设置端已定：非 config.toml 落点一律走 SideCommitter）。core 只往用户层写稀疏补丁，
//! 写完热重载，不必重启服务。

pub mod mock;
pub mod model;
mod view;

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::json;
use windui::prelude::*;

use crate::rpc;
use crate::state::{AppState, SideCommitter};
use model::{Model, parse_view};

/// 搜索锚点（`search::index::HANDWRITTEN` 的条目指向这里）。
pub const ANCHOR: &str = "input:softkeyboard";
/// 分区名（二级导航 + 卡片标题）。
pub const SECTION: &str = "软键盘";
/// 定制版 `feature:` id（core `wind_config::SETTINGS_FEATURES`）。
pub const FEATURE: &str = "softkeyboard";

/// 编辑器数据：活在 `AppState` 里，比设置树长命——主窗重建（换肤、精简清单）后，
/// 还没「应用设置」的编辑仍在。
#[derive(Clone, Default)]
pub struct SoftKbdData {
    pub model: Rc<RefCell<Model>>,
    /// 读取 core 失败的原因；空 = 正常。
    pub load_error: Rc<RefCell<String>>,
}

impl SoftKbdData {
    pub fn is_dirty(&self) -> bool {
        self.model.borrow().is_dirty()
    }

    /// 从 core 现拉视图，替换工作副本。**丢弃未提交的操作**：打开对话框时只在没有改动时调，
    /// 「重新加载」则正是要丢弃。
    pub fn reload(&self) {
        match rpc::call_core("softkeyboard.get", &json!({})).and_then(|v| parse_view(&v)) {
            Ok(pages) => {
                *self.model.borrow_mut() = Model::from_view(pages);
                self.load_error.borrow_mut().clear();
            }
            Err(e) => {
                tracing::warn!("软键盘编辑器: 读取失败: {e}");
                *self.load_error.borrow_mut() = e;
            }
        }
    }

    /// 整批提交。成功后用 core 回的新视图替换工作副本。
    pub fn commit(&self) -> Result<(), String> {
        let ops = self.model.borrow().pending.clone();
        if ops.is_empty() {
            return Ok(());
        }
        let reply = rpc::call_core("softkeyboard.apply", &json!({ "ops": ops }))
            .map_err(|e| format!("软键盘保存失败：{e}"))?;
        let pages = parse_view(&reply)?;
        *self.model.borrow_mut() = Model::from_view(pages);
        Ok(())
    }
}

/// 「输入」页的软键盘卡片。顺带登记对话框与 `SideCommitter`（设置树每次重建都会走到这里，
/// 而 `FieldRegistry::reset` 会把上一代的登记清掉，故不会重复）。
pub fn build_section(state: &AppState) -> Element {
    let data = state.softkbd.clone();
    let ui = view::EditorUi::new(data.clone());
    crate::manifest::register_dialog(state, view::build_dialog(&ui));

    let (d_dirty, d_commit) = (data.clone(), data);
    state.fields.side.borrow_mut().push(SideCommitter {
        label: "软键盘",
        is_dirty: Box::new(move || d_dirty.is_dirty()),
        commit: Box::new(move || d_commit.commit()),
    });

    crate::widgets::section(
        SECTION,
        vec![crate::widgets::setting_row(
            "自定义软键盘",
            Some(
                "修改各键盘面上每个键输出的符号与 Shift 符号，或新建自己的键盘面。\
                 软键盘可从工具栏、功能菜单或快捷键打开。",
            ),
            Element::button("编辑…")
                .small()
                .on_click(move |_| ui.open()),
        )],
    )
}
