# 设置向导

## 目标

清风的出厂默认是按五笔用户配的。拼音（全拼/双拼）用户装好以后，要在好几个页面之间来回找设置才能调顺手。
设置向导分 4 步，帮新用户按自己的输入习惯快速配好：启用哪些方案、主类型的常用选项、通用的几个开关。

**成功判据**

- 新装用户第一次打开设置就看到向导；4 步以内能把五笔用户或拼音用户的常用配置调好。
- 向导走到最后一步并确认之前，不改动任何实际配置。老用户可以放心打开看一看，中途关掉不留痕迹。
- 最后一步列出将被覆盖的现有设置，二次确认后才写入。

## 入口与弹出

- **入口**：左侧导航最底部的固定动作项「设置向导」。点击后弹出向导，不切换页面。
  样式仿照导航里的「词库」动作项。
- **强制弹出**：只看 `wind_setting.toml` 里的 `[ui] wizard_dismissed`，**所有用户都弹一次**，
  包括升级上来的老用户。弹出时机是启动后 `apply_loaded` 完成、数据就绪之后。
- **下次不再弹出**：勾选框放在向导底栏，每一步都可见。
  - 关闭向导或点「完成」时，按勾选状态写 `wizard_dismissed`。
  - 点「完成」并且写入成功后，一律写 `wizard_dismissed = true`。
- **与设置页未保存修改的关系**：打开向导前，如果设置页有未保存的修改，先弹窗让用户选
  「保存 / 放弃 / 取消」。这是为了防止向导写入并重新拉取配置之后，设置页的脏状态与 core 互相覆盖。
  首次强制弹出时设置页不可能有修改，所以不会出现这一步。

## 欢迎页

打开向导先到欢迎页（设置页有未保存修改时先过「保存 / 放弃 / 取消」那一页，处理完再到欢迎页）。
标题为「设置向导」，不计入「第 n/4 步」的步数。

- 一句介绍：清风输入法有大量功能与选项；向导只按输入习惯（五笔 / 全拼 / 双拼）配好最常用的几项，
  几步即可完成，点「完成」并确认之前不会改动任何设置。
- 「了解更多」一节，三行可点的链接，点击用系统浏览器打开：
  - 功能文档 → `https://windinput.com/docs`
  - 主题市场（浏览与下载主题）→ `https://market.windinput.com`
  - 主题编辑器（自己动手做主题）→ `https://theme.windinput.com`

  地址常量与设置页其它入口共用 `pages/mod.rs` 的 `DOCS_URL` / `THEME_MARKET_URL` / `THEME_EDITOR_URL`。
- 底栏：「开始」进第 1 步；「下次不再弹出」勾选框同样显示。第 1 步的「上一步」回到欢迎页。

## 四个步骤

所有选项打开时都按**当前实际配置**预填。第 1 步切换主类型时，第 2、3 步改用该类型的推荐预设，
用户可以再改。

### 第 1 步：输入习惯与方案

- **主类型**（单选）：五笔/码表 · 全拼 · 双拼。
  - 选双拼时，另选布局：小鹤、自然码、微软、搜狗、智能 ABC、加加、紫光、手心，
    取值来自 `schema.layouts`，出厂 `xiaohe`。
  - 主类型对应的方案（`wubi86` / `pinyin` / `shuangpin`）会设为默认方案，并且强制启用，不能取消勾选。
- **一起启用的其它方案**（多选）：列出 `schema.list` 返回的全部方案。预勾规则：

| 主类型 | 默认方案 | 预勾 |
|---|---|---|
| 五笔/码表 | `wubi86` | `wubi86_pinyin`、`pinyin` |
| 全拼 | `pinyin` | 无 |
| 双拼 | `shuangpin` | 无 |

  `english`、`stroke` 列出但不预勾。预填时如果主类型与当前默认方案一致，就按当前的 `schema.available` 勾选。

- **主类型的反推**：当前默认方案是 `pinyin` 时为全拼，是 `shuangpin` 时为双拼，其余一律为五笔/码表。

### 第 2 步：主类型的常用选项

**五笔/码表**

| 选项 | 键 | 预设 | 写入 |
|---|---|---|---|
| 全码唯一自动上屏 | `schema.codetable.auto_commit_at_full` | 开 | bool |
| 满码空码清空 | `schema.codetable.clear_on_empty_max` | 开 | bool |
| 拼音反查（打拼音时显示五笔编码） | `schema.pinyin.code_hint_source` | 开 | 开 → `codetable`，关 → `off` |

拼音反查的说明：
- 当前值为 `codetable`、`auto` 或 `shuangpin` 时，开关显示为开；为 `off` 时显示为关。
- 用户没有拨动开关时，原值保持不变，不会把 `auto` 改成 `codetable`。
- 临时拼音另有一份同名键 `input.temp_pinyin.code_hint_source`（出厂 `auto`），向导不改它。

**全拼/双拼**

| 选项 | 键 | 预设 |
|---|---|---|
| emoji 候选 | `input.emoji.enabled` | 开 |
| 模糊音 | `schema.pinyin.fuzzy.enabled` | 关 |
| 模糊音：zh/z、ch/c、sh/s、n/l | `schema.pinyin.fuzzy.{zh_z,ch_c,sh_s,n_l}` | 关；只在模糊音开时出现，缩进列在其下 |
| 允许按全拼输入（仅双拼） | `schema.pinyin.shuangpin.allow_full_pinyin` | 关 |

拼音这一步的项目以后还会再定，首版先放这几项。`model` 层按「项目表」组织，加项目时不用改流程。

### 第 3 步：通用设置

| 选项 | 键 | 预设 | 显示条件 |
|---|---|---|---|
| 快捷输入启用键 | `keys.key_actions` 里 `mix:quick_mix` 的绑定 | 分号 | 都显示 |
| / 键输出顿号 | `input.punct.custom_mappings` 的 `/` 行，以及 `input.punct.custom_enabled` | 当前值 | 都显示 |
| 临时拼音 | `input.temp_pinyin.enabled` | 开 | 仅五笔/码表 |
| 临时拼音触发键 | `keys.key_actions` 里临时拼音的绑定（出厂反引号） | 反引号 | 仅五笔/码表，且临时拼音开时 |

- **快捷输入启用键**：单选，选项为分号、反引号、单引号、不启用。
  - 选「不启用」时，清除 `mix:quick_mix` 的全部绑定。
  - 当前有多个绑定，或绑定不在这几个选项里时，显示「保持现状（当前：…）」，不改动。
- **临时拼音触发键**：单选，选项为反引号、分号、单引号。处理规则同上。
- **键冲突**：快捷输入、临时拼音、/ 顿号三者占用的键不能重复。
  - 冲突时在对应行就地提示，并禁用「下一步」。
  - 判定复用 `key_conflict.rs`；它覆盖不到的组合，在 `model` 里补纯函数判定。
- **/ 键输出顿号**：复用 `manifest.rs` 的 `slash_dun_on` / `apply_slash_dun`，
  只改 `/` 行的中文两列；打开时一并打开 `input.punct.custom_enabled`，行为与设置页那个开关一致。
- **拼音用户不显示临时拼音**：保持现值，不做修改。

### 第 4 步：确认

- 列出所有「当前值 → 新值」不同的项，按「方案 / 输入习惯 / 通用」分组，用中文标签和值描述。
  没有任何改动时显示「当前配置已符合所选，无需修改」，「完成」只负责关闭向导。
- **方案 override 提示**：五笔类的码表键和拼音类的键都是全局基线，方案可以通过 `schema_overrides` 覆盖。
  如果目标默认方案对向导会改的键有 override，就在该行提示：
  「方案 X 有独立设置，此项修改对它不生效，可在方案设置里调整」。向导不改 override。
- 有改动时点「完成」，会二次确认「将覆盖以下 N 项现有设置」。确认后写入。

## 关闭语义

任何一步关闭，或在二次确认时取消，都不改动 core 配置，只按勾选写 `wizard_dismissed`。

## 实现结构

全部在 wind-setting 内完成，core 不改。

```
src/wizard/
  mod.rs      对外：open_wizard(state) / maybe_auto_open(state, ui_cfg)
  model.rs    纯数据 + 纯函数（可单测）
  view.rs     Element::dialog + WizardStep 信号 + visible_when 分段（照搬 update_dialog 模式）
  apply.rs    写入编排
```

**model.rs**

- `WizardKind { Codetable, Pinyin, Shuangpin }`
- `WizardDraft`：主类型、双拼布局、启用方案集、第 2、3 步各项的值。每项都标记是否被用户动过，
  对应「未拨动则不改原值」这条规则。
- `WizardDraft::from_current(cfg, schemas, layout)`：按当前配置预填。
- `WizardDraft::apply_preset(kind)`：切换主类型时套用预设。
- `WizardDraft::diff(&current) -> Vec<Change>`：生成第 4 步的列表，每个 `Change` 带分组、标签、旧值、新值。
- `WizardDraft::to_writes(&current) -> Writes`：拆出三类写入：
  1. 全局 `config.setItems` 的 `[{key, value}]`，含 `keys.key_actions` 和 `input.punct.*`；
  2. `shuangpin` 方案级的 `schema.saveConfig` 布局（只在布局有变化时写）；
  3. 启用列表 `schema.available` 加默认方案。
- `key_conflicts(&draft) -> Vec<Conflict>`。

**apply.rs**：顺序写入，每一步都检查返回值和 `skipped`。

1. `config.setItems`（全局键，含 `schema.available`）
2. `schema.saveConfig`（双拼布局）
3. `schema.setActive`（默认方案；它要求目标方案已启用，所以放在启用之后）

- 任何一步失败都停下。对话框里显示失败的步骤和错误原文，并列出已经成功的步骤，不显示成功。
- 全部成功后重新拉取 `LoadedState`，让设置页显示新值。
- 写入入口经 `trait CoreCalls`（或闭包）注入，方便单测用 mock 记录调用序列。

**接线**

- `setting_config.rs` 的 `UiStateConfig` 加 `#[serde(default)] wizard_dismissed: bool`，
  经 `save_ui_state` 落盘。
- `state.rs` 加 `wizard_visible` 信号。
- `shell.rs`：根 stack 挂向导对话框；侧栏 nav 之后加底部固定的「设置向导」动作项，nav 用 weight 撑满中间。
- `main.rs`：`apply_loaded` 之后调用 `maybe_auto_open`。

## 测试

**model 单测**
- 预填：三种主类型的反推；`code_hint_source` 为 `auto` 且未拨动时，`to_writes` 不含该键。
- 预设：三种主类型的预勾与开关预设值；主类型对应的方案强制启用。
- diff：无改动时为空；有改动时内容与 `to_writes` 一致（同一个来源）。
- 触发键：「不启用」清除绑定；「保持现状」不产生写入；三者冲突能检出。
- / 顿号：复用现有函数，开关前后其它列不变。
- override 提示：目标方案有 override 时能检出。

**apply 单测**：用 mock 记录调用顺序：setItems → saveConfig → setActive；中途失败时停止并上报已完成的步骤。

**首启判定单测**：`wizard_dismissed` 缺省（旧版文件）时为 false，会弹出。

**靶机验证**（截图）
- 首启弹出；
- 三种主类型各走一遍；
- 中途关闭后 `config.toml` 不变；
- 完成后设置页显示新值，打字行为符合所选（全码自动上屏、拼音反查编码、emoji、/ 出顿号）。

## 不在本期

- 拼音用户的更多选项（后续再定，按项目表追加即可）。
- 向导内修改方案 override。
- 五笔以外码表（如 `stroke`）的专属选项。
