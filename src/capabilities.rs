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
                let ty = it
                    .get("type")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let values = it
                    .get("values")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| s.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let default = it.get("default").cloned().unwrap_or(Value::Null);
                entries.insert(
                    key.to_string(),
                    CapEntry {
                        ty,
                        values,
                        default,
                    },
                );
            }
        }
        Self { entries }
    }

    /// capability 默认值（core 权威）；未知 key 返回 Null。
    pub fn default_for(&self, key: &str) -> Value {
        self.entries
            .get(key)
            .map(|e| e.default.clone())
            .unwrap_or(Value::Null)
    }

    /// 所有 map 类型配置键（如 input.punct.custom_mappings）。
    /// 供 diff_config 作「整体叶子」：map 内部键是用户数据（标点 token 可含 `.`），
    /// 下钻 diff 会产出服务端未登记的路径被 setItems 拒绝。
    pub fn map_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| e.ty == "map")
            .map(|(k, _)| k.clone())
            .collect();
        keys.sort(); // HashMap 迭代序不稳定，排序保证 diff 行为可复现
        keys
    }

    /// 守护：清单每个 key ∈ 快照，且控件与类型兼容。返回不一致项描述列表。
    ///
    /// **视图 key**（`<配置路径>#<视图名>`）：同一配置路径需要两个独立清单项时用
    /// （`Manifest.items` 按 key 索引，一个 key 只能有一项）。校验时取 `#` 前的真实路径，
    /// 故「路径必须存在、控件类型必须兼容」两道守护对视图 key 同样生效。
    pub fn validate(&self, manifest: &Manifest) -> Vec<String> {
        let mut problems = Vec::new();
        for key in &manifest.order {
            let Some(item) = manifest.get(key) else {
                continue;
            };
            let key = config_path_of(key);
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

/// 清单项 key → 它读写的配置路径。
///
/// 视图 key `<配置路径>#<视图名>` 取 `#` 前的部分；普通 key 原样返回。
/// 视图 key 用于「同一配置路径需要两个独立清单项」——如 `schema.mix_modes` 既编辑
/// 触发键、又编辑候选来源，而 `Manifest.items` 按 key 索引，同 key 两项后者会覆盖前者。
pub fn config_path_of(key: &str) -> &str {
    key.split('#').next().unwrap_or(key)
}

/// 控件类型 ↔ capability 类型兼容判定（见计划 Global Constraints 矩阵）。
pub fn control_type_compatible(control: &str, ty: &str) -> bool {
    match control {
        "toggle" => ty == "bool",
        "select" => ty == "str" || ty == "enum",
        // segmented 既可承载枚举，也可承载二态 bool（如「初始语言模式」分段）。
        "segmented" => ty == "str" || ty == "enum" || ty == "bool",
        "number" | "slider" => ty == "int" || ty == "float",
        // number_select：离散整数下拉（如每页候选数）。实现仅按整数解析/写回，故仅兼容 int。
        "number_select" => ty == "int",
        "string" => ty == "str",
        // hotkey:功能键绑定字符串（如 "ctrl+shift+e"），编辑对话框承载。
        "hotkey" => ty == "str",
        "checkbox_group" => ty == "strlist",
        "dialog_button" => ty == "str" || ty == "strlist" || ty == "map",
        "dialog_button_mix_trigger" => ty == "structlist",
        "dialog_button_mix_members" => ty == "structlist",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_parses_all_keys() {
        let caps = Capabilities::from_embedded();
        assert!(
            caps.entries.len() >= 114,
            "快照应含 >=114 key，实际 {}",
            caps.entries.len()
        );
        let e = caps
            .entries
            .get("ui.tooltip.delay")
            .expect("应含 ui.tooltip.delay");
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
        assert!(control_type_compatible("segmented", "bool"));
        assert!(control_type_compatible("slider", "int"));
        assert!(control_type_compatible("slider", "float"));
        assert!(control_type_compatible("number", "int"));
        assert!(control_type_compatible("number", "float"));
        assert!(control_type_compatible("number_select", "int"));
        assert!(!control_type_compatible("number_select", "float"));
        assert!(control_type_compatible("string", "str"));
        assert!(control_type_compatible("hotkey", "str"));
        assert!(control_type_compatible("checkbox_group", "strlist"));
        assert!(control_type_compatible("dialog_button", "str"));
        assert!(control_type_compatible("dialog_button", "strlist"));
        assert!(control_type_compatible("dialog_button", "map"));
        assert!(control_type_compatible(
            "dialog_button_mix_trigger",
            "structlist"
        ));
        // 反例：不兼容组合
        assert!(!control_type_compatible("toggle", "str"));
        assert!(!control_type_compatible("checkbox_group", "bool"));
        assert!(!control_type_compatible("string", "int"));
        assert!(!control_type_compatible("dialog_button", "bool"));
        assert!(!control_type_compatible("slider", "str"));
        assert!(!control_type_compatible("select", "int"));
        assert!(!control_type_compatible("dialog_button_mix_trigger", "str"));
        // 未知控件恒不兼容
        assert!(!control_type_compatible("mystery", "str"));
    }

    /// 守护：内嵌清单每个 key ∈ 快照，且控件与类型兼容。失败时打印全部不一致项。
    #[test]
    fn manifest_consistent_with_capabilities() {
        let caps = Capabilities::from_embedded();
        let manifest = Manifest::from_embedded();
        let problems = caps.validate(&manifest);
        assert!(
            problems.is_empty(),
            "清单与 capability 不一致:\n{}",
            problems.join("\n")
        );
    }

    /// 有意不进设置清单的 capability key（配套 [`uncovered_capability_keys_match_allowlist`]）。
    ///
    /// 此前这里是一个 `uncovered.len() <= 24` 的阈值。阈值的问题是**它不认识键**：
    /// core 新增键让计数超标时，把数字改大就能过，于是旁边那段分类注释一路过期
    /// （写着「当前 13 项」而实际 26 项），谁也说不清多出来的到底是哪些、该不该暴露。
    /// 改为显式名单后，任何一个新的未覆盖键都必须在此处具名登记，顺带写清为什么不暴露。
    const UNCOVERED_BY_DESIGN: &[&str] = &[
        // ① 方案管理：由方案页的输入方案列表承载，不作为普通设置项。
        //    active   → 工具栏「设为当前」+ 行内「当前」徽章（schema.setActive 即时
        //               生效，配置值由 state.rs 注册的 writer 随保存写回）；
        //    available→ 行内启用勾选 + ↑↓ 排序，同样经 writer 写回。
        "schema.active",
        "schema.available",
        "schema.special_modes",
        // ② 兼容渲染进程名单，暂不开放。
        "compat.host_render_processes",
        // ③ 短语前缀列举：开关暂无作用，待 core 决定后再处理。
        "input.phrase.min_prefix",
        // ④ 非独立清单项：由各功能键的「全局」勾选经收集器汇总写回。
        "keys.global_hotkeys",
        // ⑤ 临时英文不再设独立启用开关（Shift+字母 始终可进入、触发键可留空）。
        "input.temp_english.enabled",
        // ⑥ 外观不支持 / 平台限制项，均由 core 维持默认值。
        "ui.font.path",
        "ui.font.render_mode",
        "ui.tooltip.pinyin_max_readings",
        // ⑦ 命令栏始终开启，不提供关闭入口。
        "input.cmdbar.enabled",
        // ⑧ 拼音词频衰减调参：高级/内部参数（recency_peak 尚未接入引擎），不对普通用户暴露。
        "schema.pinyin.frequency.half_life",
        "schema.pinyin.frequency.base_scale",
        "schema.pinyin.frequency.recency_peak",
        // ⑨ 拼音侧临时词晋升次数暂不暴露（码表侧的 auto_phrase.promote_count 已在清单内）。
        "schema.pinyin.auto_learn.promote_count",
        // ⑨' 码表造词的兜底与容量参数，普通用户无判断依据，需要时改配置文件。
        //    注：min_phrase_len / max_phrase_len 已在清单内（hidden=true）。
        "schema.codetable.auto_phrase.idle_timeout_ms",
        "schema.codetable.auto_phrase.temp_max_entries",
        // ⑩ core 新增键，本项目尚未接入设置界面（待评估）。
        "keys.take_screenshot",
        "debug.log_max_files",
        "debug.log_max_size_mb",
        "input.temp_pinyin.enabled",
        "input.temp_pinyin.hotkey",
        // 候选窗首显时序调参（core 侧候选窗改造新增）：判断依据是宿主的绘制时序，
        // 普通用户无从判断，需要时改配置文件。接不接进设置页待 core 侧定稿后评估。
        // fast_first_show_fallback_ms 在 core 侧的字段注释里明确标注为「内部选项」，
        // 取值依据是特定宿主（Word/记事本）的坐标下发时序，不具备可暴露性。
        "ui.candidate.first_show_settle_ratio",
        "ui.candidate.fast_typing_window_ms",
        "ui.candidate.fast_first_show_fallback_ms",
        // ⑪ 内部/实验项：顶码上屏的宿主时序策略，仅在宿主表现异常时改配置文件。
        "input.top_commit_mode",
        // ⑫ 拖动落盘的绝对坐标：由用户拖动窗口写入，手填既不直观又会与拖动互相覆盖，
        //    故刻意不在设置页暴露（与 core 侧同一决策）。
        "ui.candidate.custom_x",
        "ui.candidate.custom_y",
        "ui.status.custom_x",
        "ui.status.custom_y",
        // ⑬ 配对状态时效秒数（core 侧跨模式跳出改造新增，内部项）：兜底「用户中途用鼠标
        //    点走、把括号退格删掉」这类输入法感知不到的操作。取值依据是人的操作节奏，
        //    普通用户无判断依据，需要时改配置文件。
        "input.auto_pair.state_ttl_secs",
    ];

    /// 守护（no-silent-caps）：未被设置清单覆盖的 capability key 必须与豁免名单**逐项相等**。
    ///
    /// 双向断言：core 新增了没接进设置页的键 → 失败（逼你决定暴露还是登记）；
    /// 名单里的键已被接进清单或已被 core 删除 → 也失败（防止名单腐烂成永真条目）。
    #[test]
    fn uncovered_capability_keys_match_allowlist() {
        let caps = Capabilities::from_embedded();
        let manifest = Manifest::from_embedded();
        let covered: std::collections::HashSet<&str> =
            manifest.order.iter().map(|s| s.as_str()).collect();
        let uncovered: std::collections::BTreeSet<&str> = caps
            .entries
            .keys()
            .map(|s| s.as_str())
            .filter(|k| !covered.contains(k))
            .collect();
        let allow: std::collections::BTreeSet<&str> = UNCOVERED_BY_DESIGN.iter().copied().collect();

        let unexpected: Vec<&&str> = uncovered.difference(&allow).collect();
        let stale: Vec<&&str> = allow.difference(&uncovered).collect();
        assert!(
            unexpected.is_empty() && stale.is_empty(),
            "未覆盖 capability key 与豁免名单不符：\n  \
             未登记的未覆盖键({})：{:?}\n    \
             —— 要么接进 settings_manifest.toml，要么加进 UNCOVERED_BY_DESIGN 并写明理由\n  \
             名单中已失效的条目({})：{:?}\n    \
             —— 该键已被清单覆盖或已被 core 删除，请从 UNCOVERED_BY_DESIGN 移除",
            unexpected.len(),
            unexpected,
            stale.len(),
            stale
        );
    }

    /// 守护：检入的 `capabilities.snapshot.json` 必须等于 core 现场生成的 capability
    /// descriptor。快照是 core 能力的**检入镜像**，此前靠人工同步，已漂移出 4 处默认值
    /// 偏离 + 缺 2 键 + 多 1 键（`input.phrase.max_display_chars`，core 早已删除）。
    ///
    /// 只比 `configKeys`：`appVersion` 跟着 docs/VERSION 走，每次发版都会变，
    /// 与本测试要守的「配置契约」无关，且 wind-setting 并不读取它。
    ///
    /// 失败时请**重新生成**快照而不是手改 JSON——手改正是漂移的来源。
    #[test]
    fn snapshot_matches_core_generated_capabilities() {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../WindInput/data");
        let fresh =
            wind_rpc::capabilities::generate(Some(&data)).expect("生成 core capability 失败");
        let embedded: Value =
            serde_json::from_str(Capabilities::EMBEDDED).expect("快照 JSON 解析失败");

        let index = |v: &Value| -> std::collections::BTreeMap<String, Value> {
            v.get("configKeys")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|i| {
                            i.get("key")
                                .and_then(|k| k.as_str())
                                .map(|k| (k.to_string(), i.clone()))
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let (core, snap) = (index(&fresh), index(&embedded));
        assert!(!core.is_empty(), "core 生成结果为空，对账无意义");

        let mut problems = Vec::new();
        for (key, cv) in &core {
            match snap.get(key) {
                None => problems.push(format!("快照缺键: {key}")),
                Some(sv) if sv != cv => {
                    problems.push(format!("{key}: 快照={sv} core={cv}"));
                }
                _ => {}
            }
        }
        for key in snap.keys().filter(|k| !core.contains_key(*k)) {
            problems.push(format!("快照多余键（core 已无此键）: {key}"));
        }
        assert!(
            problems.is_empty(),
            "capabilities.snapshot.json 与 core 漂移 {} 处，请重新生成：\n\
             cargo test regenerate_capabilities_snapshot -- --ignored\n{}",
            problems.len(),
            problems.join("\n")
        );
    }

    /// 重新生成检入快照（**非自动运行**，需显式 `-- --ignored`）。
    ///
    /// 上面的守护测试要求「重新生成而不是手改 JSON」，此前却没有生成入口，只能手抄
    /// core 的输出——正是它要防的漂移来源。core 改了配置注册表或默认值后跑：
    /// `cargo test regenerate_capabilities_snapshot -- --ignored`
    #[test]
    #[ignore = "写文件的维护工具，按需显式运行"]
    fn regenerate_capabilities_snapshot() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let fresh = wind_rpc::capabilities::generate(Some(&root.join("../WindInput/data")))
            .expect("生成 core capability 失败");
        let path = root.join("src/assets/capabilities.snapshot.json");
        let mut text = serde_json::to_string_pretty(&fresh).expect("序列化失败");
        text.push('\n'); // 与检入格式一致（EOF 换行）
        std::fs::write(&path, text).expect("写入快照失败");
        println!("已重新生成 {}", path.display());
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
        println!(
            "未覆盖 capability key 数: {} / {}",
            uncovered.len(),
            caps.entries.len()
        );
        // 逐项理由见 UNCOVERED_BY_DESIGN；把关的断言在
        // uncovered_capability_keys_match_allowlist，此处只作观察输出，不设阈值。
        println!("未覆盖明细: {uncovered:?}");
    }
}
