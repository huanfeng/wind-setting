# 在线升级功能设计与实施规划

> 目标：在 wind-setting 中实现在线升级，功能对齐旧项目 WindInput-Go，并原生支持 Dev 独立分支。
> 本文只做设计与规划，不含实现代码。

## 0. 已确认的决策

| 项 | 决策 |
|---|---|
| Dev 支持形态 | Dev 是**独立分支**（独立安装、独立数据目录、独立更新源），非同流 prerelease |
| 升级包形态 | `wind-installer` 产出的 `.exe`，wind-setting 只做「检查→下载→校验→拉起→自退」 |
| 更新源组织 | 同域名不同文件：`latest.json` / `latest-dev.json`，按 `mode::get()` 自动选择 |
| 完整性校验 | `sha256` + `size`，不做元数据签名 |
| 发布流程 | 扩展 `scripts/pack.ps1` 生成 sha256 与 `latest*.json`，上传仍手工；暂不建 CI |
| Dev 发布节奏 | 打 tag 才发，版本形态 `0.107.0-dev.N`（标准 semver prerelease） |
| 包体积 | 全量包，下载前在 UI 显示体积由用户决定 |
| 实施顺序 | 先 P0/P1（纯本地、可单测），发布侧并行准备 |
| 本轮产出 | 规划文档，不写代码 |

---

## 1. 现状盘点

### 1.1 旧项目（WindInput-Go）的方案

架构是「Go 端只负责下载，把所有 Windows 难题下沉给 NSIS」：

```
CheckUpdate → FetchOfficialLatest(dl.windinput.com/latest.json)
            → ParseVersion + IsNewerThan
DownloadRelease → %TEMP%/<asset>，200ms 节流进度回调，临时文件 + rename
InstallRelease  → ShellExecuteW(open, installer.exe, "/QUIET")
                → NSIS 杀进程 / rename 被占用的 DLL / 注册 COM / 拉起 wind_input.exe
```

值得继承的设计：
- updater 模块与 UI 框架完全解耦，进度经回调注入
- 首次联网**显式同意**页（输入法产品的隐私合规刚需）
- 事件 emit + 一次性 pending 槽的**双通道**，规避「后台完成早于 UI 注册监听」的竞态
- 会话内缓存检查结果，避免重复开对话框重复请网
- release notes 的 `<!-- user-facing:start/end -->` 标记约定与三级清洗回退
- 下载进度 200ms 节流 + 结束补发一次 100%
- 临时文件写入后 rename 原子落盘

必须修正的缺陷：
- **无任何完整性校验**（无 sha256、无签名），且下载会经第三方 ghproxy 代理 —— 安装包以管理员权限运行并注册系统级 TSF DLL，这是最严重的风险
- **prerelease 之间无法比较**：`IsNewerThan` 中 `1.0.0-alpha.1` vs `1.0.0-alpha.0` 恒为 `false`，dev 流根本升不动
- 官网渠道 `AssetSize == 0`，缓存复用退化为「文件非空即视为完整」，`%TEMP%` 里一个被截断的同名文件会被直接当成完整包安装
- `Config.AutoInstall` 定义了但从未被读取（死配置）
- 并发下载时第二次调用静默阻塞在互斥锁上，前端无从感知
- 安装后不重新拉起设置界面，用户体感是「点了安装窗口就消失了」

### 1.2 新项目（wind-setting）的可用基座

已经具备的：

| 能力 | 位置 | 说明 |
|---|---|---|
| HTTP client | `ureq 2`（已在依赖中） | `theme_import.rs:118 download_https_text()` 已有 HTTPS 强制 + 10s 超时 + 3 次重定向的成熟范例 |
| Dev 变体检测 | `src/mode.rs` | `AppMode::{Release,Dev}`，五级检测优先级；`pipe_suffix()` → `_dev`；`version_suffix()` → `" (Dev)"` |
| 自身配置存储 | `src/setting_config.rs` | `%APPDATA%\WindInput{Dev}\wind_setting.toml`，tmp+rename 原子写，已按 mode 分目录 |
| 版本号注入 | `build.rs` | `WIND_SETTING_VERSION`，优先级 `WIND_APP_VERSION` 环境变量 > `git describe` > `CARGO_PKG_VERSION` |
| 跨线程回 UI | `App::channel::<T>()` | `main.rs:117` 有范例；**只能在 App 构建期注册** |
| 后台线程 + 取消 | `theme_editor_server.rs` | `Arc<AtomicBool>` 停止旗；Drop 只发信号不 join |
| 非事件路径提示 | `pages/dict/reactive.rs` | `Feedback` 信号 + `ToastSink` 转 toast |
| 进度条 | `Element::progress(Signal<f32>)` | windui 已提供，项目内**零使用** |
| 安装器 | `wind-installer`（同级仓库） | 自研归档格式、windui 向导、进程终止、TSF 注册、receipt，已支持 `--silent --dir --datadir`；被占用文件有 rename-then-create（`archive/reader.rs:176`） |
| UI 入口占位 | `pages/mod.rs:597` | `Element::button("检查更新").on_click(\|ctx\| ctx.toast("检查更新功能开发中"))` |

缺失的：
- **无任何哈希库**（无 `sha2`/`blake3`/`ring`），需新增依赖
- **core 侧无 `system.shutdown` / `system.restart` RPC**，`system.notifyReload` 是空壳（`Ok(json!({"ok":true}))`）
- 项目内既有长任务（备份、主题导入）都是**同步阻塞 UI**，无一采用后台线程 + 进度的模式；升级包几十 MB，不可照搬

### 1.3 wind-installer 的实测状况（2026-07-19 查证）

**被占用文件的替换**：真实逻辑在 `src/archive/reader.rs:176 create_or_backup`，不在 `src/installer/extract.rs`（后者整个模块是死代码，`mod.rs:2` 标了 `#[allow(dead_code)]`，零调用点 —— 按文件名找逻辑会被误导）。

```rust
// archive/reader.rs:176 —— File::create 失败且 errno ∈ {5, 32} 时改名再建
Err(ref e) if matches!(e.raw_os_error(), Some(5) | Some(32)) => {
    let old_name = format!("{}.old_{:08x}", ...);
    std::fs::rename(path, &old_path)?;   // 底层 MoveFileExW，对已加载 DLL 有效
}
```

对照 NSIS 的四级回退：

| NSIS | wind-installer | |
|---|---|---|
| ① Delete | `File::create`（隐含 truncate） | ✅ |
| ② `ren` → `.old_<rand>` | `fs::rename` → `.old_{:08x}` | ✅ 等价 |
| ③ Sleep 500 重试 | 无 | ❌ rename 一失败即致命中止 |
| ④ 改扩展名 `.bak` | 无（清理逻辑认 `.bak` 但从不生产） | ❌ |
| PendingFileRenameOperations | 无 | ❌ **`.old_` 文件永久泄漏** |

`terminate_app_processes()` 只杀 `app.process_names = ["wind_setting","wind_portable","wind_input"]`，**对 TSF 宿主进程（explorer/浏览器/Office）无覆盖且设计上不可能覆盖**。因此 `wind_tsf.dll` 走 rename 分支不是边缘情况，**是主路径** —— 每次升级必然泄漏一个 `wind_tsf.dll.old_xxxxxxxx`。卸载器虽会扫 `.old_` 删，但那时 DLL 仍被加载，删不掉，同样被 `let _ =` 吞掉。

**无回滚**：`run_plan`（`step.rs:69`）纯线性执行，致命失败直接返回，不撤销已完成步骤。且 `PersistReceipt` 是计划最后一步 —— 若在它之前致命失败（如 `ExtractFiles` 因 rename 失败），副作用已发生但回执从未落盘，系统留下半装状态且无卸载依据。

**Receipt 不记录安装形态**：`Receipt { entries: Vec<ReceiptEntry> }` 无 `mode`、无 `install_dir`、无 version/timestamp；存在 `HKLM\Software\{app_id}` 的 `Receipt` 值里（TOML 文本，非文件）。且 **Portable 模式根本不写 receipt**（`plan.rs:19` 早退路径不含 `PersistReceipt`）。

**Dev 变体：当前不存在。** 仓库只有一个 `app.toml`，`AppManifest` 无 `variant`/`channel` 字段，`meta::bootstrap()` 无环境变量/feature/编译期分支。架构为变体预留了机制（外部 app.toml + `{main_exe}`/`{setting_exe}` 占位符 + `pack.ps1 -Config` 参数），但变体本身从未创建，`dist/` 只有 release 一套产物。

---

## 2. 总体架构

分三层，边界与旧项目一致（updater 核心不依赖 UI 框架，便于单测）：

```
src/pages/mod.rs (关于页入口)
src/dialogs/update_dialog.rs      ← UI 状态机 + 进度条，挂 shell.rs 根 stack
        ↕ Signal 读写（UI 线程）
src/state.rs: AppState.update_tx  ← Sender<UpdateMsg>，main() 构建期注册
        ↕ 跨线程消息
src/update/mod.rs                 ← 编排：check / download / install
  ├─ source.rs    拉取 latest.json / latest-dev.json，归一化为 ReleaseInfo
  ├─ version.rs   版本解析与比较（含 prerelease）
  ├─ download.rs  分片并发/单连接下载 + sha256 + 进度节流 + 取消旗
  ├─ install.rs   定位安装目录、拉起 wind-installer、自身退出
  └─ config.rs    UpdateConfig（并入 SettingConfig）
        ↕ 进程启动
wind-installer.exe --silent --dir <当前安装目录>
```

**关键约束**：`windui::Signal` 是线程局部 arena 句柄，**不是 `Send`**。下载线程绝不能直接写 Signal。唯一干净的路径是 `main()` 里预注册 `app.channel::<UpdateMsg>()`，把 `Sender` 存进 `AppState`，下载线程 `tx.send(...)`，UI 线程回调里写 Signal。

**另一约束**：`rpc::call_core` 硬编码 5s 超时，升级链路全程不得经过它。

### 2.1 跨线程消息定义（草案）

```
enum UpdateMsg {
    CheckDone(Result<CheckResult, String>),
    Progress { downloaded: u64, total: u64 },
    DownloadDone(PathBuf),
    DownloadFailed(String),
}
```

`UpdateMsg` 必须 `Send`，因此内部只能放 `String`/`PathBuf`/数值等，不能夹带 Signal。

---

## 3. 更新源协议

### 3.1 端点

| 变体 | URL |
|---|---|
| Release | `https://dl.windinput.com/latest.json` |
| Dev | `https://dl.windinput.com/latest-dev.json` |

由 `mode::get()` 决定，用户不可切换 —— Dev 版永远只看 dev 源，Release 版永远只看 stable 源。二者是并行安装的独立产品，不存在互相升级/降级。

### 3.2 元数据结构（新增 sha256 与 size）

```json
{
  "version": "0.107.0",
  "tag": "v0.107.0",
  "channel": "stable",
  "exeUrl": "https://dl.windinput.com/WindInput-0.107.0-Setup.exe",
  "sha256": "a3f5...（64 位小写 hex）",
  "size": 48234496,
  "releaseNotesUrl": "https://dl.windinput.com/WindInput-0.107.0-Release.md",
  "publishedAt": "2026-07-19T10:00:00Z",
  "minUpgradableFrom": "0.100.0"
}
```

Dev 变体的 `latest-dev.json` 结构相同，差异在三个字段：

```json
{
  "version": "0.107.0-dev.3",
  "tag": "v0.107.0-dev.3",
  "channel": "dev",
  "exeUrl": "https://dl.windinput.com/WindInputDev-Setup-0.107.0-dev.3.exe",
  ...
}
```

相对旧协议的增补：
- `sha256` / `size`：**必填**。缺失时客户端应拒绝升级并提示「更新元数据不完整」，而不是降级为不校验 —— 否则等于把旧项目的漏洞原样搬过来。
- `channel`：冗余标识，客户端校验它与自身变体匹配，防止 CDN 缓存串档（stable 用户拿到 dev 包）。不匹配则拒绝。
- `minUpgradableFrom`（可选）：低于此版本需引导用户全新安装，用于将来出现不兼容的数据格式迁移时兜底。

### 3.3 校验流程

1. 下载前：检查 `channel` 与自身变体一致、`sha256` 与 `size` 均非空
2. 下载中：累计字节数，超出 `size` 立即中止（防止无限流攻击撑爆磁盘）；分片路径同规格，每片超出自身区间长度即中止
3. 下载后：字节数必须**恰好等于** `size`，且 sha256 必须匹配 —— 单连接路径边下边算，分片路径因字节乱序到达无法流式，改为落盘后整读一遍
4. 缓存复用：命中同名文件时，必须重算 sha256 才可复用 —— 不能像旧项目那样只比大小

任一校验失败：删除文件，报错，**不得**回退到「凑合安装」。

---

## 4. 版本比较

### 4.1 输入形态

`env!("WIND_SETTING_VERSION")` 可能是：
- `v0.106.0`（tag 上）
- `v0.106.0+a1b2c3d`（tag 后有提交）
- `v0.106.0+a1b2c3d.dirty`（脏工作区）
- `a1b2c3d`（无 tag，纯 hash）

比较前需剥离 `v` 前缀与 `+...` build metadata。纯 hash 形态无法解析。

### 4.2 策略

引入 `semver` crate，用其 `Version` 的 `PartialOrd` —— 它正确实现了 semver 规范的 prerelease 比较（`1.0.0-alpha.1 > 1.0.0-alpha.0`，且 `1.0.0 > 1.0.0-alpha`），直接消灭旧项目那个实质 bug。

**解析失败时视为「无更新」而非降级为 0.0.0**。旧项目的 `ParseVersion("dev")` 返回零值，导致开发态每次启动都提示升级；新版应对无法解析的本地版本直接跳过检查，并在 UI 上说明「当前为开发构建，已跳过更新检查」。

---

## 5. Dev 分支的完整支持

Dev 不只是「换个 URL」，是一条平行产品线，需要逐项对齐：

| 维度 | Release | Dev | 现状 |
|---|---|---|---|
| 更新源 | `latest.json` | `latest-dev.json` | 新增 |
| 配置存储 | `%APPDATA%\WindInput\` | `%APPDATA%\WindInputDev\` | ✅ 已有 |
| 命名管道 | `wind_input_ctrl` | `wind_input_ctrl_dev` | ✅ 已有 |
| exe 名 | `wind_setting.exe` | `wind_setting_dev.exe` | ✅ 已有 |
| 安装目录 | `%ProgramFiles%\WindInput` | `%ProgramFiles%\WindInputDev` | ✅ 已有 |
| ARP 卸载项 | 由 `display_name` 派生 | 「清风输入法 (开发版)」 | ✅ 已有 |
| TSF CLSID / profile GUID | `99C2EE30` / `99C2EE31` | `99C2DEB0` / `99C2DEB1` | ✅ 已有 |
| 打包配置 | `WindInput/config/app.toml` | 由 `dev.ps1` 动态生成 | ✅ 已有 |
| URL 协议（安装器注册） | `windinput` | `windinputdev` | ✅ 已有 |
| URL 协议（运行期自愈） | `windinput` | `windinput`（**错**） | ⚠️ **见 5.2** |

**全部身份隔离早已就位**，升级功能可直接复用，无需新建任何打包配置。

### 5.1 Dev 变体的打包配置（早已实现）

打包配置的**真相源在 WindInput 仓库**，不在 wind-installer —— 后者是通用安装器生成器，不该持有任何具体应用的身份。

```
WindInput/config/app.toml          单一真相源（release 身份 + 全部能力段）
WindInput/scripts/dev.ps1          New-InstallerConfig() 按变体动态生成
  ├─ 命令 8   → release 包
  └─ 命令 d8  → dev 包
```

`New-InstallerConfig`（`dev.ps1:840`）读 `config/app.toml`，只替换 `[app]`/`[ime]`/`[package]` 三段为变体与机器相关值，其余能力段（`[[font]]`/`[autostart]`/`[[shortcut]]`/`[startup]`/`[datadir]`/`[strings]`/`[ui]`）原样继承。生成的临时配置落在 `dist/`，带「请勿手工编辑」标头。

Dev 变体的既有取值：

| 字段 | 值 |
|---|---|
| `id` | `WindInputDev` |
| `display_name` | 清风输入法 (开发版) |
| `main_exe` / `setting_exe` | `wind_input_dev.exe` / `wind_setting_dev.exe` |
| `url_protocol` | `windinputdev` |
| `clsid` / `profile_guid` | `{99C2DEB0-…}` / `{99C2DEB1-…}` |
| `output_name` | `WindInputDev-Setup` |

**GUID 不可自行生成。** `dev.ps1:839` 注明它们必须与 `wind_tsf/src/Globals.cpp` 里的常量一致（已核对：dev=`99C2DEB0`/`99C2DEB1`，release=`99C2EE30`/`99C2EE31`）。TSF CLSID 是编译进 DLL 的类工厂标识，注册表里写一个对不上的 GUID，COM 会找不到实现 —— 装上去就是输入法不可用。

> **本文档此处曾经写错**：早期版本断言「Dev 变体尚不存在，需在 wind-installer 新建 app.dev.toml 并生成一对新 GUID」。该结论源于只调查了 wind-installer 仓库、没查 `WindInput/scripts/dev.ps1`。按那个方案做会产出一个 GUID 与 DLL 不匹配的安装包。误建的 `wind-installer/app.dev.toml` 已删除。

### 5.2 遗留：Dev 与 Release 抢 URL 协议注册

`src/protocol.rs` 把 `windinput` 硬编码在两处：

- `parse_protocol_url` —— 只认 `windinput://` 前缀
- `self_heal_registration` —— 启动时写 `HKCU\Software\Classes\windinput\shell\open\command`，指向**自身 exe 路径**

后果：Dev 版每次启动都会把 `windinput://` 抢注到 `wind_setting_dev.exe`，Release 版启动时再抢回去。谁最后启动谁赢，协议导入会打到非预期的变体上。

`app.dev.toml` 里已填 `url_protocol = "windinputdev"`，但那只管住"安装器写的那一份"，管不住"Dev 版运行期自愈注册的那一份"。

**这不是升级功能引入的问题**，而是 Dev 变体一旦真正并存就会显形的既有缺陷。彻底修法是让 `protocol.rs` 按 `mode::get()` 派生协议名（Release → `windinput`，Dev → `windinputdev`），两处硬编码一起改。工作量约半小时，建议在 Dev 包首次分发前做掉。

**UI 呈现**：Dev 变体下的更新对话框应有明确视觉标识（如标题旁 `Dev` 徽章 + 一行「你正在使用开发版，更新可能不稳定」提示），避免用户误以为在装正式版。

---

## 6. 安装环节

### 6.1 拉起 installer

`wind-installer` 已支持：
```
wind-installer.exe --silent --dir <安装目录> [--datadir <数据目录>]
```
且内部会自行 `request_elevation()` 提权、做单实例检查。因此 wind-setting 侧只需 `Command::new(installer).args([...]).spawn()`，**不要** `wait()`。

### 6.2 三个必须处理的细节

**(a) 安装目录必须显式传入。** `--silent` 分支用的是 `InstallConfig::default()`，不传 `--dir` 会装到默认目录。若用户当初装在非默认路径，会造成**双份安装**。安装目录应从 `std::env::current_exe()` 的父目录推导，或读 installer 写的 receipt。

**(b) `InstallMode` 在静默分支被硬编码为 `Standard`。** 见 `wind-installer/src/main.rs` 的 `run_install`：
```rust
let result = installer::perform_install(&config, installer::InstallMode::Standard);
```
若用户原本是 Portable 安装，静默升级会把它变成 Standard（写注册表、注册 COM/TSF、建快捷方式）。

**已定：在 wind-setting 侧规避**，不改 installer 的静默分支行为 —— installer 的静默入口有多个调用方，改默认行为影响面大；而安装形态这个信息 setting 侧推导安装目录时顺带就能拿到，判断成本近乎为零。

检测方式**取两个信号的或**：

```
is_portable = install_dir.join("portable_mode").exists()
           || !install_dir.join("uninstall.exe").exists()
```

- `portable_mode` 是 app.toml 声明的 `portable_marker`，Portable 模式由 `WritePortableMarker` 写入（内容 `portable=1\n`）
- Portable 模式**不释放** `uninstall.exe`（`is_uninstaller_entry` 过滤，`steps.rs:147`）

之所以要两个信号取或：`WritePortableMarker::fatal()` 是默认的 `false`，标记写失败只警告，会产出**既无标记又无卸载器的「幽灵便携安装」**。而 Receipt 帮不上忙 —— 它既不含 `mode` 也不含 `install_dir`，且 Portable 压根不写 receipt。

判定为 Portable（或任何非标准形态）时：**不传 `--silent`**，拉起完整安装向导让用户自行确认路径与模式。Standard 才走静默升级。

**(c) 自身退出时机。** installer 会终止 `wind_setting.exe`。为避免「installer 在杀我 vs 我在正常退出」的竞态，wind-setting 应在 `spawn()` 成功后立即 `ctx.request_close()` 主动退出，把窗口让出来。

### 6.3 升级后回到设置界面

旧项目的痛点：NSIS 装完只拉起 `wind_input.exe`，不拉 `wind_setting.exe`，用户体感是窗口凭空消失。

新版应改善。两个方案：
- **方案 A（推荐）**：在 wind-installer 的安装完成步骤中，判断若由升级触发（新增 `--from-update` 标志），则安装完成后拉起 `wind_setting.exe`
- **方案 B**：wind-setting 退出前不做任何事，仅在退出前弹一个短 toast「正在安装更新，完成后请重新打开设置」

方案 A 体验更好但需改 wind-installer；方案 B 零成本。建议先做 B，A 作为后续优化。

### 6.4 IME 进程终止

core 侧**没有** `system.shutdown` RPC，`system.notifyReload` 是空壳。但这不构成阻塞 —— `wind-installer/src/installer/process.rs` 的 `terminate_app_processes()` 已有成熟实现（Toolhelp32 枚举 + `TerminateProcess` + PowerShell `Stop-Process` 兜底 + 4 轮重试）。进程终止的职责完全归 installer，wind-setting 不碰。

TSF DLL（`wind_tsf.dll`）被**每个有输入焦点的进程**加载，不可能全部卸载。installer 已有 rename-then-create 核心机制（§1.3），足以让升级**跑通**；但缺 PendingFileRenameOperations，会持续泄漏 `.old_` 文件。这不阻塞升级功能落地，作为 wind-installer 侧的独立改进项处理（§8）。

> **已解决（2026-07-28）**：`.old_` 泄漏与「需要重启」的用户提示均已在 wind-installer 落地，见 §8 B 项的完成说明。

---

## 7. 配置项

并入 `SettingConfig`，避开 manifest / capabilities 快照 / core 契约三重改动 —— 升级偏好是设置程序自身的行为，不属于输入法配置。

```toml
# %APPDATA%\WindInput{Dev}\wind_setting.toml
[update]
network_consent = false     # 用户已同意联网；未同意时任何自动检查都不发起
auto_check = true           # 启动时自动检查（受 network_consent 门控）
skipped_version = ""        # 用户点「跳过此版本」记下的版本号
last_check_at = 0           # Unix 秒，用于「距上次检查不足 N 小时则跳过」
```

改动面：仅 `src/setting_config.rs` + 其单测。**不新增** core config key，因此不触碰 `capabilities.snapshot.json`、`settings_manifest.toml`、`mockdata/config.json`，也不影响 `capabilities.rs` 的三个守护测试。

刻意不做的：
- **不提供渠道切换 UI** —— Dev/Release 由二进制变体决定，用户切不了
- **不提供 auto_install 配置** —— 旧项目该字段是死代码；静默安装作为对话框内一次性勾选即可，不持久化（自动以管理员权限替换系统输入法组件，不应变成用户可能忘记自己开过的长期开关）

---

## 8. wind-installer 侧的配套改动

升级功能依赖 installer，以下按是否阻塞分档。

### 阻塞项

**无。** Dev 变体的打包配置早已就位（§5.1），`d8` 命令即可产出 dev 安装包。

> 早期版本在此列了「创建 app.dev.toml」并一度标记为已完成，属误判 —— 见 §5.1 末尾的更正说明。

**写配置时确认的一个约束**（对将来改 `config/app.toml` 有效）：`expand_placeholders` 只在 `src/installer/shortcut.rs` 里被调用，`[autostart]` 与 `[startup]` 段的 `exe` 拿到的是原始串。只有 `[[shortcut]] target` 能用 `{main_exe}` / `{setting_exe}` 占位符，其余位置必须写字面量。现有 `config/app.toml` 的写法是对的。

### 非阻塞但应尽快补（每次升级都在恶化）

**B. `.old_` 文件永久泄漏。** ~~`create_or_backup` rename 成功后应立即登记重启删除~~

```
MoveFileExW(old_path, NULL, MOVEFILE_DELAY_UNTIL_REBOOT)
```

这是 NSIS 有而 wind-installer 没有的唯一实质能力。不补的话，`wind_tsf.dll.old_xxxxxxxx` 每次升级新增一个，永久累积在安装目录。改动量约 10 行。

> **已完成（2026-07-28）**，且比原估的「约 10 行」多做了一层——排队本身早已补上，但**排了队这件事没有回传通道**：`schedule_delete_on_reboot` 的调用点深在 IO 层（`archive/reader.rs`、`installer/legacy.rs`），只 `eprintln!` 到无控制台的 GUI 进程里，等于丢弃；而 `RunOutcome::need_reboot` 只在「步骤失败」时置位，带锁升级时解压是**成功**返回的，所以恒为 `false`。用户永远看不到提示。
>
> 现在 `util::reboot` 维护一本进程级账本，`step::run_plan` 收尾汇总成 `need_reboot`，三处消费：
> - 完成页 warning 色提示（**不提供重启按钮**——安装器无从判断用户有没有没保存的工作）
> - **`--quiet` 放弃自动退出**，窗口留给用户亲手关闭（自动升级时用户没盯着屏幕，2 秒自弹自灭等于把提示扔了）
> - `--silent` 以 `3010` 退出——**与 wind-setting 无关**，本仓库用 `ShellExecuteW` 拿不到退出码，见 `update/install.rs` 模块文档
>
> 另：`legacy.rs` 删不掉的旧版**目录**需自底向上逐项排队——`MoveFileEx` 对非空目录无效，只排根目录会在重启时静默失败。

**C. rename 失败的二级回退。** 补 NSIS 的第 ③④ 级（Sleep 500 重试一次、改扩展名 `.bak` 再试），避免瞬时占用导致整个安装致命中止。

### 可延后

**D. Receipt 补 `mode` 与 `install_dir` 字段。** 当前"安装形态"在系统上无权威落盘，只能靠标记文件/卸载器存在性反推（§6.2b）。补上后 setting 侧的检测可以更可靠，但现有的双信号方案已够用。

**E. 清理死代码 `installer/extract.rs`。** 整个模块零调用点却看起来像主路径，是定位逻辑时的陷阱。

**F. 安装失败回滚。** 当前 `run_plan` 纯线性无补偿，致命失败会留半装状态且可能无 receipt。这是既有问题，非升级功能引入，但升级会显著提高它的触发频率（每次升级都是一次完整安装）。

---

## 9. 发布侧工作

维持手工打包，不建 CI。

### 9.1 改哪个脚本

**改 `WindInput/scripts/dev.ps1`，不是 `wind-installer/scripts/pack.ps1`。**

`latest.json` 是 WindInput 的发布元数据（版本号、下载地址、更新通道），通用安装器生成器不该知道它的存在 —— 同一条原则决定了 `app.toml` 属于 WindInput 而非 wind-installer。

落点是 `Do-Installer`（`dev.ps1:927`）在 `pack.ps1` 返回成功之后：

1. 定位刚产出的安装包（`$DistDir\$outName-$Version.exe`）
2. `Get-FileHash -Algorithm SHA256` 取哈希，`.Length` 取字节数
3. 按 `$profile` 生成 `latest.json`（release）或 `latest-dev.json`（dev），字段见 §3.2
4. 与安装包一并落到 `dist/`，便于手工上传

变体判定直接复用 `Do-Installer` 已有的 `$profile` 参数，无需新增开关。`channel` 字段取 `release → "stable"` / `dev → "dev"`。

`$Version` 在 dev.ps1 里已是现成变量（同一个值也写进了 `[app] version`），因此元数据版本号与安装包内嵌版本号天然一致，不需要 §9.2 提到的额外断言。

### 9.1.1 实现状态 ✅

`New-UpdateManifest`（`dev.ps1`）在 `Do-Installer` 找到安装包后调用，产出：

- `dist/latest.json` 或 `dist/latest-dev.json`（按 `$profile`）
- `dist/<安装包名>.exe.sha256`（标准 `sha256sum` 格式的 sidecar，顺带让 `dev.ps1` 里原先永假的 `Test-Path $sha` 预留生效）

CDN 前缀是脚本变量 `$CdnBase`，定义在 `deploy.local.ps1` 加载之前，可本地覆盖。

**已用 0.104.0 的历史产物实测**，两个变体的输出均通过断言：sha256 与实测一致、size 为 JSON 数字、无 BOM、`publishedAt` 为 ISO8601 UTC、两变体写入不同文件且 channel 分别为 `dev`/`stable`。

**两侧契约有测试守护**：`source.rs` 的 `parses_real_manifest_produced_by_pack_script` 直接嵌入了打包脚本的实际输出原文。打包脚本若改字段名、改大小写、把 size 写成字符串或给文件加 BOM，该测试立刻失败 —— 而不是等用户点「检查更新」才发现升级链路断了。

> 验证时的一个坑：PowerShell 的 `ConvertFrom-Json` 会把形似日期的字符串**自动转成 `DateTime` 对象**，用它做字符串正则断言会得到本地格式而误判。断言 JSON 内容要对原始文本做，不要经 `ConvertFrom-Json` 往返。

### 9.2 版本号形态

Dev 打 tag 才发，形态 `0.107.0-dev.N`。这是标准 semver prerelease，`semver` crate 的 `PartialOrd` 直接给出正确序：

```
0.107.0-dev.1  <  0.107.0-dev.2  <  0.107.0-dev.10  <  0.107.0
```

（注意 `dev.10 > dev.2` —— semver 规范对纯数字标识符按数值比较而非字典序，手写比较器极易在此出错，这也是不自己实现的理由之一。）

三个版本号需保持一致，发布前应校验：
- `app.dev.toml` 的 `[app] version`
- git tag
- 构建时注入 wind-setting 的 `WIND_APP_VERSION`

建议在 `pack.ps1` 里加一道断言：app.toml 的 version 与当前 git tag 不符则报错退出，避免发出版本号错位的包。

### 9.3 上传

手工上传 `dist/` 下的 `.exe` 与 `latest*.json` 到 CDN。

**上传顺序有讲究**：**先传 `.exe`，确认可访问后再传 `latest*.json`**。反过来会出现元数据已指向新版但安装包还没上去，客户端下载 404 或拿到半截文件。元数据是"开关"，最后再拨。

---

## 10. 分阶段实施计划

分两条并行轨道。**客户端轨道**（P0→P5）与**发布侧轨道**（R1→R3）互不阻塞，直到 P1 需要真实元数据、P4 需要真实安装包为止。

| 轨道 | 任务 | 阻塞关系 |
|---|---|---|
| 发布 R1 | ~~建 dev 打包配置~~ —— 早已存在（§5.1） | ✅ 无需做 |
| 发布 R2 | ✅ 已完成 —— `dev.ps1` 的 `New-UpdateManifest` | 无 |
| 发布 R3 | 跑 `d8` 产出 dev 包并上传 CDN，供 P1/P4 联调 | 依赖 R2 |
| 客户端 P0 | 版本比较 + 配置项（纯本地纯函数） | 无 |
| 客户端 P1 | 元数据拉取与解析 | 单测不依赖；**联调依赖 R3** |
| 客户端 P2 | 下载与校验 | 单测用本地 tiny_http 造端点，不依赖 R3 |
| 客户端 P3 | UI 接线 | 依赖 P0–P2 |
| 客户端 P4 | 安装与自退 | **依赖 R3**（需真实包做真机验证） |
| 客户端 P5 | 启动自动检查 | 依赖 P3 |

先动 P0/P1，发布侧并行推进。

### P0 — 骨架与检查（无网络副作用）✅ 已完成

落地文件：`src/update/mod.rs`（类型 + `decide` 编排）、`src/update/version.rs`、`src/setting_config.rs`（`UpdateConfig`）。

实测确认的两条 semver 行为，均有守护测试：
- build metadata 不参与比较 → 本地 `0.106.0+a1b2c3d` 与远端 `0.106.0` 判为同版本，tag 后的本地构建不会被"降级"
- prerelease 数字标识符按数值比较 → `dev.10 > dev.2`（字典序会得出相反结论，dev.N 到两位数时卡死升级）

- 新增 `src/update/` 模块，定义 `ReleaseInfo` / `CheckResult` / `UpdateMsg`
- `version.rs`：引入 `semver`，实现本地版本解析（剥 `v` / `+meta` / `.dirty`），配套单测覆盖四种形态 + prerelease 递增
- `config.rs`：`UpdateConfig` 并入 `SettingConfig`，单测覆盖默认值与 Dev 路径
- 验收：`cargo test` 通过，版本比较对 `0.106.0` vs `0.107.0`、`0.107.0-dev.1` vs `0.107.0-dev.2` 判定正确

### P1 — 元数据拉取 ✅ 已完成

落地文件：`src/update/source.rs`（拉取 + 校验）、`src/update/notes.rs`（三级清洗）。

校验从严：`channel` 不匹配、`sha256` 非 64 位 hex、`size == 0`、下载地址非 https、版本号不可解析 —— 任一不满足即拒绝升级，不退化为"不校验就装"。

- `source.rs`：按 `mode::get()` 选 URL，`ureq` 拉取 + 解析 + `channel` 匹配校验
- release notes 二次拉取（失败静默返回空串，不阻断主流程）
- notes 清洗：`<!-- user-facing:start/end -->` → `## 更新记录` → 按 `---` 分段的三级回退
- 离线可测：抽出纯函数 `parse_latest_json(&str) -> Result<ReleaseInfo>`，单测喂固定 JSON
- 验收：单测覆盖正常/字段缺失/channel 不匹配/JSON 损坏四类输入

### P2 — 下载与校验 ✅ 已完成

落地文件：`src/update/download.rs`。

核心 `stream_to_file` 泛型于 `Read`，与 HTTP 层解耦 —— 截断/哈希不符/取消/超长四条路径全部脱网单测，不需要起本地 HTTP 服务。

并发保护改用 `AtomicBool` + RAII guard，第二次下载**立即返回明确错误**而非像旧版那样静默阻塞在互斥锁上。参数校验（https）前置于获取独占权，非法输入不占下载位。

`cache_path` 对 URL 末段做了字符白名单净化：URL 来自远端元数据，不能让 `../` 之类的内容决定写盘位置。

**分片并发（后补）**：体积 ≥ 8MB 且服务端确实支持 `Range` 时改走 `download_parallel`，4 线程各取一段，按绝对 offset 写进同一个预分配的 `.part`。探测、分片、校验任一环节不满足都回退单连接 —— 分片是性能优化，不该成为升级失败的新来源。`stream_to_file` 及其脱网单测原样保留，它仍是兜底路径。

必须校验 `Content-Range` 而不只看 206：部分 CDN 对 Range 请求返回 **200 全量**，不校验时每个分片都会把整包写到自己的 offset 上，文件彻底损坏 —— sha256 虽能兜住，但用户只会看到「校验值不匹配」，无从定位真因。校验层次决定的不是能否发现故障，而是故障能否被归因。

未引入第三方分片下载库：`manic` 已停更且依赖 tokio + reqwest + rayon + indicatif，`parallel_downloader` 另带 ratatui，`downloader` 只支持多文件并行而非单文件 Range 分片。任一都会给这个纯同步、无异步运行时的进程塞进第二套 TLS 栈与上百个传递依赖。ureq 是阻塞 IO，N 个 OS 线程足矣。

- `download.rs`：`ureq` 流式读 + 分块写临时文件 + 流式 sha256（单连接路径）
- 进度经 `Sender<UpdateMsg>` 上报，200ms 节流，结束补发一次 100%
- 取消：`Arc<AtomicBool>` 停止旗，照搬 `theme_editor_server.rs` 模式
- 超出 `size` 立即中止；完成后校验 size 与 sha256
- 临时文件 → rename 原子落盘；任何错误路径清理临时文件
- 并发保护：已有下载在跑时返回明确错误，**不静默阻塞**
- 分片并发：`PARALLEL_PARTS = 4` / `MIN_PARALLEL_SIZE = 8MB` 均为常量，便于按 CDN 表现调整；各线程持独立文件句柄 seek 到自己起点，写区间互不重叠故无需锁，也避开 `seek_write`/`write_at` 的平台差异
- `abort` 旗与用户 `cancel` 旗分离：一个分片失败要叫停其余分片，但那不是「用户取消」，混用会让网络故障被上层误报成已取消而不提示任何错误；用户取消也**不回退**单连接，否则会把取消掉的下载又跑一遍
- 验收：本地起 tiny_http 造一个假下载端点，测正常/截断/哈希不符/取消四条路径；分片另起可控 Range 服务器，覆盖分片拼装、坏代理返回 200 全量、哈希不符、取消清理

### P3 — UI 接线 ✅ 已完成

落地文件：`src/dialogs/update_dialog.rs`（七阶段对话框）、`src/state.rs`（升级状态与行为）、`src/main.rs`（通道注册）、`src/shell.rs`（根 stack 挂载）、`src/pages/mod.rs`（关于页按钮接线）。

七个阶段共用一个框架，正文与按钮行各自 `visible_when(|| phase.get() == P)` 切换。`Element::progress` 在项目内首次投入使用。

已截图验证三个阶段（HasUpdate / Downloading / Consent），Dev 变体的徽章与更新源分流一并确认。

**验证中发现的问题**：windui 的 label 是纯文本，没有 markdown 渲染 —— 发布说明里的 `### 新增` 会把井号原样显示给用户。已在 `notes::clean` 末尾补 `to_plain_text`（去标题井号、统一列表符号、成对剥离 `**` 与反引号）。剥离必须**成对**处理：只判断"后面还有没有同种符号"会让 `**粗**` 的末个星号找不到配对而留下 `粗*`。

- `main()` 注册 `app.channel::<UpdateMsg>()`，Sender 存入 `AppState`
- `src/dialogs/update_dialog.rs`：状态机 `Consent → Checking → HasUpdate → Downloading → Done | NoUpdate | Error`
- 挂 `shell.rs` 根 stack（**必须**，否则遮罩只填满所在格，表现为「显示在行下方」而非居中模态）
- `Element::progress(Signal<f32>)` 首次投入使用
- 接线 `pages/mod.rs:597` 的占位按钮
- Dev 变体徽章 + 不稳定提示
- **下载前显示包体积**（`size` 字段格式化为 MB），让用户在点「下载」前就知道要拉多少流量
- 验收：`WIND_RPC_MOCK=1` 下走完整流程截图核对

### P4 — 安装与自退 ✅ 代码完成（真机验证待 R3）

落地文件：`src/update/install.rs`，接线在 `state.rs::launch_update_install` 与对话框的「立即安装」。

**必须用 `ShellExecuteW`，不能用 `Command::spawn`。** 安装器嵌了
`requestedExecutionLevel=requireAdministrator`（`wind-installer/assets/app.manifest` 已核对），
从非提权进程调 `CreateProcess` 会直接失败（ERROR_ELEVATION_REQUIRED, 740）。项目已有手写
Win32 FFI 的先例（`state.rs` 的 GDI 字体枚举），故直接声明 extern 而不引入 `windows-sys`。

verb 取 `"open"` 而非 `"runas"`：manifest 已声明提权需求，shell 会自动走提权路径；
显式 `runas` 在已提权时反而多一次无谓确认。返回值 `> 32` 才算成功（ShellExecute 的历史约定）；
`1223` 是用户在 UAC 点了「否」，按「主动取消」处理并留在 Ready 阶段可重试，不弹错误。

**便携安装不静默升级**（§6.2b）：`is_portable_install` 取「标记文件存在 ∨ 卸载器缺失」两个信号的或，
命中则不传 `--silent`，拉起完整向导。另外确认了非静默分支**完全忽略 `--dir`**
（`main.rs` 的 `run_install` 直接进 `run_install_wizard()`），所以该分支干脆不传，免得让人以为路径已指定。

Ready 阶段的文案**预先告知**窗口将关闭 —— 安装器要终止本进程才能替换文件，
事后再提示已来不及（旧 NSIS 方案的用户体感就是「点了安装窗口凭空消失」）。已截图验证。

**真机验证清单**（待 R3 产出真包后执行）：
- [ ] 标准安装升级后安装目录未变、TSF 正常、用户词库与配置保留
- [ ] 装在非默认路径时不产生双份安装（验证 `--dir` 生效）
- [ ] 便携安装升级弹出的是向导而非静默
- [ ] UAC 点「否」后对话框停在 Ready 且可再次点击安装

- `install.rs`：从 `current_exe()` 推导安装目录，`Command::spawn` 拉起 installer，成功后 `ctx.request_close()`
- Portable 检测与规避（依 §8.5 的结论）
- 验收：真机装 Release 变体，从旧版升到新版，确认安装目录未变、TSF 正常、数据保留

### P5 — 启动自动检查 ✅ 已完成

落地：`update::should_auto_check`（三重门控纯函数）、`state.rs::spawn_silent_update_check` / `mark_update_checked` / `skip_update_version`、`main.rs` 启动接线、对话框「跳过此版本」按钮。

静默与手动的区分**由消息携带**（`UpdateMsg::CheckDone { silent }`）而非存进 AppState —— 存状态会与用户手动点击的检查互相串。

静默路径的原则是**不打扰**：无更新、已跳过的版本、开发构建、检查失败，全部只记日志。只有确实发现未被屏蔽的新版本才弹对话框。启动时弹一个「检查更新失败」对用户毫无价值。

节流在**发起前**记录时间戳，失败也算「查过」，否则每次打开设置都会重试。时钟回拨（`now < last_check_at`）时放行 —— 只用 `now - last >= 间隔` 判断的话，一次错误的系统时间会把自动检查永久卡死。

**调试入口（后补）**：三重门控叠加「失败也记时间」，使这条链路在开发期几乎无法复现 —— 首次启动之后的 24 小时内无论重启多少次都不再请网，且没有任何外部可见迹象。`--force-update-check` 参数或 `WIND_UPDATE_FORCE=1` 环境变量绕过 24h 节流与 `auto_check` 开关，但**不绕过 `network_consent`**：未经用户明示同意联网是产品底线，不设调试后门。强制检查刻意**不写** `last_check_at`，否则调试一次就吃掉真实的节流窗口，反而更难验证。

配套把静默检查的日志补全为 `local` / `remote` / `has_update` / `local_unparsable` / `skipped` 五项。此前只打 `has_update`，无法区分「已是最新」「无 tag 的开发构建被跳过」「命中跳过版本」—— 而静默路径本就没有任何界面反馈，日志是唯一的观测面。

已截图验证 HasUpdate 阶段的三按钮布局（暗色），发布说明走真实 `clean()` 链路确认纯文本化生效。

- `network_consent && auto_check` 双门控
- 用 `App::on_interval` 延迟数秒后台检查（避免与启动期 6 路并行 RPC 抢带宽）
- `last_check_at` 节流（如 24h 内不重复检查）
- `skipped_version` 过滤
- 双通道防竞态：结果存入 AppState 的一次性 pending 槽 + 经 channel 通知；UI 先注册监听再主动拉取

---

## 11. 风险登记

| 风险 | 影响 | 缓解 |
|---|---|---|
| CDN 元数据被篡改 | 装上恶意包（管理员权限 + 系统级 TSF 注册） | 本轮仅 sha256（元数据本身仍可被改）；**建议后续补 ed25519 签名，公钥硬编码** |
| TSF CLSID 与 DLL 内常量不一致 | 注册的 COM 找不到类工厂，**装上即输入法不可用** | GUID 只能取自 `wind_tsf/src/Globals.cpp`，不得自行生成（§5.1） |
| `.old_` 文件永久泄漏 | 每次升级在安装目录留一个孤儿 `wind_tsf.dll.old_*` | §8-B 补 `MOVEFILE_DELAY_UNTIL_REBOOT` |
| rename 失败致命中止且无回滚 | 升级中途失败留半装状态，可能连 receipt 都没写 | §8-C/F；短期靠「失败后引导用户重装」兜底 |
| Portable 被静默转 Standard | 用户安装形态被悄悄改变 | §6.2(b) 双信号检测，非 Standard 不静默 |
| 下载线程误碰 Signal | 编译失败（Signal 非 Send）或运行时 arena 错乱 | 架构上强制走 `UpdateMsg` channel |
| 大包下载超时 | 慢网用户永远升不上去 | 不设总超时，只设「无数据推进」空闲超时 |
| 升级后设置窗口消失 | 用户困惑 | §6.3 方案 B 兜底提示，A 作后续优化 |

---

## 12. 明确不做的

- **元数据签名**（本轮决策为仅 sha256；已登记为风险项）
- **GitHub 渠道与镜像 fallback** —— 旧项目的 ghproxy 镜像在无签名前提下反而是攻击面；统一走自有 CDN
- **增量/差分更新** —— 首版全量
- **断点续传** —— 首版全量重下
- **PAC 代理支持** —— 旧项目也只读 `ProxyEnable`/`ProxyServer`；`ureq` 走系统代理的能力需另行确认
- **macOS** —— 当前 wind-setting 的升级链路只规划 Windows
