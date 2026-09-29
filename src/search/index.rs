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
//! - `schema` 组的五个 section 分散在方案页的五张卡片上：「主方案设置」一张，
//!   其余四节各成一张引擎卡（`manifest::scheme_config_section`）。
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
    /// 打开词库窗。
    ///
    /// 词库**不是**主窗的一页：它早已迁进独立窗口，主窗那一格只剩个永不显示的占位
    /// （`pages::dict_page` 返回空叶子）。所以词库相关的条目不能切页——切过去是一屏
    /// 空白——只能请求开窗，主窗停在原页不动（与侧栏那个「词库」动作项同一语义）。
    DictWindow,
}

impl JumpTarget {
    /// 目标所在 Tab（`SchemaLevel` 恒为方案页）。
    pub fn tab(&self) -> usize {
        match self {
            Self::Row { tab, .. } | Self::RowInDialog { tab, .. } | Self::Card { tab, .. } => *tab,
            Self::SchemaLevel { .. } => page_tab("schema"),
            // 归属上算词库页（`every_page_has_entries` 据此计数），但**不会**被切过去：
            // `jump::request` 对本变体不动 tab。
            Self::DictWindow => page_tab("dict"),
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
/// 用 0 兜底并由 `every_page_has_entries` 守门（那条测试对每个页 id 都做了
/// `page_to_index(..).expect(..)`，写错的 id 在那里当场炸）。
fn page_tab(page: &str) -> usize {
    crate::cli::page_to_index(page).unwrap_or(0)
}

/// 一个 (group, section) 的渲染落点。
enum Placement {
    /// 页面上直接成行。
    Page(&'static str),
    /// 方案页「全局方案配置」下那四张引擎卡片之一。
    SchemeCard {
        /// 卡片标题（如「码表方案配置」）。
        label: &'static str,
    },
}

/// 对话框内那一行的锚点后缀。
///
/// 同一个清单项可能**两处都有行**：`dialog_title` 宿主在卡片上留一行
/// 「<标题> …… [设置]」，它自己的控件又在对话框里。两处锚点若用同一个键，
/// 第一跳（滚到入口行）之后第二跳会被卡片上那一行再次认领——框是开了，
/// 闪的却还是外面那一行。
pub const IN_DIALOG_SUFFIX: &str = "#in-dialog";

/// 对话框内那一行的锚点键。
///
/// 渲染成**一整块**、没有单独一行可落的项（见 [`block_anchor_of`]）统一落到块锚点上。
pub fn in_dialog_anchor(key: &str) -> String {
    match block_anchor_of(key) {
        Some(block) => format!("{block}{IN_DIALOG_SUFFIX}"),
        None => format!("{key}{IN_DIALOG_SUFFIX}"),
    }
}

/// 页面上直接成行的项的锚点键。
///
/// 通常就是 key 本身；渲染成**一整块**、没有单独一行可落的项（见 [`block_anchor_of`]）
/// 统一落到块锚点上。[`in_dialog_anchor`] 的页面版，两者只差那个后缀。
pub fn page_anchor(key: &str) -> String {
    block_anchor_of(key).unwrap_or(key).to_string()
}

/// 「这一项没有自己的行，它在某一整块里」——返回那一块的锚点基名。
///
/// 模糊音那一组开关是挤在一个**网格**里的（`manifest::build_fuzzy_group`），
/// 每格只有一个 `zh ↔ z` 大小的方块，逐格挂锚点没有意义：闪一个小方块，
/// 用户多半看不见。整块闪一次反而说得清「在这儿」。
fn block_anchor_of(key: &str) -> Option<&'static str> {
    key.starts_with("schema.pinyin.fuzzy.")
        .then_some("schema.pinyin.fuzzy")
}

/// (group, section) → 落点。返回 `None` = 未登记，由守门测试报出。
fn placement_of(group: &str, section: &str) -> Option<Placement> {
    let p = match (group, section) {
        // ── 方案组：只有主方案设置直接成行 ──
        ("schema", "主方案设置") => Placement::Page("schema"),
        // 其余四节铺在「全局方案配置」下的四张引擎卡片上。「模糊音」由
        // `scheme_config_section(.., with_fuzzy=true)` 并进拼音方案配置。
        ("schema", "上屏行为") => Placement::SchemeCard {
            label: "码表方案配置",
        },
        ("schema", "混输行为") => Placement::SchemeCard {
            label: "混输方案配置",
        },
        ("schema", "候选行为") | ("schema", "模糊音") => Placement::SchemeCard {
            label: "拼音方案配置",
        },
        ("schema", "英文行为") => Placement::SchemeCard {
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
            .or_else(|| dialog_group_host(manifest, item))
            // `dialog_title` 宿主：卡片上只留一行「<标题> …… [设置]」，**它自己的控件
            // 也在框里**（见 `manifest::build_grouped_dialog_row`）。所以它是自己的宿主，
            // 目标该是 RowInDialog——当成 Row 的话，跳过去只滚到入口行就停了，
            // 而寄居在同一个框里的兄弟项却会开框，两者层级不一致。
            .or_else(|| item.dialog_title.as_ref().map(|_| key.clone()));
        let (target, mut path) = match placement {
            Placement::Page(page) => {
                let tab = page_tab(page);
                match &host {
                    Some(h) => (
                        JumpTarget::RowInDialog {
                            tab,
                            // 入口行的锚点未必等于宿主项自己的 key：宿主若是个被开关
                            // 合并掉的 `dialog_button`（`gate_key`），卡片上那一行属于
                            // **开关**，锚点也就是开关的 key。指错了第一跳无人认领，
                            // 框永远不会开。
                            host_anchor: row_anchor_of(manifest, h),
                            // 对话框注册表的键仍是宿主项自己的 key。
                            dialog: h.clone(),
                            anchor: in_dialog_anchor(key),
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
            Placement::SchemeCard { label } => (
                // 这四节 2026-09 起铺在方案页上（原先各在一个对话框里），故是普通的
                // 页面行：一跳到位，不再需要「先滚到入口行、开框、再滚框内那行」。
                JumpTarget::Row {
                    tab: page_tab("schema"),
                    anchor: page_anchor(key),
                },
                // 面包屑跟着界面走：「全局方案配置」那一层已经撤掉（见
                // `pages::scheme_page`），路径里再留着它就会指向一个界面上不存在的分组。
                vec![page_name.to_string(), label.to_string()],
            ),
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

/// 宿主项在**卡片上那一行**的锚点键。
///
/// 通常就是它自己；但被 `gate_key` 合并进开关行的 `dialog_button` 例外——那一行是
/// 开关的，锚点键也就是开关的 key（见 `manifest::section_rows` 的合并分支）。
fn row_anchor_of(manifest: &Manifest, host_key: &str) -> String {
    match manifest.get(host_key) {
        Some(it) if is_merged_into_gate(it, manifest) => {
            it.gate_key.clone().unwrap_or_else(|| host_key.to_string())
        }
        _ => host_key.to_string(),
    }
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
    /// 索引里的唯一标识（英文 slug）。
    id: &'static str,
    /// 界面上那块地方的锚点键。
    ///
    /// 与 `id` 分开是因为**多个条目常落在同一张卡片上**：方案页那五条（列表、方案设置、
    /// 导入、导出、网址导入）都在「输入方案」卡里，各挂一个锚点没有意义，也不可能——
    /// 一个节点只有一个锚点。
    anchor: &'static str,
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
    // ── 方案页：五条都落在「输入方案」卡上 ──
    Handwritten { id: "schema:list", anchor: "schema:list", page: "schema", title: "方案列表", section: "输入方案", keywords: "切换 启用 排序 拖动" },
    Handwritten { id: "schema:settings", anchor: "schema:list", page: "schema", title: "方案设置", section: "输入方案", keywords: "单个方案 配置 编辑" },
    Handwritten { id: "schema:import", anchor: "schema:list", page: "schema", title: "导入方案包", section: "输入方案", keywords: "wpkg 安装 添加" },
    Handwritten { id: "schema:export", anchor: "schema:list", page: "schema", title: "导出方案包", section: "输入方案", keywords: "wpkg 备份 分享" },
    Handwritten { id: "schema:url-import", anchor: "schema:list", page: "schema", title: "从网址导入方案", section: "输入方案", keywords: "url 在线 下载" },
    // ── 词库：走开窗，锚点用不上（见 JumpTarget::DictWindow）──
    Handwritten { id: "dict:manage", anchor: "dict:manage", page: "dict", title: "词库管理", section: "词库", keywords: "词条 编辑 删除 查找" },
    Handwritten { id: "dict:add-word", anchor: "dict:manage", page: "dict", title: "添加词条", section: "词库", keywords: "造词 新增 自定义" },
    Handwritten { id: "dict:import", anchor: "dict:manage", page: "dict", title: "导入词库", section: "词库", keywords: "文本 合并 覆盖" },
    Handwritten { id: "dict:phrase-import", anchor: "dict:manage", page: "dict", title: "短语导入", section: "词库", keywords: "phrase 批量" },
    Handwritten { id: "dict:comment", anchor: "dict:manage", page: "dict", title: "注释词库", section: "词库", keywords: "comment 提示 释义" },
    // ── 统计 ──
    Handwritten { id: "stats:overview", anchor: "stats:overview", page: "stats", title: "使用统计", section: "统计", keywords: "字数 击键 效率 图表" },
    Handwritten { id: "stats:clear", anchor: "stats:settings", page: "stats", title: "清空统计数据", section: "统计设置", keywords: "重置 删除 归零" },
    // ── 外观 ──
    Handwritten { id: "ui:theme-import", anchor: "ui:theme-import", page: "ui", title: "导入主题", section: "主题", keywords: "皮肤 toml 网址 安装" },
    Handwritten { id: "ui:theme-export", anchor: "ui:theme-import", page: "ui", title: "导出主题", section: "主题", keywords: "皮肤 wtheme 主题包 分享 打包 备份" },
    Handwritten { id: "ui:theme-editor", anchor: "ui:theme-editor", page: "ui", title: "主题编辑器连接", section: "主题编辑器", keywords: "皮肤 在线 编辑 服务 端口" },
    // ── 应用兼容：规则本身的字段来自核心的元数据、运行时才知道，索引只收页级入口 ──
    Handwritten { id: "compat:main", anchor: "advanced:compat", page: "advanced", title: "应用兼容", section: "应用兼容", keywords: "兼容性 软件 进程 候选窗 位置 飞书 微信 excel wps 规则 compat" },
    Handwritten { id: "compat:add", anchor: "advanced:compat", page: "advanced", title: "新增应用规则", section: "应用兼容", keywords: "添加 进程名 exe 手动" },
    Handwritten { id: "compat:import", anchor: "advanced:compat", page: "advanced", title: "导入应用兼容规则", section: "应用兼容", keywords: "compat.toml 分享 合并 替换" },
    Handwritten { id: "compat:export", anchor: "advanced:compat", page: "advanced", title: "导出应用兼容规则", section: "应用兼容", keywords: "compat.toml 备份 分享" },
    Handwritten { id: "compat:reset", anchor: "advanced:compat", page: "advanced", title: "还原应用兼容规则", section: "应用兼容", keywords: "出厂 默认 恢复 全部还原" },
    // ── 高级 ──
    Handwritten { id: "advanced:integration", anchor: "advanced:integration", page: "advanced", title: "桌面集成", section: "桌面集成", keywords: "协议 关联 注册" },
    Handwritten { id: "advanced:protocol", anchor: "advanced:integration", page: "advanced", title: "协议关联", section: "桌面集成", keywords: "windinput 链接 深链" },
    Handwritten { id: "advanced:file-assoc", anchor: "advanced:integration", page: "advanced", title: "文件关联", section: "桌面集成", keywords: "wpkg 双击 打开方式" },
    Handwritten { id: "advanced:import-export", anchor: "advanced:import-export", page: "advanced", title: "导入与导出", section: "导入与导出", keywords: "配置 迁移 剪贴板" },
    Handwritten { id: "advanced:backup", anchor: "advanced:backup", page: "advanced", title: "备份与还原", section: "备份与还原", keywords: "快照 恢复 存档" },
    // ── 关于（内容不多，三条共用整页锚点）──
    Handwritten { id: "about:version", anchor: "about:main", page: "about", title: "版本信息", section: "关于", keywords: "版本号 内部版本 构建" },
    Handwritten { id: "about:update", anchor: "about:main", page: "about", title: "检查更新", section: "关于", keywords: "升级 新版本 下载" },
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
            // 词库条目走开窗而不是切页（见 `JumpTarget::DictWindow`）。
            target: if h.page == "dict" {
                JumpTarget::DictWindow
            } else {
                JumpTarget::Card {
                    tab: page_tab(h.page),
                    anchor: h.anchor.to_string(),
                }
            },
        })
        .collect()
}

/// 方案设置对话框里的条目（per-schema）。
///
/// 这些设置**每个方案各有一份**，「哪个方案」在搜索时未定，故一律落
/// [`JumpTarget::SchemaLevel`]：跳到方案页 + 指示条，由用户自己选方案。
///
/// 收的是**分节 + 分节里的具体字段**。只收分节名不够——用户记得的是「横排时文字排列」
/// 这样的字段名，不是它归在哪一节；搜不到就会以为没有这个设置。
///
/// ⚠ **编译器不管这张表**，同 [`HANDWRITTEN`]：它对着的是
/// `dialogs::schema_manager::build_section` 里手写的那些行，那边加了字段、这边不加，
/// 新字段就永远搜不到。`SettingsSection::ALL` 也是同类的手写列表。
struct SchemaLevelItem {
    title: &'static str,
    /// 所属 TAB，取值须在 `schema_manager::SETTINGS_TABS` 里。
    tab: &'static str,
    /// 所属分节（面包屑末段兼指示条落点）。
    section: &'static str,
    keywords: &'static str,
}

#[rustfmt::skip]
const SCHEMA_LEVEL: &[SchemaLevelItem] = &[
    // ── 基本配置 ──
    SchemaLevelItem { title: "双拼布局", tab: "基本配置", section: "方案参数", keywords: "自然码 小鹤 微软 键位" },
    SchemaLevelItem { title: "扩展词库", tab: "基本配置", section: "扩展词库", keywords: "附加 词典 挂载 启用" },
    // ── 按键设置 ──
    SchemaLevelItem { title: "进入方式", tab: "按键设置", section: "进入方式", keywords: "直达 热键 切换 快捷键" },
    SchemaLevelItem { title: "切换到本方案按键", tab: "按键设置", section: "进入方式", keywords: "热键 直达 绑定" },
    // ── 方案自定义 ──
    SchemaLevelItem { title: "辅助码", tab: "方案自定义", section: "辅助码", keywords: "辅码 筛选 反查" },
    SchemaLevelItem { title: "本方案启用辅助码", tab: "方案自定义", section: "辅助码", keywords: "辅码 覆盖 跟随全局" },
    SchemaLevelItem { title: "本方案直接辅助码", tab: "方案自定义", section: "辅助码", keywords: "辅码 双拼 引导键 跟随全局" },
    SchemaLevelItem { title: "辅助码来源", tab: "方案自定义", section: "辅助码", keywords: "辅码 码表 笔画 五笔 小鹤 自然码 跟随方案" },
    SchemaLevelItem { title: "码表配置", tab: "方案自定义", section: "码表配置", keywords: "覆盖 编码 上屏 恢复跟随" },
    SchemaLevelItem { title: "特殊模式", tab: "方案自定义", section: "特殊模式", keywords: "overlay 引导键 快符 生僻字 临时" },
    SchemaLevelItem { title: "进入即展示候选", tab: "方案自定义", section: "特殊模式", keywords: "overlay 引导键 首屏" },
    SchemaLevelItem { title: "本方案行为", tab: "方案自定义", section: "本方案行为", keywords: "标点 候选 短语 跟随全局" },
    SchemaLevelItem { title: "标点", tab: "方案自定义", section: "本方案行为", keywords: "中文标点 英文标点 跟随全局" },
    SchemaLevelItem { title: "候选排列", tab: "方案自定义", section: "本方案行为", keywords: "横排 竖排 方向 跟随全局" },
    SchemaLevelItem { title: "横排时文字排列", tab: "方案自定义", section: "本方案行为", keywords: "横排 旋转 竖排 蒙古文 text_orientation" },
    SchemaLevelItem { title: "候选文字字体", tab: "方案自定义", section: "本方案行为", keywords: "字体 字形 跟随外观" },
    SchemaLevelItem { title: "加载快捷短语", tab: "方案自定义", section: "本方案行为", keywords: "短语 快捷短语 phrases 开关" },
    SchemaLevelItem { title: "候选注释", tab: "方案自定义", section: "本方案行为", keywords: "注释 comment 释义" },
    SchemaLevelItem { title: "自定义标点", tab: "方案自定义", section: "本方案行为", keywords: "标点 映射 符号" },
];

fn schema_level_entries() -> Vec<SearchEntry> {
    SCHEMA_LEVEL
        .iter()
        .map(|it| SearchEntry {
            id: format!("schema-level:{}:{}", it.section, it.title),
            title: it.title.to_string(),
            keywords: it.keywords.split_whitespace().map(str::to_string).collect(),
            // 「<选择一个方案>」这一段是留给用户走的。
            path: vec![
                "方案".to_string(),
                "<选择一个方案>".to_string(),
                "方案设置…".to_string(),
                it.tab.to_string(),
                it.section.to_string(),
            ],
            target: JumpTarget::SchemaLevel {
                hint_path: vec![
                    "方案".to_string(),
                    "<选择一个方案>".to_string(),
                    "方案设置…".to_string(),
                    it.tab.to_string(),
                    it.title.to_string(),
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
/// 供 `pinyin::tests::table_covers_every_title_char` 与生成脚本使用：拼音表只需覆盖
/// 标题，不必覆盖 hint——长句的首字母串没有检索价值，只会制造误命中。
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

    /// schema 组的「上屏行为」项落在方案页「码表方案配置」那张卡上，一跳到位。
    ///
    /// 这几节曾经收在对话框里（目标是 `RowInDialog`），2026-09 铺开成页内卡片后必须是
    /// `Row`：仍按对话框生成的话，跳转会去找一个已经不存在的入口行与对话框注册项，
    /// 第一跳无人认领 —— 搜到了、点了、页也切了，就是不滚也不闪。
    #[test]
    fn scheme_behavior_items_land_on_page() {
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
            JumpTarget::Row { tab, anchor } => {
                assert_eq!(*tab, crate::cli::page_to_index("schema").unwrap());
                assert_eq!(anchor, &e.id, "锚点键就是清单 key");
            }
            other => panic!("上屏行为应落页面行，实际 {other:?}"),
        }
        assert_eq!(e.path[1], "码表方案配置", "面包屑第二段就是卡片标题");
    }

    /// 模糊音那 13 个开关挤在一个网格里，索引须落到**整块**的锚点上而不是各自的 key
    /// （逐格闪一个 `zh ↔ z` 大小的方块没人看得见）。铺开成页内卡片后不再带
    /// `#in-dialog` 后缀，故这里把两件事一起钉住。
    #[test]
    fn fuzzy_items_use_block_anchor_without_dialog_suffix() {
        let m = manifest();
        let idx = build_index(&m);
        let e = idx
            .iter()
            .find(|e| e.id.starts_with("schema.pinyin.fuzzy."))
            .expect("模糊音应有可索引项");
        match &e.target {
            JumpTarget::Row { anchor, .. } => assert_eq!(anchor, "schema.pinyin.fuzzy"),
            other => panic!("模糊音应落页面块锚点，实际 {other:?}"),
        }
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
