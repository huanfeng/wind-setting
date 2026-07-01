//! 设置页 — 清单驱动（manifest 的 group → Tab、section → 卡片、item → 行）+ 自定义页。
//!
//! 主体一律由 `system.manifest` 的 `(group, section)` 聚合渲染：同一 section 的项归到
//! 一张卡片，卡片次序为 section 首次出现序，行序为 manifest 原始序——key 永远与 core 一致，
//! core 加字段自动出现，彻底消除硬编码 key 漂移。复杂页（方案管理/词库/统计/关于）手写。

use windui::prelude::*;

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
        ("高级".into(), manifest_page(state, "advanced")),
        ("关于".into(), about_page(state)),
    ]
}

/// 由若干 section 卡片组成一页（纵向滚动 + 深色 bg 衬托卡片）。
fn sections_page(sections: Vec<Element>) -> Element {
    let mut col = Element::col().width_match().spacing(18);
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
fn manifest_page(state: &AppState, group: &str) -> Element {
    let mut secs = manifest_sections(state, group);
    if secs.is_empty() {
        secs.push(
            Element::col()
                .width_match()
                .child(todo_hint("（该分组暂无清单项，或 core 未运行）")),
        );
    }
    sections_page(secs)
}

// ───────────────────────── 输入 ─────────────────────────

/// 输入页：manifest「input」分组 + 并入原「常规(general)」的启动状态等分区
/// （已取消独立「常规」Tab；general 分组若空则无副作用）。
fn input_page(state: &AppState) -> Element {
    let mut secs = manifest_sections(state, "input");
    secs.extend(manifest_sections(state, "general"));
    sections_page(secs)
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
                .outline()
                .on_click(move |_| st_refresh.refresh()),
        )
        .child(
            Element::button("导入…")
                .small()
                .outline()
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

    sections_page(secs)
}

// ───────────────────────── 按键 ─────────────────────────

fn keys_page(state: &AppState) -> Element {
    sections_page(manifest_sections(state, "keys"))
}

// ───────────────────────── 外观 ─────────────────────────

fn appearance_page(state: &AppState) -> Element {
    // 主题选择已并入 manifest ui「主题」卡（ui.theme.name，options_source=themes，
    // 下拉显示主题名、保存 slug；登记保存/加载/恢复）。整页纯清单驱动。
    sections_page(manifest_sections(state, "ui"))
}

// ───────────────────────── 词库 ─────────────────────────

fn dict_page(_state: &AppState) -> Element {
    sections_page(vec![section(
        "词库管理",
        vec![todo_hint(
            "词库管理本期占位，后续作为独立功能实现。\
             缺口：wind-ui-rust 暂无「可编辑/可搜索表格」控件；\
             core 侧 dict.* RPC 已就绪（search/add/remove/update/import/export）。",
        )],
    )])
}

// ───────────────────────── 关于 ─────────────────────────

fn about_page(state: &AppState) -> Element {
    let info = &state.system_info;
    let get = |k: &str| {
        info.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let version = get("version");

    // 头部：应用图标 + 名称 + 版本（对齐 ime.rs 设置卡片头部）。
    let logo = crate::widgets::app_icon(48);
    let header = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(14)
        .child(logo)
        .child(
            Element::col()
                .weight(1.0)
                .spacing(3)
                .child(
                    Element::label("清风输入法")
                        .font_size(18.0)
                        .font_weight(600)
                        .fg_role(Role::Text)
                        .height(24),
                )
                .child(
                    Element::label(if version.is_empty() {
                        "清风输入法设置".to_string()
                    } else {
                        format!("版本 {version}{}", crate::mode::version_suffix())
                    })
                    .font_size(12.0)
                    .fg_role(Role::TextMuted)
                    .height(16),
                ),
        );

    // 系统信息的其它字符串字段（os/arch/build 等）→ 信息卡片。
    let mut info_rows: Vec<Element> = Vec::new();
    if let Some(obj) = info.as_object() {
        for (k, v) in obj {
            if k == "version" {
                continue;
            }
            if let Some(s) = v.as_str() {
                if s.is_empty() {
                    continue;
                }
                info_rows.push(setting_row(
                    k,
                    None,
                    Element::label(s).fg_role(Role::TextMuted),
                ));
            }
        }
    }

    let mut secs = vec![crate::widgets::card(vec![header])];
    if !info_rows.is_empty() {
        secs.push(section("系统信息", info_rows));
    }
    sections_page(secs)
}
