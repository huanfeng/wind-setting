//! 设置页 — 清单驱动（manifest 的 group → Tab、section → 卡片、item → 行）+ 自定义页。
//!
//! 主体一律由 `system.manifest` 的 `(group, section)` 聚合渲染：同一 section 的项归到
//! 一张卡片，卡片次序为 section 首次出现序，行序为 manifest 原始序——key 永远与 core 一致，
//! core 加字段自动出现，彻底消除硬编码 key 漂移。复杂页（方案管理/词库/统计/关于）手写。

use std::cell::RefCell;
use std::rc::Rc;

use windui::core::EventCtx;
use windui::prelude::*;

pub mod backup;
pub mod compat;
pub mod config_import;
pub mod dict;
pub mod integration;
pub mod memory;
pub mod phrase_import;
pub mod stats;

use crate::dialogs::schema_manager::build_schema_inline_list;
use crate::manifest::{scheme_config_section, section_row_items, section_rows};
use crate::state::AppState;
use crate::subnav::SubNav;
use crate::widgets::{page, section, section_items, setting_row};

/// 一个 Tab 的三样东西：标题、页面内容、以及这一页的二级导航（见 [`crate::subnav`]）。
pub struct TabDef {
    pub name: String,
    pub page: Element,
    pub subnav: SubNav,
}

/// 全部 Tab。manifest 分组负责主体，自定义页补充方案管理/词库/统计/关于。
///
/// 每页各自持有一个 [`SubNav`]：页面构建时逐张卡片登记进去，`shell` 再据此建二级导航栏。
/// 下标必须与 `cli::PAGES` 一一对齐——`SubNav::new` 收的就是这个下标，错位的话解析器会
/// 认成别的页、二级导航整条不刷新。
pub fn all_tabs(app: &Rc<RefCell<AppState>>, state: &AppState) -> Vec<TabDef> {
    let tab = |id: &str| SubNav::new(crate::cli::page_to_index(id).unwrap_or(0));
    let (schema, input, keys, ui, dict, stats_nav, advanced, about) = (
        tab("schema"),
        tab("input"),
        tab("keys"),
        tab("ui"),
        tab("dict"),
        tab("stats"),
        tab("advanced"),
        tab("about"),
    );
    vec![
        tab_def("方案", scheme_page(app, state, &schema), schema),
        tab_def("输入", input_page(state, &input), input),
        tab_def("按键", keys_page(state, &keys), keys),
        tab_def("外观", appearance_page(app, state, &ui), ui),
        tab_def("统计", stats::build_page(state, &stats_nav), stats_nav),
        // 「词库」是动作项（点了开独立窗、主窗不切页），故排在纯切页的那五格之后。
        // ⚠ 顺序必须与 `cli::PAGES` 逐格一致，见那里的说明。
        tab_def("词库", dict_page(state), dict),
        tab_def("高级", advanced_page(app, state, &advanced), advanced),
        tab_def("关于", about_page(app, state), about),
    ]
}

/// 组装一个 [`TabDef`]，并在此处打一个内存采样点。
///
/// 采样点落在这里是因为**实参先于调用求值**：走到函数体时，这一页的整棵控件树刚好
/// 建完、还没建下一页，于是相邻两个采样点之差就是该页控件树的净堆占用。
/// `memstat` feature 关闭时 [`crate::memstat::mark`] 是空实现，不留指令。
fn tab_def(name: &'static str, page: Element, subnav: SubNav) -> TabDef {
    crate::memstat::mark(name);
    TabDef {
        name: name.into(),
        page,
        subnav,
    }
}

/// 由若干 section 卡片组成一页（页头 + 纵向滚动，深色 bg 衬托卡片）。
fn sections_page(title: &str, desc: &str, sections: Vec<Element>) -> Element {
    let mut col = Element::col().width_match().spacing(18);
    if crate::widgets::PAGE_HEADER_ENABLED {
        col = col.child(crate::widgets::page_header(title, desc));
    }
    for s in sections {
        col = col.child(s);
    }
    page(col)
}

/// 由 manifest 某分组生成若干 section 卡片（每 section 一张卡片，行按 manifest 顺序），
/// 顺带把每张卡登记进本页的二级导航。
fn manifest_sections(state: &AppState, group: &str, nav: &SubNav) -> Vec<Element> {
    state
        .manifest
        .sections_of(group)
        .into_iter()
        .map(|(sec, keys)| {
            nav.wrap(
                &sec,
                0,
                section_items(&sec, section_row_items(state, &keys)),
            )
        })
        .collect()
}

/// 高级页：manifest「advanced」分组 + 桌面集成 + 导入与导出 + 备份与还原卡片。
/// （原通用 manifest_page 仅剩本页使用，逻辑并入此处。）
///
/// 桌面集成排在备份之前：协议关联属于"这台机器上装好了没"，与备份这种一次性动作
/// 相比更接近状态展示，放前面用户切到本页就能看到关联是否还在。
fn advanced_page(app: &Rc<RefCell<AppState>>, state: &AppState, nav: &SubNav) -> Element {
    // ★ 本页的分区顺序**由本函数决定**，清单里的先后管不了：manifest 分区与三张
    // 硬编码卡片（集成 / 导入 / 备份）是两批来源，清单排到最后也只是"manifest 那批
    // 里的最后"，仍落在三张卡之前。故两个 manifest 分区都在这里显式定位：
    //
    //   系统集成 → 导入 → 备份与还原 → 应用兼容性 → 性能与诊断 → 游戏兼容 → 日志设置
    //
    // 前三张是「这台机器上装好了没 / 搬进搬出」，属于状态与一次性动作；性能与诊断（常驻开关 +
    // 内存占用摘要）是取舍与自查；游戏兼容是少数人才开的兼容开关；日志设置只在排查时才动，压轴。
    // BTreeMap 而非 HashMap：下面的"剩余分区"兜底会遍历它，HashMap 的顺序每次运行
    // 都不同，页面会莫名其妙地换序。
    let mut manifest_secs: std::collections::BTreeMap<String, Element> = state
        .manifest
        .sections_of("advanced")
        .into_iter()
        .map(|(sec, keys)| {
            // 走 split 版：日志设置分区里的隐私示警条平时不占位，混进 rows 会在它
            // 隐藏时留下一条紧贴卡片上边框的分隔线（见 widgets::card_with_attachments）。
            let (mut rows, head) = crate::manifest::section_rows_split(state, &keys);
            // 性能与诊断：清单里的开关之后接手写的「内存占用」诊断行，同一张卡片。
            // 懒加载哨兵挂在卡片外：混进 rows 会多出一条分隔线（见 widgets::card_with_attachment）。
            let mut loader = None;
            if sec == memory::SECTION && !state.manifest.feature_hidden("memory") {
                let (row, l) = memory::build_rows(state);
                rows.push(row);
                loader = Some(l);
            }
            let card = crate::widgets::section_with_head(&sec, head, rows);
            let el = match loader {
                Some(l) => Element::col().width_match().child(card).child(l),
                None => card,
            };
            (sec.clone(), el)
        })
        .collect();

    // 四张手写卡各有一个 `feature:` id，被定制版藏掉的不建（二级导航随之不登记）。
    let shown = |id: &str| !state.manifest.feature_hidden(id);
    let mut secs: Vec<Element> = Vec::new();
    if shown("integration") {
        // 搜索锚点：桌面集成/协议关联/文件关联三条都落在集成卡上。
        secs.push(nav.wrap(
            "系统集成",
            0,
            crate::search::anchor::wrap(
                integration::build_integration_section(app, state),
                "advanced:integration",
                state.jump,
            ),
        ));
    }
    if shown("config_import") {
        // 导航名与卡片标题必须一致，否则用户点了「导入与导出」滚到一张写着「导入」的卡上，
        // 会以为跳错了地方。这张卡的标题就是「导入」（`build_import_export_section`）。
        secs.push(nav.wrap(
            "导入",
            0,
            crate::search::anchor::wrap(
                config_import::build_import_export_section(
                    &state.config_import,
                    &state.schema_mgr,
                    config_import::ThemePkgConfirmSignals::of(state),
                ),
                "advanced:import-export",
                state.jump,
            ),
        ));
    }
    if shown("backup") {
        secs.push(nav.wrap(
            "备份与还原",
            0,
            crate::search::anchor::wrap(
                backup::build_backup_section(&state.backup_mgr),
                "advanced:backup",
                state.jump,
            ),
        ));
    }
    if shown("compat") {
        // 应用兼容：入口卡片，界面本身在独立窗里（规则多、三栏，放不进一页）。
        // 排在「游戏兼容」上方——两者都是「个别软件的兼容处理」，前者按软件、后者按注册表。
        secs.push(nav.wrap(
            "应用兼容性",
            0,
            crate::search::anchor::wrap(compat_entry_section(app), "advanced:compat", state.jump),
        ));
    }
    // 「性能与诊断」的清单项（常驻所有方案）被定制版藏掉时，`sections_of` 会把整个分区剔掉，
    // 可手写的「内存占用 / 诊断信息」行不是设置项、不该跟着消失（搜索里也还有它们的条目）。
    // 两者都藏了就整张卡不出。
    if shown("memory") && !manifest_secs.contains_key(memory::SECTION) {
        let (row, loader) = memory::build_rows(state);
        let card = crate::widgets::section_with_head(memory::SECTION, None, vec![row]);
        manifest_secs.insert(
            memory::SECTION.to_string(),
            Element::col().width_match().child(card).child(loader),
        );
    }
    if let Some(card) = custom_hidden_section(app) {
        secs.push(nav.wrap("定制版", 0, card));
    }
    // 取不到就跳过而不是报错：分区可能因平台/变体门控整片消失（清单项全 hidden），
    // 那是正常状态，不是配置错误。
    for name in [memory::SECTION, "游戏兼容", "日志设置"] {
        if let Some(el) = manifest_secs.remove(name) {
            secs.push(nav.wrap(name, 0, el));
        }
    }
    // 剩下的（将来新加、还没在上面排过序的 manifest 分区）接在后面，免得静默丢失。
    // 顺序不定，加新分区时请到上面那张表里点名。
    for (name, el) in std::mem::take(&mut manifest_secs) {
        secs.push(nav.wrap(&name, 0, el));
    }
    sections_page("高级", "服务与实验性功能", secs)
}

/// 「定制版」卡：一个「显示定制版隐藏的设置」开关。只在本机定制版的 `[settings] hide`
/// 非空时出现——非定制版、或定制版什么都没藏，这个开关没有意义。
///
/// 隐藏只影响显示、功能照常生效，开关是给用户找回入口用的，不改任何配置。清单折叠发生在
/// 页面树构建时，故切换后**重新打开设置窗口**才生效，说明里与 toast 都写明。
fn custom_hidden_section(app: &Rc<RefCell<AppState>>) -> Option<Element> {
    if crate::custom_hide::installed().is_empty() {
        return None;
    }
    let on = signal(app.borrow().setting_config.ui.show_custom_hidden_settings);
    let app = app.clone();
    let sw = Element::switch(on).on_switch_change(move |_ctx, v| {
        app.borrow_mut().save_show_custom_hidden_settings(v);
        app.borrow().feedback.set(Some((
            ToastKind::Info,
            "重新打开设置窗口后生效".to_string(),
        )));
    });
    Some(section(
        "定制版",
        vec![setting_row(
            "显示定制版隐藏的设置",
            Some(
                "本定制版精简了设置界面，被隐藏的设置照常生效。打开后重新打开设置窗口即可看到全部设置。",
            ),
            sw,
        )],
    ))
}

/// 「应用兼容」入口卡：一行说明 + 打开独立窗的按钮。
fn compat_entry_section(app: &Rc<RefCell<AppState>>) -> Element {
    let app = app.clone();
    section(
        "应用兼容性",
        vec![setting_row(
            "应用兼容性与独立配置",
            Some(
                "针对个别软件调整候选窗定位、位置探测、上屏换行、初始中英文状态等，\
                 解决飞书、微信、Excel、WPS 等软件里候选窗跳动、错位、换行异常的问题。\
                 出厂已内置常见软件的规则，可在这里修改、禁用、还原，也可新增、导入、导出。",
            ),
            Element::button("打开配置窗口…")
                .small()
                .on_click(move |ctx| crate::compat_window::open(ctx, &app)),
        )],
    )
}

// ───────────────────────── 输入 ─────────────────────────

/// 按键冲突固定顶栏：`state.conflict` 非空时在内容区顶部显示琥珀警告（文案已含 ⚠ 前缀，
/// 可能多行——触发键/候选键各一行）。**放在页面滚动区之外**，不随页面滚动、切页常驻，
/// 使下方配置项滚动后仍可见。无冲突时 `visible_when` 隐藏且不占布局。
pub fn conflict_bar(conflict: Signal<String>) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 10)
        // 冲突条是"警告"语义：windui 0.12 补上 `Role::Warning` 后不必再硬编码琥珀色——
        // 硬编码的深琥珀字（0xB45309）在暗色主题下压在同一片 14% 底上几乎读不出来，
        // 而语义色两套主题各有取值、且库以单测锁住对表面 ≥ 3:1。
        .bg_role_alpha(Role::Warning, 0.14)
        .border_role(Role::Divider, 1)
        .child(
            Element::label_signal(conflict)
                .font_size(12.5)
                .font_weight(500)
                .fg_role(Role::Warning)
                .weight(1.0)
                // 三域各一行，且单域多冲突会折行——放宽上限避免静默截断藏冲突。
                .max_lines(12),
        )
        .visible_when(move || !conflict.get().is_empty())
}

/// 配置降级固定顶栏：某一段配置解析失败、已整段回落出厂默认时常驻提示。空串 = 隐藏不占位。
///
/// # 为什么必须有它
///
/// 降级发生时，用户在界面上看到的只是「我的按键设置怎么变回默认了」，而唯一的线索埋在
/// 日志文件里。`config.degradation` 这个 RPC 早就有了，设置端此前一行都没消费。
///
/// # 为什么用 Danger 而不是与 [`conflict_bar`] 同样的 Warning
///
/// 按键冲突是「这两个功能会互相影响」，配置还在正常工作；降级是「你写的配置**没有生效**」，
/// 而且用户不去修配置文件就一直不生效。两条同色会让人分不出哪条需要立刻动手。
pub fn degradation_bar(degradation: Signal<String>) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 10)
        .bg_role_alpha(Role::Danger, 0.14)
        .border_role(Role::Divider, 1)
        .child(
            Element::label_signal(degradation)
                .font_size(12.5)
                .font_weight(500)
                .fg_role(Role::Danger)
                .weight(1.0)
                // 多段同时降级时逐段列出，放宽上限避免把段名截掉——那正是用户要去修的地方。
                .max_lines(8),
        )
        .visible_when(move || !degradation.get().is_empty())
}

/// 冲突实时监听：每个渲染帧跑一次 `live_conflicts`（触发键 + 候选键两域），仅当结果变化才写回
/// `state.conflict`（避免无谓重绘）。返回挂在 shell 根的 0×0 不可见叶子。
///
/// 无统一「配置改动」版本源，故用每次递增的 tick 驱动 `reactive_effect` 每次求值必触发。
/// 求值时机（非 60fps 常态）：`reactive_effect` 由 `dispatch_reactive_updates` 派发，后者只在
/// `layout_root` 内跑，而 render 由 WM_PAINT/WM_TIMER 驱动、空闲零渲染（见 windui
/// `platform/win32/d2d.rs` 的空闲零 CPU 设计）。即：交互（点击/按键）与动画补间期间求值，
/// 空闲完全不跑。
/// 性能：每次 probe 为 clone 配置 + 跑写回器（约 30-60KB 临时分配，无常驻增长）；
/// 后续可优化为仅相关 Signal 变化时重算。
pub fn conflict_watcher(app: &Rc<RefCell<AppState>>) -> Element {
    let conflict = app.borrow().conflict;
    let feedback = app.borrow().feedback;
    let eff_app = app.clone();
    let tick = std::cell::Cell::new(0u64);
    // 上一轮的共存指纹集合。`None` = 还没跑过第一轮。
    //
    // ⚠ 首轮只建基线、**不弹 Toast**：打开设置页时配置里本来就有的共存组合不是「刚发生的事」，
    // 每次开窗都弹一遍只会让人学会忽略它。Toast 只在集合**新增**成员时弹——即用户刚刚
    // 亲手配出这个组合的那一刻，此时他正需要知道这两个功能将如何分时。
    let seen: std::cell::RefCell<Option<std::collections::HashSet<String>>> =
        std::cell::RefCell::new(None);
    dict::reactive::reactive_effect(
        move || {
            tick.set(tick.get().wrapping_add(1));
            tick.get()
        },
        move || {
            let Ok(st) = eff_app.try_borrow() else { return };
            let res = st.live_conflicts();
            if res.blocking != conflict.get() {
                conflict.set(res.blocking);
            }
            let now: std::collections::HashSet<String> =
                res.coexist.iter().map(|c| c.fingerprint()).collect();
            let mut slot = seen.borrow_mut();
            if let Some(prev) = slot.as_ref() {
                // 逐条播报新增项。同一帧内多条（如一次勾选同时撞上左右两个修饰键）会互相
                // 覆盖 toast——但共存文案已按「一对分组」描述，多条内容高度重复，不值得排队。
                for c in &res.coexist {
                    if !prev.contains(&c.fingerprint())
                        && let Some(note) = crate::key_conflict::coexist_note(c)
                    {
                        feedback.set(Some((ToastKind::Info, note)));
                    }
                }
            }
            *slot = Some(now);
        },
    )
}

/// 输入页：并入原「常规(general)」的启动状态等分区 + manifest「input」分组
/// （已取消独立「常规」Tab；general 分组若空则无副作用）。冲突提示由全局固定顶栏承担。
///
/// general 那批（就一节：「默认状态」）排在**全页最前**。它讲的是"每次开始打字时是中
/// 是英、标点是全是半"，属于人人都会调的基础项，摆在十几个专项功能之后没人翻得到。
///
/// ⚠ **`nav.wrap` 的调用顺序就是导航栏上的顺序，必须与 `secs` 的 push 顺序逐项对齐。**
/// 这不是排版洁癖：`SubNav::resolve` 判高亮用的是「顶端已越过判定线的**最后**一个」，
/// 靠 `tops` 的下标序 `next_back()` 实现，隐含假设下标序 == 页面纵向序。曾经这里先把
/// general 整批 wrap 掉、再在循环里按锚点分节插进页面中部，两序就此脱钩，后果是
/// ①导航栏「默认状态」排第一、页面上却在第六；②滚到「默认状态」那张卡时高亮停在它
/// **上面**那一项，且这一条除点击钉住外永远不会自己亮起来。故 general 这批只能整批
/// 排在头或尾——要插到中间，得连 wrap 一起插。
fn input_page(state: &AppState, nav: &SubNav) -> Element {
    let mut secs = manifest_sections(state, "general", nav);
    for (sec, keys) in state.manifest.sections_of("input") {
        secs.push(nav.wrap(
            &sec,
            0,
            section_items(&sec, section_row_items(state, &keys)),
        ));
    }
    sections_page("输入", "调整输入行为与纠错策略", secs)
}

// ───────────────────────── 方案 ─────────────────────────

/// 「列表显示」下拉：把两个显示过滤收进一个菜单，不再各占一格。
///
/// windui 的 dropdown 是**单选**语义（`selected: Signal<usize>`），没有复选菜单。这里用
/// 首项当标题哨兵：收起态恒显示 `options[selected]`，故 selected 必须停在第 0 项，
/// 标题才不会变成上次点的那一项。选中 1/2 项 → effect 翻转对应开关并把 selected 拨回 0；
/// 拨回这一步本身会再触发一次 effect，靠「== 0 时不做事」收敛。
///
/// 开关态用 badge 显示（开/关），比在 label 前缀加 ✓ 更容易一眼扫到。
fn build_list_view_menu(
    app: &Rc<RefCell<AppState>>,
    mgr: &crate::dialogs::schema_manager::SchemaManagerState,
) -> Element {
    let app_hd = app.clone();
    let (m_sh, app_sh) = (mgr.clone(), app.clone());
    Element::check_menu(
        "列表显示",
        vec![
            CheckMenuItem::check("隐藏未启用", mgr.hide_disabled)
                .on_change(move |_ctx, v| app_hd.borrow_mut().save_hide_disabled_schemas(v)),
            CheckMenuItem::check("显示特殊方案", mgr.show_hidden).on_change(move |_ctx, v| {
                // set_show_hidden 内会重建列表——特殊方案的显隐改的是「进不进列表」，
                // 不是单纯的行可见性（分区标题的计数按数据项算）。
                m_sh.set_show_hidden(v);
                app_sh.borrow_mut().save_show_hidden_schemas(v);
            }),
        ],
    )
    .width(132)
}

/// 方案页四张引擎卡：(卡片 / 导航名, 清单 section, 是否带模糊音入口)。
/// 卡片名与搜索面包屑同源（`search::index::placement_of` 引用本表）。
pub(crate) const SCHEME_ENGINE_CARDS: [(&str, &str, bool); 4] = [
    ("码表", "上屏行为", false),
    ("拼音", "候选行为", true),
    ("混输", "混输行为", false),
    ("英文", "英文行为", false),
];

fn scheme_page(app: &Rc<RefCell<AppState>>, state: &AppState, nav: &SubNav) -> Element {
    let mgr = &state.schema_mgr;

    // 工具栏**拆成两行**：上行是作用于选中项与全局的操作按钮，下行是列表显示过滤。
    //
    // 曾经一行装下：左边两个复选框、右边七个按钮 + 一个分隔条。加了「显示特殊方案」后
    // 总宽超出容器，按钮被挤出可视区。这不是第一次——中间原有的「共 N 个已安装方案」
    // 状态文字也是因为 weight(1.0) 吃掉弹性空间、把按钮挤没了才被删掉的。
    // 一行里塞十个控件本就没有余量，改成分行比继续在里面腾挪稳。
    //
    // 过滤行放在按钮行**下方、紧贴列表**：它描述的是「下面这个列表显示什么」，
    // 与列表相邻比与操作按钮相邻更说得通。
    // padding_xy(0,4) 给按钮 outline 边框与下方 divider 留出间距。
    let st_refresh = mgr.clone();
    let (m_up, e_up) = (mgr.clone(), mgr.clone());
    let (m_dn, e_dn) = (mgr.clone(), mgr.clone());
    let (m_act, e_act) = (mgr.clone(), mgr.clone());
    let (m_set, e_set) = (mgr.clone(), mgr.clone());
    let (m_del, e_del) = (mgr.clone(), mgr.clone());
    let (m_exp, e_exp) = (mgr.clone(), mgr.clone());
    let m_imp = mgr.clone();
    let toolbar = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(0, 4)
        .spacing(8)
        .child(build_list_view_menu(app, mgr))
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("↑")
                .small()
                .outline_soft()
                .enabled_when(move || {
                    e_up.sel_ver.get();
                    e_up.selected_enabled_pos().is_some_and(|(i, _)| i > 0)
                })
                .on_click(move |_| m_up.move_selected(true)),
        )
        .child(
            Element::button("↓")
                .small()
                .outline_soft()
                .enabled_when(move || {
                    e_dn.sel_ver.get();
                    e_dn.selected_enabled_pos().is_some_and(|(i, t)| i + 1 < t)
                })
                .on_click(move |_| m_dn.move_selected(false)),
        )
        .child(
            // 设为当前：即时生效 + 随保存持久化（见 SchemaManagerState::set_active）。
            // enabled_when 同时读 sel_ver 与 active_ver——选中变了要重估，设完当前
            // 自身要变灰，两个来源缺一不可。
            Element::button("设为当前")
                .small()
                .outline_soft()
                .enabled_when(move || {
                    e_act.sel_ver.get();
                    e_act.active_ver.get();
                    e_act.can_set_active()
                })
                .on_click(move |_| m_act.set_active_selected()),
        )
        .child(
            Element::button("设置")
                .small()
                .outline_soft()
                .enabled_when(move || {
                    e_set.sel_ver.get();
                    e_set.selected_entry().is_some()
                })
                .on_click(move |_| m_set.open_settings_selected()),
        )
        .child(
            Element::button("删除")
                .small()
                .outline()
                .danger()
                .enabled_when(move || {
                    e_del.sel_ver.get();
                    e_del.selected_is_user()
                })
                .on_click(move |_| m_del.request_delete_selected()),
        )
        .child(Element::leaf().width(1).height(18).bg_role(Role::Divider))
        .child(
            Element::button("刷新")
                .small()
                .outline_soft()
                .on_click(move |_| st_refresh.refresh()),
        )
        .child(
            Element::button("导入")
                .small()
                .outline_soft()
                .on_click(move |ctx| m_imp.do_import_package(ctx)),
        )
        .child(
            Element::button("导出")
                .small()
                .outline_soft()
                .enabled_when(move || {
                    e_exp.sel_ver.get();
                    e_exp.selected_entry().is_some()
                })
                .on_click(move |ctx| m_exp.do_export_selected(ctx)),
        )
        // 操作反馈 toast（0×0 不可见叶子，feedback 信号变化即弹单条 toast）。
        .child(dict::reactive::toast_sink(mgr.feedback));

    // 复合行列表（已启用在前 · 未启用在后 · 特殊方案独立分区）
    let list_el = build_schema_inline_list(mgr);

    // 工具栏与列表合成**一个**卡片子项：`card()` 会在相邻子项间自动插分隔线，而列表
    // 的每一行已自带顶部分隔线（可拖行必须行行一致，否则拖动中会错位——见
    // `build_draggable_row`），两者同时存在就成了双线。
    //
    // 列表级右键菜单（导入/刷新/显示过滤）挂在这个容器上：行有自己的菜单且命中更靠内，
    // 落到这里的只有列表下方空白与工具栏空隙。两个显示过滤要持久化，故把写盘动作
    // 以回调传进去——`SchemaManagerState` 不持有 `AppState`，不该为此反向依赖。
    let (app_hd, app_sh) = (app.clone(), app.clone());
    let m_list = mgr.clone();
    let toolbar_and_list = Element::col()
        .width_match()
        .child(crate::widgets::card_pad(toolbar))
        .child(list_el)
        .on_context_menu(move || {
            let (a_hd, a_sh) = (app_hd.clone(), app_sh.clone());
            crate::dialogs::schema_manager::list_menu_items(
                &m_list,
                Rc::new(move |v| a_hd.borrow_mut().save_hide_disabled_schemas(v)),
                Rc::new(move |v| a_sh.borrow_mut().save_show_hidden_schemas(v)),
            )
        });
    // 搜索锚点：方案列表/方案设置/导入导出那几条手写索引都落在这张卡上。
    let mut secs = vec![nav.wrap(
        "输入方案",
        0,
        crate::search::anchor::wrap(
            // `Bare`：列表行自带分割线，卡片不能再给整块加水平 padding，否则「通栏」贴不到边。
            crate::widgets::section_items(
                "输入方案",
                vec![crate::widgets::CardItem::Bare(toolbar_and_list)],
            ),
            "schema:list",
            state.jump,
        ),
    )];

    // 全局方案配置：码表 / 拼音 / 混输 / 英文 四台引擎的默认配置。
    //
    // ★ 这四块原本是四个模态对话框（旧 `build_scheme_config_button`），页面上只留四行
    // 「…… [设置]」。问题不在对话框本身，而在**发现不了**：整整四节、几十个开关，
    // 界面上一个字都看不到，用户只能靠搜索碰运气。二级导航要把它们列出来，就得先让
    // 它们真的在页面上有位置，故 2026-09 起改为页内铺开，对话框退役。
    //
    // 排在「主方案设置」之前——引擎全局配置是日常会翻的，主方案 id 通常设一次就不再动。
    //
    // ★ 这四张卡**不再收在「全局方案配置」这个分组标题下面**。加一层分组读起来是反的：
    // 「全局方案配置」既不是一张卡、也点不动，却在导航里占一行、把四个真目标缩进去，
    // 层次凭空多了一级而信息量为零。导航名与卡片标题一字不差，点过去看到什么一目了然；
    // 「这些是全局的」改由卡内第一行提示说（见 `manifest::scheme_config_section`）——
    // 那句话本来就在那儿，只是原先没点明「全局」。
    //
    // 卡片名只写引擎名（「码表」而不是「码表方案配置」）：四张卡同处方案页，后缀在每一张上
    // 都一样、不提供区分度，导航里还把真正的区别挤到了最后两个字之前。
    for (title, sec, with_fuzzy) in SCHEME_ENGINE_CARDS {
        if let Some(card) = scheme_config_section(state, title, sec, with_fuzzy) {
            secs.push(nav.wrap(title, 0, card));
        }
    }

    // 主方案设置，然后是临时拼音 / 临时英文：后两者是「临时换一套方案打字」，触发键、
    // 候选形态都是方案级的概念（原先在输入页）。
    let schema_sections = state.manifest.sections_of("schema");
    for (sec, nav_label) in [
        ("主方案设置", "主方案"),
        ("临时拼音", "临时拼音"),
        ("临时英文", "临时英文"),
    ] {
        if let Some((_, keys)) = schema_sections.iter().find(|(s, _)| s == sec) {
            secs.push(nav.wrap(
                nav_label,
                0,
                section_items(sec, section_row_items(state, keys)),
            ));
        }
    }

    sections_page("方案", "选择输入方案与引擎配置", secs)
}

// ───────────────────────── 按键 ─────────────────────────

fn keys_page(state: &AppState, nav: &SubNav) -> Element {
    sections_page(
        "按键",
        "自定义快捷键与候选选择键",
        manifest_sections(state, "keys", nav),
    )
}

// ───────────────────────── 外观 ─────────────────────────

fn appearance_page(app: &Rc<RefCell<AppState>>, state: &AppState, nav: &SubNav) -> Element {
    // 「主题」卡手写：manifest 的两行（主题选择 / 主题风格）之后直接接导入、导出与市场
    // 入口——选主题与搬主题是同一件事的两半，隔开就得让用户满页找按钮。其余 ui 分区照
    // manifest 原样生成。
    //
    // 主题编辑器仍是独立一张卡：那是面向进阶用户的**连接状态**管理，混进来会分不清
    // "现在按钮是要做什么"和"现在连接是什么状态"。
    let transfer = !state.manifest.feature_hidden("theme_transfer");
    let mut ui_secs = state.manifest.sections_of("ui");
    // 「主题」卡的清单项全被定制版藏掉时 `sections_of` 不再给这一节，可三行手写入口没被藏
    // 就不该跟着消失：补一张只有这三行的「主题」卡，排在原位（页首）。
    if transfer && !ui_secs.iter().any(|(s, _)| s == "主题") {
        ui_secs.insert(0, ("主题".to_string(), Vec::new()));
    }
    let mut secs: Vec<Element> = Vec::new();
    for (sec, keys) in ui_secs {
        let rows = if sec == "主题" && transfer {
            let mut rows = section_rows(state, &keys);
            rows.extend(theme_transfer_rows(state));
            rows
        } else {
            section_rows(state, &keys)
        };
        let card = section(&sec, rows);
        // 搜索锚点仍叫 ui:theme-import（导入相关词条指向它），现在落在「主题」卡上。
        let card = if sec == "主题" {
            crate::search::anchor::wrap(card, "ui:theme-import", state.jump)
        } else {
            card
        };
        secs.push(nav.wrap(&sec, 0, card));
    }
    if !state.manifest.feature_hidden("theme_editor") {
        secs.push(nav.wrap(
            "主题编辑器",
            0,
            crate::search::anchor::wrap(
                theme_editor_connect_card(app, state),
                "ui:theme-editor",
                state.jump,
            ),
        ));
    }
    // 主题导入/编辑器连接的反馈走 AppState.feedback——shell::build 已挂了唯一的全局
    // toast_sink，这里不需要（也不应该）再单独挂一份。
    let page = sections_page("外观", "自定义候选窗与工具栏的视觉呈现", secs);
    Element::stack()
        .fill()
        .child(page)
        .child(theme_editor_mode_effect(app))
}

/// 监听 `theme_editor_mode_idx`（分段控件双向绑定）变化，触发实际启停+持久化。
/// 索引信号只表达"用户想切到哪一档"，副作用集中在 `AppState::apply_theme_editor_mode_idx`，
/// 避免和旧版 3 个按钮各自 on_click 重复的启停逻辑。
fn theme_editor_mode_effect(app: &Rc<RefCell<AppState>>) -> Element {
    let idx = app.borrow().theme_editor_mode_idx;
    let eff_state = app.clone();
    dict::reactive::reactive_effect(
        move || idx.version(),
        move || {
            eff_state
                .borrow_mut()
                .apply_theme_editor_mode_idx(idx.get())
        },
    )
}

/// 主题的「搬进搬出」三行：本地 TOML 导入 + 主题包导出 + 主题市场入口。
///
/// 返回**行**而不是整张卡：它们接在 manifest「主题」卡的主题选择/风格之后，同卡呈现。
/// 此前这是独立一张卡、且排在所有 ui 卡之后，与主题选择隔着十几张卡——用户选完主题想
/// 导出，得一路滚到页尾才找得到按钮。
///
/// 结果经 toast 反馈（成功/失败一致走 toast——界面里已没有承载一次性提示的常驻状态文字位）。
fn theme_transfer_rows(state: &AppState) -> Vec<Element> {
    let feedback = state.feedback;
    // 导出要读「下拉里此刻选中的是哪个主题」。`fields` 是 Rc，可以进闭包；表里的 getter
    // 由 manifest 在建控件时登记，页面重建会整表 reset 再填，持 Rc 读到的始终是新的那份。
    let fields = state.fields.clone();
    // 撞名覆盖确认的那几个 signal 不在这里取：导入已经整个搬进「导入主题」对话框，
    // 由它持有；卡里只剩一个开框的按钮。
    let picker_visible = state.theme_picker_visible;

    // ★ 外观页最后这两张卡曾经是**手搓的外壳**：`col + bg + border + corner(12) + 一个
    // 14px 粗体 label` 当标题。上面所有 manifest 分区走的是 `widgets::section`——彩色图标
    // chip + 标题、`flat_panel` 卡片（圆角 13、带投影、行间自动分隔线）。两套壳并排放着，
    // 差别一眼看得出：没有图标、圆角小一档、没有投影、行与行之间也没有分隔线。
    // 现改为与其余分区同一套构件，行也统一成 `setting_row`。
    vec![
        setting_row(
            "导入主题",
            Some("从本地文件（.toml 主题 / .wtheme 主题包）、链接或粘贴的原文导入"),
            Element::button("导入…")
                .small()
                .outline_soft()
                .on_click(move |_| picker_visible.set(true)),
        ),
        setting_row(
            "导出当前主题",
            Some("打成 .wtheme 主题包，连背景图一起，可直接发给别人"),
            Element::button("导出…")
                .small()
                .outline_soft()
                .on_click(move |ctx| {
                    // 在**事件线程上**先把下拉选中值读出来：getter 闭包捕获的是 Signal，
                    // 读它得在 UI 这一侧完成，不能塞进下面那个延后执行的闭包里。
                    let ui_selected = {
                        let g = fields.getters.borrow();
                        g.get("ui.theme.name")
                            .map(|f| f())
                            .and_then(|v| v.as_str().map(str::to_string))
                    };
                    // 走 `defer_blocking` 而非 `request_save_file`：两者时机语义相同
                    // （都在事件分发完全返回后才弹原生框），但前者把「取当前主题 +
                    // 选路径 + 调 core + 报结果」整条流程留在一处，不必把后半段拆进
                    // 对话框回调里。形态同方案包导出（见 `schema_manager::export_schema`）。
                    ctx.defer_blocking(move || {
                        // 当前主题**在这里实时取**，不能在构建帧把 id 算好带进闭包：
                        // 这张卡只在页面重建时求值一次，而上面那个主题下拉随时能换一个
                        // ——捕获来的 id 会停在用户打开设置页的那一刻，导出的就成了另一
                        // 个主题，而回执里的名字还是对的，用户不会发现拿错了。
                        //
                        // 取值走 `theme_import::theme_id_for_export`：下拉选中优先、
                        // 生效值兜底，那里说明了为什么不能拿生效值当准。
                        let id = match crate::theme_import::theme_id_for_export(ui_selected) {
                            Ok(id) => id,
                            Err(e) => {
                                feedback.set(Some((ToastKind::Error, e.user_message())));
                                return;
                            }
                        };
                        let Some(path) = PickDialog::new()
                            .title("导出主题包")
                            .file_name(format!("{id}.wtheme"))
                            .filter("主题包", &["wtheme"])
                            .save_file()
                        else {
                            return;
                        };
                        match crate::rpc::call_core(
                            "theme.exportPackage",
                            &serde_json::json!({
                                "slug": id,
                                "path": path.to_string_lossy(),
                            }),
                        ) {
                            Ok(v) => {
                                let name = v
                                    .get("display_name")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or(&id);
                                let n = v
                                    .get("asset_count")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or(0);
                                feedback.set(Some((
                                    ToastKind::Success,
                                    format!("已导出「{name}」（{n} 个资源文件）"),
                                )));
                            }
                            // 不再套一层「导出失败:」：core 那边的错误已经带了
                            // 「导出主题「X」失败：<原因>」这层主语（RPC 出口只传
                            // 一个字符串，原因就展开在里面），再包一层是重复。
                            Err(e) => feedback.set(Some((ToastKind::Error, e.to_string()))),
                        }
                    });
                }),
        ),
        setting_row(
            "主题市场",
            Some("前往主题市场挑选现成方案"),
            Element::button("打开市场")
                .small()
                .outline_soft()
                .on_click(|ctx| ctx.open_url(MARKET_URL)),
        ),
    ]
}

/// 主题编辑器卡：Web 编辑器通过本地 loopback 服务读写主题，三态分段控件
/// （关闭/本次开启/自动开启）取代原先 3 个平级按钮——分段控件天然高亮当前档位，
/// 用户一眼能看出"现在是不是开着"，不必再靠地址是否为空去猜状态。
fn theme_editor_connect_card(app: &Rc<RefCell<AppState>>, state: &AppState) -> Element {
    let running = state.theme_editor_running;
    let url = state.theme_editor_url;
    let mode_idx = state.theme_editor_mode_idx;
    let _ = app;

    // 地址那一行的说明文字：开着时是地址本身，关着时说「尚未开启」。
    //
    // 原先是两个 label 靠 `visible_when` 互斥切换、并列摆在卡片里——那套只在手搓外壳里
    // 成立。改用 `setting_row_desc_signal` 之后说明位只吃一个 `Signal<String>`，故把
    // 「空 → 尚未开启」这一步折进一枚 0×0 effect 里。构建帧 effect 不触发（它以当前
    // 版本为基线），所以初值要在这里直接算一次。
    let addr_desc = signal(url_desc(&url.get()));
    let addr_sync = dict::reactive::reactive_effect(
        move || url.version(),
        move || addr_desc.set(url_desc(&url.get())),
    );

    crate::widgets::section_with_attachment(
        "主题编辑器",
        vec![
            setting_row(
                "连接模式",
                // 说明要短：右边那个三档分段控件占了小半行，长句会被省略号吃掉后半截
                // （首版实测停在「仅监听本…」）。「不对外网开放」这层意思由「仅监听本机」
                // 说全了，删掉不丢信息。
                Some("Web 版编辑器直接读写本机主题；仅监听本机地址"),
                Element::segmented(
                    vec![
                        "关闭".to_string(),
                        "本次开启".to_string(),
                        "自动开启".to_string(),
                    ],
                    mode_idx,
                ),
            ),
            crate::widgets::setting_row_desc_signal(
                "本地连接地址",
                addr_desc,
                Element::row()
                    .spacing(8)
                    .child(
                        Element::button("复制地址")
                            .small()
                            .outline_soft()
                            .enabled_signal(running)
                            .on_click(move |ctx| {
                                let u = url.get();
                                if !u.is_empty() {
                                    ctx.clipboard_set(&u);
                                    ctx.toast_ok("已复制主题编辑器连接地址");
                                }
                            }),
                    )
                    .child(
                        Element::button("打开编辑器")
                            .small()
                            .outline_soft()
                            .on_click(|ctx| ctx.open_url(THEME_EDITOR_URL)),
                    ),
            ),
        ],
        addr_sync,
    )
}

/// 编辑器直连地址那一行的说明文字。空地址＝服务没开。
fn url_desc(url: &str) -> String {
    if url.is_empty() {
        "尚未开启".to_string()
    } else {
        url.to_string()
    }
}

// ───────────────────────── 词库 ─────────────────────────

/// 词库在主窗里只剩一个**永不显示的占位格**：这一页已整体迁进独立窗
/// （[`crate::dict_window`]），侧栏那一项是开窗的动作项、不切页，故 `tab` 永远不会等于
/// 它的下标。
///
/// 保留占位而不是把这一格从 `all_tabs` 里删掉：`cli::PAGES` 与本函数的顺序是靠约定对齐的
/// （`page_to_index("dict") == 5`），少一格就会让它后面的「高级/关于」整体前移一位，
/// 于是 `--page about` 打开的是高级页 —— 不报错、不 panic，只是每一条深链都错一格。
fn dict_page(_state: &AppState) -> Element {
    Element::leaf()
}

// ───────────────────────── 关于 ─────────────────────────

/// 仓库地址。与 Vue 版 `App.vue` 的 repoUrl 一致。
const REPO_URL: &str = "https://github.com/huanfeng/WindInput";
/// 官网。
const WEBSITE_URL: &str = "https://windinput.com";
/// 社区论坛（经验交流与问题反馈的第一落点，取代原来直挂 GitHub issues 的入口）。
const FORUM_URL: &str = "https://forum.windinput.com";
/// 下载页。版本发布指这里而不是 GitHub releases：官网页面对非开发者更友好，
/// 且能按平台分发定制版安装包。
const DOWNLOAD_URL: &str = "https://windinput.com/download";
/// 赞助页。
const SPONSOR_URL: &str = "https://windinput.com/sponsor";
/// 主题编辑器。
pub(crate) const THEME_EDITOR_URL: &str = "https://theme.windinput.com";
/// 资源市场（主题皮肤、输入方案等，不止主题）。
pub(crate) const MARKET_URL: &str = "https://market.windinput.com";
/// 官网文档首页（设置向导欢迎页指这里）。
pub(crate) const DOCS_URL: &str = "https://windinput.com/docs";
/// QQ 交流群号。
const QQ_GROUP: &str = "1085293418";

// 品牌图标 SVG（白色填充，24×24 viewBox），从 res/icons 嵌入。图标数据取自 Vue 版
// AboutPage，保证与网页版逐像素一致；改图标只改 .svg 文件即可，无需动代码。
const ICON_WEBSITE: &str = include_str!("../../res/icons/website.svg");
const ICON_GITHUB: &str = include_str!("../../res/icons/github.svg");
const ICON_FORUM: &str = include_str!("../../res/icons/forum.svg");
const ICON_RELEASES: &str = include_str!("../../res/icons/releases.svg");
const ICON_QQ: &str = include_str!("../../res/icons/qq.svg");
/// 页脚赞助链接的爱心。与上面几个不同，它不进彩色图标方块，而是 `tint` 成品牌红后
/// 直接跟在文字旁边，故按小尺寸渲染。
const ICON_HEART: &str = include_str!("../../res/icons/heart.svg");

/// 域名 chip（官网横幅右侧）：淡蓝底 + 蓝字 + pill 圆角，**普通字重**
/// （区别于 `badge` 的 600 加粗）。蓝色与官网图标方块同源，观感协调。
fn url_chip(text: &str) -> Element {
    let blue = Color::hex(0x3B82F6);
    Element::row()
        .cross(Align::Center)
        .padding_xy(9, 3)
        .corner(999.0)
        .bg(blue.scale_alpha(0.14))
        .child(Element::label(text).font_size(12.0).fg(blue))
}

/// 彩色圆角图标方块 + 居中白色 SVG 图标（品牌色固定、不随主题；SVG 走 `svg` feature）。
/// `target_width` 取 2× 显示像素求 HiDPI 清晰度。
fn about_icon_box(bg: Color, svg: &str, size: i32) -> Element {
    let icon = (size as f32 * 0.56) as i32;
    Element::stack().size(size, size).corner(10.0).bg(bg).child(
        Element::image_svg(svg.as_bytes(), Some((icon * 2) as u32))
            .size(icon, icon)
            .align(Align::Center),
    )
}

/// 可点击链接卡片：品牌图标 + 标题 + 描述，点击执行 `action`。
/// 表面/文字走角色化主题（明暗自适应），仅图标方块用品牌固定色。
/// `trailing` 为右侧可选挂件（如官网横幅的域名 chip）。
fn about_link_card(
    icon_bg: Color,
    svg_path: &str,
    title: &str,
    desc: &str,
    trailing: Option<Element>,
    action: impl FnMut(&mut EventCtx) + 'static,
) -> Element {
    let mut row = Element::row()
        .clickable()
        .on_click(action)
        .width_match()
        .cross(Align::Center)
        .spacing(12)
        .padding(14)
        .corner(12.0)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .child(about_icon_box(icon_bg, svg_path, 36))
        .child(
            Element::col()
                .weight(1.0)
                .spacing(2)
                .child(
                    Element::label(title)
                        .font_size(14.0)
                        .font_weight(600)
                        .fg_role(Role::Text)
                        .width_match()
                        .max_lines(1)
                        .truncate(Truncate::End)
                        .height(19),
                )
                .child(
                    Element::label(desc)
                        .font_size(12.0)
                        .fg_role(Role::TextMuted)
                        .width_match()
                        .max_lines(1)
                        .truncate(Truncate::End)
                        .height(16),
                ),
        );
    if let Some(t) = trailing {
        row = row.child(t);
    }
    row
}

/// 定制版徽章文案：`system.info` 的 `customEdition` → 「虎码定制版 1.2」，非定制版 `None`。
///
/// ★ **空值处置落在这里，不在 core。** `wind-rpc` 的 `identity_json` 刻意不做占位替换——
/// 它的另一个消费者是报障脚本，把 `<未命名>` 塞进结构化字段会让「定制者没填 version」
/// 变成一个看起来像版本号的字符串。于是「显示成什么」由每个 UI 自己定。
///
/// 取值顺序 `name` → `id`，两者皆空时**仍然出徽章**（回落到「定制版」三个字）：这一项
/// 存在的全部理由是报障时能一眼判断本机装的是什么，而「本机是定制版」这件事本身就比
/// 显示名叫什么更值得让截图带上。
///
/// 字段缺失与显式 `null` 同样处置：前者是老版本 core 还没有这个字段，后者是它明确回答
/// 了「不是定制版」——对徽章而言结论一致。
pub(crate) fn custom_edition_badge(system_info: &serde_json::Value) -> Option<String> {
    let ce = system_info.get("customEdition")?;
    if ce.is_null() {
        return None;
    }
    let pick = |k: &str| {
        ce.get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let name = pick("name").or_else(|| pick("id")).unwrap_or("定制版");
    Some(match pick("version") {
        Some(v) => format!("{name} {v}"),
        None => name.to_string(),
    })
}

/// 关于页 — 复刻品牌关于卡（对齐截图 / Vue AboutPage）：
/// 头部（图标+名称+版本徽章+定制版徽章+检查更新）· 官网横幅 · 2×2 链接网格 · 版权页脚。
fn about_page(app: &Rc<RefCell<AppState>>, state: &AppState) -> Element {
    // 「检查更新」按钮要能开对话框并起后台线程，故需共享状态而非只读快照
    let update_state = app.clone();

    // 版本徽章与标题栏统一：取 build 期注入的 WIND_SETTING_VERSION（= docs/VERSION，已含 v 前缀），
    // dev 变体追加 " (Dev)"。不再用 service 上报的 system_info（其值源自 wind-rpc 的
    // CARGO_PKG_VERSION = workspace 兜底版本 0.x）或本 crate 的 CARGO_PKG_VERSION。
    let ver_badge = format!(
        "{}{}",
        env!("WIND_SETTING_VERSION"),
        crate::mode::version_suffix()
    );

    // 头部：大图标 + 名称 + 版本徽章 / 检查更新 + 副标题。
    let header = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(18)
        .child(crate::widgets::app_icon(72))
        .child(
            Element::col()
                .weight(1.0)
                .spacing(8)
                .child(
                    Element::label("清风输入法")
                        .font_size(22.0)
                        .font_weight(700)
                        .fg_role(Role::Text)
                        .width_match()
                        .max_lines(1)
                        .height(30),
                )
                .child({
                    let mut row = Element::row()
                        .cross(Align::Center)
                        .spacing(10)
                        .child(Element::badge(ver_badge));
                    // 定制版身份。文案与可见性都走 `state.custom_edition` 这个 Signal：
                    // 页面树在数据回来之前就建好了，此刻直接读 `state.system_info` 恒为空
                    // （详见该字段注释）。空串 = 非定制版 ⇒ 徽章不占位。
                    //
                    // 用 Neutral 而不是默认的 Primary：两个强调蓝徽章并排会分不出主次，
                    // 而这里版本号是主、定制版身份是辅。
                    let ce_sig = state.custom_edition;
                    row = row.child(
                        Element::badge_intent(ce_sig, Intent::Neutral)
                            .visible_when(move || !ce_sig.get().is_empty()),
                    );
                    // 非 Windows 没有升级通路（见 update::self_update_supported）：
                    // 按钮点下去只会下到一个装不上的 .exe，不如不给。
                    if crate::update::self_update_supported()
                        && !state.manifest.feature_hidden("update_check")
                    {
                        row.child(
                            Element::button("检查更新")
                                .small()
                                .outline_soft()
                                .on_click(move |_| update_state.borrow().open_update_dialog()),
                        )
                    } else {
                        row
                    }
                })
                .child(
                    Element::label("轻量、快速、可定制的开源中文输入法")
                        .font_size(13.0)
                        .fg_role(Role::TextMuted)
                        .width_match()
                        .max_lines(1)
                        .height(19),
                ),
        );

    // 官网横幅：整行可点击 + 右侧域名 chip。
    let banner = about_link_card(
        Color::hex(0x3B82F6),
        ICON_WEBSITE,
        "官方网站",
        "文档、下载与最新动态",
        Some(url_chip("windinput.com")),
        |ctx| ctx.open_url(WEBSITE_URL),
    );

    // 2×2 链接网格：GitHub / 社区论坛 / 版本发布 / QQ 交流群。
    let github = about_link_card(
        Color::hex(0x24292F),
        ICON_GITHUB,
        "GitHub",
        "源码与文档",
        None,
        |ctx| ctx.open_url(REPO_URL),
    );
    let forum = about_link_card(
        Color::hex(0xF59E0B),
        ICON_FORUM,
        "社区论坛",
        "经验交流 · 报告 Bug 或建议",
        None,
        |ctx| ctx.open_url(FORUM_URL),
    );
    let releases = about_link_card(
        Color::hex(0x22C55E),
        ICON_RELEASES,
        "版本发布",
        "下载与更新日志",
        None,
        |ctx| ctx.open_url(DOWNLOAD_URL),
    );
    // QQ 卡：点击复制群号 + toast 反馈（脱离布局树，无需额外接线）。
    let qq = about_link_card(
        Color::hex(0x12B7F5),
        ICON_QQ,
        "QQ 交流群",
        QQ_GROUP,
        None,
        |ctx| {
            ctx.clipboard_set(QQ_GROUP);
            ctx.toast_ok(format!("已复制群号 {QQ_GROUP}"));
        },
    );

    // 每行两列等宽（stack + weight 撑满半宽，对齐示例网格）。
    let half = |el: Element| Element::stack().weight(1.0).child(el);
    let grid = Element::col()
        .width_match()
        .spacing(10)
        .child(
            Element::row()
                .width_match()
                .spacing(10)
                .cross(Align::Stretch)
                .child(half(github))
                .child(half(forum)),
        )
        .child(
            Element::row()
                .width_match()
                .spacing(10)
                .cross(Align::Stretch)
                .child(half(releases))
                .child(half(qq)),
        );

    // 页脚：版权 + 赞助入口。两端 flex_spacer 把这一组顶到居中——线性容器只有交叉轴
    // 对齐（`cross`），主轴没有 justify，弹簧是这里唯一的居中手段；label 因此不能再
    // `width_match()`（撑满就没有可分给弹簧的余量了），改由内容定宽。
    let heart = Color::hex(0xEF4444);
    // 胶囊圆角 + 内边距：`Clickable` 的 hover 叠层按 `style.corner_radius` 画圆角矩形，
    // 不给 `corner` 就是直角方块——两行小字旁边贴一个方块高亮太硬。padding 同时把
    // 点击热区从「文字本身」撑到一个能顺手点中的胶囊。
    let sponsor = Element::row()
        .clickable()
        .cross(Align::Center)
        .spacing(5)
        .padding_xy(10, 5)
        .corner(999.0)
        .on_click(|ctx| ctx.open_url(SPONSOR_URL))
        .child(crate::icons::tinted(ICON_HEART, 13, heart))
        .child(
            Element::label("赞助")
                .font_size(12.0)
                .fg(heart)
                .max_lines(1)
                .height(18),
        );
    let footer = Element::row()
        .width_match()
        .cross(Align::Center)
        // 4 而不是 10：赞助胶囊自带 10px 左内边距，两者相加才是观感上的间距。
        .spacing(4)
        .child(Element::flex_spacer())
        .child(
            Element::label("© 2026 清风输入法 · MIT License")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .max_lines(1)
                .height(18),
        )
        .child(sponsor)
        .child(Element::flex_spacer());

    // 外层品牌卡：承载全部内容，浮于内容区 SurfaceAlt 之上。
    let panel = crate::widgets::flat_panel()
        .corner(16.0)
        .width_match()
        .padding(24)
        .spacing(20)
        .child(header)
        .child(banner)
        .child(grid)
        .child(footer);

    let mut body = Element::col().width_match().spacing(18);
    if crate::widgets::PAGE_HEADER_ENABLED {
        body = body.child(crate::widgets::page_header("关于", "版本信息与支持"));
    }
    // 搜索锚点：版本/检查更新/日志目录三条都落在这张品牌卡上——本页内容不多，
    // 再往里细分锚点，闪的范围反而比读者的目光落点还小。
    crate::widgets::page(body.child(crate::search::anchor::wrap(panel, "about:main", state.jump)))
}

#[cfg(test)]
mod about_tests {
    use super::custom_edition_badge;
    use serde_json::json;

    /// 「不是定制版」的两种形态都不出徽章：core 明确回答的 `null`，与老版本 core
    /// 根本没有这个字段。跨仓契约里这两者对 UI 的结论一致。
    #[test]
    fn no_badge_when_not_custom_edition() {
        assert_eq!(
            custom_edition_badge(&json!({ "customEdition": null })),
            None
        );
        assert_eq!(custom_edition_badge(&json!({ "version": "0.120.0" })), None);
    }

    #[test]
    fn badge_prefers_name_then_id() {
        assert_eq!(
            custom_edition_badge(&json!({
                "customEdition": { "name": "虎码定制版", "id": "huma", "version": "1.2" }
            })),
            Some("虎码定制版 1.2".to_string())
        );
        // 定制者没填 name（或只填了空白）：退到 id。全角空格也算没填。
        assert_eq!(
            custom_edition_badge(&json!({
                "customEdition": { "name": "   ", "id": "huma", "version": "1.2" }
            })),
            Some("huma 1.2".to_string())
        );
    }

    /// 清单在场但字段全空：仍要出徽章。徽章要回答的是「本机是不是定制版」，
    /// 而不是「这个定制版叫什么」——前者即使无名也有报障价值。
    #[test]
    fn badge_survives_all_fields_blank() {
        assert_eq!(
            custom_edition_badge(&json!({
                "customEdition": { "name": "", "id": "", "version": "" }
            })),
            Some("定制版".to_string())
        );
    }

    /// 没填版本号时不留下尾随空格（`format!("{name} {v}")` 的那条路要走不到）。
    #[test]
    fn badge_omits_missing_version() {
        assert_eq!(
            custom_edition_badge(&json!({
                "customEdition": { "name": "虎码定制版", "id": "huma" }
            })),
            Some("虎码定制版".to_string())
        );
    }
}

#[cfg(test)]
mod tests {
    /// 守门：ui 组必须仍有一节叫「主题」。
    ///
    /// [`appearance_page`] 拿 `sec == "主题"` 这个**字符串**判据往那张卡里追加「导入 /
    /// 导出 / 主题市场」三行。清单里把这一节改个名，三行就**整体从界面上消失**——页面
    /// 照常渲染、编译照常通过、别的测试照常全绿，只有拿眼睛看才发现主题卡上少了三个
    /// 按钮。同 `search/index.rs` 那张手写表的风险（「⚠ 编译器不管这张表」）。
    #[test]
    fn ui_group_still_has_the_theme_section() {
        let m = crate::manifest::Manifest::from_embedded();
        let secs: Vec<String> = m.sections_of("ui").into_iter().map(|(s, _)| s).collect();
        assert!(
            secs.iter().any(|s| s == "主题"),
            "ui 组里没有「主题」这一节，appearance_page 的导入/导出/市场三行会静默消失；\
             现有分节：{secs:?}"
        );
    }

    /// 守门：general 组必须仍有分节，且只有一节。
    ///
    /// [`input_page`] 把 general 整批排在输入页最前，是拿"就『默认状态』这一节、且它
    /// 该排第一"当前提的。哪天 general 里多出第二节，那一节会**跟着**一起挤到页首，
    /// 页面照常渲染、测试照常全绿，只有拿眼睛看才发现输入页开头凭空多了张卡。
    #[test]
    fn general_group_is_single_leading_section() {
        let m = crate::manifest::Manifest::from_embedded();
        let secs: Vec<String> = m
            .sections_of("general")
            .into_iter()
            .map(|(s, _)| s)
            .collect();
        assert_eq!(
            secs.len(),
            1,
            "general 组不再是单节：input_page 会把这几节一起摆到输入页最前，\
             确认新分节该不该排在页首，不该就去 input_page 里显式定位：{secs:?}"
        );
    }
}
