//! 注释模板的「插入变量」弹层：可搜索的变量表，点选或回车插到模板框里打开弹层时的光标处。
//!
//! 每项三列：变量名、说明（带一行补充）、样例「你好」的取值（释义库取它自己语种的样例）。
//! 样例值打开时按变量逐个问 core（`appearance.previewTemplate`），与预览同一条求值链。
//! `${dict:<id>}` 按此刻挂载的注释词库逐个列出。键盘：上下键移动高亮、回车插入、Esc 关闭。
//!
//! 搜索框会拿走焦点，所以弹层打开时先记下模板框的选区（[`TemplateField::remember_selection`]），
//! 插入时按记下的位置写，再把焦点与光标还给模板框（见设计 §13.5）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::event::Key;
use windui::prelude::*;

use crate::dialogs::template_assist::TemplateField;

/// 一个挂载的注释词库：`(id, 显示名, 来源说明)`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictRef {
    pub id: String,
    pub label: String,
    pub source: String,
}

/// 变量表的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarDef {
    pub group: &'static str,
    /// 列表里显示的写法（`${pinyin}`、`{ … }`）。
    pub shown: String,
    /// 插进去的文字。
    pub insert: String,
    /// 插入后光标离插入段末尾几个字符（`${|}` 落在 `|` 前）。
    pub caret_back: usize,
    pub desc: String,
    pub sub: String,
    /// 问样例值用的模板；语法片段为空（不显示样例）。
    pub probe: String,
}

/// 固定的变量：`(分组, 变量名, 说明, 补充)`。与 core `EVAL_VAR_NAMES` 中注释可用的那些对应
/// （兼容别名 `code` / `code_all` 不列）。
const VARS: &[(&str, &str, &str, &str)] = &[
    (
        "编码",
        "code_hint",
        "编码提示",
        "码表前缀候选的剩余编码（si→sikao 标 kao）；混输标「拼」",
    ),
    (
        "编码",
        "code_rev",
        "主码表反查编码",
        "整词编码，拼音来源的候选才有；受方案的反查编码开关控制",
    ),
    (
        "编码",
        "code_rev_all",
        "主码表全部编码",
        "全部码位，默认 / 连接；可写 ${code_rev_all:,}",
    ),
    (
        "编码",
        "shuangpin",
        "双拼编码",
        "按当前或已装的双拼方案；全拼下也有值",
    ),
    ("读音", "pinyin", "带调拼音", "多音字按词典消歧"),
    (
        "读音",
        "correct_pinyin",
        "正确拼音",
        "只在模糊音、误读、纠错命中的候选上有值；按「纠错与提示」的来源过滤",
    ),
    ("字形", "chaizi", "拆字字根", "仅单字；需方案配了拆字表"),
    (
        "字形",
        "chaizi_code",
        "拆字编码",
        "该字在拆字表里的编码，仅单字",
    ),
    (
        "字形",
        "chaizi_all",
        "逐字字根",
        "多字词每字都拆，默认空格连接；较长，慎用于横排",
    ),
    ("字形", "chaizi_code_all", "逐字拆字编码", "同上，取编码"),
    (
        "释义",
        "dict",
        "注释词库（首个命中）",
        "查挂载的注释词库；内置释义库不参与，要用请选下面的具体库",
    ),
    (
        "释义",
        "dict_all",
        "注释词库（全部）",
        "各库各取一条，默认「；」连接；不含内置释义库",
    ),
    ("其他", "emoji", "表情", "候选对应的表情符号"),
];

/// 变量表：固定变量，`dict` 两项之后插入各词库的 `${dict:<id>}`，最后是语法片段。
/// `escaped` 为真时换行写 `\n` 两个字符（单行转义框），否则插真换行。
pub fn var_defs(dicts: &[DictRef], escaped: bool) -> Vec<VarDef> {
    let var = |group: &'static str, name: &str, desc: &str, sub: &str| VarDef {
        group,
        shown: format!("${{{name}}}"),
        insert: format!("${{{name}}}"),
        caret_back: 0,
        desc: desc.into(),
        sub: sub.into(),
        probe: format!("${{{name}}}"),
    };
    let mut out = Vec::new();
    for (g, n, d, s) in VARS {
        out.push(var(g, n, d, s));
        if *n == "dict_all" {
            out.extend(dicts.iter().map(|x| {
                var(
                    "释义",
                    &format!("dict:{}", x.id),
                    &x.label,
                    &format!("只查这一个库 · {}", x.source),
                )
            }));
        }
    }
    let syntax = |shown: &str, insert: &str, back: usize, desc: &str, sub: &str| VarDef {
        group: "语法",
        shown: shown.into(),
        insert: insert.into(),
        caret_back: back,
        desc: desc.into(),
        sub: sub.into(),
        probe: String::new(),
    };
    out.push(syntax(
        "${a|b}",
        "${|}",
        2,
        "回退链",
        "取第一个非空的变量，如 ${code_hint|code_rev}",
    ));
    out.push(syntax(
        "{ … }",
        "{ }",
        1,
        "可选段",
        "段内变量全空时整段（含文字）消失，如 { · ${pinyin}}",
    ));
    out.push(syntax(
        "换行",
        if escaped { "\\n" } else { "\n" },
        0,
        "换行",
        "开启「注释首行显示在候选上方」时，第一行显示在上方",
    ));
    out
}

/// 按查询过滤（变量名、说明、补充里任一包含，不分大小写），返回下标。
pub fn filter(defs: &[VarDef], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    defs.iter()
        .enumerate()
        .filter(|(_, d)| {
            q.is_empty()
                || [&d.shown, &d.desc, &d.sub]
                    .iter()
                    .any(|s| s.to_lowercase().contains(&q))
        })
        .map(|(i, _)| i)
        .collect()
}

/// 此刻挂载的注释词库：内置在前，再是用户词库（取设置页里的草稿），最后是方案自带的。
pub fn dict_refs(fields: &crate::state::FieldRegistry) -> Vec<DictRef> {
    let src = crate::rpc::call_core("comment.sources", &json!({})).unwrap_or(json!({}));
    let sources = crate::dialogs::comment_dicts_dialog::auto_sources_from_value(&src);
    let mut out: Vec<DictRef> = Vec::new();
    let mut push = |id: &str, label: &str, source: String| {
        if !id.is_empty() && out.iter().all(|d| d.id != id) {
            out.push(DictRef {
                id: id.into(),
                label: if label.is_empty() { id } else { label }.into(),
                source,
            });
        }
    };
    for s in sources.iter().filter(|s| s.builtin) {
        push(&s.id, &s.label, "内置".into());
    }
    let user = fields
        .getters
        .borrow()
        .get("ui.comment_dicts")
        .map(|g| g())
        .unwrap_or(Value::Null);
    for d in user.as_array().into_iter().flatten() {
        let flag = |k: &str| d.get(k).and_then(Value::as_bool).unwrap_or(false);
        if flag("builtin") || flag("auto") || d.get("enabled") == Some(&Value::Bool(false)) {
            continue;
        }
        let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("");
        push(s("id"), s("label"), format!("用户词库 {}", s("path")));
    }
    for s in sources.iter().filter(|s| !s.builtin) {
        push(&s.id, &s.label, "方案词库自带".into());
    }
    out
}

/// 一个变量的样例值：先问缺省样例（你好），释义库没值时再问英文样例 hello。
fn sample_of(probe: &str) -> String {
    let ask = |sample: Option<&str>| {
        let mut p = json!({ "template": probe, "scene": "comment" });
        if let Some(s) = sample {
            p["sample"] = json!(s);
        }
        crate::rpc::call_core("appearance.previewTemplate", &p)
            .ok()
            .and_then(|r| r["text"].as_str().map(str::to_string))
            .unwrap_or_default()
    };
    let v = ask(None);
    if !v.is_empty() || !probe.starts_with("${dict") {
        return v;
    }
    let en = ask(Some("hello"));
    if en.is_empty() {
        en
    } else {
        format!("{en}（hello）")
    }
}

/// 列表的一行。
#[derive(Debug, Clone, PartialEq)]
enum Row {
    Header(&'static str),
    Item {
        /// 在过滤结果里的序号（高亮与回车按它）。
        pos: usize,
        def: VarDef,
        sample: String,
        hl: bool,
    },
}

const PANEL_W: i32 = 540;
const LIST_H: i32 = 340;
const NAME_W: i32 = 150;
const SAMPLE_W: i32 = 110;

/// 「插入变量 ▾」按钮与它的弹层。`dicts` 在每次打开时调用，取此刻挂载的词库。
pub fn insert_var_button(
    field: &TemplateField,
    escaped: bool,
    dicts: Rc<dyn Fn() -> Vec<DictRef>>,
) -> Element {
    let open = signal(false);
    let query = signal(String::new());
    let hl = signal(0usize);
    let rows: Signal<Vec<Row>> = signal(Vec::new());
    let focus = signal(0u64);
    let reveal = signal(0u64);
    let defs: Rc<RefCell<Vec<VarDef>>> = Rc::new(RefCell::new(Vec::new()));
    let samples: Rc<RefCell<HashMap<String, String>>> = Rc::new(RefCell::new(HashMap::new()));
    // 过滤结果（defs 下标），供回车与上下键。
    let shown: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    let rebuild: Rc<dyn Fn()> = {
        let (defs, samples, shown) = (defs.clone(), samples.clone(), shown.clone());
        Rc::new(move || {
            let defs = defs.borrow();
            let idx = filter(&defs, &query.get());
            let h = hl.get().min(idx.len().saturating_sub(1));
            let mut out = Vec::new();
            let mut last = "";
            for (pos, &i) in idx.iter().enumerate() {
                let d = &defs[i];
                if d.group != last {
                    out.push(Row::Header(d.group));
                    last = d.group;
                }
                out.push(Row::Item {
                    pos,
                    def: d.clone(),
                    sample: samples.borrow().get(&d.probe).cloned().unwrap_or_default(),
                    hl: pos == h,
                });
            }
            *shown.borrow_mut() = idx;
            rows.set(out);
            reveal.update(|n| *n += 1);
        })
    };

    let pick: Rc<dyn Fn(usize)> = {
        let (defs, shown, field) = (defs.clone(), shown.clone(), field.clone());
        Rc::new(move |pos: usize| {
            let Some(&i) = shown.borrow().get(pos) else {
                return;
            };
            let d = defs.borrow()[i].clone();
            field.insert_text(&d.insert, d.caret_back);
            open.set(false);
        })
    };

    let move_hl: Rc<dyn Fn(isize)> = {
        let (shown, rebuild) = (shown.clone(), rebuild.clone());
        Rc::new(move |delta: isize| {
            let n = shown.borrow().len();
            if n == 0 {
                return;
            }
            let next = (hl.get() as isize + delta).rem_euclid(n as isize) as usize;
            hl.set(next);
            rebuild();
        })
    };

    let (rb_q, pick_enter) = (rebuild.clone(), pick.clone());
    let search = Element::text_input(query, "搜索：拼音 / 释义 / code…")
        .width_match()
        .focus_when(focus)
        .on_nav_key(move |_, ev| match ev.key {
            Key::Down => {
                move_hl(1);
                true
            }
            Key::Up => {
                move_hl(-1);
                true
            }
            _ => false,
        })
        .on_submit(move |_| pick_enter(hl.get()));
    let refilter = crate::pages::dict::reactive::reactive_effect(
        move || query.version(),
        move || {
            hl.set(0);
            rb_q();
        },
    );

    let pick_row = pick.clone();
    let list = Element::scroll()
        .width_match()
        .max_height(LIST_H)
        .child(Element::host_signal(rows, move |r: Row| {
            row_view(r, reveal, pick_row.clone())
        }));

    let panel = Element::col()
        .width(PANEL_W)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(8.0)
        .shadow(Shadow::new(0.0, 8.0, 24.0, Color::rgba(0, 0, 0, 90)))
        .child(Element::col().width_match().padding(8).child(search))
        .child(Element::divider())
        .child(list)
        .child(Element::divider())
        .child(
            Element::label("点击或回车插入到光标处 · ↑↓ 选择 · Esc 关闭 · 右列是「你好」的取值")
                .font_size(11.5)
                .fg_role(Role::TextMuted)
                .padding_xy(12, 6),
        )
        .child(refilter);

    let (f, rb_open) = (field.clone(), rebuild);
    Element::button("插入变量 ▾")
        .small()
        .outline_soft()
        .on_click(move |ctx| {
            if open.get() {
                open.set(false);
                return;
            }
            f.remember_selection(ctx);
            let list = var_defs(&dicts(), escaped);
            let mut s = samples.borrow_mut();
            s.clear();
            for d in list.iter().filter(|d| !d.probe.is_empty()) {
                s.insert(d.probe.clone(), sample_of(&d.probe));
            }
            drop(s);
            *defs.borrow_mut() = list;
            hl.set(0);
            if !query.get().is_empty() {
                query.set(String::new());
            }
            rb_open();
            open.set(true);
            focus.update(|n| *n += 1);
        })
        .popup(open, panel)
}

fn row_view(r: Row, reveal: Signal<u64>, pick: Rc<dyn Fn(usize)>) -> Element {
    match r {
        Row::Header(g) => Element::label(g)
            .font_size(11.5)
            .fg_role(Role::TextSubtle)
            .padding_xy(12, 4)
            .width_match(),
        Row::Item {
            pos,
            def,
            sample,
            hl,
        } => {
            let mut row = Element::row()
                .width_match()
                .spacing(10)
                .padding_xy(12, 5)
                .clickable()
                .on_click(move |_| pick(pos))
                .child(
                    Element::label(def.shown)
                        .font_size(12.5)
                        .fg_role(Role::AccentText)
                        .width(NAME_W)
                        .max_lines(1),
                )
                .child(
                    Element::col()
                        .weight(1.0)
                        .child(
                            Element::label(def.desc)
                                .font_size(12.5)
                                .fg_role(Role::Text)
                                .max_lines(1),
                        )
                        .child(
                            Element::label(def.sub)
                                .font_size(11.5)
                                .fg_role(Role::TextMuted)
                                .width_match()
                                .max_lines(2),
                        ),
                )
                .child(
                    Element::label(sample)
                        .font_size(12.0)
                        .fg_role(Role::TextMuted)
                        .width(SAMPLE_W)
                        .max_lines(1),
                );
            if hl {
                row = row.bg_role_alpha(Role::Accent, 0.16).reveal_when(reveal);
            }
            row
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dicts() -> Vec<DictRef> {
        vec![
            DictRef {
                id: "en_zh".into(),
                label: "英汉释义".into(),
                source: "内置".into(),
            },
            DictRef {
                id: "mine".into(),
                label: "我的".into(),
                source: "用户词库 comments/mine.dict.yaml".into(),
            },
        ]
    }

    /// 各词库的 `${dict:<id>}` 紧跟在 `dict_all` 之后，语法片段在最后。
    #[test]
    fn dict_entries_follow_dict_all_and_syntax_comes_last() {
        let d = var_defs(&dicts(), false);
        let names: Vec<&str> = d.iter().map(|v| v.shown.as_str()).collect();
        let at = names.iter().position(|n| *n == "${dict_all}").unwrap();
        assert_eq!(names[at + 1], "${dict:en_zh}");
        assert_eq!(names[at + 2], "${dict:mine}");
        assert_eq!(d.last().unwrap().group, "语法");
        assert_eq!(d.last().unwrap().insert, "\n", "多行框插真换行");
        assert_eq!(var_defs(&[], true).last().unwrap().insert, "\\n");
    }

    /// 搜索看变量名、说明与补充，不分大小写。
    #[test]
    fn filter_matches_name_desc_and_sub() {
        let d = var_defs(&dicts(), false);
        let hit = |q: &str| -> Vec<String> {
            filter(&d, q)
                .into_iter()
                .map(|i| d[i].shown.clone())
                .collect()
        };
        assert!(hit("PINYIN").contains(&"${pinyin}".to_string()));
        assert!(hit("英汉").contains(&"${dict:en_zh}".to_string()));
        assert!(
            hit("拆字表").contains(&"${chaizi}".to_string()),
            "补充文字也算"
        );
        assert_eq!(filter(&d, "").len(), d.len());
        assert!(hit("不存在的词").is_empty());
    }

    /// 按真实挂法驱动：点按钮打开弹层、在搜索框按 ↓ 再回车，第二项插到模板框记下的光标处，
    /// 弹层关闭。
    #[test]
    fn keyboard_picks_highlighted_variable() {
        use windui::core::Tree;
        use windui::event::{KeyEvent, MouseButton, PointerEvent, PointerKind};
        use windui::geometry::{Point, Size};

        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let text = signal("ab".to_string());
        let field = TemplateField::new(text, false);
        let mut tree = Tree::new();
        let root = Element::col()
            .width(600)
            .child(field.hook(Element::text_input(text, "").width(300)))
            .child(insert_var_button(&field, false, Rc::new(Vec::new)))
            .build(&mut tree);
        tree.root = Some(root);
        let size = Size::new(800, 700);
        let mut te = windui::text::NullTextEngine;
        tree.layout_root(size, &mut te);

        let button = tree.get(root).unwrap().children[1];
        let r = tree.abs_bounds(button);
        let at = Point::new(r.x + 10, r.y + r.h / 2);
        let (mut hover, mut capture) = (None, None);
        for kind in [PointerKind::Down, PointerKind::Up] {
            tree.dispatch_pointer(
                PointerEvent::single(kind, at, MouseButton::Left),
                &mut hover,
                &mut capture,
            );
        }
        tree.layout_root(size, &mut te);

        // 弹层里的搜索框：正文为空的那个输入框（模板框是 "ab"）。
        let mut stack = vec![root];
        let mut search = None;
        while let Some(id) = stack.pop() {
            stack.extend(tree.get(id).unwrap().children.iter().copied());
            if tree.ime_text_of(id).as_deref() == Some("") {
                search = Some(id);
            }
        }
        let search = search.expect("弹层里有搜索框");
        tree.dispatch_key(KeyEvent::pressed(Key::Down), Some(search));
        tree.dispatch_key(KeyEvent::pressed(Key::Enter), Some(search));
        let got = text.get();
        assert!(
            got.contains("${code_rev}") && got.contains("ab"),
            "第二项（${{code_rev}}）插进模板框：{got}"
        );
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
    }
}
