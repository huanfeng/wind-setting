# 设置向导 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 wind-setting 加一个 4 步「设置向导」，按五笔/全拼/双拼快速配置方案与常用选项。首次运行强制弹出，入口固定在左侧导航底部。

**Architecture:**
- 纯逻辑放在 `src/wizard/model.rs`：当前配置 → 草稿 → 目标配置。写入用的 `config.setItems` 条目复用现有的 `diff_config`，从「基线 → 目标」直接算出来，不另写一套 writer。
- 写入编排放在 `src/wizard/apply.rs`，RPC 经 trait 注入，可单测。
- 界面放在 `src/wizard/view.rs`，仿照 `update_dialog` 的写法：一个阶段信号，配合 `visible_when` 切换各段。
- 首启标志放在 `wind_setting.toml`。core 不改。

**Tech Stack:** Rust 2024，windui（`../wind-ui-rust`），serde_json，wind-setting 现有的 `rpc::call_core` / `Config` / `diff_config`。

**Spec:** `docs/design/setup-wizard.md`（执行者两份都要读）

## Global Constraints

- 只改 wind-setting，不改 core 和 manifest 键。向导写的全部是已登记的键。
- 向导在最后一步确认之前，**不改动任何 core 配置**。中途关闭只写 `wind_setting.toml` 的 `[ui] wizard_dismissed`。
- 未显示的项（按主类型隐藏的）和用户未改变语义的项（例如 `code_hint_source="auto"` 仍为开），**不产生写入**。
- 所有用户都弹一次，判据只看 `wizard_dismissed`（缺省 false）。
- 写入顺序固定：`config.setItems` → `schema.saveConfig`（双拼布局有变时）→ `schema.setActive`（默认方案有变时）。哪一步失败就停在哪一步，并如实报告已完成的步骤。
- 提交纪律：`git commit -F - --only -- <路径…>`。禁用 `git add -A`、`git add .`、`git commit -a`、`git stash`。本仓有并发会话，例如 `IMPLEMENTATION_PLAN.md` 不是本功能的文件，不要碰。
- 格式化只跑 `cargo fmt -- <自己的文件>` 或 `rustfmt --edition 2024 <文件>`，不要跑 `cargo fmt --all`。
- 测试命令：在 wind-setting 下执行 `cargo test --no-fail-fast <filter>`，本机原生 Linux 可编译可测。测试临时目录用独立 `TMPDIR`，例如 `TMPDIR=/tmp/wz-$$`。
- 文案用中文，跟现有设置页的语气保持一致。

## Review Focus

1. **向导写入后，再在设置页点「保存」会不会把向导的结果改回去。** `schema.available`、`schema.active`、`keys.key_actions`、自定义标点这几项都由 `schema_mgr` 或 punct cells 的 writer 覆盖式写回。写入成功后必须用 `apply_loaded(LoadedState::fetch())` 整体重灌，不能只调 `reload()`。在 Task 5 里加测试钉住。
2. **快捷输入的键选成另一个动词正在占用的键**，例如 `/` 已经绑了 `special:kf`。期望行为：冲突提示，不覆盖。Task 2 的 `key_conflicts` 要覆盖到。
3. **老用户的方案集里有自定义方案**，比如自己导入的码表。期望行为：勾选列表里能看到它，预填时保持勾选，切换主类型的预设也不会把它取消。Task 2 测试覆盖。
4. **core 连不上**（`connected=false`）时到点弹出。期望行为：不弹出，入口点开后提示「未连接输入法服务」。Task 4 测试覆盖。
5. **双拼布局列表拉取失败**（`schema.layouts` 为空）。期望行为：布局下拉只显示当前值，不产生布局写入。Task 2 测试覆盖。

---

## 文件结构

| 文件 | 职责 |
|---|---|
| Create `src/wizard/mod.rs` | 模块声明；`pub use` 对外入口 |
| Create `src/wizard/model.rs` | `Kind`、`Trigger`、`Draft`、`from_current`、`with_preset`、`to_target`、`summary`、`key_conflicts`、`override_hints` |
| Create `src/wizard/apply.rs` | `trait CoreCalls`、`apply_writes`、`ApplyOutcome` |
| Create `src/wizard/view.rs` | `build_wizard_dialog`，包括前置脏检查段、4 步正文、确认段、结果段 |
| Modify `src/main.rs` | `mod wizard;`；在 `apply_loaded` 之后调用 `maybe_auto_open` |
| Modify `src/setting_config.rs:82` | `UiStateConfig.wizard_dismissed` |
| Modify `src/state.rs` | `wizard` 状态信号组；`save_wizard_dismissed`；`open_wizard` |
| Modify `src/shell.rs` | 根 stack 挂对话框；侧栏底部加固定入口 |
| Modify `src/manifest.rs:4853` | `slash_row_index` 改为 `pub(crate)`（向导要复用） |

---

### Task 1: 草稿模型与「从当前配置预填」

**Files:**
- Create: `src/wizard/mod.rs`、`src/wizard/model.rs`
- Modify: `src/main.rs`（`mod wizard;`）、`src/manifest.rs:4853`（`pub(crate) fn slash_row_index`）

**Interfaces:**
- Consumes：
  - `crate::config::Config::{get_str,get_bool,get_array,get_path}`
  - `crate::state::SchemaInfo{id,name,kind,scheme,hidden}`
  - `crate::manifest::{PUNCT_MAP_KEY, PUNCT_ENABLED_KEY, PUNCT_DEFAULTS, seed_cell, slash_dun_on, slash_row_index}`
- Produces：

```rust
pub enum Kind { Codetable, Pinyin, Shuangpin }
pub enum Trigger { Keep(Vec<String>), Off, Key(String) }
pub struct Draft { /* 字段见下 */ }
pub fn Draft::from_current(cfg: &Config, schemas: &[SchemaInfo], layout: &str) -> Draft
pub const VERB_QUICK: &str = "mix:quick_mix";
pub const VERB_TEMP_PINYIN: &str = "temp_pinyin";
pub const QUICK_KEYS: &[(&str, &str)];   // (键名, 标签)
pub const TEMP_KEYS: &[(&str, &str)];
```

- [ ] **Step 1: 写失败测试**（`src/wizard/model.rs` 末尾 `#[cfg(test)] mod tests`）

```rust
use super::*;
use serde_json::json;

fn si(id: &str, kind: &str, scheme: &str) -> SchemaInfo {
    SchemaInfo { id: id.into(), name: id.into(), kind: kind.into(), scheme: scheme.into(), hidden: false }
}
fn schemas() -> Vec<SchemaInfo> {
    vec![
        si("wubi86", "codetable", ""), si("wubi86_pinyin", "mixed", ""),
        si("pinyin", "pinyin", "full"), si("shuangpin", "pinyin", "shuangpin"),
        si("stroke", "codetable", ""), si("english", "english", ""),
    ]
}
fn cfg(v: serde_json::Value) -> Config { Config::from_value(v) }

#[test]
fn kind_is_inferred_from_active_schema() {
    let s = schemas();
    let d = |a: &str| Draft::from_current(&cfg(json!({"schema": {"active": a, "available": [a]}})), &s, "xiaohe").kind;
    assert_eq!(d("pinyin"), Kind::Pinyin);
    assert_eq!(d("shuangpin"), Kind::Shuangpin);
    assert_eq!(d("wubi86"), Kind::Codetable);
    assert_eq!(d("my_custom_table"), Kind::Codetable);
}

#[test]
fn code_hint_auto_counts_as_on() {
    let c = cfg(json!({"schema": {"active": "wubi86", "pinyin": {"code_hint_source": "auto"}}}));
    assert!(Draft::from_current(&c, &schemas(), "xiaohe").code_hint);
    let c = cfg(json!({"schema": {"active": "wubi86", "pinyin": {"code_hint_source": "off"}}}));
    assert!(!Draft::from_current(&c, &schemas(), "xiaohe").code_hint);
}

#[test]
fn trigger_single_known_binding_is_key() {
    let c = cfg(json!({"keys": {"key_actions": {"semicolon": "mix:quick_mix", "backtick": "temp_pinyin"}}}));
    let d = Draft::from_current(&c, &schemas(), "xiaohe");
    assert_eq!(d.quick, Trigger::Key("semicolon".into()));
    assert_eq!(d.temp_key, Trigger::Key("backtick".into()));
}

#[test]
fn trigger_none_bound_is_off_and_multi_is_keep() {
    let c = cfg(json!({"keys": {"key_actions": {"semicolon": "mix:quick_mix", "quote": "mix:quick_mix"}}}));
    let d = Draft::from_current(&c, &schemas(), "xiaohe");
    assert_eq!(d.quick, Trigger::Keep(vec!["quote".into(), "semicolon".into()]));
    assert_eq!(d.temp_key, Trigger::Off);
}

#[test]
fn trigger_unlisted_key_is_keep() {
    let c = cfg(json!({"keys": {"key_actions": {"lbracket": "mix:quick_mix"}}}));
    assert_eq!(Draft::from_current(&c, &schemas(), "x").quick, Trigger::Keep(vec!["lbracket".into()]));
}

#[test]
fn slash_dun_requires_master_switch() {
    let on = cfg(json!({"input": {"punct": {"custom_enabled": true, "custom_mappings": {"/": ["、","／","、","/"]}}}}));
    assert!(Draft::from_current(&on, &schemas(), "x").slash_dun);
    let gated = cfg(json!({"input": {"punct": {"custom_enabled": false, "custom_mappings": {"/": ["、","／","、","/"]}}}}));
    assert!(!Draft::from_current(&gated, &schemas(), "x").slash_dun);
}

#[test]
fn enabled_follows_available_and_keeps_custom_schemas() {
    let c = cfg(json!({"schema": {"active": "wubi86", "available": ["wubi86", "my_table"]}}));
    let d = Draft::from_current(&c, &schemas(), "x");
    assert_eq!(d.enabled, vec!["wubi86".to_string(), "my_table".to_string()]);
}
```

- [ ] **Step 2: 运行，确认失败**

运行：`cd ../wind-setting && TMPDIR=/tmp/wz-$$ cargo test --no-fail-fast wizard::model`
期望：编译失败，报 `Draft` / `Kind` 未定义。

- [ ] **Step 3: 实现**

`src/wizard/mod.rs`：

```rust
//! 设置向导：按输入习惯（五笔/全拼/双拼）4 步快速配置。设计见 docs/design/setup-wizard.md。
pub mod apply;
pub mod model;
pub mod view;
```

先只放 `pub mod model;`。`apply`、`view` 在后续任务里加入，保证每个任务都能编译。

`src/wizard/model.rs`：

```rust
//! 向导的纯逻辑：当前配置 → 草稿 → 目标配置。不碰 RPC、不碰 UI，全部可单测。

use serde_json::Value;

use crate::config::Config;
use crate::manifest::{
    PUNCT_DEFAULTS, PUNCT_ENABLED_KEY, PUNCT_MAP_KEY, seed_cell, slash_dun_on, slash_row_index,
};
use crate::state::SchemaInfo;

pub const VERB_QUICK: &str = "mix:quick_mix";
pub const VERB_TEMP_PINYIN: &str = "temp_pinyin";
pub(crate) const KEY_ACTIONS: &str = "keys.key_actions";

/// 快捷输入可选键（键名, 标签）。只列常用的几个；其余绑定落在 `Trigger::Keep`。
pub const QUICK_KEYS: &[(&str, &str)] = &[
    ("semicolon", "分号 ( ; )"),
    ("backtick", "反引号 ( ` )"),
    ("quote", "单引号 ( ' )"),
];
/// 临时拼音可选键。
pub const TEMP_KEYS: &[(&str, &str)] = &[
    ("backtick", "反引号 ( ` )"),
    ("semicolon", "分号 ( ; )"),
    ("quote", "单引号 ( ' )"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Codetable,
    Pinyin,
    Shuangpin,
}

impl Kind {
    /// 主类型对应的默认方案 id。
    pub fn schema_id(self) -> &'static str {
        match self {
            Kind::Codetable => "wubi86",
            Kind::Pinyin => "pinyin",
            Kind::Shuangpin => "shuangpin",
        }
    }
    fn from_active(active: &str) -> Kind {
        match active {
            "pinyin" => Kind::Pinyin,
            "shuangpin" => Kind::Shuangpin,
            _ => Kind::Codetable,
        }
    }
}

/// 一个引导动词的绑定选择。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// 保持现状：现有绑定不是「恰好一个、且在可选键里」。携带排序后的现有键，供界面显示。
    Keep(Vec<String>),
    /// 不启用：清除该动词的全部绑定。
    Off,
    /// 恰好绑这一个键。
    Key(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub kind: Kind,
    /// 默认方案。主类型是五笔时，保留用户原来的码表默认方案（例如自定义码表）。
    pub active: String,
    pub layout: String,
    /// 启用方案，保持原有顺序。
    pub enabled: Vec<String>,
    // 第 2 步：码表
    pub auto_commit_at_full: bool,
    pub clear_on_empty_max: bool,
    pub code_hint: bool,
    // 第 2 步：拼音
    pub emoji: bool,
    pub fuzzy: bool,
    pub fuzzy_pairs: [bool; 4],
    pub allow_full_pinyin: bool,
    // 第 3 步
    pub quick: Trigger,
    pub slash_dun: bool,
    pub temp_pinyin: bool,
    pub temp_key: Trigger,
}

/// 模糊音常用 4 对的键名（`schema.pinyin.fuzzy.<名>`）与标签。
pub const FUZZY_PAIRS: [(&str, &str); 4] =
    [("zh_z", "zh = z"), ("ch_c", "ch = c"), ("sh_s", "sh = s"), ("n_l", "n = l")];

fn trigger_of(cfg: &Config, verb: &str, options: &[(&str, &str)]) -> Trigger {
    let mut keys: Vec<String> = cfg
        .get_path(KEY_ACTIONS)
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter(|(_, v)| v.as_str() == Some(verb))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();
    keys.sort();
    match keys.as_slice() {
        [] => Trigger::Off,
        [k] if options.iter().any(|(o, _)| o == k) => Trigger::Key(k.clone()),
        _ => Trigger::Keep(keys),
    }
}

fn slash_dun_of(cfg: &Config) -> bool {
    let idx = slash_row_index();
    let (_, tok, defs) = PUNCT_DEFAULTS[idx];
    let ov = cfg
        .get_path(PUNCT_MAP_KEY)
        .and_then(|v| v.get(tok))
        .and_then(|v| v.as_array());
    slash_dun_on(&seed_cell(ov, 0, defs[0]), cfg.get_bool(PUNCT_ENABLED_KEY).unwrap_or(false))
}

fn strs(v: Option<&Vec<Value>>) -> Vec<String> {
    v.map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

impl Draft {
    /// 按当前实际配置预填。`layout` 是 `shuangpin` 方案当前布局 id（拿不到时传出厂 `xiaohe`）。
    pub fn from_current(cfg: &Config, _schemas: &[SchemaInfo], layout: &str) -> Draft {
        let active = cfg.get_str("schema.active").unwrap_or("wubi86").to_string();
        let b = |k: &str| cfg.get_bool(k).unwrap_or(false);
        Draft {
            kind: Kind::from_active(&active),
            enabled: strs(cfg.get_array("schema.available")),
            active,
            layout: layout.to_string(),
            auto_commit_at_full: b("schema.codetable.auto_commit_at_full"),
            clear_on_empty_max: b("schema.codetable.clear_on_empty_max"),
            code_hint: cfg.get_str("schema.pinyin.code_hint_source").unwrap_or("off") != "off",
            emoji: b("input.emoji.enabled"),
            fuzzy: b("schema.pinyin.fuzzy.enabled"),
            fuzzy_pairs: FUZZY_PAIRS.map(|(k, _)| b(&format!("schema.pinyin.fuzzy.{k}"))),
            allow_full_pinyin: b("schema.pinyin.shuangpin.allow_full_pinyin"),
            quick: trigger_of(cfg, VERB_QUICK, QUICK_KEYS),
            slash_dun: slash_dun_of(cfg),
            temp_pinyin: cfg.get_bool("input.temp_pinyin.enabled").unwrap_or(true),
            temp_key: trigger_of(cfg, VERB_TEMP_PINYIN, TEMP_KEYS),
        }
    }
}
```

说明：`schema.active` 缺省时按 `wubi86` 处理，和 `data/config.toml` 的出厂值一致。`_schemas` 这个参数要留着，Task 2 的 `with_preset` 会用到同一份列表。

- [ ] **Step 4: 运行，确认通过**

运行：`TMPDIR=/tmp/wz-$$ cargo test --no-fail-fast wizard::model`
期望：7 个测试全部 PASS。

- [ ] **Step 5: 提交**

```bash
cd wind-setting
rustfmt --edition 2024 src/wizard/mod.rs src/wizard/model.rs
git add -- src/wizard/mod.rs src/wizard/model.rs
git commit -F - --only -- src/wizard/mod.rs src/wizard/model.rs src/main.rs src/manifest.rs <<'EOF'
feat(wizard): 向导草稿模型，按当前配置预填
EOF
```

`main.rs` 和 `manifest.rs` 可能被别的会话改过：提交前跑 `git diff -- src/main.rs src/manifest.rs`，确认里面只有本任务那两行。如果混进了别人的改动，就按 hunk 进暂存区：`git diff` → 过滤出自己的 hunk → `git apply --cached`。

---

### Task 2: 预设、目标配置、确认摘要、冲突与 override 提示

**Files:**
- Modify: `src/wizard/model.rs`

**Interfaces:**
- Consumes：Task 1 的 `Draft`、`Kind`、`Trigger`；`crate::config::{diff_config, ConfigItem}`；`crate::manifest::{apply_slash_dun, punct_row_override}`。
- Produces：

```rust
pub fn Draft::with_preset(&self, kind: Kind, schemas: &[SchemaInfo]) -> Draft
pub fn Draft::to_target(&self, base: &Config) -> Config
pub fn Draft::items(&self, base: &Config, atomic_keys: &[String]) -> Vec<ConfigItem>
pub struct Change { pub group: &'static str, pub label: String, pub old: String, pub new: String }
pub fn summary(cur: &Draft, new: &Draft, names: &dyn Fn(&str) -> String, layout_label: &dyn Fn(&str) -> String) -> Vec<Change>
pub struct Conflict { pub key: String, pub msg: String }
pub fn key_conflicts(d: &Draft, base: &Config) -> Vec<Conflict>
pub fn override_hints(items: &[ConfigItem], active: &str, ov: &HashMap<String, Vec<String>>) -> Vec<String>
pub fn layout_changed(cur: &Draft, new: &Draft) -> bool
```

- [ ] **Step 1: 写失败测试**（追加到 `tests`）

```rust
fn base() -> Config {
    cfg(json!({
        "schema": {"active": "wubi86", "available": ["wubi86", "wubi86_pinyin", "my_table"],
                   "codetable": {"auto_commit_at_full": false, "clear_on_empty_max": false},
                   "pinyin": {"code_hint_source": "auto", "fuzzy": {"enabled": false}}},
        "input": {"emoji": {"enabled": false}, "temp_pinyin": {"enabled": true},
                  "punct": {"custom_enabled": false, "custom_mappings": {}}},
        "keys": {"key_actions": {"semicolon": "mix:quick_mix", "backtick": "temp_pinyin",
                                 "backslash": "special:kf"}}
    }))
}
const ATOMIC: &[String] = &[];

#[test]
fn untouched_draft_produces_no_items() {
    let b = base();
    let d = Draft::from_current(&b, &schemas(), "xiaohe");
    assert!(d.items(&b, ATOMIC).is_empty());
}

#[test]
fn code_hint_auto_untouched_stays_auto() {
    let b = base();
    let mut d = Draft::from_current(&b, &schemas(), "xiaohe");
    d.auto_commit_at_full = true; // 改别的
    let t = d.to_target(&b);
    assert_eq!(t.get_str("schema.pinyin.code_hint_source"), Some("auto"));
}

#[test]
fn code_hint_on_from_off_writes_codetable() {
    let mut b = base();
    b.set_path("schema.pinyin.code_hint_source", json!("off"));
    let mut d = Draft::from_current(&b, &schemas(), "x");
    d.code_hint = true;
    assert_eq!(d.to_target(&b).get_str("schema.pinyin.code_hint_source"), Some("codetable"));
}

#[test]
fn codetable_preset() {
    let d = Draft::from_current(&base(), &schemas(), "x").with_preset(Kind::Codetable, &schemas());
    assert!(d.auto_commit_at_full && d.clear_on_empty_max && d.code_hint && d.temp_pinyin);
    assert_eq!(d.active, "wubi86");
    for id in ["wubi86", "wubi86_pinyin", "pinyin", "my_table"] {
        assert!(d.enabled.contains(&id.to_string()), "{id}");
    }
    assert_eq!(d.quick, Trigger::Key("semicolon".into()));
}

#[test]
fn pinyin_preset_keeps_custom_schemas_and_forces_main() {
    let d = Draft::from_current(&base(), &schemas(), "x").with_preset(Kind::Pinyin, &schemas());
    assert_eq!(d.active, "pinyin");
    assert_eq!(d.enabled.first().map(String::as_str), Some("pinyin"));
    assert!(d.enabled.contains(&"my_table".to_string()));
    assert!(!d.enabled.contains(&"wubi86".to_string()));
    assert!(d.emoji && !d.fuzzy);
}

#[test]
fn pinyin_kind_does_not_write_codetable_or_temp_pinyin_keys() {
    let b = base();
    let mut d = Draft::from_current(&b, &schemas(), "x").with_preset(Kind::Pinyin, &schemas());
    d.temp_pinyin = false;          // 拼音用户界面不显示，改了也不该写
    d.auto_commit_at_full = true;   // 同上
    let keys: Vec<String> = d.items(&b, ATOMIC).into_iter().map(|i| i.key).collect();
    assert!(!keys.iter().any(|k| k.starts_with("input.temp_pinyin") || k.starts_with("schema.codetable")), "{keys:?}");
}

#[test]
fn quick_off_removes_binding_and_key_moves() {
    let b = base();
    let mut d = Draft::from_current(&b, &schemas(), "x");
    d.quick = Trigger::Off;
    let t = d.to_target(&b);
    assert!(t.get_path("keys.key_actions.semicolon").is_none());
    d.quick = Trigger::Key("quote".into());
    let t = d.to_target(&b);
    assert_eq!(t.get_str("keys.key_actions.quote"), Some("mix:quick_mix"));
    assert!(t.get_path("keys.key_actions.semicolon").is_none());
    assert_eq!(t.get_str("keys.key_actions.backslash"), Some("special:kf"));
}

#[test]
fn keep_trigger_writes_nothing() {
    let b = base();
    let mut d = Draft::from_current(&b, &schemas(), "x");
    d.quick = Trigger::Keep(vec!["semicolon".into()]);
    assert_eq!(d.to_target(&b).get_path("keys.key_actions"), b.get_path("keys.key_actions"));
}

#[test]
fn slash_dun_on_touches_only_slash_row_and_enables_master() {
    let b = base();
    let mut d = Draft::from_current(&b, &schemas(), "x");
    d.slash_dun = true;
    let t = d.to_target(&b);
    assert_eq!(t.get_bool("input.punct.custom_enabled"), Some(true));
    // 路径含 `/`，不能走点分路径：取整张表再按键取
    let m = t.get_path("input.punct.custom_mappings").unwrap().as_object().unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(m["/"][0], "、");
    assert_eq!(m["/"][2], "、");
}

#[test]
fn conflicts_between_wizard_keys_and_foreign_bindings() {
    let b = base();
    let mut d = Draft::from_current(&b, &schemas(), "x");
    d.temp_key = Trigger::Key("semicolon".into()); // 与快捷输入同键
    assert!(!key_conflicts(&d, &b).is_empty());
    let mut d = Draft::from_current(&b, &schemas(), "x");
    d.quick = Trigger::Key("quote".into());
    assert!(key_conflicts(&d, &b).is_empty());
    let mut b2 = base();
    b2.set_path("keys.key_actions.quote", json!("special:kf"));
    assert!(!key_conflicts(&d, &b2).is_empty(), "占用其它动词的键要报冲突");
}

#[test]
fn slash_dun_conflicts_with_slash_trigger() {
    let mut b = base();
    b.set_path("keys.key_actions.slash", json!("mix:quick_mix"));
    b.set_path("keys.key_actions.semicolon", json!(null));
    let mut d = Draft::from_current(&b, &schemas(), "x");
    d.slash_dun = true;
    assert!(!key_conflicts(&d, &b).is_empty());
}

#[test]
fn temp_conflict_ignored_for_pinyin_kind() {
    let b = base();
    let mut d = Draft::from_current(&b, &schemas(), "x").with_preset(Kind::Pinyin, &schemas());
    d.temp_key = Trigger::Key("semicolon".into());
    assert!(key_conflicts(&d, &b).is_empty(), "拼音用户不显示临拼，不参与冲突");
}

#[test]
fn pinyin_quick_on_temp_pinyin_key_conflicts() {
    // 拼音用户向导里看不到临拼，它不会让出 backtick；覆盖即悄悄删掉临拼绑定
    let b = base();
    let mut d = Draft::from_current(&b, &schemas(), "x").with_preset(Kind::Pinyin, &schemas());
    d.quick = Trigger::Key("backtick".into());
    assert!(!key_conflicts(&d, &b).is_empty());
}

#[test]
fn codetable_swapping_quick_and_temp_keys_is_fine() {
    let b = base();
    let mut d = Draft::from_current(&b, &schemas(), "x");
    d.quick = Trigger::Key("backtick".into());
    d.temp_key = Trigger::Key("semicolon".into());
    assert!(key_conflicts(&d, &b).is_empty(), "两者都在让出原键，互换合法");
}

#[test]
fn summary_matches_items() {
    let b = base();
    let cur = Draft::from_current(&b, &schemas(), "xiaohe");
    let new = cur.with_preset(Kind::Shuangpin, &schemas());
    let id = |s: &str| s.to_string();
    let ch = summary(&cur, &new, &id, &id);
    assert!(!ch.is_empty());
    assert!(summary(&cur, &cur, &id, &id).is_empty());
}

#[test]
fn layout_change_detected_only_for_shuangpin() {
    let cur = Draft::from_current(&base(), &schemas(), "xiaohe");
    let mut new = cur.with_preset(Kind::Shuangpin, &schemas());
    new.layout = "ziranma".into();
    assert!(layout_changed(&cur, &new));
    let mut wubi = cur.clone();
    wubi.layout = "ziranma".into();
    assert!(!layout_changed(&cur, &wubi));
}

#[test]
fn override_hint_names_active_schema_keys() {
    let items = vec![crate::config::ConfigItem { key: "schema.codetable.auto_commit_at_full".into(), value: json!(true) }];
    let mut ov = std::collections::HashMap::new();
    ov.insert("schema.codetable.auto_commit_at_full".to_string(), vec!["wubi86".to_string()]);
    assert_eq!(override_hints(&items, "wubi86", &ov), vec!["schema.codetable.auto_commit_at_full".to_string()]);
    assert!(override_hints(&items, "pinyin", &ov).is_empty());
}
```

如果 `ConfigItem` 没有这两个公开字段，或者 `Config::set_path` 的签名不一样，先看一眼 `src/config.rs:87` 和 `set_path` 的定义，按实际签名改测试的构造写法，不要改断言。

- [ ] **Step 2: 运行，确认失败**

运行：`TMPDIR=/tmp/wz-$$ cargo test --no-fail-fast wizard::model`
期望：编译失败，报 `with_preset` / `to_target` 未定义。

- [ ] **Step 3: 实现**（追加到 `model.rs`）

```rust
use std::collections::HashMap;

use crate::config::{ConfigItem, diff_config};
use crate::manifest::{apply_slash_dun, punct_row_override};

impl Draft {
    /// 切换主类型：第 2、3 步套推荐预设，方案集按规则调整。
    /// 自定义方案（不在出厂 6 个里）一律保留，不受预设影响。
    pub fn with_preset(&self, kind: Kind, schemas: &[SchemaInfo]) -> Draft {
        const BUILTIN: [&str; 6] = ["wubi86", "wubi86_pinyin", "pinyin", "shuangpin", "stroke", "english"];
        let main = kind.schema_id();
        let preset: &[&str] = match kind {
            Kind::Codetable => &["wubi86_pinyin", "pinyin"],
            Kind::Pinyin | Kind::Shuangpin => &[],
        };
        let mut enabled = vec![main.to_string()];
        for id in self.enabled.iter() {
            let custom = !BUILTIN.contains(&id.as_str());
            if custom && !enabled.contains(id) {
                enabled.push(id.clone());
            }
        }
        for s in schemas {
            if preset.contains(&s.id.as_str()) && !enabled.contains(&s.id) {
                enabled.push(s.id.clone());
            }
        }
        let mut d = self.clone();
        d.kind = kind;
        d.active = main.to_string();
        d.enabled = enabled;
        match kind {
            Kind::Codetable => {
                d.auto_commit_at_full = true;
                d.clear_on_empty_max = true;
                d.code_hint = true;
                d.temp_pinyin = true;
                if !matches!(d.temp_key, Trigger::Keep(_)) {
                    d.temp_key = Trigger::Key("backtick".into());
                }
            }
            Kind::Pinyin | Kind::Shuangpin => {
                d.emoji = true;
                d.fuzzy = false;
                d.fuzzy_pairs = [false; 4];
                d.allow_full_pinyin = false;
            }
        }
        if !matches!(d.quick, Trigger::Keep(_)) {
            d.quick = Trigger::Key("semicolon".into());
        }
        d
    }

    /// 基线 + 草稿 → 目标配置。只写本主类型可见、且语义有变化的项。
    pub fn to_target(&self, base: &Config) -> Config {
        let cur = Draft::from_current(base, &[], &self.layout);
        let mut t = base.clone();
        let mut set = |k: &str, v: Value| t.set_path(k, v);

        if self.active != cur.active {
            set("schema.active", Value::String(self.active.clone()));
        }
        if self.enabled != cur.enabled {
            set("schema.available", Value::from(self.enabled.clone()));
        }
        match self.kind {
            Kind::Codetable => {
                if self.auto_commit_at_full != cur.auto_commit_at_full {
                    set("schema.codetable.auto_commit_at_full", self.auto_commit_at_full.into());
                }
                if self.clear_on_empty_max != cur.clear_on_empty_max {
                    set("schema.codetable.clear_on_empty_max", self.clear_on_empty_max.into());
                }
                if self.code_hint != cur.code_hint {
                    let v = if self.code_hint { "codetable" } else { "off" };
                    set("schema.pinyin.code_hint_source", v.into());
                }
                if self.temp_pinyin != cur.temp_pinyin {
                    set("input.temp_pinyin.enabled", self.temp_pinyin.into());
                }
            }
            Kind::Pinyin | Kind::Shuangpin => {
                if self.emoji != cur.emoji {
                    set("input.emoji.enabled", self.emoji.into());
                }
                if self.fuzzy != cur.fuzzy {
                    set("schema.pinyin.fuzzy.enabled", self.fuzzy.into());
                }
                for (i, (k, _)) in FUZZY_PAIRS.iter().enumerate() {
                    if self.fuzzy_pairs[i] != cur.fuzzy_pairs[i] {
                        set(&format!("schema.pinyin.fuzzy.{k}"), self.fuzzy_pairs[i].into());
                    }
                }
                if self.kind == Kind::Shuangpin && self.allow_full_pinyin != cur.allow_full_pinyin {
                    set("schema.pinyin.shuangpin.allow_full_pinyin", self.allow_full_pinyin.into());
                }
            }
        }
        drop(set);
        rebind(&mut t, VERB_QUICK, &self.quick, &cur.quick);
        if self.kind == Kind::Codetable {
            rebind(&mut t, VERB_TEMP_PINYIN, &self.temp_key, &cur.temp_key);
        }
        if self.slash_dun != cur.slash_dun {
            write_slash_dun(&mut t, self.slash_dun);
        }
        t
    }

    /// `config.setItems` 的条目：与设置页保存同一个 diff。
    pub fn items(&self, base: &Config, atomic_keys: &[String]) -> Vec<ConfigItem> {
        diff_config(base, &self.to_target(base), atomic_keys)
    }
}

fn rebind(t: &mut Config, verb: &str, want: &Trigger, cur: &Trigger) {
    if want == cur || matches!(want, Trigger::Keep(_)) {
        return;
    }
    let mut map = t
        .get_path(KEY_ACTIONS)
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    map.retain(|_, v| v.as_str() != Some(verb));
    if let Trigger::Key(k) = want {
        map.insert(k.clone(), Value::String(verb.to_string()));
    }
    t.set_path(KEY_ACTIONS, Value::Object(map));
}

fn write_slash_dun(t: &mut Config, on: bool) {
    let idx = slash_row_index();
    let (_, tok, defs) = PUNCT_DEFAULTS[idx];
    let mut map = t
        .get_path(PUNCT_MAP_KEY)
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let ov = map.get(tok).and_then(|v| v.as_array()).cloned();
    let mut row: [String; 4] = std::array::from_fn(|i| seed_cell(ov.as_ref(), i, defs[i]));
    apply_slash_dun(&mut row, &defs, on);
    match punct_row_override(&row, &defs) {
        Some(v) => {
            map.insert(tok.to_string(), v);
        }
        None => {
            map.remove(tok);
        }
    }
    t.set_path(PUNCT_MAP_KEY, Value::Object(map));
    if on {
        t.set_path(PUNCT_ENABLED_KEY, Value::Bool(true));
    }
}

/// 第 4 步的一行。
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub group: &'static str,
    pub label: String,
    pub old: String,
    pub new: String,
}

fn on_off(b: bool) -> String {
    if b { "开".into() } else { "关".into() }
}

fn trigger_text(t: &Trigger, opts: &[(&str, &str)]) -> String {
    match t {
        Trigger::Off => "不启用".into(),
        Trigger::Key(k) => opts
            .iter()
            .find(|(o, _)| o == k)
            .map(|(_, l)| l.to_string())
            .unwrap_or_else(|| k.clone()),
        Trigger::Keep(ks) => format!("保持现状（{}）", ks.join("、")),
    }
}

/// 第 4 步摘要：与 `to_target` 判定同一组「可见且变化」的项。
pub fn summary(
    cur: &Draft,
    new: &Draft,
    names: &dyn Fn(&str) -> String,
    layout_label: &dyn Fn(&str) -> String,
) -> Vec<Change> {
    let mut out = Vec::new();
    let mut push = |g: &'static str, l: &str, o: String, n: String| {
        if o != n {
            out.push(Change { group: g, label: l.to_string(), old: o, new: n });
        }
    };
    let list = |v: &[String]| v.iter().map(|s| names(s)).collect::<Vec<_>>().join("、");
    push("方案", "默认方案", names(&cur.active), names(&new.active));
    push("方案", "启用的方案", list(&cur.enabled), list(&new.enabled));
    if new.kind == Kind::Shuangpin {
        push("方案", "双拼布局", layout_label(&cur.layout), layout_label(&new.layout));
    }
    match new.kind {
        Kind::Codetable => {
            push("输入习惯", "全码唯一自动上屏", on_off(cur.auto_commit_at_full), on_off(new.auto_commit_at_full));
            push("输入习惯", "满码空码清空", on_off(cur.clear_on_empty_max), on_off(new.clear_on_empty_max));
            push("输入习惯", "拼音反查（显示五笔编码）", on_off(cur.code_hint), on_off(new.code_hint));
        }
        Kind::Pinyin | Kind::Shuangpin => {
            push("输入习惯", "emoji 候选", on_off(cur.emoji), on_off(new.emoji));
            push("输入习惯", "模糊音", on_off(cur.fuzzy), on_off(new.fuzzy));
            for (i, (_, l)) in FUZZY_PAIRS.iter().enumerate() {
                push("输入习惯", &format!("模糊音 {l}"), on_off(cur.fuzzy_pairs[i]), on_off(new.fuzzy_pairs[i]));
            }
            if new.kind == Kind::Shuangpin {
                push("输入习惯", "允许按全拼输入", on_off(cur.allow_full_pinyin), on_off(new.allow_full_pinyin));
            }
        }
    }
    if !matches!(new.quick, Trigger::Keep(_)) {
        push("通用", "快捷输入启用键", trigger_text(&cur.quick, QUICK_KEYS), trigger_text(&new.quick, QUICK_KEYS));
    }
    push("通用", "/ 键输出顿号", on_off(cur.slash_dun), on_off(new.slash_dun));
    if new.kind == Kind::Codetable {
        push("通用", "临时拼音", on_off(cur.temp_pinyin), on_off(new.temp_pinyin));
        if !matches!(new.temp_key, Trigger::Keep(_)) {
            push("通用", "临时拼音触发键", trigger_text(&cur.temp_key, TEMP_KEYS), trigger_text(&new.temp_key, TEMP_KEYS));
        }
    }
    out
}

/// 只有选了双拼、并且布局确实变了，才写方案级布局。
pub fn layout_changed(cur: &Draft, new: &Draft) -> bool {
    new.kind == Kind::Shuangpin && new.layout != cur.layout && !new.layout.is_empty()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    pub key: String,
    pub msg: String,
}

/// 向导要占用的键：彼此重复，或在基线里被别的动词占着、且那个动词本次不会让出，即为冲突。
///
/// ★ 判据是「占用方会不会让出」，不是「占用方是不是向导管的动词」：拼音用户把快捷输入改到
/// 反引号时，临时拼音不在拼音用户的向导里，不会让出，覆盖它就是悄悄删掉临拼绑定。
pub fn key_conflicts(d: &Draft, base: &Config) -> Vec<Conflict> {
    let cur = Draft::from_current(base, &[], &d.layout);
    let moving = |verb: &str| match verb {
        VERB_QUICK => d.quick != cur.quick && !matches!(d.quick, Trigger::Keep(_)),
        VERB_TEMP_PINYIN => {
            d.kind == Kind::Codetable
                && d.temp_key != cur.temp_key
                && !matches!(d.temp_key, Trigger::Keep(_))
        }
        _ => false,
    };
    let label = |verb: &str| match verb {
        VERB_QUICK => "快捷输入".to_string(),
        VERB_TEMP_PINYIN => "临时拼音".to_string(),
        v => v.to_string(),
    };
    // (键, 认领的动词, 标签)；顿号不是动词绑定，动词记空串，任何占用都算冲突。
    let mut claims: Vec<(String, &str, &str)> = Vec::new();
    if let Trigger::Key(k) = &d.quick {
        claims.push((k.clone(), VERB_QUICK, "快捷输入"));
    }
    if d.kind == Kind::Codetable && d.temp_pinyin {
        if let Trigger::Key(k) = &d.temp_key {
            claims.push((k.clone(), VERB_TEMP_PINYIN, "临时拼音"));
        }
    }
    if d.slash_dun {
        claims.push(("slash".into(), "", "/ 键输出顿号"));
    }
    let ka = base.get_path(KEY_ACTIONS).and_then(|v| v.as_object());
    let mut out = Vec::new();
    for (i, (k, verb, who)) in claims.iter().enumerate() {
        if let Some((_, _, other)) = claims[..i].iter().find(|c| &c.0 == k) {
            out.push(Conflict { key: k.clone(), msg: format!("「{who}」与「{other}」用了同一个键") });
            continue;
        }
        let held = ka
            .and_then(|m| m.get(k))
            .and_then(|v| v.as_str())
            .filter(|v| *v != "none");
        if let Some(v) = held {
            if v != *verb && !moving(v) {
                out.push(Conflict { key: k.clone(), msg: format!("「{who}」的键已被「{}」占用", label(v)) });
            }
        }
    }
    out
}

/// 目标默认方案对哪些「将写入的全局键」有独立设置（override）。
pub fn override_hints(items: &[ConfigItem], active: &str, ov: &HashMap<String, Vec<String>>) -> Vec<String> {
    items
        .iter()
        .filter(|it| ov.get(&it.key).is_some_and(|ids| ids.iter().any(|i| i == active)))
        .map(|it| it.key.clone())
        .collect()
}
```

- [ ] **Step 4: 运行，确认通过**

运行：`TMPDIR=/tmp/wz-$$ cargo test --no-fail-fast wizard::model`
期望：Task 1 和 Task 2 的测试全部 PASS。

- [ ] **Step 5: 变异抽查**（项目惯例）

把 `to_target` 里 `Kind::Pinyin | Kind::Shuangpin` 分支的 `if self.emoji != cur.emoji` 临时改成 `if true`，确认 `untouched_draft_produces_no_items` 变红。再把 `rebind` 的 `matches!(want, Trigger::Keep(_))` 删掉，确认 `keep_trigger_writes_nothing` 变红。两处都还原。

- [ ] **Step 6: 提交**

```bash
rustfmt --edition 2024 src/wizard/model.rs
git commit -F - --only -- src/wizard/model.rs <<'EOF'
feat(wizard): 预设、目标配置、确认摘要与键冲突判定
EOF
```

---

### Task 3: 写入编排

**Files:**
- Create: `src/wizard/apply.rs`
- Modify: `src/wizard/mod.rs`（`pub mod apply;`）

**Interfaces:**
- Consumes：`ConfigItem`；`rpc::call_core(&str, &Value) -> Result<Value, String>`。
- Produces：

```rust
pub trait CoreCalls { fn call(&mut self, method: &str, params: &Value) -> Result<Value, String>; }
pub struct RealCore;  // impl CoreCalls，转发 rpc::call_core
pub struct Plan { pub items: Vec<ConfigItem>, pub layout: Option<String>, pub active: Option<String> }
pub enum ApplyOutcome { Done, Failed { step: &'static str, error: String, done: Vec<&'static str> } }
pub fn apply_writes(core: &mut dyn CoreCalls, plan: &Plan) -> ApplyOutcome
```

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Rec { calls: Vec<String>, fail_on: Option<&'static str>, skipped: bool }
    impl CoreCalls for Rec {
        fn call(&mut self, m: &str, _p: &Value) -> Result<Value, String> {
            self.calls.push(m.to_string());
            if self.fail_on == Some(m) { return Err("boom".into()); }
            Ok(match m {
                "config.setItems" if self.skipped => json!({"skipped": [{"key": "x.y"}]}),
                "schema.getConfig" => json!({"engine": {"pinyin": {"shuangpin": {"layout": "xiaohe"}}}}),
                "schema.setActive" => json!({"ok": true}),
                _ => json!({}),
            })
        }
    }
    fn rec() -> Rec { Rec { calls: vec![], fail_on: None, skipped: false } }
    fn plan() -> Plan {
        Plan {
            items: vec![ConfigItem { key: "input.emoji.enabled".into(), value: json!(true) }],
            layout: Some("ziranma".into()),
            active: Some("shuangpin".into()),
        }
    }

    #[test]
    fn order_is_items_layout_active() {
        let mut r = rec();
        assert_eq!(apply_writes(&mut r, &plan()), ApplyOutcome::Done);
        assert_eq!(r.calls, ["config.setItems", "schema.getConfig", "schema.saveConfig", "schema.setActive"]);
    }

    #[test]
    fn empty_plan_calls_nothing() {
        let mut r = rec();
        let p = Plan { items: vec![], layout: None, active: None };
        assert_eq!(apply_writes(&mut r, &p), ApplyOutcome::Done);
        assert!(r.calls.is_empty());
    }

    #[test]
    fn skipped_items_stop_before_layout() {
        let mut r = rec();
        r.skipped = true;
        match apply_writes(&mut r, &plan()) {
            ApplyOutcome::Failed { step, done, error } => {
                assert_eq!(step, "保存配置");
                assert!(done.is_empty());
                assert!(error.contains("x.y"));
            }
            o => panic!("{o:?}"),
        }
        assert_eq!(r.calls, ["config.setItems"]);
    }

    #[test]
    fn active_failure_reports_done_steps() {
        let mut r = rec();
        r.fail_on = Some("schema.setActive");
        match apply_writes(&mut r, &plan()) {
            ApplyOutcome::Failed { step, done, .. } => {
                assert_eq!(step, "切换默认方案");
                assert_eq!(done, vec!["保存配置", "双拼布局"]);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn set_active_ok_false_is_failure() {
        struct NotOk;
        impl CoreCalls for NotOk {
            fn call(&mut self, m: &str, _p: &Value) -> Result<Value, String> {
                Ok(if m == "schema.setActive" { json!({"ok": false}) } else { json!({}) })
            }
        }
        let p = Plan { items: vec![], layout: None, active: Some("pinyin".into()) };
        assert!(matches!(apply_writes(&mut NotOk, &p), ApplyOutcome::Failed { .. }));
    }
}
```

- [ ] **Step 2: 运行，确认失败**

运行：`TMPDIR=/tmp/wz-$$ cargo test --no-fail-fast wizard::apply`
期望：编译失败。

- [ ] **Step 3: 实现**

```rust
//! 向导写入编排。顺序：全局键 → 双拼布局（方案级） → 默认方案。
//! `schema.setActive` 要求目标方案已启用，而启用表随第一步的 setItems 落盘，所以它必须排在最后。

use serde_json::{Value, json};

use crate::config::ConfigItem;

pub trait CoreCalls {
    fn call(&mut self, method: &str, params: &Value) -> Result<Value, String>;
}

pub struct RealCore;

impl CoreCalls for RealCore {
    fn call(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        crate::rpc::call_core(method, params)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub items: Vec<ConfigItem>,
    pub layout: Option<String>,
    pub active: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApplyOutcome {
    Done,
    Failed { step: &'static str, error: String, done: Vec<&'static str> },
}

pub fn apply_writes(core: &mut dyn CoreCalls, plan: &Plan) -> ApplyOutcome {
    let mut done = Vec::new();
    let fail = |step, error, done: &Vec<&'static str>| ApplyOutcome::Failed { step, error, done: done.clone() };

    if !plan.items.is_empty() {
        let items: Vec<Value> = plan.items.iter().map(|it| json!({"key": it.key, "value": it.value})).collect();
        match core.call("config.setItems", &json!({ "items": items })) {
            Ok(resp) => {
                let skipped: Vec<String> = resp
                    .get("skipped")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|it| it.get("key").and_then(|k| k.as_str())).map(str::to_string).collect())
                    .unwrap_or_default();
                if !skipped.is_empty() {
                    return fail("保存配置", format!("服务端拒绝了 {} 项：{}", skipped.len(), skipped.join(", ")), &done);
                }
            }
            Err(e) => return fail("保存配置", e, &done),
        }
        done.push("保存配置");
    }

    if let Some(layout) = &plan.layout {
        // 与方案设置对话框同一写法：取三层合并后的方案配置，改布局叶子，整份 saveConfig。
        let mut cfg = match core.call("schema.getConfig", &json!({ "id": "shuangpin" })) {
            Ok(v) if v.is_object() => v,
            Ok(_) => json!({}),
            Err(e) => return fail("双拼布局", e, &done),
        };
        set_leaf(&mut cfg, &["engine", "pinyin", "shuangpin", "layout"], json!(layout));
        if let Err(e) = core.call("schema.saveConfig", &json!({ "id": "shuangpin", "cfg": cfg })) {
            return fail("双拼布局", e, &done);
        }
        done.push("双拼布局");
    }

    if let Some(id) = &plan.active {
        match core.call("schema.setActive", &json!({ "id": id })) {
            Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => done.push("切换默认方案"),
            Ok(_) => return fail("切换默认方案", "方案未能加载".into(), &done),
            Err(e) => return fail("切换默认方案", e, &done),
        }
    }
    ApplyOutcome::Done
}

fn set_leaf(v: &mut Value, path: &[&str], leaf: Value) {
    let mut cur = v;
    for (i, k) in path.iter().enumerate() {
        let Some(obj) = cur.as_object_mut() else { return };
        if i + 1 == path.len() {
            obj.insert((*k).to_string(), leaf);
            return;
        }
        cur = obj.entry(*k).or_insert_with(|| json!({}));
        if !cur.is_object() {
            *cur = json!({});
        }
    }
}
```

先确认一点：`schema_manager.rs` 里 `settings_cfg` 的来源确实是 `schema.getConfig` 的整份回包（`dialogs/schema_manager.rs:2390` 一带），再定 `saveConfig` 传整份 cfg 的写法。如果那边实际传的是「仅差异」，就照那边的做法改，并把这里的注释改对。`set_leaf` 和 `schema_manager.rs:3522` 的私有 `set_json_path` 是同一个逻辑：把那个函数提成 `pub(crate)` 后在这里复用，不要再写一份。

- [ ] **Step 4: 运行，确认通过**

运行：`TMPDIR=/tmp/wz-$$ cargo test --no-fail-fast wizard::apply`
期望：5 个测试 PASS。

- [ ] **Step 5: 提交**

```bash
rustfmt --edition 2024 src/wizard/apply.rs src/wizard/mod.rs
git add -- src/wizard/apply.rs
git commit -F - --only -- src/wizard/apply.rs src/wizard/mod.rs src/dialogs/schema_manager.rs <<'EOF'
feat(wizard): 写入编排（setItems → 双拼布局 → 默认方案，失败如实报告已完成步骤）
EOF
```

---

### Task 4: 首启标志与自动弹出判定

**Files:**
- Modify: `src/setting_config.rs:82`（`UiStateConfig`）、`src/state.rs`（新方法与信号）
- Create（追加）: `src/wizard/mod.rs`（`should_auto_open`）

**Interfaces:**
- Produces：

```rust
// setting_config.rs
pub struct UiStateConfig { ..., #[serde(default)] pub wizard_dismissed: bool }
// wizard/mod.rs
pub fn should_auto_open(dismissed: bool, connected: bool) -> bool
// state.rs (AppState)
pub wizard_visible: Signal<bool>,
pub wizard_dont_show: Signal<bool>,
pub fn save_wizard_dismissed(&mut self, on: bool)
```

- [ ] **Step 1: 写失败测试**

`src/setting_config.rs` 的测试模块（照抄同文件已有的反序列化测试写法）：

```rust
#[test]
fn wizard_dismissed_defaults_false_for_old_files() {
    let c: SettingConfig = toml::from_str("[ui]\nhide_disabled_schemas = true\n").unwrap();
    assert!(!c.ui.wizard_dismissed);
}
```

如果顶层类型不叫 `SettingConfig`，用 `setting_config.rs` 里 `save`/`load` 实际操作的那个类型名。

`src/wizard/mod.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auto_open_only_when_not_dismissed_and_connected() {
        assert!(should_auto_open(false, true));
        assert!(!should_auto_open(true, true));
        assert!(!should_auto_open(false, false));
    }
}
```

- [ ] **Step 2: 运行，确认失败**

运行：`TMPDIR=/tmp/wz-$$ cargo test --no-fail-fast wizard_dismissed auto_open`
期望：编译失败。

- [ ] **Step 3: 实现**

`setting_config.rs` 的 `UiStateConfig` 末尾：

```rust
    /// 设置向导「下次不再弹出」/ 已完成。缺省 false ⇒ 所有用户（含升级上来的）弹一次。
    #[serde(default)]
    pub wizard_dismissed: bool,
```

`wizard/mod.rs`：

```rust
/// 启动后是否自动弹出向导。core 连不上时不弹：向导预填需要真实配置，拿空配置预填会误导。
pub fn should_auto_open(dismissed: bool, connected: bool) -> bool {
    !dismissed && connected
}
```

`state.rs`：
- 在 `AppState` 结构体的 `update_dialog_visible` 旁边加 `pub wizard_visible: Signal<bool>,` 和 `pub wizard_dont_show: Signal<bool>,`。
- 初始化处（`update_dialog_visible: signal(false),` 旁边）写 `wizard_visible: signal(false),`，以及 `wizard_dont_show: signal(setting_load.config.ui.wizard_dismissed),`。初始化变量名以那一带实际为准，`save_show_hidden_schemas` 读的也是同一个来源。
- 方法，放在 `save_show_hidden_schemas` 之后：

```rust
    /// 持久化设置向导「下次不再弹出」。
    pub fn save_wizard_dismissed(&mut self, on: bool) {
        self.save_ui_state(|ui| ui.wizard_dismissed = on);
    }
```

- [ ] **Step 4: 运行，确认通过**

运行：`TMPDIR=/tmp/wz-$$ cargo test --no-fail-fast wizard`，以及 `cargo test --no-fail-fast setting_config`
期望：PASS。

- [ ] **Step 5: 提交**

先看 `git diff -- src/state.rs src/setting_config.rs`，确认里面只有本任务的 hunk；混进别人的就按 hunk 进暂存区。

```bash
git commit -F - --only -- src/setting_config.rs src/state.rs src/wizard/mod.rs <<'EOF'
feat(wizard): 首启标志 wizard_dismissed 与自动弹出判定
EOF
```

---

### Task 5: 向导对话框、入口、首启接线

**Files:**
- Create: `src/wizard/view.rs`
- Modify: `src/wizard/mod.rs`（`pub mod view;`，以及 `open_wizard`）、`src/state.rs`（向导会话状态）、`src/shell.rs`（挂对话框和底部入口）、`src/main.rs`（apply_loaded 之后自动弹出）

**Interfaces:**
- Consumes：Task 1–4 的全部产物；`AppState::{base_config, schemas, capabilities.map_keys(), override_summary, is_dirty(), save(), apply_loaded()}`；`LoadedState::fetch()`；windui 的 `Element::{dialog, radio, checkbox, switch, dropdown, button, label, col, row}`。
- Produces：

```rust
// state.rs
pub enum WizardPhase { Dirty, Step(u8) /*1..=4*/, Confirm, Result }
pub struct WizardSession { pub cur: Draft, pub draft: Draft, pub layouts: Vec<(String, String)>, pub outcome: Option<ApplyOutcome> }
pub wizard_phase: Signal<WizardPhase>,
pub wizard: Rc<RefCell<Option<WizardSession>>>,
pub wizard_rev: Signal<u64>,   // 草稿每次修改 +1，驱动正文重算
// wizard/mod.rs
pub fn open_wizard(app: &Rc<RefCell<AppState>>)
pub fn maybe_auto_open(app: &Rc<RefCell<AppState>>, connected: bool)
pub fn finish(app: &Rc<RefCell<AppState>>)   // 写入并重灌
pub fn close(app: &Rc<RefCell<AppState>>)    // 不写 core，只按勾选写 wizard_dismissed
// wizard/view.rs
pub fn build_wizard_dialog(app: &Rc<RefCell<AppState>>) -> Element
```

- [ ] **Step 1: 写失败测试**（`src/wizard/mod.rs` 的 tests；要用到 `AppState` 的测试，照 `state.rs` 里已有的 `WIND_RPC_MOCK` 测试写法搭环境）

```rust
/// Review Focus #1：向导写入后，设置页 writer 的状态（schema_mgr.enabled_order / key_actions、
/// punct cells）必须跟上，否则用户接着点「保存」会把向导结果改回去。
#[test]
fn after_finish_settings_save_does_not_revert_wizard() {
    let _g = crate::state::tests_env_lock();     // 用 state.rs 测试同一把 ENV_LOCK（必要时提成 pub(crate)）
    unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
    let app = crate::state::tests_app_state();     // 若无现成构造器：照 state.rs:5139 一带的写法造一个并 apply_loaded
    open_wizard(&app);
    {
        let st = app.borrow();
        let mut s = st.wizard.borrow_mut();
        let s = s.as_mut().unwrap();
        s.draft = s.draft.with_preset(model::Kind::Pinyin, &st.schemas);
    }
    let base_before = app.borrow().base_config.clone();
    // 写入走 mock：mock 不落盘，所以这里只断言「重灌后设置页的 writer 对重灌后的基线不产生 diff」
    finish(&app);
    assert!(!app.borrow().is_dirty(), "重灌后设置页不应判脏");
    let _ = base_before;
}

#[test]
fn close_does_not_call_core_and_respects_checkbox() {
    let _g = crate::state::tests_env_lock();
    unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
    let app = crate::state::tests_app_state();
    open_wizard(&app);
    app.borrow().wizard_dont_show.set(true);
    close(&app);
    assert!(!app.borrow().wizard_visible.get());
    assert!(app.borrow().setting_config.ui.wizard_dismissed);
}

#[test]
fn open_when_dirty_starts_at_dirty_phase() {
    let _g = crate::state::tests_env_lock();
    unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
    let app = crate::state::tests_app_state();
    crate::state::tests_make_dirty(&app);      // 改一个 bool 信号即可：照 state.rs 判脏测试的写法
    open_wizard(&app);
    assert_eq!(app.borrow().wizard_phase.get(), crate::state::WizardPhase::Dirty);
}
```

先搜一下 `state.rs` 测试模块里有没有现成的「造 AppState + apply_loaded(mock)」辅助函数，以及「把某个字段改脏」的写法（关窗确认那组测试里有）。有就直接用；没有就把那段搭建代码提成 `#[cfg(test)] pub(crate) fn`，名字按上面的 `tests_app_state` / `tests_make_dirty` / `tests_env_lock`。`setting_config::save` 在测试里不能写到真实用户目录：先看 `setting_config.rs` 的路径解析有没有测试用的环境变量覆盖，有就用；没有就让 `close` 的测试只断言内存里的 `setting_config.ui.wizard_dismissed`，并确认 `save` 失败只会 `warn`，不会 panic（`save_ui_state` 本来就是这么处理的）。

- [ ] **Step 2: 运行，确认失败**

运行：`TMPDIR=/tmp/wz-$$ cargo test --no-fail-fast wizard::tests`
期望：编译失败，报 `open_wizard` / `finish` / `close` 未定义。

- [ ] **Step 3: 实现会话逻辑**（`wizard/mod.rs`）

```rust
use std::cell::RefCell;
use std::rc::Rc;

use crate::state::{AppState, LoadedState, WizardPhase, WizardSession};

/// 打开向导：按当前基线预填。设置页有未保存修改时先停在 Dirty 段。
pub fn open_wizard(app: &Rc<RefCell<AppState>>) {
    let st = app.borrow();
    let layouts: Vec<(String, String)> = crate::rpc::call_core("schema.layouts", &serde_json::json!({}))
        .ok()
        .and_then(|v| v.as_array().cloned())
        .map(|a| {
            a.iter()
                .filter_map(|x| Some((x.get("id")?.as_str()?.to_string(), x.get("name")?.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let layout = crate::rpc::call_core("schema.getConfig", &serde_json::json!({ "id": "shuangpin" }))
        .ok()
        .and_then(|v| v.pointer("/engine/pinyin/shuangpin/layout").and_then(|x| x.as_str()).map(str::to_string))
        .unwrap_or_else(|| "xiaohe".into());
    let cur = model::Draft::from_current(&st.base_config, &st.schemas, &layout);
    *st.wizard.borrow_mut() = Some(WizardSession { draft: cur.clone(), cur, layouts, outcome: None });
    st.wizard_phase.set(if st.is_dirty() { WizardPhase::Dirty } else { WizardPhase::Step(1) });
    st.wizard_rev.set(st.wizard_rev.get().wrapping_add(1));
    st.wizard_visible.set(true);
}

pub fn maybe_auto_open(app: &Rc<RefCell<AppState>>, connected: bool) {
    let dismissed = app.borrow().setting_config.ui.wizard_dismissed;
    if should_auto_open(dismissed, connected) {
        open_wizard(app);
    }
}

/// 关闭：不碰 core，只按勾选落 `wizard_dismissed`。
pub fn close(app: &Rc<RefCell<AppState>>) {
    let mut st = app.borrow_mut();
    let dont = st.wizard_dont_show.get();
    st.save_wizard_dismissed(dont);
    st.wizard_visible.set(false);
    *st.wizard.borrow_mut() = None;
}

/// 完成：写入 → 整体重灌（Review Focus #1：只 reload() 不刷新 schema_mgr）→ 落标志。
pub fn finish(app: &Rc<RefCell<AppState>>) {
    let plan = {
        let st = app.borrow();
        let s = st.wizard.borrow();
        let Some(s) = s.as_ref() else { return };
        apply::Plan {
            items: s.draft.items(&st.base_config, &st.capabilities.map_keys()),
            layout: model::layout_changed(&s.cur, &s.draft).then(|| s.draft.layout.clone()),
            active: (s.draft.active != s.cur.active).then(|| s.draft.active.clone()),
        }
    };
    let outcome = apply::apply_writes(&mut apply::RealCore, &plan);
    let mut st = app.borrow_mut();
    // 失败也重灌：已成功的步骤已经落盘，设置页要反映真实状态
    st.apply_loaded(LoadedState::fetch());
    if outcome == apply::ApplyOutcome::Done {
        st.save_wizard_dismissed(true);
        st.wizard_visible.set(false);
        *st.wizard.borrow_mut() = None;
        st.feedback.set(Some((crate::state::ToastKind::Success, "设置向导：已应用".into())));
    } else {
        if let Some(s) = st.wizard.borrow_mut().as_mut() {
            s.outcome = Some(outcome);
        }
        st.wizard_phase.set(WizardPhase::Result);
    }
}
```

`schema.layouts` 的回包形状以 `dialogs/schema_manager.rs:1954` 的解析为准，照抄那边的解析，或者把它提成共用函数，不要自己猜字段名。`ToastKind` 和 `feedback` 的路径以 `state.rs` 为准。`apply_loaded` 是同步拉取，正常本机耗时是几毫秒到几十毫秒级，可以接受；如果靶机上感觉卡顿，再改成走 channel。

`state.rs` 加类型和字段（放在 `update_dialog_visible` 旁边）：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardPhase { Dirty, Step(u8), Confirm, Result }

pub struct WizardSession {
    pub cur: crate::wizard::model::Draft,
    pub draft: crate::wizard::model::Draft,
    pub layouts: Vec<(String, String)>,
    pub outcome: Option<crate::wizard::apply::ApplyOutcome>,
}
// AppState 字段
pub wizard_phase: Signal<WizardPhase>,
pub wizard: Rc<RefCell<Option<WizardSession>>>,
pub wizard_rev: Signal<u64>,
```

初始化成 `signal(WizardPhase::Step(1))`、`Rc::new(RefCell::new(None))`、`signal(0)`。如果 windui 的 `Signal<T>` 要求 `T: Copy`，`WizardPhase` 已经是 Copy，没问题。

- [ ] **Step 4: 运行会话测试，确认通过**

运行：`TMPDIR=/tmp/wz-$$ cargo test --no-fail-fast wizard`
期望：PASS。如果 `after_finish_settings_save_does_not_revert_wizard` 红了，说明 `apply_loaded` 之后仍有 writer 跟新基线不同构。去查是哪个 writer：`save()` 里有一条 `tracing::debug!("save: diff …")`，看它列出的 key，然后修正重灌路径，**不要改测试**。

- [ ] **Step 5: 实现界面**（`wizard/view.rs`）

结构照 `dialogs/update_dialog.rs:27-63` 的写法：

```rust
pub fn build_wizard_dialog(app: &Rc<RefCell<AppState>>) -> Element {
    let (visible, phase, rev) = { let st = app.borrow(); (st.wizard_visible, st.wizard_phase, st.wizard_rev) };
    let is = move |p: WizardPhase| move || phase.get() == p;
    let frame = Element::col()
        .width(640)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .shadow(Shadow::new(0.0, 16.0, 40.0, Color::rgba(0, 0, 0, 130)))
        .child(title_row(phase))                       // 「设置向导 · 第 N/4 步」+ 关闭按钮（调 close）
        .child(Element::divider())
        .child(dirty_body(app).visible_when(is(WizardPhase::Dirty)))
        .child(step1_body(app, rev).visible_when(is(WizardPhase::Step(1))))
        .child(step2_body(app, rev).visible_when(is(WizardPhase::Step(2))))
        .child(step3_body(app, rev).visible_when(is(WizardPhase::Step(3))))
        .child(step4_body(app, rev).visible_when(is(WizardPhase::Step(4))))
        .child(confirm_body(app, rev).visible_when(is(WizardPhase::Confirm)))
        .child(result_body(app, rev).visible_when(is(WizardPhase::Result)))
        .child(Element::divider())
        .child(footer(app, phase, rev));               // 左：「下次不再弹出」checkbox(wizard_dont_show)；右：上一步/下一步/完成
    Element::dialog(visible, frame)
}
```

各段的要点：

- **dirty_body**：说明「设置页有未保存的修改」，给三个按钮：
  - 「保存并继续」：调 `app.borrow_mut().save()`，成功后重新调用 `open_wizard`，按新基线重新预填。
  - 「放弃修改」：调现有的「重新加载 / 丢弃」路径，即 `reload()` 加 `schema_mgr.clear_pending_edits()`，和设置页「重新加载」按钮同一条调用，去 `shell.rs` 或 `pages` 里找那颗按钮的 on_click 照抄；然后 `open_wizard`。
  - 「取消」：调 `close`。
- **step1_body**：
  - 主类型用 `Element::radio("五笔 / 码表", kind_sig, 0)` 这样三个。`kind_sig` 变化时：`draft = draft.with_preset(kind, schemas)`，然后 `wizard_rev += 1`。按 `update_dialog.rs` 里 `auto_check_persister` 的办法，用 `reactive_effect` 监听 `kind_sig.version()`。
  - 选双拼时显示 `Element::dropdown(layout_labels, layout_sig)`。`layouts` 为空时只显示当前值的 label，不建下拉（Review Focus #5）。
  - 方案勾选：对 `schemas` 里非 `hidden` 的方案，加上 `draft.enabled` 里有、但不在 `schemas` 中的自定义 id，每个一个 `Element::checkbox(name, sig)`。主类型对应的那个方案勾上并禁用：用现有的禁用写法，在 `manifest.rs` 里搜 `enabled_when` 生成的禁用控件写法。`english`、`stroke` 是 `hidden` 时也列出来，因为规格要求列出但不预勾。
- **step2_body**：按 `draft.kind` 用 `visible_when` 分出码表、拼音两组开关（`Element::switch`），每个开关通过 `reactive_effect` 回写 `draft`。四对模糊音只在 `fuzzy` 开时 `visible_when`。「允许按全拼输入」只在双拼时显示。
- **step3_body**：
  - 快捷输入用 `dropdown`，选项为 `QUICK_KEYS` 的标签加「不启用」；当 `Trigger::Keep` 时，首项是「保持现状（…）」。
  - 「/ 键输出顿号」用 switch。
  - 临时拼音的 switch 和触发键 dropdown 只在 `Kind::Codetable` 时显示。
  - 底部用 `Role::Danger` 色列出 `key_conflicts(&draft, &base)` 的每条 `msg`。有冲突时 footer 的「下一步」不可点。
- **step4_body**：
  - 按 group 分组列出 `summary(&cur, &draft, names, layout_label)`，每行为「标签  旧 → 新」。`names` 用 `schemas` 的 id→name 映射，`layout_label` 用 `layouts`。
  - 对 `override_hints(&items, &draft.active, &override_summary.get())` 里的每个 key，在对应行下方加一行灰字：「方案 {name} 有独立设置，此项修改对它不生效，可在方案设置里调整」。key→行的对应在 `summary` 里加一个 `keys: Vec<&'static str>` 字段来做，并补一条测试：每个 Change 的 keys 都出现在 items 里。或者更简单：不对应到行，在列表底部汇总列出这些提示。**选更简单的那个**。
  - 摘要为空时显示「当前配置已符合所选，无需修改」，「完成」只负责关闭（等同于 `close`，但 `wizard_dismissed` 写 true）。
- **confirm_body**：「将覆盖以下 N 项现有设置，确定吗？」，按钮为「确定」→ `finish`，「返回」→ `Step(4)`。N 取 `summary` 的长度。
- **result_body**：用 `ApplyOutcome::Failed` 显示失败的步骤、错误原文，以及已完成的步骤列表。按钮「关闭」→ `close`。
- **footer**：
  - Step(1) 不显示「上一步」；Step(4) 的主按钮是「完成」：摘要非空时进 `Confirm`，为空时直接关闭。
  - 「下次不再弹出」checkbox 在每一段都显示，Result 段除外。

正文要随草稿重算，靠 `wizard_rev`：各段内部的动态文本和 `visible_when` 闭包读 `rev.get()` 后再借用 `wizard` 会话。参照 `update_dialog` 里 `message` 信号驱动 label 的方式；如果 label 需要 `Signal<String>`，就用 `rev.map(move |_| …)` 派生，`override_summary.map` 在 `manifest.rs:752` 就是这么用的。

- [ ] **Step 6: 接线**

`shell.rs`：
- 根 stack 在 `.child(build_update_dialog(app))` 旁边加 `.child(crate::wizard::view::build_wizard_dialog(app))`。
- 侧栏：把 `.child(nav)` 改成 `.child(nav.weight(1.0))`，然后加一行底部入口：

```rust
        .child(wizard_entry(app))
```

`wizard_entry` 的样式照 `build_nav` 里「未选中态」那一行（`off`）写：高 36，chip + label「设置向导」，`on_click` 调 `crate::wizard::open_wizard(&app)`。core 未连接时（`state.connected` 或同义字段），改成 `feedback` 提示「未连接输入法服务，无法使用设置向导」（Review Focus #4）。图标：`crate::icons` 里有合适的就复用（比如魔法棒、星星之类）；没有就用 `nav_icon("关于")` 那一类的现有图标，不要为此新加 SVG。

要确认一点：`build_nav` 返回的 `Element::scroll().fill()` 加上 `.weight(1.0)` 之后，在 col 里是撑满剩余高度的。如果 `fill()` 已经抢占了全部高度、把入口挤出视野，就把 `fill()` 换成 `width_match().weight(1.0)`。

`main.rs` 的 channel 回调（`apply_loaded(data)` 之后）：

```rust
        let connected = app_state_cb.borrow().connected_flag();  // 用 apply_loaded 写入的那个连接状态字段
        crate::wizard::maybe_auto_open(&app_state_cb, connected);
```

`data.connected` 在 `apply_loaded(data)` 之后已经被 move 掉了，所以要在调用前先取出来：`let connected = data.connected;`。

- [ ] **Step 7: 编译与全量测试**

运行：`TMPDIR=/tmp/wz-$$ cargo build && TMPDIR=/tmp/wz-$$ cargo test --no-fail-fast 2>&1 | tail -30`
期望：编译通过；全量通过。通过数应该比 Task 4 之后的基线多 3。

运行：`cargo clippy --all-targets 2>&1 | grep -E "^(warning|error)" | sort | uniq -c`
期望：没有新增 warning。对比基线：开工前在 main 上先跑一次 clippy，输出存到 scratchpad。

- [ ] **Step 8: 本机 mock 跑一遍看界面**

运行设置端的 mock 模式（`WIND_RPC_MOCK=1`，启动参数见 README「命令行」一节），删掉临时配置目录里的 `wind_setting.toml` 后启动，确认：
- 自动弹出；
- 三种主类型都能走到第 4 步，摘要内容正确；
- 键冲突时「下一步」不可点；
- 关闭后 `wind_setting.toml` 里有 `wizard_dismissed` 并且值与勾选一致；
- 导航底部入口能重新打开向导。

截图存到 scratchpad。

- [ ] **Step 9: 提交**

逐个看 `git diff -- src/shell.rs src/main.rs src/state.rs`，确认只有本任务的 hunk。

```bash
rustfmt --edition 2024 src/wizard/*.rs
git add -- src/wizard/view.rs
git commit -F - --only -- src/wizard/view.rs src/wizard/mod.rs src/state.rs src/shell.rs src/main.rs <<'EOF'
feat(wizard): 设置向导对话框、导航底部入口与首启自动弹出
EOF
```

---

### Task 6: 审查、靶机验证、收尾

- [ ] **Step 1: 独立审查**：派 code-reviewer 审 Task 1–5 的提交，重点看 Review Focus 那 5 条，以及 `to_target` 有没有写出不可见项。逐条核实，修掉。
- [ ] **Step 2: 靶机部署**：只推设置端：用 dm3 构建后手工改名让路，再 scp 过去。
- [ ] **Step 3: 靶机验证**：靶机没法跑 GUI 自动化，给维护者以下判据，人工验证：
  1. 删掉 `wind_setting.toml` 里的 `wizard_dismissed` 后打开设置，向导弹出。
  2. 选全拼走完全程，确认后：方案页默认方案是全拼；打 `xiao` 时有 emoji 候选；`config.toml` 里只多了摘要里列出的那几项。
  3. 再打开向导，选五笔，打开拼音反查：在五笔拼音混输里打拼音，候选旁出现五笔编码。
  4. 打开「/ 键输出顿号」：按 `/` 出「、」。
  5. 中途关闭向导：`config.toml` 的修改时间不变。
  6. 向导完成后，在设置页随便改一项再保存：向导写入的那些值不会被改回去。
- [ ] **Step 4: 文档站**：在 WindInputDocs 的设置说明里加一节「设置向导」。只改 `content/docs`，不手写 `releases.json`。
- [ ] **Step 5: 更新设计稿状态**：在 `docs/design/setup-wizard.md` 末尾记一行实现提交区间和靶机验证结论。用 `--only` 提交。
