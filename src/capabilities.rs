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
        // key_action 型控件的 `key` 只是标识符（真实落点是 keys.key_actions），
        // 故按被标识字段的原类型校验：strlist（trigger_keys）或 structlist（mix_modes）。
        "dialog_button_key_action" => ty == "strlist" || ty == "structlist",
        "dialog_button_mix_members" => ty == "structlist",
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
        "ui.tooltip.pinyin_max_readings",
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
        // ⑨ 拼音侧临时词晋升次数暂不暴露（码表侧的 auto_phrase.promote_count 已在清单内）。
        "schema.pinyin.auto_learn.promote_count",
        // ⑨' 码表造词的兜底与容量参数，普通用户无判断依据，需要时改配置文件。
        //    注：min_phrase_len / max_phrase_len 已在清单内（hidden=true）。
        "schema.codetable.auto_phrase.idle_timeout_ms",
        "schema.codetable.auto_phrase.temp_max_entries",
        // ⑨'' 码元字符集：core 的 REGISTRY 已注册（config_schema.rs），但消费侧仍在开发中，
        //     暂不接入设置界面。**待 core 定稿后再评估**该不该暴露、放全局清单还是方案对话框。
        "schema.codetable.input_chars",
        "schema.codetable.leading_chars",
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
        // ⑭ 加词面板的候选布局（core 侧模式级候选布局改造新增）：加词面板没有独立设置
        //    分区，且它逐字确认的两行提示本就该竖排——出厂 vertical 即最优解，横排没有
        //    使用场景。同批的临拼/临英/网址/快捷四项都已进清单，只有它刻意不暴露。
        "input.add_word.candidate_layout",
        // ⑮ 候选注释词库挂载列表（core 侧候选注释改造新增）：**待接入，不是设计上不暴露**。
        //    它是用户可见功能（挂载英汉释义、emoji 名称等），理应有 GUI；但形态是可增删的
        //    条目列表（id/label/path/enabled/schemas 五个字段），manifest 的「一个 key 一个
        //    控件」模型套不上，需要 schema_manager.rs 那种列表编辑器 + 文件选择。
        //    在此之前由用户手写 config.toml（文档 customize/candidate-comment）。
        "ui.comment_dicts",
        // ⑯ `keys.schema_hotkeys` 已从这份白名单**移除**：core 侧该键已废弃并移出登记表
        //    （并入 `keys.key_actions` 的 `switch_schema:<方案id>`）。方案页「进入方式」
        //    那一节现在读写的是 key_actions，而它就在下面一条里，理由完全相同。
        // ⑰ 任务栏图标标记的微调项（core 侧语言栏图标改造新增）：两个开关
        //    （punct_badge / full_width_mark）已在清单内，这七项是它们的**外观微调**。
        //
        //    不暴露的判据是「调它需要什么依据」：这些量的取值只能靠盯着 16×16 的任务栏图标
        //    反复比对——大小差 0.1 倍、不透明度差 0.05 在设置页的预览里根本看不出来，
        //    而放进去就等于给用户七个只能盲调的旋钮。真要调的人本来就得开着图标一点点试，
        //    改配置文件反而更顺手（core 的 data/config.toml 每项都有注释）。
        //
        //    ⚠️ badge_alpha 尤其不能"顺手加个滑块"：它同时是**档位开关**（= 1.0 走实心挖空、
        //    < 1.0 走半透明保留主字，两条互斥的路径），滑到 1.0 会整个换一种画法。
        //    滑块表达不了这种跳变，用户拖到底会以为"最不透明"，实际得到的是另一种设计。
        "ui.langbar.punct_badge_scale",
        "ui.langbar.full_width_mark_scale",
        "ui.langbar.badge_alpha",
        "ui.langbar.colored",
        "ui.langbar.punct_color_cn",
        "ui.langbar.punct_color_en",
        "ui.langbar.full_width_color",
        // ⑱ 联想的四个「来源开关」（core 侧联想改造新增）：主干六项已在清单内，这四项决定
        //    联想候选**从哪来**，按配置文件里的书写顺序取配额。
        //
        //    history / bigram 是**尚无实现**的来源（core 的 data/config.toml 上标着「待实现」），
        //    开着也不会出候选——与 ⑧ 的两个死链系数同一处理：给用户拧了没反应的旋钮，
        //    比不放更糟。等各自的数据来源落地后再评估。
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
        // ㉒ 辅助码（拼音候选的字形二次筛选，core PR #68、合并提交 529b6d80 新增）：
        //    **待做 GUI，不是永久豁免**（同 ④'）。core 侧两个键已登记进 REGISTRY，设置端
        //    的控件是辅助码那条线自己列明的「未做（留给后续）」项，不是设计上不该暴露。
        //
        //    不在本次接进清单的原因有二：
        //    1. 它属于辅助码功能自身的完成度，而 hint 措辞要说清一个微妙前提——**全拼方案
        //       的反引号出厂被音节分隔符占用，绑了也进不去**（见 core `config_schema.rs`
        //       该键上方注释）。措辞做错就是给用户挖「配了没反应」的坑，这个决定该由做那条
        //       线的人下。清单里 `shuangpin.allow_full_pinyin` 有同类先例可循：`enabled_when`
        //       表达不出「当前方案是哪种」，故常驻可见、由 hint 交代适用范围。
        //    2. `enabled` 只是**全局基线**，方案段 `[engine.aux_code]` 的 tri-state 覆盖才是
        //       真开关（`AuxCodeGlobal::resolved`）——「双拼开、全拼关」正是靠它表达。于是
        //       「全局页放一个基线开关，还是只在方案设置对话框里配」是要拍板的设计题，不是
        //       接两个 [[items]] 的机械活。与 ⑨'' 的「放全局清单还是方案对话框」同一类。
        //
        //    待接入时的候选方案（存此省后来人一轮推导；section/subsection 与 hint 仍需定夺）：
        //      key = "schema.pinyin.aux_code.enabled"
        //      group = "schema"；section = "候选行为"；subsection = "辅助码"；type = "toggle"
        //      hint = "用字形码二次筛选拼音候选；全拼方案需先把引导键从音节分隔符改开"
        //
        //      key = "schema.pinyin.aux_code.max_phrase_len"
        //      group/section/subsection 同上；type = "number"；min = 0；max = 20
        //      enabled_when = "schema.pinyin.aux_code.enabled == true"
        //      hint = "超过这个字数的词组不参与筛选；0 = 不限，单字不受限"
        //
        //    （`number` 是 int 的合法控件，已对过 `control_type_compatible`；max_phrase_len
        //     的语义取自 core `config.rs`：字数 > 此值的**词组**一律排除，0 = 不限，单字恒
        //     参与——注意是 >，不是 ≥，等于上限的词组保留。）
        "schema.pinyin.aux_code.enabled",
        "schema.pinyin.aux_code.max_phrase_len",
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
