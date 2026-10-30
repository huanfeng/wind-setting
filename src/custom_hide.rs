//! 定制版的设置界面减法：`data_custom/custom.toml` 的 `[settings] hide`。
//!
//! 选择器的语法与解析在 `wind_config::custom_settings`（与 `wind_input config check --custom`
//! 共用），本模块只管「本机读到的是哪一份」与「设置页怎么消费」。
//!
//! # 为什么在本进程直接读清单，而不是等 `system.info`
//!
//! 页面树在窗口建好时一次性构建，而 `system.info` 要等后台线程 fetch 回来——那时
//! 每一行、每张卡、搜索索引都已经建完了（关于页的定制版徽章就因此只能走 Signal）。
//! 隐藏决定的是「建不建这一行」，没法事后用 Signal 补。故直接调 core 用的同一个
//! [`wind_config::Config::custom_manifest`]：同一份解析、同一个判据（清单在安装根的
//! `data_custom/` 下）。
//!
//! 安装根取自本 exe 所在目录。Windows / Linux 上设置程序与主程序同目录；macOS 的设置程序是
//! 独立 .app，读不到清单 ⇒ 什么都不隐藏——这是安全的方向（多显示几项，功能不受影响）。
//!
//! # 语义
//!
//! 隐藏只影响**显示**：被隐藏的设置照常生效，值仍来自定制层 / 用户层，本程序也不改写它。
//! 用户可以在「高级」页打开「显示定制版隐藏的设置」把它们找回来（重开设置窗口后生效）。

use std::sync::OnceLock;

use wind_config::SettingsHide;

use crate::manifest::Manifest;

/// 本机定制版的 `[settings] hide`。非定制版、清单读不到、或没写这一段 ⇒ 空。进程内只读一次。
pub fn installed() -> &'static SettingsHide {
    static HIDE: OnceLock<SettingsHide> = OnceLock::new();
    HIDE.get_or_init(|| {
        let Some(m) = wind_config::Config::custom_manifest() else {
            return SettingsHide::default();
        };
        let (hide, errors) = SettingsHide::parse(&m.settings.hide);
        for e in errors {
            tracing::warn!("定制版 custom.toml 的 [settings] hide 有一条无效，已忽略：{e}");
        }
        hide
    })
}

/// 设置页实际使用的清单：内嵌清单，再按 `hide` 折叠（`show_all` 时不折）。
///
/// 搜索索引与页面必须用**同一份**结果，否则会搜到一项、点过去却什么都没有。
pub fn settings_manifest(hide: &SettingsHide, show_all: bool) -> Manifest {
    let mut m = Manifest::from_embedded();
    if !show_all {
        for sel in m.apply_custom_hide(hide) {
            tracing::warn!(
                "定制版 [settings] hide 的 `{sel}` 没有命中任何设置项（拼错了，或该项在本版本已改名）"
            );
        }
        for (shown, hidden) in half_hidden_exclusive_pairs(&m) {
            tracing::warn!(
                "定制版 [settings] hide 只藏了互斥对中的 `{hidden}`，`{shown}` 仍可见：\
                 用户打开后者时无法联动关掉前者，两者可能同时生效。请成对隐藏"
            );
        }
    }
    m
}

/// 完整清单：定制版 `[settings] hide` 与用户自己的精简清单**依次**折叠。
///
/// 顺序即优先级：定制版先折，用户规则再折。定制版置 `hidden`；用户规则只做标记、
/// 不置 `hidden`（显隐交给运行期判据）。「谁藏的」记在不同字段里（`custom_hidden` / `user_hidden`），右键菜单据此决定
/// 「取消隐藏」删的是哪一份配置。
///
/// 两个逃生口各管自己那一份：`show_custom` 忽略定制版，`show_user` 忽略用户清单。
/// 它们互相独立——定制版藏掉的东西，用户打开自己的逃生口也不该看见。
pub fn settings_manifest_full(
    hide: &SettingsHide,
    show_custom: bool,
    user_hide: &crate::user_hide::UserHide,
    show_user: bool,
) -> Manifest {
    let mut m = settings_manifest(hide, show_custom);
    if !show_user {
        for sel in m.apply_user_hide(user_hide) {
            tracing::warn!(
                "用户设置精简清单的 `{sel}` 没有命中任何设置项（该项可能在本版本已改名）"
            );
        }
    }
    m
}

/// 互斥对（`exclusive_with`）里一方可见、一方被藏的情形：（可见方, 被藏方）。
///
/// 互斥靠点击时改对方的 Signal 实现，被藏的一方没有 Signal，联动就静默跳过了。
fn half_hidden_exclusive_pairs(m: &Manifest) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for key in &m.order {
        let Some(it) = m.get(key) else { continue };
        let Some(other) = &it.exclusive_with else {
            continue;
        };
        if !it.hidden && m.get(other).is_some_and(|o| o.hidden) {
            out.push((key.clone(), other.clone()));
        }
    }
    out
}

/// 设置向导是否被定制版关掉（`feature:wizard`）。
pub fn wizard_hidden(hide: &SettingsHide) -> bool {
    hide.hides_feature("wizard")
}

/// 打开设置时的自动检查更新是否被定制版关掉（`feature:update_check`）。同 [`wizard_hidden`]：
/// 只看清单、不看逃生口——逃生口找回的是关于页的按钮，自己点是知情选择，开窗就弹升级框不是。
pub fn update_check_hidden(hide: &SettingsHide) -> bool {
    hide.hides_feature("update_check")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hide(raw: &[&str]) -> SettingsHide {
        let (h, errors) = SettingsHide::parse(raw);
        assert!(errors.is_empty(), "{errors:?}");
        h
    }

    /// 前缀选择器把整组项折进 hidden，`sections_of` 随之把整张卡去掉。
    #[test]
    fn prefix_selector_hides_whole_section() {
        let full = Manifest::from_embedded();
        assert!(
            full.sections_of("schema")
                .iter()
                .any(|(s, _)| s == "临时英文"),
            "前提：出厂清单有「临时英文」分区"
        );
        // 只藏前缀那一批还不够：「临时英文」分区里混着 `input.english_case_cycle_key`。
        // 分区成员 ≠ 键前缀，这正是定制者最容易踩的地方，文档里要给出分区对照。
        let partly = settings_manifest(&hide(&["key:input.temp_english.*"]), false);
        assert!(
            partly
                .sections_of("schema")
                .iter()
                .any(|(s, _)| s == "临时英文")
        );
        let m = settings_manifest(
            &hide(&[
                "key:input.temp_english.*",
                "key:input.english_case_cycle_key",
            ]),
            false,
        );
        assert!(
            !m.sections_of("schema").iter().any(|(s, _)| s == "临时英文"),
            "整节被隐藏后分区不应再返回"
        );
        // 别的分区不受牵连。
        assert!(m.sections_of("schema").iter().any(|(s, _)| s == "临时拼音"));
    }

    #[test]
    fn exact_selector_hides_only_that_item() {
        let full = Manifest::from_embedded();
        let key = full
            .order
            .iter()
            .find(|k| k.starts_with("input.temp_english."))
            .expect("前提：有临时英文项")
            .clone();
        let m = settings_manifest(&hide(&[&format!("key:{key}")]), false);
        assert!(m.get(&key).unwrap().hidden);
        let others_visible = m
            .order
            .iter()
            .filter(|k| k.starts_with("input.temp_english.") && **k != key)
            .any(|k| !m.get(k).unwrap().hidden);
        assert!(others_visible, "同组其余项不应被藏");
    }

    /// 「显示定制版隐藏的设置」打开时一项都不折。
    #[test]
    fn show_all_ignores_hide() {
        let m = settings_manifest(&hide(&["key:input.temp_english.*"]), true);
        assert!(m.sections_of("schema").iter().any(|(s, _)| s == "临时英文"));
    }

    #[test]
    fn unmatched_key_selector_is_reported() {
        let mut m = Manifest::from_embedded();
        let unmatched = m.apply_custom_hide(&hide(&[
            "key:no.such.key",
            "key:input.temp_english.*",
            "feature:wizard",
        ]));
        assert_eq!(unmatched, vec!["key:no.such.key".to_string()]);
    }

    /// 搜索索引只收非 hidden 项：被定制版藏起来的设置不能被搜到（id 即清单 key）。
    #[test]
    fn hidden_items_drop_out_of_search() {
        let m = settings_manifest(&hide(&["key:input.temp_english.*"]), false);
        let index = crate::search::index::build_index(&m);
        assert!(
            index.iter().any(|e| e.id.starts_with("input.temp_pinyin.")),
            "前提：索引里有同页别的项"
        );
        let leaked: Vec<&str> = index
            .iter()
            .map(|e| e.id.as_str())
            .filter(|id| id.starts_with("input.temp_english."))
            .collect();
        assert!(leaked.is_empty(), "被隐藏的项仍在搜索索引里：{leaked:?}");
    }

    /// 方案页的四张引擎配置卡是手工拼的：项全被隐藏时整张卡不出（返回 None），
    /// 拼音卡还要连模糊音一起藏掉才算空。
    #[test]
    fn scheme_config_card_disappears_when_all_items_hidden() {
        use crate::manifest::scheme_config_section;
        let full = Manifest::from_embedded();
        let selectors_of = |sec: &str| -> Vec<String> {
            full.sections_of("schema")
                .into_iter()
                .find(|(s, _)| s == sec)
                .map(|(_, keys)| keys.iter().map(|k| format!("key:{k}")).collect())
                .expect("前提：出厂清单有该分区")
        };
        let mut st = crate::state::AppState::empty();

        st.manifest = settings_manifest(&hide_owned(&selectors_of("混输行为")), false);
        assert!(scheme_config_section(&st, "混输", "混输行为", false).is_none());
        assert!(scheme_config_section(&st, "码表", "上屏行为", false).is_some());

        let mut pinyin = selectors_of("候选行为");
        st.manifest = settings_manifest(&hide_owned(&pinyin), false);
        assert!(
            scheme_config_section(&st, "拼音", "候选行为", true).is_some(),
            "模糊音还在，卡片不能消失"
        );
        pinyin.push("key:schema.pinyin.fuzzy.*".into());
        st.manifest = settings_manifest(&hide_owned(&pinyin), false);
        assert!(scheme_config_section(&st, "拼音", "候选行为", true).is_none());
    }

    fn hide_owned(raw: &[String]) -> SettingsHide {
        let (h, errors) = SettingsHide::parse(raw);
        assert!(errors.is_empty(), "{errors:?}");
        h
    }

    #[test]
    fn half_hidden_exclusive_pair_is_detected() {
        let full = Manifest::from_embedded();
        let (a, b) = full
            .order
            .iter()
            .find_map(|k| {
                let it = full.get(k)?;
                Some((k.clone(), it.exclusive_with.clone()?))
            })
            .expect("前提：出厂清单至少有一对互斥项");
        let m = settings_manifest(&hide(&[&format!("key:{b}")]), false);
        assert!(half_hidden_exclusive_pairs(&m).contains(&(a.clone(), b.clone())));
        let both = settings_manifest(&hide(&[&format!("key:{a}"), &format!("key:{b}")]), false);
        assert!(half_hidden_exclusive_pairs(&both).is_empty());
    }

    /// 「开关 + 设置」合并对：只藏按钮 ⇒ 开关不再找它合并；只藏开关 ⇒ 按钮自己成行、仍可搜。
    #[test]
    fn gated_dialog_pair_hides_only_what_was_listed() {
        let full = Manifest::from_embedded();
        let (dlg, gate) = full
            .order
            .iter()
            .find_map(|k| {
                let it = full.get(k)?;
                (it.typ == "dialog_button" && it.opens_dialog.is_none() && !it.hidden)
                    .then(|| it.gate_key.clone())
                    .flatten()
                    .filter(|g| full.is_visible(g))
                    .map(|g| (k.clone(), g))
            })
            .expect("前提：出厂清单至少有一对开关 + 设置");

        let only_dlg = settings_manifest(&hide(&[&format!("key:{dlg}")]), false);
        assert!(
            only_dlg.dialog_for_gate(&gate).is_none(),
            "被藏的按钮不能再并进开关行"
        );

        let only_gate = settings_manifest(&hide(&[&format!("key:{gate}")]), false);
        let index = crate::search::index::build_index(&only_gate);
        assert!(
            index.iter().any(|e| e.id == dlg),
            "开关被藏后按钮自己成行，搜索里应有 `{dlg}`"
        );
    }

    /// 模糊音总开关被藏 ⇒ 整张网格不出，格子也不能留在搜索里。
    #[test]
    fn fuzzy_cells_leave_search_with_master() {
        let m = settings_manifest(&hide(&["key:schema.pinyin.fuzzy.enabled"]), false);
        let index = crate::search::index::build_index(&m);
        let leaked: Vec<&str> = index
            .iter()
            .map(|e| e.id.as_str())
            .filter(|id| id.starts_with("schema.pinyin.fuzzy."))
            .collect();
        assert!(leaked.is_empty(), "{leaked:?}");
    }

    /// 宿主被藏 ⇒ 它对话框里的成员（`dialog_group`）跟着藏：「字符与标点」里的
    /// 「临时放宽」两项只活在「候选检索范围」的扩展设置里，藏了宿主就不能再撑住空卡。
    #[test]
    fn hidden_host_takes_its_dialog_members() {
        let has = |m: &Manifest| {
            m.sections_of("input")
                .iter()
                .any(|(s, _)| s == "字符与标点")
        };
        assert!(
            has(&Manifest::from_embedded()),
            "前提：出厂清单有「字符与标点」分区"
        );
        let m = settings_manifest(
            &hide(&[
                "key:input.filter_mode",
                "key:input.rare_phrase",
                "key:input.punct.*",
            ]),
            false,
        );
        assert!(m.get("input.scope_relax.page_end_key").unwrap().hidden);
        assert!(!has(&m), "细项全藏后分区标题不应再出现");
        let index = crate::search::index::build_index(&m);
        assert!(!index.iter().any(|e| e.id.starts_with("input.scope_relax.")));
    }

    /// 寄居项（`dialog_extra_for`）同理；宿主还在时成员不受牵连。
    #[test]
    fn hidden_host_takes_its_extra_members() {
        let host = "input.symbol.smart_chars";
        let m = settings_manifest(&hide(&[&format!("key:{host}")]), false);
        assert!(m.get("input.symbol.english_punct_mode").unwrap().hidden);
        let m = settings_manifest(&hide(&["key:input.rare_phrase"]), false);
        assert!(!m.get("input.scope_relax.page_end_key").unwrap().hidden);
    }

    /// 分组对话框（「候选窗口高级设置」这类）的宿主只是组里排第一的项：藏它不连带其余成员，
    /// 入口行照出，框里只少它自己；搜成员仍能开框。
    #[test]
    fn hidden_titled_host_keeps_its_group() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let host = "ui.candidate.first_show_mode";
        let m = settings_manifest(&hide(&[&format!("key:{host}")]), false);
        assert!(m.get(host).unwrap().hidden);
        assert!(
            !m.get("ui.candidate.min_rows").unwrap().hidden,
            "成员不该被连带藏掉"
        );
        let index = crate::search::index::build_index(&m);
        let e = index
            .iter()
            .find(|e| e.id == "ui.candidate.min_rows")
            .expect("成员仍可搜");
        assert!(
            matches!(&e.target, crate::search::index::JumpTarget::RowInDialog { dialog, .. } if dialog == host)
        );
        // 建页后入口行与对话框都在（对话框以宿主 key 登记）。
        let app = Rc::new(RefCell::new(crate::state::AppState::empty()));
        app.borrow_mut().manifest = m;
        let st = app.borrow();
        let _tabs = crate::pages::all_tabs(&app, &st);
        assert!(
            st.fields.dialog_opens.borrow().contains_key(host),
            "入口行没建出来"
        );
    }

    /// `key:schema.mix_modes.*` 要连 `schema.mix_modes` 本身与 `#视图` 项一起藏。
    #[test]
    fn dot_star_covers_the_key_itself_and_its_views() {
        let m = settings_manifest(
            &hide(&["key:schema.mix_modes.*", "key:schema.quick_input.*"]),
            false,
        );
        assert!(m.get("schema.mix_modes").unwrap().hidden);
        assert!(m.get("schema.mix_modes#members").unwrap().hidden);
        assert!(!m.sections_of("input").iter().any(|(s, _)| s == "快捷输入"));
    }

    /// 精确写 `key:schema.mix_modes` 也连带它的 `#视图` 行（与 `.*` 一致），同分区别的键不动。
    #[test]
    fn exact_key_covers_its_views() {
        let m = settings_manifest(&hide(&["key:schema.mix_modes"]), false);
        let views: Vec<&String> = m
            .order
            .iter()
            .filter(|k| k.starts_with("schema.mix_modes#"))
            .collect();
        assert!(views.len() >= 2, "前提：出厂清单有 mix_modes 的视图项");
        assert!(m.get("schema.mix_modes").unwrap().hidden);
        for k in views {
            assert!(m.get(k).unwrap().hidden, "`{k}` 应随精确 key 一起藏");
        }
        assert!(
            m.order
                .iter()
                .filter(|k| k.starts_with("schema.quick_input."))
                .any(|k| !m.get(k).unwrap().hidden),
            "同分区的 schema.quick_input.* 不应被牵连"
        );
    }

    /// `feature:` 记在清单上，页面与搜索问同一份；逃生口打开时不折，一个都不藏。
    #[test]
    fn feature_selectors_are_recorded_on_the_manifest() {
        let raw = ["feature:backup", "feature:theme_editor"];
        let m = settings_manifest(&hide(&raw), false);
        assert!(m.feature_hidden("backup"));
        assert!(m.feature_hidden("theme_editor"));
        assert!(!m.feature_hidden("integration"));
        assert!(!settings_manifest(&hide(&raw), true).feature_hidden("backup"));
    }

    /// 被藏的手写卡搜不到，且只少它自己那几条。
    #[test]
    fn hidden_feature_cards_drop_out_of_search() {
        use std::collections::BTreeSet;
        let ids = |raw: &[String]| -> BTreeSet<String> {
            crate::search::index::build_index(&settings_manifest(&hide_owned(raw), false))
                .into_iter()
                .map(|e| e.id)
                .collect()
        };
        let full = ids(&[]);
        let cases: &[(&str, &[&str])] = &[
            ("theme_transfer", &["ui:theme-import", "ui:theme-export"]),
            ("theme_editor", &["ui:theme-editor"]),
            ("shuangpin_layout", &["schema:shuangpin-layout"]),
            (
                "integration",
                &[
                    "advanced:integration",
                    "advanced:protocol",
                    "advanced:file-assoc",
                ],
            ),
            ("config_import", &["advanced:import-export"]),
            ("backup", &["advanced:backup"]),
            (
                "compat",
                &[
                    "compat:main",
                    "compat:add",
                    "compat:import",
                    "compat:export",
                    "compat:reset",
                ],
            ),
            ("memory", &["advanced:memory", "advanced:diagnostics"]),
            ("update_check", &["about:update"]),
        ];
        for (feature, gone) in cases {
            let got = ids(&[format!("feature:{feature}")]);
            for id in *gone {
                assert!(full.contains(*id), "前提：索引里有 `{id}`");
                assert!(!got.contains(*id), "藏了 `{feature}` 仍搜得到 `{id}`");
            }
            assert_eq!(
                full.len() - got.len(),
                gone.len(),
                "藏 `{feature}` 只该少它自己的条目"
            );
        }
    }

    /// 按 `hide` 折清单、建整棵页面树，返回 `page` 那一页二级导航里的卡片名。
    fn nav_labels(raw: &[String], page: &str) -> Vec<String> {
        use std::cell::RefCell;
        use std::rc::Rc;
        let app = Rc::new(RefCell::new(crate::state::AppState::empty()));
        app.borrow_mut().manifest = settings_manifest(&hide_owned(raw), false);
        let st = app.borrow();
        let tabs = crate::pages::all_tabs(&app, &st);
        let i = crate::cli::page_to_index(page).expect("页 id");
        tabs[i]
            .subnav
            .items()
            .into_iter()
            .map(|it| it.label)
            .collect()
    }

    fn keys_of_section(group: &str, sec: &str) -> Vec<String> {
        Manifest::from_embedded()
            .sections_of(group)
            .into_iter()
            .find(|(s, _)| s == sec)
            .map(|(_, keys)| keys.iter().map(|k| format!("key:{k}")).collect())
            .expect("前提：出厂清单有该分区")
    }

    /// 四张独立的手写卡：藏谁谁不建，别的卡不受牵连。
    #[test]
    fn hidden_feature_cards_are_not_built() {
        let full = nav_labels(&[], "advanced");
        for (feature, card) in [
            ("integration", "系统集成"),
            ("config_import", "导入"),
            ("backup", "备份与还原"),
            ("compat", "应用兼容性"),
        ] {
            assert!(full.iter().any(|l| l == card), "前提：高级页有「{card}」");
            let got = nav_labels(&[format!("feature:{feature}")], "advanced");
            assert!(
                !got.iter().any(|l| l == card),
                "藏了 `{feature}` 仍有「{card}」"
            );
            assert_eq!(got.len() + 1, full.len(), "藏 `{feature}` 只该少一张卡");
        }
        assert!(nav_labels(&[], "ui").iter().any(|l| l == "主题编辑器"));
        let ui = nav_labels(&["feature:theme_editor".into()], "ui");
        assert!(!ui.iter().any(|l| l == "主题编辑器"));
    }

    /// 与清单项同卡的手写行：手写行与清单项都藏了卡才消失，只藏一边卡还在。
    #[test]
    fn shared_cards_vanish_only_when_both_halves_are_hidden() {
        for (page, group, card, feature) in [
            ("ui", "ui", "主题", "theme_transfer"),
            ("schema", "schema", "双拼", "shuangpin_layout"),
            (
                "advanced",
                "advanced",
                crate::pages::memory::SECTION,
                "memory",
            ),
        ] {
            let has = |raw: &[String]| nav_labels(raw, page).iter().any(|l| l == card);
            let mut raw = keys_of_section(group, card);
            assert!(has(&raw), "只藏清单项，「{card}」卡应留给手写行");
            assert!(
                has(&[format!("feature:{feature}")]),
                "只藏手写行，「{card}」卡还有清单项"
            );
            raw.push(format!("feature:{feature}"));
            assert!(!has(&raw), "两边都藏了，「{card}」不应留下空卡");
        }
    }

    #[test]
    fn wizard_feature() {
        assert!(wizard_hidden(&hide(&["feature:wizard"])));
        assert!(!wizard_hidden(&hide(&["key:input.temp_english.*"])));
    }

    #[test]
    fn update_check_feature() {
        assert!(update_check_hidden(&hide(&["feature:update_check"])));
        assert!(!update_check_hidden(&hide(&["feature:wizard"])));
    }

    /// 几种「压力型」藏法：只藏宿主、只藏成员、每个分区只留首项 / 末项 / 一个成员。
    /// 定制者最容易写出的就是这几种形状，空卡也最容易从这里冒出来。
    fn stress_hides() -> Vec<(&'static str, Vec<String>)> {
        let m = Manifest::from_embedded();
        let items: Vec<&crate::manifest::MItem> = m
            .order
            .iter()
            .filter_map(|k| m.get(k))
            .filter(|i| !i.hidden)
            .collect();
        let is_member =
            |i: &crate::manifest::MItem| i.dialog_group.is_some() || i.dialog_extra_for.is_some();
        let is_host = |i: &crate::manifest::MItem| {
            i.opens_dialog.is_some()
                || i.dialog_title.is_some()
                || i.key == crate::search::index::FUZZY_MASTER
                || items
                    .iter()
                    .any(|o| o.gate_key.as_deref() == Some(i.key.as_str()))
        };
        let sel = |i: &crate::manifest::MItem| format!("key:{}", i.key);
        let mut sections: Vec<((String, String), Vec<&crate::manifest::MItem>)> = Vec::new();
        for i in &items {
            let id = (i.group.clone(), i.section.clone());
            match sections.iter_mut().find(|(s, _)| *s == id) {
                Some((_, v)) => v.push(i),
                None => sections.push((id, vec![i])),
            }
        }
        let keep = |pick: &dyn Fn(&[&crate::manifest::MItem]) -> Option<String>| {
            sections
                .iter()
                .flat_map(|(_, its)| {
                    let kept = pick(its);
                    its.iter()
                        .filter(move |i| kept.is_some() && Some(&i.key) != kept.as_ref())
                        .map(|i| sel(i))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        vec![
            (
                "hosts",
                items
                    .iter()
                    .filter(|i| is_host(i))
                    .map(|i| sel(i))
                    .collect(),
            ),
            (
                "members",
                items
                    .iter()
                    .filter(|i| is_member(i))
                    .map(|i| sel(i))
                    .collect(),
            ),
            (
                "keep_first",
                keep(&|its| its.iter().find(|i| !is_member(i)).map(|i| i.key.clone())),
            ),
            (
                "keep_last",
                keep(&|its| {
                    its.iter()
                        .rev()
                        .find(|i| !is_member(i))
                        .map(|i| i.key.clone())
                }),
            ),
            (
                "keep_member",
                keep(&|its| its.iter().find(|i| is_member(i)).map(|i| i.key.clone())),
            ),
        ]
    }

    /// 定制版怎么藏都不能留下只有标题的空卡：凡是还在页面上（因而也在二级导航里）的卡片，
    /// 至少要有一行。
    #[test]
    fn stress_hides_leave_no_empty_card() {
        use crate::manifest::{scheme_config_section, section_rows_split};
        let engine_secs: Vec<&str> = crate::pages::SCHEME_ENGINE_CARDS
            .iter()
            .map(|(_, sec, _)| *sec)
            .collect();
        let mut st = crate::state::AppState::empty();
        for (name, raw) in stress_hides() {
            st.manifest = settings_manifest(&hide_owned(&raw), false);
            for group in ["schema", "general", "input", "keys", "ui", "advanced"] {
                for (sec, keys) in st.manifest.sections_of(group) {
                    if group == "schema" && engine_secs.contains(&sec.as_str()) {
                        // 引擎卡手工拼，逐行跳过的判据相同；出卡与否由它自己按 keys 判。
                        assert!(scheme_config_section(&st, &sec, &sec, false).is_some());
                        continue;
                    }
                    let (rows, head) = section_rows_split(&st, &keys);
                    // 用户精简清单是运行期显隐（行照建），故「有行」还要看**可见的行**：
                    // 定制版藏掉的行根本不建，这里 `rows` 为空才是真空卡。
                    assert!(
                        !rows.is_empty() || head.is_some(),
                        "[{name}] {group}/{sec} 只剩标题，没有一行"
                    );
                }
            }
        }
    }

    /// 只由卡片组成的页被藏空后要标成空页（侧栏据此不给入口）；没藏时一页都不空。
    #[test]
    fn emptied_card_pages_are_flagged() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let empty_tabs = |raw: &[String]| -> Vec<String> {
            let app = Rc::new(RefCell::new(crate::state::AppState::empty()));
            app.borrow_mut().manifest = settings_manifest(&hide_owned(raw), false);
            let st = app.borrow();
            crate::pages::all_tabs(&app, &st)
                .into_iter()
                .filter(|t| t.empty)
                .map(|t| t.name)
                .collect()
        };
        // 按页（清单 group）藏：页上的项不一定都以页名开头（按键页里有 input.* 的项）。
        let groups = |gs: &[&str]| -> Vec<String> {
            let m = Manifest::from_embedded();
            m.order
                .iter()
                .filter(|k| m.get(k).is_some_and(|i| gs.contains(&i.group.as_str())))
                .map(|k| format!("key:{k}"))
                .collect()
        };
        let with = |mut v: Vec<String>, extra: &[&str]| {
            v.extend(extra.iter().map(|s| s.to_string()));
            v
        };
        assert!(empty_tabs(&[]).is_empty());
        assert_eq!(empty_tabs(&groups(&["keys"])), ["按键"]);
        // 外观页还有两张手写卡：清单项全藏了也不空，手写卡一并藏掉才空。
        assert!(empty_tabs(&groups(&["ui"])).is_empty());
        assert_eq!(
            empty_tabs(&with(
                groups(&["ui"]),
                &["feature:theme_transfer", "feature:theme_editor"]
            )),
            ["外观"]
        );
        assert_eq!(empty_tabs(&groups(&["input", "general"])), ["输入"]);
    }
}

#[cfg(test)]
mod user_hide_tests {
    use super::*;
    use crate::user_hide::{UserHide, UserHideConfig};

    fn uh(raw: &[&str]) -> UserHide {
        UserHide::from_config(&UserHideConfig {
            enabled: true,
            show_hidden: false,
            hide: raw.iter().map(|s| s.to_string()).collect(),
        })
    }

    fn folded(raw: &[&str]) -> Manifest {
        settings_manifest_full(&SettingsHide::default(), false, &uh(raw), false)
    }

    /// 用户清单与定制版**取并集**：各藏各的，互不覆盖。
    ///
    /// 用「临时拼音」与「字符与标点」两节：后者成员不止 `input.punct.*`（还有
    /// `input.filter_mode` / `input.rare_phrase` / `input.scope_relax.*`），正好覆盖
    /// `section:` 规则「整卡藏掉」这条与本文件 `key:` 规则不同的路径。
    ///
    /// ★ 两种藏法的**可见性判据不同**，这里分别断言：
    /// - 定制版折进 `hidden`（建树期跳过，`sections_of` 不再返回该节）；
    /// - 用户清单只记标记，节仍在 `sections_of` 里，由运行期判据隐藏。
    #[test]
    fn user_and_custom_hides_union() {
        let (custom, _) = SettingsHide::parse(&["key:input.temp_pinyin.*"]);
        let m = settings_manifest_full(&custom, false, &uh(&["section:input/字符与标点"]), false);
        assert!(
            !m.sections_of("schema").iter().any(|(s, _)| s == "临时拼音"),
            "定制版藏的节在建树期就不出现"
        );
        assert!(
            m.sections_of("input")
                .iter()
                .any(|(s, _)| s == "字符与标点"),
            "用户藏的节仍在清单里（运行期隐藏，逃生口要能放回来）"
        );
        assert!(
            m.section_user_hidden("input", "字符与标点"),
            "用户那一份要留标记，页面据此挂运行期判据"
        );
        assert!(
            m.sections_of("schema")
                .iter()
                .any(|(s, _)| s == "主方案设置"),
            "别的分区不受牵连"
        );
        assert!(
            m.sections_of("input").iter().any(|(s, _)| s == "标点配对"),
            "输入页同组的别的节不受牵连"
        );
    }

    /// 定制版赢在「谁藏的」上：同一项两边都藏时，`custom_hidden` 不因用户规则失效。
    #[test]
    fn custom_marks_win_when_both_hide() {
        let key = "ui.candidate.layout";
        let (custom, _) = SettingsHide::parse(&[&format!("key:{key}")]);
        let m = settings_manifest_full(&custom, false, &uh(&[&format!("key:{key}")]), false);
        let it = m.get(key).expect("前提：该项在清单里");
        assert!(it.hidden);
        assert!(it.custom_hidden, "定制版藏的要留着 custom 标记");
        assert!(it.user_hidden, "用户也写了同一条，两边都记");
    }

    /// 用户逃生口只放回**用户自己**藏的，不影响定制版那一份。
    #[test]
    fn user_escape_hatch_does_not_reveal_custom() {
        let (custom, _) = SettingsHide::parse(&["key:ui.candidate.layout"]);
        let m = settings_manifest_full(&custom, false, &uh(&["key:ui.font.family"]), true);
        assert!(
            m.get("ui.font.family").is_some_and(|i| !i.hidden),
            "用户藏的要放回"
        );
        assert!(
            m.get("ui.candidate.layout").is_some_and(|i| i.hidden),
            "定制版藏的不受用户逃生口影响"
        );
    }

    /// 定制版逃生口只放回定制版那一份。用户藏的在清单里本来就**没折**（运行期判据），
    /// 故这里断言的是「它的标记与生效状态不受定制版逃生口影响」。
    #[test]
    fn custom_escape_hatch_does_not_reveal_user() {
        let (custom, _) = SettingsHide::parse(&["key:ui.candidate.layout"]);
        let m = settings_manifest_full(&custom, true, &uh(&["key:ui.font.family"]), false);
        assert!(
            m.get("ui.candidate.layout").is_some_and(|i| !i.hidden),
            "定制版逃生口放回它自己藏的"
        );
        assert!(
            m.get("ui.font.family")
                .is_some_and(|i| i.user_hidden && !i.hidden),
            "用户藏的不折 `hidden`（运行期判据生效），标记仍在"
        );
    }

    /// `section:` 规则记下整张卡，运行期据此隐藏（卡片与二级导航一并消失）。
    #[test]
    fn section_rule_removes_the_whole_card() {
        let full =
            settings_manifest_full(&SettingsHide::default(), false, &UserHide::default(), false);
        assert!(
            full.sections_of("keys")
                .iter()
                .any(|(s, _)| s == "候选操作")
        );
        let m = folded(&["section:keys/候选操作"]);
        assert!(
            m.section_user_hidden("keys", "候选操作"),
            "整节标记要落上，页面据此挂运行期判据"
        );
        assert!(
            m.sections_of("keys").iter().any(|(s, _)| s == "中英文切换"),
            "同组别的节不受牵连"
        );
        // 组成员都带 user_hidden 标记（卡片整体藏的同时，逐行也标出来）。
        let any_member_hidden = m
            .items
            .values()
            .any(|i| i.section == "候选操作" && i.group == "keys" && i.user_hidden);
        assert!(any_member_hidden, "节内成员应带用户隐藏标记");
    }

    /// 不带分组的 `section:` 在所有分组里命中同名分区。
    #[test]
    fn section_rule_without_group_matches_every_group() {
        let any = uh(&["section:统计设置"]);
        assert!(any.hides_section("stats", "统计设置"));
        assert!(any.hides_section("advanced", "统计设置"));
        let one = uh(&["section:stats/统计设置"]);
        assert!(one.hides_section("stats", "统计设置"));
        assert!(!one.hides_section("advanced", "统计设置"));
    }

    /// 手写卡的 `feature:` 与定制版**各认各的**：`feature_hidden` 只答定制版，
    /// 用户那一份记在 `feature_user_hidden`（页面据此挂运行期判据）。
    #[test]
    fn feature_rule_hides_handwritten_card() {
        let m = folded(&["feature:backup"]);
        assert!(!m.feature_hidden("backup"), "用户藏的走运行期，不折建树期");
        assert!(m.feature_user_hidden("backup"), "用户那份要有标记");
        assert!(!m.feature_hidden("integration"));
        assert!(!m.feature_user_hidden("integration"));
    }

    /// 搜索索引**保留**用户藏掉的条目（索引不折用户清单），过滤发生在
    /// `SearchState::refresh`：按当前开关状态决定列不列出来。
    ///
    /// 为什么不在索引层折：用户藏完还要能打开逃生口把它们搜回来，而索引是启动时建一次
    /// 的静态数据。这条与定制版正好相反（后者在建树期就折掉）。
    #[test]
    fn user_hidden_items_stay_in_index_but_are_filtered_at_query_time() {
        let m = folded(&["key:ui.candidate.*", "feature:backup"]);
        let idx = crate::search::index::build_index(&m);
        assert!(
            idx.iter().any(|e| e.id.starts_with("ui.candidate.")),
            "索引保留被藏的项（逃生口打开时要搜得到）"
        );
        assert!(
            idx.iter().any(|e| e.id == "advanced:backup"),
            "手写条目同样保留"
        );

        // 过滤判据：与页面同一套（`hides_key` / `hides_section` / `hides_feature`）。
        let plan = uh(&["key:ui.candidate.*", "feature:backup"]);
        let visible = |e: &crate::search::index::SearchEntry| {
            if let Some(f) = e.target_feature() {
                return !plan.hides_feature(f);
            }
            match m.get(&e.id) {
                Some(item) => {
                    !plan.hides_key(&item.key) && !plan.hides_section(&item.group, &item.section)
                }
                None => true,
            }
        };
        assert!(
            idx.iter()
                .filter(|e| visible(e))
                .all(|e| !e.id.starts_with("ui.candidate.")),
            "被藏的项查询时不该列出来"
        );
        assert!(
            idx.iter()
                .filter(|e| visible(e))
                .all(|e| e.id != "advanced:backup"),
            "被藏的手写卡查询时不该列出来"
        );
        assert!(
            idx.iter()
                .any(|e| e.id == "advanced:integration" && visible(e)),
            "别的条目仍在"
        );
    }

    /// 宿主被用户藏掉时，**不在建树期**连带藏成员：对话框由宿主行打开，宿主看不见就
    /// 打不开；而用户随时可能打开逃生口把宿主放回来，那时成员必须还在。
    ///
    /// （定制版那条路仍会连带折掉，见 `hide_orphaned_dialog_members` 的说明。）
    #[test]
    fn user_hidden_host_keeps_its_dialog_members() {
        let host = "input.symbol.smart_chars";
        let m = folded(&[&format!("key:{host}")]);
        assert!(m.get(host).unwrap().user_hidden);
        assert!(
            m.get("input.symbol.english_punct_mode")
                .is_some_and(|i| !i.hidden),
            "成员不能在建树期折掉，否则逃生口放回宿主后框里是空的"
        );
    }

    /// 用户把一整页的清单项全藏了：清单不折，故页面树仍建得出来（逃生口要能放回），
    /// 而**运行期**侧栏入口会因所有分区不可见而消失。
    ///
    /// 这里断言前半段（清单仍在），后半段由 `section_vis` 的判据保证。
    #[test]
    fn fully_user_hidden_page_stays_buildable() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let full = Manifest::from_embedded();
        let keys: Vec<String> = full
            .order
            .iter()
            .filter(|k| full.get(k).is_some_and(|i| i.group == "keys"))
            .map(|k| format!("key:{k}"))
            .collect();
        let raw: Vec<&str> = keys.iter().map(String::as_str).collect();
        let app = Rc::new(RefCell::new(crate::state::AppState::empty()));
        app.borrow_mut().manifest = folded(&raw);
        let st = app.borrow();
        let tabs = crate::pages::all_tabs(&app, &st);
        let i = crate::cli::page_to_index("keys").unwrap();
        assert!(
            !tabs[i].empty,
            "用户隐藏不折清单：页面照样建得出来，入口消失交给运行期判据"
        );
        drop(st);
        let j = crate::cli::page_to_index("input").unwrap();
        let st = app.borrow();
        assert!(
            !crate::pages::all_tabs(&app, &st)[j].empty,
            "输入页不受牵连"
        );
    }

    /// 方案设置对话框里同名的方案级项跟着藏：判据是 `user_hidden` 标记（**不是**
    /// `hidden`——用户清单是运行期显隐）。
    ///
    /// `SchemaManagerState::custom_hidden` 正是按 `custom_hidden || user_hidden` 判的，
    /// 故这里钉的是那两个标记。
    #[test]
    fn scheme_dialog_follows_user_hide() {
        let m = folded(&["key:schema.codetable.top_code_commit"]);
        let it = m.get("schema.codetable.top_code_commit").unwrap();
        assert!(!it.hidden, "用户隐藏不折建树期");
        assert!(it.user_hidden, "运行期判据与方案对话框都靠这个标记");
    }
}
