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
        let host = "ui.candidate.comment_template_horizontal";
        let m = settings_manifest(&hide(&[&format!("key:{host}")]), false);
        assert!(
            m.get("ui.candidate.comment_template_vertical")
                .unwrap()
                .hidden
        );
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
}
