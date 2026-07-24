# 按键冲突检测 ↔ core 键分派对应

设置应用（wind-setting）在编辑按键相关配置时，实时检测"多个功能争用同一按键"的冲突，
以内容区固定顶栏常驻提示。本文说明检测模型，并将其与输入法内核（`../WindInput`，
`wind_input/` Rust 工程）的**真实按键分派行为**逐一对应，作为该逻辑的权威依据。

> 相关内核文档：`WindInput/docs/redesign/key-pipeline.md`（分派架构总设计）、
> `WindInput/docs/superpowers/specs/quick-input-trigger-key-conflict-detection-design.md`
> （触发键冲突设计，内核侧未实现，检测统一由本应用承担）。

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
| **快捷键** | 组合键命令（多为中文态/全局） | 9 个 `hotkey` 项（`switch_engine`/`toggle_full_width`/`toggle_punct`/`toggle_s2t`/`toggle_toolbar`/`open_settings`/`add_word`/`open_add_word_dialog`/`activate_ime`）+ `pin_candidate`/`delete_candidate` | 热键匹配 `coordinator.rs:3310`；候选管理键（Ctrl+数字）`:3343` |

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

## 4. 关键：中英切换键 vs 次选键（核心校准点）

`keys.toggle_mode_keys`（中英切换，值如 `lshift`/`rshift`/`lctrl`/`rctrl`/`capslock`）在内核
**KEY_UP（松开）** 处理（`coordinator.rs:3225`、`handle_mode.rs:540` `is_toggle_mode_keycode`），
**即便候选窗打开也会触发**；次选键 `keys.select_key_groups` 的 `lrshift`/`lrctrl` 在
**KEY_DOWN（按下）** 处理（`coordinator.rs:3832`、`handle_candidate.rs:585`）。

把同一个右 Shift 同时设为"中英切换"和"次选键"时，有候选状态下：

1. 按下右 Shift（KEY_DOWN）→ 命中次选 → **提交第 2 候选**，切换意图被覆盖；
2. 松开右 Shift（KEY_UP）→ 中英切换检查触发，但候选已提交、`had_pending` 不成立 →
   回退 PassThrough，不切换。

即内核用事件类型确定性地解决了争用，但**用户设定的"右 Shift 切中英"在有候选时失效**——这是
用户不易察觉的真冲突。`keys.commit_on_switch`（默认 true，`handle_mode.rs:4220`）决定切换时
是否把编码上屏。**因此本应用把 `toggle_mode_keys` 纳入候选域检测**（与次选键 lrshift/lrctrl
比对），提示形如："候选键冲突：右Shift 键同时被「次选/三选键」「中英切换键」使用"。

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

## 6. 实现位置索引

| 关注点 | 文件 |
|---|---|
| 冲突算法、键名/组合键归一、文案 | `src/key_conflict.rs`（`find_key_conflicts`、`key_symbol`、`expand_candidate_key`、`canonical_hotkey`、`conflict_summary`） |
| 三域收集/检测、`live_conflicts`、probe 复用 | `src/state.rs`（`collect_*_key_groups`、`check_domain`、`live_conflicts`） |
| 固定顶栏 UI、每帧监听 | `src/pages/mod.rs`（`conflict_bar`、`conflict_watcher`）、挂载于 `src/shell.rs` |
| 按键取值域定义 | `src/assets/settings_manifest.toml`（`group = "keys"` 各项、触发键 options） |
