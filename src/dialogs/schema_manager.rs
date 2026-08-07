//! 方案管理对话框 — 查看/启用/禁用/重排输入方案，导入占位。
//!
//! 布局策略：
//! - 已启用方案（schema.available 中的 ID）按序显示在列表上方，可用 ↑/↓ 调整顺序
//! - 未启用方案在下方，可通过「隐藏未启用」开关收起
//! - 启用/禁用/排序变更**即时刷新列表**（Element::list_signal 驱动）
//! - 「应用设置」时写入 schema.available（经 FieldRegistry writer）

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::core::EventCtx;
use windui::prelude::*;

use crate::config::Config;
use crate::rpc;
use crate::widgets::flat_panel;

/// 引导键候选（与临时拼音 / 临时英文 / 快捷输入清单里的那组逐字一致）。
///
/// 只列符号键：字母键作引导键会吞掉以该字母开头的所有编码（`z` 是历史教训），
/// 这里不给这种选项。
pub const TRIGGER_KEY_OPTIONS: &[(&str, &str)] = &[
    ("backtick", "反引号 ( ` )"),
    ("semicolon", "分号 ( ; )"),
    ("quote", "单引号 ( ' )"),
    ("comma", "逗号 ( , )"),
    ("period", "句号 ( . )"),
    ("slash", "斜杠 ( / )"),
    ("backslash", "反斜杠 ( \\ )"),
    ("lbracket", "左方括号 ( [ )"),
    ("rbracket", "右方括号 ( ] )"),
];

/// 方案条目（含从 schema.list 解析的完整元信息）。
#[derive(Clone, Default)]
pub struct SchemaEntry {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub engine_type: String,      // "codetable" | "pinyin" | "mixed"
    pub scheme: String,           // 拼音子方案："full" | "shuangpin"（非拼音为空串）
    pub shuangpin_layout: String, // 双拼当前布局 id（仅 scheme=="shuangpin"；行名后缀显示）
    pub source: String,           // "builtin" | "user"
    pub version: String,
    pub description: String,
    pub author: String,
    /// 特殊方案（core `[schema].hidden`）：英文、快符这类。默认不在列表显示，
    /// 由工具栏「显示特殊方案」开关放出来。已启用的即使 hidden 也恒显示——
    /// 用户既然启用了它，藏起来只会让人找不到怎么停用。
    pub hidden: bool,
    pub error: Option<String>,
}

// 双拼布局清单不再硬编码：运行时经 `schema.layouts` RPC 动态拉取（合并安装目录
// 与用户目录 schemas/shuangpin/*.toml），存于 SchemaManagerState::layouts。
// 见 SchemaManagerState::layout_label / refresh。

/// `list_signal` 的数据项：行/分节标题/行间分隔线。
#[derive(Clone)]
pub enum SchemaListItem {
    Row {
        entry: SchemaEntry,
        is_enabled: bool,
    },
    /// 分区标题（自带上方分隔线）。`hidden_with_disabled` 为真时随 hide_disabled 一起隐藏
    /// ——未启用区是这样，特殊方案区不是（它跟「启用」无关）。
    SectionHeader {
        title: String,
        hidden_with_disabled: bool,
    },
    /// 行间分隔线；hidden_with_disabled=true 时随 hide_disabled 一起隐藏。
    Divider { hidden_with_disabled: bool },
}

/// 方案设置对话框的一个配置分节。新增一种方案设置 = 加一个变体 + build_section 分支。
#[derive(Clone)]
pub enum SettingsSection {
    /// 方案元信息头部（名称/类型/来源/版本/作者/描述）。恒为首节。
    Header,
    /// 进入方式：切到该方案的直达热键（`keys.schema_hotkeys`）。所有方案都有。
    EntryKeys,
    ShuangpinLayout,
    /// 码表配置：总开关 +（开启后）子对话框。仅码表方案有。
    CodeTable,
    ExtensionDicts,
    /// 节间分隔线（Header 自带结尾分隔，故只在其后的各节之间插入）。
    Divider,
    Empty,
}

/// 扩展词库行（UI 响应式包装：每行一个开关信号 + 打开时的初始值）。
/// 拨动只翻转 `on`（暂存），点「保存」时对比 `initial` 提交变更。
#[derive(Clone)]
pub struct DictRow {
    pub id: String,
    pub label: String,
    pub description: String,
    /// 打开对话框时的启用状态（用于保存时计算变更差异）。
    pub initial: bool,
    pub on: Signal<bool>,
}

/// 一个方案待提交的 override 改动。
///
/// `cfg` 是**改写后的整份方案配置**而非增量：core 的 `saveConfig` 拿它与方案文件 diff
/// 后全量重写 override，喂增量会把没带上的字段当成「已删除」。
#[derive(Clone, Default)]
struct PendingSchemaEdit {
    cfg: Option<Value>,
    /// 词库开关变更 (dict_id, enabled)。
    dicts: Vec<(String, bool)>,
}

/// 方案管理状态（Signal 为 Copy，可直接 Clone 进闭包）。
#[derive(Clone)]
pub struct SchemaManagerState {
    /// 方案列表原始数据。
    pub schemas: Rc<RefCell<Vec<SchemaEntry>>>,
    /// 已启用方案的有序 ID 列表（对应 schema.available 配置键）。
    /// 空 vec 表示"全部启用"（首次加载且配置为空时的默认状态）。
    pub enabled_order: Rc<RefCell<Vec<String>>>,
    /// 驱动 list_signal 的数据信号；任何启用/排序变更后调 rebuild_list() 更新。
    pub list_data: Signal<Vec<SchemaListItem>>,
    /// 是否隐藏未启用方案行（响应式，立即生效）。
    pub hide_disabled: Signal<bool>,
    /// 是否显示特殊方案（core `[schema].hidden`：英文、快符）。默认关。
    /// 列表数据恒含它们（refresh 走 includeHidden），此开关只控制渲染，切换即时生效。
    pub show_hidden: Signal<bool>,
    /// 当前选中方案 ID（工具栏按钮的作用对象；None=无选中）。
    pub selected_id: Rc<RefCell<Option<String>>>,
    /// 选中变更计数：每次变更自增，触发重布局使行高亮/工具栏 enabled_when 重估。
    pub sel_ver: Signal<u64>,
    /// 当前活跃方案 ID（core `schema.active`）。refresh() 时经 RPC 拉运行时值；
    /// 「设为当前」时双写（setActive 即时生效 + 由 state.rs 的 writer 随保存持久化）。
    pub active_id: Rc<RefCell<String>>,
    /// 当前方案变更计数：与 sel_ver 同理，驱动工具栏「设为当前」按钮 enabled_when 重估。
    pub active_ver: Signal<u64>,
    /// 操作反馈（三态 kind + 消息）：经 toast_sink 弹单条 toast，替代原 status 式提示。
    pub feedback: crate::pages::dict::reactive::Feedback,
    pub confirm_visible: Signal<bool>,
    pub confirm_msg: Signal<String>,
    pub confirm_id: Rc<RefCell<String>>,
    // ── 方案包导入确认（previewImport 摘要 + 合并/替换选择）──
    pub import_confirm_visible: Signal<bool>,
    pub import_preview_msg: Signal<String>,
    /// 待导入的方案包路径（预览确认后执行导入）。
    pending_import_path: Rc<RefCell<Option<std::path::PathBuf>>>,
    pub settings_visible: Signal<bool>,
    /// 设置对话框当前方案条目（驱动结构化头部渲染）。
    pub settings_entry: Rc<RefCell<Option<SchemaEntry>>>,
    /// 设置对话框当前方案 ID（保存时用）。
    pub settings_schema_id: Rc<RefCell<String>>,
    /// 设置对话框打开时经 schema.getConfig 拉取的完整方案配置（保存时改写后回传）。
    pub settings_cfg: Rc<RefCell<Value>>,
    /// 当前方案是否为双拼（决定「双拼布局」行与保存按钮的可见性）。
    pub settings_is_shuangpin: Signal<bool>,
    /// 双拼布局下拉选中索引（对应 [`SchemaManagerState::layouts`] 的顺序）。
    pub settings_layout: Signal<usize>,
    /// 双拼布局清单 (id, 显示名)：refresh() 时经 `schema.layouts` RPC 动态拉取
    /// （合并安装目录与用户目录），取代原硬编码；下拉选项/索引映射/行名后缀均以此为准。
    pub layouts: Rc<RefCell<Vec<(String, String)>>>,
    /// 当前打开方案应显示的配置分节（open_settings_at_id 重算，驱动对话框 body 重建）。
    pub settings_sections: Signal<Vec<SettingsSection>>,
    /// 扩展词库行（每行含独立受控开关信号）；ExtensionDicts 分节据此渲染。
    pub settings_dicts: Rc<RefCell<Vec<DictRow>>>,

    // ── 方案直达热键（core `keys.schema_hotkeys`，Map: 方案 id → 热键串）──
    /// 全量映射，随主保存一并写回（见 `AppState` 里注册的 writer）。
    ///
    /// **不在这里直接调 `config.setItems`**：那会与主设置页的 base/current diff 打架——
    /// 主页面持有的是打开时的旧值，用户之后按一次「保存」就会把刚配的热键整体覆盖回去
    /// （Map 型键在 diff 里是原子叶子，整对象产出）。改走 writer 注册，与
    /// `schema.available` / `schema.active` 同一条路。
    pub schema_hotkeys: Rc<RefCell<std::collections::HashMap<String, String>>>,
    // ── 特殊方案的引导键（core `schema.special_modes`）──
    /// 原始条目数组，随主保存整表写回。**保存的是 `Value` 而非结构体**：条目还带
    /// `show_all_on_enter` / `candidate_layout` / 注释模板等只能手写 config.toml 的字段，
    /// 用结构体重建会把它们静默抹掉——用户改一次引导键就丢配置。
    pub special_modes: Rc<RefCell<Vec<Value>>>,
    /// 引导键复选工作态（固定 9 个候选键，与临拼/临英同一组）。
    pub settings_trigger_sigs: Vec<(String, Signal<bool>)>,
    pub settings_trigger_visible: Signal<bool>,
    /// 行内摘要（「未设置」/「已选 N 个」）。与勾选同步更新，不等 commit 落表——
    /// 否则用户勾完看不到变化。
    pub settings_trigger_disp: Signal<String>,

    /// 热键编辑对话框的可见性与工作态（打开方案设置时按当前方案播种）。
    pub settings_hotkey_visible: Signal<bool>,
    pub settings_hotkey_raw: Signal<String>,
    pub settings_hotkey_disp: Signal<String>,
    pub settings_hotkey_enabled: Signal<bool>,
    pub settings_hotkey_mods: [Signal<bool>; 4],
    pub settings_hotkey_key_idx: Signal<usize>,

    // ── 方案级码表配置（`engine.codetable`，见 dialogs::schema_codetable）──
    /// 设置清单：方案级码表配置与全局页**共用同一份条目定义**（label/hint/范围/联动）。
    /// 本对话框不持有 `AppState`，故自带一份内嵌清单。
    pub manifest: Rc<crate::manifest::Manifest>,
    /// 「自定义码表配置」总开关。初值由配置内容推导（`schema_codetable::has_custom`），
    /// 不另存布尔键——多一个键就多一处可能与实际内容不一致的状态。
    pub settings_ct_enabled: Signal<bool>,
    pub settings_ct_visible: Signal<bool>,
    /// 子对话框标题（「码表配置 - 五笔」）：同一个浮层服务于任意方案，
    /// 标题里不带方案名的话，点进去就不知道自己在配哪个表了。
    pub settings_ct_title: Signal<String>,
    /// 行内摘要（「跟随全局配置」/「已自定义」）。**定宽**——文本随勾选变化，
    /// 若靠显隐表达状态，右侧的「设置」按钮会跟着左右跳。
    pub settings_ct_disp: Signal<String>,
    /// 各配置项的工作态信号（打开时按生效值播种，保存时落盘）。
    pub settings_ct: crate::dialogs::schema_codetable::CtFields,
    // ── 方案级按键功能表（方案顶层 `key_actions`，见 dialogs::schema_key_actions）──
    pub settings_ka: crate::dialogs::schema_key_actions::KeyActionFields,
    pub settings_ka_visible: Signal<bool>,
    pub settings_ka_title: Signal<String>,
    pub settings_ka_disp: Signal<String>,
    /// 子对话框的渲染计划：对话框结构在选中方案**之前**就建好，信号要等播种，
    /// 中间靠这个信号驱动 `host_signal` 重建。
    pub settings_ct_plan: Signal<Vec<crate::dialogs::schema_codetable::CtItem>>,
    /// 打开时的生效值快照（core 随 `schema.getConfig` 给的 `effectiveCodetable`）。
    /// 保存时用来补齐没有控件的隐藏项。
    pub settings_ct_eff: Rc<RefCell<Value>>,

    /// 打开对话框那一刻的双拼布局索引 / 码表总开关值。脏判定用——没有它就分不清
    /// 「用户拨过」和「本来就是这个值」，于是打开看一眼再关掉也会写一次盘。
    settings_layout_at_open: Rc<std::cell::Cell<usize>>,
    settings_ct_enabled_at_open: Rc<std::cell::Cell<bool>>,
    /// 待提交的方案 override 改动（方案 id → 变更）。关窗时暂存，随主保存提交。
    ///
    /// **按方案 id 存**而不是只留最后一次的工作态：用户可以连着配好几个方案再点
    /// 「应用设置」，只留一份的话先配的那几个会静默丢。
    pending_edits: Rc<RefCell<std::collections::HashMap<String, PendingSchemaEdit>>>,
}

impl SchemaManagerState {
    pub fn new() -> Self {
        let available = Self::read_available();
        let hotkeys = Self::read_schema_hotkeys();
        let special = Self::read_special_modes();
        let state = Self {
            schemas: Rc::new(RefCell::new(Vec::new())),
            enabled_order: Rc::new(RefCell::new(available)),
            schema_hotkeys: Rc::new(RefCell::new(hotkeys)),
            special_modes: Rc::new(RefCell::new(special)),
            settings_trigger_sigs: TRIGGER_KEY_OPTIONS
                .iter()
                .map(|(k, _)| (k.to_string(), signal(false)))
                .collect(),
            settings_trigger_visible: signal(false),
            settings_trigger_disp: signal("未设置".to_string()),
            settings_hotkey_visible: signal(false),
            settings_hotkey_raw: signal(String::new()),
            settings_hotkey_disp: signal(String::new()),
            settings_hotkey_enabled: signal(false),
            settings_hotkey_mods: [signal(false), signal(false), signal(false), signal(false)],
            settings_hotkey_key_idx: signal(0),
            list_data: signal(vec![]),
            hide_disabled: signal(false),
            show_hidden: signal(false),
            selected_id: Rc::new(RefCell::new(None)),
            sel_ver: signal(0),
            active_id: Rc::new(RefCell::new(String::new())),
            active_ver: signal(0),
            feedback: signal(None),
            confirm_visible: signal(false),
            confirm_msg: signal(String::new()),
            confirm_id: Rc::new(RefCell::new(String::new())),
            import_confirm_visible: signal(false),
            import_preview_msg: signal(String::new()),
            pending_import_path: Rc::new(RefCell::new(None)),
            settings_visible: signal(false),
            settings_entry: Rc::new(RefCell::new(None)),
            settings_schema_id: Rc::new(RefCell::new(String::new())),
            settings_cfg: Rc::new(RefCell::new(Value::Null)),
            settings_is_shuangpin: signal(false),
            settings_layout: signal(0),
            layouts: Rc::new(RefCell::new(Vec::new())),
            settings_sections: signal(vec![]),
            settings_dicts: Rc::new(RefCell::new(Vec::new())),
            manifest: Rc::new(crate::manifest::Manifest::from_embedded()),
            settings_ct_enabled: signal(false),
            settings_ct_visible: signal(false),
            settings_ct_title: signal("码表配置".to_string()),
            settings_ct_disp: signal(String::new()),
            settings_ct: Default::default(),
            settings_ka: Default::default(),
            settings_ka_visible: signal(false),
            settings_ka_title: signal("按键功能".to_string()),
            settings_ka_disp: signal(String::new()),
            settings_ct_plan: signal(vec![]),
            settings_ct_eff: Rc::new(RefCell::new(Value::Null)),
            settings_layout_at_open: Rc::new(std::cell::Cell::new(0)),
            settings_ct_enabled_at_open: Rc::new(std::cell::Cell::new(false)),
            pending_edits: Rc::new(RefCell::new(std::collections::HashMap::new())),
        };
        state.refresh();
        state
    }

    /// 双拼布局显示名（未知布局回退 id 原文）。数据源为 layouts（schema.layouts RPC）。
    fn layout_label(&self, id: &str) -> String {
        self.layouts
            .borrow()
            .iter()
            .find(|(lid, _)| lid == id)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    /// 同步读取 config.get → schema.available。
    fn read_available() -> Vec<String> {
        rpc::call_core("config.get", &json!({}))
            .ok()
            .and_then(|v| {
                v.get("schema")
                    .and_then(|s| s.get("available"))
                    .and_then(|a| a.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
            })
            .unwrap_or_default()
    }

    /// 读取 `keys.schema_hotkeys`（方案 id → 热键串）。缺失/类型不符时按空表处理——
    /// 该键是 Map，出厂就是 `{}`，没有「默认热键」这回事。
    fn read_schema_hotkeys() -> std::collections::HashMap<String, String> {
        rpc::call_core("config.get", &json!({}))
            .ok()
            .and_then(|v| {
                v.get("keys")
                    .and_then(|k| k.get("schema_hotkeys"))
                    .and_then(|m| m.as_object())
                    .map(|o| {
                        o.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                            .collect()
                    })
            })
            .unwrap_or_default()
    }

    /// 读取 `schema.special_modes`（原始条目数组）。
    fn read_special_modes() -> Vec<Value> {
        rpc::call_core("config.get", &json!({}))
            .ok()
            .and_then(|v| {
                v.get("schema")
                    .and_then(|s| s.get("special_modes"))
                    .and_then(|a| a.as_array())
                    .cloned()
            })
            .unwrap_or_default()
    }

    /// 组装按键功能表的动词选项。
    ///
    /// 固定动词 + 实例动词（`mix:<id>` / `special:<id>`）。实例随已安装方案增删，故每次打开
    /// 设置都重新读一遍 `config.get`，不缓存——用户可能刚在别的页加了个特殊方案。
    ///
    /// ⚠️ 首项恒为「不启用」：`Element::dropdown` 的选中值是**索引**，新建行的索引为 0，
    /// 若首项是个真动作，「添加一行还没选」就等于配了那个动作。
    ///
    /// `toggle_schema:<id>` 刻意不列：方案级表里只允许有夺取回路或可往返的动词，
    /// 见 core 仓 docs/design/schema-key-actions.md §5（方案级不允许单向 switch_schema，
    /// 而 toggle_schema 属 C 类、四期才进方案级表）。
    fn build_key_action_verbs(&self) -> Vec<crate::dialogs::schema_key_actions::Verb> {
        use crate::dialogs::schema_key_actions::Verb;
        let mut out = vec![
            Verb {
                value: "none".into(),
                label: "不启用".into(),
            },
            Verb {
                value: "temp_pinyin".into(),
                label: "临时拼音".into(),
            },
            Verb {
                value: "temp_english".into(),
                label: "临时英文".into(),
            },
        ];
        let root = rpc::call_core("config.get", &json!({})).ok();
        let arr = |seg: &str| -> Vec<Value> {
            root.as_ref()
                .and_then(|v| v.get("schema"))
                .and_then(|s| s.get(seg))
                .and_then(|a| a.as_array())
                .cloned()
                .unwrap_or_default()
        };
        for m in arr("mix_modes") {
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if id.is_empty() {
                continue;
            }
            // ★ 必须 filter 非空：core 的 MixModeConfig 带 #[serde(default)]，未填的
            // name 序列化成 ""（不是缺失），unwrap_or 对 Some("") 不生效 —— 表现为
            // 下拉里显示「快捷输入 - 」这样的半截标签。
            let name = m
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(id);
            out.push(Verb {
                value: format!("mix:{id}"),
                label: format!("快捷输入 - {name}"),
            });
        }
        for m in arr("special_modes") {
            // id 缺省时回退 schema 名，与 core 的 `SpecialModeConfig::effective_id` 一致。
            let sid = m.get("schema").and_then(|v| v.as_str()).unwrap_or("");
            let id = m
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(sid);
            if id.is_empty() {
                continue;
            }
            // 同上：未填 name 时回落 id（多半就是方案 id，如 quick_symbols）。
            let name = m
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(id);
            out.push(Verb {
                value: format!("special:{id}"),
                label: format!("特殊模式 - {name}"),
            });
        }
        // C 类：切到目标方案，再按回来。**只在修饰键上可用**——它必须在英文模式下也按得动
        // （否则切到英文方案就回不来），而有字符的键走的 keydown 链在英文态根本到不了。
        // 这里照常列出，由 `KeyActionFields` 在行内按所选键给提示（core 侧同样有裁决）。
        //
        // 刻意不提供全局那个单向的 `switch_schema:<id>`：方案级绑定 + 单向切换会锁死用户
        // （A 里配了去 B，B 没配回程 ⇒ 只能去点工具栏）。往返版由运行时来源兜底，
        // 不要求目标方案配对称的绑定。见 docs/design/schema-key-actions.md §5。
        for e in self.schemas.borrow().iter() {
            if e.id.is_empty() || e.hidden {
                continue;
            }
            let name = if e.name.is_empty() { &e.id } else { &e.name };
            out.push(Verb {
                value: format!("toggle_schema:{}", e.id),
                label: format!("切换方案 - {name}"),
            });
        }
        out
    }

    /// 用某方案已配的引导键播种复选工作态。
    ///
    /// 与热键工作态同理：这组信号是所有方案复用的，不逐个重置就会把上一个方案的
    /// 勾选带到下一个方案。
    fn seed_trigger_keys(&self, schema_id: &str) {
        let cur = crate::special_modes::special_mode_trigger_keys(
            &self.special_modes.borrow(),
            schema_id,
        )
        .unwrap_or_default();
        for (k, sig) in &self.settings_trigger_sigs {
            sig.set(cur.contains(k));
        }
    }

    /// 把引导键工作态写回 `special_modes`（内存；随主保存提交）。
    pub fn commit_trigger_keys_edit(&self) {
        let id = self.settings_schema_id.borrow().clone();
        if id.is_empty() {
            return;
        }
        // 新建条目要写显示名（UI 徽标 / 模式指示用），取方案名、空则回退 id。
        let name = self
            .settings_entry
            .borrow()
            .as_ref()
            .map(|e| {
                if e.name.is_empty() {
                    e.id.clone()
                } else {
                    e.name.clone()
                }
            })
            .unwrap_or_else(|| id.clone());
        let keys: Vec<String> = self
            .settings_trigger_sigs
            .iter()
            .filter(|(_, s)| s.get())
            .map(|(k, _)| k.clone())
            .collect();
        let cur = self.special_modes.borrow().clone();
        *self.special_modes.borrow_mut() =
            crate::special_modes::upsert_special_mode_trigger_keys(&cur, &id, &name, &keys);
    }

    /// 某方案当前已配的引导键条数（行内摘要用）。
    pub fn trigger_key_count(&self, schema_id: &str) -> usize {
        crate::special_modes::special_mode_trigger_keys(&self.special_modes.borrow(), schema_id)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// 某方案当前绑定的直达热键（未绑定返回空串）。
    pub fn hotkey_of(&self, schema_id: &str) -> String {
        self.schema_hotkeys
            .borrow()
            .get(schema_id)
            .cloned()
            .unwrap_or_default()
    }

    /// 用某个绑定串播种热键编辑器的工作态。
    ///
    /// 空串 = 未绑定：启用开关关、修饰位清空、主键回到列表首项。这些信号是**对话框
    /// 复用的同一组**，不重置的话打开 A 方案配了键、再打开 B 方案会看到 A 的值。
    fn seed_hotkey_editor(&self, binding: &str) {
        use crate::manifest::{format_hotkey, hotkey_keys, parse_hotkey};
        self.settings_hotkey_enabled.set(!binding.is_empty());
        self.settings_hotkey_raw.set(binding.to_string());
        // format_hotkey 对空串/"none" 已返回「未设置」，无需在此再分支。
        self.settings_hotkey_disp.set(format_hotkey(binding));
        let (mods, key) = parse_hotkey(binding);
        // parse_hotkey 返回的修饰序与对话框的 [Ctrl, Shift, Alt, Win] 一致。
        for (sig, on) in self.settings_hotkey_mods.iter().zip(mods.iter()) {
            sig.set(*on);
        }
        let idx = hotkey_keys()
            .iter()
            .position(|(v, _)| v == &key)
            .unwrap_or(0);
        self.settings_hotkey_key_idx.set(idx);
    }

    /// 把热键编辑器的当前工作态写回 `schema_hotkeys`（内存；随主保存提交）。
    ///
    /// 关掉启用开关 = 移除该方案的条目，而不是写空串——空串会在 config.toml 里留下
    /// 一条 `方案id = ""` 的死条目，core 侧 `parse_hotkey` 虽然会忽略它，但用户翻开
    /// 配置文件会以为自己设过什么。
    pub fn commit_hotkey_edit(&self) {
        let id = self.settings_schema_id.borrow().clone();
        let raw = self.settings_hotkey_raw.get();
        apply_hotkey_edit(
            &mut self.schema_hotkeys.borrow_mut(),
            &id,
            self.settings_hotkey_enabled.get(),
            &raw,
        );
    }

    /// 根据当前 schemas + enabled_order 重建 list_data 信号（即时刷新列表）。
    fn rebuild_list(&self) {
        let order = self.enabled_order.borrow().clone();
        let is_all_enabled = order.is_empty();
        let show_hidden = self.show_hidden.get();

        // 三个区，**特殊方案先摘出去**：它没有「启用/未启用」这回事——不进 available、
        // 不参与循环切换，只靠引导键或直达热键进入，所以既不该出现在已启用区、也不该
        // 出现在未启用区（更不该随「隐藏未启用」一起消失，那个开关管的是普通方案）。
        let (special, normal): (Vec<SchemaEntry>, Vec<SchemaEntry>) = self
            .schemas
            .borrow()
            .iter()
            .cloned()
            .partition(|e| e.hidden);

        let mut enabled_entries: Vec<SchemaEntry> = Vec::new();
        if is_all_enabled {
            enabled_entries = normal.clone();
        } else {
            for id in &order {
                if let Some(e) = normal.iter().find(|e| &e.id == id) {
                    enabled_entries.push(e.clone());
                }
            }
        }

        let disabled_entries: Vec<SchemaEntry> = if is_all_enabled {
            vec![]
        } else {
            normal
                .into_iter()
                .filter(|e| !order.contains(&e.id))
                .collect()
        };

        let n_disabled = disabled_entries.len();
        let mut items: Vec<SchemaListItem> = Vec::new();

        // 已启用区
        for (i, entry) in enabled_entries.into_iter().enumerate() {
            if i > 0 {
                items.push(SchemaListItem::Divider {
                    hidden_with_disabled: false,
                });
            }
            items.push(SchemaListItem::Row {
                entry,
                is_enabled: true,
            });
        }

        // 未启用区（SectionHeader 自带上方分隔线，随 hide_disabled 一起隐藏）
        if n_disabled > 0 {
            items.push(SchemaListItem::SectionHeader {
                title: format!("其他已安装方案（共 {n_disabled} 个）"),
                hidden_with_disabled: true,
            });
            for (j, entry) in disabled_entries.into_iter().enumerate() {
                if j > 0 {
                    items.push(SchemaListItem::Divider {
                        hidden_with_disabled: true,
                    });
                }
                items.push(SchemaListItem::Row {
                    entry,
                    is_enabled: false,
                });
            }
        }

        // 特殊方案区：整区由「显示特殊方案」控制，**不受「隐藏未启用」影响**。
        // 在这里过滤而非靠 visible_when：区标题的计数按 items 数算，隐藏行若仍在数据里，
        // 标题会报出一个比实际可见行数大的数字。
        if show_hidden && !special.is_empty() {
            items.push(SchemaListItem::SectionHeader {
                title: format!("特殊方案（共 {} 个）", special.len()),
                hidden_with_disabled: false,
            });
            for (k, entry) in special.into_iter().enumerate() {
                if k > 0 {
                    items.push(SchemaListItem::Divider {
                        hidden_with_disabled: false,
                    });
                }
                // is_enabled 对特殊方案无意义（行内不渲染启用勾选），传 true 只是让
                // 名称按正常色显示——它们随时可用，不该是「未启用」那种灰。
                items.push(SchemaListItem::Row {
                    entry,
                    is_enabled: true,
                });
            }
        }

        self.list_data.set(items);
    }

    /// apply_loaded 时调用：从配置更新启用排序，并重建列表。
    pub fn apply_config(&self, cfg: &Config) {
        let available: Vec<String> = cfg
            .get_array("schema.available")
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        // available 为空表示全部启用：展开为全部方案 ID，使后续 toggle 逻辑无歧义。
        let all_ids: Vec<String> = self.schemas.borrow().iter().map(|e| e.id.clone()).collect();
        let new_order = if available.is_empty() {
            all_ids
        } else {
            available
        };
        *self.enabled_order.borrow_mut() = new_order;
        self.rebuild_list();
    }

    /// 刷新方案列表（调用 schema.list RPC）。
    pub fn refresh(&self) {
        // 恒取全集（含特殊方案），显示与否由 `show_hidden` 在渲染层过滤——
        // 开关切换无需重新走 RPC，与 `hide_disabled` 同一套做法。
        let list = rpc::call_core("schema.list", &json!({ "includeHidden": true }))
            .unwrap_or(Value::Array(vec![]));

        // 双拼布局清单（动态，取代硬编码）：供下拉选项/索引映射/行名后缀。
        let layouts: Vec<(String, String)> = rpc::call_core("schema.layouts", &json!({}))
            .ok()
            .and_then(|v| {
                v.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|it| {
                            let id = it.get("id")?.as_str()?.to_string();
                            let name = it
                                .get("name")
                                .and_then(|v| v.as_str())
                                .map(str::to_string)
                                .unwrap_or_else(|| id.clone());
                            Some((id, name))
                        })
                        .collect()
                })
            })
            .unwrap_or_default();
        *self.layouts.borrow_mut() = layouts;

        // 当前活跃方案：取 core 的**运行时**状态（`schema.active` RPC），而非配置里的
        // 同名键——后者只是启动初值，用户可能已在别处（托盘/快捷键）切过方案。
        // 拉不到时保持原值，避免"当前"标记因一次 RPC 抖动而消失。
        if let Some(id) = rpc::call_core("schema.active", &json!({}))
            .ok()
            .and_then(|v| v.get("id").and_then(|x| x.as_str()).map(str::to_string))
        {
            *self.active_id.borrow_mut() = id;
        }

        let order = self.enabled_order.borrow();
        let all_enabled = order.is_empty();

        let entries: Vec<SchemaEntry> = list
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| {
                        let id = s.get("id")?.as_str()?.to_string();
                        let engine_type = s
                            .get("engineType")
                            .or_else(|| s.get("engine_type"))
                            .or_else(|| s.get("kind"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let source = if let Some(b) = s.get("builtin").and_then(|v| v.as_bool()) {
                            if b { "builtin" } else { "user" }.to_string()
                        } else {
                            s.get("source")
                                .and_then(|v| v.as_str())
                                .unwrap_or("builtin")
                                .to_string()
                        };
                        let enabled = all_enabled || order.contains(&id);
                        let scheme = s
                            .get("scheme")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        // 双拼方案补拉当前布局（行名显示「名称·布局」；通常仅一个双拼方案）。
                        let shuangpin_layout = if scheme == "shuangpin" {
                            rpc::call_core("schema.getConfig", &json!({ "id": id.clone() }))
                                .ok()
                                .and_then(|cfg| {
                                    cfg.pointer("/engine/pinyin/shuangpin/layout")
                                        .and_then(|v| v.as_str())
                                        .map(str::to_string)
                                })
                                .unwrap_or_default()
                        } else {
                            String::new()
                        };
                        Some(SchemaEntry {
                            id,
                            name: s
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            enabled,
                            engine_type,
                            scheme,
                            shuangpin_layout,
                            source,
                            hidden: s.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false),
                            version: s
                                .get("version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            description: s
                                .get("description")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            author: s
                                .get("author")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            error: s.get("error").and_then(|v| v.as_str()).map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        drop(order);

        *self.schemas.borrow_mut() = entries;

        // 选中项已不存在（如被删除/刷新后消失）则清空选中。
        let stale = self
            .selected_id
            .borrow()
            .as_ref()
            .is_some_and(|id| !self.schemas.borrow().iter().any(|e| &e.id == id));
        if stale {
            self.clear_selection();
        }
        // 活跃方案可能在 core 侧被切换过（托盘/快捷键），bump 让工具栏按钮重估。
        self.bump_active();
        self.rebuild_list();
    }

    // ── 选中模型（工具栏按钮的作用对象） ──────────────────────

    /// 选中指定方案（行点击回调）。
    pub fn select(&self, id: &str) {
        *self.selected_id.borrow_mut() = Some(id.to_string());
        self.bump_sel();
    }

    /// 清空选中。
    pub fn clear_selection(&self) {
        *self.selected_id.borrow_mut() = None;
        self.bump_sel();
    }

    fn bump_sel(&self) {
        self.sel_ver.set(self.sel_ver.get().wrapping_add(1));
    }

    fn bump_active(&self) {
        self.active_ver.set(self.active_ver.get().wrapping_add(1));
    }

    // ── 当前方案（schema.active） ─────────────────────────────

    /// 该 ID 是否为当前活跃方案（行内「当前」徽章用）。
    pub fn is_active(&self, id: &str) -> bool {
        self.active_id.borrow().as_str() == id
    }

    /// 该 ID 是否处于启用状态。以 enabled_order 为准（含未保存的改动），
    /// 而非 SchemaEntry::enabled——后者是 refresh 时的快照，toggle 后不更新。
    fn is_enabled(&self, id: &str) -> bool {
        schema_is_enabled(&self.enabled_order.borrow(), id)
    }

    /// 选中项能否「设为当前」：有选中 · 已启用 · 且尚不是当前方案。
    pub fn can_set_active(&self) -> bool {
        can_activate(
            &self.enabled_order.borrow(),
            &self.active_id.borrow(),
            self.selected_id.borrow().as_deref(),
        )
    }

    /// core 侧活跃方案已变更（事件驱动，非本端发起）：只同步显示。
    ///
    /// 刻意不复用 [`set_active`]——那会再往 core 发一次 setActive，形成回环。
    /// 本端自己点「设为当前」时 core 也会广播回来，此时 active_id 已是新值，
    /// 首行判断直接短路，天然幂等。
    pub fn set_active_from_core(&self, id: &str) {
        if self.is_active(id) {
            return;
        }
        *self.active_id.borrow_mut() = id.to_string();
        self.bump_active();
        self.rebuild_list();
    }

    /// 将选中方案设为当前（工具栏按钮）。
    pub fn set_active_selected(&self) {
        let Some(id) = self.selected_id.borrow().clone() else {
            return;
        };
        self.set_active(&id);
    }

    /// 将指定方案设为当前输入方案：**即时生效 + 随保存持久化**（双写）。
    ///
    /// 双写是必需的，不是保险：core 的 `EngineManager::switch_schema` 只改运行时的
    /// 活跃方案，**不写配置**；而「应用设置」触发的 `reload_from_config` 会用配置里的
    /// `schema.active` 重置活跃方案。只调 RPC 的话，用户设为当前后再保存一次设置，
    /// 活跃方案就会跳回去。故此处 RPC 让它立刻生效，同时更新 `active_id`——
    /// state.rs 注册的 writer 会在保存时把它写进 `schema.active`。
    pub fn set_active(&self, schema_id: &str) {
        if self.is_active(schema_id) {
            return;
        }
        if !self.is_enabled(schema_id) {
            self.notify(ToastKind::Error, "请先启用该方案，再设为当前");
            return;
        }
        match rpc::call_core("schema.setActive", &json!({ "id": schema_id })) {
            Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => {
                *self.active_id.borrow_mut() = schema_id.to_string();
                self.bump_active();
                self.rebuild_list(); // 「当前」徽章跟随移动
                self.notify(ToastKind::Success, "已切换当前方案");
            }
            // ok=false：core 未能加载该方案（如方案文件损坏，或启用状态尚未保存到
            // 配置、core 侧还不认这个方案）。
            Ok(_) => self.notify(
                ToastKind::Error,
                "切换失败：方案未能加载，若刚启用请先「应用设置」",
            ),
            Err(e) => self.notify(ToastKind::Error, format!("切换失败: {e}")),
        }
    }

    /// 该 ID 是否为当前选中（行高亮用）。
    pub fn is_selected(&self, id: &str) -> bool {
        self.selected_id.borrow().as_deref() == Some(id)
    }

    /// 当前选中的方案条目。
    pub fn selected_entry(&self) -> Option<SchemaEntry> {
        let id = self.selected_id.borrow().clone()?;
        self.schemas.borrow().iter().find(|e| e.id == id).cloned()
    }

    /// 选中项在启用区的位置 (idx, total)；未选中或未启用返回 None。
    /// enabled_order 为空表示「全部启用」，按 schemas 原始序折算。
    pub fn selected_enabled_pos(&self) -> Option<(usize, usize)> {
        let id = self.selected_id.borrow().clone()?;
        let order = self.enabled_order.borrow();
        if order.is_empty() {
            let schemas = self.schemas.borrow();
            let idx = schemas.iter().position(|e| e.id == id)?;
            return Some((idx, schemas.len()));
        }
        let idx = order.iter().position(|x| *x == id)?;
        Some((idx, order.len()))
    }

    /// 选中项是否为用户安装（删除按钮可用条件）。
    pub fn selected_is_user(&self) -> bool {
        self.selected_entry().is_some_and(|e| e.source == "user")
    }

    /// 上/下移选中方案。enabled_order 为空（全部启用）时先展开为全部 ID，
    /// 与 toggle_schema_enabled 的展开策略一致，保证重排有承载体。
    pub fn move_selected(&self, up: bool) {
        let Some(id) = self.selected_id.borrow().clone() else {
            return;
        };
        {
            let mut order = self.enabled_order.borrow_mut();
            if order.is_empty() {
                *order = self.schemas.borrow().iter().map(|e| e.id.clone()).collect();
            }
        }
        if up {
            self.move_schema_up(&id);
        } else {
            self.move_schema_down(&id);
        }
    }

    /// 打开选中方案的设置对话框。
    pub fn open_settings_selected(&self) {
        let Some(id) = self.selected_id.borrow().clone() else {
            return;
        };
        self.open_settings_at_id(&id);
    }

    /// 请求删除选中方案（弹确认框）。
    pub fn request_delete_selected(&self) {
        let Some(id) = self.selected_id.borrow().clone() else {
            return;
        };
        self.request_delete_at_id(&id);
    }

    /// 操作反馈：弹单条 toast（经页面挂载的 toast_sink）。
    fn notify(&self, kind: ToastKind, msg: impl Into<String>) {
        self.feedback.set(Some((kind, msg.into())));
    }

    /// 上移指定方案在 enabled_order 中的位置（立即刷新列表）。
    pub fn move_schema_up(&self, schema_id: &str) {
        {
            let mut order = self.enabled_order.borrow_mut();
            if let Some(idx) = order.iter().position(|id| id == schema_id) {
                if idx > 0 {
                    order.swap(idx, idx - 1);
                }
            }
        }
        self.notify(ToastKind::Success, "排序已调整（保存后生效）");
        self.rebuild_list();
    }

    /// 下移指定方案在 enabled_order 中的位置（立即刷新列表）。
    pub fn move_schema_down(&self, schema_id: &str) {
        {
            let mut order = self.enabled_order.borrow_mut();
            if let Some(idx) = order.iter().position(|id| id == schema_id) {
                if idx + 1 < order.len() {
                    order.swap(idx, idx + 1);
                }
            }
        }
        self.notify(ToastKind::Success, "排序已调整（保存后生效）");
        self.rebuild_list();
    }

    /// 切换方案启用状态（更新 enabled_order，立即刷新列表）。
    /// 禁用最后一个启用中的方案会被拒绝（返回 false，调用方回滚复选框）。
    /// 切换「显示特殊方案」。列表数据本身已含特殊方案（refresh 走 includeHidden），
    /// 这里只需按新值重建 items——过滤在 `rebuild_list` 里做，分节计数才跟得上。
    pub fn set_show_hidden(&self, on: bool) {
        self.show_hidden.set(on);
        self.rebuild_list();
    }

    pub fn toggle_schema_enabled(&self, schema_id: &str, new_enabled: bool) -> bool {
        if new_enabled {
            let mut order = self.enabled_order.borrow_mut();
            if !order.contains(&schema_id.to_string()) {
                order.push(schema_id.to_string());
            }
        } else {
            // 守护：当前方案不可禁用。放在展开/计数守护之前——它的成因更具体，
            // 提示也更有指导性（告诉用户该先做什么，而不是笼统说"至少留一个"）。
            if self.is_active(schema_id) {
                self.notify(ToastKind::Error, "当前方案不可禁用，请先将其他方案设为当前");
                return false;
            }
            let mut order = self.enabled_order.borrow_mut();
            // 若当前全部启用（order 为空），需先展开为全部 ID 再逐一 retain
            if order.is_empty() {
                let all_ids: Vec<String> =
                    self.schemas.borrow().iter().map(|e| e.id.clone()).collect();
                *order = all_ids;
            }
            // 守护：至少保留一个启用方案（否则 order 变空又回到「全部启用」语义，
            // 表现为"禁用最后一个反而全开"）。
            if order.len() <= 1 {
                drop(order);
                self.notify(ToastKind::Error, "至少需保留一个启用的方案");
                return false;
            }
            order.retain(|id| id != schema_id);
        }
        self.notify(
            ToastKind::Success,
            if new_enabled {
                "已启用（保存后生效）"
            } else {
                "已禁用（保存后生效）"
            },
        );
        self.rebuild_list();
        true
    }

    /// 打开指定 ID 方案的设置对话框。双拼方案额外经 schema.getConfig 拉取
    /// 当前布局，展示「双拼布局」下拉（保存走 schema.saveConfig 稀疏 override）。
    pub fn open_settings_at_id(&self, schema_id: &str) {
        let entry = {
            let schemas = self.schemas.borrow();
            let Some(e) = schemas.iter().find(|e| e.id == schema_id) else {
                return;
            };
            e.clone()
        };

        // 方案配置（三层合并结果）；失败按空对象处理（布局下拉回退首项）。
        let cfg = rpc::call_core("schema.getConfig", &json!({ "id": schema_id }))
            .unwrap_or_else(|_| json!({}));
        let is_sp = entry.scheme == "shuangpin";
        if is_sp {
            let layout = cfg
                .pointer("/engine/pinyin/shuangpin/layout")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let idx = self
                .layouts
                .borrow()
                .iter()
                .position(|(id, _)| id == layout)
                .unwrap_or(0);
            self.settings_layout.set(idx);
            self.settings_layout_at_open.set(idx);
        }
        *self.settings_schema_id.borrow_mut() = entry.id.clone();
        *self.settings_cfg.borrow_mut() = cfg;
        self.settings_is_shuangpin.set(is_sp);
        *self.settings_entry.borrow_mut() = Some(entry);

        // 直达热键 / 引导键工作态按当前方案播种（对话框复用同一组信号，必须逐个重置）。
        let sid = self.settings_schema_id.borrow().clone();
        let bound = self.hotkey_of(&sid);
        self.seed_hotkey_editor(&bound);
        self.seed_trigger_keys(&sid);

        // ── 计算配置分节（可扩展体系）：Header 恒为首节，其后按方案类型追加配置节 ──
        let mut sections: Vec<SettingsSection> = vec![SettingsSection::Header];
        // 除「进入方式」外还有没有别的配置节——只影响是否显示「暂无其它可配置项」。
        // 进入方式：所有方案都有（切到该方案的直达热键），故紧随 Header。
        sections.push(SettingsSection::EntryKeys);
        if is_sp {
            sections.push(SettingsSection::Divider);
            sections.push(SettingsSection::ShuangpinLayout);
        }
        // 码表配置：码表方案才有（含快符这类特殊方案——它们本质就是码表方案，
        // 只是靠引导键进入）。混输方案暂不给：它的码表部分与拼音部分怎么分账是另一件事，
        // core 侧 `effectiveCodetable` 也只对 codetable 类给出。
        {
            let cfg = self.settings_cfg.borrow();
            let is_ct = cfg.pointer("/engine/type").and_then(|v| v.as_str()) == Some("codetable");
            if is_ct {
                let eff = cfg.get("effectiveCodetable").cloned().unwrap_or(json!({}));
                self.settings_ct.seed(&self.manifest, &eff);
                self.settings_ct_plan.set(self.settings_ct.plan());
                let custom = crate::dialogs::schema_codetable::has_custom(&cfg);
                self.settings_ct_enabled.set(custom);
                self.settings_ct_enabled_at_open.set(custom);
                *self.settings_ct_eff.borrow_mut() = eff;
                drop(cfg);
                // 标题带方案名（名称缺失时回退 id——总比只显示「码表配置」强）。
                let who = self
                    .settings_entry
                    .borrow()
                    .as_ref()
                    .map(|e| e.name.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| sid.clone());
                self.settings_ct_title.set(format!("码表配置 - {who}"));
                self.settings_ct_disp.set(
                    ct_summary(self.settings_ct_enabled.get(), self.settings_entry_hidden())
                        .to_string(),
                );
                sections.push(SettingsSection::Divider);
                sections.push(SettingsSection::CodeTable);
            }
        }
        // 按键功能表：所有方案都有（按键功能与引擎类型无关）。放在码表配置之后、
        // 扩展词库之前——它同属「这个方案怎么用」，而词库是「这个方案有什么内容」。
        {
            self.settings_ka.set_verbs(self.build_key_action_verbs());
            self.settings_ka.seed(&self.settings_cfg.borrow());
            let who = self
                .settings_entry
                .borrow()
                .as_ref()
                .map(|e| e.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| sid.clone());
            self.settings_ka_title.set(format!("按键功能 - {who}"));
            self.settings_ka_disp.set(self.settings_ka.summary());
            // 不单独成节：内容并进 EntryKeys 那节（组名已改「按键设置」）——
            // 「切换热键 / 引导键 / 按键绑定」三行讲的是同一件事，分成两个组标题
            // 只会让人以为它们互不相干。
        }
        let dict_infos = parse_extension_dicts(&self.settings_cfg.borrow());
        let rows: Vec<DictRow> = dict_infos
            .into_iter()
            .map(|d| DictRow {
                id: d.id,
                label: d.label,
                description: d.description,
                initial: d.enabled,
                on: signal(d.enabled),
            })
            .collect();
        if !rows.is_empty() {
            sections.push(SettingsSection::Divider);
            sections.push(SettingsSection::ExtensionDicts);
        }
        *self.settings_dicts.borrow_mut() = rows;
        // 注：曾有 `has_config` 与 `SettingsSection::Empty`——「没有任何可配置项」的空状态。
        // 「按键功能」对所有方案都有之后，那个分支恒不可达，故一并退役（枚举项保留，
        // 将来若又出现真的空方案可直接复用）。
        self.settings_sections.set(sections);

        self.settings_visible.set(true);
    }

    /// 当前打开的方案是否为特殊方案（决定摘要说「跟随内置基线」还是「跟随全局配置」）。
    fn settings_entry_hidden(&self) -> bool {
        self.settings_entry
            .borrow()
            .as_ref()
            .map(|e| e.hidden)
            .unwrap_or(false)
    }

    /// 把对话框的工作态**暂存**进待提交队列（关窗时调用）。
    ///
    /// 不在这里落盘：方案 override 与 config.toml 里的热键/引导键属于同一次编辑，
    /// 分两个时机写会让用户看到「一半存了一半没存」。统一由「应用设置 / 保存并退出」
    /// 提交，见 [`Self::commit_pending_edits`] 与 `state::SideCommitter`。
    ///
    /// 无改动则不入队——否则打开看一眼再关掉也会让整个设置界面变成「有未保存改动」。
    pub fn stage_settings_edits(&self) {
        let id = self.settings_schema_id.borrow().clone();
        if id.is_empty() {
            return;
        }

        // 扩展词库：仅相对打开时发生变化的行。
        let dicts: Vec<(String, bool)> = self
            .settings_dicts
            .borrow()
            .iter()
            .filter(|r| r.on.get() != r.initial)
            .map(|r| (r.id.clone(), r.on.get()))
            .collect();

        // 方案配置（双拼布局 + 码表配置）：两者必须写进**同一份 cfg**。core 那边是
        // 「整份 cfg 与方案文件 diff，结果全量重写 override」——分两次提交的话，
        // 后一次会拿自己那份 cfg 的 diff 把前一次的结果整个覆盖掉。
        let mut cfg = self.settings_cfg.borrow().clone();
        if !cfg.is_object() {
            cfg = json!({});
        }
        let mut cfg_changed = false;

        if cfg.pointer("/engine/type").and_then(|v| v.as_str()) == Some("codetable") {
            let enabled = self.settings_ct_enabled.get();
            // 脏判定要在 apply 之前：apply 会把所有字段写实，之后再比就永远「有改动」。
            if enabled != self.settings_ct_enabled_at_open.get() || self.settings_ct.is_dirty() {
                // 关掉总开关时写入的是一串 null——core 的 json_to_toml 跳过 null，于是
                // override 里不含这些键 = 跟随基线。这是「取消自定义」唯一的落地方式。
                self.settings_ct.apply(&mut cfg, enabled);
                if enabled {
                    let eff = self.settings_ct_eff.borrow().clone();
                    self.settings_ct.apply_hidden(&mut cfg, &eff);
                }
                cfg_changed = true;
            }
        }

        // 按键功能表：写进同一份 cfg（理由同上——core 是整份 diff 后全量重写 override）。
        // 无总开关，脏判定即内容比对；空表也要写，那是「删光所有绑定」的落地方式。
        if self.settings_ka.is_dirty() {
            self.settings_ka.apply(&mut cfg);
            cfg_changed = true;
        }

        if self.settings_is_shuangpin.get()
            && self.settings_layout.get() != self.settings_layout_at_open.get()
            && !self.layouts.borrow().is_empty()
        {
            let layout = {
                let layouts = self.layouts.borrow();
                let idx = self.settings_layout.get().min(layouts.len() - 1);
                layouts[idx].0.clone()
            };
            set_json_path(
                &mut cfg,
                &["engine", "pinyin", "shuangpin", "layout"],
                json!(layout),
            );
            cfg_changed = true;
        }

        if dicts.is_empty() && !cfg_changed {
            return;
        }
        merge_pending(
            &mut self.pending_edits.borrow_mut(),
            &id,
            cfg_changed.then_some(cfg),
            dicts,
        );
    }

    /// 是否有待提交的方案 override 改动（供主保存的 `is_dirty` 汇总）。
    pub fn has_pending_edits(&self) -> bool {
        !self.pending_edits.borrow().is_empty()
    }

    /// 提交待提交队列（由主保存调用）。
    pub fn commit_pending_edits(&self) -> Result<(), String> {
        drain_pending(&self.pending_edits, |id, edit| {
            for (dict_id, want) in &edit.dicts {
                rpc::call_core(
                    "schema.setDictEnabled",
                    &json!({ "id": id, "dictId": dict_id, "enabled": *want }),
                )
                .map_err(|e| format!("扩展词库保存失败（{id}）: {e}"))?;
            }
            if let Some(cfg) = &edit.cfg {
                rpc::call_core("schema.saveConfig", &json!({ "id": id, "cfg": cfg }))
                    .map_err(|e| format!("方案配置保存失败（{id}）: {e}"))?;
            }
            Ok(())
        })?;
        // 行名可能含双拼布局后缀，提交后重拉列表刷新显示（refresh 保留选中）。
        self.refresh();
        Ok(())
    }

    /// 丢弃待提交队列（「重新加载」时调用——那是明确的「丢弃未保存改动」操作）。
    pub fn clear_pending_edits(&self) {
        self.pending_edits.borrow_mut().clear();
    }

    /// 请求删除指定 ID 方案（弹出确认对话框）。
    pub fn request_delete_at_id(&self, schema_id: &str) {
        let schemas = self.schemas.borrow();
        if let Some(entry) = schemas.iter().find(|e| e.id == schema_id) {
            if entry.source == "builtin" {
                self.notify(ToastKind::Error, "内置方案不可删除");
                return;
            }
            let display_name = if entry.name.is_empty() {
                entry.id.clone()
            } else {
                entry.name.clone()
            };
            *self.confirm_id.borrow_mut() = entry.id.clone();
            self.confirm_msg.set(format!(
                "确定要删除方案「{display_name}」吗？\n将同时删除其词库数据与未被其它方案共享的资源文件，不可恢复。"
            ));
            self.confirm_visible.set(true);
        }
    }

    pub fn do_delete(&self) {
        let id = self.confirm_id.borrow().clone();
        self.confirm_visible.set(false);
        if id.is_empty() {
            return;
        }
        match rpc::call_core("schema.delete", &json!({ "id": id })) {
            Ok(v) => {
                // refresh 内会发现选中 ID 已消失并清空选中。
                self.refresh();
                let n = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0)
                };
                let (deleted, kept) = (n("deleted"), n("keptShared"));
                let msg = if kept > 0 {
                    format!("方案已删除（清理 {deleted} 个文件，共享保留 {kept} 个）")
                } else if deleted > 0 {
                    format!("方案已删除（清理 {deleted} 个文件）")
                } else {
                    "方案已删除".to_string()
                };
                self.notify(ToastKind::Success, msg);
            }
            Err(e) => self.notify(ToastKind::Error, format!("删除失败: {e}")),
        }
    }

    // ── 方案包导入导出（scheme.exportPackage/previewImport/importPackage）──

    /// 导出选中方案为方案包 zip。对话框经 `ctx.request_save_file` 异步请求
    /// （禁止在点击回调里同步弹模态框，见 dict::do_export_phrases 注释）。
    pub fn do_export_selected(&self, ctx: &mut EventCtx) {
        let Some(entry) = self.selected_entry() else {
            self.notify(ToastKind::Error, "请先选中方案");
            return;
        };
        let state = self.clone();
        // 文件名带方案版本便于区分（schema.toml 未标版本时省略）。
        let file_name = if entry.version.is_empty() {
            format!("{}.zip", entry.id)
        } else {
            format!("{}-{}.zip", entry.id, entry.version)
        };
        ctx.request_save_file(
            PickDialog::new()
                .title("导出方案包")
                .file_name(&file_name)
                .filter("方案包", &["zip"]),
            move |path| {
                let Some(path) = path else { return };
                match rpc::call_core(
                    "scheme.exportPackage",
                    &json!({ "id": entry.id, "path": path.to_string_lossy() }),
                ) {
                    Ok(v) => {
                        let n = |k: &str| {
                            v.get(k)
                                .and_then(|x| x.as_array())
                                .map(|a| a.len())
                                .unwrap_or(0)
                        };
                        let (packed, missing) = (n("packed"), n("missing"));
                        let msg = if missing > 0 {
                            format!("已导出 {packed} 个文件（缺失引用 {missing} 个）")
                        } else {
                            format!("已导出 {packed} 个文件")
                        };
                        state.notify(ToastKind::Success, msg);
                    }
                    Err(e) => state.notify(ToastKind::Error, format!("导出失败: {e}")),
                }
            },
        );
    }

    /// 导入方案包：选 zip → `scheme.previewImport` 预览 → 确认对话框选合并/替换。
    pub fn do_import_package(&self, ctx: &mut EventCtx) {
        let state = self.clone();
        ctx.request_pick_file(
            PickDialog::new()
                .title("导入方案包")
                .filter("方案包", &["zip"]),
            move |path| {
                let Some(path) = path else { return };
                match rpc::call_core(
                    "scheme.previewImport",
                    &json!({ "path": path.to_string_lossy() }),
                ) {
                    Ok(v) => {
                        state
                            .import_preview_msg
                            .set(Self::format_import_preview(&v));
                        *state.pending_import_path.borrow_mut() = Some(path);
                        state.import_confirm_visible.set(true);
                    }
                    Err(e) => state.notify(ToastKind::Error, format!("预览失败: {e}")),
                }
            },
        );
    }

    /// previewImport 响应 → 确认对话框正文（纯函数，便于单测）。
    /// v2 契约：元信息在可选 `package`（package.toml 缺失时各字段为空串）。
    fn format_import_preview(v: &Value) -> String {
        let n = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_array())
                .map(|a| a.len())
                .unwrap_or(0)
        };
        let pkg = v.get("package");
        let s = |a: &str, b: &str| {
            pkg.and_then(|x| x.get(a))
                .and_then(|x| x.get(b))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string()
        };
        let (id, version) = (s("schema", "id"), s("schema", "version"));
        let head = match (id.is_empty(), version.is_empty()) {
            (false, false) => format!("方案 {id} v{version}"),
            (false, true) => format!("方案 {id}"),
            _ => "方案包".to_string(),
        };
        let created = s("package", "created_at");
        let created = if created.is_empty() {
            "未知".to_string()
        } else {
            created
        };
        format!(
            "{head} · 创建于 {created}\n新增文件 {} · 已存在 {} · 系统引用 {} · 缺失 {}\n\n\
             合并跳过已存在文件；替换覆盖同名文件。",
            n("willAdd"),
            n("conflicts"),
            n("systemRefs"),
            n("missing"),
        )
    }

    /// 确认导入（strategy: "merge" | "replace"）。
    pub fn confirm_import(&self, strategy: &str) {
        self.import_confirm_visible.set(false);
        let Some(path) = self.pending_import_path.borrow_mut().take() else {
            return;
        };
        match rpc::call_core(
            "scheme.importPackage",
            &json!({ "path": path.to_string_lossy(), "strategy": strategy }),
        ) {
            Ok(v) => {
                let n = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0)
                };
                let (imported, conflicts) = (n("imported"), n("conflicts"));
                self.refresh();
                let msg = if conflicts > 0 {
                    format!("已导入 {imported} 个文件（跳过已存在 {conflicts} 个）")
                } else {
                    format!("已导入 {imported} 个文件")
                };
                self.notify(ToastKind::Success, msg);
            }
            Err(e) => self.notify(ToastKind::Error, format!("导入失败: {e}")),
        }
    }

    pub fn cancel_import(&self) {
        *self.pending_import_path.borrow_mut() = None;
        self.import_confirm_visible.set(false);
    }
}

// ──────────────────────── 启用/当前 判定（纯函数） ────────────────────────

/// 方案是否启用。**空 order 表示「全部启用」**——这是全仓一致的语义
/// （见 `rebuild_list` / `toggle_schema_enabled`），漏掉它会让首次加载、
/// 配置为空时所有方案都被判成未启用。
fn schema_is_enabled(order: &[String], id: &str) -> bool {
    order.is_empty() || order.iter().any(|x| x == id)
}

/// 选中项能否「设为当前」：有选中 · 已启用 · 且尚不是当前方案。
fn can_activate(order: &[String], active: &str, selected: Option<&str>) -> bool {
    let Some(id) = selected else { return false };
    id != active && schema_is_enabled(order, id)
}

// ──────────────────────── 显示辅助 ────────────────────────

/// 沿路径逐层确保对象存在后写入叶子值（中间层非对象时覆盖为对象；根非对象则不写）。
fn set_json_path(v: &mut Value, path: &[&str], leaf: Value) {
    let Some((last, mids)) = path.split_last() else {
        return;
    };
    let mut cur = v;
    for k in mids {
        let Some(obj) = cur.as_object_mut() else {
            return;
        };
        let slot = obj.entry((*k).to_string()).or_insert_with(|| json!({}));
        if !slot.is_object() {
            *slot = json!({});
        }
        cur = slot;
    }
    if let Some(o) = cur.as_object_mut() {
        o.insert((*last).to_string(), leaf);
    }
}

/// 把一次热键编辑落到映射表上（纯函数，供测试）。
///
/// **未绑定 = 表里没有这一条**，而不是一条空串。写空串会在 config.toml 里留下
/// `方案id = ""` 的死条目：core 侧 `parse_hotkey` 确实会忽略它，但用户翻开配置
/// 看见自己的方案名挂在那儿，只会以为设过什么却没生效。
///
/// `raw` 为空（用户在对话框里把主键选成「（未设置）」）同样按解绑处理——
/// 只看 `enabled` 会写进一条 `"none"` 或空串。
fn apply_hotkey_edit(
    map: &mut std::collections::HashMap<String, String>,
    schema_id: &str,
    enabled: bool,
    raw: &str,
) {
    if schema_id.is_empty() {
        return;
    }
    if enabled && !raw.is_empty() && raw != "none" {
        map.insert(schema_id.to_string(), raw.to_string());
    } else {
        map.remove(schema_id);
    }
}

pub fn engine_label(t: &str) -> &'static str {
    match t {
        "codetable" => "码表",
        "pinyin" => "拼音",
        "mixed" => "混输",
        "english" => "英文",
        _ => "未知",
    }
}

/// 附加词库解析结果（纯数据，供 UI 层包装为响应式行）。
pub struct DictInfo {
    pub id: String,
    pub label: String,
    pub description: String,
    pub enabled: bool,
}

/// 从方案配置 `cfg["dictionaries"]` 解析可开关的附加词库（过滤主库）。
/// 生效值：enabled.unwrap_or(default_enabled.unwrap_or(true))。
fn parse_extension_dicts(cfg: &Value) -> Vec<DictInfo> {
    let Some(arr) = cfg.get("dictionaries").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter(|d| !d.get("default").and_then(|v| v.as_bool()).unwrap_or(false))
        .map(|d| {
            let default_enabled = d
                .get("default_enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let enabled = d
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(default_enabled);
            DictInfo {
                id: d
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                label: d
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                description: d
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                enabled,
            }
        })
        .collect()
}

// ──────────────────────── 内联复合列表 ────────────────────────

/// 构建内联方案列表，供 `scheme_page` 直接嵌入卡片。
///
/// 使用 `Element::host_signal`（非滚动的响应式 col）驱动：启用/禁用/排序变更后
/// list_data 更新，列表即时重建。列表**按自然高度展开**，随外层页面滚动一起滚动
/// ——刻意不用 `list_signal`（自带固定高度滚动容器）：设置页本身已是滚动容器，
/// 内嵌固定高度的内层滚动会造成"滚轮在内外层间打架、列表滚不动"，且内层滚动条
/// 压在卡片行上遮挡内容。改为自然高度后只保留页面这一层滚动，两个问题一并消除。
pub fn build_schema_inline_list(state: &SchemaManagerState) -> Element {
    let mgr = state.clone();
    let hide_sig = state.hide_disabled;

    Element::host_signal(state.list_data, move |item| match item {
        SchemaListItem::Row { entry, is_enabled } => build_compound_row(entry, is_enabled, &mgr),
        SchemaListItem::SectionHeader {
            title,
            hidden_with_disabled,
        } => {
            let el = Element::col()
                .width_match()
                .child(Element::divider())
                .child(
                    Element::row().width_match().padding_xy(8, 5).child(
                        Element::label(title)
                            .font_size(10.5)
                            .fg_role(Role::TextMuted)
                            .width_match()
                            .max_lines(1),
                    ),
                );
            if hidden_with_disabled {
                el.visible_when(move || !hide_sig.get())
            } else {
                el
            }
        }
        SchemaListItem::Divider {
            hidden_with_disabled,
        } => {
            let d = Element::divider();
            if hidden_with_disabled {
                d.visible_when(move || !hide_sig.get())
            } else {
                d
            }
        }
    })
    .width_match()
}

/// 构建单个方案复合行（工具栏+选中模式：行内无操作按钮）。
///
/// 行整体 clickable → 点击选中（事件沿祖先链派发，点 checkbox 会同时切换启用并选中，
/// 符合预期）；选中视觉 = stack 底层 accent 淡底 + 左缘指示条覆盖层（与侧栏导航同语言），
/// visible_when 轮询 `sel_ver`。行内保留：启用复选 · 名称/描述 · 类型/来源徽章 · 版本。
fn build_compound_row(entry: SchemaEntry, is_enabled: bool, mgr: &SchemaManagerState) -> Element {
    let enabled_sig = signal(is_enabled);

    // ── Checkbox ──
    // 特殊方案（英文/快符）没有「启用」这回事：它们不进 available、不参与循环切换，
    // 只靠引导键或直达热键进入。给它们一个勾选框会凭空造出一个不存在的状态，用户勾了
    // 也不知道自己勾出了什么。占位保持与上方各行的列对齐（宽度取自 small checkbox）。
    let checkbox = if entry.hidden {
        Element::leaf().width(18).height(18)
    } else {
        let mgr_toggle = mgr.clone();
        let toggle_id = entry.id.clone();
        Element::checkbox("", enabled_sig)
            .small()
            .on_toggle(move |_| {
                let new_val = !enabled_sig.get();
                // 被拒绝（最后一个启用项不可禁用）时不写 signal，勾选态保持不变。
                if mgr_toggle.toggle_schema_enabled(&toggle_id, new_val) {
                    enabled_sig.set(new_val);
                }
            })
    };

    // ── 名称列（weight-1）：名称 + 可选描述；双拼方案名称后缀当前布局（如「双拼·小鹤双拼」）──
    let base_name = if entry.name.is_empty() {
        entry.id.clone()
    } else {
        entry.name.clone()
    };
    let name_text = if entry.scheme == "shuangpin" && !entry.shuangpin_layout.is_empty() {
        format!("{base_name}·{}", mgr.layout_label(&entry.shuangpin_layout))
    } else {
        base_name
    };
    let name_fg = if is_enabled {
        Role::Text
    } else {
        Role::TextMuted
    };
    let name_label = Element::label(name_text)
        .font_size(13.0)
        .font_weight(600)
        .fg_role(name_fg)
        .width_match()
        .max_lines(1)
        .truncate(Truncate::End);

    let sub_text = if !entry.description.is_empty() {
        entry.description.clone()
    } else if !entry.author.is_empty() {
        format!("作者：{}", entry.author)
    } else {
        String::new()
    };

    let name_col = if sub_text.is_empty() {
        Element::col().weight(1.0).child(name_label)
    } else {
        Element::col()
            .weight(1.0)
            .spacing(2)
            .child(name_label)
            .child(
                Element::label(sub_text)
                    .font_size(11.0)
                    .fg_role(Role::TextMuted)
                    .width_match()
                    .max_lines(1)
                    .truncate(Truncate::End),
            )
    };

    // ── 「当前」徽章：仅活跃方案显示，accent 实心（与 outline 的类型徽章拉开层次，
    //    一眼能从整列里挑出来）。列表由 rebuild_list 整体重建，故无需响应式包装。
    let current_badge = mgr.is_active(&entry.id).then(|| {
        Element::label("当前")
            .font_size(10.5)
            .font_weight(600)
            .fg_role(Role::OnAccent)
            .padding_xy(6, 2)
            .corner(4.0)
            .bg_role(Role::Accent)
    });

    // ── 类型徽章（outline 风格，直接子节点保证垂直居中）──
    let type_badge = Element::label(engine_label(&entry.engine_type))
        .font_size(10.5)
        .fg_role(Role::Accent)
        .padding_xy(6, 2)
        .corner(4.0)
        .border_role(Role::Accent, 1);

    // ── 来源徽章 ──
    let source_badge = Element::label(if entry.source == "user" {
        "用户"
    } else {
        "内置"
    })
    .font_size(10.5)
    .fg_role(Role::TextMuted)
    .padding_xy(5, 2)
    .corner(4.0)
    .bg_role(Role::SurfaceAlt)
    .border_role(Role::Border, 1);

    // ── 行内容：点击选中 ──
    let mgr_sel = mgr.clone();
    let sel_id = entry.id.clone();
    let mut row = Element::row()
        .clickable()
        .on_click(move |_| mgr_sel.select(&sel_id))
        .width_match()
        .cross(Align::Center)
        .padding_xy(10, 8)
        .spacing(6)
        .corner(8.0)
        .child(checkbox)
        .child(name_col);

    if let Some(badge) = current_badge {
        row = row.child(badge);
    }

    row = row.child(type_badge).child(source_badge);

    if !entry.version.is_empty() {
        row = row.child(
            Element::label(entry.version.clone())
                .font_size(10.5)
                .fg_role(Role::TextMuted),
        );
    }

    // ── 选中视觉：淡蓝底 + accent 整条描边（置于行下，行 clickable 吞命中不受影响）
    //    + 左缘指示条（无背景穿透容器包裹，垂直居中）。
    //    stack 子节点铺满须用 .align(Align::Stretch)（Frame 布局只认 align，
    //    width/height_match 在此测不出父尺寸——上一版指示条不居中即此因）。
    let sel_when = |m: &SchemaManagerState, id: &str| {
        let m = m.clone();
        let id = id.to_string();
        move || {
            m.sel_ver.get();
            m.is_selected(&id)
        }
    };
    let sel_bg = Element::leaf()
        .align(Align::Stretch)
        .corner(8.0)
        .bg(Color::hex(crate::theme_def::ACCENT_LIGHT).scale_alpha(0.08))
        .visible_when(sel_when(mgr, &entry.id));
    let sel_border = Element::leaf()
        .align(Align::Stretch)
        .corner(8.0)
        .border_role(Role::Accent, 1)
        .visible_when(sel_when(mgr, &entry.id));
    let indicator = Element::row()
        .align(Align::Stretch)
        .cross(Align::Center)
        .child(
            Element::leaf()
                .width(3)
                .height(16)
                .corner(1.5)
                .bg_role(Role::Accent),
        )
        .visible_when(sel_when(mgr, &entry.id));

    // 两侧留 2px：选中描边贴列表容器边缘会被裁/被滚动条压住右缘。
    let mut item = Element::stack()
        .width_match()
        .margin_xy(2, 0)
        .child(sel_bg)
        .child(sel_border)
        .child(row)
        .child(indicator);

    // 未启用行随 hide_disabled 隐藏（list_signal 重建后无需 enabled_sig 联动）。
    // 注：特殊方案的显隐不在这里——它决定的是「进不进列表」，走 rebuild_list 过滤，
    // 否则分节标题的计数会把看不见的行也算进去。
    if !is_enabled {
        let hide_sig = mgr.hide_disabled;
        item = item.visible_when(move || !hide_sig.get());
    }

    item
}

// ──────────────────────── UI 构建 ────────────────────────

/// 构建方案相关浮层对话框：删除确认 + 方案详情/设置。
pub fn build_schema_manager_dialog(state: &SchemaManagerState) -> Element {
    let confirm_dlg = build_confirm_dialog(state);
    let settings_dlg = build_schema_settings_dialog(state);
    let import_dlg = build_import_confirm_dialog(state);
    let hotkey_dlg = build_schema_hotkey_dialog(state);
    let trigger_dlg = build_trigger_keys_dialog(state);
    let ct_dlg = build_codetable_dialog(state);
    let ka_dlg = build_key_actions_dialog(state);
    Element::stack()
        .fill()
        .child(confirm_dlg)
        .child(settings_dlg)
        .child(import_dlg)
        // 排在方案设置之后 = 叠在它上面：这几个对话框都是从方案设置里打开的。
        .child(hotkey_dlg)
        .child(trigger_dlg)
        .child(ct_dlg)
        .child(ka_dlg)
}

/// 方案级按键功能表浮层（第三级）。内容由 `KeyActionFields` 自带响应式列表驱动，
/// 无需像码表配置那样按 plan 重建——行元素绑的是 `Signal<Vec<Row>>`，播种时 `set`
/// 即触发 `host_signal` 重建，天然跟得上换方案。
fn build_key_actions_dialog(state: &SchemaManagerState) -> Element {
    let (content, buttons) = state.settings_ka.build_body();
    crate::dialogs::field_dialogs::build_group_dialog_shell_dyn(
        state.settings_ka_visible,
        state.settings_ka_title,
        Element::col()
            .width_match()
            .padding_xy(16, 6)
            .child(content),
        // 「添加绑定」进底部按钮栏（与「关闭」同排）；放列表下方会随列表一起滚走。
        // 无「保存」：与码表配置同路，改动随方案设置对话框的「保存」一并提交。
        buttons,
    )
}

/// 方案级码表配置浮层（第三级）：与全局「上屏行为」对话框同一份 manifest 定义，
/// 只是读写落点在方案 override 上。见 [`crate::dialogs::schema_codetable`]。
///
/// 内容按 `settings_ct_plan` 响应式重建——对话框结构建于选中方案之前，而控件信号要等
/// 播种；固定构建会让所有方案共用第一次播种的那批控件。
fn build_codetable_dialog(state: &SchemaManagerState) -> Element {
    let ct = state.settings_ct.clone();
    let manifest = state.manifest.clone();
    let body = Element::host_signal(state.settings_ct_plan, move |item| ct.row(&manifest, item));
    crate::dialogs::field_dialogs::build_group_dialog_shell_dyn(
        state.settings_ct_visible,
        state.settings_ct_title,
        Element::col().width_match().padding_xy(16, 2).child(body),
        // 无「保存」：改动随方案设置对话框的「保存」一并提交，与双拼布局、扩展词库同路。
        // 这里再放一个会让人以为两处各存各的。
        Vec::new(),
    )
}

/// 特殊方案的引导键多选浮层（复用清单那套 pairs 对话框）。
///
/// 写回目标是 `schema.special_modes` 里按 `schema` 定位的条目，与「一个 key 一个控件」
/// 的清单模型对不上，故不经 FieldRegistry 注册；落表由 `commit_trigger_keys_edit`
/// → `special_modes` → AppState 注册的 writer 完成。
fn build_trigger_keys_dialog(state: &SchemaManagerState) -> Element {
    use crate::dialogs::field_dialogs::build_pairs_dialog;

    let labels: Vec<String> = TRIGGER_KEY_OPTIONS
        .iter()
        .map(|(_, l)| l.to_string())
        .collect();
    build_pairs_dialog(
        state.settings_trigger_visible,
        "引导键",
        "在中文输入中按下即临时进入本方案，打完一段自动退回。可多选，全部取消即禁用。",
        state.settings_trigger_sigs.clone(),
        labels,
        // 无「恢复默认」可回落：出厂就没有任何引导键（special_modes 是空数组）。
        Vec::new(),
    )
}

/// 方案直达热键的编辑浮层（复用 manifest 那套热键对话框组件）。
///
/// 与清单里的功能热键共用同一个 UI，但**不经 FieldRegistry 注册**——它编辑的是
/// `keys.schema_hotkeys` 里按方案 id 索引的一条，而清单模型是「一个 key 一个控件」。
/// 写回由 `commit_hotkey_edit` → `schema_hotkeys` map → AppState 注册的 writer 完成。
fn build_schema_hotkey_dialog(state: &SchemaManagerState) -> Element {
    use crate::dialogs::field_dialogs::build_hotkey_dialog;
    use crate::manifest::hotkey_keys;

    let keys = hotkey_keys();
    let key_vals: Vec<String> = keys.iter().map(|(v, _)| v.clone()).collect();
    let key_labels: Vec<String> = keys.iter().map(|(_, l)| l.clone()).collect();

    build_hotkey_dialog(
        state.settings_hotkey_visible,
        "方案切换热键",
        state.settings_hotkey_raw,
        state.settings_hotkey_disp,
        // 方案热键出厂即空表，没有「默认键位」可回落，故默认 = 未绑定。
        // 必须传 core 的 sentinel "none" 而不是空串：对话框的「恢复默认」按
        // `default != "none"` 反推启用态，传空串会把「启用」勾亮、而热键显示「未设置」。
        "none".to_string(),
        state.settings_hotkey_mods,
        state.settings_hotkey_key_idx,
        key_vals,
        key_labels,
        // 不提供「全局拦截」勾选：那要写 keys.global_hotkeys，而该列表只认固定的
        // 7 个动作名，塞不进按方案变化的条目。
        None,
        state.settings_hotkey_enabled,
    )
}

/// 方案包导入确认：previewImport 摘要正文 + 合并（默认）/ 替换（danger）二选一。
fn build_import_confirm_dialog(state: &SchemaManagerState) -> Element {
    let merge = state.clone();
    let replace = state.clone();
    let cancel = state.clone();
    let content = flat_panel()
        .width(400)
        .padding(20)
        .spacing(14)
        .child(
            Element::label("导入方案包")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(24)
                .width_match(),
        )
        .child(Element::divider())
        .child(
            Element::label_rc(state.import_preview_msg)
                .font_size(13.0)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(6),
        )
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| cancel.cancel_import()),
                )
                .child(
                    Element::button("替换导入")
                        .small()
                        .danger()
                        .on_click(move |_| replace.confirm_import("replace")),
                )
                .child(
                    Element::button("合并导入")
                        .small()
                        .on_click(move |_| merge.confirm_import("merge")),
                ),
        );
    Element::dialog(state.import_confirm_visible, content)
}

fn build_confirm_dialog(state: &SchemaManagerState) -> Element {
    let st_confirm = state.clone();
    let st_cancel = state.clone();

    let content = flat_panel()
        .width(380)
        .padding(20)
        .spacing(14)
        .child(
            Element::label("确认删除")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(24)
                .width_match(),
        )
        .child(Element::divider())
        .child(
            Element::label_rc(state.confirm_msg)
                .font_size(13.0)
                .fg_role(Role::Text)
                .width_match(),
        )
        .child(
            Element::row()
                .width_match()
                .spacing(8)
                .child(
                    Element::button("取消")
                        .small()
                        .weight(1.0)
                        .outline_soft()
                        .on_click(move |_| st_cancel.confirm_visible.set(false)),
                )
                .child(
                    Element::button("删除")
                        .small()
                        .weight(1.0)
                        .danger()
                        .on_click(move |_| st_confirm.do_delete()),
                ),
        );

    Element::dialog(state.confirm_visible, content)
}

/// 按分节类型构建对话框内的一个配置区块。新增方案设置只需在此加一个 match 分支。
/// 方案设置对话框的结构化头部：标题（名称 + 类型/来源徽章）+ 元信息行 + 描述 + 分隔线。
/// 取代原先 `\n` 拼接塞进单 label 的杂乱纯文本块。
fn build_settings_header(entry: &SchemaEntry) -> Element {
    let name = if entry.name.is_empty() {
        entry.id.clone()
    } else {
        entry.name.clone()
    };

    let type_badge = Element::label(engine_label(&entry.engine_type))
        .font_size(10.5)
        .fg_role(Role::Accent)
        .padding_xy(6, 2)
        .corner(4.0)
        .border_role(Role::Accent, 1);
    let source_badge = Element::label(if entry.source == "user" {
        "用户"
    } else {
        "内置"
    })
    .font_size(10.5)
    .fg_role(Role::TextMuted)
    .padding_xy(6, 2)
    .corner(4.0)
    .bg_role(Role::SurfaceAlt)
    .border_role(Role::Border, 1);

    let title = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        .child(
            Element::label(name)
                .font_size(16.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .weight(1.0)
                .max_lines(1)
                .truncate(Truncate::End),
        )
        .child(type_badge)
        .child(source_badge);

    // 元信息行：版本 · 作者 · ID（仅拼接存在项）。
    let mut parts: Vec<String> = Vec::new();
    if !entry.version.is_empty() {
        parts.push(format!("版本 {}", entry.version));
    }
    if !entry.author.is_empty() {
        parts.push(format!("作者 {}", entry.author));
    }
    parts.push(format!("ID {}", entry.id));
    let meta = Element::label(parts.join("　·　"))
        .font_size(11.0)
        .fg_role(Role::TextMuted)
        .width_match()
        .max_lines(1)
        .truncate(Truncate::End);

    let mut col = Element::col()
        .width_match()
        .spacing(6)
        .child(title)
        .child(meta);
    if !entry.description.is_empty() {
        col = col.child(
            Element::label(entry.description.clone())
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .max_lines(3),
        );
    }
    col.child(Element::divider())
}

/// 逐个方案提交队列，**成功一个移除一个**。
///
/// 中途失败时：已落盘的必须移出队列，否则下次保存会再写一遍；未落盘的必须留着，
/// 否则用户重试时那些改动已经不在了——而他并不知道自己丢了什么。
///
/// 提交动作由调用方传入（真实路径是两个 RPC），本函数只管队列的进出，
/// 好让重试语义能脱离 RPC 单测。
fn drain_pending(
    q: &Rc<RefCell<std::collections::HashMap<String, PendingSchemaEdit>>>,
    mut submit: impl FnMut(&str, &PendingSchemaEdit) -> Result<(), String>,
) -> Result<(), String> {
    // 先取 id 列表：submit 期间不持有 q 的借用，否则 RPC 回调链上任何一处碰到队列即 panic。
    let ids: Vec<String> = q.borrow().keys().cloned().collect();
    for id in ids {
        let Some(edit) = q.borrow().get(&id).cloned() else {
            continue;
        };
        submit(&id, &edit)?;
        q.borrow_mut().remove(&id);
    }
    Ok(())
}

/// 把一次编辑合并进待提交队列。
///
/// **合并而非覆盖**：用户可以先改词库、关窗、再进去改码表配置，两次都得留下；
/// 也可以连着配好几个方案再点「应用设置」，先配的那几个不能被后面的挤掉。
/// 只留最后一份工作态的话，丢失是静默的——用户以为都存了。
fn merge_pending(
    q: &mut std::collections::HashMap<String, PendingSchemaEdit>,
    id: &str,
    cfg: Option<Value>,
    dicts: Vec<(String, bool)>,
) {
    let slot = q.entry(id.to_string()).or_default();
    // cfg 是整份配置快照，后一次天然含前一次的内容，直接替换。
    if cfg.is_some() {
        slot.cfg = cfg;
    }
    // 词库开关是逐条的，同一条以后写为准，不同条累加。
    for (dict_id, want) in dicts {
        match slot.dicts.iter_mut().find(|(d, _)| *d == dict_id) {
            Some(e) => e.1 = want,
            None => slot.dicts.push((dict_id, want)),
        }
    }
}

/// 码表配置行的摘要文案。关着时说清「跟的是谁」——普通方案跟全局，特殊方案跟内置基线
/// （core 不让特殊方案继承全局，见 `codetable_baseline`）。纯函数便于单测。
fn ct_summary(enabled: bool, hidden_schema: bool) -> &'static str {
    match (enabled, hidden_schema) {
        (true, _) => "已自定义",
        (false, true) => "跟随内置基线",
        (false, false) => "跟随全局配置",
    }
}

fn build_section(kind: SettingsSection, state: &SchemaManagerState) -> Element {
    match kind {
        SettingsSection::Header => {
            let entry = state.settings_entry.borrow().clone().unwrap_or_default();
            build_settings_header(&entry)
        }
        SettingsSection::EntryKeys => {
            let entry = state.settings_entry.borrow().clone().unwrap_or_default();
            let disp = state.settings_hotkey_disp;
            let visible = state.settings_hotkey_visible;
            // 行内不写说明：标题「进入方式」+ 行名已经说清了，再挂一行灰色小字只是噪音。
            // 语义细节（切换 vs 临时进入的区别）在文档站。
            let mut col = Element::col()
                .width_match()
                .spacing(4)
                .padding_xy(0, 4)
                .child(crate::manifest::dialog_group_header("按键设置"))
                .child(
                    Element::row()
                        .width_match()
                        .cross(Align::Center)
                        .spacing(10)
                        .child(
                            Element::label("切换热键")
                                .font_size(13.0)
                                .fg_role(Role::Text)
                                .weight(1.0)
                                .max_lines(1),
                        )
                        .child(
                            Element::checkbox("启用", state.settings_hotkey_enabled)
                                .small()
                                .fg_role(Role::Text),
                        )
                        .child(
                            // 固定宽度：长热键文本（如 Ctrl+Shift+\）不该撑宽右侧挤压左列。
                            Element::label_rc(disp)
                                .font_size(12.5)
                                .fg_role(Role::TextMuted)
                                .max_lines(1)
                                .width(114),
                        )
                        .child(
                            Element::button("设置")
                                .small()
                                .outline_soft()
                                .on_click(move |_| visible.set(true)),
                        ),
                );

            // 引导键只给特殊方案：它走 schema.special_modes 的 overlay 机制——按下引导键
            // 临时进入、打完一段自动退回原方案。普通方案没有这条通路（切过去就留在那儿），
            // 给它们显示这一行只会让人以为能配。
            if entry.hidden {
                let tv = state.settings_trigger_visible;
                col = col.child(
                    Element::row()
                        .width_match()
                        .cross(Align::Center)
                        .spacing(10)
                        .child(
                            Element::label("引导键")
                                .font_size(13.0)
                                .fg_role(Role::Text)
                                .weight(1.0)
                                .max_lines(1),
                        )
                        .child(
                            // 摘要由 effect 与勾选同步刷新（见 build_schema_settings_dialog），
                            // 不等 commit 落表——否则用户勾完看不到变化。
                            Element::label_rc(state.settings_trigger_disp)
                                .font_size(12.5)
                                .fg_role(Role::TextMuted)
                                .max_lines(1)
                                .width(114),
                        )
                        .child(
                            Element::button("设置")
                                .small()
                                .outline_soft()
                                .on_click(move |_| tv.set(true)),
                        ),
                );
            }

            // 按键绑定（方案顶层 `key_actions`）：与上面两行同属「这个方案怎么进、
            // 键怎么用」，故并入本节而非另立组标题。所有方案都有。
            let kv = state.settings_ka_visible;
            col = col.child(
                Element::row()
                    .width_match()
                    .cross(Align::Center)
                    .spacing(10)
                    .child(
                        Element::label("本方案的按键绑定")
                            .font_size(13.0)
                            .fg_role(Role::Text)
                            .weight(1.0)
                            .max_lines(1),
                    )
                    .child(
                        Element::label_rc(state.settings_ka_disp)
                            .font_size(12.5)
                            .fg_role(Role::TextMuted)
                            .max_lines(1)
                            .width(114),
                    )
                    .child(
                        Element::button("设置")
                            .small()
                            .outline_soft()
                            .on_click(move |_| kv.set(true)),
                    ),
            );
            col
        }
        SettingsSection::Divider => Element::divider(),
        SettingsSection::ShuangpinLayout => {
            let labels: Vec<String> = state
                .layouts
                .borrow()
                .iter()
                .map(|(_, name)| name.clone())
                .collect();
            // 带分组标题，与「进入方式」「扩展词库」同构——少了它这一节就是节间唯一的
            // 裸行，视觉上像是漏排了。
            Element::col()
                .width_match()
                .spacing(4)
                .padding_xy(0, 4)
                .child(crate::manifest::dialog_group_header("方案参数"))
                .child(
                    Element::row()
                        .width_match()
                        .cross(Align::Center)
                        .spacing(12)
                        .child(
                            Element::label("双拼布局")
                                .font_size(13.0)
                                .fg_role(Role::Text)
                                .weight(1.0)
                                .max_lines(1),
                        )
                        .child(Element::dropdown(labels, state.settings_layout).width(170)),
                )
        }
        SettingsSection::CodeTable => {
            let en = state.settings_ct_enabled;
            let cv = state.settings_ct_visible;
            // 与「进入方式 → 切换热键」逐字同构：勾选框 + 定宽摘要 + 常驻「设置」按钮。
            //
            // 不用 Switch、也不靠显隐表达状态：那样勾上之后摘要消失、按钮出现，同一行的
            // 元素会左右跳一下。摘要定宽 + 按钮常驻（未勾选时置灰）则行宽恒定。
            Element::col()
                .width_match()
                .spacing(4)
                .padding_xy(0, 4)
                .child(crate::manifest::dialog_group_header("码表配置"))
                .child(
                    Element::row()
                        .width_match()
                        .cross(Align::Center)
                        .spacing(10)
                        .child(
                            Element::label("自定义码表配置")
                                .font_size(13.0)
                                .fg_role(Role::Text)
                                .weight(1.0)
                                .max_lines(1),
                        )
                        .child(Element::checkbox("启用", en).small().fg_role(Role::Text))
                        .child(
                            Element::label_rc(state.settings_ct_disp)
                                .font_size(12.5)
                                .fg_role(Role::TextMuted)
                                .max_lines(1)
                                .width(114),
                        )
                        .child(
                            Element::button("设置")
                                .small()
                                .outline_soft()
                                .on_click(move |_| cv.set(true))
                                .enabled_when(move || en.get()),
                        ),
                )
        }
        SettingsSection::ExtensionDicts => {
            let mut col = Element::col()
                .width_match()
                .spacing(4)
                .padding_xy(0, 4)
                .child(crate::manifest::dialog_group_header("扩展词库"));
            for row in state.settings_dicts.borrow().iter() {
                // 默认点击即翻转绑定的 on 信号（暂存）；实际提交在「保存」按钮统一进行。
                let cb = Element::checkbox(row.label.clone(), row.on);
                let mut item = Element::col()
                    .width_match()
                    .spacing(2)
                    .padding_xy(0, 4)
                    .child(cb);
                if !row.description.is_empty() {
                    item = item.child(
                        Element::label(row.description.clone())
                            .font_size(11.0)
                            .fg_role(Role::TextMuted)
                            .width_match()
                            .padding_xy(24, 0)
                            .max_lines(2),
                    );
                }
                col = col.child(item);
            }
            col
        }
        SettingsSection::Empty => Element::label("该方案除进入方式外没有其它可配置项。")
            .font_size(11.0)
            .fg_role(Role::TextMuted)
            .width_match()
            .padding_xy(0, 4),
    }
}

fn build_schema_settings_dialog(state: &SchemaManagerState) -> Element {
    let visible = state.settings_visible;

    // 配置区：按 settings_sections 响应式重建（每次打开对话框按方案类型刷新）；
    // Header 恒为首节，元信息与各配置节统一由此渲染。
    let st_body = state.clone();
    let sections = Element::host_signal(state.settings_sections, move |kind| {
        build_section(kind, &st_body)
    });

    // 热键工作态 → 内存 map 的落盘点。**用 effect 而非在各个「关闭」按钮上调**：
    // 对话框有多条关闭路径（footer 按钮 / 标题栏 / 保存后自动关），逐个挂等于赌自己
    // 数全了；而工作态是所有方案复用的同一组信号，漏一条路径就意味着「配了 A 的热键、
    // 转头打开 B」时 A 的改动被 seed 静默冲掉。
    // raw 与 enabled 都要监听：关掉「启用」不改 raw，只看 raw 会漏掉「解绑」这个操作。
    let st_eff = state.clone();
    let raw_sig = state.settings_hotkey_raw;
    let en_sig = state.settings_hotkey_enabled;
    let hotkey_effect = crate::pages::dict::reactive::reactive_effect(
        move || raw_sig.version().wrapping_add(en_sig.version()),
        move || st_eff.commit_hotkey_edit(),
    );

    // 引导键同理：9 个复选各自独立，版本号求和即可覆盖「任一勾选变化」。
    // 顺带刷新行内摘要——它与勾选同源，放两个 effect 里只会多一次遍历。
    let st_tk = state.clone();
    let tk_sigs = state.settings_trigger_sigs.clone();
    let tk_disp = state.settings_trigger_disp;
    let ver_sigs = state.settings_trigger_sigs.clone();
    let trigger_effect = crate::pages::dict::reactive::reactive_effect(
        move || {
            ver_sigs
                .iter()
                .fold(0u64, |acc, (_, s)| acc.wrapping_add(s.version()))
        },
        move || {
            st_tk.commit_trigger_keys_edit();
            let n = tk_sigs.iter().filter(|(_, s)| s.get()).count();
            tk_disp.set(if n == 0 {
                "未设置".to_string()
            } else {
                format!("已选 {n} 个")
            });
        },
    );

    // 码表配置摘要随勾选刷新。与引导键摘要同理：不等落盘，否则勾完看不到变化。
    let st_ct = state.clone();
    let ct_en = state.settings_ct_enabled;
    let ct_disp = state.settings_ct_disp;
    let ct_effect = crate::pages::dict::reactive::reactive_effect(
        move || ct_en.version(),
        move || ct_disp.set(ct_summary(ct_en.get(), st_ct.settings_entry_hidden()).to_string()),
    );

    // 关窗即把工作态暂存进待提交队列，随「应用设置 / 保存并退出」落盘。
    //
    // 对话框自己不再有「保存」按钮：方案 override 与热键/引导键属于同一次编辑，
    // 分两个按钮提交必然出现「一半存了一半没存」。每多一个非 config.toml 的落点就多
    // 一个独立保存按钮的话，设置界面最后会是一堆各存各的按钮——所以统一登记进
    // `state::SideCommitter`，由底栏那个按钮提交。
    //
    // 挂 effect 而非挂在「关闭」按钮上：对话框有多条关闭路径（footer 按钮 / ESC /
    // 标题栏），逐个挂等于赌自己数全了。
    let st_stage = state.clone();
    let vis = visible;
    let stage_effect = crate::pages::dict::reactive::reactive_effect(
        move || vis.version(),
        move || {
            if !vis.get() {
                st_stage.stage_settings_edits();
            }
        },
    );

    // 走与「全局方案配置」相同的外壳（标题栏 + 固定高滚动区 + 按钮栏）。此前用的是
    // dialog_panel：无滚动、标题 18px、宽 440，同一个设置界面里两种弹窗长得不一样。
    let content = Element::col()
        .width_match()
        .padding_xy(16, 2)
        .child(sections)
        .child(hotkey_effect)
        .child(trigger_effect)
        .child(ct_effect)
        .child(stage_effect);

    crate::dialogs::field_dialogs::build_group_dialog_shell(visible, "方案设置", content, vec![])
}

#[cfg(test)]
mod ext_dict_tests {
    use super::*;

    /// 解绑必须**移除条目**，不能留空串——否则 config.toml 里会积下
    /// `方案id = ""` 的死条目，用户以为自己设过热键。
    #[test]
    fn hotkey_edit_unbind_removes_entry() {
        let mut m = std::collections::HashMap::new();
        apply_hotkey_edit(&mut m, "english", true, "ctrl+shift+n");
        assert_eq!(m.get("english").map(String::as_str), Some("ctrl+shift+n"));

        // 关掉「启用」
        apply_hotkey_edit(&mut m, "english", false, "ctrl+shift+n");
        assert!(!m.contains_key("english"), "解绑应移除条目，实际 {m:?}");

        // 启用着但主键选成「（未设置）」：raw 为空，同样按解绑处理
        apply_hotkey_edit(&mut m, "english", true, "ctrl+shift+n");
        apply_hotkey_edit(&mut m, "english", true, "");
        assert!(!m.contains_key("english"), "空 raw 应移除条目，实际 {m:?}");

        // core 的未绑定 sentinel 也不该被写进来
        apply_hotkey_edit(&mut m, "english", true, "none");
        assert!(!m.contains_key("english"), "\"none\" 不该入表，实际 {m:?}");
    }

    /// 待提交队列必须按方案累积。用户连着配好几个方案再点「应用设置」是正常用法，
    /// 只留最后一份的话前面几个会**静默**丢——没有任何提示。
    #[test]
    fn merge_pending_accumulates_across_schemas_and_edits() {
        let mut q = std::collections::HashMap::new();

        // 方案 A：先只改词库开关。
        merge_pending(&mut q, "wubi", None, vec![("emoji".into(), true)]);
        // 方案 B：改码表配置。
        merge_pending(
            &mut q,
            "qsym",
            Some(json!({"engine":{"codetable":{}}})),
            vec![],
        );
        assert_eq!(q.len(), 2, "两个方案各占一条，不该互相挤掉");

        // 回到方案 A 改码表配置：与上一次的词库改动**合并**。
        merge_pending(
            &mut q,
            "wubi",
            Some(json!({"engine":{"type":"codetable"}})),
            vec![],
        );
        let a = &q["wubi"];
        assert!(a.cfg.is_some(), "码表配置应记下");
        assert_eq!(
            a.dicts,
            vec![("emoji".to_string(), true)],
            "上一次的词库改动不该被这次覆盖掉"
        );

        // 同一个词库开关反复拨：以最后一次为准，不产生两条。
        merge_pending(&mut q, "wubi", None, vec![("emoji".into(), false)]);
        assert_eq!(q["wubi"].dicts, vec![("emoji".to_string(), false)]);
        // 另一个词库累加。
        merge_pending(&mut q, "wubi", None, vec![("district".into(), true)]);
        assert_eq!(q["wubi"].dicts.len(), 2);
        // cfg 未再给出时保留上一次的——不能因为这次只改了词库就把方案配置抹掉。
        assert!(q["wubi"].cfg.is_some(), "只改词库不该清掉已暂存的方案配置");
    }

    /// 中途失败时的队列状态决定了「重试」是否可用：已落盘的不能再写一遍，
    /// 未落盘的不能消失。两个方向都错过一次就是静默丢数据。
    #[test]
    fn drain_pending_keeps_unsubmitted_on_failure() {
        let q: Rc<RefCell<std::collections::HashMap<String, PendingSchemaEdit>>> =
            Rc::new(RefCell::new(std::collections::HashMap::new()));
        for id in ["a", "b", "c"] {
            merge_pending(&mut q.borrow_mut(), id, Some(json!({"id": id})), vec![]);
        }

        // 第二个方案提交失败（顺序不定，故按「第 2 次调用」判）。
        let seen = Rc::new(RefCell::new(Vec::<String>::new()));
        let s2 = seen.clone();
        let err = drain_pending(&q, move |id, _| {
            s2.borrow_mut().push(id.to_string());
            if s2.borrow().len() == 2 {
                Err("boom".to_string())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(err, "boom");

        let done = &seen.borrow()[0];
        let failed = &seen.borrow()[1];
        assert!(
            !q.borrow().contains_key(done),
            "已落盘的必须移出队列，否则下次保存重写一遍"
        );
        assert!(q.borrow().contains_key(failed), "失败的必须留下供重试");
        assert_eq!(q.borrow().len(), 2, "失败点之后的也该原样留着");

        // 重试：这次全成功 → 队列清空。
        assert!(drain_pending(&q, |_, _| Ok(())).is_ok());
        assert!(q.borrow().is_empty(), "全部成功后队列应清空");
    }

    /// 提交动作执行期间不得持有队列借用——真实路径上它会走 RPC，
    /// 回调链上任何一处碰到队列就是 BorrowMutError panic。
    #[test]
    fn drain_pending_does_not_hold_borrow_during_submit() {
        let q: Rc<RefCell<std::collections::HashMap<String, PendingSchemaEdit>>> =
            Rc::new(RefCell::new(std::collections::HashMap::new()));
        merge_pending(&mut q.borrow_mut(), "wubi", Some(json!({})), vec![]);
        let probe = q.clone();
        let r = drain_pending(&q, move |_, _| {
            // 若实现是「borrow 着遍历」，这一句直接 panic。
            assert!(probe.try_borrow_mut().is_ok(), "submit 期间队列不该被借着");
            Ok(())
        });
        assert!(r.is_ok());
    }

    /// 摘要必须区分「跟随全局」与「跟随内置基线」：特殊方案不继承全局码表配置
    /// （core `codetable_baseline`），对它说「跟随全局配置」是在骗人——用户会去改
    /// 全局，然后发现快符纹丝不动。
    #[test]
    fn ct_summary_distinguishes_baseline_by_schema_kind() {
        assert_eq!(ct_summary(false, false), "跟随全局配置");
        assert_eq!(ct_summary(false, true), "跟随内置基线");
        // 开启后跟谁都无关了，两种方案同一句。
        assert_eq!(ct_summary(true, false), "已自定义");
        assert_eq!(ct_summary(true, true), "已自定义");
    }

    /// 每个方案一条，互不干扰——对话框复用同一组工作态信号，最容易写成
    /// 「改了 B 把 A 也一起覆盖掉」。
    #[test]
    fn hotkey_edit_is_per_schema() {
        let mut m = std::collections::HashMap::new();
        apply_hotkey_edit(&mut m, "wubi86", true, "ctrl+shift+w");
        apply_hotkey_edit(&mut m, "english", true, "ctrl+shift+n");
        apply_hotkey_edit(&mut m, "english", false, "");
        assert_eq!(
            m.get("wubi86").map(String::as_str),
            Some("ctrl+shift+w"),
            "解绑 english 不应影响 wubi86：{m:?}"
        );
        // 空 id（settings_schema_id 尚未播种）不得污染表
        apply_hotkey_edit(&mut m, "", true, "ctrl+shift+x");
        assert_eq!(m.len(), 1, "空方案 id 不该产生条目：{m:?}");
    }

    #[test]
    fn scheme_import_preview_msg() {
        let v = json!({
            "package": {
                "package": { "created_at": "2026-07-13T00:00:00+08:00" },
                "schema": { "id": "demo", "version": "1.0" }
            },
            "willAdd": ["a", "b"], "conflicts": ["c"], "systemRefs": [], "missing": []
        });
        let msg = SchemaManagerState::format_import_preview(&v);
        assert!(msg.contains("方案 demo v1.0"));
        assert!(msg.contains("2026-07-13"));
        assert!(msg.contains("新增文件 2"));
        assert!(msg.contains("已存在 1"));
        // 手工包无 package.toml:回落"未知"不炸
        let bare = json!({ "willAdd": ["x"], "conflicts": [] });
        let msg2 = SchemaManagerState::format_import_preview(&bare);
        assert!(msg2.contains("未知"));
    }

    #[test]
    fn parse_filters_main_and_resolves_enabled() {
        let cfg = json!({ "dictionaries": [
            { "id": "m",  "label": "主库", "default": true },
            { "id": "a",  "label": "扩展A", "description": "描述A", "default_enabled": true },
            { "id": "b",  "label": "扩展B", "default_enabled": false },
            { "id": "c",  "label": "扩展C", "default_enabled": true, "enabled": false },
            { "id": "d",  "label": "扩展D" }
        ]});
        let rows = parse_extension_dicts(&cfg);
        // 主库被过滤，剩 4 条附加库。
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c", "d"]
        );
        assert!(rows[0].enabled, "a: default_enabled=true");
        assert!(!rows[1].enabled, "b: default_enabled=false");
        assert!(
            !rows[2].enabled,
            "c: enabled=false 覆盖 default_enabled=true"
        );
        assert!(rows[3].enabled, "d: 无字段 → 默认 true");
        assert_eq!(rows[0].description, "描述A");
    }

    /// 空 order = 全部启用。这条语义在 rebuild_list/toggle 里各写了一遍，
    /// 抽成谓词后由本测试统一守住。
    #[test]
    fn empty_order_means_all_enabled() {
        assert!(schema_is_enabled(&[], "anything"));
        let order = vec!["a".to_string(), "b".to_string()];
        assert!(schema_is_enabled(&order, "a"));
        assert!(!schema_is_enabled(&order, "c"));
    }

    #[test]
    fn can_activate_requires_selected_enabled_and_not_current() {
        let order = vec!["a".to_string(), "b".to_string()];
        assert!(!can_activate(&order, "a", None), "无选中 → 不可用");
        assert!(!can_activate(&order, "a", Some("a")), "已是当前 → 不可用");
        assert!(!can_activate(&order, "a", Some("c")), "未启用 → 不可用");
        assert!(
            can_activate(&order, "a", Some("b")),
            "已启用的非当前项 → 可用"
        );
        // 全部启用（order 空）时，任何非当前方案都可设为当前。
        assert!(can_activate(&[], "a", Some("z")));
    }

    #[test]
    fn parse_missing_dictionaries_is_empty() {
        assert!(parse_extension_dicts(&json!({})).is_empty());
        assert!(parse_extension_dicts(&json!({ "dictionaries": [] })).is_empty());
    }
}
