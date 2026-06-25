//! 8 个设置页 — 清单驱动（`manifest::field`）+ 自定义页（方案/词库/统计/关于）。
//!
//! 简单配置项一律走清单引擎按 key 渲染（控件/label/选项/默认全来自 system.manifest）；
//! 复杂页（方案管理、词库表格、关于）手写。标注 A–D 处为 wind-ui-rust 待补能力。

use serde_json::Value;
use windui::prelude::*;

use crate::manifest::field;
use crate::state::AppState;
use crate::widgets::{page, section, section_header, setting_row, todo_hint};

/// 8 个 Tab：(标题, 内容)。顺序按用户指定。
pub fn all_tabs(state: &AppState) -> Vec<(String, Element)> {
    vec![
        ("方案".into(), scheme_page(state)),
        ("输入".into(), input_page(state)),
        ("按键".into(), keys_page(state)),
        ("外观".into(), appearance_page(state)),
        ("词库".into(), dict_page(state)),
        ("统计".into(), stats_page(state)),
        ("高级".into(), advanced_page(state)),
        ("关于".into(), about_page(state)),
    ]
}

/// 批量按 key 取设置行。
fn fields(state: &AppState, keys: &[&str]) -> Vec<Element> {
    keys.iter().map(|k| field(state, k)).collect()
}

/// 由若干 section 组成一页（纵向滚动）。
fn sections_page(sections: Vec<Element>) -> Element {
    let mut col = Element::col().width_match().spacing(16);
    for s in sections {
        col = col.child(s);
    }
    page(col)
}

// ───────────────────────── 方案 ─────────────────────────

fn scheme_page(state: &AppState) -> Element {
    let mgr_visible = state.schema_mgr.visible;
    let active = state
        .current_config
        .get_str("schema.active")
        .unwrap_or("（未设置）")
        .to_string();

    sections_page(vec![
        section(
            "方案管理",
            vec![setting_row(
                "输入方案",
                Some(&format!("当前活动方案：{active}")),
                Element::button("管理方案…").on_click(move |_| mgr_visible.set(true)),
            )],
        ),
        section(
            "默认方案",
            fields(
                state,
                &["schema.primary_codetable", "schema.primary_pinyin"],
            ),
        ),
        section(
            "全局码表 / 全局拼音",
            vec![todo_hint(
                "全局码表默认配置、全局拼音(模糊音)等 key 在 core 注册表中但未进 system.manifest，\
                 待补清单项或专用对话框后接入（见能力/数据缺口说明）。",
            )],
        ),
    ])
}

// ───────────────────────── 输入 ─────────────────────────

fn input_page(state: &AppState) -> Element {
    sections_page(vec![
        section(
            "标点与符号",
            fields(
                state,
                &[
                    "input.auto_pair.chinese",
                    "input.auto_pair.english",
                    "input.punct.follow_mode",
                    "input.punct.smart_after_digit",
                    "input.punct.smart_list",
                    "input.symbol.smart_mode",
                    "input.symbol.smart_timeout_ms",
                    "input.symbol.smart_chars",
                    "input.punct.custom_enabled",
                ],
            ),
        ),
        section(
            "上屏控制",
            fields(
                state,
                &[
                    "input.enter_behavior",
                    "input.space_on_empty_behavior",
                    "input.code_commit.auto_commit_at_full",
                    "input.code_commit.auto_commit_block_on_pinyin",
                    "input.code_commit.auto_commit_min_len",
                    "input.code_commit.top_code_commit",
                    "input.code_commit.clear_on_empty_max",
                ],
            ),
        ),
        section(
            "临时英文与大小写",
            fields(
                state,
                &[
                    "input.temp_english.enabled",
                    "input.temp_english.show_candidates",
                    "input.temp_english.shift_behavior",
                    "input.temp_english.allow_symbols",
                    "input.temp_english.space_as_input",
                    "input.capslock.cancel_on_mode_switch",
                ],
            ),
        ),
        section(
            "拼音与编码",
            fields(
                state,
                &[
                    "input.filter_mode",
                    "schema.pinyin.separator",
                    "input.numpad_behavior",
                ],
            ),
        ),
        section(
            "短语",
            fields(
                state,
                &["input.phrase.min_prefix", "input.phrase.max_display_chars"],
            ),
        ),
        section("网址", fields(state, &["input.url.enabled"])),
        section(
            "启动状态",
            fields(
                state,
                &[
                    "input.default.chinese_mode",
                    "input.default.chinese_punct",
                    "input.default.full_width",
                    "input.default.remember_last_state",
                ],
            ),
        ),
    ])
}

// ───────────────────────── 按键 ─────────────────────────

fn keys_page(state: &AppState) -> Element {
    sections_page(vec![
        section("开关", fields(state, &["keys.commit_on_switch"])),
        modifier_keys_section(state),
        section("键位", {
            let mut rows = vec![section_caption(
                "缺口 B：暂无「按键捕获」控件，键位先以文本直填（如 ctrl+space）",
            )];
            rows.extend(fields(
                state,
                &[
                    "keys.switch_engine",
                    "keys.toggle_full_width",
                    "keys.toggle_punct",
                    "keys.toggle_s2t",
                    "keys.toggle_toolbar",
                    "keys.open_settings",
                    "keys.add_word",
                    "keys.pin_candidate",
                    "keys.delete_candidate",
                ],
            ));
            rows
        }),
    ])
}

/// 切换中英的"多选修饰键"集合 → keys.toggle_mode_keys（StrList）。
/// 缺口 C：暂用多个 checkbox 顶替（待"多选组"控件）。
fn modifier_keys_section(state: &AppState) -> Element {
    let cur: Vec<String> = state
        .current_config
        .get_array("keys.toggle_mode_keys")
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let has = |k: &str| cur.iter().any(|x| x == k);

    let lshift = signal(has("lshift"));
    let rshift = signal(has("rshift"));
    let lctrl = signal(has("lctrl"));
    let rctrl = signal(has("rctrl"));
    let caps = signal(has("capslock"));

    state.fields.writers.borrow_mut().push(Box::new(move |c| {
        let mut v: Vec<Value> = Vec::new();
        if lshift.get() {
            v.push(Value::String("lshift".into()));
        }
        if rshift.get() {
            v.push(Value::String("rshift".into()));
        }
        if lctrl.get() {
            v.push(Value::String("lctrl".into()));
        }
        if rctrl.get() {
            v.push(Value::String("rctrl".into()));
        }
        if caps.get() {
            v.push(Value::String("capslock".into()));
        }
        c.set_path("keys.toggle_mode_keys", Value::Array(v));
    }));

    let checks = Element::col()
        .width_match()
        .spacing(8)
        .padding_xy(0, 6)
        .child(Element::checkbox("左 Shift", lshift))
        .child(Element::checkbox("右 Shift", rshift))
        .child(Element::checkbox("左 Ctrl", lctrl))
        .child(Element::checkbox("右 Ctrl", rctrl))
        .child(Element::checkbox("Caps Lock", caps));

    Element::col()
        .width_match()
        .child(section_header("切换中英修饰键"))
        .child(checks)
}

// ───────────────────────── 外观 ─────────────────────────

fn appearance_page(state: &AppState) -> Element {
    sections_page(vec![
        theme_section(state),
        section(
            "候选窗",
            fields(
                state,
                &[
                    "ui.candidate.per_page",
                    "ui.candidate.layout",
                    "ui.candidate.preedit_display",
                    "ui.candidate.hide_window",
                    "ui.candidate.font_size",
                    "ui.candidate.max_chars",
                    "ui.candidate.index_labels",
                    "ui.candidate.flip_when_above",
                ],
            ),
        ),
        section("模式指示", fields(state, &["ui.mode_indicator.style"])),
        section(
            "悬停提示",
            fields(
                state,
                &[
                    "ui.tooltip.delay",
                    "ui.tooltip.code_enabled",
                    "ui.tooltip.pinyin_enabled",
                    "ui.tooltip.pinyin_heteronyms",
                    "ui.tooltip.pinyin_max_readings",
                    "ui.tooltip.chaizi_enabled",
                    "ui.tooltip.debug_enabled",
                ],
            ),
        ),
        section(
            "状态提示",
            fields(
                state,
                &[
                    "ui.status.offset_x",
                    "ui.status.offset_y",
                    "ui.status.schema_name_style",
                ],
            ),
        ),
    ])
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
    Element::col()
        .width_match()
        .child(section_header("主题"))
        .child(setting_row("候选窗主题", Some("按主题名称选择"), control))
        .child(Element::divider())
        .child(section_caption(
            "主题应用机制（config key 或 theme RPC）待确认，当前仅展示名称。",
        ))
}

// ───────────────────────── 词库 ─────────────────────────

fn dict_page(_state: &AppState) -> Element {
    sections_page(vec![Element::col().width_match().child(section_header("词库管理")).child(
        todo_hint(
            "词库管理（搜索/增删改/导入导出）待实现。缺口 D：wind-ui-rust 暂无「可编辑/可搜索表格」控件。\
             core 侧 dict.* RPC 已就绪（search/add/remove/update/import/export）。",
        ),
    )])
}

// ───────────────────────── 统计 ─────────────────────────

fn stats_page(_state: &AppState) -> Element {
    sections_page(vec![
        Element::col()
            .width_match()
            .child(section_header("输入统计"))
            .child(todo_hint(
                "统计面板待实现。core 侧 stats.* RPC 已就绪（summary/daily/clear/pruneBefore）。",
            )),
    ])
}

// ───────────────────────── 高级 ─────────────────────────

fn advanced_page(state: &AppState) -> Element {
    sections_page(vec![
        section(
            "输入统计",
            fields(state, &["stats.enabled", "stats.track_english"]),
        ),
        section(
            "简繁转换",
            fields(state, &["input.s2t.enabled", "input.s2t.variant"]),
        ),
        section(
            "快捷输入",
            fields(
                state,
                &[
                    "schema.quick_input.enabled",
                    "schema.quick_input.decimal_places",
                ],
            ),
        ),
        section("命令栏", fields(state, &["input.cmdbar.enabled"])),
        section("调试", fields(state, &["debug.log_level"])),
    ])
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

    let mut about = Element::col()
        .width_match()
        .spacing(6)
        .child(
            Element::label("清风输入法")
                .font_size(20.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(28),
        )
        .child(
            Element::label(if version.is_empty() {
                "原生设置 (wind_setting_native)".to_string()
            } else {
                format!("版本 {version}")
            })
            .font_size(13.0)
            .fg_role(Role::TextMuted)
            .height(18),
        );

    // 系统信息中的其它字符串字段（os/arch/build 等）。
    if let Some(obj) = info.as_object() {
        for (k, v) in obj {
            if k == "version" {
                continue;
            }
            if let Some(s) = v.as_str() {
                about = about.child(setting_row(
                    k,
                    None,
                    Element::label(s).fg_role(Role::TextMuted),
                ));
            }
        }
    }

    sections_page(vec![about])
}

/// 分区内的说明小字。
fn section_caption(text: &str) -> Element {
    Element::label(text)
        .font_size(11.0)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(3)
        .padding_xy(0, 4)
}
