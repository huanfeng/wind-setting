//! `compat.*` 的 mock 应答（`WIND_RPC_MOCK=1` 下离线开发 / 截图 / 页面测试用）。
//!
//! 应答体是**核心真实输出生成后入库**的 JSON（`src/mockdata/compat/`），不是手写：手写形状会和
//! 核心悄悄漂移，而页面在 mock 下一切正常、接上真核心才发现字段对不上。生成器与漂移守护测试见
//! 本文件的 `tests`：核心改了形状，守护测试变红，提示跑一遍生成器。

use serde_json::{Value, json};

const SCHEMA: &str = include_str!("../../mockdata/compat/schema.json");
const LIST_APPS: &str = include_str!("../../mockdata/compat/list_apps.json");
const LIST_SCOPE: &str = include_str!("../../mockdata/compat/list_initial_mode_scope.json");
const LIST_NEWLINE: &str = include_str!("../../mockdata/compat/list_commit_newline.json");
const IMPORT_PREVIEW: &str = include_str!("../../mockdata/compat/import_preview.json");
const EXPORT_USER: &str = include_str!("../../mockdata/compat/export_user.toml");

/// 备份文件名（与核心的命名同形）。
const BACKUP: &str = "compat.toml.bak-1700000000";

// mock 里「写完就变」的最小可变层：用户新增的规则及其已设置的字段（键与值）。字段清空时整条规则消失。核心里 normalize 成空的补丁不会落行，
// 进程名为 `MOCK_NOOP_PROCESS` 时照此模拟。线程内独立（每个测试各一份）。
thread_local! {
    static CREATED: std::cell::RefCell<Vec<(String, Vec<(String, Value)>)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// 见 [`CREATED`]：这个进程名的 upsert 应答成功却不落行。
pub(crate) const MOCK_NOOP_PROCESS: &str = "noop.exe";

/// 在静态的 apps 列表上叠加 [`CREATED`] 里的新规则：字段视图按已设置的键逐个生成
/// （已设置 = 用户来源，其余 = 默认来源、类型化默认值），与核心的形状一致。
fn list_apps_with_created() -> Value {
    let mut v = parse(LIST_APPS);
    let schema = parse(SCHEMA);
    let kinds: Vec<(String, String)> = schema["fields"]
        .as_array()
        .map(|fs| {
            fs.iter()
                .filter(|f| f["section"] == "apps")
                .map(|f| {
                    (
                        f["key"].as_str().unwrap_or("").to_string(),
                        f["kind"].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let created = CREATED.with(|c| c.borrow().clone());
    if let Some(rules) = v.get_mut("rules").and_then(Value::as_array_mut) {
        for (p, set) in created {
            if set.is_empty() || rules.iter().any(|r| r["process"] == p.as_str()) {
                continue;
            }
            let Some(mut r) = rules.first().cloned() else {
                continue;
            };
            let mut fields = serde_json::Map::new();
            for (k, kind) in &kinds {
                let typed = match kind.as_str() {
                    "bool" => json!(false),
                    "int" => json!(0),
                    "text_list" => json!([]),
                    _ => Value::Null,
                };
                let fv = match set.iter().find(|(sk, _)| sk == k) {
                    Some((_, val)) => json!({
                        "source": "user", "value": val, "systemValue": Value::Null, "userValue": val
                    }),
                    None => json!({
                        "source": "default", "value": typed, "systemValue": Value::Null, "userValue": Value::Null
                    }),
                };
                fields.insert(k.clone(), fv);
            }
            r["process"] = json!(p);
            r["state"] = json!("user");
            r["hasUserEntry"] = json!(true);
            r["system"] = Value::Null;
            r["fields"] = Value::Object(fields);
            rules.push(r);
        }
    }
    v
}

/// 字段的控件类型（取自 mock 的 schema）；未知字段返回空串。
fn kind_of(key: &str) -> String {
    parse(SCHEMA)["fields"]
        .as_array()
        .and_then(|fs| {
            fs.iter()
                .find(|f| f["section"] == "apps" && f["key"] == key)
        })
        .and_then(|f| f["kind"].as_str())
        .unwrap_or("")
        .to_string()
}

/// 把一次写入折算进 [`CREATED`]。
fn mutate_created(method: &str, process: &str, params: &Value) {
    CREATED.with(|c| {
        let mut c = c.borrow_mut();
        match method {
            "compat.upsert" if process != MOCK_NOOP_PROCESS => {
                let Some(patch) = params["patch"].as_object() else {
                    return;
                };
                let pos = c.iter().position(|(p, _)| p == process);
                let i = pos.unwrap_or_else(|| {
                    c.push((process.to_string(), Vec::new()));
                    c.len() - 1
                });
                for (k, v) in patch {
                    c[i].1.retain(|(x, _)| x != k);
                    // 核心的规范化：纯开关的 `false`、数字 `0` 与没写无差别，被丢掉；
                    // 三态开关（tri_bool）的显式 `false` 是有意义的，保留。
                    let plain_default = match kind_of(k).as_str() {
                        "bool" => *v == json!(false),
                        "int" => *v == json!(0),
                        _ => false,
                    };
                    if !v.is_null() && !plain_default {
                        c[i].1.push((k.clone(), v.clone()));
                    }
                }
            }
            "compat.resetField" => {
                if let (Some((_, keys)), Some(k)) = (
                    c.iter_mut().find(|(p, _)| p == process),
                    params["key"].as_str(),
                ) {
                    keys.retain(|(x, _)| x != k);
                }
            }
            _ => {}
        }
    });
}

fn parse(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or(Value::Null)
}

/// 静态应答：写方法只回「成功」，不改内容（页面写完会重拉 `compat.list`，看到的仍是同一份数据）。
/// 需要「写完就变」的交互断言请看发出去的请求（`rpc::take_mock_calls`），而不是 mock 的状态。
pub(crate) fn reply(method: &str, params: &Value) -> Value {
    match method {
        "compat.schema" => parse(SCHEMA),
        "compat.list" => match params.get("section").and_then(Value::as_str) {
            Some("initial_mode_scope") => parse(LIST_SCOPE),
            Some("commit_newline") => parse(LIST_NEWLINE),
            _ => list_apps_with_created(),
        },
        "compat.export" => json!({ "content": EXPORT_USER }),
        "compat.import" => {
            let dry = params
                .get("dryRun")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut v = parse(IMPORT_PREVIEW);
            v["applied"] = json!(!dry);
            if !dry {
                v["backup"] = json!(BACKUP);
            }
            v
        }
        "compat.resetAll" => json!({ "ok": true, "backup": BACKUP }),
        "compat.reset" => {
            if let Some(p) = params.get("process").and_then(Value::as_str) {
                CREATED.with(|c| c.borrow_mut().retain(|(x, _)| x != p));
            }
            json!({ "ok": true, "removed": true, "rule": null })
        }
        "compat.upsert" | "compat.resetField" | "compat.setDisabled" => {
            if let Some(p) = params.get("process").and_then(Value::as_str) {
                mutate_created(method, p, params);
            }
            json!({ "ok": true, "rule": null })
        }
        _ => Value::Null,
    }
}

#[cfg(test)]
pub(crate) mod fixture {
    //! 生成 mock 数据用的夹具：故意覆盖每一种规则状态与每一种字段来源，截图才看得全。

    /// 系统层（出厂）。
    pub const SYSTEM: &str = r#"
[[apps]]
process = "Feishu.exe"
comment = "飞书：在线表格换单元格首键，探测坐标停在上一格"
composition_start_pair_guard = true
stale_probe_guard = true

[[apps]]
process = "Weixin.exe"
caret_use_top = true
stale_probe_guard = true

[[apps]]
process = "EXCEL.EXE"
first_show_mode = "wait"

[[apps]]
process = "wps.exe"
pin_anchor_when_start_drifts = true

[[apps]]
process = "SearchHost.exe"
host_render = true

[[apps]]
process = "QQ.exe"
composition_start_pair_guard = true

[[apps]]
process = "Dota2.exe"
host_drawn_candidates = true

[[initial_mode_scope]]
process = "explorer.exe"
classes = ["Progman", "WorkerW"]

[[initial_mode_scope]]
process = "notepad.exe"
classes = ["Notepad"]

[[commit_newline]]
process = "WINWORD.EXE"
style = "cr"
"#;

    /// 用户层：改写、取消、禁用、新增各一类。
    pub const USER: &str = r#"
[[apps]]
process = "Weixin.exe"
first_show_mode = "instant"
unset = ["stale_probe_guard"]

[[apps]]
process = "EXCEL.EXE"
auto_pair = false

[[apps]]
process = "Dota2.exe"
disabled = true

[[apps]]
process = "Tabby.exe"
smart_method = "hold_composition"
first_show_mode = "fast"
initial_mode = "english"
initial_punct = "englsh"
caret_offset_y = 4
future_key = 3

[[initial_mode_scope]]
process = "explorer.exe"
classes = ["Progman", "WorkerW", "CabinetWClass"]

[[commit_newline]]
process = "Code.exe"
style = "keep"
"#;

    /// 导入预览用的样例文本：新增 / 覆盖 / 禁用 / 无变化各一条，外加一个不认识的键与一个无效值。
    pub const IMPORT: &str = r#"
[[apps]]
process = "Brand.exe"
auto_pair = true

[[apps]]
process = "Weixin.exe"
caret_offset_x = 2

[[apps]]
process = "QQ.exe"
disabled = true

[[apps]]
process = "Feishu.exe"
composition_start_pair_guard = true

[[apps]]
process = "wps.exe"
future_key = 1
first_show_mode = "wiat"
"#;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use wind_config::compat_admin::rpc;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "wind_setting_compat_mock_{tag}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn mock_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mockdata/compat")
    }

    /// 用**核心真实的 RPC 实现**跑夹具，得到每份 mock 应答应有的样子。
    fn generate() -> Vec<(&'static str, Value)> {
        let (d, u) = (tmp("d"), tmp("u"));
        std::fs::write(d.join("compat.toml"), fixture::SYSTEM).unwrap();
        std::fs::write(u.join("compat.toml"), fixture::USER).unwrap();
        let call = |m: &str, p: Value| -> Value {
            rpc(m, &p, Some(&d), None, Some(&u))
                .unwrap_or_else(|e| panic!("{m} 失败: {e}"))
                .value
        };
        let out = vec![
            ("schema.json", call("compat.schema", json!({}))),
            (
                "list_apps.json",
                call("compat.list", json!({"section": "apps"})),
            ),
            (
                "list_initial_mode_scope.json",
                call("compat.list", json!({"section": "initial_mode_scope"})),
            ),
            (
                "list_commit_newline.json",
                call("compat.list", json!({"section": "commit_newline"})),
            ),
            (
                "import_preview.json",
                call(
                    "compat.import",
                    json!({"content": fixture::IMPORT, "mode": "merge", "dryRun": true}),
                ),
            ),
        ];
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&u);
        out
    }

    /// 生成器：`cargo test regenerate_compat_mock -- --ignored`。核心改了契约后跑一遍，再 `git diff` 看变化。
    #[test]
    #[ignore = "生成器：手动运行，重写 src/mockdata/compat/"]
    fn regenerate_compat_mock() {
        std::fs::create_dir_all(mock_dir()).unwrap();
        for (name, v) in generate() {
            let text = serde_json::to_string_pretty(&v).unwrap() + "\n";
            std::fs::write(mock_dir().join(name), text).unwrap();
        }
        std::fs::write(
            mock_dir().join("export_user.toml"),
            fixture::USER.trim_start(),
        )
        .unwrap();
    }

    /// ★ 漂移守护：入库的 mock 必须与核心真实输出一致（按 JSON 语义比，不比键序与缩进）。
    /// 红了 = 核心的 `compat.*` 契约变了，跑上面的生成器并看 diff。
    #[test]
    fn checked_in_mock_matches_the_real_core_output() {
        for (name, want) in generate() {
            let text = std::fs::read_to_string(mock_dir().join(name))
                .unwrap_or_else(|e| panic!("缺少 mock 文件 {name}（先跑生成器）: {e}"));
            let have: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                have, want,
                "{name} 与核心真实输出不一致：核心契约变了，请跑 regenerate_compat_mock"
            );
        }
        assert_eq!(
            std::fs::read_to_string(mock_dir().join("export_user.toml")).unwrap(),
            fixture::USER.trim_start(),
            "export_user.toml 与夹具不一致"
        );
    }

    /// 入库的 mock 必须能被页面的模型解析：契约漂移在这里以「解析失败」的形式暴露，
    /// 而不是页面上一片空白。
    #[test]
    fn mock_replies_parse_with_the_page_models() {
        use super::super::model::{ImportPreview, ListResult, Schema, Section};
        let schema = Schema::parse(&reply("compat.schema", &json!({}))).expect("schema");
        assert!(!schema.fields.is_empty() && !schema.groups.is_empty());
        for sec in Section::ALL {
            let l = ListResult::parse(&reply("compat.list", &json!({"section": sec.id()})))
                .expect(sec.id());
            assert!(!l.rules.is_empty(), "{} 的 mock 至少要有一条规则", sec.id());
            for r in &l.rules {
                let want: std::collections::BTreeSet<_> = schema
                    .fields_of(sec)
                    .iter()
                    .map(|f| f.key.clone())
                    .collect();
                let got: std::collections::BTreeSet<_> = r.fields.keys().cloned().collect();
                assert_eq!(
                    got,
                    want,
                    "{} / {} 的 fields 必须覆盖本段全部已登记字段",
                    sec.id(),
                    r.process
                );
            }
        }
        let p = ImportPreview::parse(&reply("compat.import", &json!({"dryRun": true})))
            .expect("preview");
        assert!(!p.applied && p.backup.is_none());
        let done = ImportPreview::parse(&reply("compat.import", &json!({"dryRun": false})))
            .expect("applied");
        assert!(done.applied && done.backup.is_some());
    }

    /// mock 要覆盖每一种规则状态与字段来源，截图与页面测试才有得看。
    #[test]
    fn mock_covers_every_rule_state_and_field_source() {
        use super::super::model::{FieldSource, ListResult, RuleState};
        let l = ListResult::parse(&reply("compat.list", &json!({"section": "apps"}))).unwrap();
        for st in [
            RuleState::System,
            RuleState::Modified,
            RuleState::User,
            RuleState::Disabled,
        ] {
            assert!(l.rules.iter().any(|r| r.state == st), "mock 缺状态 {st:?}");
        }
        for src in [
            FieldSource::Default,
            FieldSource::System,
            FieldSource::User,
            FieldSource::Cleared,
        ] {
            assert!(
                l.rules
                    .iter()
                    .flat_map(|r| r.fields.values())
                    .any(|f| f.source == src),
                "mock 缺字段来源 {src:?}"
            );
        }
        assert!(
            !l.warnings.is_empty(),
            "夹具里的无效值应产生至少一条 warning"
        );
    }
}
