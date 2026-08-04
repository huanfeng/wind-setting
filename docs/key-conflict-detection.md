# 按键冲突检测 ↔ core 键分派对应

设置应用（wind-setting）在编辑按键相关配置时，实时检测"多个功能争用同一按键"的冲突，
以内容区固定顶栏常驻提示。本文说明检测模型，并将其与输入法内核（`../WindInput`，
`wind_input/` Rust 工程）的**真实按键分派行为**逐一对应，作为该逻辑的权威依据。

> 相关内核文档：`WindInput/docs/redesign/key-pipeline.md`（分派架构总设计）、
> `WindInput/docs/superpowers/specs/quick-input-trigger-key-conflict-detection-design.md`
> （触发键冲突设计，内核侧未实现，检测统一由本应用承担）。

## 0. 两级呈现：Blocking 常驻警告，Coexist 只 Toast 一次

判据是**内核对这次争用的实际处置**，不是"看起来像不像问题"：

| 级别 | 内核行为 | 呈现 | 措辞 |
|---|---|---|---|
| `Blocking` | 两个功能在同一输入状态下都想要这个键，内核只能选一个 → 另一个**静默失效** | 内容区固定顶栏（琥珀），改到没有为止 | ⚠ …冲突 |
| `Coexist` | 有确定的分时裁决，**两个功能都能用**，只是生效条件不同 | 刚配出这个组合时 Toast（Info）一次，说明实际行为 | 「有候选时…，无候选时…」 |

分级函数是 `key_conflict::pair_level`，**按分组 `id` 匹配、不看 `label`**——规则若按文案匹配，
改一次措辞就会静默失掉分级，而失效的表现是「本该提示一次的 Toast 变成常驻警告条」，
没人会去查代码。

Coexist 只在集合**新增**成员时弹：打开设置页时配置里本来就有的组合不是"刚发生的事"，
每次开窗弹一遍只会让人学会忽略它。首轮求值只建基线（`conflict_watcher` 里的 `seen`）。

> 常驻警告条的语气是"这里坏了，去改"。把可用的设计放进去，用户会去改一个本来没坏的配置——
> 这比不提示更糟。

## 1. 核心原则：按"输入状态"分域，跨域不算冲突

内核对一次按键的处理是**状态相关**的：同一物理键在不同输入状态下被路由到不同功能
（`wind-coordinator/src/coordinator.rs:3194` `handle_key_event`）。因此"同一物理键被多个
功能使用"**未必是冲突**——只有当两个功能在**同一输入状态**下都可能响应该键时，才是真冲突。

据此把检测拆成若干**场景域**，仅在域内两两检测、**跨域不报**。例如 `;` 在空缓冲态是临时
拼音触发键、在有候选态是次选键，二者分时复用，不算冲突。

## 2. 场景域划分与内核依据

| 域 | 生效输入状态 | 覆盖配置 | 内核依据（file:line） |
|---|---|---|---|
| **触发键** | 空缓冲、无候选 | `input.temp_pinyin.trigger_keys`、`input.temp_english.trigger_keys`、`schema.mix_modes[].trigger_keys`、`schema.special_modes[].trigger_keys` | `handle_lifecycle.rs:54-200` 激活优先级链：temp_english > temp_pinyin > special > mix |
| **候选键** | 有候选时 | `keys.select_key_groups`、`keys.page_keys`、`keys.highlight_keys`、`keys.select_char_keys`、**`keys.toggle_mode_keys`**（见 §4） | 次选 `coordinator.rs:3832`/`handle_candidate.rs:585`；以词定字 `:3443`（先于翻页）；翻页/高亮 `:3457` |
| **快捷键** | 组合键命令（多为中文态/全局） | 9 个 `hotkey` 项（`switch_engine`/`toggle_full_width`/`toggle_punct`/`toggle_s2t`/`toggle_toolbar`/`open_settings`/`add_word`/`open_add_word_dialog`/`activate_ime`）+ `pin_candidate`/`delete_candidate` + **`keys.schema_hotkeys`**（方案直达）+ **`input.temp_pinyin.hotkey`**、**`schema.special_modes[].hotkey`**（模式直达） | 热键匹配 `coordinator.rs` 的 `match_key_down`；注册表编译在 `wind-config/src/hotkey.rs` 的 `compile` |

快捷键域的三类来源在内核是**同一张 key_down 表**，匹配走 `.find()` → **先注册者赢、后者静默
失效**，故必须放在一个域里比。后两类（方案直达 / 模式直达）此前完全没参与检测：在方案管理
对话框里把某方案配成 `Ctrl+Shift+E`（＝切换方案的出厂值）是点得出来的，配完毫无提示。

`pin_candidate`/`delete_candidate` 的值是**模板** `ctrl+number`，`expand_hotkey` 把它展开成
`Ctrl+0`…`Ctrl+9` 十个具体键再比——热键控件的主键列表里有 0-9（`manifest::hotkey_keys`），
"把打开设置配成 `Ctrl+1`"同样是 GUI 点得出来的操作，不展开就检不出这层重叠。
同一对分组撞多个键时文案合并成一条（超过 3 个缩写成「… 等 N 个键」），否则十条会刷满警告条。

实现：`src/state.rs` 的 `collect_trigger_key_groups` / `collect_candidate_key_groups` /
`collect_hotkey_groups`，各经 `check_domain(groups, scene)` 跑
`key_conflict::find_key_conflicts` + `conflict_summary`；`live_conflicts()` 合并三域。

## 3. 键名归一化

检测需把两套命名归一到同一物理键空间再比较。内核的权威解析在
`wind-config/src/hotkey.rs:322-403`（`parse_key_name`），本应用的归一在 `src/key_conflict.rs`。

- **触发键（单键名）**：`backtick`/`semicolon`/`comma`/`lbracket`… 直接作物理键；
  `key_symbol` 映射为符号（`` ` ``、`;`、`,`、`[`…）。内核接受别名（`.`≡`period`、
  `=`≡`equal`≡`equals`、`-`≡`minus`≡`hyphen`），本应用取值域来自单一 manifest 选项、命名
  统一，故未处理别名（见 §5 局限）。
- **候选组名（`expand_candidate_key`）**：成组键名展开成物理键——
  - `shift_tab`(翻页) 与 `tab`(高亮移动) 是同一对物理键 Tab/Shift+Tab，归一到 `tab`；
  - `lrshift`/`lrctrl` 展开成 `{lshift,rshift}`/`{lctrl,rctrl}`，以便与 `toggle_mode_keys` 匹配；
  - 其余（`comma_period`/`minus_equal`/`brackets`/`semicolon_quote`…）域内同名即同键，保持单 token。
- **组合键（`canonical_hotkey`）**：拆分 `+`、大小写无关、修饰键按固定序（Ctrl<Shift<Alt<Win）
  排前、主键在后，归一形直接作显示（`Ctrl+Shift+E`、`Ctrl+数字`）。修饰键别名 `win/super/meta/cmd`
  统一为 Win（对齐 `manifest.rs`；注意内核目前仅处理 Windows VK，不识别 `cmd`，见 §5）。

## 4. 关键：中英切换键 vs 次选键 —— 唯一的 Coexist（核心校准点）

`keys.toggle_mode_keys`（中英切换，值如 `lshift`/`rshift`/`lctrl`/`rctrl`/`capslock`）与
`keys.select_key_groups` 里的修饰键组（`lrshift`/`lrctrl`）争用同一个物理修饰键。

**内核 2026-08-03 起两者都在 KEY_UP 处理，并有明确裁决**（`handle_candidate.rs`
`handle_select_key_up`，调用点在 `coordinator.rs` 的 keyup 分支、**先于**
`is_toggle_mode_keycode`）：

| 状态 | 行为 |
|---|---|
| 有候选、位次够 | 选中该候选，**不切**中英文 |
| 有候选、位次不够（页内候选不足） | 吞键，既不选也不切 |
| 无候选 | 中英切换 |

即两个功能**都能用**，是分时复用 → `Coexist`，走 Toast 而非常驻警告条。

> 为什么修饰键选词在 KEY_UP：纯修饰键的 KEY_DOWN 不能吃（吃了宿主就看不见修饰键，
> AutoCAD 正交模式会失效并卡顿），且在 KEY_DOWN 判定会让 `Ctrl+A` 的第一下 Ctrl 就选走候选。
> TSF 侧复用切换键那套「轻敲」机制（<500ms + 中途按别的键即取消）。
> 详见主仓 `AGENTS.md` 的「候选导航键」一节。

**⚠ 本节结论被反转过一次。** 2026-08-03 之前，修饰键选词吃 KEY_DOWN、切换在 KEY_UP 等不到
`had_pending`，用户设定的"右 Shift 切中英"在有候选时确实静默失效，那时它是 `Blocking`，
本文与代码也都按真冲突写。是**内核行为变了**，这里跟着变——不是当初判错。
守护用例：`state.rs` 的 `test_toggle_mode_vs_select_key_coexists`（内核若改回去，它会先红）。

## 5. 已知局限与刻意取舍

- **混合模式触发键横跨两域未检**：`mix_modes` 触发键在空缓冲态是触发键，在**有候选**态还会
  作为次选溢出分支被检查（`coordinator.rs:3849`）。它与次选键的候选态争用由内核按优先级
  （次选优先）解决。纳入检测会显著增加复杂度且多为可控行为，故当前**仅文档说明、不检测**。
- **别名不归一**：本应用不把 `=`/`equal`、`.`/`period` 等内核别名视为等价——因取值均来自统一
  manifest 选项、命名一致，实际不会出现混写。若未来配置来源多样化需补别名归一。
- **`cmd` 修饰键**：本应用将 `cmd` 归入 Win 以与 `manifest.rs` 一致，但内核当前只处理 Windows
  VK、不识别 `cmd`（macOS 为遗留 Go 实现）。跨平台若启用 `cmd` 需内核同步。
- **修饰/导航键的跨域复用**：如左右 Ctrl 既可作次选(lrctrl)又可作中英切换——已按 §4 纳入；
  但方向键/Tab 等与触发键标点不同层，天然不撞，无需跨域处理。
- **同帧多条 Coexist 只弹最后一条**：`feedback` 是单值信号，同一帧内写两次后写覆盖先写。
  实际影响很小——共存文案按「一对分组」描述，一次勾选撞上左右两个修饰键时两条内容仅差一个
  键名。要逐条播报得给 toast 加队列，不值当。
- **Coexist 不入库存**：关掉设置窗再打开，已存在的共存组合不会重新提示（首轮只建基线）。
  这是刻意的——见 §0。用户想回看规则得查文档或本页 hint。

## 6. 实现位置索引

| 关注点 | 文件 |
|---|---|
| 冲突算法、分级、键名/组合键归一、文案 | `src/key_conflict.rs`（`KeyGroup`、`ConflictLevel`、`pair_level`、`find_key_conflicts`、`key_symbol`、`expand_candidate_key`、`canonical_hotkey`、`expand_hotkey`、`conflict_summary`、`coexist_note`） |
| 三域收集/检测、`live_conflicts`、probe 复用 | `src/state.rs`（`collect_*_key_groups`、`check_domain`、`LiveConflicts`、`live_conflicts`） |
| 固定顶栏 UI、每帧监听、Coexist Toast 去抖 | `src/pages/mod.rs`（`conflict_bar`、`conflict_watcher`）、挂载于 `src/shell.rs`（**须排在 `toast_sink` 之前**） |
| 按键取值域定义 | `src/assets/settings_manifest.toml`（`group = "keys"` 各项、触发键 options） |
