//! 方案级指示条 —— 「这项设置我带不了你过去，但能告诉你它在哪」。
//!
//! 方案设置对话框里的项**每个方案各有一份**，搜索时「哪个方案」是未定的：
//! 直接替用户挑一个方案打开，等于替他做了个他没做的决定。所以这类命中只跳到方案页，
//! 再浮一条说明路径的提示，方案由他自己选。
//!
//! ## 为什么不做成 toast、也不做成对话框
//!
//! - **对话框**会拦住操作——而用户此刻正需要去点方案列表，提示挡在中间适得其反；
//! - **toast** 几秒就没了，而「照着路径走一遍」要花的时间远不止几秒。
//!
//! 故取 [`crate::pages::conflict_bar`] 那套位置语义：内容区顶部、滚动区之外、切页常驻、
//! 无内容时隐藏且不占位。它不抢焦点、不拦点击，读着提示的同时就能去点方案列表。
//!
//! ## 为什么不自动消失
//!
//! 规划里原本写了「12 秒自动淡出」。实现时否掉了：本应用**空闲零渲染**（帧由交互驱动），
//! 定时消失既不可靠，又可能在用户还没读完时就没了。改成只由「知道了」关闭——
//! 它占的是一条窄横幅，留着不碍事。

use windui::prelude::*;

/// 由方案级目标的路径生成提示文案。
///
/// `path` 的形状由 `search::index::schema_level_entries` 约定：
/// `[方案, <选择一个方案>, 方案设置…, <TAB>, <标题>]`。取末两段即可，
/// 前面几段是写给结果列表看的面包屑。
pub fn hint_text(path: &[String]) -> String {
    let title = path.last().map(String::as_str).unwrap_or("该设置");
    let tab = path
        .get(path.len().wrapping_sub(2))
        .map(String::as_str)
        .unwrap_or("方案设置");
    // 一句话说清「从哪走」：先点明它是方案级的（所以搜索带不过去），再给一条箭头路径。
    // 写成整句解释会折成两行——这条横幅越矮越不碍事。
    format!("「{title}」是方案级设置：选中下面某个方案 →「方案设置…」→「{tab}」")
}

/// 指示条。`hint` 为空时整条隐藏且不占布局。
pub fn build_hint_bar(hint: Signal<String>) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 10)
        .spacing(12)
        // 用 Accent 而不是 Warning：这是导航提示，不是警告。本主题没有 Info 角色，
        // accent 的淡底在明暗两套下都与内容区拉得开。
        .bg_role_alpha(Role::Accent, 0.12)
        .border_role(Role::Divider, 1)
        .child(
            Element::label("ⓘ")
                .font_size(13.0)
                .fg_role(Role::Accent)
                .max_lines(1),
        )
        .child(
            Element::label_signal(hint)
                .font_size(12.5)
                .fg_role(Role::Text)
                .weight(1.0)
                // 路径较长、窄窗会折行——给足行数，免得静默截断把最后那一段吃掉。
                .max_lines(4),
        )
        .child(
            Element::row()
                .padding_xy(10, 4)
                .child(Element::label("知道了").font_size(12.0).max_lines(1))
                .clickable()
                .corner(4.0)
                .on_click(move |_| hint.set(String::new())),
        )
        .visible_when(move || !hint.get().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::index::{JumpTarget, build_index};

    #[test]
    fn hint_names_the_tab_and_title() {
        let path: Vec<String> = [
            "方案",
            "<选择一个方案>",
            "方案设置…",
            "方案自定义",
            "码表配置",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let t = hint_text(&path);
        assert!(t.contains("码表配置"), "{t}");
        assert!(t.contains("方案自定义"), "{t}");
        // 顺序也要对：标题在前作主语（「『码表配置』是方案级设置：…」），
        // TAB 在末尾收尾路径。倒过来就不成句了。
        // （不查「方案设置…」——那是格式串里的字面量，与传入的 path 无关，
        // 断言它等于什么都没断言。）
        let (i_title, i_tab) = (t.find("码表配置"), t.find("方案自定义"));
        assert!(i_title < i_tab, "标题应出现在 TAB 之前：{t}");
    }

    /// 路径短到不合约定也不能 panic（索引改坏时宁可给句含糊的话，也不要崩）。
    #[test]
    fn hint_survives_short_path() {
        assert!(!hint_text(&[]).is_empty());
        assert!(!hint_text(&["方案".to_string()]).is_empty());
    }

    /// 索引里每条方案级条目都能生成一句像样的提示。
    #[test]
    fn every_schema_level_entry_has_hint() {
        let idx = build_index(&crate::manifest::Manifest::from_embedded());
        let mut n = 0;
        for e in &idx {
            if let JumpTarget::SchemaLevel { hint_path } = &e.target {
                let t = hint_text(hint_path);
                assert!(
                    t.contains(&e.title),
                    "「{}」的提示没提到它自己：{t}",
                    e.title
                );
                n += 1;
            }
        }
        assert!(n >= 5, "方案级条目太少（{n}），索引可能没派生出来");
    }

    #[test]
    fn hint_bar_builds_without_panicking() {
        let _ = build_hint_bar(signal(String::new()));
    }
}
