//! 词库页视图 — 两级导航（数据域下拉 + 方案子标签）+ 响应式表格 + 搜索/分页工具栏。
//!
//! 反应式装配：
//! - 数据域用 `dropdown_reactive`（选项随 `set_schemas` 更新）、子标签用 `segmented`；
//!   二者变化由 [`super::reactive::reactive_effect`] 监听后触发 `on_nav_changed`（重载）。
//! - 表格用 `list_signal(list_data)`，随重载即时刷新；表头/行/空态由 `DictListItem` 分发。
//! - 工具栏/分页的可见与可用性用 `visible_when`/`enabled_when` 轮询当前 spec，无需重建整页。

use windui::prelude::*;

use crate::state::AppState;
use crate::widgets::flat_panel;

use super::reactive::reactive_effect;
use super::spec::{ColumnSpec, WordField, WordRow};
use super::state::{DictListItem, DictManagerState};

/// 构建词库页。
pub fn build_page(state: &AppState) -> Element {
    let mgr = state.dict_mgr.clone();

    // ── 响应式 effect：数据域/子标签变化 → 归零分页 + 清空搜索 + 重载 ──
    let dom_v = mgr.domain;
    let sub_v = mgr.sub_tab;
    let mgr_eff = mgr.clone();
    let effect = reactive_effect(
        move || dom_v.version().wrapping_mul(1_000_003).wrapping_add(sub_v.version()),
        move || mgr_eff.on_nav_changed(),
    );

    // ── 数据域选择器（+ 状态文字 + 刷新）──
    let mgr_refresh = mgr.clone();
    let domain_row = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(10)
        .padding_xy(0, 2)
        .child(Element::label("词库类型").font_size(13.0).fg_role(Role::TextMuted))
        .child(Element::dropdown_reactive(mgr.domain_items, mgr.domain).width(260))
        .child(Element::leaf().weight(1.0))
        .child(
            Element::label_rc(mgr.status)
                .font_size(11.5)
                .fg_role(Role::TextMuted)
                .max_lines(1),
        )
        .child(
            Element::button("刷新")
                .small()
                .outline()
                .on_click(move |_| mgr_refresh.reload()),
        );

    // ── 方案子标签（仅方案域显示）──
    let dom_sub = mgr.domain;
    let sub_tabs = Element::row()
        .width_match()
        .padding_xy(0, 2)
        .child(Element::segmented(mgr.sub_tab_titles(), mgr.sub_tab).width(380))
        .visible_when(move || dom_sub.get() > 0);

    // ── 搜索栏（仅可搜类别显示）──
    let mgr_search = mgr.clone();
    let mgr_clear = mgr.clone();
    let dom_srch = mgr.clone();
    let search_box = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(0, 2)
        .child(Element::text_input(mgr.search, "搜索编码或词条…").width(260).height(30))
        .child(
            Element::button("搜索")
                .small()
                .outline()
                .on_click(move |_| mgr_search.apply_search()),
        )
        .child(
            Element::button("清除")
                .small()
                .outline()
                .on_click(move |_| {
                    mgr_clear.search.set(String::new());
                    mgr_clear.apply_search();
                }),
        )
        .visible_when(move || dom_srch.current_spec().searchable);

    // ── 表格（表头 + 行 + 空态，随 list_data 刷新）──
    let mgr_row = mgr.clone();
    let table = Element::list_signal(mgr.list_data, |_| (), move |item| match item {
        DictListItem::Header => build_header(&mgr_row),
        DictListItem::Row(row, idx) => build_row(&mgr_row, &row, idx),
        DictListItem::Empty(msg) => build_empty(&msg),
    })
    .weight(1.0);

    let card = flat_panel()
        .corner(10.0)
        .width_match()
        .weight(1.0)
        .padding_xy(4, 2)
        .child(table);

    // ── 分页（仅分页类别显示）──
    let dom_pg = mgr.clone();
    let mgr_prev = mgr.clone();
    let mgr_next = mgr.clone();
    let mgr_hp = mgr.clone();
    let mgr_hn = mgr.clone();
    let pagination = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(2, 4)
        .child(
            Element::label_rc(mgr.page_status)
                .font_size(11.5)
                .fg_role(Role::TextMuted)
                .max_lines(1),
        )
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("‹ 上一页")
                .small()
                .outline()
                .enabled_when(move || mgr_hp.has_prev())
                .on_click(move |_| mgr_prev.prev_page()),
        )
        .child(
            Element::button("下一页 ›")
                .small()
                .outline()
                .enabled_when(move || mgr_hn.has_next())
                .on_click(move |_| mgr_next.next_page()),
        )
        .visible_when(move || dom_pg.current_spec().paged);

    let main_col = Element::col()
        .fill()
        .bg_role(Role::SurfaceAlt)
        .padding_xy(20, 16)
        .spacing(12)
        .child(domain_row)
        .child(sub_tabs)
        .child(search_box)
        .child(card)
        .child(pagination);

    // effect 为不可见节点，叠在 stack 里避免占据 col 间距。
    Element::stack().fill().child(main_col).child(effect)
}

/// 表头行（列名随当前类别 spec 变化）。
fn build_header(mgr: &DictManagerState) -> Element {
    let spec = mgr.current_spec();
    let mut r = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(10, 8)
        .spacing(8)
        .bg_role(Role::SurfaceAlt);
    for c in spec.columns {
        r = r.child(
            Element::label(c.header)
                .font_size(11.5)
                .font_weight(600)
                .fg_role(Role::TextMuted)
                .weight(c.weight)
                .width_match()
                .max_lines(1),
        );
    }
    r
}

/// 一行词条（列按当前 spec 渲染）+ 底部分隔线。
fn build_row(mgr: &DictManagerState, row: &WordRow, _idx: usize) -> Element {
    let spec = mgr.current_spec();
    let mut r = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(10, 7)
        .spacing(8);
    for c in spec.columns {
        r = r.child(build_cell(c, row));
    }
    Element::col().width_match().child(r).child(Element::divider())
}

/// 单元格：编码用等宽徽章、类型/启用用状态标签，其余纯文本。
fn build_cell(c: &ColumnSpec, row: &WordRow) -> Element {
    match c.field {
        WordField::Code => cell_wrap(
            c.weight,
            Element::label(row.code.clone())
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .padding_xy(6, 2)
                .corner(4.0)
                .bg_role(Role::Surface)
                .border_role(Role::Border, 1)
                .max_lines(1),
        ),
        WordField::Kind => {
            let (label, role) = match row.kind.as_str() {
                "pin" => ("置顶", Role::Accent),
                "delete" => ("删除", Role::TextMuted),
                _ => ("—", Role::TextMuted),
            };
            cell_wrap(
                c.weight,
                Element::label(label)
                    .font_size(11.0)
                    .fg_role(role)
                    .padding_xy(6, 2)
                    .corner(4.0)
                    .border_role(role, 1),
            )
        }
        WordField::Enabled => Element::label(if row.enabled { "启用" } else { "停用" })
            .font_size(12.0)
            .fg_role(if row.enabled { Role::Accent } else { Role::TextMuted })
            .weight(c.weight)
            .width_match()
            .max_lines(1),
        _ => Element::label(row.cell_text(c.field))
            .font_size(13.0)
            .fg_role(Role::Text)
            .weight(c.weight)
            .width_match()
            .max_lines(1)
            .truncate(Truncate::End),
    }
}

/// 把徽章/标签放进一个按列宽分配的左对齐格子（徽章紧贴内容，不被拉伸）。
fn cell_wrap(weight: f32, inner: Element) -> Element {
    Element::row()
        .weight(weight)
        .cross(Align::Center)
        .child(inner)
        .child(Element::leaf().weight(1.0))
}

/// 空态提示（无数据/未连接/无方案）。
fn build_empty(msg: &str) -> Element {
    Element::col()
        .width_match()
        .cross(Align::Center)
        .padding_xy(0, 40)
        .child(
            Element::label(msg)
                .font_size(13.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .text_align(Align::Center),
        )
}
