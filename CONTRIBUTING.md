# 贡献指南

感谢您对清风输入法设置程序 (wind-setting) 的关注！我们欢迎所有形式的贡献，包括 Bug 报告、功能建议和代码提交。

本仓库是清风输入法 (WindInput) 的组件之一，负责**设置界面**：经 JSON-RPC 读写输入法配置，
并提供词库管理、外观、兼容规则等页面。输入法核心引擎在主仓库
[WindInput](https://github.com/huanfeng/WindInput)——引擎、词库、候选、主题相关的问题请到主仓库反馈。

> **⚠️ alpha 阶段 PR 政策**
>
> 本项目目前处于 alpha 阶段，代码与文档变动频繁。为降低维护与冲突成本，**此阶段暂不接受仅包含文档改动或轻微改动的 Pull Request**，例如：
>
> - 纯文档变更（错别字、措辞润色、README/注释更新等）
> - 轻微改动（个别字符串、代码格式调整、无功能影响的小修小补等）
>
> 如发现文档错误或有改进建议，欢迎通过 [Issue](../../issues) 反馈，由维护者统一处理。功能性的 Bug 修复与新特性 PR 不受此限制，仍然欢迎提交。

## 签署 CLA（必须）

**所有贡献者在首次提交 Pull Request 前必须签署贡献者许可协议 (CLA)。**

这是为了确保项目的许可证管理和知识产权的一致性。流程如下：

1. 提交您的 Pull Request
2. CLA Assistant 机器人会自动在 PR 中发起签署请求
3. 在 PR 评论中回复：`I have read the CLA Document and I hereby sign the CLA`
4. 签署完成后，CLA 检查将自动通过

未签署 CLA 的 PR 将无法合并。完整协议内容请参阅 [CLA.md](CLA.md)。

## Bug 报告

请通过 [GitHub Issues](../../issues) 提交，并尽量包含以下信息：

- 操作系统与版本（如 Windows 11 24H2，或发行版与桌面环境）
- 清风输入法版本
- 出现问题的页面与操作步骤、预期行为与实际行为
- 相关日志

## 功能建议

欢迎通过 [GitHub Issues](../../issues) 提交。请描述您希望实现的功能、使用场景，如有参考实现请提供链接。

## 代码贡献

### 开发环境

- Rust stable 工具链（edition 2024，含 `rustfmt` 与 `clippy` 组件）
- 本项目依赖同级目录下的两个仓库，请按如下布局克隆：

```
<工作目录>/
├── wind-setting/      本仓库
├── wind-ui-rust/      https://github.com/huanfeng/wind-ui-rust
└── WindInput/         https://github.com/huanfeng/WindInput
```

### 构建

```sh
cargo build
cargo test
```

Windows 与 Linux 均可原生构建，详见 [README](README.md)。

> **本仓不单独发版。** 设置程序随 WindInput 的安装包与便携包一起分发，由主仓的
> [Release 流程](https://github.com/huanfeng/WindInput/releases)构建并打包。

### Git Hooks（首次克隆后建议激活）

仓库自带 `.githooks/pre-commit`（提交前自动跑 `cargo fmt --check`），默认不生效，需一次性激活：

```sh
git config core.hooksPath .githooks
```

（仅影响本地 clone，不会随仓库自动传播。）

### 提交规范

本项目使用 [Conventional Commits](https://www.conventionalcommits.org/zh-hans/) 规范：

```
<类型>(<范围>): <描述>

[可选的正文]
```

类型包括：

| 类型 | 说明 |
|------|------|
| `feat` | 新功能 |
| `fix` | Bug 修复 |
| `docs` | 文档变更 |
| `refactor` | 代码重构（不改变行为） |
| `perf` | 性能优化 |
| `test` | 测试相关 |
| `chore` | 构建/工具变更 |
| `style` | 格式化（如 `cargo fmt`） |

### 代码风格

- 必须使用 `cargo fmt` 格式化；**逻辑修改与 fmt 修改分开提交**
- 新增设置项在 `src/assets/settings_manifest.toml` 中声明，不在页面代码里硬编码

### Pull Request 流程

1. Fork 本仓库并从 `main` 分支创建您的分支
2. 完成修改后运行 `cargo fmt --check`、`cargo clippy --all-targets` 与 `cargo test`
3. 提交 PR 并等待 CLA 检查和代码审查
4. 根据审查意见修改后，PR 将被合并

## 许可证

本项目采用 [GPL-3.0-or-later](LICENSE) 许可证。提交贡献即表示您同意按照 [CLA](CLA.md) 的条款授权您的贡献。

若您将本项目分支后公开发布，请更换项目名称与 logo 并注明为非官方分支，
详见主仓库的 [项目名称与标识使用约定](https://github.com/huanfeng/WindInput/blob/main/BRANDING.md)。
