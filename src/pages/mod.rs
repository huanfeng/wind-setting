//! 设置页 — 清单驱动（manifest 的 group → Tab、section → 卡片、item → 行）+ 自定义页。
//!
//! 主体一律由 `system.manifest` 的 `(group, section)` 聚合渲染：同一 section 的项归到
//! 一张卡片，卡片次序为 section 首次出现序，行序为 manifest 原始序——key 永远与 core 一致，
//! core 加字段自动出现，彻底消除硬编码 key 漂移。复杂页（方案管理/词库/统计/关于）手写。

use windui::prelude::*;

use crate::manifest::field;
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
        ("统计".into(), stats_page(state)),
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
        .map(|(sec, keys)| {
            let rows: Vec<Element> = keys.iter().map(|k| field(state, k)).collect();
            section(&sec, rows)
        })
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
    let mgr_visible = state.schema_mgr.visible;
    let active = state
        .current_config
        .get_str("schema.active")
        .unwrap_or("（未设置）")
        .to_string();

    let mut secs = vec![section(
        "方案管理",
        vec![setting_row(
            "输入方案",
            Some(&format!("当前活动方案：{active}")),
            Element::button("管理方案…").on_click(move |_| mgr_visible.set(true)),
        )],
    )];
    secs.extend(manifest_sections(state, "schema"));
    sections_page(secs)
}

// ───────────────────────── 按键 ─────────────────────────

fn keys_page(state: &AppState) -> Element {
    sections_page(manifest_sections(state, "keys"))
}

// ───────────────────────── 外观 ─────────────────────────

fn appearance_page(state: &AppState) -> Element {
    let mut secs = vec![theme_section(state)];
    secs.extend(manifest_sections(state, "ui"));
    sections_page(secs)
}

/// 主题选择：用主题**名称**（theme.list）而非 id。
/// 注：主题应用路径（config key / 专用 RPC）尚不明确，此处先展示名称下拉，
/// 暂不登记保存写回（避免写入未知 key 被 core 校验拒绝）——见说明。
fn theme_section(state: &AppState) -> Element {
    let control: Element = if state.theme_names.is_empty() {
        Element::label("（无可用主题）").fg_role(Role::TextMuted)
    } else {
        let names = state.theme_names.clone();
        let sig = signal(0usize);
        Element::dropdown(names, sig).width(180)
    };
    section(
        "主题",
        vec![setting_row("候选窗主题", Some("按主题名称选择"), control)],
    )
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

// ───────────────────────── 统计 ─────────────────────────

fn stats_page(_state: &AppState) -> Element {
    sections_page(vec![section(
        "输入统计",
        vec![todo_hint(
            "统计面板本期占位，后续作为独立功能实现。\
             core 侧 stats.* RPC 已就绪（summary/daily/clear/pruneBefore）。",
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

    // 头部：渐变 logo + 名称 + 版本（对齐 ime.rs 设置卡片头部）。
    let logo = Element::stack()
        .size(40, 40)
        .child(
            Element::leaf()
                .fill()
                .corner(10.0)
                .bg_gradient(Gradient::linear(
                    (0.0, 0.0),
                    (1.0, 1.0),
                    vec![
                        (0.0, Color::hex(0x2563EB)),
                        (1.0, Color::hex(crate::theme_def::ACCENT)),
                    ],
                )),
        )
        .child(
            Element::label("清")
                .fill()
                .font_size(20.0)
                .fg(Color::WHITE)
                .text_align(Align::Center),
        );
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
                        "原生设置 (wind_setting_native)".to_string()
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
