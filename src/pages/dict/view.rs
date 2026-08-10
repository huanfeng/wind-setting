//! 词库页视图 — 两级导航（数据域下拉 + 方案子标签）+ 可排序表格 + 工具栏（批量/搜索）+ 分页。
//!
//! 反应式装配：
//! - 数据域用 `dropdown_reactive`（选项随 `set_schemas` 更新）、子标签用 `segmented`；
//!   二者变化由 [`super::reactive::reactive_effect`] 监听后触发 `on_nav_changed`（重载）。
//! - 表格用 `table_sortable_server`：正文绑定 `table_rows` 信号随重载即时刷新；点表头由
//!   `sort` 信号 + `on_sort_changed` 驱动（分页类走服务端排序、非分页类客户端重排）；
//!   行悬停高亮为表格内建。列集随类别变化，由 `table_epoch` 信号驱动整表重建。
//! - 编码列徽章 / 候选调整类型彩标经 `.cell_render` 自定义；行操作按钮（含短语启用开关）
//!   经 `.actions` 生成，按显示下标从 `display_rows` 取行绑定回调。
//! - 工具栏/分页按钮的可见性用 `visible_when`/`enabled_when` 轮询当前 spec 的
//!   `ops`（可用操作集），无需重建整页——切类别时按钮自动增减。
//! - 写操作直接 RPC 即时生效（`DictManagerState` 的 do_*/request_*），破坏性操作走确认对话框。

use windui::prelude::*;

use crate::state::AppState;
use crate::widgets::flat_panel;

use super::reactive::{reactive_effect, toast_sink};
use super::spec::{ColumnSpec, WordField};
use super::state::{DictManagerState, DomainKind};

/// 操作列宽度权重（相对数据列分配；需容纳 启用开关 + 最多两枚小按钮）。
const ACTIONS_WEIGHT: f32 = 1.5;

/// 子标签单段固定宽度（各数据域分段等宽，总宽 = 段数 × 此值）。
const SUB_TAB_SEG_W: i32 = 104;

// 各子标签分段适用的数据域类型。**并集必须覆盖 `DomainKind` 的每个变体**——漏掉哪个，
// 那类方案下就一个标签都不渲染，而逻辑层 `tabs_for_domain_at` 照常给出标签集，
// 表现为「能加词条、能导入导出，就是看不到标签」。守护见
// `sub_tab_segments_cover_every_domain_kind`。
const SEG_PHRASE: &[DomainKind] = &[DomainKind::Phrase];
const SEG_PINYIN: &[DomainKind] = &[DomainKind::Pinyin];
/// 未知引擎类型挂在码表这组：逻辑层对 `Other` 也是按码表从宽给全四类，两处保持一致。
const SEG_CODETABLE: &[DomainKind] = &[DomainKind::Codetable, DomainKind::Other];
const SEG_ENGLISH: &[DomainKind] = &[DomainKind::English];
const SEG_MIXED: &[DomainKind] = &[DomainKind::Mixed];
/// 覆盖性校验用（顺序无关），仅测试构建需要。加新分段时记得挂进来。
#[cfg(test)]
const ALL_SEGMENTS: &[&[DomainKind]] = &[
    SEG_PHRASE,
    SEG_PINYIN,
    SEG_CODETABLE,
    SEG_ENGLISH,
    SEG_MIXED,
];

/// 构建词库页。
pub fn build_page(state: &AppState) -> Element {
    let mgr = state.dict_mgr.clone();

    // ── 响应式 effect：数据域/子标签变化 → 归零分页 + 重载（搜索词刻意保留）──
    let dom_v = mgr.domain;
    let sub_v = mgr.sub_tab;
    let mgr_eff = mgr.clone();
    let effect = reactive_effect(
        move || {
            dom_v
                .version()
                .wrapping_mul(1_000_003)
                .wrapping_add(sub_v.version())
        },
        move || mgr_eff.on_nav_changed(),
    );

    // ── 响应式 effect：搜索内容变化 → 输入即搜 ──
    let search_v = mgr.search;
    let mgr_se = mgr.clone();
    let search_effect = reactive_effect(
        move || search_v.version(),
        move || mgr_se.on_search_changed(),
    );

    // ── 响应式 effect：导入/导出目标方案变化 → 重算标题/说明/勾选，并按新目标重新预览 ──
    // 下拉只写信号、无变更回调（同 domain/sub_tab），故仍走版本轮询这条路。
    let target_v = mgr.io_target;
    let mgr_io = mgr.clone();
    let io_target_effect = reactive_effect(
        move || target_v.version(),
        move || mgr_io.on_io_target_changed(),
    );

    // ── 数据域选择器（富内容下拉：引擎类型徽章 + ‹ › 顺序切换 + 状态文字 + 刷新）──
    let mgr_refresh = mgr.clone();
    let dom_prev = mgr.clone();
    let dom_next = mgr.clone();
    let dom_en_p = mgr.clone();
    let dom_en_n = mgr.clone();
    let domain_row = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(10)
        .padding_xy(0, 2)
        .child(
            Element::label("词库类型")
                .font_size(13.0)
                .fg_role(Role::TextMuted),
        )
        .child(Element::dropdown_items_reactive(mgr.domain_rich_items, mgr.domain).width(240))
        // ‹ › 按顺序切换数据域（循环），下拉列表过长时无需展开逐个查找。
        .child(
            Element::button("‹")
                .small()
                .outline_soft()
                .on_click(move |_| dom_prev.prev_domain())
                .enabled_when(move || dom_en_p.domain_count() > 1),
        )
        .child(
            Element::button("›")
                .small()
                .outline_soft()
                .on_click(move |_| dom_next.next_domain())
                .enabled_when(move || dom_en_n.domain_count() > 1),
        )
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
                .outline_soft()
                .on_click(move |_| mgr_refresh.reload()),
        );

    // ── 方案子标签（按 domain_kind 动态显隐，同时只显一条）──
    //
    // 每条分段挂一组适用的 DomainKind，而非单个：`Other`（未知引擎类型）必须挂在某条上，
    // 否则那类方案下一个标签都不渲染——**而逻辑层 `tabs_for_domain_at` 仍会给出标签集**，
    // 于是「有功能、没入口」：能加词条、能导入导出，就是看不到标签。英文方案正是这么漏的
    // （`domain_kind()` 把它落到 `Other`，而这里只逐个列举了 Phrase/Pinyin/Codetable/Mixed）。
    // 覆盖性由 `sub_tab_segments_cover_every_domain_kind` 守护。
    let seg = |titles: Vec<&str>, mgr: DictManagerState, kinds: &'static [DomainKind]| {
        let m = mgr.clone();
        // 每段固定宽、总宽随段数变化：避免 1/3/4 段被均分导致各域分段宽度不一致。
        let seg_w = titles.len() as i32 * SUB_TAB_SEG_W;
        Element::segmented(
            titles.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            mgr.sub_tab,
        )
        .width(seg_w)
        .visible_when(move || kinds.contains(&m.domain_kind()))
    };
    // 「导入」「导出」入口：置于子标签行最右（弹簧右对齐），方案域与短语域均显示，
    // 保持统一入口位置（短语走原生对话框，方案打开对应对话框）。
    let imp_open = mgr.clone();
    let imp_vis = mgr.clone();
    let imp_en = mgr.clone();
    let exp_open = mgr.clone();
    let exp_vis = mgr.clone();
    let exp_en = mgr.clone();
    let import_entry = Element::button("导入")
        .small()
        .outline_soft()
        .on_click(move |ctx| imp_open.do_import(ctx))
        .visible_when(move || imp_vis.io_entry_visible())
        .enabled_when(move || imp_en.io_entry_enabled());
    let export_entry = Element::button("导出")
        .small()
        .outline_soft()
        .on_click(move |ctx| exp_open.do_export(ctx))
        .visible_when(move || exp_vis.io_entry_visible())
        .enabled_when(move || exp_en.io_entry_enabled());
    // 混输域只有「候选调整」一条分段，标签行右侧大片留白——正好安置这句说明，不占额外行高。
    //
    // 候选调整只对**已经出现过的候选**做固定/隐藏，不是加词的地方。用户在这里点「添加」
    // 填上编码和词、看到列表里多了一行，就以为词加进去了，实际打不出来——与导入那条
    // 误解同源（见 `DictManagerState::io_scope_note_for_target`）：混输方案自身不存词条。
    let hint_vis = mgr.clone();
    let mixed_hint = Element::label("混输模式只有候选调整，加词请切到码表或拼音方案")
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .max_lines(1)
        .visible_when(move || hint_vis.domain_kind() == DomainKind::Mixed);
    let sub_tabs = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(0, 2)
        .child(seg(vec!["系统短语", "用户短语"], mgr.clone(), SEG_PHRASE))
        .child(seg(
            vec!["用户词库", "临时词库", "词频"],
            mgr.clone(),
            SEG_PINYIN,
        ))
        .child(seg(
            vec!["用户词库", "临时词库", "词频", "候选调整"],
            mgr.clone(),
            SEG_CODETABLE,
        ))
        // 英文无临时词库（无造词流程，那张表恒空），故三段而非四段。
        .child(seg(
            vec!["用户词库", "词频", "候选调整"],
            mgr.clone(),
            SEG_ENGLISH,
        ))
        .child(seg(vec!["候选调整"], mgr.clone(), SEG_MIXED))
        .child(mixed_hint)
        .child(Element::leaf().weight(1.0))
        .child(import_entry)
        .child(export_entry);

    // ── 工具栏：批量操作（左）+ 搜索（右）──
    let toolbar = build_toolbar(&mgr);

    // ── 表格（列集随类别变化，table_epoch 递增时整表重建；数据/排序变化由表格内部响应）──
    // host_signal 为非滚动宿主：表格自带正文滚动，weight 布局在其中能拿到确定高度。
    let mgr_tbl = mgr.clone();
    let table_host = Element::host_signal(mgr.table_epoch, move |_| build_table(&mgr_tbl));

    // 空态提示覆盖层（无数据/未连接/无方案；empty_msg 非空时显示在表头下方）。
    let v_empty = mgr.clone();
    let empty_overlay = Element::col()
        .fill()
        .cross(Align::Center)
        .padding_xy(0, 80)
        .child(
            Element::label_rc(mgr.empty_msg)
                .font_size(13.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .text_align(Align::Center),
        )
        .visible_when(move || !v_empty.empty_msg.get().is_empty());

    let card = flat_panel()
        .corner(10.0)
        .width_match()
        .weight(1.0)
        .padding_xy(4, 2)
        .child(
            Element::stack()
                .fill()
                .child(table_host)
                .child(empty_overlay),
        );

    // ── 分页（仅分页类别显示）──
    let pagination = build_pagination(&mgr);

    let mut main_col = Element::col()
        .fill()
        .bg_role(Role::SurfaceAlt)
        .padding_xy(20, 16)
        .spacing(12);
    if crate::widgets::PAGE_HEADER_ENABLED {
        main_col = main_col.child(crate::widgets::page_header("词库", "管理词库与自定义词条"));
    }
    let main_col = main_col
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
        .child(io_target_effect)
        .child(toast_sink(mgr.feedback))
}

/// 工具栏：批量操作按钮（按 ops 可见）+ 搜索框（可搜类别）。
fn build_toolbar(mgr: &DictManagerState) -> Element {
    // 添加（dict/phrase/shadow 按类别分派）
    let m_add = mgr.clone();
    let v_add = mgr.clone();
    let add = Element::button("+ 添加")
        .small()
        .on_click(move |_| {
            if m_add.current_category() == super::spec::WordCategory::Shadow {
                m_add.open_shadow_add();
            } else {
                m_add.open_add();
            }
        })
        .visible_when(move || v_add.current_spec().ops.add);

    // 批量：全部转正（temp）/ 清空（dict/temp/freq/shadow）/ 恢复默认（phrase）
    let m_pa = mgr.clone();
    let v_pa = mgr.clone();
    let e_pa = mgr.clone();
    let promote_all = Element::button("全部转正")
        .small()
        .outline_soft()
        .on_click(move |_| m_pa.do_promote_all())
        .enabled_when(move || e_pa.total.get() > 0)
        .visible_when(move || v_pa.current_spec().ops.promote_all);

    let m_clear = mgr.clone();
    let v_clear = mgr.clone();
    let e_clear = mgr.clone();
    let clear = Element::button("清空")
        .small()
        .outline()
        .danger()
        .on_click(move |_| m_clear.request_clear())
        .enabled_when(move || e_clear.total.get() > 0)
        .visible_when(move || v_clear.current_spec().ops.clear);

    let m_reset = mgr.clone();
    let v_reset = mgr.clone();
    let reset_default = Element::button("恢复默认")
        .small()
        .outline_soft()
        .on_click(move |_| m_reset.request_reset_default())
        .visible_when(move || v_reset.current_spec().ops.reset_default);

    // 导入/导出入口已上移到子标签行（见 build_page 的 sub_tabs），工具栏不再放置。

    // 搜索（输入即搜，仅留清除按钮；分页类/非分页类均显示）
    let m_clr_s = mgr.clone();
    let e_clr_s = mgr.clone();
    let v_search = mgr.clone();
    let search = Element::row()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::text_input(mgr.search, "搜索编码或词条…")
                .width(240)
                .height(30),
        )
        .child(
            Element::button("清除")
                .small()
                .outline_soft()
                .on_click(move |_| {
                    m_clr_s.search.set(String::new());
                })
                .enabled_when(move || !e_clr_s.search.get().is_empty()),
        )
        .visible_when(move || {
            v_search.current_spec().searchable || v_search.current_spec().client_search
        });

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
                .outline_soft()
                .enabled_when(move || m_hp.has_prev())
                .on_click(move |_| m_prev.prev_page()),
        )
        .child(
            Element::button("下一页 ›")
                .small()
                .outline_soft()
                .enabled_when(move || m_hn.has_next())
                .on_click(move |_| m_next.next_page()),
        )
        .visible_when(move || dom_pg.current_spec().paged)
}

/// 构建当前类别的可排序表格（服务端排序模式）：正文绑定 `table_rows` 信号，点表头
/// 更新 `sort` 后触发 `on_sort_changed`（分页类归零分页走服务端排序、非分页类客户端重排）；
/// 行悬停高亮为表格内建。编码/类型列自定义渲染，尾部操作列按显示下标生成。
fn build_table(mgr: &DictManagerState) -> Element {
    let spec = mgr.current_spec();
    let cols: Vec<(String, f32)> = spec
        .columns
        .iter()
        .map(|c| (c.header.to_string(), c.weight))
        .collect();
    let columns = spec.columns;
    let m_cell = mgr.clone();
    let m_sort = mgr.clone();
    let m_act = mgr.clone();
    let m_dbl = mgr.clone();
    Element::table_sortable_server(cols, mgr.table_rows, mgr.sort, move |_, _| {
        m_sort.on_sort_changed()
    })
    .cell_render(move |row, col, text| render_cell(&m_cell, columns, row, col, text))
    .actions("操作", ACTIONS_WEIGHT, move |disp| {
        build_row_actions(&m_act, disp)
    })
    // 双击整行进入编辑（仅支持编辑的类别；落点在操作列按钮上不触发）。
    .on_row_activate(move |_ctx, disp| activate_row(&m_dbl, disp))
    // 系统词库等长条目最多显示两行，超出裁切省略（不再溢出到下方行）。
    .cell_lines(2)
    .fill()
}

/// 双击整行的激活动作：可编辑类别进入编辑对话框（候选调整走 shadow 编辑，其余走 open_edit）。
/// 行按显示下标从 `display_rows` 即时取（与操作列"编辑"按钮同一入口）。
fn activate_row(mgr: &DictManagerState, disp: usize) {
    let spec = mgr.current_spec();
    if !spec.ops.edit {
        return;
    }
    let Some(row) = mgr.display_row(disp) else {
        return;
    };
    if spec.ops.remove_rule {
        // 传 row.text（规则里的原始文本）而非 display_text()：那是给人看的模板原文，
        // 提交回去会把规则的 word 写成模板串。cand_id 原样带上，别把它擦掉。
        mgr.open_shadow_edit(&row.code, &row.text, &row.kind, row.position, &row.cand_id);
    } else {
        mgr.open_edit(&row.code, &row.text, row.weight, row.position);
    }
}

/// 自定义单元格：编码列渲染为边框徽章、候选调整类型列渲染为彩色标签（置顶=强调色），
/// 其余列返回 `None` 走默认文本渲染。排序仍按单元格文本。
///
/// 编码列在**遮蔽了系统条目的用户短语**上追加一枚「覆盖」标签（`overrides_system`）：
/// 那条系统短语已从系统短语列表隐去，不标注的话用户只会觉得系统短语莫名少了一条。
fn render_cell(
    mgr: &DictManagerState,
    columns: &'static [ColumnSpec],
    row: usize,
    col: usize,
    text: &str,
) -> Option<Element> {
    match columns.get(col)?.field {
        WordField::Code => {
            let badge = Element::label(text)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .padding_xy(6, 2)
                .corner(4.0)
                .bg_role(Role::Surface)
                .border_role(Role::Border, 1)
                .max_lines(1)
                // 编码过长时末尾省略（编码前缀…），不再无差别硬裁。
                .truncate(Truncate::End);
            if !mgr.display_row(row).is_some_and(|r| r.overrides_system) {
                return Some(badge);
            }
            Some(
                Element::row()
                    .cross(Align::Center)
                    .spacing(4)
                    .child(badge)
                    .child(
                        Element::label("覆盖")
                            .font_size(10.5)
                            .fg_role(Role::Accent)
                            .padding_xy(4, 1)
                            .corner(3.0)
                            .border_role(Role::Accent, 1)
                            .max_lines(1),
                    ),
            )
        }
        WordField::Kind => {
            let role = if text == "置顶" {
                Role::Accent
            } else {
                Role::TextMuted
            };
            Some(
                Element::label(text)
                    .font_size(11.0)
                    .fg_role(role)
                    .padding_xy(6, 2)
                    .corner(4.0)
                    .border_role(role, 1),
            )
        }
        _ => None,
    }
}

/// 行尾操作（按当前 spec 的 ops 增减）：启用开关 / 查看 / 编辑 / 转正 / 撤销 / 删除。
/// `disp` 为页内显示下标，行数据从 `display_rows` 取（与表格正文同源同序；
/// 数据变化必然重建正文并重新调用本生成器，下标不会陈旧）。
fn build_row_actions(mgr: &DictManagerState, disp: usize) -> Element {
    let spec = mgr.current_spec();
    let Some(row) = mgr.display_row(disp) else {
        return Element::leaf();
    };
    let mut r = Element::row().cross(Align::Center).spacing(6);

    // 启用开关（短语）：勾选即调 `phrase.setEnabled` 即时生效。
    // signal 按显示下标复用（见 `DictManagerState::enabled_sig`）——本生成器每次正文重建都会
    // 重跑，逐行新建 signal 会在 arena 里越积越多（系统短语每敲一个搜索键即全量重建）。
    if spec.ops.set_enabled {
        let sig = mgr.enabled_sig(disp, row.enabled);
        let m = mgr.clone();
        let (c, t) = (row.code.clone(), row.text.clone());
        r = r.child(Element::checkbox("", sig).small().on_toggle(move |_| {
            let v = !sig.get();
            sig.set(v);
            m.do_set_enabled(&c, &t, v);
        }));
    }

    // 只读查看（系统短语）。「转为用户短语」在该对话框内，不占操作列宽度。
    if spec.ops.view {
        let m = mgr.clone();
        let (c, t, w, pos) = (row.code.clone(), row.text.clone(), row.weight, row.position);
        r = r.child(
            Element::button("查看")
                .small()
                .outline_soft()
                .on_click(move |_| m.open_view(&c, &t, w, pos)),
        );
    }

    // 编辑（候选调整走 open_shadow_edit，用户词库/短语走 open_edit）
    if spec.ops.edit {
        let m = mgr.clone();
        if spec.ops.remove_rule {
            // Shadow 类别：预填现值打开 shadow 对话框。
            // `row.text` 是规则的原始文本（列表显示的模板原文不能回填，见 activate_row）；
            // `cand_id` 必须带上，否则提交会擦掉短语规则的稳定 id。
            let (c, t, k, pos, cid) = (
                row.code.clone(),
                row.text.clone(),
                row.kind.clone(),
                row.position,
                row.cand_id.clone(),
            );
            r = r.child(
                Element::button("编辑")
                    .small()
                    .outline_soft()
                    .on_click(move |_| m.open_shadow_edit(&c, &t, &k, pos, &cid)),
            );
        } else {
            let (c, t, w, pos) = (row.code.clone(), row.text.clone(), row.weight, row.position);
            r = r.child(
                Element::button("编辑")
                    .small()
                    .outline_soft()
                    .on_click(move |_| m.open_edit(&c, &t, w, pos)),
            );
        }
    }

    if spec.ops.promote {
        let m = mgr.clone();
        let (c, t) = (row.code.clone(), row.text.clone());
        r = r.child(
            Element::button("转正")
                .small()
                .outline_soft()
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
                .outline_soft()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::dict::spec::{LogicalTab, WordCategory};
    use crate::state::SchemaInfo;
    use serde_json::json;
    use windui::core::Tree;
    use windui::geometry::Size;

    /// 子标签分段必须覆盖 `DomainKind` 的**每个**变体。
    ///
    /// 漏掉哪个，那类方案下就一个标签都不渲染——而逻辑层 `tabs_for_domain_at` 照常给出
    /// 标签集，于是「能加词条、能导入导出，就是看不到标签」。英文方案实际这么漏过一次：
    /// `domain_kind()` 把它落到 `Other`，而分段只逐个列举了 Phrase/Pinyin/Codetable/Mixed。
    ///
    /// 枚举变体在此**逐个写死**：新增变体时这里编译不过（match 不穷尽），强制来这里
    /// 决定它挂哪一段——比「漏了也照样编译、跑起来才发现没标签」强。
    #[test]
    fn sub_tab_segments_cover_every_domain_kind() {
        let all = [
            DomainKind::Phrase,
            DomainKind::Pinyin,
            DomainKind::Codetable,
            DomainKind::Mixed,
            DomainKind::English,
            DomainKind::Other,
        ];
        // 穷尽性哨兵：新增变体会让这个 match 编译失败，逼人回到上面的数组补齐。
        for k in all {
            let _: () = match k {
                DomainKind::Phrase
                | DomainKind::Pinyin
                | DomainKind::Codetable
                | DomainKind::Mixed
                | DomainKind::English
                | DomainKind::Other => (),
            };
        }
        for k in all {
            let n = ALL_SEGMENTS.iter().filter(|s| s.contains(&k)).count();
            assert_eq!(
                n, 1,
                "{k:?} 应恰好挂在一个分段上（实际 {n} 个）——0 个 = 该类方案看不到任何标签，\
                 多于 1 个 = 同时显示两条分段"
            );
        }
    }

    /// 导入/导出对话框必须能真的搭起来并完成布局。
    ///
    /// windui 的部件约束多是**运行时 downcast**（`config_button` 那类 `debug_assert!`），
    /// 编译期一律放行——「特殊模式」节就因为给 `segmented` 加了 `.small()` 而一打开就崩，
    /// 编译和既有测试全绿。目标方案下拉与提示条是新加的部件组合，这里过一遍布局。
    #[test]
    fn io_dialogs_build_and_layout() {
        let mgr = DictManagerState::new();
        *mgr.schemas.borrow_mut() = vec![
            SchemaInfo {
                id: "wb".into(),
                name: "五笔86".into(),
                kind: "codetable".into(),
                ..Default::default()
            },
            SchemaInfo {
                id: "mx".into(),
                name: "五笔拼音混输".into(),
                kind: "mixed".into(),
                ..Default::default()
            },
        ];
        mgr.io_target_items
            .set(mgr.domain_rich_items().into_iter().skip(1).collect());
        // 混输目标：提示条非空、勾选行只剩候选调整——恰是最容易出问题的那一档。
        mgr.io_target.set(1);
        mgr.io_scope_note.set("混输方案说明".into());
        mgr.io_auto_note.set("已自动切换".into());
        mgr.import_confirm_visible.set(true);
        mgr.export_visible.set(true);

        let mut tree = Tree::new();
        let root = Element::stack()
            .width(800)
            .height(600)
            .child(crate::pages::dict::dialogs::build_dict_dialogs(&mgr))
            .build(&mut tree);
        tree.root = Some(root);
        layout(&mut tree);
    }

    /// 组装最小词库页视图（同 build_page 的表格接线：导航 effect + host_signal 表格），
    /// 复现「切换类别 → 整表重建」的真实多帧时序。
    fn table_view(mgr: &DictManagerState) -> Element {
        let dom_v = mgr.domain;
        let sub_v = mgr.sub_tab;
        let m_eff = mgr.clone();
        let effect = reactive_effect(
            move || {
                dom_v
                    .version()
                    .wrapping_mul(1_000_003)
                    .wrapping_add(sub_v.version())
            },
            move || m_eff.on_nav_changed(),
        );
        let m_tbl = mgr.clone();
        let host = Element::host_signal(mgr.table_epoch, move |_| build_table(&m_tbl));
        Element::stack()
            .width(800)
            .height(600)
            .child(host)
            .child(effect)
    }

    fn layout(tree: &mut Tree) {
        tree.layout_root(Size::new(800, 600), &mut windui::text::NullTextEngine);
    }

    /// (表头单元格数, 正文行数) —— 表格结构 col[header, divider, scroll[col(挂正文 widget)]]。
    fn table_shape(tree: &Tree) -> (usize, usize) {
        let root = tree.root.unwrap();
        let table = tree.get(root).unwrap().children[0]; // host 容器的首子 = 表格
        let table = tree.get(table).unwrap().children[0]; // host_signal 内层（重建宿主的子）
        let header = tree.get(table).unwrap().children[0];
        let scroll = *tree.get(table).unwrap().children.last().unwrap();
        let body = tree.get(scroll).unwrap().children[0];
        (
            tree.get(header).unwrap().children.len(),
            tree.get(body).unwrap().children.len(),
        )
    }

    #[test]
    fn nav_switch_and_back_rebuilds_table_with_data() {
        let mgr = DictManagerState::new();
        *mgr.schemas.borrow_mut() = vec![SchemaInfo {
            id: "wb".into(),
            name: "五笔".into(),
            kind: "codetable".into(),
            scheme: String::new(),
            hidden: false,
        }];
        // 初始类别 = 系统短语，注入 2 行。
        mgr.apply_rows(&json!([
            { "code": "rq", "text": "$date", "weight": 1000, "enabled": true, "isSystem": true },
            { "code": "dd", "text": "(^_^)", "weight": 1000, "enabled": false, "isSystem": true },
        ]));

        let mut tree = Tree::new();
        let root = table_view(&mgr).build(&mut tree);
        tree.root = Some(root);
        layout(&mut tree);
        assert_eq!(
            table_shape(&tree),
            (4, 2),
            "初始：短语 3 数据列 + 操作列，2 行"
        );
        assert!(
            mgr.current_spec().ops.reset_default,
            "系统短语应有恢复默认操作"
        );

        // 切到五笔域用户词库：effect 触发 on_nav_changed（reload 后数据到达以 apply_rows 模拟）。
        mgr.domain.set(1);
        layout(&mut tree);
        mgr.apply_rows(&json!({ "items": [
            { "code": "aaaa", "text": "工", "weight": 0 },
            { "code": "ggkg", "text": "王中", "weight": 5 },
            { "code": "wghg", "text": "程序", "weight": 3 },
        ], "total": 3 }));
        layout(&mut tree);
        assert_eq!(
            table_shape(&tree),
            (4, 3),
            "切换后：用户词库 3 数据列 + 操作列，3 行"
        );

        // 切回快捷短语（系统短语）：表格应重建回短语列集并显示数据。
        mgr.domain.set(0);
        layout(&mut tree);
        mgr.apply_rows(&json!([
            { "code": "rq", "text": "$date", "weight": 1000, "enabled": true, "isSystem": true },
            { "code": "dd", "text": "(^_^)", "weight": 1000, "enabled": false, "isSystem": true },
        ]));
        layout(&mut tree);
        assert_eq!(table_shape(&tree), (4, 2), "切回后：短语列集恢复，2 行数据");
        assert!(
            mgr.current_spec().ops.reset_default,
            "切回系统短语后恢复默认操作应可用（按钮 visible_when 据此显隐）"
        );
        assert!(mgr.empty_msg.get().is_empty(), "有数据时不应显示空态提示");
    }

    /// 命令行深链（`--page dict --schema wb --type shadow`）在挂着导航 effect 的真实
    /// 视图里跑完整多帧：定位后 effect 会在下一帧触发 `on_nav_changed`——若 `navigate_to`
    /// 没有提前对齐 `last_domain`/`sub_tab_memory`，这里会被打回用户词库的 3 列。
    #[test]
    fn deep_link_lands_on_shadow_tab_across_layout_frames() {
        let mgr = DictManagerState::new();
        *mgr.schemas.borrow_mut() = vec![SchemaInfo {
            id: "wb".into(),
            name: "五笔".into(),
            kind: "codetable".into(),
            scheme: String::new(),
            hidden: false,
        }];

        let mut tree = Tree::new();
        let root = table_view(&mgr).build(&mut tree);
        tree.root = Some(root);
        layout(&mut tree);

        assert_eq!(
            mgr.apply_deep_link("wb", Some(LogicalTab::Shadow)),
            None,
            "五笔（码表）域有候选调整，应完全命中、无降级提示"
        );
        layout(&mut tree); // effect 在这一帧跑 on_nav_changed
        mgr.apply_rows(&json!([
            { "code": "ab", "word": "阿", "type": "pin", "position": 1 },
            { "code": "cd", "word": "屏", "type": "delete" },
        ]));
        layout(&mut tree);

        assert_eq!(mgr.current_category(), WordCategory::Shadow);
        assert_eq!(
            table_shape(&tree),
            (5, 2),
            "候选调整 4 数据列（编码/词/类型/位置）+ 操作列，2 行"
        );
    }
}
