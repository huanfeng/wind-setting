//! capability 契约消费：解析 core 导出的能力快照，提供默认值回退与一致性守护。

use std::collections::HashMap;

use serde_json::Value;

use crate::manifest::Manifest;

/// 单个能力条目（capability 快照 configKeys 的一项）。
pub struct CapEntry {
    pub ty: String,
    pub values: Vec<String>,
    pub default: Value,
    /// 该项可被方案级配置覆盖时的提示；不可覆盖的键为 `None`。
    ///
    /// 它回答的是「我在全局改了，为什么某个方案里没变」。★ 这是**静态**事实（由配置模型
    /// 决定，与本机有什么文件无关），所以随快照来，不花任何运行时代价——⛔ 别为了它去
    /// 加 RPC 或读盘。core 侧权威是 `config_schema::SCHEMA_OVERRIDES`。
    pub schema_override: Option<SchemaOverrideHint>,
}

/// 「此项可被方案覆盖」的提示内容。
pub struct SchemaOverrideHint {
    /// 方案文件里的落点段名，如 `[engine.codetable]`。
    pub section: String,
    /// 给用户看的一句话，core 侧写好，设置端**原样显示不再改写**——
    /// 两边各写一份措辞，改了一处就会分叉。
    pub note: String,
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
                // 字段缺席 = 不可被方案覆盖（与 `values` 一样是可选字段，不是给 null）。
                let schema_override = it.get("schemaOverride").and_then(|o| {
                    Some(SchemaOverrideHint {
                        section: o.get("section")?.as_str()?.to_string(),
                        note: o.get("note")?.as_str()?.to_string(),
                    })
                });
                entries.insert(
                    key.to_string(),
                    CapEntry {
                        ty,
                        values,
                        default,
                        schema_override,
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
            if is_local_key(key) {
                continue;
            }
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

/// 落点在 **core `config.toml` 之外**的清单项前缀。
///
/// 这类项由 `manifest::local_field` 直接读写各自的文件，经 `SideCommitter` 提交：
/// TSF 的日志开关在 `tsf_log_config`（DLL 在 `DllMain` 里就要读到，那时 IPC 还没连上），
/// 设置程序自己的在 `wind_setting.toml`（`logger::init` 早于 RPC）。另有纯说明行
/// （`type = "note"`）与动作按钮（`open_log_dir`），它们压根没有"值"。
///
/// ⚠️ 它们**不在** core 的 capability 空间里，故所有拿快照校验清单的守门测试都必须
/// 跳过本前缀——否则每加一项就是一片"缺失"误报。反向的守门（caps 有、清单没有）
/// 不受影响：那一侧遍历的是 caps，本前缀根本不会出现。
pub const LOCAL_KEY_PREFIX: &str = "local.";

/// 见 [`LOCAL_KEY_PREFIX`]。
pub fn is_local_key(key: &str) -> bool {
    key.starts_with(LOCAL_KEY_PREFIX)
}

/// 控件类型 ↔ capability 类型兼容判定（见计划 Global Constraints 矩阵）。
pub fn control_type_compatible(control: &str, ty: &str) -> bool {
    match control {
        "toggle" => ty == "bool",
        // toggle_slash_dun：长得像开关，落点却是自定义标点表里的一行（map），故单列一档
        // 而不是放宽 `toggle` —— 通用开关一旦兼容 map，任何 map 键都能被接成一个二值开关，
        // 而那件事只在「这张表的这一行」这个语境下才成立。
        "toggle_slash_dun" => ty == "map",
        // log_level_select 与 select 读写完全同构（只多登记一个"当前档位"探针，
        // 供隐私示警条读），故兼容性判据也一样。
        "select" | "log_level_select" => ty == "str" || ty == "enum",
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
        // key_action 型控件的 `key` 只是标识符（真实落点是 keys.key_actions），
        // 故按被标识字段的原类型校验：strlist（trigger_keys）或 structlist（mix_modes）。
        "dialog_button_key_action" => ty == "strlist" || ty == "structlist",
        "dialog_button_mix_members" => ty == "structlist",
        // 注释词库：整表读写的条目列表（id/label/path/enabled/schemas 五个字段）。
        "dialog_button_comment_dicts" => ty == "structlist",
        // 语言栏角标规则：整表读写的条目列表（enabled/state/corner/两个色值/scale）。
        "dialog_button_langbar_badges" => ty == "structlist",
        // 悬停提示段列表：整表读写的条目列表（enabled/label/template/each/promote/inline）。
        "dialog_button_tooltip_sections" => ty == "structlist",
        // 任务栏主字颜色：宿主是四个色值字符串键之一（其余三键同一对话框读写）。
        "dialog_button_langbar_text_colors" => ty == "str",
        // ordered_pairs：与 checkbox_group 读写同一形态（strlist），差别只在**顺序是否被
        // core 消费**——那是清单声明的语义，capability 里没有这个维度，故类型校验一致。
        "dialog_button_ordered_pairs" => ty == "strlist",
        // open_pairs：与 ordered_pairs / checkbox_group 读写同一形态（strlist），差别在
        // **清单列不列得全值域**——列不全故写回只增删自己列出的项。capability 里同样
        // 没有这个维度（它只知道类型），故也只能由清单的 type 声明。
        "dialog_button_open_pairs" => ty == "strlist",
        // select_by_layout：横竖两个下拉读写**同一个** enum 键（core 的 ByLayout，值形如
        // `"h:hide v:always"`）。故按 enum 校验——值域就是那个 enum 的值域，每一档各取
        // 其一；core 侧另有 `byLayout` 标记位表明这个键允许分档写法。
        "select_by_layout" => ty == "enum",
        // select_mix_layout：下拉，但读写的是 structlist 里按 id 定位的一个字符串字段
        // （quick_mix 的 candidate_layout），故按 structlist 而非 enum 校验。
        "select_mix_layout" => ty == "structlist",
        // 同理，读写 structlist 里按 id 定位的一个字段：free_input（字符串三态）与
        // free_input_takes_select_keys（布尔）。
        "select_mix_free_input" => ty == "structlist",
        "checkbox_mix_free_select_keys" => ty == "structlist",
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
            "dialog_button_key_action",
            "structlist"
        ));
        // 反例：不兼容组合
        assert!(!control_type_compatible("toggle", "str"));
        assert!(!control_type_compatible("checkbox_group", "bool"));
        assert!(!control_type_compatible("string", "int"));
        assert!(!control_type_compatible("dialog_button", "bool"));
        assert!(!control_type_compatible("slider", "str"));
        assert!(!control_type_compatible("select", "int"));
        assert!(!control_type_compatible("dialog_button_key_action", "str"));
        assert!(control_type_compatible(
            "dialog_button_ordered_pairs",
            "strlist"
        ));
        assert!(!control_type_compatible(
            "dialog_button_ordered_pairs",
            "str"
        ));
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
        // ③ 短语前缀列举：开关暂无作用，待 core 决定后再处理。
        "input.phrase.min_prefix",
        // ④ 非独立清单项：由各功能键的「全局」勾选经收集器汇总写回。
        "keys.global_hotkeys",
        // ⑤ 临时英文不再设独立启用开关（Shift+字母 始终可进入、触发键可留空）。
        "input.temp_english.enabled",
        // ⑥ 外观不支持 / 平台限制项，均由 core 维持默认值。
        "ui.font.path",
        "ui.font.render_mode",
        // ⑦ 命令栏始终开启，不提供关闭入口。
        "input.cmdbar.enabled",
        // ⑧ 词频打分模型的两个残留系数：**改动无任何效果**，故不暴露。
        //    core 改用位置提升模型后，配置项 → FreqProfile 字段 → pinyin_score 整条链已无
        //    生产调用者（位置模型只取 decay_factor，不打分）。core 侧已在 data/config.toml
        //    与函数文档上标注，保留仅为将来若恢复打分模型可复用。
        //
        //    ⚠️ **不能因为「词频参数都该放一起」就把它俩也接进设置页**：调频的其余参数
        //    （enabled / strategy / promote_prefix / half_life ×2 / protect_top_n* /
        //    english_code_scope）都已在清单内，唯独这两个是死链。放上去等于给用户两个拧了
        //    没反应的旋钮，比不放更糟。要放的前提是 core 先恢复打分模型。
        //    见 core 的 docs/design/freq-rerank-model.md §9.1.2。
        "schema.pinyin.frequency.base_scale",
        "schema.pinyin.frequency.recency_peak",
        // ⑨ （已撤出豁免）schema.pinyin.auto_learn.promote_count 现已在清单内：出厂 0 = 不
        //    晋升，不暴露就等于拼音自动学到的词永远出不了临时词库，而设置页原来的 hint
        //    还写着「加入用户词库」。码表侧的 auto_phrase.promote_count 一直是暴露的。
        // ⑨' 码表造词的兜底与容量参数，普通用户无判断依据，需要时改配置文件。
        //    注：min_phrase_len / max_phrase_len 已在清单内（hidden=true）。
        "schema.codetable.auto_phrase.idle_timeout_ms",
        "schema.codetable.auto_phrase.temp_max_entries",
        // ⑨''' 滑窗草稿层的三个内部参数（core 的
        //     docs/design/auto-phrase-draft-layer.md）。草稿是自动造词滑窗切出的猜测词，
        //     用户用过一次才转正进临时词库——这三项是它的有效期、容量与落库批量。
        //     同 ⑨' 的判据：普通用户无判断依据，且 draft_max_entries / draft_flush_batch
        //     的默认值目前还是**纸面估算**、待真机数据重定，此时暴露出去只会把一个未定的
        //     数字固化进用户配置文件。**待实测调参后再评估**要不要暴露。
        "schema.codetable.auto_phrase.draft_ttl_hours",
        "schema.codetable.auto_phrase.draft_max_entries",
        "schema.codetable.auto_phrase.draft_flush_batch",
        // ⑨'' 码元字符集：core 的 REGISTRY 已注册（config_schema.rs），但消费侧仍在开发中，
        //     暂不接入设置界面。**待 core 定稿后再评估**该不该暴露、放全局清单还是方案对话框。
        "schema.codetable.input_chars",
        "schema.codetable.leading_chars",
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
        // ⑪'' 内部项：顶屏（标点 / 非码元字符 / 进临时模式）上屏时是否造词、推临时词晋升计数，
        //      出厂开。是否让顶屏参与学习属产品口径，普通用户无需调，需要时改配置文件。
        "input.top_commit_learn",
        // ⑪' 打字速度的修正系数（core 侧速度模型 v2 新增）：采集期已修掉三条结构性偏差
        //     （毫秒分母 / 段首口径对称 / 短码长词封顶），这个系数只补最后一条——输入法
        //     无从知道用户打错了没有，退格重打的字符计两遍而耗时只算一遍。
        //
        //     不暴露的判据仍是「调它需要什么依据」：用户手上没有任何可对照的真值
        //     （要对照就得同时开着测速软件打同一段文字），放个滑块出去只会变成
        //     「嫌速度低就调大」，而调大恰恰让显示值重新偏离真实手速。core 侧 data/config.toml
        //     有完整注释，需要的人改配置文件。
        "stats.speed_factor",
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
        // ⑭ 加词面板的候选布局（core 侧模式级候选布局改造新增）：加词面板没有独立设置
        //    分区，且它逐字确认的两行提示本就该竖排——出厂 vertical 即最优解，横排没有
        //    使用场景。同批的临拼/临英/网址/快捷四项都已进清单，只有它刻意不暴露。
        "input.add_word.candidate_layout",
        // ㉓ 候选窗定位方式（core 新增 `[input.caret]`，2026-09-15）：**排障开关，不是偏好**。
        //    这几个模式由热键激活、激活瞬间无输入内容，却要立刻显示候选窗，于是靠一个占位
        //    composition 把 range 撑开好让 GetTextExt 出矩形；而那个占位建在当前 selection 上，
        //    用户选中着文字时会把选中内容替换掉（t123）。关掉它则坐标退到 GUITHREADINFO/
        //    GetCaretPos 回退链 —— 但很多宿主的那几个 API 给不出正确坐标，关了会换来候选窗错位。
        //    也就是说这四项没有「更好」的一侧，只有「在你这台机器的这个宿主上哪边代价小」，
        //    普通用户无从判断。出厂值 core 已按场景各自定过：**加词 false**（t123 报的就是它
        //    吃掉选中内容，而加词弹的是预览窗、对坐标精度的敏感度低于候选列表），其余三项 true。
        "input.caret.add_word_via_composition",
        "input.caret.temp_pinyin_via_composition",
        "input.caret.special_via_composition",
        "input.caret.rare_char_via_composition",
        // ㉔ 上屏换行形式的全局默认档（core `input.commit_newline`；本仓快照此前一直漏登记，
        //    2026-09-16 重新生成快照时才补进来）：**不是偏好，是宿主适配**。core 的
        //    `NewlineStyle` 写明了为什么没有全局正确值——富文本宿主（Word / WPS / RichEdit）
        //    的段落边界**就是** CR，LF 在那个模型里根本不构成换行；而 VS Code / 终端 /
        //    textarea 是字节即存储，做任何转换都是在改写用户的数据（t112 那个「正则直通后
        //    行尾从 LF 变成 CR」正是这么来的），且这种失败是静默的。
        //    真正该配的是 **per-app 覆盖**（`compat.toml` 的 `[[commit_newline]]` 段，属另一
        //    份 schema，不归本清单管）；这里这个只是全局兜底档，出厂 `keep` ＝「不知道宿主
        //    要什么的时候不动用户数据」。把它摆到界面上，等于请普通用户在看不见后果的地方
        //    赌一把——赌输的那一侧还是静默的。
        "input.commit_newline",
        // ⑮ 候选注释词库挂载列表 **已接入清单**（`dialog_button_comment_dicts`，
        //    2026-08-26），故从本名单移除。它当年登记的理由写的就是「待接入，不是设计上
        //    不暴露」——那条自陈是这次动手的直接依据。⚠️ 本名单是**双向**断言：接进清单
        //    后若还留在这里，`uncovered_capability_keys_match_allowlist` 会以 stale 报红。
        // ⑯ `keys.schema_hotkeys` 已从这份白名单**移除**：core 侧该键已废弃并移出登记表
        //    （并入 `keys.key_actions` 的 `switch_schema:<方案id>`）。方案页「进入方式」
        //    那一节现在读写的是 key_actions，而它就在下面一条里，理由完全相同。
        // ⑰ 任务栏图标角标的九项（core 侧语言栏图标改造新增）**已全部接入清单**，故本名单
        //    里一条不剩。当年不暴露的理由是"这些量只能盯着 16×16 反复比对、放进去等于给
        //    用户一排盲调旋钮"，2026-09-01 改成规则表时逐条推翻：配色有色块可看，大小与
        //    透明度是用户明确要的自由度。
        //
        //    其中 badge_alpha 曾单独留守，理由是它同时是**档位开关**（= 1.0 走实心挖空、
        //    < 1.0 走半透明保留主字，两条互斥的画法），滑块表达不了那个跳变。推翻它的判据
        //    是**两害相权**：一个用户看不见也改不了的出厂 0.88，比一个末端有跳变、但跳变
        //    已写进 hint 的滑块更糟。跳变本身没有消失——改 hint 时别把那句话删了。
        // ⑱ 联想的四个「来源开关」（core 侧联想改造新增）：主干六项已在清单内，这四项决定
        //    联想候选**从哪来**，按配置文件里的书写顺序取配额。
        //
        //    bigram 是**尚无实现**的来源，开着也不会出候选——与 ⑧ 的两个死链系数同一处理：
        //    给用户拧了没反应的旋钮，比不放更糟。等数据来源落地后再评估。
        //    history（联想历史，2026-09 已实现）出厂开、只学用户自己从联想里选过的词，
        //    没有「需要关它」的常见诉求；清数据走「清空词频」，故仍不放开关。
        //
        //    prefix / punct 已有实现，仍不暴露，判据是「调它需要什么依据」：
        //    - prefix 是词语联想档的**唯一**来源，关掉它等于把「联想类型 = 词语联想」变成空转,
        //      而用户已经有「联想类型 = 关闭」这个正主，两个入口关同一件事只会让人误关；
        //    - punct 只在智能联想档有意义（词语联想按定义不出标点），而智能联想档是移动端
        //      默认、桌面端要显式选——为一个桌面端少数人才会进入的档位挂一个常驻开关，
        //      收益抵不过卡片变长的代价。
        //    要调的人改配置文件（core 的 data/config.toml 四项各有注释）。
        "input.association.history",
        "input.association.bigram",
        "input.association.prefix",
        "input.association.punct",
        // ⑲ 拼音语法模型（core 侧 octagram 接入新增）：**实验性且模型文件不随安装包分发**。
        //    weight = 0.0 与 model = "" 两个默认都是关，且必须两个都显式配置才生效；
        //    模型因许可与体积（数百 MB）要用户自行获取。⇒ 在设置页放这两项，绝大多数用户
        //    拧了完全没反应——没有模型文件时调 weight 是空转，而 model 是个要填文件名的
        //    输入框，填什么只能去看设计文档。与 ⑧、⑱ 同一判据。
        //    真正暴露的前提是 core 先决定模型怎么分发（见 core 的
        //    docs/design/pinyin-grammar-model.md）；那时该配的多半也不是这两个裸键，
        //    而是「下载/选择模型」那样的功能入口。
        //
        //    注：这两个键是 core 早先引入却未同步本仓的遗留项，此前因旧快照里没有它们而
        //    一直没报红，是本次重生成把它们激活的——不是本次 core 改动带来的。
        "schema.pinyin.grammar.weight",
        "schema.pinyin.grammar.model",
        // ⑳ core 的 `[mobile.*]` 域：**移动端对桌面基线的覆盖**，桌面构建根本不读。
        //    本程序只在桌面端运行 ⇒ 这些键在这里既显示不出有意义的当前值，改了也不会
        //    影响本机任何行为。它们的编辑入口属于移动端设置页（安卓仓的 Compose 界面）。
        //
        //    ⚠️ core 将来往 [mobile.*] 加键时，这道测试会再报一次——**照样登记进这里，
        //    别接进清单**。判据不是「这个键有没有用」而是「它管的是不是本平台」。
        "mobile.association.kind",
        "mobile.association.mode",
        "mobile.association.punct",
        // ㉑ 联想的编码栏标识：纯文案定制项，出厂「联想输入」已够用。
        //    它只在「编码不嵌入宿主程序」时可见——而嵌入是出厂默认，多数用户根本看不到
        //    这个标识，却要在卡片里常占一行。要改的人手写 config.toml。
        //    ⚠️ 与 ⑱ 那四个来源开关不同，这一项**不是**「改了没反应」——它有实际作用，
        //    只是作用面窄。将来若卡片有余地，可以放回清单（type = "string"）。
        "input.association.hint",
        // ㉒' 工具栏自定义按钮（core 0.119）：**已有 GUI**，但没有属于自己的清单项。
        //
        //    它由 `ui.toolbar.items` 那一项（外观 → 工具栏 → 显示内容）**一并读写**：
        //    同一个对话框里既排顺序、开关显隐，也新建/编辑/删除按钮。两个键是同一个
        //    功能的两半（定义 + 引用），拆成两个清单项会让用户在两处来回跳，
        //    且「删了按钮但引用还在」这类不一致会变成用户要自己收拾的事。
        //
        //    故它在本名单里，理由是「没有独立清单项」而非「不该有 GUI」。
        //    ⚠️ 若日后给它单独立项，记得从本名单撤出（双向断言会提醒）。
        //
        //    2026-08-26 之前这里写的是「设计上不做专属控件」——用户要求做，已推翻。
        "ui.toolbar.buttons",
        // ㉕ 任务栏图标主字颜色的另外三格：**已有 GUI**，但没有属于自己的清单项。
        //    四格（中/英 × 浅/深任务栏）由 `ui.langbar.text_color_cn_light` 那一项
        //    （外观 → 任务栏图标 → 主字颜色）的 2×2 对话框一并读写，理由同 ㉒'：
        //    同一件事拆成四行就是用户要求消掉的「大量同类控件」。
        //    ⚠️ 若日后给其中某格单独立项，记得从本名单撤出（双向断言会提醒）。
        "ui.langbar.text_color_cn_dark",
        "ui.langbar.text_color_en_light",
        "ui.langbar.text_color_en_dark",
        // 注：㉔ 区块名列表两项（`input.rare_char.include_blocks` /
        // `schema.frequency.exclude_blocks`）曾登记在此，注明「待接入，缺区块多选控件」。
        // 2026-09-01 已接进清单（输入页的「生僻字」与「词频」两块），故从名单撤出。
        // 当时记下的那条约束仍然成立、并已写进控件文档：区块表 50 项而界面只列几个粗组，
        // 故写回必须只增删自己列出的项（`dialog_button_open_pairs`），
        // 用 `checkbox_group` 会把用户手写的区块名静默抹掉。
        // 注：㉓ 辅助码那两个键（`schema.pinyin.aux_code.*`）曾登记在此，注明「待做 GUI，
        // 不是永久豁免」。2026-08-22 已接进清单（候选行为 → 辅助码），故从名单撤出——
        // 本名单的双向断言不容许留下已被覆盖的条目。
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

    /// 剥掉**值域自带的语法修饰**，还原成清单 `options` 里那个裸值。
    ///
    /// 目前只有 `ui.toolbar.items` 有这种修饰：`-` 前缀 = 「在这个位置，但不显示」，
    /// 出厂的 `-s2t` 是**合法条目**而不是一个没登记的值。
    ///
    /// ⚠️ **按键名点名而不是对所有 strlist 一律剥 `-`**：别的键里 `-` 完全可能是值的
    /// 一部分（符号表、成对括号那类），一律剥就把真正的漏登记放过去了。加新的带语法
    /// 修饰的键时在这里登记一行，附上它的语法是什么。
    ///
    /// 放宽是安全的，因为下面那两个后果对它都不成立：`ui.toolbar.items` 的 writer
    /// （`ordered_pairs_write_value`）写的就是「全序 + 关掉的加 `-`」，`-s2t` 正是它
    /// 自己的输出格式，读得回也写得出，既不判脏也不会吃掉。
    fn strip_domain_syntax<'a>(key: &str, value: &'a str) -> &'a str {
        match key {
            "ui.toolbar.items" => value.strip_prefix('-').unwrap_or(value),
            _ => value,
        }
    }

    /// ★★★ 勾选组（`checkbox_group`）的选项集**必须覆盖 core 的出厂值**。
    ///
    /// 漏一个值的后果有两个，都不会报错、也都不像是清单的问题：
    /// 1. **打开设置什么都不动就恒判「有未保存的更改」**——`is_dirty` 把全部 writer 跑一遍
    ///    再与 base 比，而 checkbox_group 的 writer 只写「勾选了的选项」，清单里没有的值
    ///    根本没有勾选信号，于是写回值天生就少一项。
    /// 2. 用户只要保存一次，那个值就被**静默吃掉**。
    ///
    /// 现场：`input.auto_pair.english_pairs` 的出厂值含 `<>`，清单漏了它
    /// （2026-08-27 用户报「进入设置什么也不操作，关闭时提示有内容修改」）。
    #[test]
    fn checkbox_group_options_cover_factory_values() {
        let caps = Capabilities::from_embedded();
        let manifest = crate::manifest::Manifest::from_embedded();
        let mut problems = Vec::new();
        for key in &manifest.order {
            let Some(item) = manifest.items.get(key) else {
                continue;
            };
            // ⚠️ 判据是「**声明了 options** 且配置类型是 strlist」，不是 `typ == "checkbox_group"`：
            //    `input.auto_pair.english_pairs` 的 typ 是 `dialog_button`（勾选组在对话框里渲染），
            //    按 typ 过滤会把它整个跳过——本测试第一版就是这么写的，反向验证时（把 `<>`
            //    从清单里删掉）它照样绿，等于白写。
            if item.options.is_empty()
                || caps.entries.get(key).map(|e| e.ty.as_str()) != Some("strlist")
            {
                continue;
            }
            let declared: Vec<&str> = item.options.iter().map(|o| o.value.as_str()).collect();
            let Value::Array(factory) = caps.default_for(key) else {
                continue;
            };
            for v in factory.iter().filter_map(|v| v.as_str()) {
                let bare = strip_domain_syntax(key, v);
                if !declared.contains(&bare) {
                    problems.push(format!("{key} 的出厂值 {v:?} 不在清单选项里"));
                }
            }
        }
        assert!(
            problems.is_empty(),
            "这些出厂值会被设置页吃掉，并让「打开就判脏」：{problems:#?}"
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

#[cfg(test)]
mod enum_domain_gate {
    use super::Capabilities;
    use crate::manifest::Manifest;

    /// StructList 成员字段的合成键（`表名#字段`）→ **同值域**的已登记键。
    ///
    /// core 的注册表不给 StructList 的成员字段单独立项（「一个配置键只能有一个 manifest 项」），
    /// capability 里因此查不到它们的值域 —— 而它们与某个已登记键是**同一个枚举**。
    ///
    /// ⚠️ 没有这张表的话，这几个下拉在闸门眼里根本不存在：加 `upright` 时故意漏掉
    /// `schema.mix_modes#candidate_layout` 一处，三条闸门全绿。**这正是本表存在的理由。**
    const STRUCTLIST_MEMBER_DOMAIN: &[(&str, &str)] = &[(
        "schema.mix_modes#candidate_layout",
        "input.temp_english.candidate_layout",
    )];

    /// 合成键 → 借来的值域键；普通键返回自身。
    fn domain_key(key: &str) -> &str {
        STRUCTLIST_MEMBER_DOMAIN
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, d)| *d)
            .unwrap_or(key)
    }

    /// 清单里**刻意只列一部分**取值的键：key → 为什么。
    ///
    /// 登记的门槛是「界面上少一个选项是有意的」，理由必须同时写在清单该项旁边——
    /// 这里只放键名与一句话，方便下一个人知道该去哪读。
    const NARROWED_ON_PURPOSE: &[(&str, &str)] = &[
        (
            "input.association.kind",
            "不列 smart：桌面上它与 word 完全等价，摆一个没区别的选项只会让人以为选错了",
        ),
        (
            "input.emoji.show_as",
            "不列 comment：注释区没有上屏入口，选了 emoji 只能看不能选；待注释上屏落地后加回",
        ),
    ];

    /// ★★ 清单的 select/segmented **不得漏掉 core 的取值**。
    ///
    /// 漏掉的后果不是「界面上少个选项」那么轻——读取时未知值回落下标 0、保存时把 0 写回去，
    /// 于是**用户在配置文件里手写的那个值，打开一次设置页再保存就没了**。
    /// `ui.candidate.layout` 已经这么丢过一次（core 值域从 3 变 4，清单滞后）。
    ///
    /// ⚠️ 这条闸门是补上的：此前只有 `schema_manager` 里那一处硬编码数组有守门，
    /// 清单里同一个值域的**另外四个下拉**完全裸奔——加 `upright` 时故意漏掉一处跑全量测试，
    /// 808 条全绿。
    ///
    /// **顺序不比**：呈现顺序是界面的自由（日志级别从严到松更好读）。比的是集合。
    #[test]
    fn manifest_never_omits_a_core_enum_value() {
        let caps = Capabilities::from_embedded();
        let man = Manifest::from_embedded();
        let mut missing: Vec<String> = Vec::new();
        for (key, item) in &man.items {
            // options_source = 选项在运行时动态填（如方案列表），与 capability 值域无关。
            if item.options.is_empty() || item.options_source.is_some() {
                continue;
            }
            let Some(e) = caps.entries.get(domain_key(key)) else {
                continue;
            };
            if e.ty != "enum" {
                continue;
            }
            if NARROWED_ON_PURPOSE.iter().any(|(k, _)| k == key) {
                continue;
            }
            let mine: Vec<&str> = item.options.iter().map(|o| o.value.as_str()).collect();
            for v in &e.values {
                if !mine.contains(&v.as_str()) {
                    missing.push(format!("{key}: core 有 {v:?}，清单没有（{mine:?}）"));
                }
            }
        }
        missing.sort();
        assert!(
            missing.is_empty(),
            "清单漏了 core 的取值，用户手写这些值会在保存时被静默改写：
{}",
            missing.join(
                "
"
            )
        );
    }

    /// 反向：清单**不得多出** core 不认的取值——选了会写进配置，而 core 解析不了。
    /// 这一条没有豁免：多出来的项永远是错的。
    #[test]
    fn manifest_never_invents_a_value_core_rejects() {
        let caps = Capabilities::from_embedded();
        let man = Manifest::from_embedded();
        let mut extra: Vec<String> = Vec::new();
        for (key, item) in &man.items {
            if item.options.is_empty() || item.options_source.is_some() {
                continue;
            }
            let Some(e) = caps.entries.get(domain_key(key)) else {
                continue;
            };
            if e.ty != "enum" {
                continue;
            }
            for o in &item.options {
                if !e.values.contains(&o.value) {
                    extra.push(format!(
                        "{key}: 清单有 {:?}，core 不认（{:?}）",
                        o.value, e.values
                    ));
                }
            }
        }
        extra.sort();
        assert!(
            extra.is_empty(),
            "清单编了 core 不认的取值：
{}",
            extra.join(
                "
"
            )
        );
    }

    /// 借域表不得过期：合成键必须还在清单里，且被借的键必须真是个 enum。
    /// 表项失效后闸门会静默跳过那个下拉——与压根没有这张表等价。
    #[test]
    fn structlist_domain_table_has_no_stale_entries() {
        let caps = Capabilities::from_embedded();
        let man = Manifest::from_embedded();
        for (synthetic, donor) in STRUCTLIST_MEMBER_DOMAIN {
            let item = man
                .items
                .get(*synthetic)
                .unwrap_or_else(|| panic!("借域表里的 {synthetic} 已不在清单里"));
            assert!(
                !item.options.is_empty(),
                "{synthetic} 没有选项了，借域没有意义"
            );
            assert!(
                caps.entries.get(*donor).map(|e| e.ty.as_str()) == Some("enum"),
                "{synthetic} 借的 {donor} 不是 enum"
            );
            assert!(
                !caps.entries.contains_key(*synthetic),
                "{synthetic} 现在 core 自己登记了，该把它从借域表里删掉"
            );
        }
    }

    /// 豁免名单本身不得过期：登记的键必须真的还在窄着，否则名单会掩护将来的真漏。
    #[test]
    fn narrowing_allowlist_has_no_stale_entries() {
        let caps = Capabilities::from_embedded();
        let man = Manifest::from_embedded();
        for (key, why) in NARROWED_ON_PURPOSE {
            let item = man
                .items
                .get(*key)
                .unwrap_or_else(|| panic!("豁免了不存在的项 {key}"));
            let e = caps
                .entries
                .get(domain_key(key))
                .unwrap_or_else(|| panic!("豁免了 core 没有的键 {key}"));
            let mine: Vec<&str> = item.options.iter().map(|o| o.value.as_str()).collect();
            assert!(
                e.values.iter().any(|v| !mine.contains(&v.as_str())),
                "{key} 已经列全了取值，该从豁免名单里删掉（登记理由：{why}）"
            );
        }
    }
}
