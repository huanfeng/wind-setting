//! 设置页 — 清单驱动（manifest 的 group → Tab、section → 卡片、item → 行）+ 自定义页。
//!
//! 主体一律由 `system.manifest` 的 `(group, section)` 聚合渲染：同一 section 的项归到
//! 一张卡片，卡片次序为 section 首次出现序，行序为 manifest 原始序——key 永远与 core 一致，
//! core 加字段自动出现，彻底消除硬编码 key 漂移。复杂页（方案管理/词库/统计/关于）手写。

use windui::core::EventCtx;
use windui::prelude::*;

pub mod dict;

use crate::dialogs::schema_manager::build_schema_inline_list;
use crate::manifest::{build_scheme_config_button, section_rows};
use crate::state::AppState;
use crate::widgets::{page, section, setting_row, todo_hint};

/// Tab：(标题, 内容)。manifest 分组负责主体，自定义页补充方案管理/词库/统计/关于。
pub fn all_tabs(state: &AppState) -> Vec<(String, Element)> {
    vec![
        ("方案".into(), scheme_page(state)),
        ("输入".into(), input_page(state)),
        ("按键".into(), keys_page(state)),
        ("外观".into(), appearance_page(state)),
        ("词库".into(), dict_page(state)),
        // 统计页未实现，暂从导航隐藏（功能完成后恢复）。
        ("高级".into(), manifest_page(state, "advanced", "高级", "服务与实验性功能")),
        ("关于".into(), about_page(state)),
    ]
}

/// 由若干 section 卡片组成一页（页头 + 纵向滚动，深色 bg 衬托卡片）。
fn sections_page(title: &str, desc: &str, sections: Vec<Element>) -> Element {
    let mut col = Element::col().width_match().spacing(18);
    if crate::widgets::PAGE_HEADER_ENABLED {
        col = col.child(crate::widgets::page_header(title, desc));
    }
    for s in sections {
        col = col.child(s);
    }
    page(col)
}

/// 由 manifest 某分组生成若干 section 卡片（每 section 一张卡片，行按 manifest 顺序）。
fn manifest_sections(state: &AppState, group: &str) -> Vec<Element> {
    state
        .manifest
        .sections_of(group)
        .into_iter()
        .map(|(sec, keys)| section(&sec, section_rows(state, &keys)))
        .collect()
}

/// 纯清单分组页（无自定义卡片）。
fn manifest_page(state: &AppState, group: &str, title: &str, desc: &str) -> Element {
    let mut secs = manifest_sections(state, group);
    if secs.is_empty() {
        secs.push(
            Element::col()
                .width_match()
                .child(todo_hint("（该分组暂无清单项，或 core 未运行）")),
        );
    }
    sections_page(title, desc, secs)
}

// ───────────────────────── 输入 ─────────────────────────

/// 输入页：manifest「input」分组 + 并入原「常规(general)」的启动状态等分区
/// （已取消独立「常规」Tab；general 分组若空则无副作用）。
fn input_page(state: &AppState) -> Element {
    let mut secs = manifest_sections(state, "input");
    secs.extend(manifest_sections(state, "general"));
    sections_page("输入", "调整输入行为与纠错策略", secs)
}

// ───────────────────────── 方案 ─────────────────────────

fn scheme_page(state: &AppState) -> Element {
    let mgr = &state.schema_mgr;
    let status = mgr.status;

    // 工具栏：左侧「隐藏未启用」· 中间状态文字 · 右侧 刷新/导入
    // padding_xy(0,4) 给按钮 outline 边框与下方 divider 留出间距
    let st_refresh = mgr.clone();
    let st_import  = mgr.clone();
    let toolbar = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(0, 4)
        .spacing(8)
        .child(
            // fg_role(Text) 确保暗色模式下标签文字跟随主题色
            Element::checkbox("隐藏未启用", mgr.hide_disabled)
                .small()
                .fg_role(Role::Text),
        )
        .child(
            Element::label_rc(status)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .text_align(Align::Center)
                .weight(1.0)
                .max_lines(1),
        )
        .child(
            Element::button("刷新")
                .small()
                .outline_soft()
                .on_click(move |_| st_refresh.refresh()),
        )
        .child(
            Element::button("导入…")
                .small()
                .outline_soft()
                .on_click(move |_| st_import.status.set("导入功能开发中".into())),
        );

    // 复合行列表（已启用在前 · 未启用在后 · ↑/↓ · outline 类型徽章）
    let list_el = build_schema_inline_list(mgr);

    let mut secs = vec![section("输入方案", vec![toolbar, list_el])];

    // 主方案设置（manifest schema 分组中的"主方案设置" section）。
    if let Some((_, keys)) = state
        .manifest
        .sections_of("schema")
        .into_iter()
        .find(|(s, _)| s == "主方案设置")
    {
        secs.push(section("主方案设置", section_rows(state, &keys)));
    }

    // 全局方案配置：码表 / 混输 / 拼音 三个 [设置] 对话框。
    let codetable = build_scheme_config_button(state, "码表方案配置", "上屏行为", false);
    let mixed = build_scheme_config_button(state, "混输方案配置", "混输行为", false);
    let pinyin = build_scheme_config_button(state, "拼音方案配置", "候选行为", true);
    secs.push(section(
        "全局方案配置",
        vec![
            setting_row("码表方案配置", Some("码表引擎的全局默认配置"), codetable),
            setting_row("混输方案配置", Some("混输引擎的全局默认配置"), mixed),
            setting_row("拼音方案配置", Some("拼音引擎的全局默认配置"), pinyin),
        ],
    ));

    sections_page("方案", "选择输入方案与引擎配置", secs)
}

// ───────────────────────── 按键 ─────────────────────────

fn keys_page(state: &AppState) -> Element {
    sections_page("按键", "自定义快捷键与候选选择键", manifest_sections(state, "keys"))
}

// ───────────────────────── 外观 ─────────────────────────

fn appearance_page(state: &AppState) -> Element {
    // 主题选择已并入 manifest ui「主题」卡（ui.theme.name，options_source=themes，
    // 下拉显示主题名、保存 slug；登记保存/加载/恢复）。整页纯清单驱动。
    sections_page("外观", "自定义候选窗与工具栏的视觉呈现", manifest_sections(state, "ui"))
}

// ───────────────────────── 词库 ─────────────────────────

fn dict_page(state: &AppState) -> Element {
    dict::build_page(state)
}

// ───────────────────────── 关于 ─────────────────────────

/// 仓库地址（issues/releases 在其后拼段）。与 Vue 版 `App.vue` 的 repoUrl 一致。
const REPO_URL: &str = "https://github.com/huanfeng/WindInput";
/// 官网。
const WEBSITE_URL: &str = "https://windinput.com";
/// QQ 交流群号。
const QQ_GROUP: &str = "1085293418";

// 品牌图标 SVG（白色填充，24×24 viewBox），从 res/icons 嵌入。图标数据取自 Vue 版
// AboutPage，保证与网页版逐像素一致；改图标只改 .svg 文件即可，无需动代码。
const ICON_WEBSITE: &str = include_str!("../../res/icons/website.svg");
const ICON_GITHUB: &str = include_str!("../../res/icons/github.svg");
const ICON_ISSUES: &str = include_str!("../../res/icons/issues.svg");
const ICON_RELEASES: &str = include_str!("../../res/icons/releases.svg");
const ICON_QQ: &str = include_str!("../../res/icons/qq.svg");

/// 域名 chip（官网横幅右侧）：淡蓝底 + 蓝字 + pill 圆角，**普通字重**
/// （区别于 `badge` 的 600 加粗）。蓝色与官网图标方块同源，观感协调。
fn url_chip(text: &str) -> Element {
    let blue = Color::hex(0x3B82F6);
    Element::row()
        .cross(Align::Center)
        .padding_xy(9, 3)
        .corner(999.0)
        .bg(blue.scale_alpha(0.14))
        .child(Element::label(text).font_size(12.0).fg(blue))
}

/// 彩色圆角图标方块 + 居中白色 SVG 图标（品牌色固定、不随主题；SVG 走 `svg` feature）。
/// `target_width` 取 2× 显示像素求 HiDPI 清晰度。
fn about_icon_box(bg: Color, svg: &str, size: i32) -> Element {
    let icon = (size as f32 * 0.56) as i32;
    Element::stack().size(size, size).corner(10.0).bg(bg).child(
        Element::image_svg(svg.as_bytes(), Some((icon * 2) as u32))
            .size(icon, icon)
            .align(Align::Center),
    )
}

/// 可点击链接卡片：品牌图标 + 标题 + 描述，点击执行 `action`。
/// 表面/文字走角色化主题（明暗自适应），仅图标方块用品牌固定色。
/// `trailing` 为右侧可选挂件（如官网横幅的域名 chip）。
fn about_link_card(
    icon_bg: Color,
    svg_path: &str,
    title: &str,
    desc: &str,
    trailing: Option<Element>,
    action: impl FnMut(&mut EventCtx) + 'static,
) -> Element {
    let mut row = Element::row()
        .clickable()
        .on_click(action)
        .width_match()
        .cross(Align::Center)
        .spacing(12)
        .padding(14)
        .corner(12.0)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .child(about_icon_box(icon_bg, svg_path, 36))
        .child(
            Element::col()
                .weight(1.0)
                .spacing(2)
                .child(
                    Element::label(title)
                        .font_size(14.0)
                        .font_weight(600)
                        .fg_role(Role::Text)
                        .width_match()
                        .max_lines(1)
                        .truncate(Truncate::End)
                        .height(19),
                )
                .child(
                    Element::label(desc)
                        .font_size(12.0)
                        .fg_role(Role::TextMuted)
                        .width_match()
                        .max_lines(1)
                        .truncate(Truncate::End)
                        .height(16),
                ),
        );
    if let Some(t) = trailing {
        row = row.child(t);
    }
    row
}

/// 关于页 — 复刻品牌关于卡（对齐截图 / Vue AboutPage）：
/// 头部（图标+名称+版本徽章+检查更新）· 官网横幅 · 2×2 链接网格 · 版权页脚。
fn about_page(state: &AppState) -> Element {
    let version = state
        .system_info
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let version = if version.is_empty() {
        env!("CARGO_PKG_VERSION").to_string()
    } else {
        version
    };
    let ver_badge = format!("v{version}{}", crate::mode::version_suffix());

    // 头部：大图标 + 名称 + 版本徽章 / 检查更新 + 副标题。
    let header = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(18)
        .child(crate::widgets::app_icon(72))
        .child(
            Element::col()
                .weight(1.0)
                .spacing(8)
                .child(
                    Element::label("清风输入法")
                        .font_size(22.0)
                        .font_weight(700)
                        .fg_role(Role::Text)
                        .width_match()
                        .max_lines(1)
                        .height(30),
                )
                .child(
                    Element::row()
                        .cross(Align::Center)
                        .spacing(10)
                        .child(Element::badge(ver_badge))
                        .child(
                            Element::button("检查更新")
                                .small()
                                .outline_soft()
                                .on_click(|ctx| ctx.toast("检查更新功能开发中")),
                        ),
                )
                .child(
                    Element::label("轻量、快速、可定制的开源中文输入法")
                        .font_size(13.0)
                        .fg_role(Role::TextMuted)
                        .width_match()
                        .max_lines(1)
                        .height(19),
                ),
        );

    // 官网横幅：整行可点击 + 右侧域名 chip。
    let banner = about_link_card(
        Color::hex(0x3B82F6),
        ICON_WEBSITE,
        "官方网站",
        "文档、下载与最新动态",
        Some(url_chip("windinput.com")),
        |ctx| ctx.open_url(WEBSITE_URL),
    );

    // 2×2 链接网格：GitHub / 问题反馈 / 版本发布 / QQ 交流群。
    let github = about_link_card(
        Color::hex(0x24292F),
        ICON_GITHUB,
        "GitHub",
        "源码与文档",
        None,
        |ctx| ctx.open_url(REPO_URL),
    );
    let issues = about_link_card(
        Color::hex(0xF59E0B),
        ICON_ISSUES,
        "问题反馈",
        "报告 Bug 或建议",
        None,
        |ctx| ctx.open_url(&format!("{REPO_URL}/issues")),
    );
    let releases = about_link_card(
        Color::hex(0x22C55E),
        ICON_RELEASES,
        "版本发布",
        "更新日志",
        None,
        |ctx| ctx.open_url(&format!("{REPO_URL}/releases")),
    );
    // QQ 卡：点击复制群号 + toast 反馈（脱离布局树，无需额外接线）。
    let qq = about_link_card(
        Color::hex(0x12B7F5),
        ICON_QQ,
        "QQ 交流群",
        QQ_GROUP,
        None,
        |ctx| {
            ctx.clipboard_set(QQ_GROUP);
            ctx.toast_ok(format!("已复制群号 {QQ_GROUP}"));
        },
    );

    // 每行两列等宽（stack + weight 撑满半宽，对齐示例网格）。
    let half = |el: Element| Element::stack().weight(1.0).child(el);
    let grid = Element::col()
        .width_match()
        .spacing(10)
        .child(
            Element::row()
                .width_match()
                .spacing(10)
                .cross(Align::Stretch)
                .child(half(github))
                .child(half(issues)),
        )
        .child(
            Element::row()
                .width_match()
                .spacing(10)
                .cross(Align::Stretch)
                .child(half(releases))
                .child(half(qq)),
        );

    let footer = Element::label("© 2026 WindInput Contributors · MIT License")
    .font_size(12.0)
    .fg_role(Role::TextMuted)
    .width_match()
    .max_lines(1)
    .height(18)
    .text_align(Align::Center);

    // 外层品牌卡：承载全部内容，浮于内容区 SurfaceAlt 之上。
    let panel = crate::widgets::flat_panel()
        .corner(16.0)
        .width_match()
        .padding(24)
        .spacing(20)
        .child(header)
        .child(banner)
        .child(grid)
        .child(footer);

    let mut body = Element::col().width_match().spacing(18);
    if crate::widgets::PAGE_HEADER_ENABLED {
        body = body.child(crate::widgets::page_header("关于", "版本信息与支持"));
    }
    crate::widgets::page(body.child(panel))
}
