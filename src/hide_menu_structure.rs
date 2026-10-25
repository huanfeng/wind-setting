//! 节级菜单挂在**标题栏**而不是整张卡片——结构级守门测试。
//!
//! 单独成文件而不是并进 `hide_menu_e2e`：那边靠坐标扫描，只能证「某处能弹」，
//! 证不了「卡片正文不能弹」。这里直接走树：找出所有带 `context_menu` 的节点，
//! 按它们的**高度**分类——标题栏是一条窄带（`section_header` 约 32px），
//! 卡片正文若挂了菜单，会出现与整卡同高的节点。

use std::cell::RefCell;
use std::rc::Rc;

use windui::core::{NodeId, Tree};
use windui::event::{Mods, MouseButton, PointerEvent, PointerKind};
use windui::geometry::{Point, Size};

use crate::state::AppState;
use crate::user_hide::{UserHideConfig, UserHideStore};

struct Fixture {
    _app: Rc<RefCell<AppState>>,
    tree: Tree,
}

fn fixture() -> Fixture {
    let _g = crate::rpc::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let app = Rc::new(RefCell::new(AppState::empty()));
    app.borrow_mut().user_hide_store = Rc::new(UserHideStore::in_memory(UserHideConfig {
        enabled: true,
        show_hidden: false,
        hide: Vec::new(),
    }));
    let (w, h) = (1000, 700);
    let mut tree = Tree::new();
    let root = crate::shell::build(&app)
        .width(w)
        .height(h)
        .build(&mut tree);
    tree.root = Some(root);
    tree.layout_root(Size::new(w, h), &mut windui::text::NullTextEngine);
    let idx = crate::cli::page_to_index("input").expect("页 id");
    app.borrow().tab.set(idx);
    tree.layout_root(Size::new(w, h), &mut windui::text::NullTextEngine);
    // 把 `pointer_mods` 快照置为 ctrl=true：它只由真实指针分发写入，而 `menu_for`
    // 据此决定要不要给本功能的菜单。不种这一下，下面调 `build()` 恒得空表。
    let mut hover = None;
    let mut capture = None;
    tree.dispatch_pointer(
        PointerEvent {
            kind: PointerKind::Down,
            pos: Point::new(w / 2, h / 2),
            button: MouseButton::Right,
            click_count: 1,
            mods: Mods {
                ctrl: true,
                ..Default::default()
            },
        },
        &mut hover,
        &mut capture,
    );
    Fixture { _app: app, tree }
}

/// 收集所有挂着**节级隐藏菜单**的节点高度。
///
/// `hide_menu::menu_for` 在入口读 `pointer_mods().ctrl`，而那个快照由真实指针分发写入；
/// 故先 dispatch 一次 Ctrl+右键把快照置为 ctrl=true（见 [`fixture`]），之后调用构建器
/// 才拿得到真实菜单项。只认「隐藏这一节 / 取消隐藏这一节」，其它菜单（方案列表的
/// 导入与显示过滤之类）不参与判定。
fn section_menu_nodes(tree: &Tree, id: NodeId, out: &mut Vec<i32>) {
    let Some(n) = tree.get(id) else { return };
    if let Some(build) = &n.context_menu {
        let is_section = build()
            .iter()
            .filter(|i| !i.separator)
            .any(|i| i.label == "隐藏这一节" || i.label == "取消隐藏这一节");
        if is_section {
            out.push(n.bounds.h);
        }
    }
    for &c in &n.children {
        section_menu_nodes(tree, c, out);
    }
}

/// 每个右键菜单都必须挂在**窄条**上（单行或标题栏），不能有与整卡同高的。
///
/// 这是「操作区边缘被划进整节」那个问题的守门测试：菜单一旦挂回卡片外壳，
/// 卡片里任何空白处都会误触。卡片外壳高几百像素，断言当场失败。
#[test]
fn menus_are_attached_to_short_nodes_only() {
    let f = fixture();
    let mut heights = Vec::new();
    section_menu_nodes(&f.tree, f.tree.root.unwrap(), &mut heights);
    assert!(
        !heights.is_empty(),
        "全文没有任何右键菜单——挂载逻辑整条断了"
    );
    let tallest = heights.iter().copied().max().unwrap_or(0);
    assert!(
        tallest <= 60,
        "有菜单挂到了高 {tallest}px 的节点上（标题栏/单行约 24–40px）。\
         说明它被挂回整张卡片了：卡片里任何空白处都会误触。\
         实测节点高度：{heights:?}"
    );
}

/// 行菜单（含「隐藏这一项」）与节级菜单不在同一节点上——两者各归各的层级。
#[test]
fn item_and_section_menus_live_on_different_nodes() {
    let f = fixture();
    let mut mixed = Vec::new();
    fn walk(tree: &Tree, id: NodeId, mixed: &mut Vec<i32>) {
        let Some(n) = tree.get(id) else { return };
        if let Some(build) = &n.context_menu {
            let labels: Vec<String> = build()
                .iter()
                .filter(|i| !i.separator)
                .map(|i| i.label.clone())
                .collect();
            let has_item = labels
                .iter()
                .any(|l| l == "隐藏这一项" || l == "取消隐藏这一项");
            let has_section = labels
                .iter()
                .any(|l| l == "隐藏这一节" || l == "取消隐藏这一节");
            if has_item && has_section {
                mixed.push(n.bounds.h);
            }
        }
        for &c in &n.children {
            walk(tree, c, mixed);
        }
    }
    walk(&f.tree, f.tree.root.unwrap(), &mut mixed);
    assert!(
        mixed.is_empty(),
        "这些节点同时带项级与节级菜单：{mixed:?}——行与卡片应各只给一档"
    );
}
