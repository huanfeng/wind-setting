//! 词库窗中栏 — 当前数据域下的类别；「用户词库」再分出主词库与各分库。
//!
//! 取代原先横在表格上方的子标签行与分库切换器（分段 / 下拉两种形态）和「管理…」对话框：
//! 类别、分库、分库开关都在这一栏，每个节点右键给出它能做的事，栏顶按钮新建或导入分库。
//!
//! 点击只写 `sub_tab`（和分库的选择记忆），重载交给 `view::build_page_for` 里监听
//! `sub_tab` 的 effect，同左栏只写 `domain`。
//!
//! 整栏挂在 `nav_epoch` 上整体重建：切数据域、方案集变化、分库列表变化（含开关与条目数）
//! 时递增。选中态靠 `visible_when` 切换，不需要重建。
//!
//! 样式与主窗二级导航（`subnav`）一致：行高 30、12.5px、选中**不加粗**（只靠强调色、淡底
//! 与竖条）；分组标题同左栏的「全局 / 输入方案」，不可点。

use windui::prelude::*;

use super::sidebar::{SIDE_PAD, group_title, selectable};
use super::spec::LogicalTab;
use super::state::DictManagerState;
use super::user_libs::{MAIN_LABEL, UserLib};

/// 中栏总宽。放得下「快捷输入历史」与「主词库 + 五位条目数」；分库名更长时截断。
pub const CATNAV_W: i32 = 168;

/// 行高，同主窗二级导航。
const ROW_H: i32 = 30;
/// 行内左右内边距，同主窗二级导航。
const ROW_PAD_X: i32 = 10;
/// 勾选框那一列的宽度：分库的勾选框放在这里，主词库空着，两者的名字因此左对齐。
const CHECK_W: i32 = 20;

/// 中栏的一个节点。下标都是当前数据域的子标签下标（`sub_tab` 的定义域）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavNode {
    /// 一个类别。
    Tab(usize),
    /// 「用户词库」分组标题（不可选，右键新建 / 导入分库）。
    LibGroup(usize),
    /// 其余类别的分组标题（不可选、无菜单）。
    Heading,
    /// 主词库。
    Main(usize),
    /// 一个分库（文件主名）。
    Lib(usize, String),
    /// 末行「词库统计」：不是类别，点击打开统计对话框（方案域才有）。
    Stats,
}

impl NavNode {
    /// 节点所在的子标签下标。
    pub fn tab(&self) -> usize {
        match self {
            NavNode::Tab(i) | NavNode::LibGroup(i) | NavNode::Main(i) | NavNode::Lib(i, _) => *i,
            NavNode::Heading | NavNode::Stats => usize::MAX,
        }
    }

    /// 是不是分组标题。
    pub fn is_heading(&self) -> bool {
        matches!(self, NavNode::LibGroup(_) | NavNode::Heading)
    }
}

/// 中栏的一行。
#[derive(Clone, Debug, PartialEq)]
pub struct NavEntry {
    pub node: NavNode,
    pub title: String,
    /// 缩进层级：0 = 类别，1 = 用户词库下的主词库与分库（留出勾选框那一列）。
    pub depth: u8,
    /// 分库：开关状态。
    pub enabled: Option<bool>,
    /// 主词库与分库：条目数。
    pub count: Option<u64>,
    /// 分库：有解析问题。
    pub warn: bool,
}

impl NavEntry {
    fn plain(node: NavNode, title: &str, depth: u8) -> Self {
        Self {
            node,
            title: title.to_string(),
            depth,
            enabled: None,
            count: None,
            warn: false,
        }
    }
}

/// 「用户词库」分组之后其余类别的分组标题。
pub(crate) const OTHER_HEADING: &str = "其他";

/// 末行「词库统计」的标题。说「词库」不说「主词库」：本栏的「主词库」是用户的主分库。
pub(crate) const STATS_TITLE: &str = "词库统计";

/// 按子标签序列与分库列表排出中栏。`libs` 为 `None`（不是方案域、或 core 不认分库）时
/// 「用户词库」只是普通的一类；否则它是一个分组（主词库 + 各分库），其余类别归到「其他」。
/// `main_count` 是主词库的条目数（还没取到为 `None`）。`stats` 为真（方案域）时末尾加一行
/// 「词库统计」。
pub fn nav_entries(
    tabs: &[LogicalTab],
    libs: Option<&[UserLib]>,
    main_count: Option<u64>,
    stats: bool,
) -> Vec<NavEntry> {
    let mut out = Vec::new();
    let mut grouped = false;
    for (i, tab) in tabs.iter().enumerate() {
        match (tab, libs) {
            (LogicalTab::UserDict, Some(libs)) => {
                grouped = true;
                out.push(NavEntry::plain(NavNode::LibGroup(i), tab.title(), 0));
                let mut main = NavEntry::plain(NavNode::Main(i), MAIN_LABEL, 1);
                main.count = main_count;
                out.push(main);
                out.extend(libs.iter().map(|l| NavEntry {
                    node: NavNode::Lib(i, l.name.clone()),
                    title: l.label.clone(),
                    depth: 1,
                    enabled: Some(l.enabled),
                    count: Some(l.entries),
                    warn: !l.issues.is_empty(),
                }));
            }
            _ => {
                if grouped {
                    grouped = false;
                    out.push(NavEntry::plain(NavNode::Heading, OTHER_HEADING, 0));
                }
                out.push(NavEntry::plain(NavNode::Tab(i), tab.title(), 0));
            }
        }
    }
    if stats {
        out.push(NavEntry::plain(NavNode::Stats, STATS_TITLE, 0));
    }
    out
}

impl DictManagerState {
    /// 当前数据域的中栏。
    pub fn nav_entries_now(&self) -> Vec<NavEntry> {
        let libs = self.libs.list.get();
        nav_entries(
            &self.tabs_for_domain(),
            self.has_lib_group().then_some(libs.as_slice()),
            self.libs.main_count.get(),
            self.current_schema_id().is_some(),
        )
    }

    /// 当前子标签下标（越界钳到末项，同 [`Self::current_tab`]）。
    fn current_tab_index(&self) -> usize {
        self.sub_tab
            .get()
            .min(self.tabs_for_domain().len().saturating_sub(1))
    }

    /// 节点是否选中。组标题从不高亮（选中的是它下面的主词库）。
    pub fn is_nav_selected(&self, node: &NavNode) -> bool {
        let cur = self.current_tab_index();
        match node {
            NavNode::Tab(i) => cur == *i,
            NavNode::LibGroup(_) | NavNode::Heading | NavNode::Stats => false,
            NavNode::Main(i) => cur == *i && self.libs.current.get().is_none(),
            NavNode::Lib(i, n) => cur == *i && self.libs.current.get().as_deref() == Some(n),
        }
    }

    /// 选中节点：先记下用户词库里选的是主词库还是哪个分库，再切子标签（切子标签由 effect
    /// 重载；子标签没变而分库变了，由 [`Self::select_lib`] 重载）。
    pub fn select_nav(&self, node: &NavNode) {
        match node {
            NavNode::LibGroup(_) | NavNode::Heading => return,
            NavNode::Stats => {
                self.open_stats_at(self.domain.get());
                return;
            }
            NavNode::Main(_) => self.select_lib(None),
            NavNode::Lib(_, n) => self.select_lib(Some(n.clone())),
            NavNode::Tab(_) => {}
        }
        if self.sub_tab.get() != node.tab() {
            self.sub_tab.set(node.tab());
        }
    }

    /// 右栏标题：类别名；用户词库分了库时带上「主词库」或分库名。
    pub(crate) fn pane_title_now(&self) -> String {
        let tab = self.current_tab();
        if tab != LogicalTab::UserDict || !self.has_lib_group() {
            return tab.title().to_string();
        }
        let part = match self.selected_lib() {
            Some(n) => self
                .libs
                .list
                .get()
                .into_iter()
                .find(|l| l.name == n)
                .map_or(n, |l| l.label),
            None => MAIN_LABEL.to_string(),
        };
        format!("{} · {part}", tab.title())
    }

    /// 节点的右键菜单。右键先选中该节点（同文件管理器），菜单项因此都按「当前」来做。
    /// 分组标题不可选，右键也不换掉正在看的那一项。
    pub fn nav_menu_items(&self, node: &NavNode) -> Vec<MenuItem> {
        // 「词库统计」是动作行：右键不能等同于点开它。
        if *node == NavNode::Stats {
            return Vec::new();
        }
        self.select_nav(node);
        match node {
            NavNode::LibGroup(_) => self.lib_add_menu_items(),
            NavNode::Heading | NavNode::Stats => Vec::new(),
            NavNode::Lib(_, name) => self.lib_menu_items(name),
            NavNode::Tab(_) | NavNode::Main(_) => self.file_menu_items(),
        }
    }

    /// 「用户词库」组标题与栏顶「+」的菜单。
    pub fn lib_add_menu_items(&self) -> Vec<MenuItem> {
        let (m_new, m_imp, m_dir) = (self.clone(), self.clone(), self.clone());
        vec![
            MenuItem::run("新建分库…", move |_| m_new.open_lib_create(), false),
            MenuItem::run(
                "从文件导入为分库…",
                move |ctx| m_imp.pick_lib_import_file(ctx),
                false,
            ),
            MenuItem::separator(),
            MenuItem::run("打开分库目录", move |_| m_dir.open_libs_dir(), false),
        ]
    }

    fn lib_menu_items(&self, name: &str) -> Vec<MenuItem> {
        let Some(lib) = self.libs.list.get().into_iter().find(|l| l.name == name) else {
            return Vec::new();
        };
        let (m_on, n_on, next) = (self.clone(), lib.name.clone(), !lib.enabled);
        let (m_edit, n_edit) = (self.clone(), lib.name.clone());
        let (m_ren, n_ren) = (self.clone(), lib.name.clone());
        let m_dir = self.clone();
        let (m_del, n_del, l_del) = (self.clone(), lib.name.clone(), lib.label.clone());
        vec![
            MenuItem::run(
                if lib.enabled { "停用" } else { "启用" },
                move |_| {
                    m_on.set_lib_enabled(&n_on, next);
                },
                false,
            ),
            MenuItem::run("编辑文件", move |_| m_edit.edit_lib_file(&n_edit), false),
            MenuItem::run("重命名…", move |_| m_ren.open_lib_rename(&n_ren), false),
            MenuItem::run("打开分库目录", move |_| m_dir.open_libs_dir(), false),
            MenuItem::separator(),
            MenuItem::run(
                "删除分库…",
                move |_| m_del.request_delete_user_lib(&n_del, &l_del),
                false,
            )
            .danger(),
        ]
    }

    /// 普通类别与主词库：这一类与文件相关的操作，最后是打开数据目录。
    fn file_menu_items(&self) -> Vec<MenuItem> {
        let mut items = Vec::new();
        if self.edit_file_target().is_some() {
            let m = self.clone();
            items.push(MenuItem::run("编辑文件", move |_| m.do_edit_file(), false));
        }
        if self.io_entry_visible() && self.io_entry_enabled() {
            let (m_imp, m_exp) = (self.clone(), self.clone());
            items.push(MenuItem::run(
                "导入…",
                move |ctx| m_imp.do_import(ctx),
                false,
            ));
            items.push(MenuItem::run(
                "导出…",
                move |ctx| m_exp.do_export(ctx),
                false,
            ));
        }
        if !items.is_empty() {
            items.push(MenuItem::separator());
        }
        let m = self.clone();
        items.push(MenuItem::run(
            "打开数据目录",
            move |_| m.open_data_dir(),
            false,
        ));
        items
    }
}

/// 构建中栏：栏顶（数据域名 + 分库「+」）与节点列表，整体挂在 `nav_epoch` 上重建。
/// 底色同主窗二级导航（`SurfaceAlt`，与右栏同色、靠分隔线分开）。
pub fn build_category_nav(mgr: &DictManagerState) -> Element {
    let (m_head, m_list) = (mgr.clone(), mgr.clone());
    let head = Element::host_signal(mgr.nav_epoch, move |_| build_head(&m_head));
    let list =
        Element::host_signal(mgr.nav_epoch, move |_| build_list(&m_list)).padding_xy(SIDE_PAD, 0);
    Element::col()
        .width(CATNAV_W)
        .height_match()
        .bg_role(Role::SurfaceAlt)
        .padding_xy(0, 10)
        .spacing(4)
        .child(head)
        .child(Element::scroll().fill().weight(1.0).child(list))
}

/// 栏顶：数据域名（去掉「 · 引擎」后缀，同左栏）+ 有分库时的「+」。
fn build_head(mgr: &DictManagerState) -> Element {
    let label = mgr.domain_label_at(mgr.domain.get());
    let name = label
        .split_once(" · ")
        .map_or(label.clone(), |(n, _)| n.to_string());
    let mut row = Element::row()
        .width_match()
        .height(28)
        .cross(Align::Center)
        .spacing(6)
        .padding_xy(SIDE_PAD + ROW_PAD_X, 0)
        .child(
            Element::label(name)
                .font_size(13.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .max_lines(1)
                .weight(1.0),
        );
    if mgr.has_lib_group() {
        let m = mgr.clone();
        row = row.child(
            Element::button("+")
                .small()
                .outline_soft()
                .tooltip("新建或导入分库")
                .on_click(move |ctx| {
                    let items = m.lib_add_menu_items();
                    ctx.show_dropdown_menu(ctx.bounds(), items);
                }),
        );
    }
    row
}

fn build_list(mgr: &DictManagerState) -> Element {
    let mut col = Element::col().width_match().spacing(1);
    for e in mgr.nav_entries_now() {
        if e.node == NavNode::Stats {
            // 与上面的类别隔开一点：它不是又一个类别。
            col = col.child(Element::leaf().height(8));
        }
        col = col.child(if e.node.is_heading() {
            heading_row(mgr, &e)
        } else {
            nav_row(mgr, e)
        });
    }
    col
}

/// 分组标题：同左栏「全局 / 输入方案」。「用户词库」那条右键给新建 / 导入分库。
fn heading_row(mgr: &DictManagerState, e: &NavEntry) -> Element {
    let el = group_title(&e.title);
    match e.node {
        NavNode::LibGroup(_) => {
            let (m, node) = (mgr.clone(), e.node.clone());
            el.on_context_menu(move || m.nav_menu_items(&node))
        }
        _ => el,
    }
}

/// 一行：选中样式同左栏（`selectable`），行本体同主窗二级导航；右键给节点菜单。
fn nav_row(mgr: &DictManagerState, e: NavEntry) -> Element {
    let (m_sel, m_click, m_menu) = (mgr.clone(), mgr.clone(), mgr.clone());
    let (n_sel, n_click, n_menu) = (e.node.clone(), e.node.clone(), e.node.clone());
    // 分库的勾选框：选中 / 未选中两套行共用一个信号，开关失败时退回。
    let toggle = e.enabled.map(|on| {
        let sig = signal(on);
        let (m, name) = (
            mgr.clone(),
            match &e.node {
                NavNode::Lib(_, n) => n.clone(),
                _ => String::new(),
            },
        );
        (sig, m, name)
    });
    selectable(
        ROW_H,
        move || m_sel.is_nav_selected(&n_sel),
        move || m_click.select_nav(&n_click),
        move |active| {
            let mut row = Element::row()
                .width_match()
                .height(ROW_H)
                .corner(7.0)
                .cross(Align::Center)
                .spacing(4)
                .padding_xy(ROW_PAD_X, 0);
            // 用户词库分组里的行留出勾选框那一列：分库放勾选框，主词库空着，名字对齐。
            if e.depth > 0 {
                let cell = Element::row().width(CHECK_W).cross(Align::Center);
                row = row.child(match &toggle {
                    // 主词库：与分库对称也有勾选框，但它总是开着、点不动。
                    None if matches!(e.node, NavNode::Main(_)) => cell.child(
                        Element::checkbox("", signal(true))
                            .small()
                            .enabled(false)
                            .tooltip("主词库总是启用"),
                    ),
                    Some((sig, m, name)) => {
                        let (sig, m, name) = (*sig, m.clone(), name.clone());
                        cell.child(Element::checkbox("", sig).small().on_toggle(move |_| {
                            let next = !sig.get();
                            sig.set(next);
                            if !m.set_lib_enabled(&name, next) {
                                sig.set(!next);
                            }
                        }))
                    }
                    None => cell,
                });
            }
            // 字重两态相同（500）：同主窗二级导航，加粗的中文糊、且选中时文字会横移。
            row = row.child(
                Element::label(e.title.clone())
                    .font_size(12.5)
                    .font_weight(500)
                    .fg_role(if active {
                        Role::Accent
                    } else {
                        Role::TextMuted
                    })
                    .weight(1.0)
                    .max_lines(1)
                    .height(17),
            );
            if e.warn {
                row = row.child(
                    Element::label("⚠")
                        .font_size(11.0)
                        .fg_role(Role::Warning)
                        .tooltip("文件里有格式问题，选中后看详情"),
                );
            }
            if let Some(n) = e.count {
                row = row.child(
                    Element::label(n.to_string())
                        .font_size(11.0)
                        .fg_role(Role::TextDisabled)
                        .max_lines(1),
                );
            }
            row
        },
    )
    .on_context_menu(move || m_menu.nav_menu_items(&n_menu))
}

#[cfg(test)]
mod tests {
    use super::super::spec::WordCategory;
    use super::*;
    use serde_json::json;

    fn lib(name: &str, enabled: bool) -> UserLib {
        UserLib {
            name: name.into(),
            label: name.into(),
            enabled,
            entries: 7,
            file: format!("wb/dicts/{name}.dict.yaml"),
            issues: Vec::new(),
        }
    }

    fn titles(v: &[NavEntry]) -> Vec<(String, u8)> {
        v.iter().map(|e| (e.title.clone(), e.depth)).collect()
    }

    #[test]
    fn user_dict_splits_into_main_and_libraries() {
        let tabs = [
            LogicalTab::UserDict,
            LogicalTab::Temp,
            LogicalTab::Freq,
            LogicalTab::Shadow,
        ];
        let libs = [lib("成语", true), lib("行业词", false)];
        let v = nav_entries(&tabs, Some(&libs), Some(1203), false);
        assert_eq!(
            titles(&v),
            [
                ("用户词库".into(), 0),
                ("主词库".into(), 1),
                ("成语".into(), 1),
                ("行业词".into(), 1),
                ("其他".into(), 0),
                ("临时词库".into(), 0),
                ("词频".into(), 0),
                ("候选调整".into(), 0),
            ]
        );
        assert_eq!(v[0].node, NavNode::LibGroup(0));
        assert!(v[0].node.is_heading() && v[4].node.is_heading());
        assert_eq!(v[1].node, NavNode::Main(0));
        assert_eq!(v[1].count, Some(1203), "主词库也显示条目数");
        assert_eq!(v[3].node, NavNode::Lib(0, "行业词".into()));
        assert_eq!(v[3].enabled, Some(false));
        assert_eq!(v[3].count, Some(7));
        assert_eq!(v[5].node, NavNode::Tab(1), "后面的类别下标不受分库行数影响");
        assert_eq!(v[7].node, NavNode::Tab(3));
    }

    /// 没有分库能力（全局域、旧 core）：用户词库就是普通一类；空分库列表仍分出主词库。
    #[test]
    fn without_library_support_user_dict_is_plain() {
        let tabs = [LogicalTab::UserDict, LogicalTab::Temp];
        assert_eq!(
            titles(&nav_entries(&tabs, None, None, false)),
            [("用户词库".into(), 0), ("临时词库".into(), 0)]
        );
        assert_eq!(
            titles(&nav_entries(&tabs, Some(&[]), None, false)),
            [
                ("用户词库".into(), 0),
                ("主词库".into(), 1),
                ("其他".into(), 0),
                ("临时词库".into(), 0)
            ]
        );
        let phrase = [LogicalTab::PhraseSystem, LogicalTab::PhraseUser];
        let v = nav_entries(&phrase, Some(&[lib("x", true)]), None, false);
        assert_eq!(titles(&v), [("系统短语".into(), 0), ("用户短语".into(), 0)]);
    }

    /// 方案域末尾多一行「词库统计」：不是类别，下标不占子标签。
    #[test]
    fn schema_domain_ends_with_stats_row() {
        let tabs = [LogicalTab::UserDict, LogicalTab::Temp];
        let v = nav_entries(&tabs, Some(&[]), None, true);
        let last = v.last().unwrap();
        assert_eq!(
            (last.title.as_str(), &last.node),
            (STATS_TITLE, &NavNode::Stats)
        );
        assert_eq!(last.node.tab(), usize::MAX);
        assert!(!last.node.is_heading());
    }

    // ── 状态（mock RPC）────────────────────────────────────

    use crate::state::SchemaInfo;

    fn with_mock<R>(f: impl FnOnce() -> R) -> R {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        crate::rpc::take_mock_calls();
        let r = f();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        r
    }

    /// 一个码表方案，停在用户词库上（mock 分库：成语、hangye）。
    fn state() -> DictManagerState {
        let st = DictManagerState::new();
        *st.schemas.borrow_mut() = vec![SchemaInfo {
            id: "wb".into(),
            name: "五笔".into(),
            kind: "codetable".into(),
            ..Default::default()
        }];
        st.domain.set(super::super::state::GLOBAL_DOMAINS);
        st.sub_tab.set(0);
        st.on_nav_changed();
        st
    }

    /// 测试里没有 effect：切子标签后手动走一遍导航。
    fn click(st: &DictManagerState, node: &NavNode) {
        let before = st.sub_tab.get();
        st.select_nav(node);
        if st.sub_tab.get() != before {
            st.on_nav_changed();
        }
    }

    fn labels(items: &[MenuItem]) -> Vec<String> {
        items
            .iter()
            .map(|i| {
                if i.separator {
                    "─".to_string()
                } else {
                    i.label.clone()
                }
            })
            .collect()
    }

    #[test]
    fn clicking_nodes_switches_category_and_library() {
        with_mock(|| {
            let st = state();
            let v = st.nav_entries_now();
            assert!(st.is_nav_selected(&v[1].node), "默认主词库");
            assert_eq!(
                v[1].count,
                Some(3),
                "主词库条目数取自 dict.listPaged 的总数"
            );
            assert_eq!(st.pane_title.get(), "用户词库 · 主词库");

            click(&st, &NavNode::Lib(0, "成语".into()));
            assert_eq!(st.current_category(), WordCategory::UserLib);
            assert!(st.is_nav_selected(&NavNode::Lib(0, "成语".into())));
            assert!(!st.is_nav_selected(&NavNode::Main(0)));
            assert_eq!(st.pane_title.get(), "用户词库 · 成语");

            click(&st, &NavNode::Tab(1));
            assert_eq!(st.current_category(), WordCategory::Temp);
            assert!(!st.is_nav_selected(&NavNode::Lib(0, "成语".into())));
            assert_eq!(st.pane_title.get(), "临时词库");

            // 从别的类别直接点分库：一步到位
            click(&st, &NavNode::Lib(0, "hangye".into()));
            assert_eq!(st.current_category(), WordCategory::UserLib);
            assert_eq!(st.selected_lib().as_deref(), Some("hangye"));
            assert_eq!(st.pane_title.get(), "用户词库 · 行业词");

            // 分组标题不可选：点了什么都不变
            click(&st, &NavNode::LibGroup(0));
            assert_eq!(st.selected_lib().as_deref(), Some("hangye"));
            assert!(!st.is_nav_selected(&NavNode::LibGroup(0)));
        });
    }

    /// 换域的跳转（导入后带过去、深链）也要重建中栏：两个全局域之间跳，分库列表前后都空，
    /// 不能指望分库列表变化来触发。
    #[test]
    fn jumping_to_another_domain_rebuilds_the_column() {
        with_mock(|| {
            let st = DictManagerState::new();
            st.on_nav_changed();
            let before = st.nav_epoch.get()[0];
            st.navigate_to(super::super::state::DOMAIN_QUICK, None);
            st.on_nav_changed(); // effect
            assert!(st.nav_epoch.get()[0] > before, "中栏应重建");
            assert_eq!(st.nav_entries_now()[0].title, "快捷输入");
        });
    }

    /// 在分库里删了词：中栏的条目数跟上表格总数（mock 列表报 3 条，表格实有 2 条）。
    #[test]
    fn library_count_follows_the_table() {
        with_mock(|| {
            let st = state();
            let before = st.nav_epoch.get()[0];
            click(&st, &NavNode::Lib(0, "成语".into()));
            let lib = st
                .libs
                .list
                .get()
                .into_iter()
                .find(|l| l.name == "成语")
                .unwrap();
            assert_eq!(lib.entries, 2);
            assert!(st.nav_epoch.get()[0] > before);
        });
    }

    /// 正在看的分库没了：回到主词库，分库里勾着的行一并清掉（否则「删除选中」会去删主词库
    /// 里的同名词）。
    #[test]
    fn vanished_library_clears_the_checked_rows() {
        with_mock(|| {
            let st = state();
            click(&st, &NavNode::Lib(0, "成语".into()));
            let row = st.display_rows.borrow()[0].clone();
            st.toggle_selected(&row);
            assert_eq!(st.selected_count.get(), 1);
            st.libs
                .memory
                .borrow_mut()
                .insert("wb".into(), "已删".into());
            st.reload_with_libs();
            assert_eq!(st.current_category(), WordCategory::UserDict);
            assert_eq!(st.selected_count.get(), 0);
        });
    }

    /// 左栏右键：先选中该条目；方案有数据才给「删除此方案的用户数据…」，确认后调 core。
    #[test]
    fn domain_menu_selects_and_offers_purge() {
        with_mock(|| {
            let st = state();
            let phrase = super::super::state::DOMAIN_PHRASE;
            assert_eq!(labels(&st.domain_menu_items(phrase)), ["打开数据目录"]);
            assert_eq!(st.domain.get(), phrase, "右键先选中");
            let wb = super::super::state::GLOBAL_DOMAINS;
            let items = st.domain_menu_items(wb);
            assert_eq!(
                labels(&items),
                ["词库统计…", "打开数据目录", "─", "删除此方案的用户数据…"]
            );
            let windui::event::MenuAction::Run(f) = &items[0].action else {
                panic!("应是普通动作");
            };
            windui::testing::run_with_ctx(|ctx| f(ctx));
            assert!(st.stats.visible.get(), "右键菜单与中栏末行打开同一个对话框");
            assert_eq!(st.stats.view.borrow().schema_id, "wb");
            let windui::event::MenuAction::Run(f) = &items[3].action else {
                panic!("应是普通动作");
            };
            windui::testing::run_with_ctx(|ctx| f(ctx));
            assert!(st.confirm_visible.get(), "先确认");
            crate::rpc::take_mock_calls();
            st.confirm_pending();
            let calls: Vec<_> = crate::rpc::take_mock_calls()
                .into_iter()
                .filter(|(m, _)| m == "userdata.purgeSchema")
                .map(|(_, p)| p)
                .collect();
            assert_eq!(calls, vec![json!({"schemaId": "wb"})]);
        });
    }

    /// 右键组标题不换掉正在看的分库。
    #[test]
    fn right_clicking_the_group_keeps_the_selection() {
        with_mock(|| {
            let st = state();
            click(&st, &NavNode::Lib(0, "成语".into()));
            st.nav_menu_items(&NavNode::LibGroup(0));
            assert_eq!(st.selected_lib().as_deref(), Some("成语"));
        });
    }

    #[test]
    fn library_menu_acts_on_that_library() {
        with_mock(|| {
            let st = state();
            let items = st.nav_menu_items(&NavNode::Lib(0, "hangye".into()));
            assert_eq!(
                labels(&items),
                [
                    "启用",
                    "编辑文件",
                    "重命名…",
                    "打开分库目录",
                    "─",
                    "删除分库…"
                ],
                "mock 里行业词是关着的"
            );
            assert_eq!(st.selected_lib().as_deref(), Some("hangye"), "右键先选中");
            crate::rpc::take_mock_calls();
            let windui::event::MenuAction::Run(f) = &items[0].action else {
                panic!("启用项应是普通动作");
            };
            windui::testing::run_with_ctx(|ctx| f(ctx));
            let calls: Vec<_> = crate::rpc::take_mock_calls()
                .into_iter()
                .filter(|(m, _)| m == "userDicts.setEnabled")
                .map(|(_, p)| p)
                .collect();
            assert_eq!(
                calls,
                vec![json!({"schemaId": "wb", "name": "hangye", "enabled": true})]
            );
        });
    }

    /// 点「词库统计」打开对话框、不切类别；右键它不弹菜单也不打开。全局域没有这一行。
    #[test]
    fn stats_row_opens_the_dialog() {
        with_mock(|| {
            let st = state();
            let v = st.nav_entries_now();
            assert_eq!(v.last().unwrap().node, NavNode::Stats);
            assert!(st.nav_menu_items(&NavNode::Stats).is_empty());
            assert!(!st.stats.visible.get(), "右键不打开");
            let before = st.sub_tab.get();
            click(&st, &NavNode::Stats);
            assert!(st.stats.visible.get());
            assert_eq!(st.sub_tab.get(), before, "不切类别");
            assert!(!st.is_nav_selected(&NavNode::Stats));
            assert_eq!(st.stats.view.borrow().schema_id, "wb");
            // 取数前标题用左栏的方案名，回包后换成 core 给的
            assert_eq!(st.stats.view.borrow().title, "五笔 86");

            st.domain.set(super::super::state::DOMAIN_PHRASE);
            assert!(
                !st.nav_entries_now()
                    .iter()
                    .any(|e| e.node == NavNode::Stats),
                "全局域没有系统词库"
            );
        });
    }

    #[test]
    fn group_and_file_menus() {
        with_mock(|| {
            let st = state();
            assert_eq!(
                labels(&st.nav_menu_items(&NavNode::LibGroup(0))),
                ["新建分库…", "从文件导入为分库…", "─", "打开分库目录"]
            );
            assert_eq!(
                labels(&st.nav_menu_items(&NavNode::Main(0))),
                ["编辑文件", "导入…", "导出…", "─", "打开数据目录"]
            );
            assert_eq!(
                labels(&st.nav_menu_items(&NavNode::Tab(1))),
                ["编辑文件", "导入…", "导出…", "─", "打开数据目录"],
                "临时词库也能编辑文件"
            );
        });
    }
}
