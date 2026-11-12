//! 「系统词库 → 分类词库」（主仓设计 `system-dict-patch-and-candidate-adjust.md` §6.1）：方案
//! 附加的扩展 / 细胞词库在词库窗里开关。
//!
//! - 中栏：「系统词库」分组下一个「分类词库」节点（带已启用数），其下缩进只列**已启用**的库，
//!   勾选框可直接关；节点见 `category_nav`。
//! - 右栏：点节点换成分类清单（按方案文件里的 `group` 分组；未分组的随包库归「其他」，发现
//!   目录登记的归「我放入的」），顶部「只看已启用」与搜索。清单不是表格类别，不占子标签、
//!   不进导出（见 `state::tabs_for_kind` 的说明）。
//!
//! 开关拨动即生效，与分库开关同一套：对域内每个成员方案各调一次 `schema.setDictEnabled`，
//! 任一失败就把已改的改回去（[`DictManagerState::set_sys_cat_enabled`]）。清单取自 core
//! `sysDict.categories`，切到别的方案或收到 `schema.configChanged` 时重取。

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::prelude::*;

use super::state::DictManagerState;
use crate::rpc;

/// 中栏分组标题。
pub(crate) const SYS_HEADING: &str = "系统词库";
/// 中栏节点与右栏标题。
pub(crate) const NODE_TITLE: &str = "分类词库";
/// 方案文件没写 `group` 的随包库归这一组。
pub(crate) const OTHER_GROUP: &str = "其他";
/// 发现目录（`dir:` 前缀）登记的库归这一组。
pub(crate) const DIR_GROUP: &str = "我放入的";

/// 一张分类词库（`sysDict.categories` 的一项）。
#[derive(Clone, Debug, PartialEq)]
pub struct SysCat {
    pub id: String,
    pub label: String,
    pub description: String,
    /// 展示用的分组名（已按来源归好「其他」「我放入的」）。
    pub group: String,
    pub enabled: bool,
    /// 条目数；core 数不出（文件不在）为 `None`。
    pub count: Option<u64>,
}

/// `sysDict.categories` 的返回 → 清单（缺 `id` 的项丢弃）。
pub(crate) fn parse_cats(v: &Value) -> Vec<SysCat> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|it| {
            let s = |k: &str| it.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let id = it
                .get("id")?
                .as_str()
                .filter(|s| !s.is_empty())?
                .to_string();
            let group = if it.get("source").and_then(Value::as_str) == Some("dir") {
                DIR_GROUP.to_string()
            } else {
                Some(s("group"))
                    .filter(|g| !g.is_empty())
                    .unwrap_or_else(|| OTHER_GROUP.to_string())
            };
            Some(SysCat {
                label: Some(s("label"))
                    .filter(|l| !l.is_empty())
                    .unwrap_or_else(|| id.clone()),
                description: s("description"),
                group,
                enabled: it.get("enabled").and_then(Value::as_bool).unwrap_or(false),
                count: it.get("count").and_then(Value::as_u64),
                id,
            })
        })
        .collect()
}

/// 右栏清单：按组排好、按「只看已启用」与搜索词滤过。组按首次出现的顺序（即方案文件顺序），
/// 「其他」与「我放入的」压到最后；滤空的组不出。搜索不分大小写，查名称与说明。
pub(crate) fn group_cats<'a>(
    cats: &'a [SysCat],
    only_enabled: bool,
    query: &str,
) -> Vec<(String, Vec<&'a SysCat>)> {
    let q = query.trim().to_lowercase();
    let mut out: Vec<(String, Vec<&SysCat>)> = Vec::new();
    for c in cats {
        if only_enabled && !c.enabled {
            continue;
        }
        if !q.is_empty()
            && !c.label.to_lowercase().contains(&q)
            && !c.description.to_lowercase().contains(&q)
        {
            continue;
        }
        match out.iter_mut().find(|(g, _)| *g == c.group) {
            Some((_, v)) => v.push(c),
            None => out.push((c.group.clone(), vec![c])),
        }
    }
    let rank = |g: &str| match g {
        OTHER_GROUP => 1,
        DIR_GROUP => 2,
        _ => 0,
    };
    out.sort_by_key(|(g, _)| rank(g)); // 稳定排序：同档保持首次出现的顺序
    out
}

/// 分类词库的状态（Signal 为 Copy，整体可 Clone 进闭包）。
#[derive(Clone)]
pub struct SysCatsState {
    /// 当前方案的清单。
    pub list: Signal<Vec<SysCat>>,
    /// 右栏是否换成了分类清单（点中栏「分类词库」打开，点别的节点或换数据域关上）。
    pub open: Signal<bool>,
    /// 右栏「只看已启用」。
    pub only_enabled: Signal<bool>,
    /// 右栏搜索词。
    pub query: Signal<String>,
    /// 右栏标题旁的「已启用 n / 共 m」。
    pub summary: Signal<String>,
    /// 右栏清单整体重建（清单、筛选或搜索变化时递增）。
    pub epoch: Signal<Vec<u64>>,
    /// 清单是为哪个方案取的；与当前不同时 [`DictManagerState::reload`] 重取一次。
    loaded_for: Rc<RefCell<Option<String>>>,
}

impl SysCatsState {
    pub fn new() -> Self {
        Self {
            list: signal(Vec::new()),
            open: signal(false),
            only_enabled: signal(false),
            query: signal(String::new()),
            summary: signal(String::new()),
            epoch: signal(vec![0]),
            loaded_for: Rc::new(RefCell::new(None)),
        }
    }

    pub(crate) fn bump_epoch(&self) {
        let e = self.epoch.get().first().copied().unwrap_or(0);
        self.epoch.set(vec![e + 1]);
    }
}

impl Default for SysCatsState {
    fn default() -> Self {
        Self::new()
    }
}

impl DictManagerState {
    /// 清单还不是为当前方案取的就取一次（[`Self::reload`] 开头调用）。
    pub(crate) fn ensure_sys_cats(&self) {
        if *self.cats.loaded_for.borrow() != self.current_schema_id() {
            self.refresh_sys_cats();
        }
    }

    /// 重取清单；有变化时重建中栏与右栏。不是方案域、或 core 不认这个方法（旧版）时为空，
    /// 中栏随之不出「系统词库」。
    pub fn refresh_sys_cats(&self) {
        let scope = self.current_schema_id();
        *self.cats.loaded_for.borrow_mut() = scope.clone();
        let list = match scope.and_then(|_| self.lib_switch_schema()) {
            Some(id) => match rpc::call_core("sysDict.categories", &json!({ "schema": id })) {
                Ok(v) => parse_cats(&v),
                Err(e) => {
                    tracing::debug!("取分类词库失败：{e}");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        if list.is_empty() && self.cats.open.get() {
            self.cats.open.set(false);
        }
        if list != self.cats.list.get() {
            let on = list.iter().filter(|c| c.enabled).count();
            self.cats
                .summary
                .set(format!("已启用 {on} / 共 {}", list.len()));
            self.cats.list.set(list);
            self.bump_nav_epoch();
            self.cats.bump_epoch();
        }
    }

    /// core 广播的方案配置变化：是当前域的成员（或不带 id 的整批变化）就重取清单。
    /// 返回是否重取了。
    pub fn on_schema_config_changed(&self, id: Option<&str>) -> bool {
        let hit = match id {
            Some(id) => self.current_domain_members().iter().any(|m| m == id),
            None => self.current_schema_id().is_some(),
        };
        if hit {
            self.refresh_sys_cats();
        }
        hit
    }

    /// 打开右栏的分类清单。
    pub fn open_sys_cats(&self) {
        if !self.cats.open.get() {
            self.cats.open.set(true);
        }
    }

    /// 开关一张分类词库，对域内每个成员方案都改（拼音族折叠时全拼、双拼各存一份开关）。任一
    /// 成员失败就把已改的成员改回去并提示；返回是否成功（勾选框据此回滚）。
    pub fn set_sys_cat_enabled(&self, id: &str, enabled: bool) -> bool {
        let call = |schema: &str, on: bool| {
            rpc::call_core(
                "schema.setDictEnabled",
                &json!({ "id": schema, "dictId": id, "enabled": on }),
            )
        };
        let mut done: Vec<String> = Vec::new();
        let mut failed = None;
        for schema in self.current_domain_members() {
            match call(&schema, enabled) {
                Ok(_) => done.push(schema),
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = &failed {
            for schema in done.iter().rev() {
                if let Err(e2) = call(schema, !enabled) {
                    tracing::warn!("分类词库开关回滚失败（{schema}）：{e2}");
                }
            }
            self.notify_err(&format!("切换失败：{e}"));
        }
        let ok = failed.is_none() && !done.is_empty();
        if ok {
            let label = self
                .cats
                .list
                .get()
                .into_iter()
                .find(|c| c.id == id)
                .map_or_else(|| id.to_string(), |c| c.label);
            let verb = if enabled { "已启用" } else { "已停用" };
            self.notify_ok(&format!("{verb}「{label}」"));
        }
        self.refresh_sys_cats();
        ok
    }
}

// ── 右栏 ─────────────────────────────────────────────────────

/// 右栏的分类清单：标题 + 说明 + 筛选行 + 按组列出的卡片。
pub fn build_sys_cats_pane(mgr: &DictManagerState) -> Element {
    let cats = mgr.cats.clone();
    let (q_v, oe_v) = (cats.query, cats.only_enabled);
    let c_eff = cats.clone();
    let filter_effect = super::reactive::reactive_effect(
        move || {
            q_v.version()
                .wrapping_mul(1_000_003)
                .wrapping_add(oe_v.version())
        },
        move || c_eff.bump_epoch(),
    );

    // 窄窗口下让计数先截，标题保持完整。
    let summary = Element::label_signal(cats.summary)
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .max_lines(1)
        .weight(1.0)
        .text_align(Align::End);
    let title_row = Element::row()
        .width_match()
        .height(30)
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::label(NODE_TITLE)
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .max_lines(1),
        )
        .child(summary);
    let hint = Element::label("勾选即生效。放进方案扩展词库目录的词库列在「我放入的」。")
        .font_size(12.0)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(1);

    let small_h = windui::theme::current().metrics.control_h_sm;
    let oe = cats.only_enabled;
    let only_enabled = Element::checkbox("只看已启用", oe)
        .small()
        .on_toggle(move |_| oe.set(!oe.get()));
    let (q_clr, q_en) = (cats.query, cats.query);
    let search = Element::row()
        .width_match()
        .max_width(260)
        .align_xy(Align::End, Align::Center)
        .cross(Align::Center)
        .spacing(4)
        .child(
            Element::text_input(cats.query, "搜索名称或说明…")
                .weight(1.0)
                .height(small_h),
        )
        .child(
            Element::button("×")
                .small()
                .outline_soft()
                .tooltip("清除搜索")
                .on_click(move |_| q_clr.set(String::new()))
                .enabled_when(move || !q_en.get().is_empty()),
        );
    let toolbar = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .padding_xy(0, 2)
        .child(only_enabled)
        .child(Element::stack().weight(1.0).child(search));

    let m = mgr.clone();
    let body = Element::host_signal(cats.epoch, move |_| build_groups(&m));
    let card = crate::widgets::card_frame()
        .width_match()
        .weight(1.0)
        .padding_xy(4, 2)
        .child(Element::scroll().fill().child(body));

    let col = Element::col()
        .fill()
        .bg_role(Role::SurfaceAlt)
        .padding_xy(20, 16)
        .spacing(12)
        .child(title_row)
        .child(hint)
        .child(toolbar)
        .child(card);
    Element::stack().fill().child(col).child(filter_effect)
}

/// 清单正文：各组一个小标题 + 若干行；全滤空时一句提示。
fn build_groups(mgr: &DictManagerState) -> Element {
    let list = mgr.cats.list.get();
    let groups = group_cats(&list, mgr.cats.only_enabled.get(), &mgr.cats.query.get());
    let mut col = Element::col().width_match().spacing(2).padding_xy(12, 8);
    if groups.is_empty() {
        let msg = if list.is_empty() {
            "这个方案没有分类词库"
        } else if !mgr.cats.query.get().trim().is_empty() {
            "没有匹配的词库"
        } else {
            "还没有启用的分类词库"
        };
        return col.child(
            Element::label(msg)
                .font_size(13.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .text_align(Align::Center)
                .padding_xy(0, 40),
        );
    }
    for (i, (group, items)) in groups.into_iter().enumerate() {
        if i > 0 {
            col = col.child(Element::leaf().height(6));
        }
        col = col.child(
            Element::label(group)
                .font_size(12.0)
                .font_weight(600)
                .fg_role(Role::TextMuted)
                .padding_xy(0, 4),
        );
        for c in items {
            col = col.child(cat_row(mgr, c));
        }
    }
    col
}

/// 一行：勾选框 / 名称与说明 / 条数。
fn cat_row(mgr: &DictManagerState, c: &SysCat) -> Element {
    let sig = signal(c.enabled);
    let (m, id) = (mgr.clone(), c.id.clone());
    let check = Element::checkbox("", sig).small().on_toggle(move |_| {
        let next = !sig.get();
        sig.set(next);
        if !m.set_sys_cat_enabled(&id, next) {
            sig.set(!next);
        }
    });
    let mut text = Element::col().weight(1.0).spacing(1).child(
        Element::label(c.label.clone())
            .font_size(13.0)
            .fg_role(Role::Text)
            .max_lines(1),
    );
    if !c.description.is_empty() {
        text = text.child(
            Element::label(c.description.clone())
                .font_size(11.5)
                .fg_role(Role::TextMuted)
                .max_lines(1),
        );
    }
    let count = c.count.map_or_else(|| "—".to_string(), |n| n.to_string());
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(10)
        .padding_xy(0, 5)
        .child(check)
        .child(text)
        .child(
            Element::label(count)
                .font_size(11.5)
                .fg_role(Role::TextDisabled)
                .max_lines(1),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SchemaInfo;

    fn cat(id: &str, group: &str, enabled: bool) -> SysCat {
        SysCat {
            id: id.into(),
            label: id.into(),
            description: String::new(),
            group: group.into(),
            enabled,
            count: Some(1),
        }
    }

    #[test]
    fn parse_resolves_groups_by_source() {
        let v = json!([
            { "id": "cell_idiom", "label": "成语", "description": "成语俗语", "group": "文史",
              "enabled": true, "source": "bundled", "count": 48721 },
            { "id": "wb_extra", "label": "", "group": null, "enabled": false,
              "source": "bundled", "count": null },
            { "id": "dir:pinyin/ext/mine", "label": "我的", "group": "文史", "enabled": false,
              "source": "dir", "count": 3 },
            { "label": "缺 id" }
        ]);
        let l = parse_cats(&v);
        assert_eq!(l.len(), 3, "缺 id 的丢弃");
        assert_eq!(
            (l[0].group.as_str(), l[0].count, l[0].enabled),
            ("文史", Some(48721), true)
        );
        assert_eq!(
            (l[1].label.as_str(), l[1].group.as_str(), l[1].count),
            ("wb_extra", OTHER_GROUP, None),
            "没名字用 id，没分组归「其他」，数不出为 None"
        );
        assert_eq!(l[2].group, DIR_GROUP, "发现目录的不看 group");
        assert!(parse_cats(&json!({"error": 1})).is_empty());
    }

    #[test]
    fn groups_keep_file_order_and_push_other_and_dir_last() {
        let l = [
            cat("a", DIR_GROUP, true),
            cat("b", "网络", false),
            cat("c", OTHER_GROUP, true),
            cat("d", "文史", true),
            cat("e", "网络", true),
        ];
        let g = group_cats(&l, false, "");
        let names: Vec<(&str, Vec<&str>)> = g
            .iter()
            .map(|(n, v)| (n.as_str(), v.iter().map(|c| c.id.as_str()).collect()))
            .collect();
        assert_eq!(
            names,
            [
                ("网络", vec!["b", "e"]),
                ("文史", vec!["d"]),
                (OTHER_GROUP, vec!["c"]),
                (DIR_GROUP, vec!["a"]),
            ]
        );
        let on = group_cats(&l, true, "");
        assert_eq!(on[0].1.len(), 1, "只看已启用：网络组只剩 e");
        assert!(on.iter().all(|(_, v)| v.iter().all(|c| c.enabled)));
    }

    #[test]
    fn search_matches_label_and_description_case_insensitively() {
        let mut a = cat("x", "专业术语", false);
        a.label = "计算机".into();
        a.description = "IT 术语".into();
        let b = cat("y", "网络", false);
        let l = [a, b];
        assert_eq!(group_cats(&l, false, "计算")[0].1[0].id, "x");
        assert_eq!(
            group_cats(&l, false, " it ")[0].1[0].id,
            "x",
            "说明也查、不分大小写"
        );
        assert!(group_cats(&l, false, "无此").is_empty());
        assert!(group_cats(&l, true, "计算").is_empty(), "两个条件同时生效");
    }

    // ── 状态（mock RPC）────────────────────────────────────

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

    fn calls_of(method: &str) -> Vec<Value> {
        crate::rpc::take_mock_calls()
            .into_iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p)
            .collect()
    }

    /// 拼音域（全拼 py + 双拼 `sp`，代表 py）。`bad` 打头的方案 mock 开关会失败。
    fn pinyin_state(sp: &str) -> DictManagerState {
        let st = DictManagerState::new();
        *st.schemas.borrow_mut() = vec![
            SchemaInfo {
                id: "py".into(),
                name: "全拼".into(),
                kind: "pinyin".into(),
                scheme: "full".into(),
                ..Default::default()
            },
            SchemaInfo {
                id: sp.into(),
                name: "双拼".into(),
                kind: "pinyin".into(),
                scheme: "shuangpin".into(),
                ..Default::default()
            },
        ];
        st.domain.set(super::super::state::GLOBAL_DOMAINS);
        st.sub_tab.set(0);
        st.on_nav_changed();
        st
    }

    #[test]
    fn reload_fetches_the_list_once_per_schema() {
        with_mock(|| {
            let st = pinyin_state("sp");
            assert!(!st.cats.list.get().is_empty());
            crate::rpc::take_mock_calls();
            st.reload();
            assert!(calls_of("sysDict.categories").is_empty(), "同一方案不重取");
            st.domain.set(super::super::state::DOMAIN_PHRASE);
            st.on_nav_changed();
            assert!(st.cats.list.get().is_empty(), "全局域没有系统词库");
        });
    }

    #[test]
    fn folded_domain_toggles_every_member() {
        with_mock(|| {
            let st = pinyin_state("sp");
            crate::rpc::take_mock_calls();
            assert!(st.set_sys_cat_enabled("cell_idiom", true));
            assert_eq!(
                calls_of("schema.setDictEnabled"),
                vec![
                    json!({"id": "py", "dictId": "cell_idiom", "enabled": true}),
                    json!({"id": "sp", "dictId": "cell_idiom", "enabled": true}),
                ]
            );
            let idiom = st
                .cats
                .list
                .get()
                .into_iter()
                .find(|c| c.id == "cell_idiom")
                .unwrap();
            assert!(idiom.enabled, "开关后重取，清单跟上");
        });
    }

    #[test]
    fn folded_domain_rolls_back_on_partial_failure() {
        with_mock(|| {
            let st = pinyin_state("bad_sp");
            crate::rpc::take_mock_calls();
            assert!(!st.set_sys_cat_enabled("cell_idiom", true));
            assert_eq!(
                calls_of("schema.setDictEnabled"),
                vec![
                    json!({"id": "py", "dictId": "cell_idiom", "enabled": true}),
                    json!({"id": "bad_sp", "dictId": "cell_idiom", "enabled": true}),
                    json!({"id": "py", "dictId": "cell_idiom", "enabled": false}),
                ],
                "已改的成员改回去"
            );
            let (_, msg) = st.feedback.get().expect("应有提示");
            assert!(msg.starts_with("切换失败："), "{msg}");
            assert!(
                !st.cats
                    .list
                    .get()
                    .iter()
                    .any(|c| c.id == "cell_idiom" && c.enabled),
                "回滚后清单仍是关"
            );
        });
    }

    /// 别处改了当前域成员的开关（方案页、命令直通车）：重取清单、重建中栏；别的方案不理。
    #[test]
    fn config_changed_of_a_member_refreshes() {
        with_mock(|| {
            let st = pinyin_state("sp");
            let nav0 = st.nav_epoch.get()[0];
            crate::rpc::take_mock_calls();
            assert!(!st.on_schema_config_changed(Some("wubi86")), "别的方案不理");
            assert!(calls_of("sysDict.categories").is_empty());

            // 模拟在方案页开了全拼的成语库（mock 记下开关），广播里带的是另一个成员
            crate::rpc::call_core(
                "schema.setDictEnabled",
                &json!({"id": "py", "dictId": "cell_idiom", "enabled": true}),
            )
            .unwrap();
            assert!(st.on_schema_config_changed(Some("sp")), "成员都算");
            assert_eq!(calls_of("sysDict.categories").len(), 1);
            assert!(
                st.cats
                    .list
                    .get()
                    .iter()
                    .any(|c| c.id == "cell_idiom" && c.enabled)
            );
            assert!(st.nav_epoch.get()[0] > nav0, "中栏重建");
            assert!(st.on_schema_config_changed(None), "不带 id 按全部处理");
        });
    }
}
