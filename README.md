<p align="center">
  <img src="res/wind_setting_icon.png" alt="清风输入法设置" width="128">
</p>

<h1 align="center">清风输入法设置 (wind-setting)</h1>

<p align="center">
  清风输入法的原生图形设置程序
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-Windows%2010%2F11-brightgreen" alt="Platform">
  <img src="https://img.shields.io/badge/macOS-12%2B-blue" alt="macOS">
  <img src="https://img.shields.io/badge/Linux-Fcitx5%20%28preview%29-orange" alt="Linux">
  <img src="https://img.shields.io/badge/license-GPL--3.0-blue" alt="License">
</p>

<p align="center">
  <a href="https://windinput.com"><b>官网</b></a> ·
  <a href="https://windinput.com/download"><b>下载</b></a> ·
  <a href="https://windinput.com/docs"><b>使用文档</b></a> ·
  <a href="https://github.com/huanfeng/WindInput"><b>主仓库</b></a>
</p>

## 特性

- **清单驱动** — 设置项在 `src/assets/settings_manifest.toml` 中声明，按 group / section / item 映射为页面 / 卡片 / 行
- **与核心解耦** — 经 JSON-RPC（Windows 命名管道，Linux / macOS 为 Unix socket）读写配置，不改动 core 接口
- **词库管理** — 用户词库、临时词库、快捷短语、词频与候选调整的查看与编辑
- **应用兼容性** — 按应用配置输入行为，规则可按程序名、窗口类名、标题匹配
- **设置搜索** — 支持拼音首字母检索设置项，结果直接跳转到对应位置
- **亮暗主题** — 跟随系统明暗切换，也可通过命令行强制
- **单实例 + 协议激活** — `windinput://` 协议与 `.wpkg` / `.wtheme` 文件关联，冷启动可直达指定页面
- **跨平台** — 基于 [windui](https://github.com/huanfeng/wind-ui-rust) 自绘界面，支持 Windows、macOS 与 Linux

## 获取

设置程序**不单独发版**，随清风输入法的安装包与便携包一起分发。请前往
[windinput.com/download](https://windinput.com/download) 下载清风输入法，安装后从托盘菜单、
语言栏或应用菜单打开「清风输入法设置」。

## 仓库范围

本仓库是清风输入法的组件之一，只负责**设置界面**。输入引擎、词库、候选与主题等功能在主仓库
[WindInput](https://github.com/huanfeng/WindInput) 中维护，相关问题请到主仓库反馈。

| 仓库 | 职责 |
|------|------|
| [WindInput](https://github.com/huanfeng/WindInput) | 核心服务与各平台输入法前端；本仓库经 path 依赖使用其中的 `wind-ipc` |
| [wind-ui-rust](https://github.com/huanfeng/wind-ui-rust) | 跨平台轻量 GUI 库 `windui`，本仓库的界面基础（path 依赖） |
| **wind-setting**（本仓库） | 图形设置程序 |

## 从源码构建

需要 Rust stable 工具链（edition 2024）。本仓库以 path 依赖引用两个同级仓库，请按如下布局克隆：

```
<工作目录>/
├── wind-setting/      本仓库
├── wind-ui-rust/      https://github.com/huanfeng/wind-ui-rust
└── WindInput/         https://github.com/huanfeng/WindInput
```

```sh
cargo build            # debug 构建（opt-level=1，保证界面流畅）
cargo build --release  # release 构建
cargo test             # 运行测试
```

- **Windows**：原生构建；图标、版本信息与 DPI 感知 manifest 通过 `winresource` 在构建期嵌入
- **Linux**：原生构建，也可用 `scripts/dev.sh` 交叉编译 Windows 产物；平台细节见 [docs/linux.md](docs/linux.md)
- **macOS**：由主仓库的 `scripts/mac/dev.sh` 一并构建并打进 `.pkg`

没有运行中的输入法服务时，可以用 `WIND_RPC_MOCK=1` 以模拟数据预览界面（改动不会保存）：

```sh
WIND_RPC_MOCK=1 cargo run
```

提交前请运行 `cargo fmt`。仓库在 `.githooks/` 提供了 `pre-commit` 钩子（跑 `cargo fmt --check`），
启用一次即可：

```sh
git config core.hooksPath .githooks
```

## 命令行

| 参数 | 说明 |
|---|---|
| `--light` / `--dark` | 强制明 / 暗主题 |
| `--page <id>` / `--<id>` | 启动时定位到指定页面 |
| `--tab <n>` | 启动时定位到第 n 个页面 |
| `--schema <id>` | 仅 `--page dict`：定位到指定数据域 |
| `--type <id>` | 仅 `--page dict`：定位到指定子标签 |

`--schema` 取方案 id（如 `wubi86`）、引擎族名（`pinyin` / `codetable` / `mixed`）或 `phrase`（快捷短语）；
双拼等被折叠的拼音变体 id 会落到「拼音」域。`--type` 取 `phrase-system` / `phrase-user` /
`user-dict`（别名 `dict`）/ `temp` / `freq` / `shadow`。两者各自可选：

```sh
wind_setting.exe --page dict --schema wubi86 --type shadow   # 五笔的候选调整
wind_setting.exe --page dict --type freq                     # 当前数据域的词频
wind_setting.exe --dict --schema pinyin                      # 拼音域，子标签沿用上次
```

方案未找到、或该数据域没有请求的类型（如拼音域没有「候选调整」）时会降级到最接近的位置并 toast 说明，不会报错阻断。

## 目录结构

```
src/
  main.rs            入口：初始化、重连、协议路由
  shell.rs           主窗口框架与导航
  manifest.rs        设置清单解析
  rpc.rs             与核心服务的 JSON-RPC 通信
  pages/             各设置页（词库 dict/、应用兼容性 compat/ 等）
  dialogs/           各类编辑对话框
  search/            设置搜索
  wizard/            设置向导（按输入方式快速配置）
  update/            在线更新
  protocol.rs        自定义协议注册与自愈（Windows）
  file_assoc.rs      .wpkg / .wtheme 文件关联（Windows 注册表）
  xdg_assoc.rs       Linux 的协议与文件关联（XDG）
  assets/            设置清单与 capability 快照
docs/                设计文档与平台说明
```

## 参与贡献

欢迎贡献代码、报告 Bug 或提出建议！请阅读 [贡献指南](CONTRIBUTING.md)。

> 首次提交 PR 需要签署 [贡献者许可协议 (CLA)](CLA.md)。

## 许可证

本项目采用 [GNU 通用公共许可证第 3 版或更高版本](LICENSE)（GPL-3.0-or-later）。

基于本项目修改并分发的作品须同样以 GPL-3.0 发布并提供完整源代码。

## 关于项目名称

GPL 许可证授予您使用、修改与分发源代码的权利，但不包括使用项目名称与标识的权利。

若您将本项目分支后公开发布，请更换项目名称与 logo，并注明为非官方分支，
详见主仓库的 [项目名称与标识使用约定](https://github.com/huanfeng/WindInput/blob/main/BRANDING.md)。

## 交流与反馈

- **QQ 交流群**：[1085293418](https://qm.qq.com/q/u2A8FfafIs)
- **社区论坛**：[forum.windinput.com](https://forum.windinput.com)
- **GitHub Issues**：[问题反馈](../../issues)
