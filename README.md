# wind_setting

清风输入法的原生设置应用,基于 [`windui`](../wind-ui-rust) 构建。

通过 JSON-RPC over named pipe 与输入法核心服务通信,不改动 core 接口。界面采用无边框窗口 + 自定义标题栏,设置项由 manifest 清单驱动(分组为多个 Tab),支持运行时明暗换肤。

## 特性

- **清单驱动的设置界面** —— 设置项在 `src/assets/settings_manifest.toml` 中声明,按 group / section / item 映射为 Tab / 卡片 / 行。
- **与核心解耦** —— 经 named pipe 上的 JSON-RPC 读写配置,设置应用不侵入 core。
- **词库管理** —— 独立的词库页,支持规则的增删改与候选调整(见 `src/pages/dict/`)。
- **运行时换肤** —— 跟随系统明暗主题,亦可通过命令行强制。
- **单实例 + 协议激活** —— 支持自定义协议 URL 冷启动路由到指定页面。

## 构建

需要 Rust(edition 2024)工具链。本项目依赖若干同级路径仓库(见 `Cargo.toml`):

- `windui`(`../wind-ui-rust`)
- `wind-ipc`(`../WindInput/wind_input/crates/wind-ipc`)

请确保这些依赖已在相应位置就绪,然后:

```sh
cargo build            # debug 构建(opt-level=1,布局更顺滑)
cargo build --release  # release 构建
cargo test             # 运行测试
```

> 目标平台为 Windows:图标 / 版本信息 / DPI 感知 manifest 通过 `winresource` 在构建期嵌入,注册表访问使用 `winreg`。

### 代码格式

本项目采用 `cargo fmt` 默认风格,提交前请先 `cargo fmt`。仓库在 `.githooks/` 提供了 `pre-commit` 钩子(跑 `cargo fmt --check`),启用一次即可在提交前自动拦住格式漂移:

```sh
git config core.hooksPath .githooks
```

## 命令行

| 参数 | 说明 |
|---|---|
| `--light` / `--dark` | 强制明 / 暗主题 |
| `--page <id>` / `--<id>` | 启动时定位到指定页面 |
| `--tab <n>` | 启动时定位到第 n 个 Tab |
| `--schema <id>` | 仅 `--page dict`:定位到指定数据域 |
| `--type <id>` | 仅 `--page dict`:定位到指定子标签 |

`--schema` 取方案 id(如 `wubi86`)、引擎族名(`pinyin` / `codetable` / `mixed`)或 `phrase`(快捷短语);
双拼等被折叠的拼音变体 id 会落到「拼音」域。`--type` 取 `phrase-system` / `phrase-user` /
`user-dict`(别名 `dict`)/ `temp` / `freq` / `shadow`。两者各自可选:

```sh
wind_setting.exe --page dict --schema wubi86 --type shadow   # 五笔的候选调整
wind_setting.exe --page dict --type freq                     # 当前数据域的词频
wind_setting.exe --dict --schema pinyin                      # 拼音域,子标签沿用上次
```

方案未找到、或该数据域没有请求的类型(如拼音域没有「候选调整」)时会降级到最接近的位置并 toast 说明,不会报错阻断。

## 目录结构

```
src/
  main.rs            入口:初始化、重连、协议路由
  manifest.rs        设置清单解析
  pages/dict/        词库管理页(state / view / spec / dialogs)
  rpc.rs             JSON-RPC over named pipe
  protocol.rs        自定义协议注册与自愈
  assets/            设置清单与 capability 快照
docs/redesign/       引擎配置对照与布局设计文档
```

## 许可

尚未确定许可证。
