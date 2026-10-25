//! 用户自己的「设置界面精简」：隐藏个别设置项 / 整个分区。
//!
//! 与定制版的 `data_custom/custom.toml` `[settings] hide` 是**两套独立配置**：
//!
//! | | 定制版 | 本模块 |
//! |---|---|---|
//! | 存放 | 安装根 `data_custom/custom.toml` | 用户目录 `wind_setting_hide.toml` |
//! | 由谁维护 | 定制者随包分发 | 最终用户在设置界面里自己勾 |
//! | 逃生口 | 高级页「显示定制版隐藏的设置」 | 「界面细节」里的「显示已隐藏的设置」 |
//!
//! **两套同时生效、取并集**：定制版藏掉的项，用户侧的配置既不能把它放出来、也不需要
//! 重复写一遍——定制版优先。两者各有一个逃生口，只作用于自己那一份。
//!
//! # 语义
//!
//! 隐藏只影响**显示**：被隐藏的设置照常生效，值仍来自配置文件，本程序不改写它。
//! `enabled = false` 时整份文件不再生效（但**不删除**，下次打开开关原样恢复）。
//!
//! # 选择器
//!
//! - `key:<配置键>` —— 精确；以 `*` 结尾按前缀匹配。匹配语义与定制版同一套
//!   （[`wind_config::SettingsSelector`]），故也连带视图项 `a.b#视图`。
//! - `section:<分区名>` / `section:<分组>/<分区名>` —— 整张卡片。分区名就是界面上
//!   卡片标题；带分组可消歧（不同页可能有同名分区）。
//! - `feature:<id>` —— 设置清单之外的手写卡，取值见 [`wind_config::SETTINGS_FEATURES`]。
//!
//! 不提供 `page:` 选择器：整页隐藏会连带侧栏、二级导航与深链跳转，属于另一层设计。
//! 用户要清掉一整页，逐个藏它的分区即可。
//!
//! # 生效时机：即时
//!
//! 与定制版 `[settings] hide` **不同**，用户清单在**运行期**判定：行、卡片、手写卡各带
//! 一个 `visible_when` 读 [`UserHideStore::version`]，改完当场重排，不必重开窗口。
//!
//! 定制版不必如此——它是随包分发的静态配置，用不着热更；而用户是在设置窗里一边藏一边
//! 看效果，每改一次都得关窗重开就完全没法评估。
//!
//! 隐藏**只影响显示**，行照常登记 writer / loader：这样保存与「恢复本页」都会带上它的
//! 值（用户看不见 ≠ 该被当成默认值）。代价是被藏的行仍在树上、仍占内存——可接受，
//! 用户藏的通常只有几项。

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use windui::prelude::Signal;

use crate::state::AppState;

/// 文件名。与 `wind_setting.toml` 同目录（见 [`crate::user_dir`]），但**不是**它的一个
/// 子表：这份配置可以整份删除、整份替换，语义上是一份独立的「界面精简清单」。
pub const FILE_NAME: &str = "wind_setting_hide.toml";

/// 磁盘形态。`hide` 里是选择器原文，顺序即用户隐藏的先后（菜单里按它找「哪条规则命中了」）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserHideConfig {
    /// 总开关。`false` = 下面 `hide` 整份不生效（文件保留）。
    #[serde(default)]
    pub enabled: bool,
    /// 「显示已隐藏的设置」逃生口。打开后已隐藏的项重新出现，便于右键取消隐藏。
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default)]
    pub hide: Vec<String>,
}

/// 一条解析过的隐藏规则。
///
/// `key:` / `feature:` 直接复用 [`wind_config::SettingsSelector`]——前缀、视图项这些
/// 语义在定制版那边已经踩实了，这里再写一份必然漂移。`section:` 是本模块独有的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HideRule {
    Core(wind_config::SettingsSelector),
    /// `group = None` 表示只写了分区名（跨分组匹配同名分区）。
    Section {
        group: Option<String>,
        section: String,
    },
}

impl HideRule {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let s = raw.trim();
        if let Some(body) = s.strip_prefix("section:") {
            let body = body.trim();
            if body.is_empty() {
                return Err(format!("`{s}` 没有给出分区名。"));
            }
            let (group, section) = match body.split_once('/') {
                Some((g, sec)) => {
                    let (g, sec) = (g.trim(), sec.trim());
                    if g.is_empty() || sec.is_empty() {
                        return Err(format!(
                            "`{s}` 的分组/分区名不能为空。写成 `section:<分组>/<分区>`。"
                        ));
                    }
                    (Some(g.to_string()), sec.to_string())
                }
                None => (None, body.to_string()),
            };
            if section.contains('*') {
                return Err(format!("`{s}` 里的 `*` 不受支持：分区名请写完整。"));
            }
            return Ok(Self::Section { group, section });
        }
        wind_config::SettingsSelector::parse(s).map(Self::Core)
    }

    /// 写回配置文件的规范形态。
    pub fn as_selector(&self) -> String {
        match self {
            Self::Section { group, section } => match group {
                Some(g) => format!("section:{g}/{section}"),
                None => format!("section:{section}"),
            },
            Self::Core(wind_config::SettingsSelector::Key { pattern, prefix }) => {
                format!("key:{pattern}{}", if *prefix { "*" } else { "" })
            }
            Self::Core(wind_config::SettingsSelector::Feature(id)) => format!("feature:{id}"),
        }
    }

    pub fn matches_key(&self, key: &str) -> bool {
        match self {
            Self::Core(sel) => sel.matches_key(key),
            Self::Section { .. } => false,
        }
    }

    pub fn matches_section(&self, group: &str, section: &str) -> bool {
        match self {
            Self::Section {
                group: g,
                section: s,
            } => match g.as_deref() {
                None => s == section,
                // 分区名本身带斜杠（如 `Emoji/OpenCC`）时，不带分组的写法会被拆成
                // 「分组/分区」；这里把拆开的两半拼回去再比一次。
                Some(g) => (s == section && g == group) || format!("{g}/{s}") == section,
            },
            Self::Core(_) => false,
        }
    }

    pub fn feature(&self) -> Option<&str> {
        match self {
            Self::Core(wind_config::SettingsSelector::Feature(id)) => Some(id),
            _ => None,
        }
    }
}

/// 一份解析好的规则集（纯数据，可克隆）。`enabled` / `show_hidden` 一并带出来，
/// 调用方不必再问 store。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserHide {
    pub enabled: bool,
    pub show_hidden: bool,
    pub rules: Vec<HideRule>,
    /// 被丢弃的坏规则与原因（设置界面可提示；写盘不会保留它们）。
    pub errors: Vec<String>,
}

impl UserHide {
    pub fn from_config(cfg: &UserHideConfig) -> Self {
        let mut rules = Vec::new();
        let mut errors = Vec::new();
        for raw in &cfg.hide {
            if raw.trim().is_empty() {
                continue;
            }
            match HideRule::parse(raw) {
                Ok(r) => {
                    if !rules.contains(&r) {
                        rules.push(r);
                    }
                }
                Err(e) => errors.push(e),
            }
        }
        Self {
            enabled: cfg.enabled,
            show_hidden: cfg.show_hidden,
            rules,
            errors,
        }
    }

    /// 是否真的要把规则应用到清单上。
    pub fn is_active(&self) -> bool {
        self.enabled && !self.show_hidden
    }

    pub fn hides_key(&self, key: &str) -> bool {
        self.is_active() && self.rules.iter().any(|r| r.matches_key(key))
    }

    pub fn hides_section(&self, group: &str, section: &str) -> bool {
        self.is_active() && self.rules.iter().any(|r| r.matches_section(group, section))
    }

    pub fn hides_feature(&self, id: &str) -> bool {
        self.is_active()
            && self
                .rules
                .iter()
                .any(|r| r.feature().is_some_and(|f| f == id))
    }

    pub fn selectors(&self) -> Vec<String> {
        self.rules.iter().map(HideRule::as_selector).collect()
    }
}

struct Stored {
    cfg: UserHideConfig,
    /// 解析后的规则；与 `cfg.hide` 一一对应（坏规则已剔除并记在 `errors`）。
    rules: Vec<HideRule>,
    errors: Vec<String>,
    /// `None` = 不落盘（测试 / 定位不到用户目录）。此时只做内存态。
    path: Option<PathBuf>,
}

/// 进程内共享的用户精简配置。放在 [`AppState`] 里；菜单、页面与「界面细节」窗都通过它
/// 读写同一份，`version` 用来刷新依赖它的文案。
pub struct UserHideStore {
    inner: RefCell<Stored>,
    /// 规则/开关每次变化自增。设置窗内容区建树时已经定型，这个信号只供「界面细节」窗里的
    /// 摘要与按钮可用态实时刷新。
    version: Signal<u64>,
}

impl UserHideStore {
    pub fn load() -> Self {
        // 单测不读真实用户目录：开发机上若装了正式版，测试会读到用户自己的隐藏清单，
        // 结果随环境变化。与 `save_wizard_dismissed` 同理。
        let path = if cfg!(test) {
            None
        } else {
            crate::user_dir::user_config_dir_for_mode(crate::mode::get()).map(|d| d.join(FILE_NAME))
        };
        let Some(path) = path else {
            return Self::from_config(UserHideConfig::default(), None, Vec::new());
        };
        match fs::read_to_string(&path) {
            Ok(text) => match text.parse::<toml::Value>() {
                Ok(v) => match v.try_into::<UserHideConfig>() {
                    Ok(cfg) => Self::from_config(cfg, Some(path), Vec::new()),
                    Err(e) => {
                        tracing::warn!("{FILE_NAME} 字段类型不符，已按默认处理：{e}");
                        backup_broken(&path);
                        Self::from_config(
                            UserHideConfig::default(),
                            Some(path),
                            vec![e.to_string()],
                        )
                    }
                },
                Err(e) => {
                    tracing::warn!("{FILE_NAME} 语法错误，已按默认处理：{e}");
                    backup_broken(&path);
                    Self::from_config(UserHideConfig::default(), Some(path), vec![e.to_string()])
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Self::from_config(UserHideConfig::default(), Some(path), Vec::new())
            }
            Err(e) => {
                tracing::warn!("读取 {FILE_NAME} 失败，已按默认处理：{e}");
                backup_broken(&path);
                Self::from_config(UserHideConfig::default(), Some(path), vec![e.to_string()])
            }
        }
    }

    fn from_config(cfg: UserHideConfig, path: Option<PathBuf>, errors: Vec<String>) -> Self {
        let plan = UserHide::from_config(&cfg);
        let mut errors = errors;
        errors.extend(plan.errors.clone());
        Self {
            inner: RefCell::new(Stored {
                rules: plan.rules,
                cfg,
                errors,
                path,
            }),
            version: windui::prelude::signal(0u64),
        }
    }

    /// 测试 / 独立构建用：不读盘、不落盘。
    pub fn in_memory(cfg: UserHideConfig) -> Self {
        Self::from_config(cfg, None, Vec::new())
    }

    pub fn version(&self) -> Signal<u64> {
        self.version
    }

    fn bump(&self) {
        self.version.set(self.version.get().wrapping_add(1));
    }

    pub fn config(&self) -> UserHideConfig {
        self.inner.borrow().cfg.clone()
    }

    pub fn plan(&self) -> UserHide {
        let st = self.inner.borrow();
        UserHide {
            enabled: st.cfg.enabled,
            show_hidden: st.cfg.show_hidden,
            rules: st.rules.clone(),
            errors: st.errors.clone(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.inner.borrow().cfg.enabled
    }

    pub fn show_hidden(&self) -> bool {
        self.inner.borrow().cfg.show_hidden
    }

    /// 此刻是否要让被隐藏的项**显示出来并打标识**（总开关开着 + 逃生口开着）。
    pub fn marking_hidden(&self) -> bool {
        let st = self.inner.borrow();
        st.cfg.enabled && st.cfg.show_hidden
    }

    /// 运行期判据：这一项目前该不该隐藏。
    ///
    /// 与 [`Self::rule_hiding_key`] 的分工：那个**无视**开关与逃生口，只回答「有没有这条
    /// 规则」（右键菜单要这个）；本方法回答「界面此刻该不该藏它」——逃生口一开就返回 false。
    ///
    /// 直接读 `inner` 而不走 [`Self::plan`]：`visible_when` 每次布局都会为每一行调一次，
    /// `plan()` 每次克隆整个规则表，一页几百行就是几百次分配。
    pub fn hides_key(&self, key: &str) -> bool {
        let st = self.inner.borrow();
        st.cfg.enabled && !st.cfg.show_hidden && st.rules.iter().any(|r| r.matches_key(key))
    }

    /// 同 [`Self::hides_key`]，按整张卡片。
    pub fn hides_section(&self, group: &str, section: &str) -> bool {
        let st = self.inner.borrow();
        st.cfg.enabled
            && !st.cfg.show_hidden
            && st.rules.iter().any(|r| r.matches_section(group, section))
    }

    /// 同 [`Self::hides_key`]，按手写卡。
    pub fn hides_feature(&self, id: &str) -> bool {
        let st = self.inner.borrow();
        st.cfg.enabled
            && !st.cfg.show_hidden
            && st
                .rules
                .iter()
                .any(|r| r.feature().is_some_and(|f| f == id))
    }

    /// **忽略开关与逃生口**的手写卡规则查询（打标识用）：只要清单里有这条规则就为真。
    pub fn rule_hiding_feature(&self, id: &str) -> bool {
        self.inner
            .borrow()
            .rules
            .iter()
            .any(|r| r.feature().is_some_and(|f| f == id))
    }

    pub fn selectors(&self) -> Vec<String> {
        self.inner
            .borrow()
            .rules
            .iter()
            .map(HideRule::as_selector)
            .collect()
    }

    pub fn count(&self) -> usize {
        self.inner.borrow().cfg.hide.len()
    }

    pub fn errors(&self) -> Vec<String> {
        self.inner.borrow().errors.clone()
    }

    /// 改内存态并落盘；落盘失败则整体回滚（界面上的开关据 `version` 退回去），避免
    /// 「界面看着生效、重启后丢失」。`f` 返回 `None` 表示没有变化，不落盘。
    fn mutate<R>(&self, f: impl FnOnce(&mut Stored) -> Option<R>) -> Result<Option<R>, String> {
        let snapshot = {
            let st = self.inner.borrow();
            (st.cfg.clone(), st.rules.clone())
        };
        let Some(out) = f(&mut self.inner.borrow_mut()) else {
            return Ok(None);
        };
        if let Err(e) = self.persist() {
            let mut st = self.inner.borrow_mut();
            (st.cfg, st.rules) = snapshot;
            drop(st);
            self.bump();
            return Err(e);
        }
        Ok(Some(out))
    }

    pub fn set_enabled(&self, on: bool) -> Result<(), String> {
        self.mutate(|st| {
            (st.cfg.enabled != on).then(|| {
                st.cfg.enabled = on;
            })
        })
        .map(|_| ())
    }

    pub fn set_show_hidden(&self, on: bool) -> Result<(), String> {
        self.mutate(|st| {
            (st.cfg.show_hidden != on).then(|| {
                st.cfg.show_hidden = on;
            })
        })
        .map(|_| ())
    }

    /// 追加一条规则。同一条选择器已存在时返回 `Ok(false)`，调用方据此给「本来就藏着」
    /// 的提示。（被更宽的规则覆盖的窄规则仍会追加——取消宽规则后它才显出来，不算重复。）
    pub fn add(&self, selector: &str) -> Result<bool, String> {
        let rule = HideRule::parse(selector)?;
        let canonical = rule.as_selector();
        let added = self.mutate(|st| {
            if st.rules.iter().any(|r| r == &rule) {
                return None;
            }
            st.rules.push(rule);
            st.cfg.hide.push(canonical);
            Some(())
        })?;
        Ok(added.is_some())
    }

    /// 删除一条规则（按规范化后的选择器比较）。返回是否真的删掉了。
    pub fn remove(&self, selector: &str) -> Result<bool, String> {
        let target = HideRule::parse(selector)?.as_selector();
        let removed = self.mutate(|st| {
            let before = st.cfg.hide.len();
            st.cfg.hide.retain(|s| {
                HideRule::parse(s)
                    .map(|r| r.as_selector())
                    .unwrap_or_else(|_| s.clone())
                    != target
            });
            if st.cfg.hide.len() == before {
                return None;
            }
            st.rules.retain(|r| r.as_selector() != target);
            Some(())
        })?;
        Ok(removed.is_some())
    }

    /// 清空全部规则。开关与逃生口保持原样。
    pub fn reset(&self) -> Result<usize, String> {
        let n = self.mutate(|st| {
            let n = st.cfg.hide.len();
            if n == 0 {
                return None;
            }
            st.cfg.hide.clear();
            st.rules.clear();
            Some(n)
        })?;
        Ok(n.unwrap_or(0))
    }

    /// 当前藏着 `key` 的那条规则（规范化原文）。用于右键菜单的「取消隐藏」。
    ///
    /// **只看规则本身，不看开关与逃生口**：逃生口打开时恰恰最需要这个——已隐藏的项正显示
    /// 在界面上，用户右键它就是要取消隐藏。若这里跟着 `is_active()` 返回 `None`，菜单会
    /// 给「隐藏这一项」（再加一次，提示「已在清单里」），用户在专用模式里反而取消不掉。
    /// 功能总开关由菜单自己在入口处判定（见 [`crate::hide_menu::hiding_enabled`]）。
    pub fn rule_hiding_key(&self, key: &str) -> Option<String> {
        self.inner
            .borrow()
            .rules
            .iter()
            .find(|r| r.matches_key(key))
            .map(HideRule::as_selector)
    }

    /// 当前藏着某个分区的那条规则。`group = None` 时匹配任意分组。取舍同
    /// [`Self::rule_hiding_key`]。
    pub fn rule_hiding_section(&self, group: Option<&str>, section: &str) -> Option<String> {
        self.inner
            .borrow()
            .rules
            .iter()
            .find(|r| match r {
                HideRule::Section {
                    group: rg,
                    section: rs,
                } => {
                    rs == section && group.is_none_or(|g| rg.as_deref() == Some(g) || rg.is_none())
                }
                HideRule::Core(_) => false,
            })
            .map(HideRule::as_selector)
    }

    fn persist(&self) -> Result<(), String> {
        self.bump();
        let (path, text) = {
            let st = self.inner.borrow();
            let Some(path) = st.path.clone() else {
                return Ok(());
            };
            let text = toml::to_string_pretty(&st.cfg)
                .map_err(|e| format!("序列化设置精简配置失败：{e}"))?;
            (path, text)
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败：{e}"))?;
        }
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text).map_err(|e| format!("写入临时配置失败：{e}"))?;
        // `rename` 在 Windows 上也会覆盖已有文件；先删再改名会在两步之间留下「文件不存在」的窗口。
        fs::rename(&tmp, &path).map_err(|e| format!("替换配置失败：{e}"))?;
        Ok(())
    }
}

/// [`AppState`] 里的规则集快照（供清单折叠与页面构建只读使用）。
impl AppState {
    pub fn user_hide(&self) -> UserHide {
        self.user_hide_store.plan()
    }
}

/// 便捷构造：测试里大量需要「没有用户隐藏」。
/// 配置文件损坏时按默认处理，随后用户的下一次改动会覆盖原文件；先留一份 `.bad`，
/// 免得手写的规则被静默吃掉。
fn backup_broken(path: &std::path::Path) {
    let _ = fs::copy(path, path.with_extension("toml.bad"));
}

#[cfg(test)]
pub fn none() -> UserHideStore {
    UserHideStore::in_memory(UserHideConfig::default())
}

/// 落盘路径（供文档与测试断言）。
#[cfg(test)]
pub fn path_for_mode(mode: crate::mode::AppMode) -> Option<PathBuf> {
    crate::user_dir::user_config_dir_for_mode(mode).map(|d| d.join(FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(cfg: UserHideConfig) -> UserHideStore {
        UserHideStore::in_memory(cfg)
    }

    fn cfg(raw: &[&str]) -> UserHideConfig {
        UserHideConfig {
            enabled: true,
            show_hidden: false,
            hide: raw.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn key_selector_shares_custom_semantics() {
        let h = UserHide::from_config(&cfg(&["key:schema.mix_modes.*"]));
        assert!(h.hides_key("schema.mix_modes"));
        assert!(h.hides_key("schema.mix_modes#members"));
        assert!(!h.hides_key("schema.mix_modes_x"));
        let h = UserHide::from_config(&cfg(&["key:ui.candidate.layout"]));
        assert!(h.hides_key("ui.candidate.layout#h"));
        assert!(!h.hides_key("ui.candidate.layout_x"));
    }

    #[test]
    fn section_selector_supports_optional_group() {
        let any = HideRule::parse("section:统计设置").unwrap();
        assert!(any.matches_section("stats", "统计设置"));
        assert!(any.matches_section("advanced", "统计设置"));
        let one = HideRule::parse("section:ui/主题").unwrap();
        assert!(one.matches_section("ui", "主题"));
        assert!(!one.matches_section("advanced", "主题"));
        assert_eq!(one.as_selector(), "section:ui/主题");
    }

    #[test]
    fn malformed_rules_are_dropped_with_errors() {
        let h = UserHide::from_config(&cfg(&[
            "key:ui.font.family",
            "bogus",
            "section:",
            "feature:not_a_feature",
        ]));
        assert_eq!(h.rules.len(), 1);
        assert_eq!(h.errors.len(), 3);
        assert!(h.hides_key("ui.font.family"));
    }

    /// 关闭总开关与打开逃生口都让规则**整体**失效，但规则本身仍在（文件不被清空）。
    #[test]
    fn disabled_and_show_hidden_make_rules_inert() {
        let mut c = cfg(&["key:ui.font.family", "feature:backup"]);
        let off = UserHide::from_config(&UserHideConfig {
            enabled: false,
            ..c.clone()
        });
        assert!(!off.is_active());
        assert!(!off.hides_key("ui.font.family"));
        assert!(!off.hides_feature("backup"));
        assert_eq!(off.rules.len(), 2, "规则仍在，只是不生效");

        c.show_hidden = true;
        let shown = UserHide::from_config(&c);
        assert!(!shown.is_active());
        assert!(!shown.hides_key("ui.font.family"));
        assert_eq!(shown.rules.len(), 2);
    }

    #[test]
    fn feature_rule_matches_only_features() {
        let h = UserHide::from_config(&cfg(&["feature:backup"]));
        assert!(h.hides_feature("backup"));
        assert!(!h.hides_feature("wizard"));
        assert!(!h.hides_key("backup"));
    }

    /// 去重：同一规则写两遍只留一条；不同写法（`section:ui/主题` 与 `section:主题`）
    /// 是两条规则，各自可删。
    #[test]
    fn duplicate_rules_collapse() {
        let h = UserHide::from_config(&cfg(&[
            "key:ui.font.family",
            "key:ui.font.family",
            " section:统计设置 ",
        ]));
        assert_eq!(h.rules.len(), 2);
        assert_eq!(h.selectors(), ["key:ui.font.family", "section:统计设置"]);
    }

    #[test]
    fn store_add_remove_and_reset_roundtrip() {
        let s = store(UserHideConfig::default());
        assert!(s.add("key:ui.font.family").unwrap());
        assert!(!s.add("key:ui.font.family").unwrap(), "重复添加是空操作");
        assert!(s.add("section:ui/主题").unwrap());
        assert_eq!(s.count(), 2);
        assert_eq!(
            s.rule_hiding_key("ui.font.family").as_deref(),
            Some("key:ui.font.family")
        );
        assert_eq!(
            s.rule_hiding_section(Some("ui"), "主题").as_deref(),
            Some("section:ui/主题")
        );
        assert!(s.remove("key:ui.font.family").unwrap());
        assert!(!s.remove("key:ui.font.family").unwrap());
        assert_eq!(s.reset().unwrap(), 1);
        assert_eq!(s.count(), 0);
        assert!(s.rule_hiding_key("ui.font.family").is_none());
    }

    /// 前缀规则命中单项时，「取消隐藏」删的是**那条前缀**（整组一起放出来），
    /// 菜单不能只删一个精确 key——那样点十几次才放完。
    #[test]
    fn rule_hiding_key_returns_the_broad_rule() {
        let s = store(cfg(&["key:ui.candidate.*"]));
        assert_eq!(
            s.rule_hiding_key("ui.candidate.layout").as_deref(),
            Some("key:ui.candidate.*")
        );
        assert_eq!(s.count(), 1);
        s.remove("key:ui.candidate.*").unwrap();
        assert_eq!(s.count(), 0);
    }

    /// 逃生口打开时规则仍“在”，但 `is_active()` 为假（清单不折）；
    /// `rule_hiding_*` 照常返回规则——正因为它此刻显示着，用户才要右键取消隐藏。
    #[test]
    fn escape_hatch_keeps_lookup_but_stops_folding() {
        let s = store(UserHideConfig {
            enabled: true,
            show_hidden: true,
            hide: vec!["key:ui.font.family".into()],
        });
        assert!(!s.plan().is_active(), "逃生口打开时清单不折");
        assert!(!s.plan().hides_key("ui.font.family"), "项应重新可见");
        assert_eq!(
            s.rule_hiding_key("ui.font.family").as_deref(),
            Some("key:ui.font.family"),
            "规则仍在，菜单要能提供「取消隐藏」"
        );
    }

    #[test]
    fn enabled_and_show_hidden_persist_in_memory() {
        let s = store(UserHideConfig::default());
        s.set_enabled(true).unwrap();
        s.set_show_hidden(true).unwrap();
        assert!(s.enabled());
        assert!(s.show_hidden());
    }

    /// 落盘失败必须整体回滚：内存里留着「已隐藏」而磁盘上没有，重启后就悄悄丢了。
    #[test]
    fn failed_write_rolls_back_memory_state() {
        // 可执行文件当「目录」用：create_dir_all 在任何平台都会失败。
        let bad = std::env::current_exe()
            .unwrap()
            .join("nope")
            .join(FILE_NAME);
        let s = UserHideStore::from_config(UserHideConfig::default(), Some(bad), Vec::new());
        assert!(s.add("key:ui.font.family").is_err());
        assert_eq!(s.count(), 0, "规则不得留在内存里");
        assert!(!s.plan().hides_key("ui.font.family"));
        assert!(s.set_show_hidden(true).is_err());
        assert!(!s.show_hidden(), "开关也要退回去");
    }

    /// 分区名自带斜杠时，不带分组的写法会被拆开；匹配要能拼回去。
    #[test]
    fn section_name_containing_slash_still_matches() {
        let r = HideRule::parse("section:Emoji/OpenCC").unwrap();
        assert!(r.matches_section("input", "Emoji/OpenCC"));
        assert!(!r.matches_section("input", "Emoji"));
        let r = HideRule::parse("section:input/字符与标点").unwrap();
        assert!(r.matches_section("input", "字符与标点"));
        assert!(!r.matches_section("ui", "字符与标点"));
    }

    #[test]
    fn version_advances_on_every_change() {
        let s = store(UserHideConfig::default());
        let v0 = s.version().get();
        s.add("key:ui.font.family").unwrap();
        let v1 = s.version().get();
        assert!(v1 > v0);
        s.set_enabled(true).unwrap();
        assert!(s.version().get() > v1);
    }

    /// 文件里写坏的规则不影响其余规则，也不让整个加载失败。
    #[test]
    fn bad_rule_does_not_poison_the_rest() {
        let h = UserHide::from_config(&cfg(&["key:ui.font.family", "section:", "key:ui.*"]));
        assert!(h.hides_key("ui.font.family"));
        assert!(h.hides_key("ui.theme.name"));
        assert_eq!(h.errors.len(), 1);
    }

    /// 落盘路径与 `wind_setting.toml` 同目录（`user_dir` 一条链决定），只是文件名不同。
    /// 这条守的是「两份配置别分家」——分开的话便携包里会把隐藏清单丢在机器上。
    #[test]
    fn path_sits_beside_the_setting_config() {
        let mode = crate::mode::AppMode::Release;
        // 定位不到用户目录的环境（精简的 CI 容器等）没有可比对的路径，直接跳过。
        let Some(p) = path_for_mode(mode) else {
            return;
        };
        assert!(p.ends_with(FILE_NAME), "{}", p.display());
        let cfg_path = crate::setting_config::config_path_for_mode(mode).expect("同上");
        assert_eq!(p.parent(), cfg_path.parent(), "{}", p.display());
    }

    #[test]
    fn none_has_no_rules() {
        let s = none();
        assert_eq!(s.count(), 0);
        assert!(!s.enabled());
        assert!(s.plan().rules.is_empty());
    }
}
