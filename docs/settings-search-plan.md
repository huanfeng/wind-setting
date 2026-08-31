# 设置页全局搜索 — 实施规划

> 目标：导航栏上方常驻搜索框，输入汉字或拼音首字母检索全部设置项，回车/点击直达目标位置并闪烁；
> 目标藏在对话框里就自动打开对话框；目标是 per-schema（方案级）这种「哪个方案未定」的多层级项，
> 就跳到最近可达位置并浮一条**不打断操作**的指示信息。

---

## 0. 已定决策

| 维度 | 决策 |
|---|---|
| 结果呈现 | 搜索框下方**下拉浮层**，↑↓ 选择、Enter 直达、Esc 收起 |
| 索引覆盖 | manifest 全局项 + 对话框内项 + 手写页卡片 + per-schema 方案级项 |
| 拼音数据 | 按索引语料生成的**精简首字母表** + 守护测试（语料出现新字即测试失败） |
| 对话框内项 | **自动打开对话框**并闪烁其中那一行 |
| 方案级项 | 跳到方案页 + 顶部指示条说明完整路径，由用户自己选方案 |

---

## 1. 验收标准

1. 输入「码表」→ 命中「主码表方案」「反查码表」「码表配置」等，路径面包屑正确。
2. 输入 `mbfa` → 同样命中「主码表方案」（首字母连续匹配）。
3. 点击结果 → 切页 → 滚动到该行 → 该行闪烁三次后恢复；行在首屏内时不做无谓滚动。
4. 命中项在「扩展设置」类对话框内 → 切页 → 滚到宿主行 → 对话框自动弹出 → 对话框内该行闪烁。
5. 命中项是方案级（如「码表方案覆盖」）→ 切到方案页 → 顶部浮一条指示条，写明
   `方案 › <选择一个方案> › 方案设置… › 方案自定义 › 码表配置`，指示条不遮挡、不拦截操作，可手动关闭。
6. 搜索框空 → 浮层收起，界面与今天完全一致（零回归）。
7. `WIND_RPC_MOCK=1 cargo run -- --screenshot` 离线截图可复现上述 1/3/5 三种状态。

---

## 2. 可用底座（已存在，不要重造）

| 能力 | 位置 | 说明 |
|---|---|---|
| 滚动到节点 | `../wind-ui-rust/src/core.rs:2263` `Tree::scroll_into_view` | 逐级向外滚动，焦点系统已在用 |
| 从 widget 拿到 Tree | `../wind-ui-rust/src/core.rs:1550` `EventCtx::tree_mut` | **公开**，故锚点可在 `on_update` 里自行滚动 |
| 每帧回调 | `Widget::on_update` + `Element::reactive()` | 本仓 `src/pages/dict/reactive.rs:65` 的 `reactive_effect` 即此模式 |
| 输入框回车 / 上下键 | `../wind-ui-rust/src/ui/mod.rs:1510` `on_submit`、`:1566` `on_nav_key` | 正是为「查询框 + 结果列表」设计的 |
| 窗口级快捷键 | `../wind-ui-rust/src/app/mod.rs:1100` `App::on_shortcut` | 焦点控件不要该键时才轮到，可做 Ctrl+F |
| 焦点回搜索框 | `../wind-ui-rust/src/event.rs:163` `ShortcutCtx::focus_main_input` + `Element::autofocus`（`ui/mod.rs:1016`） | Ctrl+F 语义直接复用 |
| 命中穿透 | `Widget::hit_opaque()` 返回 `false` | 闪烁覆盖层不能吃掉点击，靠它 |
| 层级真相源 | `src/manifest.rs:250` `sections_of`、`MItem` 的 `dialog_group`/`dialog_extra_for`/`dialog_title`/`opens_dialog`/`subsection` | 面包屑与「在哪个对话框里」全部可推导 |
| Tab 索引 | `src/cli.rs:7` `PAGES` | 结果项的 tab 下标一律走 `page_to_index`，不写字面量 |
| 方案级层级 | `src/dialogs/schema_manager.rs:119` `SettingsSection`、`:144` `SETTINGS_TABS` | 方案设置对话框的三个 TAB 与各分节 |

---

## 3. 三条必须先知道的约束

### 3.1 `on_update` **不过滤可见性**

`Tree::dispatch_reactive_updates`（`core.rs:652`）对**全部**注册的响应式节点每帧广播，
不看祖先是否 `visible`。设置窗把 8 个页面全部建好、用 `visible_when` 叠在一个 stack 里
（`src/shell.rs:110` 附近），所以**未选中页里的锚点也每帧收到 `on_update`**。

⚠ 后果：若锚点不加判断就调 `scroll_into_view(ctx.id())`，它会拿一个未参与布局的
过期 bounds 去算，把某个滚动容器滚到莫名其妙的位置。

⇒ **锚点执行前必须自查祖先链可见**：从 `ctx.tree_mut().get(id)` 起沿 `Node::parent`
（pub 字段）上溯，任一 `Node::effective_visible()`（`core.rs:273`，pub）为 false 就
本帧不动，等下一帧。这条同时天然实现了「切页 → 下一帧目标页可见 → 才滚动」的时序，
不需要额外的帧计数。

### 3.2 按键只发给焦点节点，**不冒泡**

`Tree::dispatch_key`（`core.rs:2564`）只对焦点节点调一次 `on_event`，未消费也不上溯。
所以结果列表**不能**指望自己收 ↑↓——焦点在搜索框上。唯一正确的做法是搜索框的
`on_nav_key` 改选中项、`on_submit` 执行跳转，这也正是这两个回调存在的理由。

Esc 同理：TextInput 不处理 Escape，未消费的 Escape 会被宿主当作「关窗兜底」。
⇒ Esc 收起浮层必须走 `App::on_shortcut`（它排在 Escape 关窗**之前**），
在浮层开着时吃掉这次 Escape 返回 `true`。**这条不做的话，搜索时按 Esc 会关掉设置窗。**

### 3.3 对话框的 `visible` 信号目前是局部变量

`src/manifest.rs:489`（`build_dialog_group_row`）与 `:2026`（`build_grouped_dialog_row`）
里 `let visible = signal(false);` 建完就只被按钮闭包捕获，外界拿不到。
⇒ 要「自动打开对话框」，必须把它登记进注册表（见 §5.3）。

### 3.4 重绘帧 ≠ 响应式帧

`request_repaint()` 只请求绘制；`on_update` 挂在 `layout_root` 上，只在
`needs_relayout` 时才跑（`app/mod.rs:1846`）。所以**任何"每帧推进一点"的状态机
（多段动画、逐帧计数）都不能建立在 `on_update` 上**。

对本功能只影响闪烁一处，规避办法见 §6.3：把动画写成「时刻 → 值」的纯函数，
状态只有一个起始时刻。这比多段补间还简单，所以不构成阻碍——但**不知道这条**
就会写出「只闪第一下然后卡住」，而且它在测试里看不见（测试不跑渲染循环）。

---

## 4. 架构分层

```
        ┌──────────────────────────────────────────────┐
        │ 索引层 search/index.rs                        │
        │   manifest 派生 + 手写页补充 + per-schema      │
        │   → Vec<SearchEntry>（启动构建一次，常驻）      │
        └───────────────┬──────────────────────────────┘
                        │
        ┌───────────────▼──────────────────────────────┐
        │ 匹配层 search/match.rs + search/pinyin.rs     │
        │   汉字子串 / 拼音首字母连续 / 英文 key 子串    │
        │   → 打分排序，取前 N 条                        │
        └───────────────┬──────────────────────────────┘
                        │  用户点击 / Enter
        ┌───────────────▼──────────────────────────────┐
        │ 导航层 search/jump.rs                          │
        │   JumpRequest{seq,target} 写进 Signal          │
        │   ① 切 tab ② 开对话框 ③ 锚点自认领 → 滚动+闪烁 │
        └───────────────┬──────────────────────────────┘
                        │  无法直达时
        ┌───────────────▼──────────────────────────────┐
        │ 反馈层 search/hint.rs                          │
        │   内容区顶部指示条（同 conflict_bar 的位置语义）│
        └──────────────────────────────────────────────┘
```

新增目录 `src/search/`：`mod.rs` / `index.rs` / `pinyin.rs` / `matcher.rs` /
`jump.rs` / `anchor.rs` / `ui.rs` / `hint.rs`。

---

## 5. 数据结构

### 5.1 索引条目

```rust
pub struct SearchEntry {
    /// 稳定 id：manifest 项即其 key；手写项用 `page:slug`。用于去重与测试断言。
    pub id: String,
    /// 主标题（结果列表首行）——manifest 的 label / 手写卡片名。
    pub title: String,
    /// 附加检索词：hint 摘要、别名（如「五笔」之于码表）、配置 key 本身。
    pub keywords: Vec<String>,
    /// 面包屑，结果列表第二行；最后一段即 title 的容器。
    pub path: Vec<String>,          // ["方案", "主方案设置"]
    pub target: JumpTarget,
}

pub enum JumpTarget {
    /// 页面上直接成行的项：切页 + 滚动 + 闪烁。
    Row { tab: usize, anchor: String },
    /// 对话框内的项：切页 + 滚到宿主行 + 打开对话框 + 闪烁框内那行。
    RowInDialog { tab: usize, host_anchor: String, dialog: String, anchor: String },
    /// 手写页里没有细分锚点的整块（如「统计」页）：只切页 + 闪烁整张卡片。
    Card { tab: usize, anchor: String },
    /// per-schema：跳到方案页，附指示条文案。
    SchemaLevel { hint_path: Vec<String> },
}
```

`anchor` 是字符串键，与页面构建时挂上的锚点一一对应（§6）。

### 5.2 跳转请求

```rust
pub struct JumpRequest {
    pub seq: u64,           // 每次搜索递增，锚点据此判断是不是"这一次"的目标
    pub target: JumpTarget,
    pub started_frame: u64, // 超时作废用
}
// AppState 新增：pub jump: Signal<Option<JumpRequest>>
```

生命周期：**认领即清**。锚点执行完滚动后把 `jump` 置 `None`。
另设 `JUMP_TIMEOUT_FRAMES`（建议 120，约 2 秒）：超时未被认领即丢弃。
不设超时的话，用户中途关掉对话框会留下一个悬挂请求，下次打开该对话框时会莫名闪一下。

### 5.3 注册表扩展

`FieldRegistry`（`src/state.rs:69`）新增一项：

```rust
/// 清单项 key → 该项所属对话框的可见信号。搜索跳转据此程序化打开对话框。
pub dialog_opens: RefCell<HashMap<String, Signal<bool>>>,
```

`reset()` 里一并清空（`src/state.rs:110` 那段注释解释了为什么必须清——
旧 Signal 句柄的槽位会被下一批信号复用，读它拿到的是别人的值）。
`build_dialog_group_row` / `build_grouped_dialog_row` 建完 `visible` 后登记一次。

---

## 6. 锚点：怎么把「一行」变成可跳转的目标

### 6.1 锚点哨兵 widget

```rust
struct Anchor {
    key: String,
    jump: Signal<Option<JumpRequest>>,
    /// 闪烁起始时刻（0 = 不在闪）。Cell 是因为 `paint` 只有 `&self`（见 §6.3）。
    flash_start: Cell<u64>,
}

impl Widget for Anchor {
    fn on_update(&mut self, ctx: &mut EventCtx) {
        // ① jump 为 None → 立即 return。常态下每帧只读一个信号，不做父链上溯。
        // ② 祖先链不可见 → 本帧什么都不做（见 §3.1）
        // ③ jump 目标是我 → tree_mut().scroll_into_view(ctx.id())
        //    + mark_layout_dirty()，flash_start = clock_ms()，随后清空 jump
    }
    fn paint(&self, bounds, ..., canvas, _style) {
        // 按 clock_ms() - flash_start 算相位，fill_round_rect + stroke_round_rect
    }
    fn hit_opaque(&self) -> bool { false }   // 不吃点击（见 §6.2）
}
```

### 6.2 挂载方式

`section_rows`（`src/manifest.rs:306`）给每一行包一层 stack：

```rust
Element::stack()
    .child(<原行>)
    .child(anchor(key).align(Align::Stretch))   // 覆盖层
```

⚠ **两个不能想当然的点**（都在 `core.rs` 里验证过）：

1. **撑满靠 `Align::Stretch`，不是 `.fill()`**。`arrange_frame`（`core.rs:1098`）判的是
   `align == Align::Stretch` 才用父的可用尺寸，否则一律取子的 measured 尺寸。
   而 `.fill()` 给的是 `Dimension::Match`——在 `measure_frame` 的 unbounded 路径下
   leaf 测得 0，arrange 时又不满足 Stretch 判据，结果是**一个 0×0 的锚点**：
   不报错、不闪、看不出为什么。
2. **闪烁必须自绘，绝不能设 `style.bg`**。`hit_node`（`core.rs:2130`）的「吞命中」
   判据里 `n.style.bg.is_some()` 与 `hit_opaque()` 是**并列**的——只要给覆盖层设了背景色，
   哪怕 `hit_opaque` 返回 false，它照样吞掉点击，那一行就此点不动。
   所以高亮只能在 `paint` 里用 `canvas.fill_round_rect` 画（Canvas 原语见 `render/mod.rs:184`）。

对话框内的行同理（`field` 在对话框里生成的那批）。
手写页按卡片粒度挂（`widgets::section` 的返回值外面包一层）。

**为什么用覆盖层而不是改行的背景色**：行的背景由 `widgets::setting_row` 与
主题角色决定，闪烁改它要在恢复时把原色还回去，而原色随明暗主题变——覆盖层
自绘一层带 alpha 的 accent，恢复就是不画，两套主题都不必特判。

### 6.3 闪烁参数与**帧驱动**

三次呼吸：alpha 峰值 0.22，每次 160 ms 起落，总时长约 960 ms。
`Role::Accent` 的 alpha 底 + 1px accent 描边，圆角对齐行的 corner。

⚠ **不能用 `Anim<f32>` 的多段 `retarget` 来串这三次**。理由是本仓已经踩过的那条
「渲染帧是事件驱动、非 60fps」：`Anim::animate()`（`anim.rs:317`）只调
`request_repaint()`，而 `layout_root`→`dispatch_reactive_updates` 只在
`needs_relayout` 时才跑（`app/mod.rs:1846`）。⇒ **动画期间 `on_update` 根本不会每帧被调用**，
靠它推进下一段补间会卡在第一段。

⇒ 正确做法是让 `paint` **无状态地算相位**：

```rust
let t = anim::clock_ms().saturating_sub(self.flash_start.get());
if t >= FLASH_TOTAL_MS { return; }               // 结束：什么都不画，零开销
let alpha = flash_alpha(t);                       // 纯函数，三次正弦半波
anim::request_repaint();                          // 自驱下一帧（同 toast.rs:318）
```

`flash_alpha` 是纯函数 → 可直接单测波形（起点 0、峰值 0.22、终点 0、三次过零）。

---

## 7. 拼音首字母

### 7.1 资产

`src/assets/pinyin_initials.txt`，一字一行：

```
主 z
码 m
表 b
重 c z          # 多音字：全部首字母都收，任一命中即算
```

编译期 `include_str!` 读入，构建成 `HashMap<char, SmallVec<[u8;2]>>`。
只收录索引语料里实际出现的字，预计 600–900 字，体积 < 10 KB。

### 7.2 生成：**零人工录入**，从 core 的拼音表派生

真相源已经存在：`../WindInput/build/data/pinyin_map.txt`（44 439 行）——

```
# 格式: U+XXXX: py1,py2  # 字   (多音字按常用频率排序)
U+3007: líng,yuán,xīng  # 〇
```

它由 core 的构建产出（源自 pinyin-data，经 kMandarin/kTGHZ2013/kXHC1983 白名单裁过古音），
是这套输入法自己在用的读音。⇒ 生成脚本 `scripts/gen-pinyin-initials.ps1`：

1. 构建索引语料 → 取出全部汉字（`cargo test --lib -- --nocapture dump_index_chars` 之类的转储入口）；
2. 查 `pinyin_map.txt`，**只取前两个读音**的首字母去重（表已按常用频率排序，
   第三个往后基本是生僻音，收进来只会制造误命中）；
3. 声母首字母按拼音首字符取，`zh/ch/sh` 取 `z/c/s`（首字母检索的通行约定）;
4. 写出 `src/assets/pinyin_initials.txt`。

⇒ **没有一个字需要人工核对读音**，这是整套方案里原本最大的一块隐性成本。
`pinyin_map.txt` 是构建产物、不随本仓发布，故只在生成时读一次，**运行时零依赖**。

### 7.3 守门

守门测试 `search::pinyin::tests::table_covers_every_title_char`（本仓是 binary crate、
没有 `tests/` 集成测试目录，测试一律内联）：构建完整索引 → 取出全部**标题**里的汉字 →
与表求差集 → **非空即失败**，并打印缺失字列表与「跑一次生成脚本」的指引。

这条与本仓既有的 `capabilities.snapshot.json` 守门是同一套路子：
表是生成物、手工合入，靠测试保证它不与语料漂移。漏一个字的症状是「搜某项拼音搜不到」，
没有堆栈、只能靠测试看见。

### 7.3 匹配算法

对每个 entry，把 `title` 逐字转成首字母串（多音字取笛卡尔积上限 2 支，避免爆炸），
查询串小写后：

| 方式 | 例 | 得分 |
|---|---|---|
| 标题完全相等 | 「主码表方案」 | 1000 |
| 标题前缀 | 「主码」 | 800 |
| 标题子串 | 「码表」 | 600 |
| 首字母前缀 | `mbfa` → 主码表方案 | 500 |
| 首字母子串 | `bfa` | 350 |
| 关键词/hint 命中 | 「五笔」 | 200 |
| 配置 key 子串 | `primary_codetable` | 150 |

同分时按「页面顺序 → manifest 顺序」稳定排序。取前 12 条。
255 条 entry × 短串匹配是微秒级，不必建倒排，也不必防抖。

---

## 8. 跳转时序

以「词频调整幅度」（藏在输入页某个「扩展设置」对话框里）为例：

```
用户点结果
  │
  ├─ jump.set(Some(JumpRequest{seq:7, RowInDialog{tab:1, host, dlg, key}}))
  ├─ tab.set(1)                       ← 立即
  │
  ▼ 下一帧
  宿主行锚点 on_update：祖先可见 ✔ 且 seq 未认领
  ├─ scroll_into_view(宿主行)         ← 让对话框弹出前后视觉连续
  ├─ dialog_opens[host].set(true)     ← 打开对话框
  └─ 保留 jump（还没到最终目标）
  │
  ▼ 再下一帧（对话框已可见）
  框内目标行锚点 on_update：祖先可见 ✔
  ├─ scroll_into_view(目标行)
  ├─ flash.retarget(...)              ← 开始闪烁
  └─ jump.set(None)                   ← 认领完成
```

`Row` 目标少一跳；`Card` 只闪不滚（卡片通常在首屏）；`SchemaLevel` 不进这条路，
直接切到方案页并写 `hint`（§9）。

**并发保护**：`seq` 递增即作废前一次；用户在动画期间再搜一次不会两处同时闪。

---

## 9. 指示条（方案级 / 兜底）

复用 `conflict_bar`（`src/pages/mod.rs:89`）的位置语义——**内容区滚动区之外**，
切页常驻、不随页面滚动、无内容时 `visible_when` 隐藏且不占位。

差别是三点：

1. 语义色用 `Role::Info` 而非 `Role::Warning`（这不是警告，是导航提示）。
2. 右侧带「知道了」关闭按钮；另设 12 秒自动淡出。
3. 文案是路径而非句子：

   ```
   ⓘ 「码表方案覆盖」在：方案 › <选择一个方案> › 方案设置… › 方案自定义 › 码表配置   [知道了]
   ```

`<选择一个方案>` 用 `Role::TextDisabled` 弱化，明确「这一段要你自己选」。

⚠ 不做模态、不抢焦点、不拦点击——用户读着提示的同时就能去点方案列表，
这正是需求里「不影响操作」的落点。

---

## 10. UI 规格

### 10.1 搜索框

位置：`src/shell.rs:91` 那个 `sidebar` 的 col 内、`build_nav` **之前**。

```
宽度 184 - padding_xy(8) * 2 = 168
高度 30，corner 6
Role::SurfaceAlt 底 + Role::Divider 1px 边，聚焦时描边转 Accent
左内嵌 🔍 图标（走 icons.rs 现有 SVG 管线，尺寸对齐 ICON_PX）
placeholder「搜索设置」
非空时右侧出 ✕ 清除按钮
.autofocus()  → 让 Ctrl+F 的 focus_main_input 有落点
```

### 10.2 结果浮层

**挂在根 stack**（与各对话框同级，`src/shell.rs` 末尾那批 child），
不要挂在 sidebar 内——sidebar 是固定宽 col，浮层需要溢出到内容区之上。

定位靠固定 margin：`left = 8`、`top = 标题栏高 + 搜索框高 + 4`（都是常量，不必测量）。
宽度 320，最大高 380，超出走 `Element::scroll`。
`visible_when(|| 浮层开 && 有结果)`。

单条结果两行：标题（13px/500）+ 面包屑（11px/`Role::TextDisabled`）。
选中项底色 `Role::Accent` alpha 0.12。命中片段加粗（若 `rich` 支持分段样式则用之，
否则一期不做高亮片段，只做选中态）。

无结果时浮层内显示一行「未找到匹配项」。

### 10.3 键盘

| 键 | 行为 | 落点 |
|---|---|---|
| Ctrl+F | 焦点回搜索框并全选 | `App::on_shortcut` + `focus_main_input` |
| ↑ / ↓ | 移动选中项 | 搜索框 `on_nav_key`，返回 `true` 消费 |
| Enter | 跳转到选中项 | 搜索框 `on_submit` |
| Esc | 浮层开 → 收起；否则交回默认 | `App::on_shortcut`（**必须**，见 §3.2） |
| Tab | 保持默认焦点导航 | `on_nav_key` 对 Tab 返回 `false` |

---

## 11. 索引来源清单

### 11.1 manifest 自动派生（约 255 条）

遍历 `Manifest::order`，跳过 `hidden`，按以下规则定 `target` 与 `path`：

| MItem 特征 | target | path |
|---|---|---|
| 普通项 | `Row{tab(group), key}` | `[页面名, section]` |
| `dialog_extra_for = H` | `RowInDialog{host: H}` | `[页面名, section, H 的对话框名]` |
| `dialog_group = G` | `RowInDialog{host: opens_dialog==G 的那项}` | 同上（+ `subsection`） |
| `opens_dialog` / `dialog_title` 宿主 | `Row` | 同普通项（宿主自己就是页面上那一行） |
| `gate_key` 合并行的 `dialog_button` | **跳过** | 行的锚点归门控开关 |

`keywords` = `hint` 首句（截 28 字）+ 配置 key 末两段。

#### ⚠ group **不等于** Tab

实现时才发现的，规划原先照 group 直接映射会全错：

| group | section | 实际渲染在 |
|---|---|---|
| `schema` | 主方案设置 | 方案页，直接成行 |
| `schema` | 上屏行为 | 方案页 › 全局方案配置 › **码表方案配置**对话框 |
| `schema` | 混输行为 | 同上 › **混输方案配置** |
| `schema` | 候选行为 / 模糊音 | 同上 › **拼音方案配置**（模糊音由 `with_fuzzy` 并入）|
| `schema` | 英文行为 | 同上 › **英文方案配置** |
| `general` | 默认状态 | **输入页**（`input_page` 把 general 组 extend 进去）|
| `stats` | 统计设置 | 统计页 |
| 其余 | * | 同名页面 |

⇒ `index.rs` 里显式建表 `placement_of`，并由 `sections_have_placement` 守门：
新增 (group, section) 组合而忘了登记，那批项会**静默**从搜索里消失——测试会红。

#### 拼音首字母只对标题生效

实现时的一处收敛：`title` 走拼音，`hint`/`keywords` 只做汉字子串。
拿一句 20 字的说明去比首字母，四五个字母的查询会命中一堆不相干的项。
副产品是拼音表只需覆盖标题的 290 个字（2.2 KB），比预估的 600–900 字还小。

#### 方案级条目降权 30 分

方案级条目**到不了**（「哪个方案」未定，只能给指示）。同样匹配程度下，
能一步到位的那条更该排前面。只降 30 分故**不跨档**：方案级项若是标题前缀命中、
全局项只是子串命中，前者仍排在前面——那时它确实更贴题，由结果列表给它一个
「需先选方案」的标记（P1）。

### 11.2 手写页补充索引（约 40 条，手工维护）

`src/search/index.rs` 里一张静态表，逐条登记。首批：

- 方案页：方案列表、导入方案包、方案设置…、码表配置、导出方案包
- 词库页：词库管理、添加词条、导入词库、短语导入、注释词库
- 统计页：使用统计、清空统计
- 高级页：桌面集成、协议关联、文件关联、导入与导出、备份与还原、导入配置
- 外观页：主题导入、主题编辑器、皮肤连接服务
- 关于页：版本、检查更新、日志目录

⚠ 这张表**编译器不管**，与 `SettingsSection::ALL`（`schema_manager.rs:168`）同类风险。
配一条测试：每个 `PAGES` 页至少有一条索引条目，防止新增页忘了登记。

### 11.3 per-schema（约 10 条）

由 `SettingsSection::ALL` + `SETTINGS_TABS` 派生，全部落 `SchemaLevel`：
进入方式 / 双拼布局 / 辅助码 / 码表配置 / 特殊模式 / 本方案行为 / 扩展词库。

---

## 12. 分期与文件清单

| 期 | 内容 | 落点 | 新增代码 | 工作量 | 可独立验收 |
|---|---|---|---|---|---|
| **P0** | 索引 + 匹配 + 拼音表 + 单测 | `src/search/{mod,index,matcher,pinyin}.rs`、`src/assets/pinyin_initials.txt`、`scripts/gen-pinyin-initials.ps1` | ~750 行 + 生成数据 | 1.0 d | `cargo test`：搜「码表」「mbfa」返回期望 id 序列 |
| **P1** | 搜索框 + 浮层 + 键盘 | `src/search/ui.rs`、`src/shell.rs`、`src/main.rs`(on_shortcut) | ~400 行 | 1.0 d | 截图：浮层形态；手测 Esc 不关窗 |
| **P2** | 锚点 + 滚动 + 闪烁（`Row`/`Card`） | `src/search/{anchor,jump}.rs`、`src/manifest.rs`、`src/widgets.rs` | ~450 行 | 1.5 d | 截图：闪烁中一帧；不可见页锚点不误滚的单测 |
| **P3** | 对话框内项（`RowInDialog`） | `src/state.rs`(registry)、`src/manifest.rs` | ~200 行 | 0.5 d | 手测四步链路 |
| **P4** | 指示条 + per-schema | `src/search/hint.rs`、`src/pages/mod.rs` | ~250 行 | 0.5 d | 截图：方案页指示条 |
| — | 联调 / 截图验收 / 文档 | — | — | 1.0 d | §1 七条验收全过 |

合计约 **2 050 行新增代码**（按本仓注释密度实际会到 2 600–3 000 行）+ 约 800 行生成数据，
**5.5 人日**。P0 与 P1 无依赖可并行；P2 依赖 P1 的 `jump` 信号；P3 依赖 P2 的锚点。

每期一个原子提交。P0/P1 之间无依赖，可并行；P2 依赖 P1 的 `jump` 信号。

---

## 13. 测试计划

**单元（P0，纯逻辑，无 UI）**
- `index_covers_every_visible_manifest_item`：非 hidden 项全部有 entry。
- `index_paths_are_non_empty`：面包屑不为空、末段等于 section 或对话框名。
- `pinyin_coverage`：语料汉字 ⊆ 首字母表（§7.2）。
- `match_ranks_exact_before_substring`：打分序稳定。
- `match_initials_hits_expected`：`mbfa` → `schema.primary_codetable`。
- `every_page_has_at_least_one_entry`：手写补充表不漏页。

**构建（P2/P3）**
- `anchor_wraps_every_section_row`：`section_rows` 每行都带锚点（防止后续改渲染时漏包）。
- `dialog_opens_registered_for_every_dialog_row`：注册表覆盖全部对话框行。
- 沿用本仓惯例：`build_section` 那类「建树不 panic」的遍历测试要把新 widget 也覆盖到
  （windui 链式 API 是运行时 downcast，`.small()` 落错 widget 会在 debug 下当场 panic）。

**截图（P1/P2/P4）**
`WIND_RPC_MOCK=1 cargo run -- --page input --screenshot target/search-*.png`，
按 `docs/` 既有 UI 保真流程核对浮层、闪烁、指示条三态。

---

## 14. 风险与取舍

| 风险 | 处置 |
|---|---|
| 锚点在不可见页误滚（§3.1） | 祖先可见性门控 + 一条针对性测试（构造不可见页锚点，断言 `scroll_y` 不变） |
| Esc 误关窗（§3.2） | `on_shortcut` 吃掉；P1 验收必须手测这一条 |
| 每行多一个覆盖层节点，约 +255 节点 | 锚点是 0 事件、静止零绘制的叶子；若 profile 显示布局变慢，退化为「只给每个 section 挂锚点」（跳到卡片而非行） |
| 手写索引与页面漂移 | §11.2 的每页至少一条测试；新增页时会红 |
| 多音字导致误命中 | 首字母全收、只影响召回不影响精度；排序里首字母匹配本就低于汉字匹配 |
| 拼音全拼（非首字母）搜索 | **一期不做**。全拼要完整拼音表（体积×10）且收益低于首字母。留作后续，索引结构无需改动 |

---

## 15. 明确不做

- 模糊/编辑距离匹配：设置项标题短，错一个字宁可无结果也不要给错项。
- 搜索历史与热门项：本地状态又一处落盘，价值不足。
- 搜索方案**内容**（词库词条、方案文件字段）：那是词库页的职责，不是设置搜索。
- 全拼匹配（见上）。

---

## 16. wind-ui-rust 要不要增强

**结论：一期不需要改 windui 一行。** 全部能力都能用现有公开 API 拼出来——
逐条验证记录见 §2 与 §3。以下四项是「做了会更好、不做也能上线」的候选，按价值排序，
**都不是本功能的前置条件**。

### 16.1 定向焦点转移（唯一有真实功能限制的一处）

今天 Ctrl+F 只能走 `ShortcutCtx::focus_main_input()`，它的语义是
「焦点交还给声明了 `autofocus` 的那个控件」。⇒ 两个副作用：

- 设置窗**启动即聚焦搜索框**（与 Windows 11 设置一致，可接受，但不是零成本的选择）；
- 全窗只有一个 `autofocus` 名额。将来词库窗/对话框里也想要一个查询框时，就分不清了。

增强形态：`Element::focus_key("search")` + `ShortcutCtx::focus_key("search")`，
或退一步 `Tree::set_focused` 走一条按 key 的公开入口。
**触发条件**：第二个查询框出现时，或产品明确不要「启动即聚焦」时。

### 16.2 节点锚点（性能与语义）

现在每个可跳转行要挂一个 0×0 的响应式哨兵，全页面约 255 个，
每个响应式帧都会收到一次 `on_update` 广播（`core.rs:652` 不按可见性过滤）。
虽然加上「`jump` 为 None 立即 return」后常态只是 255 次信号读（微秒级），
但这终究是拿「每帧广播」在模拟「按名字找节点」。

增强形态：`Node` 加 `anchor: Option<Box<str>>`、`Tree::find_anchor(&str) -> Option<NodeId>`、
`EventCtx::scroll_to_anchor(&str)`。收益是 255 个响应式节点归零、跳转逻辑从
「等目标自己认领」变成「直接找到它」，两跳时序（§8）也随之简化成一次调用。
**触发条件**：profile 显示响应式广播进入热点，或锚点数量随功能继续增长。

### 16.3 帧驱动的动画状态机

§3.4 那条鸿沟——`request_repaint` 不带 relayout，故 `on_update` 不逐帧跑——
对本功能可以用「时刻→值纯函数」绕过，但任何**多段/有状态**的动画都会再撞一次。

增强形态：`EventCtx::request_animation_frame()`（下一帧连 `on_update` 一起跑），
或让 `Anim` 在活跃期间自动置 `needs_relayout`。
**触发条件**：下一个需要多段动画的控件出现时。这是本仓迟早要还的一笔债，
但不该由搜索功能来还。

### 16.4 覆盖层语义糖

`Align::Stretch` 才撑满、设了 `style.bg` 就吞命中——这两条（§6.2）都是正确的设计，
只是组合起来对「装饰性覆盖层」这个常见需求不够直白。
增强形态：`Element::overlay()` 语义糖（= Stretch + 不吞命中 + 自绘背景通道）。
纯便利性，价值最低。
