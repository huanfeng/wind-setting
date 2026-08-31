//! 搜索索引 —— 「设置界面上所有能被搜到的东西」的清单。
//!
//! 三个来源：
//! 1. **manifest 派生**（约 255 条）：`settings_manifest.toml` 的每个非 hidden 项；
//! 2. **手写页补充**（约 40 条）：方案管理/词库/统计/备份/桌面集成/关于这些手写卡片，
//!    manifest 里没有它们，只能登记；
//! 3. **方案级**（约 10 条）：方案设置对话框里的分节。它们「哪个方案」未定，
//!    故不生成跳转，只给指示信息。
//!
//! ## 为什么 group 不等于 Tab
//!
//! 最容易想当然的一处：以为 manifest 的 `group` 就是页面。实际上
//! - `general` 组渲染在**输入页**（`pages::input_page` 把它 extend 进去）；
//! - `schema` 组只有「主方案设置」直接成行，其余四个 section 全部收在方案页
//!   「全局方案配置」那四个按钮的对话框里（`manifest::build_scheme_config_button`）。
//!
//! 照 group 直接映射到 Tab，搜出来的项会指向一个根本不在那一页的锚点——
//! 跳过去什么也不会发生，且没有任何报错。故此处显式建表 [`placement_of`]，
//! 并由 `sections_have_placement` 守门：新增 (group, section) 组合而忘了登记就测试失败。

#[cfg(test)]
use std::collections::BTreeSet;

use crate::manifest::{MItem, Manifest};

/// 跳转目标 —— 「点了这条结果，界面该发生什么」。
///
/// `anchor` 是字符串键，与页面构建时挂上的锚点一一对应（P2 落地）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JumpTarget {
    /// 页面上直接成行的项：切页 → 滚动 → 闪烁。
    Row { tab: usize, anchor: String },
    /// 对话框内的项：切页 → 滚到宿主行 → 打开对话框 → 闪烁框内那行。
    RowInDialog {
        tab: usize,
        /// 宿主行（那个「设置…」/「码表方案配置」按钮所在行）的锚点。
        host_anchor: String,
        /// 对话框标识，与 `FieldRegistry::dialog_opens` 的键一致。
        dialog: String,
        anchor: String,
    },
    /// 手写页里没有细分锚点的整块：切页 → 闪烁整张卡片。
    Card { tab: usize, anchor: String },
    /// 方案级：跳到方案页，附指示条路径（「哪个方案」要用户自己选）。
    SchemaLevel { hint_path: Vec<String> },
}

impl JumpTarget {
    /// 目标所在 Tab（`SchemaLevel` 恒为方案页）。
    pub fn tab(&self) -> usize {
        match self {
            Self::Row { tab, .. } | Self::RowInDialog { tab, .. } | Self::Card { tab, .. } => *tab,
            Self::SchemaLevel { .. } => page_tab("schema"),
        }
    }
}

/// 一条可被搜到的设置项。
#[derive(Clone, Debug)]
pub struct SearchEntry {
    /// 稳定 id：manifest 项即其 key，手写项用 `<page>:<slug>`，方案级用 `schema-level:<slug>`。
    pub id: String,
    /// 主标题，结果列表首行；**拼音首字母只对它生效**（见 [`crate::search::matcher`]）。
    pub title: String,
    /// 附加检索词：hint 首句、配置 key 末段、别名。只参与子串匹配。
    pub keywords: Vec<String>,
    /// 面包屑，结果列表第二行。
    pub path: Vec<String>,
    pub target: JumpTarget,
}

/// 页 id → Tab 下标。走 `cli::page_to_index` 而非字面量：`PAGES` 与 `all_tabs` 的顺序
/// 本就靠约定对齐，这里再手抄一份下标，将来插一页就会指到隔壁那页上。
///
/// 页 id 是本模块写死的常量（都在 `PAGES` 里），故解析不出来属于编程错误，
/// 用 0 兜底并由 `every_placement_page_is_known` 守门。
fn page_tab(page: &str) -> usize {
    crate::cli::page_to_index(page).unwrap_or(0)
}

/// 一个 (group, section) 的渲染落点。
enum Placement {
    /// 页面上直接成行。
    Page(&'static str),
    /// 方案页「全局方案配置」卡片里某个按钮打开的对话框。
    SchemeDialog {
        /// 按钮标签，兼对话框标题（如「码表方案配置」）。
        label: &'static str,
    },
}

/// 方案页「全局方案配置」四个按钮的锚点/对话框键前缀。
///
/// 与 `pages::scheme_page` 里那四行一一对应；P2 给那四行挂锚点时用同一个键。
const SCHEME_CFG: &str = "scheme_cfg";

/// (group, section) → 落点。返回 `None` = 未登记，由守门测试报出。
fn placement_of(group: &str, section: &str) -> Option<Placement> {
    let p = match (group, section) {
        // ── 方案组：只有主方案设置直接成行 ──
        ("schema", "主方案设置") => Placement::Page("schema"),
        // 其余四节收在「全局方案配置」的四个对话框里。「模糊音」由
        // `build_scheme_config_button(.., with_fuzzy=true)` 并进拼音方案配置。
        ("schema", "上屏行为") => Placement::SchemeDialog {
            label: "码表方案配置",
        },
        ("schema", "混输行为") => Placement::SchemeDialog {
            label: "混输方案配置",
        },
        ("schema", "候选行为") | ("schema", "模糊音") => Placement::SchemeDialog {
            label: "拼音方案配置",
        },
        ("schema", "英文行为") => Placement::SchemeDialog {
            label: "英文方案配置",
        },
        // ── 其余组：整组落在一页上 ──
        // general 组没有自己的页，跟着 input 页渲染（见模块文档）。
        ("input", _) | ("general", _) => Placement::Page("input"),
        ("keys", _) => Placement::Page("keys"),
        ("ui", _) => Placement::Page("ui"),
        ("advanced", _) => Placement::Page("advanced"),
        ("stats", _) => Placement::Page("stats"),
        _ => return None,
    };
    Some(p)
}

/// 分组 key → 面包屑首段（页面名）。与 `pages::all_tabs` 的标签一致。
fn group_label(group: &str) -> &'static str {
    match group {
        "schema" => "方案",
        // general 组渲染在输入页，面包屑也就该写「输入」——写「常规」会指向一个
        // 界面上根本不存在的页名。
        "input" | "general" => "输入",
        "keys" => "按键",
        "ui" => "外观",
        "stats" => "统计",
        "advanced" => "高级",
        _ => "设置",
    }
}

/// 从 manifest 项取附加检索词：hint 首句 + 配置 key 末段。
///
/// hint 只取首句且截断：它是一整段说明，整段进检索词会让「的」「是」这类字
/// 匹配上几乎所有项，把结果列表变成噪音。
fn keywords_of(key: &str, item: &MItem) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(h) = item.hint.as_deref() {
        let head: String = h
            .split(['；', ';', '。', '（', '('])
            .next()
            .unwrap_or(h)
            .chars()
            .take(28)
            .collect();
        if !head.is_empty() {
            out.push(head);
        }
    }
    // 配置 key 的末两段：用户搜 `codetable` / `auto_phrase` 这类英文标识时能命中。
    let tail: Vec<&str> = key.rsplit('.').take(2).collect();
    for t in tail.into_iter().rev() {
        if !t.is_empty() {
            out.push(t.to_string());
        }
    }
    out
}

/// 该 manifest 项在**页面/对话框上是否独立成一行**——不成行的项跳过索引，
/// 因为它们没有可跳转的落点。
///
/// 两类不成行：
/// - `dialog_button` 且有 `gate_key`：它与门控开关合并成一行，行的锚点归开关；
/// - 被 `dialog_extra_for` / `dialog_group` 收编的项：它们在别人的对话框里，
///   仍然可搜（落点是那个对话框），故**不**在此跳过。
fn is_merged_into_gate(item: &MItem, manifest: &Manifest) -> bool {
    item.typ == "dialog_button"
        && item.opens_dialog.is_none()
        && item
            .gate_key
            .as_deref()
            .is_some_and(|g| manifest.get(g).is_some())
}

/// 由 manifest 派生索引条目。
fn manifest_entries(manifest: &Manifest) -> Vec<SearchEntry> {
    let mut out = Vec::new();
    for key in &manifest.order {
        let Some(item) = manifest.get(key) else {
            continue;
        };
        if item.hidden || item.label.is_empty() {
            continue;
        }
        if is_merged_into_gate(item, manifest) {
            continue;
        }
        let Some(placement) = placement_of(&item.group, &item.section) else {
            continue; // 未登记的组合：由 sections_have_placement 报出，不静默塞进索引
        };
        let page_name = group_label(&item.group);
        // 寄居在别人对话框里的项（`dialog_extra_for` / `dialog_group`），面包屑要
        // 带上宿主对话框那一层，否则用户看到的路径会停在卡片上、找不到东西。
        let host = item
            .dialog_extra_for
            .clone()
            .or_else(|| dialog_group_host(manifest, item));
        let (target, mut path) = match placement {
            Placement::Page(page) => {
                let tab = page_tab(page);
                match &host {
                    Some(h) => (
                        JumpTarget::RowInDialog {
                            tab,
                            host_anchor: h.clone(),
                            dialog: h.clone(),
                            anchor: key.clone(),
                        },
                        vec![
                            page_name.to_string(),
                            item.section.clone(),
                            dialog_label(manifest, h),
                        ],
                    ),
                    None => (
                        JumpTarget::Row {
                            tab,
                            anchor: key.clone(),
                        },
                        vec![page_name.to_string(), item.section.clone()],
                    ),
                }
            }
            Placement::SchemeDialog { label } => {
                let host = format!("{SCHEME_CFG}:{label}");
                (
                    JumpTarget::RowInDialog {
                        tab: page_tab("schema"),
                        host_anchor: host.clone(),
                        dialog: host,
                        anchor: key.clone(),
                    },
                    vec![
                        page_name.to_string(),
                        "全局方案配置".to_string(),
                        label.to_string(),
                    ],
                )
            }
        };
        // 对话框内子分区（如「词频调整」）再补一层，路径才够指到那一行。
        if let Some(sub) = item.subsection.as_deref() {
            path.push(sub.to_string());
        }
        out.push(SearchEntry {
            id: key.clone(),
            title: item.label.clone(),
            keywords: keywords_of(key, item),
            path,
            target,
        });
    }
    out
}

/// 找 `item` 所属 `dialog_group` 的宿主项 key（`opens_dialog == item.dialog_group`）。
fn dialog_group_host(manifest: &Manifest, item: &MItem) -> Option<String> {
    let g = item.dialog_group.as_deref()?;
    manifest
        .order
        .iter()
        .find(|k| manifest.get(k).and_then(|i| i.opens_dialog.as_deref()) == Some(g))
        .cloned()
}

/// 宿主项的对话框显示名：分组对话框用 `dialog_title`，子对话框用「<label> · 扩展设置」。
fn dialog_label(manifest: &Manifest, host_key: &str) -> String {
    match manifest.get(host_key) {
        Some(i) => i
            .dialog_title
            .clone()
            .unwrap_or_else(|| format!("{} · 扩展设置", i.label)),
        None => host_key.to_string(),
    }
}

/// 手写页的一条补充索引。
struct Handwritten {
    /// 锚点 id（英文 slug，P2 给对应卡片挂同名锚点）。
    id: &'static str,
    page: &'static str,
    title: &'static str,
    /// 面包屑第二段（卡片名）。
    section: &'static str,
    /// 空格分隔的附加检索词。
    keywords: &'static str,
}

/// 手写页卡片补充索引。
///
/// ⚠ **编译器不管这张表**——与 `SettingsSection::ALL` 同类风险：新增一张手写卡片而
/// 忘了登记，它就永远搜不到，且没有任何报错。`every_page_has_entries` 只能守住
/// 「整页都漏了」，守不住「漏一张卡」。新增手写卡片时请顺手加一行。
///
/// `rustfmt::skip`：这是一张表，一行一条读起来最清楚；交给 rustfmt 会把每条拆成
/// 七行，22 条变成 150 多行，反倒看不出它是张表。
#[rustfmt::skip]
const HANDWRITTEN: &[Handwritten] = &[
    // ── 方案页 ──
    Handwritten { id: "schema:list", page: "schema", title: "方案列表", section: "输入方案", keywords: "切换 启用 排序 拖动" },
    Handwritten { id: "schema:settings", page: "schema", title: "方案设置", section: "输入方案", keywords: "单个方案 配置 编辑" },
    Handwritten { id: "schema:import", page: "schema", title: "导入方案包", section: "输入方案", keywords: "wpkg 安装 添加" },
    Handwritten { id: "schema:export", page: "schema", title: "导出方案包", section: "输入方案", keywords: "wpkg 备份 分享" },
    Handwritten { id: "schema:url-import", page: "schema", title: "从网址导入方案", section: "输入方案", keywords: "url 在线 下载" },
    // ── 词库 ──
    Handwritten { id: "dict:manage", page: "dict", title: "词库管理", section: "词库", keywords: "词条 编辑 删除 查找" },
    Handwritten { id: "dict:add-word", page: "dict", title: "添加词条", section: "词库", keywords: "造词 新增 自定义" },
    Handwritten { id: "dict:import", page: "dict", title: "导入词库", section: "词库", keywords: "文本 合并 覆盖" },
    Handwritten { id: "dict:phrase-import", page: "dict", title: "短语导入", section: "词库", keywords: "phrase 批量" },
    Handwritten { id: "dict:comment", page: "dict", title: "注释词库", section: "词库", keywords: "comment 提示 释义" },
    // ── 统计 ──
    Handwritten { id: "stats:overview", page: "stats", title: "使用统计", section: "统计", keywords: "字数 击键 效率 图表" },
    Handwritten { id: "stats:clear", page: "stats", title: "清空统计数据", section: "统计", keywords: "重置 删除 归零" },
    // ── 外观 ──
    Handwritten { id: "ui:theme-import", page: "ui", title: "导入主题", section: "主题导入", keywords: "皮肤 toml 网址 安装" },
    Handwritten { id: "ui:theme-editor", page: "ui", title: "主题编辑器连接", section: "主题编辑器", keywords: "皮肤 在线 编辑 服务 端口" },
    // ── 高级 ──
    Handwritten { id: "advanced:integration", page: "advanced", title: "桌面集成", section: "桌面集成", keywords: "协议 关联 注册" },
    Handwritten { id: "advanced:protocol", page: "advanced", title: "协议关联", section: "桌面集成", keywords: "windinput 链接 深链" },
    Handwritten { id: "advanced:file-assoc", page: "advanced", title: "文件关联", section: "桌面集成", keywords: "wpkg 双击 打开方式" },
    Handwritten { id: "advanced:import-export", page: "advanced", title: "导入与导出", section: "导入与导出", keywords: "配置 迁移 剪贴板" },
    Handwritten { id: "advanced:backup", page: "advanced", title: "备份与还原", section: "备份与还原", keywords: "快照 恢复 存档" },
    // ── 关于 ──
    Handwritten { id: "about:version", page: "about", title: "版本信息", section: "关于", keywords: "版本号 内部版本 构建" },
    Handwritten { id: "about:update", page: "about", title: "检查更新", section: "关于", keywords: "升级 新版本 下载" },
    Handwritten { id: "about:logs", page: "about", title: "日志目录", section: "关于", keywords: "log 排查 诊断" },
];

/// 手写页面名（面包屑首段）。
fn page_label(page: &str) -> &'static str {
    match page {
        "schema" => "方案",
        "input" => "输入",
        "keys" => "按键",
        "ui" => "外观",
        "dict" => "词库",
        "stats" => "统计",
        "advanced" => "高级",
        "about" => "关于",
        _ => "设置",
    }
}

fn handwritten_entries() -> Vec<SearchEntry> {
    HANDWRITTEN
        .iter()
        .map(|h| SearchEntry {
            id: h.id.to_string(),
            title: h.title.to_string(),
            keywords: h.keywords.split_whitespace().map(str::to_string).collect(),
            path: vec![page_label(h.page).to_string(), h.section.to_string()],
            target: JumpTarget::Card {
                tab: page_tab(h.page),
                anchor: h.id.to_string(),
            },
        })
        .collect()
}

/// 方案设置对话框里的分节（per-schema）。
///
/// 对应 `dialogs::schema_manager::SettingsSection` 的各变体与 `SETTINGS_TABS`。
/// 这些设置**每个方案各有一份**，「哪个方案」在搜索时未定，故一律落
/// [`JumpTarget::SchemaLevel`]：跳到方案页 + 指示条，由用户自己选方案。
const SCHEMA_LEVEL: &[(&str, &str, &str)] = &[
    // (标题, 所属 TAB, 检索词)
    ("进入方式", "按键设置", "直达 热键 切换 快捷键"),
    ("双拼布局", "基本配置", "方案 键位 自然码 小鹤"),
    ("扩展词库", "基本配置", "附加 词典 挂载"),
    ("辅助码", "方案自定义", "辅码 筛选 反查"),
    ("码表配置", "方案自定义", "覆盖 编码 上屏"),
    ("特殊模式", "方案自定义", "overlay 引导键 快符 生僻字"),
    ("本方案行为", "方案自定义", "标点 候选 短语 跟随全局"),
];

fn schema_level_entries() -> Vec<SearchEntry> {
    SCHEMA_LEVEL
        .iter()
        .map(|(title, tab, kw)| SearchEntry {
            id: format!("schema-level:{title}"),
            title: (*title).to_string(),
            keywords: kw.split_whitespace().map(str::to_string).collect(),
            // 「<选择一个方案>」这一段是留给用户走的，指示条会把它弱化显示。
            path: vec![
                "方案".to_string(),
                "<选择一个方案>".to_string(),
                "方案设置…".to_string(),
                (*tab).to_string(),
            ],
            target: JumpTarget::SchemaLevel {
                hint_path: vec![
                    "方案".to_string(),
                    "<选择一个方案>".to_string(),
                    "方案设置…".to_string(),
                    (*tab).to_string(),
                    (*title).to_string(),
                ],
            },
        })
        .collect()
}

/// 构建完整索引（manifest + 手写 + 方案级）。启动时跑一次，之后常驻。
pub fn build_index(manifest: &Manifest) -> Vec<SearchEntry> {
    let mut out = manifest_entries(manifest);
    out.extend(handwritten_entries());
    out.extend(schema_level_entries());
    out
}

/// 索引里**参与拼音首字母检索**的全部汉字（即所有标题里的字）。
///
/// 供 `tests/pinyin_coverage.rs` 与生成脚本使用：拼音表只需覆盖标题，
/// 不必覆盖 hint——长句的首字母串没有检索价值，只会制造误命中。
#[cfg(test)]
pub fn title_chars(entries: &[SearchEntry]) -> BTreeSet<char> {
    entries
        .iter()
        .flat_map(|e| e.title.chars())
        .filter(|c| is_cjk(*c))
        .collect()
}

/// 是否为需要转拼音的汉字（CJK 基本区 + 扩展 A）。
#[cfg(test)]
pub fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}')
}

/// manifest 里出现过、但 [`placement_of`] 没登记的 (group, section) 组合。
///
/// 空 = 全部有落点。非空即说明新增了分组/分区却没告诉搜索索引，那批项会静默
/// 从搜索里消失——由 `sections_have_placement` 断言。
#[cfg(test)]
pub fn unplaced_sections(manifest: &Manifest) -> Vec<(String, String)> {
    let mut out = BTreeSet::new();
    for key in &manifest.order {
        let Some(item) = manifest.get(key) else {
            continue;
        };
        if item.hidden {
            continue;
        }
        if placement_of(&item.group, &item.section).is_none() {
            out.insert((item.group.clone(), item.section.clone()));
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest::from_embedded()
    }

    /// 每个 (group, section) 都必须有渲染落点，否则那批项搜不到且无报错。
    #[test]
    fn sections_have_placement() {
        let m = manifest();
        let missing = unplaced_sections(&m);
        assert!(
            missing.is_empty(),
            "这些 (group, section) 没在 placement_of 里登记，其设置项将无法被搜索到：{missing:?}"
        );
    }

    /// 索引非空且规模合理（防止派生逻辑整体失效却静默通过）。
    #[test]
    fn index_covers_manifest_items() {
        let m = manifest();
        let idx = build_index(&m);
        assert!(
            idx.len() > 200,
            "索引条目过少（{}），manifest 派生可能整体失效",
            idx.len()
        );
    }

    /// 每条都得有标题与面包屑——空标题在结果列表里是一行看不懂的东西。
    #[test]
    fn entries_have_title_and_path() {
        let m = manifest();
        for e in build_index(&m) {
            assert!(!e.title.is_empty(), "{} 标题为空", e.id);
            assert!(!e.path.is_empty(), "{} 面包屑为空", e.id);
        }
    }

    /// id 唯一：重复 id 会让跳转认错锚点。
    #[test]
    fn entry_ids_are_unique() {
        let m = manifest();
        let idx = build_index(&m);
        let mut seen = BTreeSet::new();
        for e in &idx {
            assert!(seen.insert(e.id.clone()), "重复的索引 id：{}", e.id);
        }
    }

    /// 每个 Tab 都至少有一条索引——整页漏登记时报出。
    #[test]
    fn every_page_has_entries() {
        let m = manifest();
        let idx = build_index(&m);
        for page in [
            "schema", "input", "keys", "ui", "dict", "stats", "advanced", "about",
        ] {
            let tab = crate::cli::page_to_index(page).expect("页 id 应在 PAGES 里");
            assert!(
                idx.iter().any(|e| e.target.tab() == tab),
                "「{page}」页没有任何可搜索条目"
            );
        }
    }

    /// 主码表方案：manifest 里的项应落在方案页、直接成行。
    #[test]
    fn primary_codetable_is_a_page_row() {
        let m = manifest();
        let idx = build_index(&m);
        let e = idx
            .iter()
            .find(|e| e.id == "schema.primary_codetable")
            .expect("应索引到主码表方案");
        assert_eq!(e.title, "主码表方案");
        assert_eq!(
            e.target,
            JumpTarget::Row {
                tab: crate::cli::page_to_index("schema").unwrap(),
                anchor: "schema.primary_codetable".into(),
            }
        );
        assert_eq!(e.path, vec!["方案", "主方案设置"]);
    }

    /// schema 组的「上屏行为」项落在方案页的码表方案配置对话框里，不是页面直出行。
    #[test]
    fn scheme_behavior_items_land_in_dialog() {
        let m = manifest();
        let idx = build_index(&m);
        let e = idx
            .iter()
            .find(|e| {
                m.get(&e.id)
                    .is_some_and(|i| i.group == "schema" && i.section == "上屏行为")
            })
            .expect("上屏行为应有可索引项");
        match &e.target {
            JumpTarget::RowInDialog { dialog, .. } => {
                assert_eq!(dialog, "scheme_cfg:码表方案配置");
            }
            other => panic!("上屏行为应落对话框，实际 {other:?}"),
        }
        assert_eq!(e.path[1], "全局方案配置");
    }

    /// general 组渲染在输入页，面包屑与 Tab 都该是「输入」。
    #[test]
    fn general_group_lands_on_input_page() {
        let m = manifest();
        let idx = build_index(&m);
        let e = idx
            .iter()
            .find(|e| m.get(&e.id).is_some_and(|i| i.group == "general"))
            .expect("general 组应有可索引项");
        assert_eq!(e.target.tab(), crate::cli::page_to_index("input").unwrap());
        assert_eq!(e.path[0], "输入");
    }

    /// 方案级条目不生成跳转，只带指示路径。
    #[test]
    fn schema_level_entries_are_hint_only() {
        let idx = schema_level_entries();
        assert_eq!(idx.len(), SCHEMA_LEVEL.len());
        for e in idx {
            match e.target {
                JumpTarget::SchemaLevel { hint_path } => {
                    assert!(hint_path.len() >= 4, "指示路径太短：{hint_path:?}");
                    assert_eq!(hint_path[1], "<选择一个方案>");
                }
                other => panic!("方案级条目不该有直达目标：{other:?}"),
            }
        }
    }

    /// 转储标题语料，供 `scripts/gen-pinyin-initials.ps1` 生成拼音表。
    ///
    /// 走测试而不是让脚本正则扫源文件：语料来自三处（manifest 的 label、手写表的
    /// title、方案级分节名），正则扫 Rust 源既脆弱又会漏，而这里输出的就是索引
    /// 真正用的那批字，与守门测试同源。
    #[test]
    #[ignore = "生成脚本调用：cargo test dump_title_chars -- --ignored --nocapture"]
    fn dump_title_chars() {
        let chars = title_chars(&build_index(&manifest()));
        println!("<<<TITLE_CHARS>>>");
        println!("{}", chars.iter().collect::<String>());
        println!("<<<END>>>");
    }

    /// 标题语料只含汉字，且规模在预期量级（拼音表据此生成）。
    #[test]
    fn title_chars_are_reasonable() {
        let m = manifest();
        let chars = title_chars(&build_index(&m));
        assert!(
            (200..2000).contains(&chars.len()),
            "标题汉字数 {} 超出预期量级，拼音表可能需要重新生成",
            chars.len()
        );
        assert!(chars.iter().all(|c| is_cjk(*c)));
    }
}
