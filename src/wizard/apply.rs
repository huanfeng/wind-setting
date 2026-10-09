//! 向导写入编排。顺序：全局键 → 双拼布局（方案级） → 默认方案。
//! `schema.setActive` 要求目标方案已启用，而启用表随第一步的 setItems 落盘，所以它必须排在最后。

use serde_json::{Value, json};

use crate::config::ConfigItem;
use crate::dialogs::schema_manager::set_json_path;

pub trait CoreCalls {
    fn call(&mut self, method: &str, params: &Value) -> Result<Value, String>;
}

pub struct RealCore;

impl CoreCalls for RealCore {
    fn call(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        crate::rpc::call_core(method, params)
    }
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub items: Vec<ConfigItem>,
    pub layout: Option<String>,
    pub active: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApplyOutcome {
    Done,
    Failed {
        step: &'static str,
        error: String,
        done: Vec<&'static str>,
    },
}

pub fn apply_writes(core: &mut dyn CoreCalls, plan: &Plan) -> ApplyOutcome {
    let mut done: Vec<&'static str> = Vec::new();
    let fail = |step: &'static str, error: String, done: &Vec<&'static str>| ApplyOutcome::Failed {
        step,
        error,
        done: done.clone(),
    };

    if !plan.items.is_empty() {
        let items: Vec<Value> = plan
            .items
            .iter()
            .map(|it| json!({"key": it.key, "value": it.value}))
            .collect();
        match core.call("config.setItems", &json!({ "items": items })) {
            Ok(resp) => {
                let skipped: Vec<String> = resp
                    .get("skipped")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|it| it.get("key").and_then(|k| k.as_str()))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                if !skipped.is_empty() {
                    return fail(
                        "保存配置",
                        format!("服务端拒绝了 {} 项：{}", skipped.len(), skipped.join(", ")),
                        &done,
                    );
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
        set_json_path(
            &mut cfg,
            &["engine", "pinyin", "shuangpin", "layout"],
            json!(layout),
        );
        if let Err(e) = core.call(
            "schema.saveConfig",
            &json!({ "id": "shuangpin", "cfg": cfg }),
        ) {
            return fail("双拼布局", e, &done);
        }
        done.push("双拼布局");
    }

    if let Some(id) = &plan.active {
        match core.call("schema.setActive", &json!({ "id": id })) {
            Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => {}
            Ok(_) => return fail("切换默认方案", "方案未能加载".into(), &done),
            Err(e) => return fail("切换默认方案", e, &done),
        }
    }
    ApplyOutcome::Done
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Rec {
        calls: Vec<String>,
        fail_on: Option<&'static str>,
        skipped: bool,
    }
    impl CoreCalls for Rec {
        fn call(&mut self, m: &str, _p: &Value) -> Result<Value, String> {
            self.calls.push(m.to_string());
            if self.fail_on == Some(m) {
                return Err("boom".into());
            }
            Ok(match m {
                "config.setItems" if self.skipped => json!({"skipped": [{"key": "x.y"}]}),
                "schema.getConfig" => {
                    json!({"engine": {"pinyin": {"shuangpin": {"layout": "xiaohe"}}}})
                }
                "schema.setActive" => json!({"ok": true}),
                _ => json!({}),
            })
        }
    }
    fn rec() -> Rec {
        Rec {
            calls: vec![],
            fail_on: None,
            skipped: false,
        }
    }
    fn plan() -> Plan {
        Plan {
            items: vec![ConfigItem {
                key: "input.emoji.enabled".into(),
                value: json!(true),
            }],
            layout: Some("ziranma".into()),
            active: Some("shuangpin".into()),
        }
    }

    #[test]
    fn order_is_items_layout_active() {
        let mut r = rec();
        assert_eq!(apply_writes(&mut r, &plan()), ApplyOutcome::Done);
        assert_eq!(
            r.calls,
            [
                "config.setItems",
                "schema.getConfig",
                "schema.saveConfig",
                "schema.setActive"
            ]
        );
    }

    #[test]
    fn empty_plan_calls_nothing() {
        let mut r = rec();
        let p = Plan {
            items: vec![],
            layout: None,
            active: None,
        };
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
                Ok(if m == "schema.setActive" {
                    json!({"ok": false})
                } else {
                    json!({})
                })
            }
        }
        let p = Plan {
            items: vec![],
            layout: None,
            active: Some("pinyin".into()),
        };
        assert!(matches!(
            apply_writes(&mut NotOk, &p),
            ApplyOutcome::Failed { .. }
        ));
    }
}
