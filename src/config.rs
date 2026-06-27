//! Config serde 类型 — 与 Go 后端 pkg/config (v1 结构) 和 TS 前端 settings.ts 一一对应。
//!
//! 所有字段使用 serde_json::Value 存储，通过点分路径访问，
//! 这样可以灵活处理后端返回的部分字段，无需为每个子结构定义完整类型。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 完整配置 — 以 serde_json::Value 持有，支持点分路径访问。
/// 这样做的好处：不需要为每个嵌套结构定义完整 Rust 类型，
/// 后端返回什么就存什么，读写都通过路径操作。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config(pub Value);

impl Config {
    /// 从 JSON Value 构造。
    pub fn from_value(v: Value) -> Self {
        Self(v)
    }

    /// 按点分路径获取值，如 "input.enter_behavior"。
    pub fn get_path(&self, path: &str) -> Option<&Value> {
        let parts: Vec<&str> = path.split('.').collect();
        let mut cur = &self.0;
        for part in &parts {
            cur = cur.get(part)?;
        }
        Some(cur)
    }

    /// 按点分路径获取字符串值。
    pub fn get_str(&self, path: &str) -> Option<&str> {
        self.get_path(path)?.as_str()
    }

    /// 按点分路径获取布尔值。
    pub fn get_bool(&self, path: &str) -> Option<bool> {
        self.get_path(path)?.as_bool()
    }

    /// 按点分路径获取 f64 数值。
    pub fn get_f64(&self, path: &str) -> Option<f64> {
        self.get_path(path)?.as_f64()
    }

    /// 按点分路径获取 u64 整数。
    pub fn get_u64(&self, path: &str) -> Option<u64> {
        self.get_path(path)?.as_u64()
    }

    /// 按点分路径获取 i64 整数。
    pub fn get_i64(&self, path: &str) -> Option<i64> {
        self.get_path(path)?.as_i64()
    }

    /// 按点分路径获取数组。
    pub fn get_array(&self, path: &str) -> Option<&Vec<Value>> {
        self.get_path(path)?.as_array()
    }

    /// 按点分路径设置值。
    pub fn set_path(&mut self, path: &str, value: Value) {
        let parts: Vec<&str> = path.split('.').collect();
        let mut cur = &mut self.0;
        // 确保中间路径存在
        for part in &parts[..parts.len() - 1] {
            if cur.get(part).is_none() {
                cur.as_object_mut()
                    .unwrap()
                    .insert(part.to_string(), Value::Object(serde_json::Map::new()));
            }
            cur = cur.get_mut(part).unwrap();
        }
        if let Some(obj) = cur.as_object_mut() {
            obj.insert(parts.last().unwrap().to_string(), value);
        }
    }
}

/// 配置变更项 — 用于 config.setItems 增量写入。
#[derive(Debug, Clone, Serialize)]
pub struct ConfigItem {
    pub key: String,
    pub value: Value,
}

/// 计算两个 Config 之间的差异，返回变更的路径列表。
pub fn diff_config(base: &Config, current: &Config) -> Vec<ConfigItem> {
    let mut items = Vec::new();
    diff_value("", &base.0, &current.0, &mut items);
    items
}

// 测试模块后仍有 diff_value 等私有辅助函数；toolchain 升级新增的 items_after_test_module
// lint 仅为风格性约束，此处局部豁免以保持函数就近声明。
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_get_path() {
        let cfg = Config::from_value(json!({
            "input": {
                "enter_behavior": "commit",
                "filter_mode": "smart",
                "temp_english": {
                    "enabled": true
                }
            }
        }));
        assert_eq!(cfg.get_str("input.enter_behavior"), Some("commit"));
        assert_eq!(cfg.get_str("input.filter_mode"), Some("smart"));
        assert_eq!(cfg.get_bool("input.temp_english.enabled"), Some(true));
        assert_eq!(cfg.get_str("nonexistent.path"), None);
    }

    #[test]
    fn test_set_path() {
        let mut cfg = Config::from_value(json!({ "input": { "enter_behavior": "commit" } }));
        cfg.set_path("input.enter_behavior", Value::String("clear".to_string()));
        assert_eq!(cfg.get_str("input.enter_behavior"), Some("clear"));
    }

    #[test]
    fn test_set_path_create_intermediate() {
        let mut cfg = Config::from_value(json!({}));
        cfg.set_path("a.b.c", Value::Bool(true));
        assert_eq!(cfg.get_bool("a.b.c"), Some(true));
    }

    #[test]
    fn test_diff_no_change() {
        let base = Config::from_value(json!({ "input": { "enter_behavior": "commit" } }));
        let current = Config::from_value(json!({ "input": { "enter_behavior": "commit" } }));
        let items = diff_config(&base, &current);
        assert!(items.is_empty());
    }

    #[test]
    fn test_diff_detects_change() {
        let base = Config::from_value(json!({ "input": { "enter_behavior": "commit" } }));
        let current = Config::from_value(json!({ "input": { "enter_behavior": "clear" } }));
        let items = diff_config(&base, &current);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key, "input.enter_behavior");
        assert_eq!(items[0].value, Value::String("clear".to_string()));
    }

    #[test]
    fn test_diff_ignores_base_only_keys() {
        // base 有 extra 字段（我们不管理的配置），current 没有 → 不应产出
        let base = Config::from_value(json!({
            "input": { "enter_behavior": "commit" },
            "debug": { "log_level": "info", "perf_sampling": false }
        }));
        let current = Config::from_value(json!({
            "input": { "enter_behavior": "clear" }
        }));
        let items = diff_config(&base, &current);
        // 只应产出 current 中变更的 enter_behavior，不应产出 debug.* 或 null
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key, "input.enter_behavior");
    }

    #[test]
    fn test_diff_multiple_changes() {
        let base = Config::from_value(json!({
            "input": { "enter_behavior": "commit", "filter_mode": "smart" },
            "ui": { "theme": { "style": "system" } }
        }));
        let current = Config::from_value(json!({
            "input": { "enter_behavior": "clear", "filter_mode": "smart" },
            "ui": { "theme": { "style": "dark" } }
        }));
        let items = diff_config(&base, &current);
        assert_eq!(items.len(), 2);
        let keys: Vec<&str> = items.iter().map(|i| i.key.as_str()).collect();
        assert!(keys.contains(&"input.enter_behavior"));
        assert!(keys.contains(&"ui.theme.style"));
    }
}

fn diff_value(prefix: &str, base: &Value, current: &Value, items: &mut Vec<ConfigItem>) {
    // 与 Tauri 版 configDiffToItems 对齐：只遍历 current 的键。
    // base 有而 current 没有的键（我们不管理的配置字段）完全忽略，
    // 避免对称 diff 产出 null 覆盖核心默认值。
    let c = match current {
        Value::Object(m) => m,
        _ => {
            // 叶子节点：值不同则产出
            if base != current && !current.is_null() {
                items.push(ConfigItem {
                    key: prefix.to_string(),
                    value: current.clone(),
                });
            }
            return;
        }
    };

    let b = match base {
        Value::Object(m) => Some(m),
        _ => None,
    };

    for key in c.keys() {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        let cv = &c[key];
        let bv = b.and_then(|m| m.get(key)).unwrap_or(&Value::Null);

        // 两者都是对象且 current 没有删除 base 的子键 → 递归
        // 注意：用本层子节点的 base（bv）比对，而非父级 base map（b），
        // 否则会误判为「删除了键」而把整个子对象当叶子产出（key 退化为父路径）。
        let both_objects = cv.is_object() && bv.is_object();
        let has_removed_keys = if let (Value::Object(co), Value::Object(bo)) = (cv, bv) {
            bo.keys().any(|bk| !co.contains_key(bk))
        } else {
            false
        };

        if both_objects && !has_removed_keys {
            diff_value(&path, bv, cv, items);
        } else if bv != cv && !cv.is_null() {
            items.push(ConfigItem {
                key: path,
                value: cv.clone(),
            });
        }
    }
}
