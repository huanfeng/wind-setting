//! dialog_button 各定制对话框主体（按 key 注册）。

use serde_json::{Map, Value};
use windui::prelude::*;

/// strlist → 空格分隔字符串（对话框文本框展示）。
pub fn strlist_to_spaced(items: &[String]) -> String {
    items.join(" ")
}

/// 空格分隔字符串 → strlist（去空白、跳过空项）。
pub fn spaced_to_strlist(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

/// 空格分隔文本编辑对话框：提示 + text_input + [恢复默认 | 取消 确认]。
/// 确认即写入 `text`（writer 随主保存提交）；取消仅关闭，未确认改动仍保留在 text。
pub fn build_spaced_text_dialog(
    visible: Signal<bool>,
    title: &str,
    hint: &str,
    text: Signal<String>,
    default_text: String,
) -> Element {
    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(hint)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3)
                .padding_xy(18, 10),
        )
        .child(
            // padding 包裹：text_input 用 width_match 填满内边距后的宽，避免 margin 外推溢出对话框。
            Element::col().width_match().padding_xy(18, 6).child(
                Element::text_input(text, "按空格分隔")
                    .multiline()
                    .wrap(true)
                    .width_match()
                    .height(96),
            ),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    Element::button("恢复默认")
                        .small()
                        .outline()
                        .on_click(move |_| text.set(default_text.clone())),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .outline()
                        .on_click(move |_| visible.set(false)),
                )
                .child(
                    Element::button("确认")
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// 单行字符串编辑对话框：提示 + 单行 text_input + [恢复默认 | 取消 确认]。
/// 用于 str 类型的「启用 + 设置」项（如智能标点字符集），需要「恢复默认」故走对话框。
pub fn build_text_dialog(
    visible: Signal<bool>,
    title: &str,
    hint: &str,
    text: Signal<String>,
    default_text: String,
    placeholder: &str,
) -> Element {
    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(hint)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3)
                .padding_xy(18, 10),
        )
        .child(
            Element::col().width_match().padding_xy(18, 6).child(
                Element::text_input(text, placeholder).width_match().height(36),
            ),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    Element::button("恢复默认")
                        .small()
                        .outline()
                        .on_click(move |_| text.set(default_text.clone())),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .outline()
                        .on_click(move |_| visible.set(false)),
                )
                .child(
                    Element::button("确认")
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// map → multi-line text（每行一对 `原符号=替换`）。
///
/// 不变量：键不含 `=`——`=` 是行内键值分隔符，含 `=` 的键经 [`rows_to_map`]
/// 回读会被错误截断；故此降级编辑器仅适用于键无 `=` 的映射（标点映射满足）。
pub fn map_to_rows(map: &Map<String, Value>) -> String {
    let mut pairs: Vec<String> = map
        .iter()
        .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or("")))
        .collect();
    pairs.sort(); // deterministic order for tests
    pairs.join("\n")
}

/// multi-line text → map（去空白、跳过无 `=` 行）。
///
/// 不变量：键不含 `=`——按首个 `=` 切分（`split_once`），故键侧出现 `=` 会被
/// 截断到分隔符前；与 [`map_to_rows`] 对称，仅适用于键无 `=` 的映射。
pub fn rows_to_map(text: &str) -> Map<String, Value> {
    let mut m = Map::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            m.insert(k.trim().to_string(), Value::String(v.trim().to_string()));
        }
    }
    m
}

/// 映射表编辑对话框（降级实现：每行一对 `原符号=替换`）。
/// windui 无动态增删行的 table_editable，故用 multiline text_input 替代。
pub fn build_map_dialog(
    visible: Signal<bool>,
    title: &str,
    hint: &str,
    text: Signal<String>,
    default_text: String,
) -> Element {
    let full_hint = if hint.is_empty() {
        "每行一对，格式：原符号=替换符号".to_string()
    } else {
        format!("{}\n每行一对，格式：原符号=替换符号", hint)
    };
    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(&full_hint)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(4)
                .padding_xy(18, 10),
        )
        .child(
            Element::col().width_match().padding_xy(18, 6).child(
                Element::text_input(text, "，=，")
                    .multiline()
                    .width_match()
                    .height(120),
            ),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    Element::button("恢复默认")
                        .small()
                        .outline()
                        .on_click(move |_| text.set(default_text.clone())),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .outline()
                        .on_click(move |_| visible.set(false)),
                )
                .child(
                    Element::button("确认")
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// 配对复选网格对话框（用于 auto_pair *_pairs 类 strlist）。
/// `sigs`: (value, Signal<bool>)；`labels`: 每项对应的展示标签（1:1 with sigs）；
/// `defaults`: capability 默认 strlist（恢复默认用）。
pub fn build_pairs_dialog(
    visible: Signal<bool>,
    title: &str,
    hint: &str,
    sigs: Vec<(String, Signal<bool>)>,
    labels: Vec<String>,
    defaults: Vec<String>,
) -> Element {
    let sigs_reset = sigs.clone();
    let checkboxes: Vec<Element> = sigs
        .iter()
        .zip(labels.iter())
        .map(|((_, s), lbl)| Element::checkbox(lbl, *s).fg_role(Role::Text))
        .collect();
    let grid = Element::grid(2, 10, checkboxes);
    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label(hint)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3)
                .padding_xy(18, 10),
        )
        .child(grid.width_match().margin_xy(18, 8))
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    Element::button("恢复默认")
                        .small()
                        .outline()
                        .on_click(move |_| {
                            for (v, s) in &sigs_reset {
                                s.set(defaults.iter().any(|d| d == v));
                            }
                        }),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .outline()
                        .on_click(move |_| visible.set(false)),
                )
                .child(
                    Element::button("确认")
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// 设置组对话框：标题 + 若干设置行（行间分隔线）+ [关闭]。
/// 用于「全局方案配置」码表/拼音对话框：内部控件经各自 writer 随主保存提交，无需独立保存。
pub fn build_group_dialog(visible: Signal<bool>, title: &str, rows: Vec<Element>) -> Element {
    let mut content = Element::col().width_match().padding_xy(16, 2);
    let last = rows.len().saturating_sub(1);
    for (i, r) in rows.into_iter().enumerate() {
        content = content.child(r);
        if i != last {
            content = content.child(Element::divider());
        }
    }
    let frame = Element::col()
        .width(460)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(content)
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("关闭")
                        .outline()
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// 自定义序号标签对话框：10 个文本框对应序号 1-10（每框一个字符），2 列排布。
/// `inputs` 为 10 个单字符信号（由调用方注册 writer/loader），编辑随主保存提交；
/// 「恢复默认」清空全部（空 = 默认 1-9）。
pub fn build_index_labels_dialog(
    visible: Signal<bool>,
    title: &str,
    inputs: Vec<Signal<String>>,
) -> Element {
    let cells: Vec<Element> = inputs
        .iter()
        .enumerate()
        .map(|(i, s)| {
            Element::row()
                .cross(Align::Center)
                .spacing(8)
                .child(
                    Element::label(format!("序号 {}", i + 1))
                        .font_size(12.5)
                        .fg_role(Role::TextMuted)
                        .width(48),
                )
                .child(Element::text_input(*s, "").width(64))
        })
        .collect();
    let grid = Element::grid(2, 12, cells);
    let inputs_reset = inputs.clone();
    let frame = Element::col()
        .width(420)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label("从序号 1 起依次填写自定义标签（每框一个字符，如 a s d f g）；整体留空用默认 1-9。")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(2)
                .padding_xy(18, 10),
        )
        .child(grid.width_match().margin_xy(18, 8))
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    Element::button("恢复默认").small().outline().on_click(move |_| {
                        for s in &inputs_reset {
                            s.set(String::new());
                        }
                    }),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("关闭")
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// 自定义标点 5 列可编辑表格对话框（原字符 / 英文半角 / 英文全角 / 中文半角 / 中文全角）。
/// 复刻 examples/settings.rs 的表格 + 单元格编辑子对话框：点单元格弹文本框编辑，确认写回。
/// - `src_labels`：每行第 0 列「原字符」展示（只读，点击不弹编辑）；
/// - `cells_data`：每行 4 个可编辑单元格信号（英半/英全/中半/中全，与 core 数组序一致），
///   这些信号由调用方注册到 writer/loader，编辑即改它们，随主保存提交；
/// - `defaults`：每行 4 列默认值（单元格清空时回落 + 「恢复默认」用）。
///
/// 返回一个含「表格对话框 + 单元格编辑子对话框」两层的 stack，整体挂根。
pub fn build_punct_table_dialog(
    visible: Signal<bool>,
    title: &str,
    src_labels: Vec<String>,
    cells_data: Vec<[Signal<String>; 4]>,
    defaults: Vec<[String; 4]>,
) -> Element {
    // 单元格编辑子对话框状态
    let edit_show = signal(false);
    let edit_buf = signal(String::new());
    let edit_row = signal(0usize);
    let edit_col = signal(0usize); // 0..3，对应 cells_data 行内列

    // 表格单元格：第 0 列为原字符（只读展示），第 1..4 列为可编辑信号。
    let src_sigs: Vec<Signal<String>> = src_labels.iter().map(|s| signal(s.clone())).collect();
    let table_cells: Vec<Vec<Signal<String>>> = src_sigs
        .iter()
        .zip(cells_data.iter())
        .map(|(src, row)| vec![*src, row[0], row[1], row[2], row[3]])
        .collect();

    // 点单元格：第 0 列只读跳过；其余载入缓冲并弹编辑子对话框。
    let cells_for_edit = cells_data.clone();
    let table = Element::table_editable(
        vec![
            ("原字符", 0.8f32),
            ("英文半角", 1.0),
            ("英文全角", 1.0),
            ("中文半角", 1.0),
            ("中文全角", 1.0),
        ],
        table_cells,
        move |_ctx, r, c| {
            if c == 0 {
                return;
            }
            let ci = c - 1;
            edit_buf.set(cells_for_edit[r][ci].get());
            edit_row.set(r);
            edit_col.set(ci);
            edit_show.set(true);
        },
    )
    .height(420);

    // 恢复默认：全部单元格回落默认值。
    let cells_reset = cells_data.clone();
    let defaults_reset = defaults.clone();
    let frame = Element::col()
        .width(720)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::label("点单元格编辑替换字符（留空恢复该格默认）；仅与默认不同的行会被保存。")
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(2)
                .padding_xy(18, 10),
        )
        .child(Element::col().width_match().padding_xy(18, 6).child(table))
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(
                    Element::button("恢复默认").small().outline().on_click(move |_| {
                        for (row, def) in cells_reset.iter().zip(defaults_reset.iter()) {
                            for i in 0..4 {
                                row[i].set(def[i].clone());
                            }
                        }
                    }),
                )
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("关闭")
                        .on_click(move |_| visible.set(false)),
                ),
        );
    let table_dialog = Element::dialog(visible, frame);

    // 单元格编辑子对话框。
    let cells_commit = cells_data.clone();
    let defaults_commit = defaults.clone();
    let edit_frame = Element::col()
        .width(360)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("编辑标点映射")
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(
            Element::col().width_match().padding_xy(18, 10).child(
                Element::text_input(edit_buf, "替换字符（留空恢复默认）")
                    .width_match(),
            ),
        )
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .outline()
                        .on_click(move |_| edit_show.set(false)),
                )
                .child(Element::button("确认").on_click(move |_| {
                    let r = edit_row.get();
                    let ci = edit_col.get();
                    let raw = edit_buf.get();
                    // 半角/全角空格为合法映射值，不可 trim 掉；其余去空白后若空则回落默认。
                    let is_space = raw == " " || raw == "\u{3000}";
                    let trimmed = if is_space { raw.clone() } else { raw.trim().to_string() };
                    let val = if trimmed.is_empty() {
                        defaults_commit[r][ci].clone()
                    } else {
                        trimmed
                    };
                    cells_commit[r][ci].set(val);
                    edit_show.set(false);
                })),
        );
    let edit_dialog = Element::dialog(edit_show, edit_frame);

    Element::stack().fill().child(table_dialog).child(edit_dialog)
}

/// 热键编辑对话框：修饰键勾选 + 主键下拉（+ 可选「全局」）+ [恢复默认 | 取消 确认]。
/// 确认时合成 "ctrl+shift+e" 写回 `raw`（并更新行内显示 `disp`）；恢复默认从 `default` 反填。
#[allow(clippy::too_many_arguments)]
pub fn build_hotkey_dialog(
    visible: Signal<bool>,
    title: &str,
    raw: Signal<String>,
    disp: Signal<String>,
    default: String,
    mods: [Signal<bool>; 4],
    key_idx: Signal<usize>,
    key_values: Vec<String>,
    key_labels: Vec<String>,
    global: Option<Signal<bool>>,
    enabled: Signal<bool>,
) -> Element {
    use crate::manifest::{compose_hotkey, format_hotkey, parse_hotkey};
    let [mc, ms, ma, mw] = mods;

    let mod_row = Element::row()
        .spacing(16)
        .child(Element::checkbox("Ctrl", mc).small().fg_role(Role::Text))
        .child(Element::checkbox("Shift", ms).small().fg_role(Role::Text))
        .child(Element::checkbox("Alt", ma).small().fg_role(Role::Text))
        .child(Element::checkbox("Win", mw).small().fg_role(Role::Text));

    let mut body = Element::col()
        .width_match()
        .spacing(10)
        .padding_xy(18, 12)
        .child(Element::label("修饰键").font_size(12.0).fg_role(Role::TextMuted))
        .child(mod_row)
        .child(Element::label("主键").font_size(12.0).fg_role(Role::TextMuted))
        .child(Element::dropdown(key_labels, key_idx).width(160));
    if let Some(g) = global {
        body = body.child(
            Element::checkbox("注册为全局热键（其它程序中也生效）", g)
                .small()
                .fg_role(Role::Text),
        );
    }

    let kv_ok = key_values.clone();
    let confirm = Element::button("确认").on_click(move |_| {
        let m = [mc.get(), ms.get(), ma.get(), mw.get()];
        let k = kv_ok.get(key_idx.get()).cloned().unwrap_or_default();
        let v = compose_hotkey(m, &k);
        // 同步启用态：对话框选「（未设置）」合成 "none" 即视为禁用，避免与行内复选漂移。
        enabled.set(v != "none");
        disp.set(format_hotkey(&v));
        raw.set(v);
        visible.set(false);
    });
    let kv_def = key_values;
    let reset = Element::button("恢复默认")
        .small()
        .outline()
        .on_click(move |_| {
            let (m, k) = parse_hotkey(&default);
            mc.set(m[0]);
            ms.set(m[1]);
            ma.set(m[2]);
            mw.set(m[3]);
            key_idx.set(kv_def.iter().position(|v| *v == k).unwrap_or(0));
            disp.set(format_hotkey(&default));
            raw.set(default.clone());
            // 同步启用态：默认绑定为 "none" 则视为禁用。
            enabled.set(default != "none");
            // 全局热键默认为空（无功能全局），恢复默认同步取消全局勾选。
            if let Some(g) = global {
                g.set(false);
            }
        });

    let frame = Element::col()
        .width(380)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label(title)
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(body)
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .spacing(8)
                .child(reset)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .outline()
                        .on_click(move |_| visible.set(false)),
                )
                .child(confirm),
        );
    Element::dialog(visible, frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spaced_strlist_roundtrip() {
        let v = vec!["http://".to_string(), "https://".to_string(), "ftp://".to_string()];
        let s = strlist_to_spaced(&v);
        assert_eq!(s, "http:// https:// ftp://");
        assert_eq!(spaced_to_strlist(&s), v);
    }

    #[test]
    fn spaced_to_strlist_trims_and_skips_empty() {
        assert_eq!(spaced_to_strlist("  a   b  "), vec!["a".to_string(), "b".to_string()]);
        assert!(spaced_to_strlist("   ").is_empty());
    }

    #[test]
    fn map_roundtrip() {
        let mut m = serde_json::Map::new();
        m.insert("，".to_string(), Value::String("，".to_string()));
        m.insert("。".to_string(), Value::String("。".to_string()));
        let text = map_to_rows(&m);
        let m2 = rows_to_map(&text);
        assert_eq!(m, m2);
    }

    #[test]
    fn rows_to_map_skips_blank_and_no_eq() {
        let text = "  \n，=，\nbadline\n。=。\n";
        let m = rows_to_map(text);
        assert_eq!(m.len(), 2);
        assert_eq!(m["，"], Value::String("，".to_string()));
    }

    #[test]
    fn map_to_rows_deterministic() {
        // map_to_rows sorts keys, output must be stable
        let mut m = serde_json::Map::new();
        m.insert("b".to_string(), Value::String("B".to_string()));
        m.insert("a".to_string(), Value::String("A".to_string()));
        let text = map_to_rows(&m);
        assert_eq!(text, "a=A\nb=B");
    }
}
