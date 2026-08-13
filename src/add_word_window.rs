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

use crate::cli::AddWordParams;
use crate::state::{SchemaInfo, load_schemas};
use crate::theme_def::theme_for;

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
    let code = signal(params.code.clone());
    let text = signal(params.text.clone());
    let weight = signal("1200".to_string());
    let error = signal(String::new());

    let root = build_form(title, schema_id, code, text, weight, error);

    let app_id = crate::single_instance_app_id(true);
    App::new("添加用户词库", 400, 300)
        // 二次启动：重新解析参数并重填字段（复用同一小窗，不新起进程）。
        .single_instance(app_id, move |argv| {
            if let Some(p) = crate::cli::parse_add_word(&argv) {
                title.set(title_text(&resolve_schema_name(&p.schema, &load_schemas())));
                schema_id.set(p.schema);
                code.set(p.code);
                text.set(p.text);
                error.set(String::new());
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

/// 构建整窗表单：标题栏（可拖动 + 关闭）· 编码/出码 · 词条 · 权重 · 错误提示 · 取消/确定。
fn build_form(
    title: Signal<String>,
    schema_id: Signal<String>,
    code: Signal<String>,
    text: Signal<String>,
    weight: Signal<String>,
    error: Signal<String>,
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
            Element::label_rc(title)
                .font_size(13.5)
                .font_weight(600)
                .fg_role(Role::Text)
                .weight(1.0)
                .max_lines(1),
        )
        .child(Element::window_button(WindowButtonKind::Close).fg_role(Role::Text));

    // 编码行：输入 + 「出码」按钮（dict.encode，手动出码；core 按方案判拼音/五笔）。
    let code_input = Element::row()
        .weight(1.0)
        .cross(Align::Center)
        .spacing(8)
        .child(Element::text_input(code, "").height(34).weight(1.0))
        .child(
            Element::button("出码")
                .small()
                .outline_soft()
                .on_click(move |_| {
                    let t = text.get().trim().to_string();
                    if t.is_empty() {
                        error.set("请先填写词条".to_string());
                        return;
                    }
                    let sid = schema_id.get();
                    match crate::rpc::call_core(
                        "dict.encode",
                        &json!({ "schemaId": sid, "text": t }),
                    ) {
                        Ok(v) => {
                            if let Some(s) = v.as_str() {
                                code.set(s.to_string());
                                error.set(String::new());
                            }
                        }
                        Err(e) => error.set(format!("出码失败：{e}")),
                    }
                }),
        );

    let body = Element::col()
        .width_match()
        .padding_xy(16, 12)
        .spacing(10)
        .child(field("编码", code_input))
        .child(field(
            "词条",
            Element::text_input(text, "").height(34).weight(1.0),
        ))
        .child(field(
            "权重",
            Element::text_input(weight, "").width(90).height(34),
        ))
        // 错误提示（红字，仅在有错时显示；替代会被 on_update 丢弃的 toast）。
        .child(
            Element::label_rc(error)
                .font_size(12.0)
                .fg(Color::hex(0xEF4444))
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
            let t = text.get().trim().to_string();
            let c = code.get().trim().to_string();
            if t.is_empty() {
                error.set("词条不能为空".to_string());
                return;
            }
            if c.is_empty() {
                error.set("编码不能为空（可点「出码」生成）".to_string());
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
        .child(Element::leaf().weight(1.0))
        .child(Element::divider())
        .child(footer)
}
