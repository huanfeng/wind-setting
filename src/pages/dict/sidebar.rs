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

/// 构建左栏。`back` 是底栏左侧的「返回设置」按钮，由窗口层构造（它要 `AppState`，
/// 而本模块只认识 `DictManagerState`）。
///
/// 整个列表挂在 `host_signal(domain_items)` 上：方案是运行期才由 `set_schemas` 注入的
/// （冷启动时后台 RPC 还没回来，列表是空的），且用户在方案页增删方案后也会变。不挂响应式
/// 就会停在建窗那一刻的快照上 —— 症状是「装了新方案，词库窗里死活看不到」。
pub fn build_sidebar(mgr: &DictManagerState, back: Element) -> Element {
    let mgr_list = mgr.clone();
    // 挂 `domain_epoch`（单元素计数器）而**不是** `domain_items`：`host_signal` 按 Vec 的
    // 每个元素调一次构建器，挂多元素信号会得到 N 份完整列表叠在一起。同 view.rs 的
    // table_host。
    let list = Element::host_signal(mgr.domain_epoch, move |_| build_list(&mgr_list));

    let mgr_refresh = mgr.clone();
    let status = mgr.status;
    // 状态与按钮**分两行**：200 宽的左栏放不下「一行状态 + 两个按钮」，挤在一行会把
    // 状态压成两三个字。状态为空时整行隐藏且不占位（`visible_when` 语义），故通常看到的
    // 就是紧贴底边的一排按钮。
    let footer = Element::col()
        .width_match()
        .padding_xy(10, 8)
        .spacing(6)
        .child(
            // 状态文字（「已加载 250 条」「未连接」等）。原先挤在页顶下拉右侧，那里同时还要
            // 放刷新按钮与 ‹ ›，长一点的状态就被 max_lines(1) 截掉；挪到左栏底部后有整行可用。
            Element::label_signal(status)
                .font_size(11.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(2)
                .visible_when(move || !status.get().is_empty()),
        )
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(6)
                // 两个按钮等宽平分：一个靠左一个靠右会在窄栏里显得散，等宽是这一栏里
                // 唯一能同时对齐左右边缘的排法。
                .child(back.weight(1.0))
                .child(
                    Element::button("刷新")
                        .small()
                        .outline_soft()
                        .weight(1.0)
                        .on_click(move |_| mgr_refresh.reload()),
                ),
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

/// 左栏的一个条目。`domain` 是它要写进 `mgr.domain` 的**数据域下标**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarEntry {
    /// 数据域下标（`domain_slot` 的定义域）。
    pub domain: usize,
    /// 显示名（方案名，已去掉折叠标签里的「 · 引擎」后缀）。
    pub name: String,
    /// 右侧徽章文字（「全局」或引擎类型）。
    pub tag: String,
    /// 是否属于「输入方案」组（false = 全局组）。
    pub in_schema_group: bool,
}

/// 算出左栏要渲染哪些条目。
///
/// 与渲染分开是为了能单测**下标**：这一栏最危险的错误不是画歪，而是条目与数据域错位
/// 一格——点「五笔86」出来的是拼音的词，不报错、不 panic，只是数据全不对。
/// `state.rs:303` 记着同一个坑（加第二个全局域时四处手写的 `domain - 1` 漏改一处）。
///
/// 故这里一律以 [`GLOBAL_DOMAINS`] 为界，不写字面量 0/1，也不做下标算术。
pub fn sidebar_entries(mgr: &DictManagerState) -> Vec<SidebarEntry> {
    let mut out = Vec::new();

    let labels = mgr.domain_labels();
    for (i, label) in labels.iter().take(GLOBAL_DOMAINS).enumerate() {
        out.push(SidebarEntry {
            domain: i,
            name: label.clone(),
            tag: "全局".to_string(),
            in_schema_group: false,
        });
    }

    for (n, e) in mgr.folded_domains().iter().enumerate() {
        // 折叠标签形如「五笔86 · 码表」：主文本取 · 前段，引擎类型另作徽章。
        // 与 `domain_rich_items` 同一套拆法，两处必须一致，否则同一个方案在下拉与
        // 左栏里显示成两个名字。
        let name = e
            .label
            .split_once(" · ")
            .map(|(n, _)| n.to_string())
            .unwrap_or_else(|| e.label.clone());
        out.push(SidebarEntry {
            domain: GLOBAL_DOMAINS + n,
            name,
            tag: engine_label(&e.kind).to_string(),
            in_schema_group: true,
        });
    }
    out
}

/// 列表主体：两个分组标题 + 各自的条目。
fn build_list(mgr: &DictManagerState) -> Element {
    let mut col = Element::col().width_match().spacing(2);
    let entries = sidebar_entries(mgr);

    col = col.child(group_title("全局"));
    for e in entries.iter().filter(|e| !e.in_schema_group) {
        col = col.child(nav_item(mgr, e.domain, &e.name, &e.tag));
    }

    let schemas: Vec<&SidebarEntry> = entries.iter().filter(|e| e.in_schema_group).collect();
    // 一个方案都没装（或 core 没连上）时不画这个标题：光秃秃一个「输入方案」下面什么
    // 都没有，比不画更像出了错。空态提示由右侧的 empty_msg 覆盖层负责。
    if !schemas.is_empty() {
        col = col.child(group_title("输入方案"));
        for e in schemas {
            col = col.child(nav_item(mgr, e.domain, &e.name, &e.tag));
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
        .child(badge(tag, active))
}

/// 引擎类型徽章（「拼音」「码表」「全局」…）。
///
/// 与方案页的徽章同一套尺寸与配色（`schema_manager` 的 type/source badge），两处并排
/// 出现时不该有两种长相。
///
/// **靠边框而不是靠颜色把它立起来**：左栏每行都挂一枚，密度比方案页高得多，整列染上
/// 强调色会很吵；但为了不吵而一路降到 `TextDisabled` + 无底无框，就掉到可读性下限
/// 之外了——这正是 windui `badge_intent` 注释里记着的那个坑（灰字灰底几乎看不清）。
/// 形状由边框提供，颜色保持克制。
///
/// 选中行例外：徽章跟着选中态转成强调色，与行内文字呼应。只有一行是这样，不构成噪音。
fn badge(tag: &str, active: bool) -> Element {
    let el = Element::label(tag)
        .font_size(10.5)
        .max_lines(1)
        .corner(4.0)
        .padding_xy(5, 2);
    if active {
        el.fg_role(Role::Accent).border_role(Role::Accent, 1)
    } else {
        el.fg_role(Role::TextMuted)
            .bg_role(Role::SurfaceAlt)
            .border_role(Role::Border, 1)
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::{DomainSlot, domain_slot};
    use super::*;
    use crate::state::SchemaInfo;

    fn schema(id: &str, name: &str, kind: &str) -> SchemaInfo {
        SchemaInfo {
            id: id.into(),
            name: name.into(),
            kind: kind.into(),
            ..Default::default()
        }
    }

    /// 直接写 schemas，不经 `set_schemas`（那会触发 RPC）。
    fn with_schemas(mgr: &DictManagerState, list: Vec<SchemaInfo>) {
        *mgr.schemas.borrow_mut() = list;
    }

    fn fixture() -> DictManagerState {
        let mgr = DictManagerState::new();
        with_schemas(
            &mgr,
            vec![
                schema("wb", "五笔86", "codetable"),
                schema("quanpin", "全拼", "pinyin"),
                schema("mixed", "混输", "mixed"),
            ],
        );
        mgr
    }

    /// 本栏最危险的错误：条目与数据域**错位一格** —— 点「五笔86」出来的是拼音的词，
    /// 不报错也不 panic。故逐条把下标喂回 `domain_slot`，核对语义与它在列表里的位置一致。
    #[test]
    fn entry_domains_match_domain_slot_semantics() {
        let entries = sidebar_entries(&fixture());

        let globals: Vec<&SidebarEntry> = entries.iter().filter(|e| !e.in_schema_group).collect();
        assert_eq!(
            globals.len(),
            GLOBAL_DOMAINS,
            "全局组条目数必须等于 GLOBAL_DOMAINS，多一条少一条都会让其后的方案域整体错位"
        );
        assert_eq!(domain_slot(globals[0].domain), DomainSlot::Phrase);
        assert_eq!(domain_slot(globals[1].domain), DomainSlot::QuickFormat);

        for (n, e) in entries.iter().filter(|e| e.in_schema_group).enumerate() {
            assert_eq!(
                domain_slot(e.domain),
                DomainSlot::Schema(n),
                "方案组第 {n} 项「{}」的域下标解析错位",
                e.name
            );
        }
    }

    /// 名字/徽章的拆法必须与数据域下拉（`domain_rich_items`）一致 —— 同一个方案在两处
    /// 显示成两个名字，用户只会以为那是两个不同的东西。
    #[test]
    fn entry_names_have_engine_suffix_stripped() {
        let entries = sidebar_entries(&fixture());
        let schemas: Vec<&SidebarEntry> = entries.iter().filter(|e| e.in_schema_group).collect();

        for e in &schemas {
            assert!(
                !e.name.contains(" · "),
                "方案名「{}」仍带着折叠标签的引擎后缀，应当拆进 tag",
                e.name
            );
            assert!(!e.tag.is_empty(), "方案条目必须带引擎类型徽章");
        }
        assert!(
            schemas
                .iter()
                .any(|e| e.name == "五笔86" && e.tag == "码表"),
            "应有「五笔86」+「码表」徽章，实得 {:?}",
            schemas
                .iter()
                .map(|e| (&e.name, &e.tag))
                .collect::<Vec<_>>()
        );
    }

    /// 一个方案都没有时（core 没连上 / 全部隐藏）只剩全局组，不留一个空的「输入方案」标题。
    #[test]
    fn no_schema_group_when_no_schemas() {
        let entries = sidebar_entries(&DictManagerState::new());

        assert_eq!(entries.len(), GLOBAL_DOMAINS);
        assert!(entries.iter().all(|e| !e.in_schema_group));
    }

    /// 域集合变化必须递增 `domain_epoch`，否则左栏停在建窗那一刻的快照上 ——
    /// 症状是「装了新方案，词库窗里死活看不到」。
    #[test]
    fn domain_epoch_advances_when_schemas_change() {
        let mgr = DictManagerState::new();
        let before = mgr.domain_epoch.get()[0];

        mgr.set_schemas_no_reload(&[schema("wb", "五笔86", "codetable")]);

        assert_eq!(mgr.domain_epoch.get()[0], before + 1);
    }
}
