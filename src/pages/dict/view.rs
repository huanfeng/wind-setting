//! 词库页视图 — 两级导航（数据域下拉 + 方案子标签）+ 响应式表格 + 工具栏（批量/搜索）+ 分页。
//!
//! 反应式装配：
//! - 数据域用 `dropdown_reactive`（选项随 `set_schemas` 更新）、子标签用 `segmented`；
//!   二者变化由 [`super::reactive::reactive_effect`] 监听后触发 `on_nav_changed`（重载）。
//! - 表格用 `list_signal(list_data)`，随重载即时刷新；表头/行/空态由 `DictListItem` 分发。
//! - 工具栏/分页/行操作按钮的可见性用 `visible_when`/`enabled_when` 轮询当前 spec 的
//!   `ops`（可用操作集），无需重建整页——切类别时按钮自动增减。
//! - 写操作直接 RPC 即时生效（`DictManagerState` 的 do_*/request_*），破坏性操作走确认对话框。

use windui::prelude::*;

use crate::state::AppState;
use crate::widgets::flat_panel;

use super::reactive::{reactive_effect, toast_sink};
use super::spec::{ColumnSpec, WordField, WordRow};
use super::state::{DictListItem, DictManagerState, DomainKind};

/// 操作列固定宽度（表头与行需一致）。
const ACTION_W: i32 = 168;

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

    // ── 响应式 effect：搜索内容变化 → 输入即搜 ──
    let search_v = mgr.search;
    let mgr_se = mgr.clone();
    let search_effect = reactive_effect(
        move || search_v.version(),
        move || mgr_se.on_search_changed(),
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

    // ── 方案子标签（按 domain_kind 动态显隐，四条 segmented 同时只显一条）──
    let k_phrase = mgr.clone();
    let k_py = mgr.clone();
    let k_wb = mgr.clone();
    let k_mx = mgr.clone();
    let seg = |titles: Vec<&str>, mgr: DictManagerState, kind: DomainKind| {
        let m = mgr.clone();
        Element::segmented(titles.iter().map(|s| s.to_string()).collect::<Vec<_>>(), mgr.sub_tab)
            .width(380)
            .visible_when(move || m.domain_kind() == kind)
    };
    let sub_tabs = Element::row()
        .width_match()
        .padding_xy(0, 2)
        .child(seg(vec!["系统短语", "用户短语"], k_phrase, DomainKind::Phrase))
        .child(seg(vec!["用户词库", "临时词库", "词频"], k_py, DomainKind::Pinyin))
        .child(seg(vec!["用户词库", "临时词库", "词频", "候选调整"], k_wb, DomainKind::Codetable))
        .child(seg(vec!["候选调整"], k_mx, DomainKind::Mixed));

    // ── 工具栏：批量操作（左）+ 搜索（右）──
    let toolbar = build_toolbar(&mgr);

    // ── 表格（表头 + 行 + 空态，随 list_data 刷新）──
    let mgr_row = mgr.clone();
    let table = Element::list_signal(mgr.list_data, |_| (), move |item| match item {
        DictListItem::Header => build_header(&mgr_row),
        DictListItem::Row(row, idx) => build_row(&mgr_row, &row, idx),
        DictListItem::Empty(msg) => build_empty(&msg),
    })
    .width_match();

    let card = flat_panel()
        .corner(10.0)
        .width_match()
        .weight(1.0)
        .padding_xy(4, 2)
        .child(Element::scroll().fill().child(table));

    // ── 分页（仅分页类别显示）──
    let pagination = build_pagination(&mgr);

    let main_col = Element::col()
        .fill()
        .bg_role(Role::SurfaceAlt)
        .padding_xy(20, 16)
        .spacing(12)
        .child(domain_row)
        .child(sub_tabs)
        .child(toolbar)
        .child(card)
        .child(pagination);

    // effect/toast 为不可见/浮层节点，叠在 stack 里避免占据 col 间距。
    Element::stack()
        .fill()
        .child(main_col)
        .child(effect)
        .child(search_effect)
        .child(toast_sink(mgr.feedback))
}

/// 工具栏：批量操作按钮（按 ops 可见）+ 搜索框（可搜类别）。
fn build_toolbar(mgr: &DictManagerState) -> Element {
    // 添加（dict/phrase）
    let m_add = mgr.clone();
    let v_add = mgr.clone();
    let add = Element::button("+ 添加")
        .small()
        .on_click(move |_| m_add.open_add())
        .visible_when(move || v_add.current_spec().ops.add);

    // 批量：全部转正（temp）/ 清空（dict/temp/freq/shadow）/ 恢复默认（phrase）
    let m_pa = mgr.clone();
    let v_pa = mgr.clone();
    let promote_all = Element::button("全部转正")
        .small()
        .outline()
        .on_click(move |_| m_pa.do_promote_all())
        .visible_when(move || v_pa.current_spec().ops.promote_all);

    let m_clear = mgr.clone();
    let v_clear = mgr.clone();
    let clear = Element::button("清空")
        .small()
        .outline()
        .danger()
        .on_click(move |_| m_clear.request_clear())
        .visible_when(move || v_clear.current_spec().ops.clear);

    let m_reset = mgr.clone();
    let v_reset = mgr.clone();
    let reset_default = Element::button("恢复默认")
        .small()
        .outline()
        .on_click(move |_| m_reset.request_reset_default())
        .visible_when(move || v_reset.current_spec().ops.reset_default);

    // 搜索（输入即搜，仅留清除按钮；分页类/非分页类均显示）
    let m_clr_s = mgr.clone();
    let v_search = mgr.clone();
    let search = Element::row()
        .cross(Align::Center)
        .spacing(8)
        .child(Element::text_input(mgr.search, "搜索编码或词条…").width(240).height(30))
        .child(
            Element::button("清除")
                .small()
                .outline()
                .on_click(move |_| {
                    m_clr_s.search.set(String::new());
                }),
        )
        .visible_when(move || v_search.current_spec().searchable || v_search.current_spec().client_search);

    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(0, 2)
        .child(add)
        .child(promote_all)
        .child(clear)
        .child(reset_default)
        .child(Element::leaf().weight(1.0))
        .child(search)
}

/// 分页条（仅分页类别显示）。
fn build_pagination(mgr: &DictManagerState) -> Element {
    let dom_pg = mgr.clone();
    let m_prev = mgr.clone();
    let m_next = mgr.clone();
    let m_hp = mgr.clone();
    let m_hn = mgr.clone();
    Element::row()
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
                .enabled_when(move || m_hp.has_prev())
                .on_click(move |_| m_prev.prev_page()),
        )
        .child(
            Element::button("下一页 ›")
                .small()
                .outline()
                .enabled_when(move || m_hn.has_next())
                .on_click(move |_| m_next.next_page()),
        )
        .visible_when(move || dom_pg.current_spec().paged)
}

/// 表头行（列名随当前类别 spec 变化 + 操作列）。
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
    r.child(
        Element::label("操作")
            .font_size(11.5)
            .font_weight(600)
            .fg_role(Role::TextMuted)
            .width(ACTION_W),
    )
}

/// 一行词条（列按当前 spec 渲染 + 操作按钮）+ 底部分隔线。
fn build_row(mgr: &DictManagerState, row: &WordRow, _idx: usize) -> Element {
    let spec = mgr.current_spec();
    let mut r = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(10, 6)
        .spacing(8);
    for c in spec.columns {
        // 启用列渲染为交互开关（短语）；其余为静态单元格。
        if c.field == WordField::Enabled {
            r = r.child(build_enabled_toggle(mgr, row, c.weight));
        } else {
            r = r.child(build_cell(c, row));
        }
    }
    r = r.child(build_actions(mgr, row));
    Element::col().width_match().child(r).child(Element::divider())
}

/// 行尾操作按钮（按当前 spec 的 ops 增减）：转正 / 撤销 / 删除。
fn build_actions(mgr: &DictManagerState, row: &WordRow) -> Element {
    let spec = mgr.current_spec();
    let mut r = Element::row().width(ACTION_W).cross(Align::Center).spacing(6);

    // 编辑（用户词库/短语）
    if spec.ops.edit {
        let m = mgr.clone();
        let (c, t, w) = (row.code.clone(), row.text.clone(), row.weight);
        r = r.child(
            Element::button("编辑")
                .small()
                .outline()
                .on_click(move |_| m.open_edit(&c, &t, w)),
        );
    }

    if spec.ops.promote {
        let m = mgr.clone();
        let (c, t) = (row.code.clone(), row.text.clone());
        r = r.child(
            Element::button("转正")
                .small()
                .outline()
                .on_click(move |_| m.do_promote(&c, &t)),
        );
    }
    // 候选调整：撤销规则（轻量，直接执行不确认）。
    if spec.ops.remove_rule {
        let m = mgr.clone();
        let (c, t) = (row.code.clone(), row.text.clone());
        r = r.child(
            Element::button("撤销")
                .small()
                .outline()
                .on_click(move |_| m.do_delete_row(&c, &t)),
        );
    }
    // 删除词条（确认后执行）。
    if spec.ops.remove {
        let m = mgr.clone();
        let (c, t) = (row.code.clone(), row.text.clone());
        r = r.child(
            Element::button("删除")
                .small()
                .danger()
                .on_click(move |_| m.request_delete_row(&c, &t)),
        );
    }
    r
}

/// 启用开关单元格（短语）：勾选即调 `phrase.setEnabled` 即时生效。
fn build_enabled_toggle(mgr: &DictManagerState, row: &WordRow, weight: f32) -> Element {
    let sig = signal(row.enabled);
    let m = mgr.clone();
    let (c, t) = (row.code.clone(), row.text.clone());
    let cb = Element::checkbox("", sig).small().on_toggle(move |_| {
        let v = !sig.get();
        sig.set(v);
        m.do_set_enabled(&c, &t, v);
    });
    cell_wrap(weight, cb)
}

/// 单元格：编码用等宽徽章、类型用状态标签，其余纯文本。
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
        _ => Element::label(row.cell_text(c.field))
            .font_size(13.0)
            .fg_role(Role::Text)
            .weight(c.weight)
            .width_match()
            .max_lines(1)
            .truncate(Truncate::End),
    }
}

/// 把徽章/开关放进一个按列宽分配的左对齐格子（内容紧贴左侧，不被拉伸）。
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
