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

use crate::widgets::flat_panel;

use super::reactive::{reactive_effect, toast_sink};
use super::spec::{ColumnSpec, WordField, kind_label};
#[cfg(test)]
use super::state::{DOMAIN_PHRASE, DOMAIN_QUICK, GLOBAL_DOMAINS};
use super::state::{DictManagerState, DomainKind, tabs_for_kind};

// 操作列宽度权重由类别决定，见 `CategorySpec::actions_weight`——快捷输入的用户条目
// 最多同时有五个控件（启用 + ↑↓ + 编辑 + 删除），默认宽度装不下。

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
// 快捷输入只有一个子标签，分段本身没有切换意义，但仍要有：分段的显隐决定这一行画不画，
// 缺了它这个域就一个标签都不渲染（见 `sub_tab_segments_cover_every_domain_kind`）。
const SEG_QUICK: &[DomainKind] = &[DomainKind::QuickFormat];
// 常用字同理：单标签，分段只为让这一行画出来。
const SEG_COMMON: &[DomainKind] = &[DomainKind::CommonChars];
/// 覆盖性校验用（顺序无关），仅测试构建需要。加新分段时记得挂进来。
#[cfg(test)]
const ALL_SEGMENTS: &[&[DomainKind]] = &[
    SEG_PHRASE,
    SEG_PINYIN,
    SEG_CODETABLE,
    SEG_ENGLISH,
    SEG_MIXED,
    SEG_QUICK,
    SEG_COMMON,
];

/// 按一份 [`DictManagerState`] 构建词库页主体。
///
/// 收值而不是 `&AppState`：这一页现在只长在独立词库窗里，而 `Window::content` 收的是
/// `'static` 闭包，借用进不去。本页原本也只依赖 `dict_mgr`（它自带全部 Signal 与 Rc，
/// 是个自包含的 `Clone` 单元），传值即可。
pub fn build_page_for(mgr: DictManagerState) -> Element {
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

    // ── 响应式 effect：导入格式下拉变化 → 按新格式重新解释已选文件 ──
    let fmt_v = mgr.io_format;
    let mgr_fmt = mgr.clone();
    let io_format_effect = reactive_effect(
        move || fmt_v.version(),
        move || mgr_fmt.on_io_format_changed(),
    );

    // ── 方案子标签（按 domain_kind 动态显隐，同时只显一条）──
    //
    // 这里只决定**哪条分段在什么时候显示**；显示哪些标题由 `tabs_for_kind` 说了算。
    //
    // 显隐：每条分段挂一组适用的 DomainKind 而非单个——`Other`（未知引擎类型）也必须挂在
    // 某条上，否则那类方案下一个标签都不渲染，而逻辑层 `tabs_for_domain_at` 仍会给出标签集，
    // 于是「有功能、没入口」：能加词条、能导入导出，就是看不到标签。英文方案正是这么漏的
    // （`domain_kind()` 把它落到 `Other`，而这里只逐个列举了 Phrase/Pinyin/Codetable/Mixed）。
    // 覆盖性由 `sub_tab_segments_cover_every_domain_kind` 守护。
    //
    // 标题：**一律派生，不在这里硬编码**。同一份清单写两遍就会漏改一遍，而漏改的症状与上面
    // 那种同形、同样只在特定引擎类型下出现。同一分段内各 kind 的标签集必须一致
    //（Codetable/Other 同为四类），故取 kinds[0] 即可，该前提由
    // `segment_kinds_share_one_tab_set` 守护。
    let seg = |mgr: DictManagerState, kinds: &'static [DomainKind]| {
        let m = mgr.clone();
        let titles: Vec<String> = tabs_for_kind(kinds[0])
            .iter()
            .map(|t| t.title().to_string())
            .collect();
        // 每段固定宽、总宽随段数变化：避免 1/3/4 段被均分导致各域分段宽度不一致。
        let seg_w = titles.len() as i32 * SUB_TAB_SEG_W;
        Element::segmented(titles, mgr.sub_tab)
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
        .child(seg(mgr.clone(), SEG_PHRASE))
        .child(seg(mgr.clone(), SEG_PINYIN))
        .child(seg(mgr.clone(), SEG_CODETABLE))
        .child(seg(mgr.clone(), SEG_ENGLISH))
        .child(seg(mgr.clone(), SEG_MIXED))
        .child(seg(mgr.clone(), SEG_QUICK))
        .child(seg(mgr.clone(), SEG_COMMON))
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
            Element::label_signal(mgr.empty_msg)
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
        .child(io_format_effect)
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
            use super::spec::WordCategory as C;
            match m_add.current_category() {
                C::Shadow => m_add.open_shadow_add(),
                // 预选类别：取当前列表里第一个类别，用户可在对话框里改。
                // 不固定成 date——用户搜过/滚到某一类时，那一类才是他要加的。
                C::QuickFormat => {
                    let kind = m_add
                        .quick_kind_options()
                        .first()
                        .cloned()
                        .unwrap_or_default();
                    m_add.open_quick_add(&kind);
                }
                C::CommonChars => m_add.open_common_add(),
                _ => m_add.open_add(),
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

    // 「只看已修改」：全表 8104 条里自己动过的那几个，靠翻页是找不到的。
    // ⚠️ `Element::checkbox` 只负责显示、自己不翻转，必须配 on_toggle。
    let m_om = mgr.clone();
    let v_om = mgr.clone();
    let sig_om = mgr.only_modified;
    let only_modified = Element::checkbox("只看已修改", sig_om)
        .small()
        .on_toggle(move |_| {
            sig_om.set(!sig_om.get());
            // 与改搜索词同一条路：分页类走「归零分页 + 重载」。不归零的话，留在第 5 页
            // 而过滤后只剩 3 条，用户看到的是一张空表。
            m_om.on_search_changed();
        })
        .visible_when(move || v_om.current_category() == super::spec::WordCategory::CommonChars);

    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(0, 2)
        .child(add)
        .child(promote_all)
        .child(clear)
        .child(reset_default)
        .child(only_modified)
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
            Element::label_signal(mgr.page_status)
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
    let m_menu = mgr.clone();
    Element::table_sortable_server(cols, mgr.table_rows, mgr.sort, move |_, _| {
        m_sort.on_sort_changed()
    })
    .cell_render(move |row, col, text| render_cell(&m_cell, columns, row, col, text))
    // 宽度按类别取：表格随类别整表重建（table_epoch 驱动），故这里读到的恒是当前类别。
    .actions("操作", mgr.current_spec().actions_weight(), move |disp| {
        build_row_actions(&m_act, disp)
    })
    // 双击整行进入编辑（仅支持编辑的类别；落点在操作列按钮上不触发）。
    .on_row_activate(move |_ctx, disp| activate_row(&m_dbl, disp))
    // 右击整行：与操作列同源的菜单，外加复制与「转为用户短语」（列宽装不下的那几样）。
    .on_row_context_menu(move |disp| row_menu_items(&m_menu, disp))
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
            // 与 `kind_label` 同源比对，别写字面量：这里拿到的 text 已经是标签文本，
            // 改文案时若只改 spec 那边，徽章会静默退成灰色（位置类规则不再被强调）。
            let role = if text == kind_label("pin") {
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
        // 常用性两列渲染成徽章。**颜色这个维度只承载一件事：这一行我改过没有**。
        //
        // 「常用/生僻」由文字本身说清楚了，再让颜色也去表达它，两个信息就挤在同一个视觉
        // 通道里，谁也读不准。全表 8104 行里真正要一眼认出的是「哪几个是我动过的」，
        // 那正是强调色该留给的东西——与「只看已修改」互补：那个是筛出来，这个是扫过去
        // 也能注意到。
        //
        // 只给「当前」列上色，「默认」列恒是中性：后者是参照物，两列都亮就没有对比可言。
        WordField::CommonNow => {
            let adjusted = mgr.display_row(row).is_some_and(|r| r.adjusted);
            let role = if adjusted {
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
        WordField::CommonBase => Some(
            Element::label(text)
                .font_size(11.0)
                .fg_role(Role::TextMuted)
                .padding_xy(6, 2)
                .corner(4.0)
                .border_role(Role::TextMuted, 1),
        ),
        _ => None,
    }
}

/// 行右键菜单（按当前 spec 的 ops 增减）。与 [`build_row_actions`] 同源同判据——
/// 两处各写一套 ops 分支，加类别时必漏一边。
///
/// 比操作列多出两样，都是**列宽装不下**才落在这里的：
/// - 复制编码 / 复制词条：查完词想拿去搜一下，此前只能手抄；
/// - 复制为一键导入文本：短语两级专有（`OpFlags::share_text`），把这一条拼成 `wind:p1`
///   分发格式，对方粘进导入框即可一键装上；
/// - 「转为用户短语」：`OpFlags::claim_as_user` 的注释写着"操作列再加一枚按钮就挤爆了"，
///   于是它一直藏在只读查看对话框二层里——右键菜单没有宽度约束，正好把它提到一层。
///
/// **刻意不放"当前行"的标题项**：词条与编码都可能很长（整句短语、长编码），菜单宽度按
/// 最宽项算，一行长文本就把整个菜单撑成一大片，反而看不清有哪些操作。菜单弹在指针处、
/// 紧贴被右击的那一行，作用对象本就在眼前，不值得为标注它付出这个代价。
fn row_menu_items(mgr: &DictManagerState, disp: usize) -> Vec<MenuItem> {
    let spec = mgr.current_spec();
    let Some(row) = mgr.display_row(disp) else {
        return Vec::new();
    };
    // 行级能力：同一类别里两种行可以有不同权限（快捷输入的出厂条目不可编辑/删除）。
    // 菜单与操作列都读它，判据只此一处——两边各判一遍迟早漂移，而症状是
    // 「菜单里能点、点下去报错」。
    let rops = super::spec::row_ops(spec.category, &row);
    let text = row.display_text();
    let mut items: Vec<MenuItem> = Vec::new();

    // 操作组：**整组可能一项都不剩**（词频这类只有批量操作、无行级操作的类别），
    // 故用 `push_group` 装配——组空时连同它前面的分隔线一起省掉，不留一条孤线。
    let mut ops = Vec::new();
    if spec.ops.view {
        let m = mgr.clone();
        let (c, t, w, pos) = (row.code.clone(), row.text.clone(), row.weight, row.position);
        ops.push(MenuItem::run(
            "查看…",
            move |_ctx| m.open_view(&c, &t, w, pos),
            false,
        ));
    }
    if rops.edit {
        let m = mgr.clone();
        if spec.category == super::spec::WordCategory::QuickFormat {
            let r = row.clone();
            ops.push(MenuItem::run(
                "编辑模板…",
                move |_ctx| m.open_quick_edit(&r),
                false,
            ));
        } else if spec.category == super::spec::WordCategory::CommonChars {
            // 与操作列按钮同一动作：直接切换，不带省略号（没有后续对话框）。
            let r = row.clone();
            let label = if row.common_now {
                "设为生僻字"
            } else {
                "设为常用字"
            };
            ops.push(MenuItem::run(
                label,
                move |_ctx| m.toggle_common_row(&r),
                false,
            ));
        } else if spec.ops.remove_rule {
            // Shadow 类别：`row.text` 是规则原文（列表显示的模板原文不能回填，见 activate_row），
            // `cand_id` 必须带上，否则提交会擦掉短语规则的稳定 id。
            let (c, t, k, pos, cid) = (
                row.code.clone(),
                row.text.clone(),
                row.kind.clone(),
                row.position,
                row.cand_id.clone(),
            );
            ops.push(MenuItem::run(
                "编辑…",
                move |_ctx| m.open_shadow_edit(&c, &t, &k, pos, &cid),
                false,
            ));
        } else {
            let (c, t, w, pos) = (row.code.clone(), row.text.clone(), row.weight, row.position);
            ops.push(MenuItem::run(
                "编辑…",
                move |_ctx| m.open_edit(&c, &t, w, pos),
                false,
            ));
        }
    }
    if spec.ops.claim_as_user {
        let m = mgr.clone();
        let (c, t, w, pos) = (row.code.clone(), row.text.clone(), row.weight, row.position);
        ops.push(MenuItem::run(
            "转为用户短语",
            move |_ctx| m.claim_phrase_as_user(&c, &t, w, pos),
            false,
        ));
    }
    if spec.ops.set_enabled {
        let m = mgr.clone();
        let (r, on) = (row.clone(), row.enabled);
        // 勾选态直接读行数据：菜单每次右击重建，取到的就是当刻的值。
        // 复选框那边还要维护 `enabled_sig` 是因为它常驻在行里、要跟着点击即时变。
        ops.push(MenuItem::run(
            "启用",
            move |_ctx| m.do_set_enabled(&r, !on),
            on,
        ));
    }
    // 快捷输入的调序：置顶在菜单里（上移/下移在操作列，那里点着更顺手）。
    // 停用项不参与排序（它不在候选里），故置灰而不是让它点了没反应。
    if spec.ops.reorder {
        let m = mgr.clone();
        let r = row.clone();
        let movable = row.move_index.is_some_and(|i| i > 0);
        ops.push(
            MenuItem::run("移到本类首位", move |_ctx| m.do_quick_move_top(&r), false)
                .enabled(movable),
        );
    }
    // 出厂条目才有「默认」可回到；用户自己加的条目没有，`row_ops` 已把它算进去
    // （那一项对用户条目恒 false，故整条菜单项不出现，而不是出现后灰着）。
    if spec.ops.reset_entry {
        let m = mgr.clone();
        let r = row.clone();
        let can = rops.reset_entry;
        ops.push(
            // 按类别分派（快捷输入的主键是 (kind,id)、常用字是那个字），见 `do_reset_entry`。
            MenuItem::run("恢复此条默认", move |_ctx| m.do_reset_entry(&r), false).enabled(can),
        );
    }
    if spec.ops.promote {
        let m = mgr.clone();
        let (c, t) = (row.code.clone(), row.text.clone());
        ops.push(MenuItem::run(
            "转正",
            move |_ctx| m.do_promote(&c, &t),
            false,
        ));
    }
    if spec.ops.remove_rule {
        let m = mgr.clone();
        let (c, t) = (row.code.clone(), row.text.clone());
        ops.push(MenuItem::run(
            "撤销",
            move |_ctx| m.do_delete_row(&c, &t),
            false,
        ));
    }
    push_group(&mut items, ops);

    let (m_t, m_c) = (mgr.clone(), mgr.clone());
    let (copy_text, copy_code) = (text, row.code.clone());
    let mut copies: Vec<MenuItem> = Vec::new();
    // 「编码 → 词条」跟着表格列序走：表格第一列是编码、第二列才是词条，菜单里反过来
    // 排就得每次重新读一遍才敢点。两处顺序一致时，"第几项"这个肌肉记忆才立得住。
    //
    // 「复制编码」只在这一列真有内容时给。常用字表的键就是那个字、没有编码维度，
    // `row.code` 恒空——给了也是复制一个空串，属于点了不知道发生没发生的那类项。
    if !row.code.is_empty() {
        copies.push(MenuItem::run(
            "复制编码",
            move |_ctx| copy_to_clipboard(&m_c, &copy_code),
            false,
        ));
    }
    copies.push(MenuItem::run(
        "复制词条",
        move |_ctx| copy_to_clipboard(&m_t, &copy_text),
        false,
    ));
    if spec.ops.share_text {
        let m = mgr.clone();
        // 取 `row.text` 而不是 `display_text()`：后者是**候选调整**那类的模板投影
        // （`phrase:{code}:{模板}` 里的模板原文），短语行两者恒同，但写 display_text
        // 会让「哪天给 shadow 也开了 share_text」时静默分发出规则模板而非短语正文。
        let (c, t) = (row.code.clone(), row.text.clone());
        copies.push(MenuItem::run(
            "复制为一键导入文本",
            move |_ctx| copy_share_text(&m, &c, &t),
            false,
        ));
    }
    push_group(&mut items, copies);

    if rops.remove {
        let m = mgr.clone();
        let (c, t) = (row.code.clone(), row.text.clone());
        let quick_row =
            (spec.category == super::spec::WordCategory::QuickFormat).then(|| row.clone());
        push_group(
            &mut items,
            vec![
                MenuItem::run(
                    "删除",
                    move |_ctx| match &quick_row {
                        // 快捷输入的主键是 (kind, id)，走专用确认——通用那条只带
                        // (code, text)，kind 丢在半路，确认之后就拼不出请求了。
                        Some(r) => m.request_quick_delete(r),
                        None => m.request_delete_row(&c, &t),
                    },
                    false,
                )
                // 与操作列「删除」按钮的 danger 同语义：菜单各项长得一样，颜色是唯一
                // 能在扫读时把不可逆操作与紧邻的那组复制项拦开的信号。
                .danger(),
            ],
        );
    }
    items
}

/// 往菜单里追加一组，组前自动补分隔线；**组为空则整组连同分隔线一起省掉**。
///
/// 词频这类只有批量操作、没有任何行级操作的类别，操作组会一项不剩；无条件写下分隔线
/// 就会在菜单里留一条孤线。windui 弹出时也会兜底折叠（`normalize_separators`），
/// 但在生成侧就不产生更直接——这样"这一组有没有内容"是可单测的，不必依赖渲染层。
fn push_group(items: &mut Vec<MenuItem>, group: Vec<MenuItem>) {
    if group.is_empty() {
        return;
    }
    if !items.is_empty() {
        items.push(MenuItem::separator());
    }
    items.extend(group);
}

/// 写剪贴板并给一条 toast 回执。
///
/// 走平台单例而非 `EventCtx::clipboard_set`：菜单项动作是无参 `Fn()`，没有 `ctx`。
/// 剪贴板不同于原生文件对话框——它不起模态消息泵，在菜单项里同步调用是安全的，
/// 不必绕 `defer_blocking`（windui 自己的 toast 右键「复制内容」也是这么做的）。
fn copy_to_clipboard(mgr: &DictManagerState, s: &str) {
    use windui::core::ClipboardProvider;
    if s.is_empty() {
        return;
    }
    windui::platform::Clipboard.set_text(s);
    mgr.notify_ok("已复制");
}

/// 「复制为一键导入文本」：把这一条拼成 `wind:p1` 分发格式写进剪贴板。
///
/// 生成不出来时**报错而不是退而求其次**：`phrase_share_text` 拒绝的三种情形
/// （编码带空白 / 编码非可打印 ASCII / 正文带换行制表）都会让对方那边整行失效，
/// 而失败点在他导入的那一刻、不在这里。当场说清楚，好过让人把一串废文本发出去。
fn copy_share_text(mgr: &DictManagerState, code: &str, text: &str) {
    use windui::core::ClipboardProvider;
    let Some(s) = crate::pages::phrase_import::phrase_share_text(code, text) else {
        mgr.notify_err("这条短语的编码或内容拼不出分发文本，未复制");
        return;
    };
    windui::platform::Clipboard.set_text(&s);
    mgr.notify_ok("已复制为一键导入文本");
}

/// 行尾操作（按当前 spec 的 ops 增减）：启用开关 / 查看 / 编辑 / 转正 / 撤销 / 删除。
/// `disp` 为页内显示下标，行数据从 `display_rows` 取（与表格正文同源同序；
/// 数据变化必然重建正文并重新调用本生成器，下标不会陈旧）。
fn build_row_actions(mgr: &DictManagerState, disp: usize) -> Element {
    let spec = mgr.current_spec();
    let Some(row) = mgr.display_row(disp) else {
        return Element::leaf();
    };
    // 与右键菜单同源的行级能力（见 `row_menu_items` 里的同名变量）。
    let rops = super::spec::row_ops(spec.category, &row);
    let mut r = Element::row().cross(Align::Center).spacing(6);

    // 启用开关（短语）：勾选即调 `phrase.setEnabled` 即时生效。
    // 逐行现建 signal：本生成器每次正文重建都会重跑，而 windui 0.12 起表格正文自持
    // `SignalScope`，重建时连同旧子节点一并回收这一批构建期信号，故不再需要本仓那个
    // 按显示下标复用的手工池（它反而会在回收后留下悬垂句柄，一读即 panic）。
    if spec.ops.set_enabled {
        let sig = signal(row.enabled);
        let m = mgr.clone();
        let target = row.clone();
        r = r.child(Element::checkbox("", sig).small().on_toggle(move |_| {
            let v = !sig.get();
            sig.set(v);
            m.do_set_enabled(&target, v);
        }));
    }

    // 行内调序（快捷输入）。windui 没有拖拽重排控件，只能一对小按钮。
    //
    // 箭头用 `↑`/`↓` 而不是 `▲`/`▼`：与方案管理的调序菜单同一套字形（那边用作快捷键提示），
    // 实心三角在小号按钮里显得又黑又胖，两枚并排挤在操作列里尤其重。
    //
    // ⚠️ 传给状态层的是 `move_index`（候选内下标），不是行号——停用项占着行号却不在
    // 候选里，用行号会让「上移一位」跳过好几条。停用项 `move_index` 为 None，
    // 两个按钮一并置灰。
    if spec.ops.reorder {
        let can_up = row.move_index.is_some_and(|i| i > 0);
        // 下移不设上界：越界由 core 侧 clamp 到末尾（条目数会因停用而变，UI 这边
        // 算不准「最后一条是谁」，硬算反而会把最后一行的按钮错误地留成可点）。
        let can_down = row.move_index.is_some();
        let (mu, md) = (mgr.clone(), mgr.clone());
        let (ru, rd) = (row.clone(), row.clone());
        r = r
            .child(
                Element::button("↑")
                    .small()
                    .outline_soft()
                    .enabled(can_up)
                    .on_click(move |_| mu.do_quick_move(&ru, -1)),
            )
            .child(
                Element::button("↓")
                    .small()
                    .outline_soft()
                    .enabled(can_down)
                    .on_click(move |_| md.do_quick_move(&rd, 1)),
            );
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

    // 编辑（快捷输入走 open_quick_edit，候选调整走 open_shadow_edit，其余走 open_edit）
    if rops.edit {
        let m = mgr.clone();
        if spec.category == super::spec::WordCategory::QuickFormat {
            let row_q = row.clone();
            r = r.child(
                Element::button("编辑")
                    .small()
                    .outline_soft()
                    .on_click(move |_| m.open_quick_edit(&row_q)),
            );
        } else if spec.category == super::spec::WordCategory::CommonChars {
            // **直接切换，不弹对话框**：这一类只有一个二值属性，为改一个 bool 开一层
            // 模态、再点确定，比它本身的操作还重。按钮文案说的是「点下去会变成什么」。
            let row_c = row.clone();
            let label = if row.common_now {
                "设为生僻"
            } else {
                "设为常用"
            };
            r = r.child(
                Element::button(label)
                    .small()
                    .outline_soft()
                    .on_click(move |_| m.toggle_common_row(&row_c)),
            );
        } else if spec.ops.remove_rule {
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
    //
    // ⚠️ 判据用 `rops`（行级）而不是 `spec.ops`（类别级）：快捷输入的出厂条目不可删，
    // 只能停用。用类别级判据会在出厂条目上也摆出删除按钮，而它按下去只会收到一句
    // core 的报错——用户看到的是「有这个按钮，但点了就出错」。
    //
    // 主键也要按类别分流：快捷输入是 `(kind, id)`，通用路径发的是 `(code, text)`，
    // 发错的症状正是 core 回一句「缺少参数 kind」。这与右键菜单是同一份判据的两个
    // 落点，改一处漏一处就会不一致（`quick_format_factory_row_offers_no_delete`
    // 同时钉住两侧）。
    if rops.remove {
        let m = mgr.clone();
        let (c, t) = (row.code.clone(), row.text.clone());
        let quick_row =
            (spec.category == super::spec::WordCategory::QuickFormat).then(|| row.clone());
        r = r.child(
            Element::button("删除")
                .small()
                .danger()
                .on_click(move |_| match &quick_row {
                    Some(rq) => m.request_quick_delete(rq),
                    None => m.request_delete_row(&c, &t),
                }),
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
            DomainKind::QuickFormat,
            DomainKind::CommonChars,
        ];
        // 穷尽性哨兵：新增变体会让这个 match 编译失败，逼人回到上面的数组补齐。
        for k in all {
            let _: () = match k {
                DomainKind::Phrase
                | DomainKind::Pinyin
                | DomainKind::Codetable
                | DomainKind::Mixed
                | DomainKind::English
                | DomainKind::Other
                | DomainKind::QuickFormat
                | DomainKind::CommonChars => (),
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

    /// 渲染取 `kinds[0]` 的标签集当作整条分段的标题，故同一分段内各 kind 的标签集必须一致。
    ///
    /// 不成立时的表现极隐蔽：分段按 `kinds` 显隐、标题却只按第一个 kind 生成，于是某类方案
    /// 显示的是**另一类方案的标签**——点进去还能正常加载数据（数据按 `current_category` 走），
    /// 只是标签名对不上。目前唯一的多 kind 分段是 Codetable + Other（未知从宽，同为四类）。
    #[test]
    fn segment_kinds_share_one_tab_set() {
        for seg in ALL_SEGMENTS {
            let head = tabs_for_kind(seg[0]);
            for k in &seg[1..] {
                assert_eq!(
                    tabs_for_kind(*k),
                    head,
                    "{k:?} 与同分段的 {:?} 标签集不一致——渲染只取首个 kind 的标签集当标题",
                    seg[0]
                );
            }
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
        // 走真实构造，不在夹具里另抄一份 skip —— 抄一份就等于把偏移 bug 一起复制进测试。
        mgr.io_target_items.set(mgr.io_target_item_list());
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

    fn menu_labels(items: &[MenuItem]) -> Vec<String> {
        items
            .iter()
            .filter(|i| !i.separator)
            .map(|i| i.label.clone())
            .collect()
    }

    /// 菜单里**不出现行内容本身**（词条 / 编码）。
    ///
    /// ★ 菜单宽度按最宽项算：整句短语或长编码放进去，一行长文本就把整个菜单撑成一大片，
    /// 操作反而看不清。曾用它当"作用对象"的标题，但菜单本就弹在指针处、紧贴被右击那行，
    /// 标注的收益抵不过这个代价。判据查**任何项**都不含行内容，而不只是首项——
    /// 换个位置塞回去同样会撑宽。
    #[test]
    fn row_menu_never_echoes_row_content() {
        let mgr = DictManagerState::new();
        let long = "这是一条很长很长的短语内容会把菜单撑得很宽";
        mgr.apply_rows(&json!([
            { "code": "averylongcodehere", "text": long, "weight": 1000, "enabled": true, "isSystem": true },
        ]));
        for it in row_menu_items(&mgr, 0) {
            assert!(
                !it.label.contains(long) && !it.label.contains("averylongcodehere"),
                "菜单项「{}」回显了行内容，会把菜单撑宽",
                it.label
            );
            assert!(
                it.subtitle.is_none(),
                "副标题同样会撑宽（且让行高变高），项「{}」不该有",
                it.label
            );
        }
    }

    /// 系统短语：只读类别。菜单给「查看/启用/复制」，并把藏在查看对话框二层的
    /// 「转为用户短语」提到一层（`OpFlags::claim_as_user` 的注释说明它是因操作列
    /// 挤不下才下沉的，右键菜单没有宽度约束）。
    #[test]
    fn system_phrase_row_menu_surfaces_claim_as_user() {
        let mgr = DictManagerState::new();
        mgr.apply_rows(&json!([
            { "code": "rq", "text": "$date", "weight": 1000, "enabled": true, "isSystem": true },
        ]));
        let items = row_menu_items(&mgr, 0);
        assert_eq!(
            menu_labels(&items),
            [
                "查看…",
                "转为用户短语",
                "启用",
                "复制编码",
                "复制词条",
                "复制为一键导入文本"
            ]
        );
        assert!(items.iter().find(|i| i.label == "启用").unwrap().checked);
        assert!(
            !items.iter().any(|i| i.label == "删除"),
            "系统短语不可删除，菜单里不该出现"
        );
    }

    /// 「复制为一键导入文本」是**短语两级专有**（`OpFlags::share_text`）。
    ///
    /// ★ 判据挂在 ops 上而不是写成 `category == PhraseUser`：`wind:p1` 只承载
    /// `code` + `text` 二元组，用户词库的 weight、词频的 count、候选调整的 candId
    /// 它一个都表达不了。给这些类别开等于生成一串**看着像、装进去缺一半**的文本，
    /// 而缺的那一半在对方导入成功之后才显形（词序不对、规则失效），无从归因。
    #[test]
    fn share_as_import_text_is_phrase_only() {
        const LABEL: &str = "复制为一键导入文本";

        // 用户短语：有
        let mgr = DictManagerState::new();
        mgr.sub_tab.set(1);
        assert_eq!(
            mgr.current_spec().category,
            WordCategory::PhraseUser,
            "前提：域 0 的第二个子标签是用户短语"
        );
        mgr.apply_rows(&json!({ "items": [
            { "code": "kx", "text": "(＾▽＾)", "weight": 1000, "enabled": true },
        ], "total": 1 }));
        assert!(
            menu_labels(&row_menu_items(&mgr, 0))
                .iter()
                .any(|l| l == LABEL),
            "用户短语该有分发入口"
        );

        // 用户词库：没有
        let mgr = DictManagerState::new();
        *mgr.schemas.borrow_mut() = vec![SchemaInfo {
            id: "wb".into(),
            name: "五笔".into(),
            kind: "codetable".into(),
            ..Default::default()
        }];
        mgr.domain.set(GLOBAL_DOMAINS);
        mgr.on_nav_changed();
        mgr.apply_rows(&json!({ "items": [
            { "code": "aaaa", "text": "工", "weight": 0 },
        ], "total": 1 }));
        assert_eq!(mgr.current_spec().category, WordCategory::UserDict, "前提");
        assert!(
            !menu_labels(&row_menu_items(&mgr, 0))
                .iter()
                .any(|l| l == LABEL),
            "★ 用户词库的 weight 分发格式装不下，不该有这个入口"
        );
    }

    /// 用户词库：可编辑可删除、无查看/转正/启用开关。菜单必须与操作列同一套 ops 判据
    /// ——两处各写一份分支，加类别时必漏一边。
    #[test]
    fn user_dict_row_menu_matches_the_action_column_ops() {
        let mgr = DictManagerState::new();
        *mgr.schemas.borrow_mut() = vec![SchemaInfo {
            id: "wb".into(),
            name: "五笔".into(),
            kind: "codetable".into(),
            ..Default::default()
        }];
        mgr.domain.set(GLOBAL_DOMAINS);
        mgr.on_nav_changed();
        mgr.apply_rows(&json!({ "items": [
            { "code": "aaaa", "text": "工", "weight": 0 },
        ], "total": 1 }));
        let spec = mgr.current_spec();
        assert!(
            spec.ops.edit && spec.ops.remove,
            "前提：用户词库可编辑可删除"
        );

        let items = row_menu_items(&mgr, 0);
        assert_eq!(
            menu_labels(&items),
            ["编辑…", "复制编码", "复制词条", "删除"]
        );
    }

    /// ★★ 快捷输入：**出厂条目不可删、不可改模板**，用户自己加的才可以。
    ///
    /// 这是本页第一个「同类别里两种行权限不同」的域，判据在 `spec::row_ops` 一处，
    /// 菜单与操作列都读它。曾经操作列漏读、仍用类别级 `spec.ops.remove`，于是出厂条目上
    /// 也摆出了删除按钮，按下去收到 core 一句「缺少参数 kind」（通用删除路径发的是
    /// `(code, text)`，而快捷输入的主键是 `(kind, id)`）——两个症状同一个根因。
    #[test]
    fn quick_format_factory_row_offers_no_delete() {
        let mgr = DictManagerState::new();
        mgr.domain.set(DOMAIN_QUICK);
        mgr.on_nav_changed();
        mgr.apply_rows(&json!([
            { "kind": "date", "id": "date.cn", "text": "$Y年", "displayPos": 1,
              "moveIndex": 0, "enabled": true, "adjusted": true, "user": false, "sample": "2026年" },
            { "kind": "date", "id": "date.u1", "text": "$Y/$M", "displayPos": 2,
              "moveIndex": 1, "enabled": true, "adjusted": false, "user": true, "sample": "2026/8" }
        ]));

        // 第 0 行：出厂条目——能停用、能恢复默认，但不能删、不能改模板
        let factory = menu_labels(&row_menu_items(&mgr, 0));
        assert!(
            !factory.iter().any(|l| l == "删除"),
            "★ 出厂条目不该有删除入口: {factory:?}"
        );
        assert!(
            !factory.iter().any(|l| l == "编辑模板…"),
            "★ 出厂条目的模板不可改: {factory:?}"
        );
        assert!(
            factory.iter().any(|l| l == "启用"),
            "但可以停用: {factory:?}"
        );
        assert!(
            factory.iter().any(|l| l == "恢复此条默认"),
            "调整过的出厂条目可恢复: {factory:?}"
        );

        // 第 1 行：用户条目——可删、可改模板；「恢复此条默认」对它无意义，应禁用
        let mine = row_menu_items(&mgr, 1);
        let labels = menu_labels(&mine);
        assert!(
            labels.iter().any(|l| l == "删除"),
            "用户条目可删: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l == "编辑模板…"),
            "用户条目可改模板: {labels:?}"
        );
        assert!(
            !mine
                .iter()
                .find(|i| i.label == "恢复此条默认")
                .map(|i| i.enabled)
                .unwrap_or(false),
            "用户条目没有「默认」可回到，该项应禁用"
        );
    }

    /// 没有任何行级操作的类别（词频只有批量清空），菜单不该因为"操作组为空"而留下孤立分隔线。
    ///
    /// ★ 症状很轻微、很容易漏：菜单里凭空多一条线，功能全对。判据直接查连续/首尾分隔线，
    /// 而不是比对某个类别的期望列表——后者只锁住写测试那天的那一个类别。
    #[test]
    fn empty_op_group_leaves_no_stray_separator() {
        let mgr = DictManagerState::new();
        *mgr.schemas.borrow_mut() = vec![SchemaInfo {
            id: "wb".into(),
            name: "五笔".into(),
            kind: "codetable".into(),
            ..Default::default()
        }];
        mgr.domain.set(GLOBAL_DOMAINS);
        // 逐个走一遍该域的所有子标签，任何一类都不许出现孤线。
        let mut saw_empty_group = 0;
        for (i, tab) in mgr.tabs_for_domain().into_iter().enumerate() {
            mgr.sub_tab.set(i);
            mgr.on_nav_changed();
            mgr.apply_rows(&json!({ "items": [
                { "code": "aaaa", "text": "工", "weight": 1, "count": 1 },
            ], "total": 1 }));
            let spec = mgr.current_spec();
            if !(spec.ops.view
                || spec.ops.edit
                || spec.ops.claim_as_user
                || spec.ops.set_enabled
                || spec.ops.promote
                || spec.ops.remove_rule)
            {
                saw_empty_group += 1;
            }
            let items = row_menu_items(&mgr, 0);
            if items.is_empty() {
                continue;
            }
            assert!(
                !items[0].separator && !items[items.len() - 1].separator,
                "{tab:?}：菜单首尾不该是分隔线"
            );
            for w in items.windows(2) {
                assert!(
                    !(w[0].separator && w[1].separator),
                    "{tab:?}：出现连续分隔线，说明某一组为空却仍写下了分隔线"
                );
            }
        }
        // 自证有效：本轮必须真的遇到过"操作组为空"的类别，否则上面的断言只是空转。
        assert!(
            saw_empty_group > 0,
            "该域下没有任何「无行级操作」的类别，这条测试没测到要防的情况——\
             ops 配置若有调整，换一个仍有空操作组的数据域"
        );
    }

    /// 回归守护：表格正文反复重建，活跃 signal 槽位不增长。
    ///
    /// 行内「启用」开关的 signal 由 `build_row_actions` **逐行现建**，回收全靠 windui 表格
    /// 自持的 `SignalScope`（0.12 起，见其 CHANGELOG「重建子树时构建期信号永久累积」）。
    /// 本仓曾为绕开旧版「arena 只增不减」写过一个按显示下标复用的手工池，上游修掉根因后
    /// 那个池反而会在回收后留下悬垂句柄。这条断言同时锁两端：上游若丢掉回收、或本仓再退回
    /// 池化，系统短语每敲一个搜索键泄漏一批槽位的老问题就会立刻现形。
    #[test]
    fn table_rebuild_does_not_leak_signals() {
        let mgr = DictManagerState::new();
        let rows = |n: i64| {
            json!([
                { "code": "rq", "text": "$date", "weight": n, "enabled": true, "isSystem": true },
                { "code": "dd", "text": "(^_^)", "weight": n, "enabled": false, "isSystem": true },
            ])
        };
        mgr.apply_rows(&rows(1000));
        let mut tree = Tree::new();
        let root = table_view(&mgr).build(&mut tree);
        tree.root = Some(root);
        layout(&mut tree);
        assert_eq!(table_shape(&tree).1, 2, "前提：正文有两行");

        // 首轮重建后取基线：建树那一轮还含着不属于行的一次性信号。
        mgr.apply_rows(&rows(1));
        layout(&mut tree);
        let base = windui::signal::stats().live;
        for i in 2..22 {
            mgr.apply_rows(&rows(i));
            layout(&mut tree);
        }
        let after = windui::signal::stats().live;
        assert_eq!(
            after,
            base,
            "20 轮正文重建后活跃 signal 应回到基线（泄漏 {} 个）",
            after as i64 - base as i64
        );
    }

    /// 越界下标（数据刚被换掉、菜单构建器拿到旧下标）返回空菜单而不是 panic。
    /// 空 `Vec` 在框架侧就是"不弹"，正是想要的行为。
    #[test]
    fn row_menu_on_a_stale_index_yields_nothing() {
        let mgr = DictManagerState::new();
        assert!(row_menu_items(&mgr, 7).is_empty());
    }

    /// 接线守护：在**真实表格**上派发右键，菜单要真的弹出来。
    ///
    /// 上面几条只验证菜单内容——`.on_row_context_menu` 忘了接、或接到了错误的节点
    /// （表格结构是 col[header, divider, scroll[body]]，挂错一层就没反应），它们照样全绿。
    #[test]
    fn right_clicking_a_table_row_opens_the_menu() {
        use windui::event::{MouseButton, PointerEvent, PointerKind};
        use windui::geometry::Point;

        let mgr = DictManagerState::new();
        mgr.apply_rows(&json!([
            { "code": "rq", "text": "$date", "weight": 1000, "enabled": true, "isSystem": true },
            { "code": "dd", "text": "(^_^)", "weight": 1000, "enabled": false, "isSystem": true },
        ]));
        let mut tree = Tree::new();
        let root = table_view(&mgr).build(&mut tree);
        tree.root = Some(root);
        layout(&mut tree);
        assert_eq!(table_shape(&tree).1, 2, "前提：正文有两行");

        // 表头约 38px 高，首行落在其下。x=100 在编码列内。
        let (mut hover, mut capture) = (None, None);
        let res = tree.dispatch_pointer(
            PointerEvent::single(PointerKind::Down, Point::new(100, 60), MouseButton::Right),
            &mut hover,
            &mut capture,
        );
        let menu = res.menu.expect("右击正文行应弹出行菜单");
        assert!(
            menu.items.iter().any(|i| i.label == "复制编码"),
            "弹出的应是词库行菜单，实际：{:?}",
            menu_labels(&menu.items)
        );
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
        mgr.domain.set(GLOBAL_DOMAINS);
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
        mgr.domain.set(DOMAIN_PHRASE);
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
