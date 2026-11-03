//! 「方案覆盖」页：稀疏列表 + 「添加覆盖」。
//!
//! 设计见 WindInput `docs/design/schema-settings-vertical-tabs.md` §4：
//! - 全局设置里没有的项（横排时文字排列、候选注释跳转）：直接列出；
//! - 方案文件里已经覆盖的项：直接列出，说明里标「方案文件默认」；
//! - 其余全局有的项：默认不显示，标题右侧「＋ 添加覆盖」勾选后才出现，行尾 ✕ 移除。
//!
//! 行**预建、靠 `visible_when(listed)` 显隐**，不做行级重建：各行的工作态挂在 Element 上，
//! 重建会丢用户刚改的值。

use super::*;

/// 可添加 / 移除的覆盖字段（全局有对应项的那些）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum OvField {
    PerPage,
    Layout,
    Font,
    Punct,
    PunctMap,
    Phrases,
}

/// 覆盖页的分类，每类一张卡，标题行右侧放该类的「添加覆盖」。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OvGroup {
    Candidate,
    Punct,
    Input,
}

impl OvGroup {
    const ALL: [OvGroup; 3] = [Self::Candidate, Self::Punct, Self::Input];

    fn title(self) -> &'static str {
        match self {
            Self::Candidate => "候选窗",
            Self::Punct => "标点",
            Self::Input => "输入",
        }
    }
}

impl OvField {
    pub(super) const ALL: [OvField; 6] = [
        Self::PerPage,
        Self::Layout,
        Self::Font,
        Self::Punct,
        Self::PunctMap,
        Self::Phrases,
    ];

    fn group(self) -> OvGroup {
        match self {
            Self::PerPage | Self::Layout | Self::Font => OvGroup::Candidate,
            Self::Punct | Self::PunctMap => OvGroup::Punct,
            Self::Phrases => OvGroup::Input,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::PerPage => "每页候选数",
            Self::Layout => "候选排列",
            Self::Font => "候选文字字体",
            Self::Punct => "标点",
            Self::PunctMap => "自定义标点",
            Self::Phrases => "加载命令短语",
        }
    }

    fn desc(self) -> &'static str {
        match self {
            Self::PerPage => "本方案期间每页几条；临拼、快捷等扩展档若单独设了条数则不受影响",
            Self::Layout => "本方案期间的候选窗方向",
            Self::Font => "只改候选词本身；序号与编码栏不跟随",
            Self::Punct => "中文标点 / 英文标点",
            Self::PunctMap => "本方案专用的标点映射表。开启后全局那份整份不再生效",
            Self::Phrases => {
                "关闭后本方案不出命令短语候选（与词库里的词组无关）。英文方案建议关闭：短语码常与英文词撞车"
            }
        }
    }

    /// 定制版 / 用户精简藏掉的字段：不进添加菜单，也不渲染（已有的值原样保留）。
    fn hidden(self, h: &BehaviorRowsHidden) -> bool {
        match self {
            Self::PerPage => h.per_page,
            Self::Layout => h.layout,
            Self::Font => h.font,
            Self::PunctMap => h.punct_map,
            Self::Punct | Self::Phrases => false,
        }
    }

    /// 「这一项现在列在覆盖页里」的信号。
    ///
    /// 每页候选数 / 字体 / 自定义标点沿用既有的「勾选」信号（语义恰好就是「有覆盖」），
    /// 其余三项是新增的 `*_listed`。
    pub(super) fn listed(self, s: &SchemaManagerState) -> Signal<bool> {
        match self {
            Self::PerPage => s.settings_bh_pp_over,
            Self::Font => s.settings_bh_font_over,
            Self::PunctMap => s.settings_bh_punct_map_on,
            Self::Layout => s.settings_bh_layout_listed,
            Self::Punct => s.settings_bh_punct_listed,
            Self::Phrases => s.settings_bh_phrases_listed,
        }
    }
}

/// 方案文件里已经写了（与默认不同）的覆盖字段，及「用户层有没有再改」。
///
/// 这类字段**不能移除**：core 写 null 的含义是「回到方案文件的值」，而方案文件的值本来就
/// 在，移除是个空操作（行消失、重开又出现），标点 / 候选排列写的 `"follow"` 甚至会反过来
/// 改掉方案作者的值。所以它们恒列出、无 ✕，只能改值。`file_only` = 用户层没动过，
/// 行说明里标「方案文件默认」。
///
/// 方案文件层的值取 core 旁路字段 `schemaBase`，用同一个 [`seed_behavior`] 读，
/// 「非默认」的判据因此与合并值那边完全一致；用户层取 `schemaUserOverride`。
pub(super) fn file_level(cfg: &Value) -> Vec<(OvField, bool)> {
    let Some(base) = cfg.get("schemaBase") else {
        return Vec::new();
    };
    let b = seed_behavior(base);
    let user = |p: &str| {
        cfg.pointer(&format!("/schemaUserOverride{p}"))
            .is_some_and(|v| !v.is_null())
    };
    OvField::ALL
        .into_iter()
        .filter_map(|f| {
            let (set, user_has) = match f {
                OvField::PerPage => (b.per_page.is_some(), user("/candidate/per_page")),
                OvField::Layout => (b.layout != 0, user("/candidate/layout")),
                OvField::Font => (b.font_family.is_some(), user("/candidate/font_family")),
                OvField::Punct => (b.punct != 0, user("/punct/mode")),
                OvField::PunctMap => (b.punct_map.is_some(), user("/punct/custom_mappings")),
                OvField::Phrases => (!b.phrases, user("/phrases/enabled")),
            };
            set.then_some((f, !user_has))
        })
        .collect()
}

fn is_file_set(s: &SchemaManagerState, f: OvField) -> bool {
    s.settings_ov_file.borrow().iter().any(|(x, _)| *x == f)
}

/// 添加一项：值取**当前生效值**（跟随值），添加后行为不变，用户再去改。
pub(super) fn add(s: &SchemaManagerState, f: OvField) {
    let cfg = s.settings_cfg.borrow();
    match f {
        OvField::PerPage => {
            let followed = cfg
                .pointer("/followedBehavior/perPage")
                .and_then(Value::as_i64)
                .filter(|n| *n >= 1)
                .unwrap_or(7);
            s.settings_bh_pp.set(followed as f64);
        }
        OvField::Font => {
            let followed = cfg
                .pointer("/followedBehavior/fontFamily")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            s.settings_bh_font.set(followed);
        }
        OvField::Layout => {
            let followed = cfg
                .pointer("/followedBehavior/layout")
                .and_then(Value::as_str)
                .unwrap_or("horizontal");
            // 下拉是 [竖排, 横排]
            s.settings_bh_layout
                .set(overlay_layout_idx(followed).saturating_sub(1));
        }
        // 标点跟随值是运行时状态，没有静态值：从「中文标点」起步。
        OvField::Punct => s.settings_bh_punct.set(0),
        // 自定义标点：勾上即启用，格子与追加行一并重置为「内置默认」——移除后再添加不留上次的编辑。
        // （全局那份自定义映射不会带进来：方案表整份替换全局表，和原来勾选「覆盖」时一样。）
        OvField::PunctMap => {
            *s.settings_bh_punct_extra.borrow_mut() = punct_extra_rows(&serde_json::Map::new());
            for (row, (_, _tok, defs)) in s
                .settings_bh_punct_cells
                .iter()
                .zip(crate::manifest::PUNCT_DEFAULTS.iter())
            {
                for i in 0..4 {
                    row[i].set(crate::manifest::seed_cell(None, i, defs[i]));
                }
            }
        }
        OvField::Phrases => s.settings_bh_phrases.set(true),
    }
    drop(cfg);
    f.listed(s).set(true);
}

/// 移除一项：写回「未设置」（落盘由 `behavior_now` 把未列出的字段合成成默认，再由各 `*_write` 写 null）。
pub(super) fn remove(s: &SchemaManagerState, f: OvField) {
    if is_file_set(s, f) {
        // 方案文件自己写了这一项：移除是空操作（见 [`file_level`]），保持列出。
        f.listed(s).set(true);
        return;
    }
    f.listed(s).set(false);
    if f == OvField::Phrases {
        s.settings_bh_phrases.set(true);
    }
}

/// 当前已列出的（且没被精简藏掉的）覆盖项数，供 TAB 标签计数。
pub(super) fn listed_count(s: &SchemaManagerState) -> usize {
    let h = behavior_rows_hidden(s);
    OvField::ALL
        .iter()
        .filter(|f| !f.hidden(&h) && f.listed(s).get())
        .count()
}

fn remove_icon(s: &SchemaManagerState, f: OvField) -> Element {
    let st = s.clone();
    Element::icon_button("\u{2715}")
        .size(24, 24)
        .font_size(11.0)
        .fg_role(Role::TextMuted)
        .tooltip("移除此覆盖")
        .on_click(move |_| remove(&st, f))
}

/// 一个已列出字段的行：控件 + 行尾移除图标。
fn field_row(s: &SchemaManagerState, f: OvField) -> Element {
    let from_file = s
        .settings_ov_file
        .borrow()
        .iter()
        .any(|(x, file_only)| *x == f && *file_only);
    let desc = if from_file {
        format!("{}（方案文件默认）", f.desc())
    } else {
        f.desc().to_string()
    };
    let body = match f {
        OvField::PerPage => crate::widgets::setting_row(
            f.label(),
            Some(&desc),
            Element::stepper(s.settings_bh_pp, 2.0, 10.0, 1.0).width(AUX_CTRL_W),
        ),
        OvField::Font => crate::widgets::setting_row(
            f.label(),
            Some(&desc),
            Element::text_input(s.settings_bh_font, "留空同未覆盖").width(AUX_CTRL_W),
        ),
        OvField::Layout => crate::widgets::setting_row(
            f.label(),
            Some(&desc),
            Element::dropdown(vec!["竖排", "横排"], s.settings_bh_layout).width(BH_DD_W),
        ),
        OvField::Punct => crate::widgets::setting_row(
            f.label(),
            Some(&desc),
            Element::dropdown(vec!["中文标点", "英文标点"], s.settings_bh_punct).width(BH_DD_W),
        ),
        OvField::PunctMap => summary_action_row(
            f.label(),
            Some(&desc),
            s.settings_bh_punct_map_disp,
            "设置",
            s.settings_bh_punct_map_visible,
        ),
        OvField::Phrases => crate::widgets::setting_row(
            f.label(),
            Some(&desc),
            Element::checkbox("开启", s.settings_bh_phrases)
                .small()
                .fg_role(Role::Text),
        ),
    };
    let listed = f.listed(s);
    // 方案文件自己写了的项不给 ✕（见 [`file_level`]）；占位保持各行控件右缘对齐。
    let tail = if is_file_set(s, f) {
        Element::leaf().width(24).height(24)
    } else {
        remove_icon(s, f)
    };
    Element::row()
        .width_match()
        .cross(Align::Center)
        .child(body.weight(1.0))
        .child(tail)
        .visible_when(move || listed.get())
}

/// 该类标题行右侧的「添加覆盖」：勾选 = 列出，取消勾选 = 移除。
fn add_menu(s: &SchemaManagerState, fields: &[OvField]) -> Element {
    let items: Vec<CheckMenuItem> = fields
        .iter()
        .map(|&f| {
            let st = s.clone();
            CheckMenuItem::check(f.label(), f.listed(s))
                .on_change(move |_ctx, v| if v { add(&st, f) } else { remove(&st, f) })
        })
        .collect();
    Element::check_menu("＋ 添加覆盖", items).width(132)
}

/// 「方案覆盖」页本体（`SettingsSection::Behavior` 的构建函数）。
pub(super) fn build(s: &SchemaManagerState) -> Element {
    let hidden = behavior_rows_hidden(s);
    let mut page = Element::col().width_match().spacing(12);
    for g in OvGroup::ALL {
        let fields: Vec<OvField> = OvField::ALL
            .into_iter()
            .filter(|f| f.group() == g && !f.hidden(&hidden))
            .collect();

        // 全局没有对应项的行：直接列出。
        let mut rows: Vec<Element> = Vec::new();
        if g == OvGroup::Candidate {
            rows.push(crate::widgets::setting_row(
                "横排时文字排列",
                // ⚠️ 一行装得下才算数（说明区超宽静默截断、不加省略号）。
                Some("竖排时不生效。蒙古文选旋转 90°，汉字对联式选文字竖排"),
                Element::dropdown(TEXT_ORIENT_LABELS.to_vec(), s.settings_bh_text_orient)
                    .width(BH_DD_W),
            ));
            if !hidden.comment {
                // 注释模板不在方案文件里配置：方案行在全局「候选注释」总表里。
                let req = s.open_comment_settings;
                rows.push(crate::widgets::setting_row(
                    "候选注释",
                    Some("本方案的注释模板在「外观 → 候选标注 → 候选注释」的分配表里配置"),
                    Element::button("在 候选注释 中配置")
                        .small()
                        .outline_soft()
                        .on_click(move |_| req.update(|n| *n += 1)),
                ));
            }
        }
        let direct = rows.len();

        for &f in &fields {
            rows.push(field_row(s, f));
        }
        if rows.is_empty() {
            continue;
        }
        // 没有直接行、也没有已列出项时，给一句话，免得卡片只剩一个标题。
        if direct == 0 {
            let sigs: Vec<Signal<bool>> = fields.iter().map(|f| f.listed(s)).collect();
            rows.push(
                Element::label("没有覆盖项。点右上角「添加覆盖」按需添加。")
                    .font_size(12.0)
                    .fg_role(Role::TextMuted)
                    .padding_xy(14, 10)
                    .visible_when(move || sigs.iter().all(|x| !x.get())),
            );
        }
        let action = {
            let (st, fields) = (s.clone(), fields.clone());
            move || add_menu(&st, &fields)
        };
        page = page.child(crate::widgets::dialog_section_full(
            g.title(),
            None,
            Some(&action),
            rows,
        ));
    }
    page
}
