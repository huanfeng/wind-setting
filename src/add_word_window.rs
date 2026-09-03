//! 快捷加词小窗 — 无标题栏独立窗口，自包含表单**直接渲染**（不用 `Element::dialog`
//! 模态遮罩，避免灰底遮罩套在独立窗里）。
//!
//! 关窗靠按钮 `on_click` 内 `ctx.request_close()`：wind-ui-rust 的
//! `core.rs::call_on_update` 会**丢弃** `on_update` 产生的 `EventOutcome`，故不能靠
//! 响应式哨兵在 `on_update` 里 `request_close`（那样永不生效）；而事件派发路径的
//! `out.close` 是被采纳的（同 `shell.rs` 的「保存并退出」）。
//!
//! 入库直接以 core 传来的 `schemaId` 调 `dict.add`（core 侧 `web_dict_add` 用
//! `data_schema_id` 归一，拼音族折叠为 "pinyin"），**不依赖**词库页的域/类别状态机——
//! 那套状态机对无表格视图的独立窗是错配（曾导致确定不入库）。
//!
//! 与主设置窗用不同 app_id（`wind_setting_addword{suffix}`），互不劫持单例。
//! 启动只做最小 RPC（schema.list 一次取方案名；出码/入库按需）。

use serde_json::json;
use windui::prelude::*;

use crate::auto_encode::{AutoEncode, auto_encode_node};
use crate::cli::AddWordParams;
use crate::pages::dict::state::escape_input_newlines;
use crate::state::{SchemaInfo, load_schemas};
use crate::theme_def::theme_for;
use crate::widgets::{ENCODE_BTN_W, clear_text_button, paste_button};

/// 从 schema.list 解析目标方案显示名（仅用于标题，best-effort）。
/// 精确 id 命中取其名；未命中且传入非空（可能是拼音变体 id）→ 取拼音域代表名；
/// 再取不到 → 回退显示原始 schema id。
fn resolve_schema_name(schema_id: &str, schemas: &[SchemaInfo]) -> String {
    let name_of = |s: &SchemaInfo| {
        if s.name.is_empty() {
            s.id.clone()
        } else {
            s.name.clone()
        }
    };
    if let Some(s) = schemas.iter().find(|s| s.id == schema_id) {
        return name_of(s);
    }
    if !schema_id.is_empty()
        && let Some(s) = schemas.iter().find(|s| s.kind == "pinyin")
    {
        return name_of(s);
    }
    schema_id.to_string()
}

/// 标题文案：方案名非空 → 「添加到「<方案名>」词库」；否则退回「添加用户词库」。
fn title_text(schema_name: &str) -> String {
    if schema_name.is_empty() {
        "添加用户词库".to_string()
    } else {
        format!("添加到「{schema_name}」词库")
    }
}

/// 运行加词小窗；不返回（App::run 进入消息循环，关窗即退进程）。
pub fn run(params: AddWordParams) {
    let dark = std::env::args().any(|a| a == "--dark");
    // 缺参补齐必须在算标题与初始编码**之前**：两者都吃 schema，补晚了标题显示不出方案名、
    // 初始出码也会拿着空方案去问 core。
    let params = resolve_params(&params);

    // 字段信号（Signal 为 Copy，可直接进多个闭包）。
    let title = signal(title_text(&resolve_schema_name(
        &params.schema,
        &load_schemas(),
    )));
    let schema_id = signal(params.schema.clone());
    let code = signal(initial_code(&params));
    let text = signal(params.text.clone());
    let weight = signal("1200".to_string());
    let error = signal(String::new());
    let auto = AutoEncode::new();

    let root = build_form(title, schema_id, code, text, weight, error, auto);

    let app_id = crate::single_instance_app_id(true);
    App::new("添加用户词库", 400, 360)
        // 二次启动：重新解析参数并重填字段（复用同一小窗，不新起进程）。
        .single_instance(app_id, move |argv| {
            if let Some(p) = crate::cli::parse_add_word(&argv) {
                // 二次启动同样可能是裸 `--add-word`（用户从开始菜单再点一次），补齐口径
                // 与首次开窗一致。
                let p = resolve_params(&p);
                title.set(title_text(&resolve_schema_name(&p.schema, &load_schemas())));
                code.set(initial_code(&p));
                schema_id.set(p.schema);
                text.set(p.text);
                error.set(String::new());
                // 预填不是「用户手改编码」：不复位的话第二次深链进来就再也不自动出码了。
                auto.reseed();
            }
        })
        .frameless()
        .centered()
        .theme(theme_for(dark))
        .bg(theme_for(dark).palette.bg)
        .screenshot_from_args()
        .content(root)
        .run();
}

/// 向 core 取加词界面的默认上下文。
///
/// 取不到一律回空（core 没起来、或旧版本没有这个方法）——窗口照常打开，只是没有默认值，
/// 与本次改动前的行为相同。**不弹错**：用户可能就是想手填。
#[derive(Default)]
struct AddWordContext {
    schema: String,
    recent: String,
    /// 加词字数上限（core 的 `ADD_WORD_MAX_LEN`）。0 = core 没说，此时不截断。
    ///
    /// 上限必须由 core 交代：本仓够不着那个常量（core 的 crate 在这里是 dev-dependency），
    /// 自己猜一个数迟早与 core 分叉，而分叉的表现是「填得进去、加不进来」——词条填了 15 字，
    /// 「生成编码」出不来、「确定」被 core 的 `check_derivable_word` 拒掉。
    max_len: usize,
}

fn add_word_context() -> AddWordContext {
    let Ok(v) = crate::rpc::call_core("dict.addWordContext", &json!({})) else {
        return AddWordContext::default();
    };
    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    AddWordContext {
        schema: s("schemaId"),
        recent: s("recentText"),
        max_len: v.get("maxLen").and_then(|x| x.as_u64()).unwrap_or_default() as usize,
    }
}

/// 按字符数截断（`max_len == 0` 即 core 没交代上限，原样返回）。
///
/// 按 `chars()` 而不是字节：中文一个字三字节，按字节截会把最后一个字劈成半个。
fn truncate_chars(s: &str, max_len: usize) -> String {
    if max_len == 0 || s.chars().count() <= max_len {
        return s.to_string();
    }
    s.chars().take(max_len).collect()
}

/// 读剪贴板并归一成词条框能用的形态（多行口径同 `widgets::paste_button`）。
///
/// 走 `windui::platform::Clipboard` 而不是 `EventCtx::clipboard_get`：这是**构建期**取
/// 默认值，那时还没有事件上下文。同 `pages::dict::view` 的菜单项复制。
fn clipboard_text() -> String {
    use windui::core::ClipboardProvider;
    windui::platform::Clipboard
        .get_text()
        .unwrap_or_default()
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_string()
}

/// 缺参时该用哪个来源——**纯决策**，两个来源都由调用方取好传进来。
///
/// 单独成函数是为了可测：`resolve_params` 要发 RPC、读剪贴板，测不了；而「哪个参数缺了
/// 用谁补」正是本次要守住的东西。
///
/// 口径：**带了参数就原样用**（深链的意图已经明确，不该被默认值盖掉）；没带才补——方案
/// 取 core 的加词目标方案，词条取剪贴板、剪贴板没有再取最近输入（与 Ctrl+Shift+= 在
/// core 侧的优先级一致）。
fn resolve_fields(p: &AddWordParams, clip: &str, ctx: &AddWordContext) -> (String, String) {
    let schema = if p.schema.trim().is_empty() {
        ctx.schema.clone()
    } else {
        p.schema.clone()
    };
    let text = if !p.text.trim().is_empty() {
        // 深链带来的词条不截断：core 那边已经按同一个上限取好了，再截一次只会掩盖分叉。
        p.text.clone()
    } else if !clip.trim().is_empty() {
        // 剪贴板是**本仓自己读的**，core 没经手 ⇒ 上限要在这里落实。取前 N 字而不是整段
        // 作废，与 core 侧加词面板的剪贴板口径一致（那边同样是截断 + ↓ 砍尾）。
        truncate_chars(clip.trim(), ctx.max_len)
    } else {
        // 最近上屏由 core 按上限取好，原样用。
        ctx.recent.clone()
    };
    (schema, text)
}

/// 把 CLI 参数补成**能用**的一套字段。
///
/// `wind-setting --add-word` 这条裸入口不经输入法热键，schema 与 text 都是空的：方案为空
/// 则 `dict.encode` 与 `dict.add` 都会失败，窗口看着正常却什么也做不了。这里按
/// [`resolve_fields`] 的口径补齐。
///
/// 两个来源都**按需取**：参数齐全时一次 RPC 都不发，剪贴板就已经填上词条时也不去问
/// core 要最近输入。
fn resolve_params(p: &AddWordParams) -> AddWordParams {
    let need_schema = p.schema.trim().is_empty();
    let need_text = p.text.trim().is_empty();
    if !need_schema && !need_text {
        return p.clone();
    }
    let clip = if need_text {
        clipboard_text()
    } else {
        String::new()
    };
    // 走到这里必然缺了点什么（参数齐全的那一支上面已提前返回，一次 RPC 都不发），
    // 而三种缺法都得问 core：缺方案要 schemaId、要用剪贴板得知道截断上限、剪贴板空了
    // 还得退到最近上屏。故这里无条件调。
    let ctx = add_word_context();
    let (schema, text) = resolve_fields(p, &clip, &ctx);
    AddWordParams {
        text,
        code: p.code.clone(),
        schema,
    }
}

/// 开窗时编码框的初值：core 给了码就用它；只给词条没给码时**当场出一次**。
///
/// 自动出码那个叶子以构建时的版本为基线（构建帧刻意不触发），故这一步不能指望它——
/// 少了这一步，从输入法带着词进来看到的就是「词条填好了、编码空着」，用户还得先点一下
/// 「生成编码」，「自动计算编码」这件事在最常见的入口上恰好不生效。
///
/// 出不了码就留空（同「出不了码不清空」的口径），此后照常可手填或点按钮。
fn initial_code(p: &AddWordParams) -> String {
    if !p.code.trim().is_empty() || p.text.trim().is_empty() {
        return p.code.clone();
    }
    encode_text(&p.schema, &p.text)
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// 词条框尾部的「最近」按钮：填入输入法里最近上屏的文字。
///
/// 与「粘贴」并列成一组 ＝ 加词的两个来源（剪贴板 / 最近输入），对应输入法端加词面板的
/// Tab 切换。**从 Ctrl+Shift+= 进来时词条已经预填了剪贴板**，这个按钮就是那条路的另一半
/// ——切到最近输入。
///
/// 取的是 core 侧**字符池上限**那么多字（`ADD_WORD_MAX_LEN`），不是两字：能整段一次上屏
/// 的词说明词库里已经有它、本就不必加，真正要加的词散在多条上屏记录里，取末尾两字往往只
///截到半个词。多出来的部分在多行框里删掉很便宜，少了却要回输入法重打一遍。
fn recent_button(text: Signal<String>, error: Signal<String>) -> Element {
    Element::button("最近")
        .small()
        .outline_soft()
        .width(ENCODE_BTN_W)
        .on_click(move |_| {
            let recent = add_word_context().recent;
            if recent.trim().is_empty() {
                error.set("输入法里还没有最近输入的文字".to_string());
                return;
            }
            error.set(String::new());
            text.set(recent);
        })
}

/// 标签 + 输入行（左标签定宽，右输入撑满）。
fn field(label: &str, input: Element) -> Element {
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::label(label)
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width(40),
        )
        .child(input)
}

/// 按方案反查编码。`Ok(None)` = 出不了码（core 回空码，典型是纯符号），
/// 调用方据此**保持原编码不动**——用户可能已手填了一个能用的码，被一个符号抹掉是净损失。
fn encode_text(schema_id: &str, text: &str) -> Result<Option<String>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    // 词条框是多行的，而 core 收的是**转义形态**（`store_text` 还原）。发真换行过去，
    // core 那侧还原不动它，逐字反查会撞上一个不可读音的字符而整条出不了码——
    // 表现是「词条明明填了，编码就是不出来」。同 `DictManagerState::encode_text`。
    let t = escape_input_newlines(t);
    let v = crate::rpc::call_core("dict.encode", &json!({ "schemaId": schema_id, "text": t }))?;
    let c = v.as_str().unwrap_or_default().trim().to_string();
    Ok(if c.is_empty() { None } else { Some(c) })
}

/// 构建整窗表单：标题栏（可拖动 + 关闭）· 编码/生成编码 · 多行词条/粘贴/清空 · 权重 ·
/// 错误提示 · 取消/确定，外加一个不可见的自动出码叶子。
fn build_form(
    title: Signal<String>,
    schema_id: Signal<String>,
    code: Signal<String>,
    text: Signal<String>,
    weight: Signal<String>,
    error: Signal<String>,
    auto: AutoEncode,
) -> Element {
    // 标题栏：可拖动区 + 标题 + 关闭按钮（window_button::Close 内置 request_close）。
    let titlebar = Element::row()
        .width_match()
        .height(40)
        .cross(Align::Center)
        .padding_xy(14, 0)
        .bg_role(Role::SurfaceAlt)
        .window_drag()
        .child(
            Element::label_signal(title)
                .font_size(13.5)
                .font_weight(600)
                .fg_role(Role::Text)
                .weight(1.0)
                .max_lines(1),
        )
        .child(Element::window_button(WindowButtonKind::Close).fg_role(Role::Text));

    // 编码行：输入 + 「生成编码」按钮（dict.encode，core 按方案判拼音/五笔）。
    // 平时编码由词条自动生成（见下面的 auto 叶子），这个按钮是**手改编码之后的回头路**：
    // 点它即重新按词条生成，并复位「用户手改」标记恢复自动。
    let code_input = Element::row()
        .weight(1.0)
        .cross(Align::Center)
        .spacing(8)
        .child(Element::text_input(code, "").height(34).weight(1.0))
        .child(
            Element::button("生成编码")
                .small()
                .outline_soft()
                .min_width(ENCODE_BTN_W)
                .on_click(move |_| {
                    let t = text.get();
                    if t.trim().is_empty() {
                        error.set("请先填写词条".to_string());
                        return;
                    }
                    match encode_text(&schema_id.get(), &t) {
                        Ok(Some(c)) => {
                            code.set(c);
                            // 先写码再复位：这次写回不算用户手改，且恢复自动生成。
                            auto.reseed();
                            error.set(String::new());
                        }
                        Ok(None) => error.set("无法为该词条生成编码，已保留原编码".to_string()),
                        Err(e) => error.set(format!("生成编码失败：{e}")),
                    }
                }),
        );

    // 词条行：多行输入 + 粘贴 + 清空。与词库页加/改词对话框同款（同样的 88 高多行框、
    // 同样的尾部按钮），两处界面做同一件事，不该一个单行一个多行。
    // 顶对齐而非居中：正文框有 88 高，两个小按钮吊在正中间会离标签很远。
    let text_input = Element::row()
        .weight(1.0)
        .cross(Align::Start)
        .spacing(8)
        .child(
            Element::text_input(text, "")
                .multiline()
                .wrap(true)
                .height(88)
                .weight(1.0),
        )
        // 三个按钮竖排：词条框有 88 高，正好容得下，且输入框宽度不必再让出一截。
        //
        // 宽度显式取 `ENCODE_BTN_W` 与上一行的「生成编码」对齐——两行的右边缘要齐平。
        // 原先两按钮横排时这件事由 `ENCODE_BTN_W == TEXT_BTN_W * 2 + 8` 那条等式承担
        // （见 widgets 里的注释），竖排之后那条等式在这里失效，改由本处显式声明接手。
        .child(
            Element::col()
                .spacing(6)
                .child(paste_button(text, false).width(ENCODE_BTN_W))
                .child(recent_button(text, error))
                .child(clear_text_button(text).width(ENCODE_BTN_W)),
        );

    // 自动出码：词条一变即按当前方案反查编码；用户手改过编码后让位（见 `auto_encode`）。
    // 小窗里没有「当前类别」这一维，恒生效。
    let auto_node = auto_encode_node(
        auto,
        text,
        code,
        || true,
        move |t| encode_text(&schema_id.get(), t).ok().flatten(),
    );

    let body = Element::col()
        .width_match()
        .padding_xy(16, 12)
        .spacing(10)
        .child(field("编码", code_input))
        .child(field("词条", text_input))
        .child(field(
            "权重",
            Element::text_input(weight, "").width(90).height(34),
        ))
        // 错误提示（红字，仅在有错时显示；替代会被 on_update 丢弃的 toast）。
        .child(
            Element::label_signal(error)
                .font_size(12.0)
                // 错误文字走语义色而非硬编码红：随主题解析，与各处 danger 按钮同源。
                .fg_role(Role::Danger)
                .width_match()
                .max_lines(2)
                .visible_when(move || !error.get().is_empty()),
        );

    // 底栏：取消（关窗）· 确定（dict.add 成功即关窗，失败显示错误保持打开）。
    let footer = Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(16, 12)
        .spacing(8)
        .child(Element::leaf().weight(1.0))
        .child(
            Element::button("取消")
                .small()
                .outline_soft()
                .on_click(|ctx| ctx.request_close()),
        )
        .child(Element::button("确定").small().on_click(move |ctx| {
            // 多行框里敲下的真换行统一投影成字面 \n 再发 core（同词库页 `save_calls`）。
            let t = escape_input_newlines(text.get().trim());
            let c = code.get().trim().to_string();
            if t.is_empty() {
                error.set("词条不能为空".to_string());
                return;
            }
            if c.is_empty() {
                error.set("编码不能为空（可点「生成编码」）".to_string());
                return;
            }
            let w: i64 = weight.get().trim().parse().unwrap_or(1200);
            let sid = schema_id.get();
            let p = json!({ "schemaId": sid, "code": c, "text": t, "weight": w });
            match crate::rpc::call_core("dict.add", &p) {
                Ok(_) => ctx.request_close(),
                Err(e) => error.set(format!("添加失败：{e}")),
            }
        }));

    Element::col()
        .fill()
        .bg_role(Role::Surface)
        .child(titlebar)
        .child(Element::divider())
        .child(body)
        // 0×0 不可见叶子，col 无 spacing 故不占位。
        .child(auto_node)
        .child(Element::leaf().weight(1.0))
        .child(Element::divider())
        .child(footer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(text: &str, schema: &str) -> AddWordParams {
        AddWordParams {
            text: text.to_string(),
            code: String::new(),
            schema: schema.to_string(),
        }
    }

    /// core 交代的上下文；`max_len` 取 10（与 core 的 `ADD_WORD_MAX_LEN` 现值一致，
    /// 但本仓不该硬依赖那个数——测试只验「给了上限就截断」这条行为）。
    fn ctx(schema: &str, recent: &str) -> AddWordContext {
        AddWordContext {
            schema: schema.to_string(),
            recent: recent.to_string(),
            max_len: 10,
        }
    }

    /// 深链带齐了参数：两个默认来源一律不许插手。
    #[test]
    fn explicit_params_win_over_defaults() {
        let (s, t) = resolve_fields(
            &params("你好", "wubi"),
            "剪贴板内容",
            &ctx("pinyin", "最近输入"),
        );
        assert_eq!((s.as_str(), t.as_str()), ("wubi", "你好"));
    }

    /// 裸 `--add-word`：方案取 core 的加词目标方案，词条取剪贴板。
    /// 方案补不上正是「窗口看着正常却什么都做不了」的根因。
    #[test]
    fn bare_launch_takes_current_schema_and_clipboard() {
        let (s, t) = resolve_fields(&params("", ""), "量子纠缠", &ctx("wubi", "最近输入"));
        assert_eq!((s.as_str(), t.as_str()), ("wubi", "量子纠缠"));
    }

    /// 剪贴板没有可用内容 → 退到最近输入（与 Ctrl+Shift+= 在 core 侧的优先级一致）。
    #[test]
    fn falls_back_to_recent_when_clipboard_blank() {
        for clip in ["", "   ", "\n\t "] {
            let (_, t) = resolve_fields(&params("", ""), clip, &ctx("wubi", "最近输入"));
            assert_eq!(t, "最近输入", "剪贴板为 {clip:?} 时应退到最近输入");
        }
    }

    /// 两个来源都空 → 词条留空，窗口照常打开（只是要手填）。
    #[test]
    fn empty_sources_leave_text_blank() {
        let (s, t) = resolve_fields(&params("", ""), "", &ctx("wubi", ""));
        assert_eq!((s.as_str(), t.as_str()), ("wubi", ""));
    }

    /// 只缺其中一个：另一个不受影响。
    #[test]
    fn only_the_missing_field_is_filled() {
        // 带了词条、没带方案
        let (s, t) = resolve_fields(&params("你好", ""), "剪贴板", &ctx("wubi", "最近"));
        assert_eq!((s.as_str(), t.as_str()), ("wubi", "你好"));
        // 带了方案、没带词条
        let (s, t) = resolve_fields(&params("", "pinyin"), "剪贴板", &ctx("wubi", "最近"));
        assert_eq!((s.as_str(), t.as_str()), ("pinyin", "剪贴板"));
    }

    /// 全空白的参数等同于没带（`--text=" "` 这种）。
    #[test]
    fn blank_params_count_as_missing() {
        let (s, t) = resolve_fields(&params("  ", "  "), "剪贴板", &ctx("wubi", "最近"));
        assert_eq!((s.as_str(), t.as_str()), ("wubi", "剪贴板"));
    }

    /// 剪贴板超过 core 交代的上限 → **取前 N 字**，不是整段作废。
    ///
    /// 本仓自己读的剪贴板 core 没经手，上限只能在这里落实；不落实的话词条填了十几字，
    /// 「生成编码」出不来、「确定」被 core 拒掉——又是一个「填得进去、加不进来」。
    #[test]
    fn overlong_clipboard_is_truncated_to_core_limit() {
        let long = "量子纠缠态的测量与坍缩过程";
        assert!(long.chars().count() > 10, "夹具本身要够长");
        let (_, t) = resolve_fields(&params("", ""), long, &ctx("wubi", "最近"));
        assert_eq!(t.chars().count(), 10, "应截到 core 交代的上限");
        assert_eq!(
            t, "量子纠缠态的测量与坍",
            "截的是**开头**，与 ↓ 砍尾的方向一致"
        );
    }

    /// core 没交代上限（旧版本、或 core 没起来）→ 不截断，原样填。
    /// 猜一个数比不截断更糟：与 core 分叉后表现是静默丢字。
    #[test]
    fn no_limit_from_core_means_no_truncation() {
        let long = "量子纠缠态的测量与坍缩过程";
        let c = AddWordContext {
            schema: "wubi".into(),
            recent: String::new(),
            max_len: 0,
        };
        let (_, t) = resolve_fields(&params("", ""), long, &c);
        assert_eq!(t, long, "core 没说上限时不得自作主张截断");
    }

    /// 深链带来的词条不截断：core 那边已按同一上限取好，再截一次只会掩盖分叉。
    #[test]
    fn explicit_text_is_never_truncated() {
        let long = "量子纠缠态的测量与坍缩过程";
        let (_, t) = resolve_fields(&params(long, "wubi"), "", &ctx("wubi", "最近"));
        assert_eq!(t, long);
    }
}
