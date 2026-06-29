# 方案页「全局方案配置」配置项清单对照

> 目的：整理「参考 webview 方案设置(engine.schema.ts)」与「本项目 core 已有引擎键(capability 快照)」
> 的全量配置项，供决定 码表/混输/拼音 三个对话框各放哪些项。
>
> **核心事实**：参考的引擎键命名空间是 `engine.codetable.*` / `engine.pinyin.*` / `engine.mixed.*` / `learning.*`，
> **本项目 core 一个都没有**。我们 core 用的是 `input.code_commit.*` / `schema.pinyin.*` 等，且只覆盖参考的一小部分。

---

## A. 本项目 core 已有、可用于「全局方案配置」的键（capability 快照实有）

### 码表上屏（input.code_commit.*，现在方案页「上屏行为」卡）
| key | 标签 | 类型 | 参考近义项 |
|---|---|---|---|
| input.code_commit.auto_commit_at_full | 全码自动上屏 | bool | ≈满码唯一自动上屏 |
| input.code_commit.auto_commit_block_on_pinyin | 有拼音候选时否决全码上屏 | bool | 我们独有 |
| input.code_commit.auto_commit_min_len | 自动上屏最短码长 | int | 我们独有 |
| input.code_commit.top_code_commit | 超长时取前 N 码顶字上屏 | bool | ≈顶码上屏 |
| input.code_commit.clear_on_empty_max | 满码无候选时清空缓冲 | bool | ≈满码空码清空 |

### 拼音（schema.pinyin.*）
| key | 标签 | 类型 | 参考近义项 |
|---|---|---|---|
| schema.pinyin.candidate_order | 候选排序模式 | enum | ≈候选排序（参考归码表） |
| schema.pinyin.show_code_hint | 显示编码提示 | bool | ≈编码反查提示 |
| schema.pinyin.use_smart_compose | 智能组词 | bool | ≈智能组句 |
| schema.pinyin.separator | 拼音分隔符 | enum | 我们独有（现输入页/临时拼音） |
| schema.pinyin.fuzzy.enabled | 模糊音总开关 | bool | 我们独有 |
| schema.pinyin.fuzzy.{an_ang,ch_c,en_eng,f_h,ian_iang,in_ing,n_l,r_l,sh_s,uan_uang,zh_z} | 11 对模糊音 | bool | 我们独有（参考无逐项） |

### 通用输入行为（可归输入页，也可视作引擎全局）
| key | 标签 | 类型 | 现位置 |
|---|---|---|---|
| input.filter_mode | 候选检索范围 | enum | 输入页/字符与标点 |
| input.numpad_behavior | 数字小键盘功能 | enum | 输入页/按键行为 |
| input.enter_behavior | 回车键功能 | enum | 输入页/按键行为 |
| input.space_on_empty_behavior | 空码时空格键功能 | enum | 输入页/按键行为 |
| input.s2t.enabled / variant | 简入繁出 | bool/enum | 输入页/简入繁出 |
| schema.quick_input.* | 快捷输入(3 项) | — | 输入页/快捷输入 |

### 方案管理（不是「全局配置」，是方案列表本身）
schema.active / schema.available / schema.mix_modes / schema.special_modes —— 归「方案管理」对话框。

---

## B. 参考 webview「方案设置」全量（engine.schema.ts）—— 多数键本项目 core **没有**

### 码表引擎（codetable）· 基础
| 参考标签 | 参考 key | 本项目对应 |
|---|---|---|
| 满码唯一自动上屏 | engine.codetable.auto_commit_unique | ≈input.code_commit.auto_commit_at_full |
| 满码空码清空 | engine.codetable.clear_on_empty_max | ≈input.code_commit.clear_on_empty_max |
| 顶码上屏 | engine.codetable.top_code_commit | ≈input.code_commit.top_code_commit |
| 标点顶码上屏 | engine.codetable.punct_commit | **无** |
| 精确匹配 | engine.codetable.single_code_input | **无** |
| 精确匹配时空码补全 | engine.codetable.single_code_complete | **无** |
| Z键重复上屏 | engine.codetable.z_key_repeat | **无** |
| 临时拼音 | engine.codetable.temp_pinyin.enabled | **无**(我们有触发键 input.temp_pinyin.trigger_keys) |
| 显示编码提示 | engine.codetable.show_code_hint | **无**(我们只有 schema.pinyin.show_code_hint) |
| 自动调频 | learning.freq.enabled | **无** |
| 首选保护 | learning.freq.protect_top_n | **无** |
| 单字不调频 | engine.codetable.skip_single_char_freq | **无** |
| 自动造词 | learning.auto_learn.enabled | **无** |
| 晋升用户词库次数 | learning.temp_promote_count | **无** |

### 码表引擎 · 高级
| 候选排序 | engine.codetable.candidate_sort_mode | **无**(我们 candidate_order 属拼音) |
| 候选字符偏好 | engine.codetable.charset_preference | **无** |
| 短码优先 | engine.codetable.short_code_first | **无** |
| 候选去重 | engine.codetable.dedup_candidates | **无** |
| 前缀匹配模式 | engine.codetable.prefix_mode | **无** |
| 权重解释策略 | engine.codetable.weight_mode | **无** |
| 加载模式 | engine.codetable.load_mode | **无** |

### 拼音引擎（pinyin）
| 双拼方案 | engine.pinyin.shuangpin.layout | **无** |
| 编码反查提示 | engine.pinyin.show_code_hint | ✓ schema.pinyin.show_code_hint |
| 智能组句 | engine.pinyin.use_smart_compose | ✓ schema.pinyin.use_smart_compose |
| 自动调频 | learning.freq.enabled | **无** |
| 自动造词 | learning.auto_learn.enabled | **无** |
| 晋升用户词库次数 | learning.temp_promote_count | **无** |
| （模糊音，参考在双拼方案弹窗 FuzzyDialog） | — | ✓ schema.pinyin.fuzzy.*（我们有 12 项） |

### 混输引擎（mixed）
| 拼音最小触发长度 | engine.mixed.min_pinyin_length | **无** |
| 显示来源标记 | engine.mixed.show_source_hint | **无** |
| 简拼匹配 | engine.mixed.enable_abbrev_match | **无** |
| Z键重复上屏 | engine.mixed.z_key_repeat | **无** |
| 歧义码顶码上屏 | engine.mixed.topcode_override_pinyin | **无** |
| （码表/拼音设置复用上面 codetable/pinyin 键） | — | 同上 |

---

## C. 结论与可分配项

**本项目 core 当前可用于三对话框的引擎全局键，仅以下这些：**

- **码表对话框**：input.code_commit.* ×5（上屏行为）
- **拼音对话框**：schema.pinyin.candidate_order / show_code_hint / use_smart_compose / separator + fuzzy.*（模糊音 12 项）
- **混输对话框**：**核心无任何 mixed 键 → 空**

参考的 30+ 引擎选项中，约 25 项（标「无」者）本项目 core 未暴露对应 config key，
**无法在设置端落地，须 core 先增加**。

**待你决定**：上述「可分配项」如何放进 码表/混输/拼音 三个对话框（或是否调整结构）；
混输对话框暂空如何处理（占位 / 暂不建该对话框 / 等 core）。
