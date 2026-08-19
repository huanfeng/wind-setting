//! 词库窗左栏 — 按「全局 / 输入方案」分组的词库类型列表 + 底部状态与刷新。
//!
//! 取代了原先横在页顶的「词库类型」下拉 + ‹ › 顺序切换：方案一多，下拉就得展开逐个找，
//! 而这里全部平铺，切换是一次点击。
//!
//! 选中样式与主窗侧栏同源（accent 淡底 + accent 文字 + 左缘指示条），只是把渐变图标块
//! 换成了引擎类型徽章 —— 那个渐变块是主窗的品牌位，逐项复制到二级导航里会喧宾夺主。
//!
//! ## 与逻辑层的接线
//!
//! 点击只写 `mgr.domain`，**不直接调重载**：`view::build_page_for` 里已经挂着监听
//! `domain`/`sub_tab` 版本的 `reactive_effect`，它会触发 `on_nav_changed`（归零分页 +
//! 重置排序 + 递增 `table_epoch` + 重载）。在这里再调一次 `reload` 会让切域多打一轮 RPC，
//! 且绕过 `on_nav_changed` 的子标签记忆恢复。

use windui::prelude::*;

use crate::dialogs::schema_manager::engine_label;
use crate::theme_def::ACCENT_LIGHT;

use super::state::{DictManagerState, GLOBAL_DOMAINS};

/// 左栏总宽。180 放不下「五笔86」+「码表」徽章（徽章会被挤到省略号），220 又让表格
/// 在最小窗宽下开始丢列，200 是两边都不难受的点。
pub const SIDEBAR_W: i32 = 200;

/// 单项行高，与主窗侧栏一致（36），保持两级导航的节奏感相同。
const ITEM_H: i32 = 36;

/// 构建左栏。
///
/// 整个列表挂在 `host_signal(domain_items)` 上：方案是运行期才由 `set_schemas` 注入的
/// （冷启动时后台 RPC 还没回来，列表是空的），且用户在方案页增删方案后也会变。不挂响应式
/// 就会停在建窗那一刻的快照上 —— 症状是「装了新方案，词库窗里死活看不到」。
pub fn build_sidebar(mgr: &DictManagerState) -> Element {
    let mgr_list = mgr.clone();
    // 挂 `domain_epoch`（单元素计数器）而**不是** `domain_items`：`host_signal` 按 Vec 的
    // 每个元素调一次构建器，挂多元素信号会得到 N 份完整列表叠在一起。同 view.rs 的
    // table_host。
    let list = Element::host_signal(mgr.domain_epoch, move |_| build_list(&mgr_list));

    let mgr_refresh = mgr.clone();
    let footer = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(6)
        .padding_xy(10, 8)
        .child(
            // 状态文字（「已加载 250 条」「未连接」等）。原先挤在页顶下拉右侧，那里同时还要
            // 放刷新按钮与 ‹ ›，长一点的状态就被 max_lines(1) 截掉；挪到左栏底部后有整行可用。
            Element::label_signal(mgr.status)
                .font_size(11.0)
                .fg_role(Role::TextMuted)
                .weight(1.0)
                .max_lines(2),
        )
        .child(
            Element::button("刷新")
                .small()
                .outline_soft()
                .on_click(move |_| mgr_refresh.reload()),
        );

    Element::col()
        .width(SIDEBAR_W)
        .height_match()
        .bg_role(Role::Bg)
        .padding_xy(8, 10)
        .child(Element::scroll().fill().weight(1.0).child(list))
        .child(Element::divider())
        .child(footer)
}

/// 列表主体：两个分组标题 + 各自的条目。
fn build_list(mgr: &DictManagerState) -> Element {
    let mut col = Element::col().width_match().spacing(2);

    col = col.child(group_title("全局"));
    // 全局域固定两个（快捷短语、快捷输入），下标就是 0..GLOBAL_DOMAINS。
    //
    // ⚠️ 不写字面量 0/1，也不写 `domain - 1` 这类下标算术：`state.rs` 那条注释记着，
    // 加第二个全局域时四处手写的偏移漏改一处就整体错位一格 —— 选「五笔86」看到拼音的
    // 数据，不报错也不 panic。这里一律以 GLOBAL_DOMAINS 为界、以 domain_slot 为判据。
    let labels = mgr.domain_labels();
    for i in 0..GLOBAL_DOMAINS {
        let label = labels.get(i).cloned().unwrap_or_default();
        col = col.child(nav_item(mgr, i, &label, "全局"));
    }

    let folded = mgr.folded_domains();
    if !folded.is_empty() {
        col = col.child(group_title("输入方案"));
        for (n, e) in folded.iter().enumerate() {
            // 折叠标签形如「五笔86 · 码表」：主文本取 · 前段，引擎类型另作徽章。
            // 与 `domain_rich_items` 同一套拆法，两处必须一致，否则同一个方案在下拉与
            // 左栏里显示成两个名字。
            let name = e
                .label
                .split_once(" · ")
                .map(|(n, _)| n.to_string())
                .unwrap_or_else(|| e.label.clone());
            col = col.child(nav_item(
                mgr,
                GLOBAL_DOMAINS + n,
                &name,
                engine_label(&e.kind),
            ));
        }
    }
    col
}

/// 分组小标题（同主窗侧栏顶部的「设置」）。
fn group_title(text: &str) -> Element {
    Element::label(text)
        .font_size(11.0)
        .font_weight(600)
        .fg_role(Role::TextDisabled)
        .width_match()
        .max_lines(1)
        .height(26)
        .padding_xy(10, 5)
}

/// 一个词库类型条目。选中/未选中两套元素 `visible_when` 切换（windui 无响应式样式）。
fn nav_item(mgr: &DictManagerState, idx: usize, name: &str, tag: &str) -> Element {
    let sel = mgr.domain;

    // 左缘指示条作覆盖层，不占行内宽度 —— 选中/未选中 padding 一致，切换时文字不横移。
    let indicator = Element::row()
        .width_match()
        .height(ITEM_H)
        .cross(Align::Center)
        .child(
            Element::leaf()
                .width(3)
                .height(14)
                .corner(1.5)
                .bg_role(Role::Accent),
        )
        .visible_when(move || sel.get() == idx);

    let on = item_row(name, tag, true)
        .bg(Color::hex(ACCENT_LIGHT).scale_alpha(0.11))
        .visible_when(move || sel.get() == idx);

    let off = item_row(name, tag, false)
        .clickable()
        .on_click(move |_| sel.set(idx))
        .visible_when(move || sel.get() != idx);

    Element::stack()
        .width_match()
        .height(ITEM_H)
        .child(on)
        .child(off)
        .child(indicator)
}

/// 条目行的共同骨架：名字（撑满）+ 引擎类型徽章。`active` 决定文字与徽章的配色。
fn item_row(name: &str, tag: &str, active: bool) -> Element {
    let (fg, weight) = if active {
        (Role::Accent, 600)
    } else {
        (Role::TextMuted, 500)
    };
    Element::row()
        .width_match()
        .height(ITEM_H)
        .corner(9.0)
        .cross(Align::Center)
        .spacing(6)
        .padding_xy(10, 0)
        .child(
            Element::label(name)
                .font_size(13.0)
                .font_weight(weight)
                .fg_role(fg)
                .weight(1.0)
                .max_lines(1)
                .height(18),
        )
        .child(badge(tag))
}

/// 引擎类型徽章（「拼音」「码表」「全局」…）。中性淡底小字，不用强调色 —— 每一行都有
/// 一枚，染上强调色整列都在闪。
fn badge(tag: &str) -> Element {
    Element::label(tag)
        .font_size(10.0)
        .fg_role(Role::TextDisabled)
        .max_lines(1)
        .corner(4.0)
        .bg_role(Role::SurfaceAlt)
        .padding_xy(5, 2)
}
