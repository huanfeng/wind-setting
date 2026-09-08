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

    /// 按点分路径获取对象（Map 型配置，如 `keys.schema_hotkeys`）。
    pub fn get_object(&self, path: &str) -> Option<&serde_json::Map<String, Value>> {
        self.get_path(path)?.as_object()
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
///
/// `atomic_keys`：须**整体发送**的对象型配置键（core registry 的 map 类型，如
/// `input.punct.custom_mappings`）。map 的内部键是用户数据（标点 token 可含
/// `.`/`/` 等字符），若按普通对象下钻会产出 `custom_mappings./` 甚至
/// `custom_mappings..` 这类服务端未登记的路径，被 config.setItems 校验拒绝。
/// 列表由调用方从 capabilities（type=="map"）提取。
pub fn diff_config(base: &Config, current: &Config, atomic_keys: &[String]) -> Vec<ConfigItem> {
    let mut items = Vec::new();
    diff_value("", &base.0, &current.0, atomic_keys, &mut items);
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
        let items = diff_config(&base, &current, &[]);
        assert!(items.is_empty());
    }

    /// ★ 顺序无语义的键：只换顺序不算改动。
    ///
    /// 现场：清单调整了标点对的展示顺序，而写回按清单序整表覆盖 ⇒ 老用户配置里的既有
    /// 顺序与之不符 ⇒ 打开设置页什么都不动就判脏、每次关窗被拦。
    #[test]
    fn reordering_an_unordered_list_is_not_a_change() {
        let base = Config::from_value(json!({
            "input": { "auto_pair": { "chinese_pairs": ["（）", "【】", "｛｝", "《》"] } }
        }));
        let current = Config::from_value(json!({
            "input": { "auto_pair": { "chinese_pairs": ["（）", "【】", "《》", "｛｝"] } }
        }));
        assert!(
            diff_config(&base, &current, &[]).is_empty(),
            "同一批标点对换个顺序不该判脏"
        );
    }

    /// ★ 但**内容**变了照旧判脏——豁免的只是顺序，不是这个键本身。
    #[test]
    fn changing_an_unordered_lists_content_is_still_a_change() {
        let base = Config::from_value(json!({
            "input": { "auto_pair": { "chinese_pairs": ["（）", "【】"] } }
        }));
        for current in [
            json!({ "input": { "auto_pair": { "chinese_pairs": ["（）", "「」"] } } }), // 换成员
            json!({ "input": { "auto_pair": { "chinese_pairs": ["（）"] } } }),         // 少一个
            json!({ "input": { "auto_pair": { "chinese_pairs": ["（）", "【】", "「」"] } } }), // 多一个
        ] {
            let items = diff_config(&base, &Config::from_value(current.clone()), &[]);
            assert_eq!(items.len(), 1, "内容变了必须判脏：{current}");
        }
    }

    /// ★★ 不得误伤**有序**的键：顺序即语义的那些，换序就是改动。
    ///
    /// 误判方向不对称——把有序的当成无序，用户调了顺序却不判脏，改动会静默丢失。
    /// 工具栏项的顺序即渲染顺序、选词键组的顺序决定候选位次，都必须照旧严格比较。
    #[test]
    fn reordering_an_ordered_list_is_still_a_change() {
        for key in ["toolbar", "status"] {
            let base = Config::from_value(json!({ "ui": { key: { "items": ["a", "b"] } } }));
            let current = Config::from_value(json!({ "ui": { key: { "items": ["b", "a"] } } }));
            assert_eq!(
                diff_config(&base, &current, &[]).len(),
                1,
                "ui.{key}.items 未列入豁免表，换序必须判脏"
            );
        }
        let base = Config::from_value(json!({ "keys": { "select_key_groups": ["a", "b"] } }));
        let current = Config::from_value(json!({ "keys": { "select_key_groups": ["b", "a"] } }));
        assert_eq!(
            diff_config(&base, &current, &[]).len(),
            1,
            "选词键组的顺序决定候选位次，换序必须判脏"
        );
    }

    #[test]
    fn test_diff_detects_change() {
        let base = Config::from_value(json!({ "input": { "enter_behavior": "commit" } }));
        let current = Config::from_value(json!({ "input": { "enter_behavior": "clear" } }));
        let items = diff_config(&base, &current, &[]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key, "input.enter_behavior");
        assert_eq!(items[0].value, Value::String("clear".to_string()));
    }

    #[test]
    fn test_diff_atomic_map_key_emitted_whole() {
        // 回归：custom_mappings（map 型）曾被按普通对象下钻，产出
        // "input.punct.custom_mappings./" 等未登记路径被服务端拒绝（映射从未落盘）。
        // 标点 token 含 `.` 时还会拼出 "custom_mappings.." 烂路径。
        let base = Config::from_value(json!({
            "input": { "punct": { "custom_mappings": {} } }
        }));
        let current = Config::from_value(json!({
            "input": { "punct": { "custom_mappings": {
                "/": ["、", "／", "、", "/"],
                ".": ["。", "．", "。", "."]
            } } }
        }));
        let atomic = vec!["input.punct.custom_mappings".to_string()];
        let items = diff_config(&base, &current, &atomic);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key, "input.punct.custom_mappings");
        assert!(items[0].value.is_object());
        assert_eq!(items[0].value.as_object().unwrap().len(), 2);

        // 无 atomic 声明时的旧行为（下钻出带 token 的路径）——确保新参数确实改变了它
        let bad = diff_config(&base, &current, &[]);
        assert!(
            bad.iter()
                .any(|i| i.key.starts_with("input.punct.custom_mappings.")),
            "无 atomic 时应下钻（旧行为），若此断言失败说明测试前提已变"
        );
    }

    #[test]
    fn test_diff_atomic_map_key_no_change_not_emitted() {
        let v = json!({ "input": { "punct": { "custom_mappings": { "/": ["、", "", "", ""] } } } });
        let base = Config::from_value(v.clone());
        let current = Config::from_value(v);
        let atomic = vec!["input.punct.custom_mappings".to_string()];
        assert!(diff_config(&base, &current, &atomic).is_empty());
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
        let items = diff_config(&base, &current, &[]);
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
        let items = diff_config(&base, &current, &[]);
        assert_eq!(items.len(), 2);
        let keys: Vec<&str> = items.iter().map(|i| i.key.as_str()).collect();
        assert!(keys.contains(&"input.enter_behavior"));
        assert!(keys.contains(&"ui.theme.style"));
    }
}

/// 顺序**无语义**的字符串数组键：diff 时按多重集比较，顺序变了不算改动。
///
/// ## ★ 默认是「有序」，本表是显式豁免——因为误判方向不对称
///
/// | 误判 | 后果 |
/// | --- | --- |
/// | 有序的当成无序 | 用户调了顺序却不判脏 ⇒ **改动静默丢失** |
/// | 无序的当成有序 | 保留一次假脏 ⇒ 只是烦 |
///
/// 所以只列确有把握的，宁可漏也不可多。`keys.select_key_groups`、
/// `ui.candidate.index_labels`、`ui.font.fallback`、`ui.toolbar.items`、`schema.available`
/// 这些的顺序都是有语义的（选词位次 / 回退次序 / 渲染序 / 切换循环序），**不得**加进来。
/// core 的 `config_schema.rs` 里已有同款区分的先例（`ui.toolbar.items` 的顺序即渲染顺序，
/// 与 `ui.status.items` 相对）。
///
/// ## 为什么需要它
///
/// 这两个键的控件是「勾一批中文/英文标点对」，写回走 `collect_checked` 的**整表覆盖**，
/// 输出顺序 = 清单选项序。清单里调整过展示顺序之后，它与用户配置里的既有顺序不再一致
/// ⇒ 打开设置页什么都不动就判脏 ⇒ 每次关窗都被拦一次。集合本身没变，顺序对这两个键
/// 也没有任何语义。
const ORDER_INSENSITIVE_LISTS: &[&str] = &[
    "input.auto_pair.chinese_pairs",
    "input.auto_pair.english_pairs",
];

/// 叶子值是否等价（顺序无语义的数组按多重集比）。
///
/// 用**排序后逐项比**而不是 HashSet：重复项的增减也是真实改动，集合比会漏掉。
fn leaf_equivalent(key: &str, base: &Value, current: &Value) -> bool {
    if base == current {
        return true;
    }
    if !ORDER_INSENSITIVE_LISTS.contains(&key) {
        return false;
    }
    match (base.as_array(), current.as_array()) {
        (Some(b), Some(c)) if b.len() == c.len() => {
            let key_of = |v: &Value| v.as_str().map(str::to_string);
            let (mut bs, mut cs): (Vec<_>, Vec<_>) = (
                b.iter().map(key_of).collect(),
                c.iter().map(key_of).collect(),
            );
            // 含非字符串元素时不做等价判定，退回严格比较（None 参与排序但下面会不等）。
            if bs.iter().any(Option::is_none) || cs.iter().any(Option::is_none) {
                return false;
            }
            bs.sort();
            cs.sort();
            bs == cs
        }
        _ => false,
    }
}

fn diff_value(
    prefix: &str,
    base: &Value,
    current: &Value,
    atomic_keys: &[String],
    items: &mut Vec<ConfigItem>,
) {
    // 与 Tauri 版 configDiffToItems 对齐：只遍历 current 的键。
    // base 有而 current 没有的键（我们不管理的配置字段）完全忽略，
    // 避免对称 diff 产出 null 覆盖核心默认值。
    let c = match current {
        Value::Object(m) => m,
        _ => {
            // 叶子节点：值不同则产出（顺序无语义的数组按多重集比，见 `leaf_equivalent`）
            if !leaf_equivalent(prefix, base, current) && !current.is_null() {
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

        // map 型配置键：整体叶子，变更即整对象产出，绝不下钻（见 diff_config 文档）。
        if atomic_keys.iter().any(|k| k == &path) {
            if bv != cv && !cv.is_null() {
                items.push(ConfigItem {
                    key: path,
                    value: cv.clone(),
                });
            }
            continue;
        }

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
            diff_value(&path, bv, cv, atomic_keys, items);
        } else if !leaf_equivalent(&path, bv, cv) && !cv.is_null() {
            // ⚠️ 数组走的是**这一条**，不是函数开头那个 `current` 非对象的提前 return
            // ——那个分支只在根节点本身就是叶子时才到得了。顺序无语义的判定必须挂在这里，
            // 挂错位置的表现是「豁免完全不生效」且没有任何报错。
            items.push(ConfigItem {
                key: path,
                value: cv.clone(),
            });
        }
    }
}
