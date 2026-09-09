# 设置页布局对应表（对齐参考项目 WindInput/wind_setting）

> 目的：把 native 设置页（左栏导航 / 右栏卡片 / 字段）的**组织结构**对齐参考项目
> 旧版 Go 项目的 `wind_setting`（Go + Wails + Vue3，web 配置页）。
>
> **范围说明**：本表只重排「分组 / 分区 / 字段顺序 / 标签文案」。
> **config key 不照搬参考项目**——我们的 key 必须与本项目 core 的 capability 快照逐字一致
> （`src/assets/capabilities.snapshot.json`）。参考项目 key 仅作语义锚点。
>
> 决策（已确认）：
> 1. 跨页分歧**全部靠齐参考项目**。
> 2. 码表/拼音引擎设置**归入「方案」页**（全局级已落地为 schema 组「上屏行为/候选行为」；per-schema 级待弹窗）。
> 3. 「统计」页**临时隐藏**（功能未实现）。
> 4. 「短语」开关实际无作用 —— 设置端**已移除**，待 core 决定后再处理。
>
> 二轮修正（已落地）：
> - **状态提示**：并非整组删除，仅纯视觉项（颜色/字号/透明度/圆角）由主题处理；
>   行为/位置类 9 项（enabled/display_mode/duration/schema_name_style/position_mode/
>   offset_x,y/custom_x,y）**保留**于外观页「状态提示」卡。
> - **主题名称对接**：`ui.theme.name` 做成 `options_source=themes` 的清单下拉
>   （显示主题名、保存 slug），经 build_select 登记保存/加载/恢复；移除原不落盘手写主题卡。
>   theme.list 改存 (id/slug, 显示名) 对。
> - **功能说明（hint）**：按参考项目逐字补全输入/按键/外观/方案/高级各字段说明。

---

## 一、左栏导航对应

| 参考项目 | 我们当前 | 目标 |
|---|---|---|
| 方案 (general) | 方案 | 方案 |
| 输入 (input) | 输入 | 输入 |
| 按键 (hotkey) | 按键 | 按键 |
| 外观 (appearance) | 外观 | 外观 |
| 词库 (dictionary) | 词库 | 词库 |
| **高级 (advanced)** | 统计 | **高级**（前移） |
| **统计 (stats)** | 高级 | **隐藏**（未实现） |
| 关于 (about) | 关于 | 关于 |

目标导航顺序：`方案 / 输入 / 按键 / 外观 / 词库 / 高级 / 关于`

文件：`src/pages/mod.rs` 的 `all_tabs()`。

---

## 二、方案页（general → 「方案」）

参考结构：
- **方案管理**（自定义卡片）：方案列表启用/禁用/排序/设为当前/详情/**方案设置**
- 分区「主方案设置」：主码表方案 `schema.primary_codetable`、主拼音方案 `schema.primary_pinyin`
- **方案设置弹窗**（`SchemaSettingsDialog`，per-schema，按引擎×Tab）：
  - 码表·基础：上屏行为 / 输入模式 / 常用功能 / 智能学习
  - 码表·高级：候选行为（候选排序 / 字符偏好 / 短码优先 …）
  - 拼音 / 混输：各自引擎设置

我们当前：方案管理（自定义）+ 分区「默认方案」（主码表/主拼音）。

**目标**：
- 分区名「默认方案」→「主方案设置」。
- **新增承载码表/拼音引擎设置**（见第七节迁入清单）。承载形式（per-schema 弹窗 vs 全局分区卡片）**待定**——
  取决于本项目 core 这些 key 是全局还是 per-schema。

---

## 三、输入页（input）—— 参考项目最「大」的一页

参考项目分区顺序与字段（靠齐目标）：

| # | 分区 | 字段（参考 label — 参考 key） |
|---|---|---|
| 1 | 字符与标点 | 候选检索范围 `input.filter_mode`；标点随中英文切换；数字后智能标点(+弹窗)；智能符号模式(+弹窗)；智能符号时限；自定义标点映射(+弹窗) |
| 2 | 按键行为 | 回车键功能 `input.enter_behavior`；空码时空格键功能；数字小键盘功能 |
| 3 | 候选无效按键 | 数字键无效时 `input.overflow.number_key`；次选三选键无效时；以词定字键无效时 |
| 4 | 简入繁出 | 启用简入繁出；转换变体 |
| 5 | 标点配对 | 中文标点自动配对(+弹窗)；英文标点自动配对(+弹窗) |
| 6 | 快捷输入 | 触发键；强制竖排显示；小数保留位数 |
| 7 | 临时拼音 | 拼音分隔符；触发键 |
| 8 | 临时英文 | Shift+字母行为；显示英文候选；允许输入符号与数字；空格作为输入字符；触发键 |
| 9 | 网址输入 | 启用网址输入模式；触发前缀 |
| 10 | 默认状态 | 记忆前次状态；初始语言模式；初始字符宽度；初始标点模式 |

**已知偏离（2026-07-28，刻意为之）**：智能符号从「字符与标点」拆成**独立卡片**，并把「标点配对」提到它下面，实际顺序为 `字符与标点 → 智能符号 → 标点配对 → 按键行为 → …`。原因：智能符号增加到三个独立开关（中文标点 / 英文标点 / 英文输入模式）+ 两个字符集 + 两个共用参数，共 7 项，塞在「字符与标点」里已经喧宾夺主；而配对与符号同属标点类，相邻更好找。同时「智能符号上屏方案 / 时限」去掉了 `enabled_when`——它们是三个开关共用的公共参数，挂在任何单个开关上都会造成「另外两个开着却调不了」。

为这张卡片新增了两个 manifest 机制（都是通用的，不是智能符号特判）：

- **`gate_soft = true`**：仍与 `gate_key` 合并成「启用 + 设置…」一行，但**按钮不随开关置灰**。用于对话框内容不专属于该开关的情形——「英文符号字符集」同时服务于英文标点智能符号，「中文符号字符集」的对话框里还寄居着别的开关；置灰会让另一条路的用户彻底够不着。
- **`dialog_extra_for = "<宿主 dialog_button 的 key>"`**：把一两个从属项塞进**已有的**值编辑对话框，不额外挂按钮。与 `opens_dialog`/`dialog_group` 那套「扩展设置」的区别是不多开一层套娃——为一个开关单开一个对话框按钮不划算。守护测试 `dialog_extra_for_targets_are_valid` 校验宿主存在、类型是 `dialog_button`、同 section，且**宿主必须 `gate_soft`**（否则寄居项在开关关闭时无法访问，且这种失联是静默的：项不独立成行，覆盖守护也查不出）。

我们当前→目标的逐项映射（我们的 key 不变，仅调 group/section/顺序/label）：

| 我们当前位置 | 我们的 key | → 目标分区 | 备注 |
|---|---|---|---|
| 输入/拼音与编码：字符集过滤 | `input.filter_mode` | 字符与标点 | 换卡 |
| 输入/标点与符号：标点跟随中英模式 | `input.punct.follow_mode` | 字符与标点 | |
| 输入/标点与符号：数字后智能标点 | `input.punct.smart_after_digit` | 字符与标点 | |
| 输入/标点与符号：数字后保持英文的标点 | `input.punct.smart_list` | 字符与标点 | 参考用弹窗承载，我们现为 string，可保留或改弹窗（待定） |
| 输入/标点与符号：智能符号模式 | `input.symbol.smart_mode` | **智能符号** | 已拆出独立卡（见上方「已知偏离」） |
| 输入/标点与符号：智能符号判定时限 | `input.symbol.smart_timeout_ms` | **智能符号** | 三场景共用，无 `enabled_when` |
| 输入/标点与符号：智能符号字符集 | `input.symbol.smart_chars` | **智能符号** | 参考归入智能符号弹窗 |
| （新增）英文标点智能符号 | `input.symbol.english_punct_mode` | **智能符号** | 不独立成行：`dialog_extra_for` 寄居在「中文符号字符集」对话框内 |
| （新增）英文模式智能符号 | `input.symbol.english_mode` | **智能符号** | 与「英文符号字符集」合并成「启用 + 设置…」行；开启会让 core 接管这些标点键 |
| （新增）英文符号字符集 | `input.symbol.english_chars` | **智能符号** | `gate_key = english_mode` + `gate_soft`（两个英文场景共用，不随单一开关置灰） |
| 输入/标点与符号：启用自定义标点映射(+弹窗) | `input.punct.custom_enabled` / `custom_mappings` | 字符与标点 | |
| 输入/上屏控制：有编码时回车行为 | `input.enter_behavior` | 按键行为 | |
| 输入/上屏控制：空码时空格行为 | `input.space_on_empty_behavior` | 按键行为 | |
| 输入/拼音与编码：小键盘行为 | `input.numpad_behavior` | 按键行为 | 换卡 |
| **按键/满码溢出处理**：满码后按数字键 | `keys.overflow.number_key` | **输入/候选无效按键** | **跨页迁入** |
| **按键/满码溢出处理**：满码后按选择键 | `keys.overflow.select_key` | **输入/候选无效按键** | **跨页迁入** |
| **按键/满码溢出处理**：满码后按以词定字键 | `keys.overflow.select_char_key` | **输入/候选无效按键** | **跨页迁入** |
| **高级/简繁转换**：简繁转换 | `input.s2t.enabled` | **输入/简入繁出** | **跨页迁入** |
| **高级/简繁转换**：简繁变体 | `input.s2t.variant` | **输入/简入繁出** | **跨页迁入** |
| 输入/标点与符号：中文标点自动配对(+弹窗) | `input.auto_pair.chinese` / `chinese_pairs` | 标点配对 | 拆出独立卡 |
| 输入/标点与符号：英文标点自动配对(+弹窗) | `input.auto_pair.english` / `english_pairs` | 标点配对 | |
| **高级/快捷输入**：快捷输入(日期/计算) | `schema.quick_input.enabled` | **输入/快捷输入** | **跨页迁入**；参考此卡首项是「触发键」 |
| **高级/快捷输入**：计算结果小数位数 | `schema.quick_input.decimal_places` | **输入/快捷输入** | **跨页迁入** |
| **高级/快捷输入**：快捷输入候选强制竖排 | `schema.quick_input.force_vertical` | **输入/快捷输入** | **跨页迁入** |
| 输入/拼音与编码：拼音分隔符 | `schema.pinyin.separator` | 临时拼音 | 注意：参考把分隔符放「临时拼音」卡 |
| 输入/拼音与编码：临时拼音触发键 | `input.temp_pinyin.trigger_keys` | 临时拼音 | |
| 输入/临时英文与大小写：Shift 行为 | `input.temp_english.shift_behavior` | 临时英文 | |
| 输入/临时英文与大小写：临时英文显示候选 | `input.temp_english.show_candidates` | 临时英文 | |
| 输入/临时英文与大小写：临时英文允许符号 | `input.temp_english.allow_symbols` | 临时英文 | |
| 输入/临时英文与大小写：临时英文空格作输入 | `input.temp_english.space_as_input` | 临时英文 | |
| 输入/临时英文与大小写：临时英文触发键 | `input.temp_english.trigger_keys` | 临时英文 | |
| 输入/临时英文与大小写：Shift 临时英文（总开关） | `input.temp_english.enabled` | 临时英文 | 参考无显式总开关项，待定保留 |
| 输入/临时英文与大小写：切换模式时取消大写锁定 | `input.capslock.cancel_on_mode_switch` | 临时英文 | 参考无此项（新增），暂归此卡 |
| 输入/网址输入：网址输入模式 | `input.url.enabled` | 网址输入 | |
| 输入/网址输入：网址前缀 | `input.url.prefixes` | 网址输入 | |
| 输入/启动状态：记住上次中英/标点状态 | `input.default.remember_last_state` | 默认状态 | 参考此项排第一 |
| 输入/启动状态：启动默认中文模式 | `input.default.chinese_mode` | 默认状态 | 参考用 segmented |
| 输入/启动状态：启动默认全角 | `input.default.full_width` | 默认状态 | 参考用 segmented |
| 输入/启动状态：启动默认中文标点 | `input.default.chinese_punct` | 默认状态 | 参考用 segmented |

**迁出输入页**（移到方案页，见第七节）：
- 输入/上屏控制：全码自动上屏 / 否决全码上屏 / 自动上屏最短码长 / 顶字上屏 / 满码无候选清空缓冲（`input.code_commit.*`）
- 输入/拼音与编码：候选排序模式 `schema.pinyin.candidate_order`、显示编码提示 `schema.pinyin.show_code_hint`、智能组词 `schema.pinyin.use_smart_compose`
- 输入/短语：短语前缀触发最小长度 / 短语候选最大显示字数（`input.phrase.*`）——**开关无作用，待核实**

---

## 四、按键页（hotkey）

参考分区顺序与字段：

| # | 分区 | 字段（参考 label — 参考 key） |
|---|---|---|
| 1 | 中英文切换 | 切换按键 `hotkeys.toggle_mode_keys`；切换时编码上屏 `hotkeys.commit_on_switch` |
| 2 | 候选词管理 | 置顶词条 `hotkeys.pin_candidate`；删除词条 `hotkeys.delete_candidate` |
| 3 | 候选操作 | 次选/三选 `input.select_key_groups`；高亮移动 `input.highlight_keys`；翻页 `input.page_keys`；以词定字 `input.select_char_keys` |
| 4 | 功能快捷键 | 切换方案/全半角/中英标点/状态栏/打开设置/快捷加词/打开加词界面/切换简繁/切换到本输入法/截图 |

我们当前→目标映射：

| 我们当前 | 我们的 key | → 目标分区 | 备注 |
|---|---|---|---|
| 按键/中英切换：切换中英修饰键 | `keys.toggle_mode_keys` | 中英文切换 | |
| 按键/开关：切换中英时上屏当前编码 | `keys.commit_on_switch` | 中英文切换 | 参考并入「中英文切换」卡 |
| 按键/键位：置顶候选 | `keys.pin_candidate` | 候选词管理 | 参考为 select，我们现为 string |
| 按键/键位：删除候选 | `keys.delete_candidate` | 候选词管理 | |
| 按键/候选键组：选词键组 | `keys.select_key_groups` | 候选操作 | |
| 按键/候选键组：移动高亮键组 | `keys.highlight_keys` | 候选操作 | |
| 按键/候选键组：翻页键组 | `keys.page_keys` | 候选操作 | |
| 按键/候选键组：以词定字键组 | `keys.select_char_keys` | 候选操作 | |
| 按键/键位：切换方案 | `keys.switch_engine` | 功能快捷键 | |
| 按键/键位：切换全/半角 | `keys.toggle_full_width` | 功能快捷键 | |
| 按键/键位：切换中/英标点 | `keys.toggle_punct` | 功能快捷键 | |
| 按键/键位：切换简/繁 | `keys.toggle_s2t` | 功能快捷键 | |
| 按键/键位：显示/隐藏工具栏 | `keys.toggle_toolbar` | 功能快捷键 | |
| 按键/键位：打开设置 | `keys.open_settings` | 功能快捷键 | |
| 按键/键位：造词 | `keys.add_word` | 功能快捷键 | |
| 按键/键位：切换到本输入法 | `keys.activate_ime` | 功能快捷键 | |

差异提示：
- **满码溢出（overflow）迁出**到输入页（见第三节）。
- 参考「功能快捷键」用 hotkey-composer（组合键编辑器），我们现为纯 string —— 控件升级待定。
- 参考有 `打开加词界面 / 截图` 等项，我们核心是否有对应 key 待核实。
- 「全局热键」`keys.global_hotkeys`：参考无独立卡，我们现为独立弹窗，暂保留。

---

## 五、外观页（appearance）

参考分区顺序与字段（节选关键差异）：

| # | 分区 | 字段（参考 key） |
|---|---|---|
| - | 主题选择/预览/Web编辑器（自定义卡片） | 主题列表、删除、导入、实时预览 |
| 1 | 主题 | 主题风格 `ui.theme.style`；翻页栏显示 `ui.candidate.pager_bar_display`；显示页码 `ui.candidate.page_number_display` |
| 2 | 候选窗口 | 字体大小/字体（手写）；每页候选数；扩展候选数；隐藏候选窗口；嵌入式编码行 `inline_preedit`；非嵌入编码显示方式；候选布局；上方反转；模式彩色边框 `mode_accent_border`；候选最大字符数；**命令直通车标注**（cmdbar 前缀，手写） |
| 3 | 候选项提示 | 悬停延时；编码反查；拼音反查；拆字反查；调试信息 |
| 4 | 状态提示 | 启用；显示模式；显示时长；方案名显示；位置模式；水平偏移；垂直偏移；字体大小；透明度 `opacity`；圆角 `border_radius` |
| 5 | 工具栏 | 显示工具栏；全屏隐藏 |

我们当前→目标映射：

| 我们当前 | 我们的 key | → 目标分区 | 备注 |
|---|---|---|---|
| 外观/主题（手写卡）：候选窗主题 | （未登记写回） | 主题选择卡片 | 与 ui.theme 合并，去掉重复 |
| 外观/主题（ui 组）：主题名称 | `ui.theme.name` | 主题 | 与手写主题卡合并 |
| 外观/主题（ui 组）：主题明暗 | `ui.theme.style` | 主题 | |
| 外观/候选窗：每页候选数 | `ui.candidate.per_page` | 候选窗口 | |
| 外观/候选窗：扩展档每页候选数 | `ui.candidate.per_page_extended` | 候选窗口 | |
| 外观/候选窗：候选窗排列 | `ui.candidate.layout` | 候选窗口 | |
| 外观/候选窗：编码显示方式 | `ui.candidate.preedit_display` | 候选窗口 | 对应参考「非嵌入编码显示方式」 |
| 外观/候选窗：隐藏候选窗 | `ui.candidate.hide_window` | 候选窗口 | |
| 外观/候选窗：候选字号 | `ui.candidate.font_size` | 候选窗口 | |
| 外观/候选窗：候选字号跟随主题 | `ui.candidate.font_size_follow_theme` | 候选窗口 | 参考无（新增），暂归此卡 |
| 外观/候选窗：候选文本最大字数 | `ui.candidate.max_chars` | 候选窗口 | |
| 外观/候选窗：自定义序号标签 | `ui.candidate.index_labels` | 候选窗口 | 参考无（新增） |
| 外观/候选窗：候选窗在上方时反转顺序 | `ui.candidate.flip_when_above` | 候选窗口 | |
| **高级/命令栏**：命令栏 | `input.cmdbar.enabled` | **外观/候选窗口** | **跨页迁入**（参考「命令直通车标注」） |
| **高级/命令栏**：命令栏候选前缀 | `input.cmdbar.candidate_prefix` | **外观/候选窗口** | **跨页迁入** |
| 外观/模式指示：模式指示样式 | `ui.mode_indicator.style` | 候选窗口或主题 | 参考无独立项，待定归属 |
| 外观/悬停提示：提示延迟 | `ui.tooltip.delay` | 候选项提示 | |
| 外观/悬停提示：显示编码 | `ui.tooltip.code_enabled` | 候选项提示 | |
| 外观/悬停提示：显示拼音 | `ui.tooltip.pinyin_enabled` | 候选项提示 | |
| 外观/悬停提示：显示拆字 | `ui.tooltip.chaizi_enabled` | 候选项提示 | |
| 外观/悬停提示：显示调试信息 | `ui.tooltip.debug_enabled` | 候选项提示 | |
| 外观/悬停提示：显示多音字所有读音 | `ui.tooltip.pinyin_heteronyms` | 候选项提示 | 参考无（新增） |
| 外观/悬停提示：每字最多读音数 | `ui.tooltip.pinyin_max_readings` | 候选项提示 | 参考无（新增） |
| 外观/状态提示：启用状态提示气泡 | `ui.status.enabled` | 状态提示 | |
| 外观/状态提示：显示模式 | `ui.status.display_mode` | 状态提示 | |
| 外观/状态提示：显示时长 | `ui.status.duration` | 状态提示 | |
| 外观/状态提示：定位方式 | `ui.status.position_mode` | 状态提示 | |
| 外观/状态提示：水平偏移 | `ui.status.offset_x` | 状态提示 | |
| 外观/状态提示：垂直偏移 | `ui.status.offset_y` | 状态提示 | |
| 外观/状态提示：固定位置 X/Y | `ui.status.custom_x` / `custom_y` | 状态提示 | 参考无（新增） |
| 外观/状态提示：方案名显示样式 | `ui.status.schema_name_style` | 状态提示 | |
| 外观/字体：候选窗字体 | `ui.font.family` | 候选窗口 | 参考「字体」在候选窗口卡 |
| 外观/字体：自定义字体文件 | `ui.font.path` | 候选窗口 | |
| 外观/字体：字体渲染方式 | `ui.font.render_mode` | 候选窗口 | 参考无（新增） |
| 外观/工具栏：显示工具栏 | `ui.toolbar.visible` | 工具栏 | |
| 外观/工具栏：全屏时隐藏工具栏 | `ui.toolbar.hide_in_fullscreen` | 工具栏 | |

参考独有、我们核心可能缺的 key（待核实是否存在）：
翻页栏显示 `pager_bar_display`、显示页码 `page_number_display`、嵌入式编码行 `inline_preedit`、
模式彩色边框 `mode_accent_border`、状态透明度 `opacity`、状态圆角 `border_radius`。

---

## 六、词库页 / 高级页 / 统计 / 关于

**词库（dictionary）**：参考为词库管理面板（快捷短语/方案、导入导出、方案操作）。
我们当前占位。结构对齐待功能实现（本期不动）。

**高级（advanced）**：参考分区 = 配置文件 / 桌面集成 / 日志设置 / 性能诊断 / 内存诊断（多为手写动作按钮）。
我们当前「高级」= 输入统计 / 简繁转换 / 快捷输入 / 命令栏 / 调试。
靠齐后我们高级页**仅剩**：
- 调试：日志级别 `debug.log_level`（参考「日志设置」）
- 输入统计开关 `stats.enabled` / `stats.track_english`：参考归「统计」页；统计页隐藏期间**暂留高级页**或随统计页隐藏（待定）。
- 简繁/快捷输入/命令栏均**迁出**（见上）。
- 参考的配置目录 / 备份还原 / 协议关联 / 性能内存诊断等**动作类**功能：我们多为占位，按需补。

**统计（stats）**：**隐藏**。`all_tabs()` 中移除该 Tab。

**关于（about）**：参考为英雄区 + 链接 + 群号 + 更新检查。我们当前为图标头部 + 系统信息卡。
基本一致，链接/群号/检查更新为后续增强（本期不强求）。

---

## 七、码表/拼音引擎设置 → 迁入「方案」页（细节待定）

参考项目把这些放在 **方案设置弹窗**（per-schema，按引擎×Tab），key 形如 `engine.codetable.*` / `learning.*`。
我们核心的对应 key（现散在输入页）需迁入方案页：

| 我们的 key | 当前位置 | 语义（参考对应分区） |
|---|---|---|
| `input.code_commit.auto_commit_at_full` | 输入/上屏控制 | 上屏行为·满码自动上屏 |
| `input.code_commit.auto_commit_block_on_pinyin` | 输入/上屏控制 | 上屏行为 |
| `input.code_commit.auto_commit_min_len` | 输入/上屏控制 | 上屏行为 |
| `input.code_commit.top_code_commit` | 输入/上屏控制 | 上屏行为·顶码上屏 |
| `input.code_commit.clear_on_empty_max` | 输入/上屏控制 | 上屏行为·满码空码清空 |
| `schema.pinyin.candidate_order` | 输入/拼音与编码 | 候选行为·候选排序 |
| `schema.pinyin.show_code_hint` | 输入/拼音与编码 | 常用功能·显示编码提示 |
| `schema.pinyin.use_smart_compose` | 输入/拼音与编码 | 智能组词 |
| `input.phrase.min_prefix` | 输入/短语 | （短语，开关无作用，待核实） |
| `input.phrase.max_display_chars` | 输入/短语 | （短语，待核实） |

**待定决策**：
1. 这些 key 在本项目 core 是**全局**还是 **per-schema**？决定方案页用「全局分区卡片」还是「per-schema 弹窗」。
2. 是否需要像参考那样区分 码表/拼音/混输 三套引擎设置 + 基础/高级 Tab。
3. 「短语」是否保留（开关无作用）。

---

## 八、落地涉及文件

| 改动 | 文件 |
|---|---|
| 导航顺序、隐藏统计、各页调哪些分组、手写卡片 | `src/pages/mod.rs` |
| group / section / 字段顺序 / label / hint | `src/assets/settings_manifest.toml` |
| 方案页引擎设置承载（新弹窗/分区） | 待定：`src/pages/mod.rs` + 可能新增 `src/dialogs/*` |
| 控件升级（hotkey-composer、segmented 等） | `src/manifest.rs` + windui 能力核对 |

> 注：改 `manifest.toml` 后须跑一致性守护测试（`cargo test`），key 必须仍在 capability 快照内。
