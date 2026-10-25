//! 设置项 Ctrl+右键菜单的端到端验证：真的建出主窗，在内容区里按 Ctrl+右键 / 裸右键，
//! 看 windui 交出什么菜单。
//!
//! 放在这里而不是 `hide_menu` 单测：那边只验证「菜单项构造」，这里验证**挂载**
//! （`section_rows_split` / `manifest_sections` 有没有把构建器接到行与卡片上）与
//! **修饰键判定**（裸右键必须让路给既有菜单）。两个缺口都只表现为「右键没反应」，
//! 单测各自全绿。
//!
//! 选点策略是扫描而不是按节点定位：本仓没有「锚点键 → NodeId」的公开映射，为测试补一个
//! 会把生产代码弄脏；而内容区只有几百个候选点，单测里可以忽略。

use std::cell::RefCell;
use std::rc::Rc;

use windui::core::Tree;
use windui::event::{Mods, MouseButton, PointerEvent, PointerKind};
use windui::geometry::{Point, Size};

use crate::state::AppState;
use crate::user_hide::{UserHideConfig, UserHideStore};

/// 本功能菜单的识别特征：与 `hide_menu` 里那几项文案同源。
///
/// 项级与节级都要收：行菜单只给项级，卡片菜单只给节级，两类目标都得认出来
/// （见 `hide_menu::build_items`）。
const MARKERS: &[&str] = &[
    "隐藏这一项",
    "取消隐藏这一项",
    "隐藏这一节",
    "取消隐藏这一节",
    "隐藏这一块",
    "取消隐藏这一块",
];

struct Fixture {
    /// 持有 `AppState`：树里的信号与闭包都指向它，提前释放会有悬垂句柄。
    _app: Rc<RefCell<AppState>>,
    tree: Tree,
    content: windui::geometry::Rect,
    /// 内容区那一格的节点 id（`content` 矩形的来源），供找页面滚动容器。
    content_node: windui::core::NodeId,
}

fn fixture(store: UserHideStore) -> Fixture {
    fixture_on(store, "input")
}

/// 同 [`fixture`]，但可指定落在哪一页（方案页的行是手写的另一条构建路径）。
fn fixture_on(store: UserHideStore, page: &str) -> Fixture {
    let _g = crate::rpc::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let app = Rc::new(RefCell::new(AppState::empty()));
    app.borrow_mut().user_hide_store = Rc::new(store);
    let (w, h) = (1000, 700);
    let mut tree = Tree::new();
    let root = crate::shell::build(&app)
        .width(w)
        .height(h)
        .build(&mut tree);
    tree.root = Some(root);
    tree.layout_root(Size::new(w, h), &mut windui::text::NullTextEngine);
    let idx = crate::cli::page_to_index(page).expect("页 id");
    app.borrow().tab.set(idx);
    tree.layout_root(Size::new(w, h), &mut windui::text::NullTextEngine);
    // 根 stack → 主列 [标题栏, 分隔线, 主体, 分隔线, 底栏]；
    // 主体 = [侧栏, 分隔线, 二级导航, 分隔线, 内容]。
    let main_col = tree.get(root).unwrap().children[0];
    let body = tree.get(main_col).unwrap().children[2];
    let content_node = *tree.get(body).unwrap().children.last().unwrap();
    let content = tree.abs_bounds(content_node);
    Fixture {
        _app: app,
        tree,
        content,
        content_node,
    }
}

fn menu_labels(items: &[windui::event::MenuItem]) -> Vec<String> {
    items
        .iter()
        .filter(|i| !i.separator)
        .map(|i| i.label.clone())
        .collect()
}

/// 在内容区里逐点右键，返回**第一个满足 `accept` 的**那份菜单。
///
/// 只认 [`MARKERS`]：内容区里文本框自带右键菜单（复制/粘贴），把它当成命中会让
/// 「功能关闭时不该弹」那一条假阳性。
fn scan(f: &mut Fixture, ctrl: bool, accept: impl Fn(&[String]) -> bool) -> Option<Vec<String>> {
    let (x0, y0) = (f.content.x, f.content.y);
    let (w, h) = (f.content.w, f.content.h);
    let mut y = y0 + 8;
    while y < y0 + h {
        let mut x = x0 + 24;
        while x < x0 + w {
            let ev = PointerEvent {
                kind: PointerKind::Down,
                pos: Point::new(x, y),
                button: MouseButton::Right,
                click_count: 1,
                mods: Mods {
                    ctrl,
                    ..Default::default()
                },
            };
            let (mut hover, mut capture) = (None, None);
            let res = f.tree.dispatch_pointer(ev, &mut hover, &mut capture);
            if let Some(m) = res.menu {
                let labels = menu_labels(&m.items);
                if labels.iter().any(|l| MARKERS.contains(&l.as_str())) && accept(&labels) {
                    return Some(labels);
                }
            }
            x += 40;
        }
        y += 24;
    }
    None
}

fn enabled_store() -> UserHideStore {
    UserHideStore::in_memory(UserHideConfig {
        enabled: true,
        show_hidden: false,
        hide: Vec::new(),
    })
}

/// Ctrl+右键落在设置行上时，只弹项级动作——**不含**「隐藏这一节」。
///
/// 两档同时出现在行菜单里，每次右键都要分辨「这一项 / 这一节」；整节隐藏仍可从卡片
/// 空白处拿到（见下一条测试），故行上不再重复。
#[test]
fn ctrl_right_click_on_a_row_offers_only_item_actions() {
    let mut f = fixture(enabled_store());
    let labels = scan(&mut f, true, |ls| {
        ls.iter()
            .any(|l| l == "隐藏这一项" || l == "取消隐藏这一项")
    })
    .expect("内容区里应在某一行上弹出含项级动作的隐藏菜单");
    assert!(
        labels.iter().any(|l| l == "复制选择器"),
        "应能复制选择器：{labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l == "隐藏这一节"),
        "行菜单不该再给整节隐藏：{labels:?}"
    );
}

/// 卡片空白处 Ctrl+右键：只有节级动作，没有「隐藏这一项」（卡片不是一项）。
#[test]
fn ctrl_right_click_on_a_card_offers_only_section_actions() {
    let mut f = fixture(enabled_store());
    let labels = scan(&mut f, true, |ls| {
        ls.iter()
            .any(|l| l == "隐藏这一节" || l == "取消隐藏这一节")
            && !ls
                .iter()
                .any(|l| l == "隐藏这一项" || l == "取消隐藏这一项")
    })
    .expect("卡片空白处应弹出含整节隐藏的菜单");
    assert_eq!(labels, vec!["隐藏这一节", "复制选择器"]);
}

/// 功能关闭时这一手势不存在：扫遍内容区也拿不到本功能的菜单。
#[test]
fn ctrl_right_click_is_inert_when_feature_disabled() {
    let mut f = fixture(UserHideStore::in_memory(UserHideConfig::default()));
    assert!(
        scan(&mut f, true, |_| true).is_none(),
        "总开关关闭时不该弹本功能的菜单"
    );
}

/// 裸右键不弹本功能那一份（让路给既有菜单 / 文本框自己的剪贴板菜单）。
#[test]
fn bare_right_click_does_not_open_the_hide_menu() {
    let mut f = fixture(enabled_store());
    assert!(
        scan(&mut f, false, |_| true).is_none(),
        "裸右键不该出现隐藏菜单"
    );
}

/// **即时生效的守门测试**：窗口建好之后才往清单里加一条规则，重排一帧，页面必须当场变矮。
///
/// 这条钉的是「判据挂在节点上、读活配置」这件事。若哪天有人把它改回「建树时有规则才挂
/// 判据」或「折进 manifest.hidden」，这里当场失败——那种写法下，窗口开着期间新加的规则
/// 要等重开窗口才生效，而用户正是在窗口里一边藏一边看效果的。
///
/// 判据取**滚动容器的内容总高**：藏掉一节整卡后，内容列少了那张卡的高度。
/// 不看单个节点的 bounds —— 本仓没有「锚点键 → NodeId」的公开映射。
#[test]
fn rule_added_after_the_tree_is_built_takes_effect_immediately() {
    use windui::anim;

    let mut f = fixture(enabled_store());
    let (group, section) = ("input", "字符与标点");
    let store = f._app.borrow().user_hide_store.clone();
    assert!(!store.hides_section(group, section), "前提：尚未隐藏");

    let height_before = page_content_height(&f);
    assert!(height_before > 0, "前提：页面已排过版");

    // 建树**之后**才藏。
    store.add(&format!("section:{group}/{section}")).unwrap();
    assert!(store.hides_section(group, section));

    // 请求重排并跑一帧：`visible_when` 在布局期现求。
    anim::request_relayout();
    f.tree
        .layout_root(Size::new(1000, 700), &mut windui::text::NullTextEngine);
    let height_after = page_content_height(&f);

    assert!(
        height_after < height_before,
        "藏掉一整节后页面应变矮：{height_before} → {height_after}"
    );
}

/// 当前**可见页**的内容总高（`content_h`）。
///
/// 只找可见的那个滚动容器：全部页面叠在一个 stack 里靠 `visible_when` 切换，未选中的
/// 页面节点仍在树上（只是不可见），其 `content_h` 也是非零的旧值。判据取「祖先链全可见」，
/// 与 `subnav::Spy` 同一套做法。
fn page_content_height(f: &Fixture) -> i32 {
    fn visible_chain(tree: &Tree, id: windui::core::NodeId) -> bool {
        let mut cur = Some(id);
        while let Some(i) = cur {
            let Some(n) = tree.get(i) else { return false };
            if !n.effective_visible() {
                return false;
            }
            cur = n.parent;
        }
        true
    }
    fn find_scroll(tree: &Tree, id: windui::core::NodeId, out: &mut Option<windui::core::NodeId>) {
        let Some(n) = tree.get(id) else { return };
        if matches!(n.layout, windui::core::Layout::Scroll) && visible_chain(tree, id) {
            *out = Some(id);
            return;
        }
        for &c in &n.children {
            find_scroll(tree, c, out);
            if out.is_some() {
                return;
            }
        }
    }
    let mut found = None;
    find_scroll(&f.tree, f.content_node, &mut found);
    found
        .and_then(|s| f.tree.get(s).map(|n| n.content_h))
        .unwrap_or(0)
}

/// 方案页的四张引擎卡（码表 / 拼音 / 混输 / 英文）里，**每一行**也要有项级菜单。
///
/// 这几张卡早期只挂了卡片级的「隐藏这一节」，于是右键某一行只能藏整节——用户报的
/// 「拼音里的一些设置只能隐藏整节」就是这条（2026-10-25）。
#[test]
fn scheme_engine_rows_offer_item_level_actions() {
    let mut f = fixture_on(enabled_store(), "schema");
    let labels = scan(&mut f, true, |ls| {
        ls.iter()
            .any(|l| l == "隐藏这一项" || l == "取消隐藏这一项")
    })
    .expect("方案页的引擎卡里，某一行应弹出项级菜单");
    assert!(
        labels.iter().any(|l| l == "复制选择器"),
        "应能复制选择器：{labels:?}"
    );
}

/// 整节隐藏仍然可达：卡片**标题栏**上 Ctrl+右键只给节级动作。
#[test]
fn section_hide_is_still_reachable_from_the_header() {
    let mut f = fixture_on(enabled_store(), "input");
    let labels = scan(&mut f, true, |ls| {
        ls.iter()
            .any(|l| l == "隐藏这一节" || l == "取消隐藏这一节")
            && !ls
                .iter()
                .any(|l| l == "隐藏这一项" || l == "取消隐藏这一项")
    })
    .expect("标题栏上应能藏整节");
    assert_eq!(labels, vec!["隐藏这一节", "复制选择器"]);
}

/// 节级菜单只挂在**标题栏**，不挂整张卡片——这是用户报的「操作区边缘容易被划进整节」
/// 那个问题的守门测试。
///
/// 判据：沿内容区纵向逐行扫描，记录「能弹出节级菜单」的 y 区间。菜单若挂在整张卡片上，
/// 一张高几百像素的卡会整片命中；只挂标题栏时，命中只出现在每张卡顶部那条窄带里。
/// 故断言最长的一段连续命中**不超过标题栏量级**（卡内还有行级菜单与控件，采样点不连续）。
#[test]
fn section_menu_only_covers_the_header_band() {
    let mut f = fixture_on(enabled_store(), "input");
    let (x0, y0) = (f.content.x, f.content.y);
    let (w, h) = (f.content.w, f.content.h);
    // 取一条靠右的纵向采样线：那里通常落在卡片内边距里，不会被行的控件挡住。
    let x = x0 + w - 40;
    let mut runs: Vec<(i32, i32)> = Vec::new();
    let mut cur: Option<i32> = None;
    let mut y = y0 + 4;
    while y < y0 + h {
        let ev = PointerEvent {
            kind: PointerKind::Down,
            pos: Point::new(x, y),
            button: MouseButton::Right,
            click_count: 1,
            mods: Mods {
                ctrl: true,
                ..Default::default()
            },
        };
        let (mut hover, mut capture) = (None, None);
        let res = f.tree.dispatch_pointer(ev, &mut hover, &mut capture);
        let is_section = res.menu.as_ref().is_some_and(|m| {
            menu_labels(&m.items)
                .iter()
                .any(|l| l == "隐藏这一节" || l == "取消隐藏这一节")
        });
        match (is_section, cur) {
            (true, None) => cur = Some(y),
            (false, Some(start)) => {
                runs.push((start, y - start));
                cur = None;
            }
            _ => {}
        }
        y += 4;
    }
    if let Some(start) = cur {
        runs.push((start, y - start));
    }
    assert!(!runs.is_empty(), "标题栏上应能弹出节级菜单");
    let longest = runs.iter().map(|(_, len)| *len).max().unwrap_or(0);
    assert!(
        longest <= 120,
        "节级菜单的命中区不该超过标题栏量级（最长 {longest}px，共 {runs:?}）——\
         说明它又挂回整张卡片了"
    );
}
