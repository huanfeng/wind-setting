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
            full.sections_of("input")
                .iter()
                .any(|(s, _)| s == "临时英文"),
            "前提：出厂清单有「临时英文」分区"
        );
        // 只藏前缀那一批还不够：「临时英文」分区里混着 `input.english_case_cycle_key`。
        // 分区成员 ≠ 键前缀，这正是定制者最容易踩的地方，文档里要给出分区对照。
        let partly = settings_manifest(&hide(&["key:input.temp_english.*"]), false);
        assert!(
            partly
                .sections_of("input")
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
            !m.sections_of("input").iter().any(|(s, _)| s == "临时英文"),
            "整节被隐藏后分区不应再返回"
        );
        // 别的分区不受牵连。
        assert!(m.sections_of("input").iter().any(|(s, _)| s == "临时拼音"));
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
        assert!(m.sections_of("input").iter().any(|(s, _)| s == "临时英文"));
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
        assert!(scheme_config_section(&st, "混输方案配置", "混输行为", false).is_none());
        assert!(scheme_config_section(&st, "码表方案配置", "上屏行为", false).is_some());

        let mut pinyin = selectors_of("候选行为");
        st.manifest = settings_manifest(&hide_owned(&pinyin), false);
        assert!(
            scheme_config_section(&st, "拼音方案配置", "候选行为", true).is_some(),
            "模糊音还在，卡片不能消失"
        );
        pinyin.push("key:schema.pinyin.fuzzy.*".into());
        st.manifest = settings_manifest(&hide_owned(&pinyin), false);
        assert!(scheme_config_section(&st, "拼音方案配置", "候选行为", true).is_none());
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

    #[test]
    fn wizard_feature() {
        assert!(wizard_hidden(&hide(&["feature:wizard"])));
        assert!(!wizard_hidden(&hide(&["key:input.temp_english.*"])));
    }
}
