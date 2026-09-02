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
        .child(paste_button(text, false))
        .child(clear_text_button(text));

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
