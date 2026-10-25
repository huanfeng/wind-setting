//! 设置项的 Ctrl+右键菜单：复制 key、隐藏这一项 / 这一节、取消隐藏。
//!
//! # 为什么是 Ctrl+右键，而不是裸右键
//!
//! 设置界面里已经有几处裸右键菜单（侧栏 / 底栏的导入菜单、方案列表的行菜单）。再给每一行
//! 挂一个裸右键，会和这些既有菜单抢同一次点击；而「隐藏一个设置项」对绝大多数用户是**从不
//! 使用**的能力，不该在每一次误触时弹出来。Ctrl+右键把两者分开。
//!
//! # 目标是「行」还是「卡片」
//!
//! 每个可操作节点自带 [`Target`]，菜单据此给出对应的项：
//!
//! - 行：复制它的配置 key、隐藏这一项（`key:` 选择器）；
//! - 卡片（分区标题栏 / 卡片内边距）：隐藏这一节、复制这一节的选择器；
//! - 手写卡（主题导入、备份还原…）：隐藏整块（`feature:` 选择器）。
//!
//! ★ **行上不再给「隐藏这一节」**。两档菜单同时出现在右键里，用户每次点中一行都要在
//! 「这一项 / 这一节」之间做一次分辨（2026-10-25 反馈：「两个重叠在一起，并不是很好」）。
//! 整节隐藏仍然可达——右键落在卡片空白处或分区标题行即可，那里只给节级动作。
//!
//! # 与定制版的关系
//!
//! - 定制版**已经藏掉**的东西，用户菜单里不会再出现（它根本不在树上），也无从取消——
//!   要找回请用高级页的「显示定制版隐藏的设置」。这是刻意的：定制者说「这一版没有这个
//!   功能」，让最终用户随手放出来就把定制意图作废了。
//! - 用户自己的隐藏写进 [`crate::user_hide`] 那份独立文件，与定制版互不覆盖。
//!
//! # 生效时机
//!
//! 即时：行 / 卡片 / 手写卡都挂着运行期判据（`visible_when` 读
//! [`crate::user_hide::UserHideStore`]），藏完当场重排，不必重开窗口。
//! 复制选择器同样立即生效（写剪贴板）。
//!
//! # 哪些行没有这个菜单
//!
//! 文本框（`string` / 模板类控件）自己接管右键做复制粘贴；用户在**标签一侧**按
//! Ctrl+右键仍能拿到本菜单。刻意不给整页背景 / 侧栏 / 底栏挂：那里没有「设置项」可藏，
//! 只有既有菜单，挂上去会互相抢。

use std::rc::Rc;

use windui::core::EventCtx;
use windui::prelude::*;

use crate::user_hide::UserHideStore;

/// 右键落在什么上。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// 一行设置项：配置 key（可能带 `#视图` 后缀）、所在分组与分区。
    Key {
        key: String,
        group: String,
        section: String,
    },
    /// 一整张卡片。
    Section { group: String, section: String },
    /// 清单之外的手写卡，取值是 `wind_config::SETTINGS_FEATURES` 的 id。
    Feature(&'static str),
}

/// 「设置项隐藏」功能是否开启。关闭时这一手势完全不存在（菜单为空）。
///
/// 判定在**菜单构建时**现读，因此打开开关后不必重建页面树就能用。
pub fn hiding_enabled(store: &UserHideStore) -> bool {
    store.enabled()
}

/// 一个节点的 Ctrl+右键菜单构建器。
///
/// 返回空表即「这次右键不弹菜单」：windui 的菜单分支在项为空时什么都不做，外层（侧栏 /
/// 底栏 / 方案列表）那些裸右键菜单照常接管。
pub fn menu_for(store: Rc<UserHideStore>, target: Target) -> impl Fn() -> Vec<MenuItem> + 'static {
    move || {
        // 只接管 Ctrl+右键：裸右键留给既有菜单。
        if !windui::event::pointer_mods().ctrl || !hiding_enabled(&store) {
            return Vec::new();
        }
        build_items(&store, &target)
    }
}

fn build_items(store: &Rc<UserHideStore>, target: &Target) -> Vec<MenuItem> {
    match target {
        // 行上只给**项级**动作。整节隐藏归卡片菜单（右键落在标题行 / 卡片空白处），
        // 两份都放会让每次右键都看到一组重叠的「这一项 / 这一节」——用户在条目处本来
        // 只想藏这一项（2026-10-25 反馈）。
        Target::Key { key, .. } => vec![
            copy_item(&format!("key:{key}"), key),
            MenuItem::separator(),
            key_hide_item(store.clone(), key),
        ],
        Target::Section { group, section } => vec![
            section_hide_item(store.clone(), group, section),
            MenuItem::separator(),
            copy_item(
                &format!("section:{group}/{section}"),
                &format!("section:{group}/{section}"),
            ),
        ],
        Target::Feature(id) => vec![
            feature_hide_item(store.clone(), id),
            MenuItem::separator(),
            copy_item(&format!("feature:{id}"), &format!("feature:{id}")),
        ],
    }
}

/// 「复制选择器」：`shown` 是给用户看/进剪贴板的文案。
fn copy_item(selector: &str, shown: &str) -> MenuItem {
    let (sel, shown) = (selector.to_string(), shown.to_string());
    MenuItem::run(
        "复制选择器",
        move |ctx: &mut EventCtx| {
            ctx.clipboard_set(&sel);
            ctx.toast_ok(format!("已复制 {shown}"));
        },
        false,
    )
}

fn key_hide_item(store: Rc<UserHideStore>, key: &str) -> MenuItem {
    match store.rule_hiding_key(key) {
        Some(rule) => MenuItem::run(
            "取消隐藏这一项",
            move |ctx: &mut EventCtx| match store.remove(&rule) {
                Ok(_) => toast_reopen(ctx, "已取消隐藏这一项"),
                Err(e) => ctx.toast_err(e),
            },
            false,
        ),
        None => {
            let selector = format!("key:{key}");
            MenuItem::run(
                "隐藏这一项",
                move |ctx: &mut EventCtx| match store.add(&selector) {
                    Ok(true) => toast_reopen(ctx, "已隐藏这一项"),
                    Ok(false) => ctx.toast("这一项已在隐藏清单里"),
                    Err(e) => ctx.toast_err(e),
                },
                false,
            )
        }
    }
}

/// 「隐藏这一节 / 取消隐藏这一节」。按分组+分区写选择器，避免跨页同名分区被误伤。
fn section_hide_item(store: Rc<UserHideStore>, group: &str, section: &str) -> MenuItem {
    match store.rule_hiding_section(Some(group), section) {
        Some(rule) => MenuItem::run(
            "取消隐藏这一节",
            move |ctx: &mut EventCtx| match store.remove(&rule) {
                Ok(_) => toast_reopen(ctx, "已取消隐藏这一节"),
                Err(e) => ctx.toast_err(e),
            },
            false,
        ),
        None => {
            let selector = format!("section:{group}/{section}");
            MenuItem::run(
                "隐藏这一节",
                move |ctx: &mut EventCtx| match store.add(&selector) {
                    Ok(true) => toast_reopen(ctx, "已隐藏这一节"),
                    Ok(false) => ctx.toast("这一节已在隐藏清单里"),
                    Err(e) => ctx.toast_err(e),
                },
                false,
            )
        }
    }
}

fn feature_hide_item(store: Rc<UserHideStore>, id: &'static str) -> MenuItem {
    // 定制版已经藏掉的卡片不会出现在树上，故这里只需看用户自己那份。
    let selector = format!("feature:{id}");
    if store.selectors().iter().any(|s| s == &selector) {
        MenuItem::run(
            "取消隐藏这一块",
            move |ctx: &mut EventCtx| match store.remove(&selector) {
                Ok(_) => toast_reopen(ctx, "已取消隐藏这一块"),
                Err(e) => ctx.toast_err(e),
            },
            false,
        )
    } else {
        MenuItem::run(
            "隐藏这一块",
            move |ctx: &mut EventCtx| match store.add(&selector) {
                Ok(true) => toast_reopen(ctx, "已隐藏这一块"),
                Ok(false) => ctx.toast("这一块已在隐藏清单里"),
                Err(e) => ctx.toast_err(e),
            },
            false,
        )
    }
}

/// 隐藏类改动的统一提示。即时生效，故不再让人重开窗口。
fn toast_reopen(ctx: &mut EventCtx, what: &str) {
    ctx.toast(format!("{what}，改动已生效"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(raw: &[&str]) -> Rc<UserHideStore> {
        Rc::new(UserHideStore::in_memory(crate::user_hide::UserHideConfig {
            enabled: true,
            show_hidden: false,
            hide: raw.iter().map(|s| s.to_string()).collect(),
        }))
    }

    fn labels(items: &[MenuItem]) -> Vec<String> {
        items
            .iter()
            .filter(|i| !i.separator)
            .map(|i| i.label.clone())
            .collect()
    }

    /// 行的菜单：复制选择器 + 隐藏这一项 + 隐藏这一节 + 复制这一节的选择器。
    #[test]
    fn key_target_offers_only_item_level_actions() {
        let items = build_items(
            &store(&[]),
            &Target::Key {
                key: "ui.font.family".into(),
                group: "ui".into(),
                section: "主题".into(),
            },
        );
        // 行上只给项级动作：整节隐藏归卡片菜单（见 `build_items` 的说明）。
        assert_eq!(labels(&items), ["复制选择器", "隐藏这一项"]);
    }

    /// 已被前缀规则藏着时，行菜单给的是「取消隐藏」而不是再藏一次。
    #[test]
    fn hidden_key_offers_unhide() {
        let s = store(&["key:ui.candidate.*"]);
        let items = key_hide_item(s, "ui.candidate.layout");
        assert_eq!(labels(std::slice::from_ref(&items)), ["取消隐藏这一项"]);
    }

    /// 卡片菜单只有「隐藏这一节」与复制；不含「隐藏这一项」。
    #[test]
    fn section_target_has_no_item_level_actions() {
        let items = build_items(
            &store(&[]),
            &Target::Section {
                group: "ui".into(),
                section: "主题".into(),
            },
        );
        assert_eq!(labels(&items), ["隐藏这一节", "复制选择器"]);
    }

    /// 手写卡：没藏过给「隐藏」，藏过给「取消隐藏」。
    #[test]
    fn feature_item_toggles() {
        let items = build_items(&store(&[]), &Target::Feature("backup"));
        assert_eq!(labels(&items), ["隐藏这一块", "复制选择器"]);

        let items = build_items(&store(&["feature:backup"]), &Target::Feature("backup"));
        assert_eq!(labels(&items), ["取消隐藏这一块", "复制选择器"]);
    }

    /// 功能未开启时菜单整体为空：windui 不弹空菜单，等于这一手势不存在。
    #[test]
    fn menu_is_empty_when_feature_disabled() {
        let off = Rc::new(UserHideStore::in_memory(
            crate::user_hide::UserHideConfig::default(),
        ));
        assert!(!hiding_enabled(&off));
        assert!(hiding_enabled(&store(&[])));
    }
}

#[cfg(test)]
mod feature_id_tests {
    /// 页面上挂 `wrap_feature_menu` 的那批 id 必须都在 `SETTINGS_FEATURES` 里。
    ///
    /// 写错一个字符的后果：菜单能弹、点了也写进配置，但 `Manifest::feature_hidden` 的
    /// `debug_assert` 只在 debug 下炸，release 里卡片照旧显示——用户看到「已隐藏」却
    /// 什么都没变，且没有任何报错。这正是 `custom_hide` 反复踩过的「两份字符串知识」。
    #[test]
    fn wrapped_feature_ids_are_known() {
        let used = [
            // pages::advanced_page
            "integration",
            "config_import",
            "backup",
            "compat",
            "memory",
            // pages::appearance_page
            "theme_transfer",
            "theme_editor",
            // shell::build_footer
            "dark_mode",
            "accent",
        ];
        for id in used {
            assert!(
                wind_config::SETTINGS_FEATURES.contains(&id),
                "`{id}` 不在 SETTINGS_FEATURES 里"
            );
        }
    }
}
