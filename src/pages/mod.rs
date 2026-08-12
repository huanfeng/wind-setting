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
pub mod dict;
pub mod stats;

use crate::dialogs::schema_manager::build_schema_inline_list;
use crate::manifest::{build_scheme_config_button, section_rows};
use crate::state::AppState;
use crate::widgets::{page, section, setting_row, todo_hint};

/// Tab：(标题, 内容)。manifest 分组负责主体，自定义页补充方案管理/词库/统计/关于。
pub fn all_tabs(app: &Rc<RefCell<AppState>>, state: &AppState) -> Vec<(String, Element)> {
    vec![
        ("方案".into(), scheme_page(app, state)),
        ("输入".into(), input_page(state)),
        ("按键".into(), keys_page(state)),
        ("外观".into(), appearance_page(app, state)),
        ("词库".into(), dict_page(state)),
        ("统计".into(), stats::build_page(state)),
        ("高级".into(), advanced_page(state)),
        ("关于".into(), about_page(app, state)),
    ]
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

/// 由 manifest 某分组生成若干 section 卡片（每 section 一张卡片，行按 manifest 顺序）。
fn manifest_sections(state: &AppState, group: &str) -> Vec<Element> {
    state
        .manifest
        .sections_of(group)
        .into_iter()
        .map(|(sec, keys)| section(&sec, section_rows(state, &keys)))
        .collect()
}

/// 高级页：manifest「advanced」分组 + 备份与还原卡片。
/// （原通用 manifest_page 仅剩本页使用，逻辑并入此处。）
fn advanced_page(state: &AppState) -> Element {
    let mut secs = manifest_sections(state, "advanced");
    if secs.is_empty() {
        secs.push(
            Element::col()
                .width_match()
                .child(todo_hint("（该分组暂无清单项，或 core 未运行）")),
        );
    }
    secs.push(backup::build_backup_section(&state.backup_mgr));
    sections_page("高级", "服务与实验性功能", secs)
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
        .bg(Color::hex(0xF59E0B).scale_alpha(0.14))
        .border_role(Role::Divider, 1)
        .child(
            Element::label_rc(conflict)
                .font_size(12.5)
                .font_weight(500)
                .fg(Color::hex(0xB45309))
                .weight(1.0)
                // 三域各一行，且单域多冲突会折行——放宽上限避免静默截断藏冲突。
                .max_lines(12),
        )
        .visible_when(move || !conflict.get().is_empty())
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

/// 输入页：manifest「input」分组 + 并入原「常规(general)」的启动状态等分区
/// （已取消独立「常规」Tab；general 分组若空则无副作用）。冲突提示由全局固定顶栏承担。
fn input_page(state: &AppState) -> Element {
    let mut secs = manifest_sections(state, "input");
    secs.extend(manifest_sections(state, "general"));
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
                .on_change(move |v| app_hd.borrow_mut().save_hide_disabled_schemas(v)),
            CheckMenuItem::check("显示特殊方案", mgr.show_hidden).on_change(move |v| {
                // set_show_hidden 内会重建列表——特殊方案的显隐改的是「进不进列表」，
                // 不是单纯的行可见性（分区标题的计数按数据项算）。
                m_sh.set_show_hidden(v);
                app_sh.borrow_mut().save_show_hidden_schemas(v);
            }),
        ],
    )
    .width(132)
}

fn scheme_page(app: &Rc<RefCell<AppState>>, state: &AppState) -> Element {
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

    let mut secs = vec![section("输入方案", vec![toolbar, list_el])];

    // 全局方案配置：码表 / 混输 / 拼音 / 英文 四个 [设置] 对话框。
    // 排在「主方案设置」之前——引擎全局配置是日常会翻的，主方案 id 通常设一次就不再动。
    let codetable = build_scheme_config_button(state, "码表方案配置", "上屏行为", false);
    let mixed = build_scheme_config_button(state, "混输方案配置", "混输行为", false);
    let pinyin = build_scheme_config_button(state, "拼音方案配置", "候选行为", true);
    let english = build_scheme_config_button(state, "英文方案配置", "英文行为", false);
    secs.push(section(
        "全局方案配置",
        vec![
            setting_row("码表方案配置", Some("码表引擎的全局默认配置"), codetable),
            setting_row("混输方案配置", Some("混输引擎的全局默认配置"), mixed),
            setting_row("拼音方案配置", Some("拼音引擎的全局默认配置"), pinyin),
            setting_row("英文方案配置", Some("英文引擎的全局默认配置"), english),
        ],
    ));

    // 主方案设置（manifest schema 分组中的"主方案设置" section）。
    if let Some((_, keys)) = state
        .manifest
        .sections_of("schema")
        .into_iter()
        .find(|(s, _)| s == "主方案设置")
    {
        secs.push(section("主方案设置", section_rows(state, &keys)));
    }

    sections_page("方案", "选择输入方案与引擎配置", secs)
}

// ───────────────────────── 按键 ─────────────────────────

fn keys_page(state: &AppState) -> Element {
    sections_page(
        "按键",
        "自定义快捷键与候选选择键",
        manifest_sections(state, "keys"),
    )
}

// ───────────────────────── 外观 ─────────────────────────

fn appearance_page(app: &Rc<RefCell<AppState>>, state: &AppState) -> Element {
    // 主题选择已并入 manifest ui「主题」卡（ui.theme.name，options_source=themes，
    // 下拉显示主题名、保存 slug；登记保存/加载/恢复）。
    // 主题导入/编辑器直连拆两张卡：前者是日常操作，后者是面向进阶用户的连接管理，
    // 混在一张卡里容易分不清"现在按钮是要做什么"和"现在连接是什么状态"。
    let mut secs = manifest_sections(state, "ui");
    secs.push(theme_import_card(state));
    secs.push(theme_editor_connect_card(app, state));
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

/// 主题导入卡：本地 TOML 导入 + 主题市场入口。导入结果经 toast 反馈（成功/失败一致走
/// toast，不再用状态文字——状态文字留给"标题栏右侧"的持续性连接状态展示）。
fn theme_import_card(state: &AppState) -> Element {
    let feedback = state.feedback;

    Element::col()
        .width_match()
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .padding_xy(16, 14)
        .spacing(8)
        .child(Element::label("主题导入").font_size(14.0).font_weight(600))
        .child(
            Element::label("从本地 .toml 文件导入主题，或前往主题市场挑选现成方案。")
                .font_size(12.0)
                .fg_role(Role::TextMuted),
        )
        .child(
            Element::row()
                .spacing(8)
                .child(
                    Element::button("导入 TOML 主题")
                        .small()
                        .on_click(move |ctx| {
                            ctx.request_pick_file(
                                PickDialog::new()
                                    .title("导入 TOML 主题")
                                    .filter("TOML", &["toml"]),
                                move |path| {
                                    let Some(path) = path else { return };
                                    // 直接读取 → 导入；import_text（core）已校验，无需单独预览。
                                    match crate::theme_import::read_local_toml(&path).and_then(
                                        |text| crate::theme_import::import_text(&text, false, None),
                                    ) {
                                        Ok(result) => feedback.set(Some((
                                            ToastKind::Success,
                                            format!("已导入主题：{}", result.display_name),
                                        ))),
                                        Err(err) => feedback
                                            .set(Some((ToastKind::Error, err.user_message()))),
                                    }
                                },
                            );
                        }),
                )
                .child(
                    Element::button("打开主题市场")
                        .small()
                        .outline_soft()
                        .on_click(|ctx| ctx.open_url(THEME_MARKET_URL)),
                ),
        )
}

/// 主题编辑器直连卡：Web 编辑器通过本地 loopback 服务读写主题，三态分段控件
/// （关闭/本次开启/自动开启）取代原先 3 个平级按钮——分段控件天然高亮当前档位，
/// 用户一眼能看出"现在是不是开着"，不必再靠地址是否为空去猜状态。
fn theme_editor_connect_card(app: &Rc<RefCell<AppState>>, state: &AppState) -> Element {
    let running = state.theme_editor_running;
    let url = state.theme_editor_url;
    let mode_idx = state.theme_editor_mode_idx;
    let _ = app;

    Element::col()
        .width_match()
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .padding_xy(16, 14)
        .spacing(8)
        .child(
            Element::label("主题编辑器直连")
                .font_size(14.0)
                .font_weight(600),
        )
        .child(
            Element::label(
                "开启后可用 Web 版主题编辑器直接读写本机主题，仅监听本机地址，不对外网开放。",
            )
            .font_size(12.0)
            .fg_role(Role::TextMuted),
        )
        .child(
            Element::row()
                .spacing(8)
                .child(Element::segmented(
                    vec![
                        "关闭".to_string(),
                        "本次开启".to_string(),
                        "自动开启".to_string(),
                    ],
                    mode_idx,
                ))
                .child(
                    Element::button("打开主题编辑器")
                        .small()
                        .outline_soft()
                        .on_click(|ctx| ctx.open_url(THEME_EDITOR_URL)),
                ),
        )
        .child(
            Element::row()
                .spacing(8)
                .cross(Align::Center)
                .child(
                    Element::label("本地连接地址：尚未开启")
                        .font_size(12.0)
                        .fg_role(Role::TextMuted)
                        .weight(1.0)
                        .visible_when(move || url.get().is_empty()),
                )
                .child(
                    Element::row()
                        .spacing(0)
                        .weight(1.0)
                        .visible_when(move || !url.get().is_empty())
                        .child(
                            Element::label("本地连接地址：")
                                .font_size(12.0)
                                .fg_role(Role::TextMuted),
                        )
                        .child(
                            Element::label_rc(url)
                                .font_size(12.0)
                                .fg_role(Role::TextMuted)
                                .weight(1.0),
                        ),
                )
                .child(
                    Element::button("复制地址")
                        .small()
                        .outline_soft()
                        .enabled_when(move || running.get())
                        .on_click(move |ctx| {
                            let u = url.get();
                            if !u.is_empty() {
                                ctx.clipboard_set(&u);
                                ctx.toast_ok("已复制主题编辑器连接地址");
                            }
                        }),
                ),
        )
}

// ───────────────────────── 词库 ─────────────────────────

fn dict_page(state: &AppState) -> Element {
    dict::build_page(state)
}

// ───────────────────────── 关于 ─────────────────────────

/// 仓库地址（issues/releases 在其后拼段）。与 Vue 版 `App.vue` 的 repoUrl 一致。
const REPO_URL: &str = "https://github.com/huanfeng/WindInput";
/// 官网。
const WEBSITE_URL: &str = "https://windinput.com";
/// 主题编辑器。
const THEME_EDITOR_URL: &str = "https://theme.windinput.com";
/// 主题市场。
const THEME_MARKET_URL: &str = "https://market.windinput.com";
/// QQ 交流群号。
const QQ_GROUP: &str = "1085293418";

// 品牌图标 SVG（白色填充，24×24 viewBox），从 res/icons 嵌入。图标数据取自 Vue 版
// AboutPage，保证与网页版逐像素一致；改图标只改 .svg 文件即可，无需动代码。
const ICON_WEBSITE: &str = include_str!("../../res/icons/website.svg");
const ICON_GITHUB: &str = include_str!("../../res/icons/github.svg");
const ICON_ISSUES: &str = include_str!("../../res/icons/issues.svg");
const ICON_RELEASES: &str = include_str!("../../res/icons/releases.svg");
const ICON_QQ: &str = include_str!("../../res/icons/qq.svg");

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

/// 关于页 — 复刻品牌关于卡（对齐截图 / Vue AboutPage）：
/// 头部（图标+名称+版本徽章+检查更新）· 官网横幅 · 2×2 链接网格 · 版权页脚。
fn about_page(app: &Rc<RefCell<AppState>>, _state: &AppState) -> Element {
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
                    let row = Element::row()
                        .cross(Align::Center)
                        .spacing(10)
                        .child(Element::badge(ver_badge));
                    // 非 Windows 没有升级通路（见 update::self_update_supported）：
                    // 按钮点下去只会下到一个装不上的 .exe，不如不给。
                    if crate::update::self_update_supported() {
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

    // 2×2 链接网格：GitHub / 问题反馈 / 版本发布 / QQ 交流群。
    let github = about_link_card(
        Color::hex(0x24292F),
        ICON_GITHUB,
        "GitHub",
        "源码与文档",
        None,
        |ctx| ctx.open_url(REPO_URL),
    );
    let issues = about_link_card(
        Color::hex(0xF59E0B),
        ICON_ISSUES,
        "问题反馈",
        "报告 Bug 或建议",
        None,
        |ctx| ctx.open_url(&format!("{REPO_URL}/issues")),
    );
    let releases = about_link_card(
        Color::hex(0x22C55E),
        ICON_RELEASES,
        "版本发布",
        "更新日志",
        None,
        |ctx| ctx.open_url(&format!("{REPO_URL}/releases")),
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
                .child(half(issues)),
        )
        .child(
            Element::row()
                .width_match()
                .spacing(10)
                .cross(Align::Stretch)
                .child(half(releases))
                .child(half(qq)),
        );

    let footer = Element::label("© 2026 WindInput Contributors · MIT License")
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(1)
        .height(18)
        .text_align(Align::Center);

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
    crate::widgets::page(body.child(panel))
}
