//! capability 契约消费：解析 core 导出的能力快照，提供默认值回退与一致性守护。

use std::collections::HashMap;

use serde_json::Value;

use crate::manifest::Manifest;

/// 单个能力条目（capability 快照 configKeys 的一项）。
pub struct CapEntry {
    pub ty: String,
    pub values: Vec<String>,
    pub default: Value,
}

/// core 能力契约：key → 条目。
#[derive(Default)]
pub struct Capabilities {
    pub entries: HashMap<String, CapEntry>,
}

impl Capabilities {
    const EMBEDDED: &'static str = include_str!("assets/capabilities.snapshot.json");

    /// 从内嵌快照解析；解析失败返回空集（守护测试会即时暴露）。
    pub fn from_embedded() -> Self {
        let v: Value = serde_json::from_str(Self::EMBEDDED).unwrap_or(Value::Null);
        Self::from_value(&v)
    }

    pub(crate) fn from_value(v: &Value) -> Self {
        let mut entries = HashMap::new();
        if let Some(arr) = v.get("configKeys").and_then(|x| x.as_array()) {
            for it in arr {
                let Some(key) = it.get("key").and_then(|x| x.as_str()) else {
                    continue;
                };
                let ty = it.get("type").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let values = it
                    .get("values")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let default = it.get("default").cloned().unwrap_or(Value::Null);
                entries.insert(key.to_string(), CapEntry { ty, values, default });
            }
        }
        Self { entries }
    }

    /// capability 默认值（core 权威）；未知 key 返回 Null。
    pub fn default_for(&self, key: &str) -> Value {
        self.entries.get(key).map(|e| e.default.clone()).unwrap_or(Value::Null)
    }

    /// 守护：清单每个 key ∈ 快照，且控件与类型兼容。返回不一致项描述列表。
    pub fn validate(&self, manifest: &Manifest) -> Vec<String> {
        let mut problems = Vec::new();
        for key in &manifest.order {
            let Some(item) = manifest.get(key) else {
                continue;
            };
            match self.entries.get(key) {
                None => problems.push(format!("[缺失] 清单 key `{key}` 不在 capability 快照中")),
                Some(entry) => {
                    if !control_type_compatible(&item.typ, &entry.ty) {
                        problems.push(format!(
                            "[不兼容] `{key}`：控件 `{}` 与类型 `{}` 不匹配",
                            item.typ, entry.ty
                        ));
                    }
                }
            }
        }
        problems
    }
}

/// 控件类型 ↔ capability 类型兼容判定（见计划 Global Constraints 矩阵）。
pub fn control_type_compatible(control: &str, ty: &str) -> bool {
    match control {
        "toggle" => ty == "bool",
        "select" | "segmented" => ty == "str" || ty == "enum",
        "number" | "slider" => ty == "int" || ty == "float",
        "string" => ty == "str",
        "checkbox_group" => ty == "strlist",
        "dialog_button" => ty == "str" || ty == "strlist" || ty == "map",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_parses_all_keys() {
        let caps = Capabilities::from_embedded();
        assert!(caps.entries.len() >= 114, "快照应含 >=114 key，实际 {}", caps.entries.len());
        let e = caps.entries.get("ui.tooltip.delay").expect("应含 ui.tooltip.delay");
        assert_eq!(e.ty, "int");
    }

    #[test]
    fn default_for_returns_capability_default() {
        let caps = Capabilities::from_embedded();
        // schema.active 的 core 默认值非空（系统预置）
        assert!(!caps.default_for("schema.active").is_null());
        // 未知 key 回退 Null
        assert!(caps.default_for("nonexistent.key").is_null());
    }

    #[test]
    fn control_type_matrix() {
        // 正例：每种控件 → 兼容类型
        assert!(control_type_compatible("toggle", "bool"));
        assert!(control_type_compatible("select", "str"));
        assert!(control_type_compatible("select", "enum"));
        assert!(control_type_compatible("segmented", "str"));
        assert!(control_type_compatible("segmented", "enum"));
        assert!(control_type_compatible("slider", "int"));
        assert!(control_type_compatible("slider", "float"));
        assert!(control_type_compatible("number", "int"));
        assert!(control_type_compatible("number", "float"));
        assert!(control_type_compatible("string", "str"));
        assert!(control_type_compatible("checkbox_group", "strlist"));
        assert!(control_type_compatible("dialog_button", "str"));
        assert!(control_type_compatible("dialog_button", "strlist"));
        assert!(control_type_compatible("dialog_button", "map"));
        // 反例：不兼容组合
        assert!(!control_type_compatible("toggle", "str"));
        assert!(!control_type_compatible("checkbox_group", "bool"));
        assert!(!control_type_compatible("string", "int"));
        assert!(!control_type_compatible("dialog_button", "bool"));
        assert!(!control_type_compatible("slider", "str"));
        assert!(!control_type_compatible("select", "int"));
        // 未知控件恒不兼容
        assert!(!control_type_compatible("mystery", "str"));
    }

    /// 守护：内嵌清单每个 key ∈ 快照，且控件与类型兼容。失败时打印全部不一致项。
    #[test]
    fn manifest_consistent_with_capabilities() {
        let caps = Capabilities::from_embedded();
        let manifest = Manifest::from_embedded();
        let problems = caps.validate(&manifest);
        assert!(problems.is_empty(), "清单与 capability 不一致:\n{}", problems.join("\n"));
    }

    /// 非失败：报告 capability 中尚未被清单覆盖的 key 数（no-silent-caps）。
    #[test]
    fn report_uncovered_capability_keys() {
        let caps = Capabilities::from_embedded();
        let manifest = Manifest::from_embedded();
        let covered: std::collections::HashSet<&str> =
            manifest.order.iter().map(|s| s.as_str()).collect();
        let uncovered: Vec<&str> = caps
            .entries
            .keys()
            .map(|s| s.as_str())
            .filter(|k| !covered.contains(k))
            .collect();
        println!("未覆盖 capability key 数: {} / {}", uncovered.len(), caps.entries.len());
        // 未覆盖项（均为有意不入清单）：
        //   ①方案级深度项：schema.pinyin.fuzzy.*（模糊音）、schema.active/available/special_modes/
        //     mix_modes（方案管理）、compat.host_render_processes —— 待方案设置弹窗承载。
        //   ②input.phrase.*（短语）—— 开关暂无作用，待 core 决定后再处理。
        // 注：ui.status.*（行为/位置类）与 ui.theme.name 已纳入清单；状态气泡的纯视觉项
        //   （颜色/字号/透明度/圆角）由主题 theme.toml 处理，core 快照本就无对应 key。
        assert!(uncovered.len() <= 20, "未覆盖项过多({}): {:?}", uncovered.len(), uncovered);
    }
}
