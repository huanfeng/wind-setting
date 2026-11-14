//! 「高级 → 扩展程序」卡（设计见 `WindInput/docs/design/extension-hook-channel.md` §6.3、§6.4）。
//!
//! 扩展是独立程序，核心经扩展宿主把按键、上屏等事件发给它们。这张卡管三样：
//!
//! - 总开关 `extensions.enabled`；
//! - 每个扩展的启用与授权（`extensions.list[].enabled / granted`）；
//! - 扩展在自己清单里声明的选项（`extensions.list[].options`）：按声明渲染成下拉 / 开关 / 数值。
//!
//! 总开关关着时，各扩展的开关置灰，「设置…」仍可打开查看但不能改。第一次打开总开关要先在
//! 隐私提示里勾选同意（记在设置程序自己的 `wind_setting.toml`，之后不再提示）：扩展能收到
//! 输入内容，输入法不替用户审查扩展。
//!
//! 装了哪些扩展、各自的清单与运行状态来自 core 的 `extensions.inventory`（构建 / 重载 / 点
//! 「刷新」时现取，同注释词库的来源）。改动照常随底栏「应用设置」一起写回。
//!
//! # 写回不能无中生有地「改」
//!
//! 整个 `extensions.list` 是一个整值键：只要写回的数组与配置里的有一丝不同，打开设置就会被判
//! 「有未保存的更改」。所以 [`write_back`] 逐字段比较**生效值**，没变的字段保留原样（包括
//! 原本省略的字段）；扩展目录里已经没有的条目原样留着；没配置过的扩展只有用户真动了才追加。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::{Map, Value, json};
use wind_hook::options::{OptionKind, OptionSpec, resolve};
use windui::prelude::*;

use crate::config::Config;
use crate::state::AppState;
use crate::widgets::{section, setting_row, setting_row_desc_signal};

/// 搜索锚点（`search::index::HANDWRITTEN` 的 `advanced:extensions` 条目指向这里）。
pub const ANCHOR: &str = "advanced:extensions";
/// 分区名（二级导航 + 卡片标题）。
pub const SECTION: &str = "扩展程序（实验性）";
/// 定制版 `feature:` id。
pub const FEATURE: &str = "extensions";
/// 扩展程序 v1 只在 Windows 上可用（macOS 的进程绑定、Linux 的打包都还没做），别处不显示这张卡。
pub const SUPPORTED: bool = cfg!(windows);

const KEY_ENABLED: &str = "extensions.enabled";
const KEY_LIST: &str = "extensions.list";

/// 授权档位的界面文案。顺序即对话框里勾选框的顺序；不在表里的档位按原名显示。
const TIERS: &[(&str, &str)] = &[
    (
        "key.class",
        "按键类别（字母、空格、回车…，不含具体是哪个键）",
    ),
    ("key.detail", "具体按键"),
    ("mode", "中英文、全半角与当前方案"),
    ("composition", "输入中的编码（当前版本暂不提供）"),
    ("candidates", "候选词（当前版本暂不提供）"),
    ("commit", "上屏的文字"),
];
/// 预留给候选注入的档位，当前版本不授予（core 也会拒绝）。
const RESERVED_TIERS: &[&str] = &["candidate.provide"];

fn tier_label(tier: &str) -> String {
    TIERS
        .iter()
        .find(|(t, _)| *t == tier)
        .map_or_else(|| tier.to_string(), |(_, l)| l.to_string())
}

// ───────────────────────────── 数据（纯逻辑，可测） ─────────────────────────────

/// `extensions.inventory` 里的一个扩展。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Installed {
    pub id: String,
    pub name: String,
    pub want: Vec<String>,
    pub options: Vec<OptionSpec>,
    /// 清单读不了（缺文件、格式错）：扩展不能用，界面只显示原因。
    pub error: Option<String>,
    /// 宿主上报的运行状态；没在运行（未启用、总开关关）时为空。
    pub status: Option<Status>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Status {
    pub state: String,
    pub restarts: u64,
    pub error: Option<String>,
}

pub(crate) fn parse_inventory(v: &Value) -> (String, Vec<Installed>) {
    let dir = v.get("dir").and_then(Value::as_str).unwrap_or_default();
    let list = v
        .get("extensions")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(parse_installed).collect())
        .unwrap_or_default();
    (dir.to_string(), list)
}

fn parse_installed(v: &Value) -> Option<Installed> {
    let id = v.get("id")?.as_str()?.to_string();
    let strs = |k: &str| -> Vec<String> {
        v.get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let status = v.get("status").filter(|s| s.is_object()).map(|s| Status {
        state: s
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        restarts: s.get("restarts").and_then(Value::as_u64).unwrap_or(0),
        error: s.get("error").and_then(Value::as_str).map(String::from),
    });
    Some(Installed {
        name: v
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(&id)
            .to_string(),
        want: strs("want"),
        // 声明解析不了的选项跳过：宿主那边同一份清单会整份判无效，这里不重复报。
        options: v
            .get("options")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|o| serde_json::from_value(o.clone()).ok())
                    .collect()
            })
            .unwrap_or_default(),
        error: v.get("error").and_then(Value::as_str).map(String::from),
        status,
        id,
    })
}

/// 一行扩展的副标题：现在是什么状态。反映的是**已应用**的配置（core 现在的样子），
/// 不是本页还没应用的改动。
pub(crate) fn status_text(p: &Installed, master_on: bool, enabled: bool) -> String {
    if let Some(e) = &p.error {
        return format!("扩展程序无效：{e}");
    }
    if !master_on {
        return "未运行（扩展程序总开关关着）".to_string();
    }
    if !enabled {
        return "未启用".to_string();
    }
    let Some(s) = &p.status else {
        return "未运行（应用设置后启动）".to_string();
    };
    let err = s
        .error
        .as_deref()
        .map(|e| format!("：{e}"))
        .unwrap_or_default();
    match s.state.as_str() {
        "running" => "运行中".to_string(),
        "starting" => "启动中".to_string(),
        "backoff" => format!("退出后等待重启（已重启 {} 次）{err}", s.restarts),
        "tripped" => format!("反复崩溃，已停用{err}"),
        "invalid" => format!("无效{err}"),
        "stopped" => "已停止".to_string(),
        other => other.to_string(),
    }
}

/// 一个扩展在本页上的取值。`options` 是补齐默认值后的完整取值。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Edit {
    pub enabled: bool,
    pub granted: Vec<String>,
    pub options: Map<String, Value>,
}

/// 配置里某扩展的条目（没有则 `None`）。
fn entry_of<'a>(list: &'a [Value], id: &str) -> Option<&'a Value> {
    list.iter()
        .find(|e| e.get("id").and_then(Value::as_str) == Some(id))
}

fn granted_of(e: &Value) -> Vec<String> {
    e.get("granted")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn options_of(e: Option<&Value>) -> Map<String, Value> {
    e.and_then(|e| e.get("options"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// 从配置播种。没配置过的扩展：不启用，授权预填它申请的（启用时一并授予），选项取默认值。
pub(crate) fn seed_edit(list: &[Value], p: &Installed) -> Edit {
    let e = entry_of(list, &p.id);
    Edit {
        enabled: e.is_some_and(|e| e.get("enabled").and_then(Value::as_bool).unwrap_or(true)),
        granted: match e {
            Some(e) => granted_of(e),
            None => p
                .want
                .iter()
                .filter(|t| !RESERVED_TIERS.contains(&t.as_str()))
                .cloned()
                .collect(),
        },
        options: resolve(&p.options, &options_of(e)),
    }
}

fn same_set(a: &[String], b: &[String]) -> bool {
    let mut a = a.to_vec();
    let mut b = b.to_vec();
    a.sort();
    b.sort();
    a == b
}

/// 只存与默认值不同的选项：默认值改了（扩展升级），没动过的项跟着新默认走。
fn non_default(specs: &[OptionSpec], values: &Map<String, Value>) -> Map<String, Value> {
    let defaults = resolve(specs, &Map::new());
    values
        .iter()
        .filter(|(k, v)| defaults.get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// 把本页的取值合进配置里的 `extensions.list`（见模块文档「写回不能无中生有地改」）。
pub(crate) fn write_back(list: &[Value], edits: &[(&Installed, &Edit)]) -> Value {
    let mut out: Vec<Value> = list.to_vec();
    for (p, edit) in edits {
        match out
            .iter_mut()
            .find(|e| e.get("id").and_then(Value::as_str) == Some(&p.id))
        {
            Some(entry) => {
                let old = entry.clone();
                let Some(obj) = entry.as_object_mut() else {
                    continue;
                };
                let old_enabled = old.get("enabled").and_then(Value::as_bool).unwrap_or(true);
                if edit.enabled != old_enabled {
                    obj.insert("enabled".into(), json!(edit.enabled));
                }
                if !same_set(&edit.granted, &granted_of(&old)) {
                    obj.insert("granted".into(), json!(edit.granted));
                }
                if resolve(&p.options, &options_of(Some(&old))) != edit.options {
                    let stored = non_default(&p.options, &edit.options);
                    if stored.is_empty() {
                        obj.remove("options");
                    } else {
                        obj.insert("options".into(), Value::Object(stored));
                    }
                }
            }
            None => {
                let stored = non_default(&p.options, &edit.options);
                // 没配置过、也没动过：不写，免得一打开设置就多出一条。
                if !edit.enabled && stored.is_empty() {
                    continue;
                }
                let mut e = json!({ "id": p.id, "enabled": edit.enabled, "granted": edit.granted });
                if !stored.is_empty() {
                    e["options"] = Value::Object(stored);
                }
                out.push(e);
            }
        }
    }
    Value::Array(out)
}

// ───────────────────────────── 界面 ─────────────────────────────

/// 一个选项的控件信号。
#[derive(Clone)]
enum OptSignal {
    Choice(Signal<usize>),
    Bool(Signal<bool>),
    Number(Signal<f64>),
}

/// 一个扩展的界面状态。信号都在播种时（行作用域之外）建好：行重建时句柄才不会失效。
struct ExtensionUi {
    info: Installed,
    enabled: Signal<bool>,
    status: Signal<String>,
    grants: Vec<(String, Signal<bool>)>,
    options: Vec<(OptionSpec, OptSignal)>,
}

impl ExtensionUi {
    fn new(info: Installed, edit: &Edit, status: String) -> ExtensionUi {
        let mut tiers: Vec<String> = info
            .want
            .iter()
            .filter(|t| !RESERVED_TIERS.contains(&t.as_str()))
            .cloned()
            .collect();
        // 配置里授过、清单已不再申请的档位也列出来，用户才取消得掉。
        for t in &edit.granted {
            if !tiers.contains(t) {
                tiers.push(t.clone());
            }
        }
        tiers.sort_by_key(|t| TIERS.iter().position(|(x, _)| x == t).unwrap_or(usize::MAX));
        let grants = tiers
            .into_iter()
            .map(|t| {
                let on = edit.granted.contains(&t);
                (t, signal(on))
            })
            .collect();
        let options = info
            .options
            .iter()
            .map(|spec| {
                let v = edit.options.get(&spec.key);
                let sig = match &spec.kind {
                    OptionKind::Choice { choices, .. } => {
                        let i = v
                            .and_then(Value::as_str)
                            .and_then(|s| choices.iter().position(|c| c.value == s))
                            .unwrap_or(0);
                        OptSignal::Choice(signal(i))
                    }
                    OptionKind::Bool { .. } => {
                        OptSignal::Bool(signal(v.and_then(Value::as_bool).unwrap_or(false)))
                    }
                    OptionKind::Number { default, .. } => {
                        OptSignal::Number(signal(v.and_then(Value::as_f64).unwrap_or(*default)))
                    }
                };
                (spec.clone(), sig)
            })
            .collect();
        ExtensionUi {
            enabled: signal(edit.enabled),
            status: signal(status),
            grants,
            options,
            info,
        }
    }

    fn edit(&self) -> Edit {
        Edit {
            enabled: self.enabled.get(),
            granted: self
                .grants
                .iter()
                .filter(|(_, s)| s.get())
                .map(|(t, _)| t.clone())
                .collect(),
            options: self
                .options
                .iter()
                .map(|(spec, sig)| {
                    let v = match (sig, &spec.kind) {
                        (OptSignal::Choice(i), OptionKind::Choice { choices, default }) => {
                            json!(choices.get(i.get()).map_or(default, |c| &c.value))
                        }
                        (OptSignal::Bool(b), _) => json!(b.get()),
                        (OptSignal::Number(n), _) => json!(n.get()),
                        _ => Value::Null,
                    };
                    (spec.key.clone(), v)
                })
                .collect(),
        }
    }

    fn has_settings(&self) -> bool {
        !self.grants.is_empty() || !self.options.is_empty()
    }
}

/// 整张卡的状态：挂在构建期的闭包里（writer / loader / 按钮共用）。
#[derive(Clone)]
struct ExtensionsUi {
    master: Signal<bool>,
    dir: Signal<String>,
    rows: Signal<Vec<Rc<ExtensionUi>>>,
    /// 「设置…」对话框当前编辑的扩展（0 或 1 个；用列表承载好让对话框内容随之重建）。
    editing: Signal<Vec<Rc<ExtensionUi>>>,
    dialog: Signal<bool>,
    /// 配置里的 `extensions.list` 原样（写回时以它为底）。
    base: Rc<RefCell<Vec<Value>>>,
    /// 用户是否已同意过隐私提示。
    consented: Rc<Cell<bool>>,
    /// 隐私提示框是否显示、框里的「我已了解」是否勾上。
    consent: Signal<bool>,
    consent_ack: Signal<bool>,
}

impl ExtensionsUi {
    fn new() -> ExtensionsUi {
        ExtensionsUi {
            master: signal(false),
            dir: signal(String::new()),
            rows: signal(Vec::new()),
            editing: signal(Vec::new()),
            dialog: signal(false),
            base: Rc::new(RefCell::new(Vec::new())),
            consented: Rc::new(Cell::new(false)),
            consent: signal(false),
            consent_ack: signal(false),
        }
    }

    /// 用户拧了总开关。第一次打开时先退回关，弹隐私提示，同意后才真正打开。
    fn master_toggled(&self, on: bool) {
        if on && !self.consented.get() {
            self.master.set(false);
            self.consent_ack.set(false);
            self.consent.set(true);
        }
    }

    /// 隐私提示里点了「启用」：`save` 记下已同意，再打开总开关。
    fn accept_consent(&self, save: impl FnOnce()) {
        save();
        self.consented.set(true);
        self.consent.set(false);
        self.master.set(true);
    }

    /// 从配置播种（构建、重新加载时），并现取一次扩展清单与状态。
    fn seed(&self, cfg: &Config) {
        self.master.set(cfg.get_bool(KEY_ENABLED).unwrap_or(false));
        *self.base.borrow_mut() = cfg.get_array(KEY_LIST).cloned().unwrap_or_default();
        self.rows.set(Vec::new());
        self.refresh();
    }

    /// 重取扩展清单与运行状态（「刷新」）：新装的扩展出现、状态更新，本页没应用的改动保留。
    fn refresh(&self) {
        let (dir, installed) = match crate::rpc::call_core("extensions.inventory", &json!({})) {
            Ok(v) => parse_inventory(&v),
            // 取不到（core 没开 / 旧版 core）：列表空着，目录行说明原因。
            Err(e) => (format!("读取扩展程序列表失败：{e}"), Vec::new()),
        };
        self.dir.set(dir);
        let old = self.rows.get();
        let base = self.base.borrow().clone();
        let master = self.master.get();
        let rows = installed
            .into_iter()
            .map(|p| {
                let edit = match old.iter().find(|r| r.info.id == p.id) {
                    Some(r) => r.edit(),
                    None => seed_edit(&base, &p),
                };
                let status = status_text(&p, master, edit.enabled);
                Rc::new(ExtensionUi::new(p, &edit, status))
            })
            .collect();
        self.dialog.set(false);
        self.editing.set(Vec::new());
        self.rows.set(rows);
    }

    fn list_value(&self) -> Value {
        let rows = self.rows.get();
        let edits: Vec<(Installed, Edit)> =
            rows.iter().map(|r| (r.info.clone(), r.edit())).collect();
        let refs: Vec<(&Installed, &Edit)> = edits.iter().map(|(p, e)| (p, e)).collect();
        write_back(&self.base.borrow(), &refs)
    }
}

/// 「扩展程序」卡。四件套（writer / loader / getter / 对话框）同注释词库。
pub fn build_section(app: &Rc<RefCell<AppState>>, state: &AppState) -> Element {
    build(app, state).0
}

fn build(app: &Rc<RefCell<AppState>>, state: &AppState) -> (Element, ExtensionsUi) {
    let ui = ExtensionsUi::new();
    ui.consented
        .set(state.setting_config.ui.extensions_consented);
    ui.seed(&state.current_config);

    let w = ui.clone();
    state
        .fields
        .writers
        .borrow_mut()
        .push(Box::new(move |c: &mut Config| {
            c.set_path(KEY_ENABLED, json!(w.master.get()));
            c.set_path(KEY_LIST, w.list_value());
        }));
    let l = ui.clone();
    state
        .fields
        .loaders
        .borrow_mut()
        .insert(KEY_LIST.to_string(), Box::new(move |c: &Config| l.seed(c)));
    let g = ui.clone();
    state
        .fields
        .getters
        .borrow_mut()
        .insert(KEY_LIST.to_string(), Box::new(move || g.list_value()));
    let g = ui.clone();
    state.fields.getters.borrow_mut().insert(
        KEY_ENABLED.to_string(),
        Box::new(move || json!(g.master.get())),
    );
    state.fields.dialogs.borrow_mut().push(build_dialog(&ui));
    state
        .fields
        .dialogs
        .borrow_mut()
        .push(build_consent_dialog(&ui, app));

    let toggled = ui.clone();
    let master_row = setting_row(
        "启用扩展程序",
        Some("扩展程序是独立运行的小程序，借按键、上屏等事件实现按键音、朗读等功能，不影响打字"),
        Element::switch(ui.master).on_switch_change(move |_, on| toggled.master_toggled(on)),
    );

    let row_ui = ui.clone();
    let list = Element::host_signal(ui.rows, move |p: Rc<ExtensionUi>| extension_row(&row_ui, p));

    let refresh_ui = ui.clone();
    let open_dir = ui.dir;
    let dir_row = setting_row_desc_signal(
        "扩展程序目录",
        ui.dir,
        Element::row()
            .cross(Align::Center)
            .spacing(6)
            .child(
                Element::button("打开目录")
                    .small()
                    .outline_soft()
                    .on_click(move |_| {
                        let dir = std::path::PathBuf::from(open_dir.get());
                        let _ = std::fs::create_dir_all(&dir);
                        let _ = crate::open_external::open_with_default_app(&dir);
                    }),
            )
            .child(
                Element::button("刷新")
                    .small()
                    .outline_soft()
                    .on_click(move |_| refresh_ui.refresh()),
            ),
    );

    (section(SECTION, vec![master_row, list, dir_row]), ui)
}

fn extension_row(ui: &ExtensionsUi, p: Rc<ExtensionUi>) -> Element {
    // 这些行同在卡片的一格里（`host_signal`），卡片只在格与格之间画分隔线：行间的自己补。
    let first = ui.rows.get().first().is_some_and(|r| Rc::ptr_eq(r, &p));
    let mut ctrl = Element::row().cross(Align::Center).spacing(8);
    if p.info.error.is_none() {
        if p.has_settings() {
            let (editing, dialog, pp) = (ui.editing, ui.dialog, p.clone());
            ctrl = ctrl.child(Element::button("设置…").small().outline_soft().on_click(
                move |_| {
                    editing.set(vec![pp.clone()]);
                    dialog.set(true);
                },
            ));
        }
        let master = ui.master;
        ctrl = ctrl.child(Element::switch(p.enabled).enabled_when(move || master.get()));
    }
    let row = setting_row_desc_signal(&p.info.name, p.status, ctrl);
    if first {
        row
    } else {
        Element::col()
            .width_match()
            .child(Element::divider())
            .child(row)
    }
}

fn build_dialog(ui: &ExtensionsUi) -> Element {
    let visible = ui.dialog;
    let master = ui.master;
    let body = Element::host_signal(ui.editing, move |p: Rc<ExtensionUi>| {
        dialog_body(&p, master)
    });
    let frame = Element::col()
        .width(520)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(body)
        .child(Element::divider())
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .padding_xy(16, 12)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("完成")
                        .small()
                        .on_click(move |_| visible.set(false)),
                ),
        );
    Element::dialog(visible, frame)
}

/// 第一次打开总开关时的隐私提示：勾选「我已了解」后才能启用。
fn build_consent_dialog(ui: &ExtensionsUi, app: &Rc<RefCell<AppState>>) -> Element {
    let visible = ui.consent;
    let ack = ui.consent_ack;
    let text = |t: &str, role: Role| {
        Element::label(t)
            .font_size(12.5)
            .fg_role(role)
            .width_match()
            .max_lines(6)
    };
    let body = Element::col()
        .width_match()
        .padding_xy(18, 14)
        .spacing(10)
        .child(text(
            "扩展程序是你自行安装的独立程序。启用后，输入法会按你给每个扩展的授权，\
             把按键、上屏的文字等输入信息发给它，其中可能包含聊天内容、账号等隐私。",
            Role::Text,
        ))
        .child(text(
            "输入法只负责转发，不审查扩展程序的来源和行为，无法保证它们安全：\
             恶意的扩展可能记录或上传你输入的内容。请只安装、启用你信任的扩展程序。",
            Role::Warning,
        ))
        .child(text(
            "输入法会尽量识别密码框，在其中不发送具体按键与上屏文字，但不能保证识别所有场景。",
            Role::TextMuted,
        ))
        .child(Element::checkbox(
            "我已了解上述风险，自行判断扩展程序是否可信",
            ack,
        ));
    let (accept, app) = (ui.clone(), app.clone());
    let frame = Element::col()
        .width(480)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(
            Element::label("启用扩展程序")
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
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| visible.set(false)),
                )
                .child(
                    Element::button("启用")
                        .small()
                        .enabled_when(move || ack.get())
                        .on_click(move |_| {
                            accept.accept_consent(|| app.borrow_mut().save_extensions_consented())
                        }),
                ),
        );
    Element::dialog(visible, frame)
}

fn dialog_body(p: &ExtensionUi, master: Signal<bool>) -> Element {
    let heading = |t: &str| {
        Element::label(t)
            .font_size(12.0)
            .font_weight(600)
            .fg_role(Role::TextMuted)
            .padding_xy(0, 6)
    };
    let body = Element::col()
        .width_match()
        .padding_xy(18, 10)
        .spacing(6)
        .child(
            Element::label("扩展程序总开关关闭时只能查看，打开后才能修改")
                .font_size(12.0)
                .fg_role(Role::Warning)
                .width_match()
                .visible_when(move || !master.get()),
        );
    let mut form = Element::col()
        .width_match()
        .spacing(6)
        .enabled_when(move || master.get());
    if !p.grants.is_empty() {
        form = form.child(heading("允许扩展程序收到"));
        for (tier, on) in &p.grants {
            form = form.child(Element::checkbox(tier_label(tier), *on));
        }
    }
    if !p.options.is_empty() {
        form = form.child(heading("选项"));
        for (spec, sig) in &p.options {
            let label = if spec.label.is_empty() {
                spec.key.as_str()
            } else {
                spec.label.as_str()
            };
            let ctrl = match (sig, &spec.kind) {
                (OptSignal::Choice(i), OptionKind::Choice { choices, .. }) => Element::dropdown(
                    choices
                        .iter()
                        .map(|c| {
                            if c.label.is_empty() {
                                c.value.clone()
                            } else {
                                c.label.clone()
                            }
                        })
                        .collect(),
                    *i,
                ),
                (OptSignal::Bool(b), _) => Element::switch(*b),
                (OptSignal::Number(n), OptionKind::Number { min, max, step, .. }) => {
                    Element::stepper(*n, *min, *max, step.unwrap_or((max - min) / 20.0))
                }
                _ => Element::leaf(),
            };
            let sub = (!spec.description.is_empty()).then_some(spec.description.as_str());
            form = form.child(setting_row(label, sub, ctrl));
        }
    }
    let body = body.child(form);
    Element::col()
        .width_match()
        .child(
            Element::label(format!("{} 设置", p.info.name))
                .font_size(14.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .width_match()
                .padding_xy(18, 14),
        )
        .child(Element::divider())
        .child(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keysound() -> Installed {
        let inv = json!({
            "dir": "C:/x/extensions",
            "extensions": [{
                "id": "keysound", "name": "按键音", "want": ["key.detail"],
                "options": [
                    {"key": "theme", "label": "音色", "type": "choice", "default": "click",
                     "choices": [{"value": "click", "label": "机械键盘"}, {"value": "piano", "label": "钢琴"}]},
                    {"key": "volume", "label": "音量", "type": "number", "default": 0.6, "min": 0.0, "max": 1.0, "step": 0.05}
                ],
                "status": {"id": "keysound", "state": "running", "restarts": 0, "dropped": 0}
            }, {"id": "broken", "error": "读 extension.toml 失败"}]
        });
        let (dir, list) = parse_inventory(&inv);
        assert_eq!(dir, "C:/x/extensions");
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].error.as_deref(), Some("读 extension.toml 失败"));
        list.into_iter().next().unwrap()
    }

    #[test]
    fn untouched_list_is_written_back_verbatim() {
        let p = keysound();
        // 省略了 enabled、选项里存着与默认相同的值：生效值没变就一字不动。
        let list = vec![
            json!({"id": "keysound", "granted": ["key.detail"], "options": {"theme": "piano", "volume": 0.6}}),
            json!({"id": "gone", "enabled": true, "granted": ["mode"]}),
        ];
        let edit = seed_edit(&list, &p);
        assert!(edit.enabled, "省略 enabled 即启用");
        assert_eq!(
            write_back(&list, &[(&p, &edit)]),
            Value::Array(list.clone())
        );
    }

    #[test]
    fn unconfigured_extension_is_not_added_until_touched() {
        let p = keysound();
        let edit = seed_edit(&[], &p);
        assert!(!edit.enabled);
        assert_eq!(edit.granted, vec!["key.detail"], "授权预填它申请的");
        assert_eq!(write_back(&[], &[(&p, &edit)]), json!([]));

        let mut on = edit.clone();
        on.enabled = true;
        assert_eq!(
            write_back(&[], &[(&p, &on)]),
            json!([{"id": "keysound", "enabled": true, "granted": ["key.detail"]}]),
            "启用时一并授予申请的档位；选项全是默认值不存"
        );
    }

    #[test]
    fn changed_fields_are_written_and_defaults_dropped() {
        let p = keysound();
        let list = vec![
            json!({"id": "keysound", "enabled": true, "granted": ["key.detail"], "options": {"theme": "piano"}}),
        ];
        let mut edit = seed_edit(&list, &p);
        assert_eq!(edit.options["theme"], "piano");
        edit.options.insert("theme".into(), json!("click"));
        edit.options.insert("volume".into(), json!(0.3));
        edit.granted = vec!["key.class".into()];
        edit.enabled = false;
        assert_eq!(
            write_back(&list, &[(&p, &edit)]),
            json!([{"id": "keysound", "enabled": false, "granted": ["key.class"], "options": {"volume": 0.3}}])
        );
        edit.options.insert("volume".into(), json!(0.6));
        let v = write_back(&list, &[(&p, &edit)]);
        assert!(v[0].get("options").is_none(), "全回到默认值就不存选项");
    }

    /// 建好卡片、什么都不动：不能产生 diff（否则一打开设置就「有未保存的更改」）；
    /// 拧开开关才产生，且落在这两个键上。走 mock 的 `extensions.inventory` 与出厂配置。
    #[test]
    fn untouched_card_is_clean_and_toggles_are_dirty() {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        let mut st = AppState::empty();
        st.apply_loaded(crate::state::LoadedState::fetch());
        let app = Rc::new(RefCell::new(st));
        let (_, ui) = build(&app, &app.borrow());
        let st = app.borrow();
        let keys = |st: &AppState| -> Vec<String> {
            st.debug_diff_after_writers()
                .into_iter()
                .map(|i| i.key)
                .collect()
        };
        let clean = keys(&st);
        let rows = ui.rows.get();
        assert_eq!(rows.len(), 2, "mock 里一个正常扩展、一个坏清单");
        rows[0].enabled.set(true);
        ui.master.set(true);
        let dirty = keys(&st);
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        assert!(clean.is_empty(), "没动却有 diff：{clean:?}");
        assert!(
            dirty.contains(&"extensions.enabled".to_string()),
            "{dirty:?}"
        );
        assert!(dirty.contains(&"extensions.list".to_string()), "{dirty:?}");
    }

    /// 第一次打开总开关：开关退回关、弹隐私提示；同意后才打开，以后再开不再提示。
    #[test]
    fn first_master_switch_on_requires_consent_once() {
        let ui = ExtensionsUi::new();
        ui.master.set(true);
        ui.master_toggled(true);
        assert!(!ui.master.get(), "未同意前不能打开");
        assert!(ui.consent.get(), "应弹出隐私提示");

        let saved = Cell::new(false);
        ui.accept_consent(|| saved.set(true));
        assert!(saved.get(), "同意要落盘");
        assert!(ui.master.get() && !ui.consent.get());

        ui.master.set(false);
        ui.master_toggled(false);
        ui.master.set(true);
        ui.master_toggled(true);
        assert!(ui.master.get() && !ui.consent.get(), "同意过就不再提示");
    }

    #[test]
    fn status_text_follows_master_switch_enablement_and_runtime_state() {
        let mut p = keysound();
        assert_eq!(status_text(&p, true, true), "运行中");
        assert!(status_text(&p, false, true).contains("总开关"));
        assert_eq!(status_text(&p, true, false), "未启用");
        p.status = Some(Status {
            state: "tripped".into(),
            restarts: 3,
            error: Some("exit code 3".into()),
        });
        assert_eq!(status_text(&p, true, true), "反复崩溃，已停用：exit code 3");
        p.status = None;
        assert!(status_text(&p, true, true).contains("应用设置后"));
        p.error = Some("坏了".into());
        assert_eq!(status_text(&p, true, true), "扩展程序无效：坏了");
    }
}
