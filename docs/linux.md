# Linux 版设置程序

Linux 上原生构建（`cargo build` / `cargo test` 直接跑，windui 走 X11；`WINDUI_BACKEND=wayland`
才试原生 Wayland，否则在 Wayland 会话里经 XWayland 运行）。随清风输入法的 `.deb` 分发，装在
`/usr/lib/windinput/wind_setting`（打包见 WindInput 仓 `scripts/linux/package-deb.sh`）。入口两个：

- 应用菜单「清风输入法设置」（`windinput-setting.desktop`，同时声明 `windinput://` 协议）；
- 输入时按「打开设置」热键（出厂 `Ctrl+Shift+]`）：服务推扩展信封 `settings.open`，Fcitx5 addon
  启动本程序（`WIND_INPUT_SETTING` 可覆盖路径）。已开着时新进程只把参数转给首实例。

端点与目录都与服务同一套规则：控制 socket `$XDG_RUNTIME_DIR/wind_input[_dev]_ctrl.sock`，
用户配置 `~/.config/WindInput[Dev]`，日志 `~/.local/share/WindInput[Dev]/logs`，打开目录/链接走
`xdg-open`。

## 系统关联（`windinput://`、`.wpkg`、`.wtheme`）

高级页「系统集成」按 XDG 现查、可按当前用户注册/取消（`src/xdg_assoc.rs`，Windows 的注册表版在
`protocol.rs` / `file_assoc.rs`，三态与门控两边同一套）：

| | 做法 |
|---|---|
| 状态 | `xdg-mime query default <类型>` 取默认处理者，再按 XDG 查找顺序读那份 `.desktop` 的 `Exec`：是本程序 → 已注册；别的程序（含另一份清风，如装了 deb 又跑便携版）→ 显示应用名、`.desktop` 与程序路径；查不到或那份 `.desktop` 已不在 → 未注册。系统包（`/usr/share`）提供且指向本程序时注明「系统安装包提供，在此不能取消」、「取消注册」灰掉；缺 xdg-mime 时直说原因 |
| 注册 | 系统包那份就指向本程序时只 `xdg-mime default`；否则先把包里同一份 `.desktop` 与 MIME 描述（`include_str!` 取 WindInput 仓 `scripts/linux/pkg/`）以**本程序绝对路径**写进 `$XDG_DATA_HOME/applications`、`$XDG_DATA_HOME/mime/packages`（带 `X-WindInput-UserInstall` 标记），跑 `update-mime-database` / `update-desktop-database`，再设默认；写完回读，被桌面专属 `mimeapps.list` 盖掉时报错。缺 `update-mime-database` 对包文件是硬错误（认不出类型就会按 zip 打开），缺 `update-desktop-database` 只跳过 |
| 取消 | 只去掉 `mimeapps.list` 里指向我们 `.desktop` 的项、只删带标记的用户级文件；两种包共用 `windinput-import.desktop`，各管各的 `MimeType`，最后一个包类型取消时才删 MIME 描述。系统文件与别人的关联一律不动，所以不弹「将删除他人关联」的确认 |
| 启动自愈 | 门控同 Windows（便携版不碰、用户取消过不再写）。只修本程序写过、`Exec` 指向已消失路径的用户级文件：改指本程序；本程序就是 deb 那份时删掉残留，让系统那份生效。从没注册过、或指向另一份还在的清风，都不动 |

端到端：WindInput 仓 `scripts/linux/e2e-assoc.sh`（隔离 XDG + 私有 Xvfb：类型识别、默认处理者、
`xdg-open` / `gio open` 拉起本程序进导入确认、首实例已开时转参、界面上注册/取消一圈、别人的关联
逐字节不变）。未验：真桌面的文件管理器双击（Nautilus / Dolphin / Deepin 文件管理器）、KDE 下
xdg-mime 走 ktraderclient 的查询分支、snap / flatpak 沙箱里的浏览器点链接（要经 portal 转出）。
用户级注册不装图标：便携包的类型图标与应用图标要等打包形态（tarball / AppImage）定了再补。

## 按平台屏蔽的设置项

清单项的 `platform` 字段取 `windows` / `macos` / `linux` 的逗号列表。Linux 版与 macOS 同一套
精简范围（WindInput 仓 `docs/design/linux-port.md` §2），在其上 Linux 额外没有：

| 设置项 | Linux 上为何无落点 |
|---|---|
| `keys.activate_ime` | 协调器的处理臂只在 macOS 编译，Linux 落 `Unhandled hotkey`；切换输入法归 Fcitx5 |
| `system.dota2_compat*` | 落点是 Windows 注册表 |
| 热键对话框的「全局」勾选 | 服务在 Linux 不注册全局热键（`keys.global_hotkeys` 原值照常保留写回） |

工具栏（含 `ui.toolbar.visible`、`keys.toggle_toolbar`）与软键盘（`keys.softkeyboard`）在 Linux 上与
Windows 一致，对应设置项全部开放；`ui.toolbar.hide_in_fullscreen` 在 X11 精确生效，Wayland 下
拿不到前台全屏状态，只是工具栏放在全屏应用之下，属近似效果。

仅 Windows 的项（`ui.toolbar.fullscreen_watch`——Windows 的定时轮询开关、`input.capslock.cancel_on_mode_switch`、`stats.track_english`、
TSF 日志、语言栏角标等）在 Linux 上同样隐藏。**应用兼容性（按应用配置）保留**：服务按 addon
报上来的程序名（Fcitx5 `InputContext::program()`）匹配规则，与 macOS 按 bundle id 同一机制。

## 已知限制

- **文件对话框**：rfd 走 xdg-desktop-portal，无门户时退回 `zenity`；两者都没有时「导入文件 / 导出」
  点了没有反应（rfd 分不出「取消」与「不可用」）。`.deb` 已 Recommends `xdg-desktop-portal | zenity`；
  导入还可以走剪贴板 / 链接两条不依赖对话框的路。
- **已开着时的深链切页要等下一次输入事件才生效**（windui Linux 后端：单实例转来的 argv 在主线程
  执行后没有触发重新布局；冷启动的深链正常）。窗口会被拉到前台，页要点一下才切过去。
