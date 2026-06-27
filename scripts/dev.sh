#!/usr/bin/env bash
# wind_setting 开发脚本 (Linux → Windows 交叉编译)
#
# 用法:
#   ./scripts/dev.sh            # 交互式菜单
#   ./scripts/dev.sh <命令>     # 非交互直调
#
# 命令:
#   b  / build      Release 构建（产物 wind_setting.exe）
#   db / dbuild     Dev 构建（debug profile，产物仍为 wind_setting.exe）
#   p  / push       Push release exe → Windows (REMOTE_DIR_RELEASE)\wind_setting.exe
#   dp / dpush      Push dev exe → Windows (REMOTE_DIR_DEV)\wind_setting_dev.exe（改名带 _dev）
#   t  / test       测试 (编译验证)
#   c  / check      cargo check
#   l  / clippy     cargo clippy
#   f  / fmt        cargo fmt
#   ci              fmt + clippy + check + test
#   clean           清理构建产物
#   run             本机运行 release (mock: WIND_RPC_MOCK=1 ./scripts/dev.sh run)
#   dr / drun       本机运行 dev (注入 WIND_VARIANT=dev)
#
# 变体身份（对齐 core wind-config::variant）：靠 exe 文件名 `_dev` 后缀识别。
#   release 部署为 wind_setting.exe，dev 部署为 wind_setting_dev.exe。
#   编译画像与变体身份解耦：dev 用 debug profile 快编译，推送时改名带 `_dev`。
#
# 部署配置 scripts/deploy.local:
#   WIND_REMOTE              = user@host
#   WIND_REMOTE_DIR_RELEASE  = C:/.../WindInput      (p 推送目标)
#   WIND_REMOTE_DIR_DEV      = C:/.../WindInputDev   (dp 推送目标)

set -o pipefail

# ---------- 路径 ----------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# 加载本地部署配置
[ -f "$SCRIPT_DIR/deploy.local" ] && . "$SCRIPT_DIR/deploy.local"
WIND_REMOTE="${WIND_REMOTE:-}"
WIND_REMOTE_DIR_RELEASE="${WIND_REMOTE_DIR_RELEASE:-}"
WIND_REMOTE_DIR_DEV="${WIND_REMOTE_DIR_DEV:-}"
WIND_REMOTE_DIR="${WIND_REMOTE_DIR:-}"   # 兼容旧配置：未设 _RELEASE 时作 release 回退
WIND_PIPE_SUFFIX="${WIND_PIPE_SUFFIX:-}"

# 交叉编译目标
TARGET="x86_64-pc-windows-gnu"
# cargo 产物名（package 名）；release / dev 两个 profile 的产物都是这个文件名
ARTIFACT_NAME="wind_setting.exe"
REMOTE_DIR=""   # 由 resolve_remote_dir 按 variant 填充

# ---------- 颜色 ----------
if [ -t 1 ]; then
    C_CYAN='\033[36m'; C_YELLOW='\033[33m'; C_GREEN='\033[32m'
    C_RED='\033[31m'; C_GRAY='\033[90m'; C_RESET='\033[0m'
else
    C_CYAN=''; C_YELLOW=''; C_GREEN=''; C_RED=''; C_GRAY=''; C_RESET=''
fi
say()  { printf '%b%b%b\n' "$C_GREEN" "$1" "$C_RESET"; }
warn() { printf '%b%b%b\n' "$C_YELLOW" "$1" "$C_RESET"; }
err()  { printf '%b%b%b\n' "$C_RED" "$1" "$C_RESET"; }
gray() { printf '%b%b%b\n' "$C_GRAY" "$1" "$C_RESET"; }

# ---------- 变体信息 ----------
# 变体（release/dev）→ "cargo产物目录 部署文件名"。
# 编译画像与变体身份解耦：dev 走 debug profile（target/.../debug），推送时改名带 _dev。
variant_info() {
    local variant="${1:-release}"
    if [ "$variant" = dev ]; then
        echo "debug wind_setting_dev.exe"
    else
        echo "release wind_setting.exe"
    fi
}

# ---------- 远程目录解析 ----------
# release → WIND_REMOTE_DIR_RELEASE（兼容旧 WIND_REMOTE_DIR）；dev → WIND_REMOTE_DIR_DEV
resolve_remote_dir() {
    local variant="${1:-release}"
    if [ "$variant" = dev ]; then
        REMOTE_DIR="${WIND_REMOTE_DIR_DEV:-}"
        [ -n "$REMOTE_DIR" ] || { err "未配置 WIND_REMOTE_DIR_DEV（deploy.local）"; return 1; }
    else
        REMOTE_DIR="${WIND_REMOTE_DIR_RELEASE:-${WIND_REMOTE_DIR:-}}"
        [ -n "$REMOTE_DIR" ] || { err "未配置 WIND_REMOTE_DIR_RELEASE（deploy.local）"; return 1; }
    fi
    REMOTE_DIR="${REMOTE_DIR%/}"
}

# ---------- 核心函数 ----------

do_check() {
    say "▶ cargo check ($TARGET)"
    cargo check --target "$TARGET" 2>&1
}

do_clippy() {
    say "▶ cargo clippy ($TARGET)"
    cargo clippy --target "$TARGET" -- -W clippy::all 2>&1
}

do_fmt() {
    say "▶ cargo fmt"
    cargo fmt --all
}

do_fmt_check() {
    say "▶ cargo fmt --check"
    cargo fmt --all -- --check
}

do_test() {
    say "▶ cargo test ($TARGET, 编译验证)"
    cargo test --target "$TARGET" --no-run 2>&1
    gray "  (测试二进制已生成, 需在 Windows 上执行验证)"
}

do_build() {
    local variant="$1"   # release 或 dev
    local targetdir deployname
    read -r targetdir deployname <<< "$(variant_info "$variant")"

    local flags=""
    if [ "$variant" = "release" ]; then
        flags="--release"
        say "▶ Release 构建 ($TARGET)"
    else
        say "▶ Dev 构建 ($TARGET, debug profile)"
    fi

    cargo build --target "$TARGET" $flags 2>&1
    local status=$?

    if [ $status -eq 0 ]; then
        local dir="target/$TARGET/$targetdir"
        local size
        size=$(ls -lh "$dir/$ARTIFACT_NAME" 2>/dev/null | awk '{print $5}')
        say "✓ 构建成功: $dir/$ARTIFACT_NAME ($size)"
    else
        err "✗ 构建失败"
        return 1
    fi
}

# 推送 exe 到 Windows 远程机（dev 改名带 _dev）。
# 流程: 杀远端进程 → 改名旧 exe 让路 → scp 新 exe（按部署名）→ 完成。
do_push() {
    local variant="$1"
    local targetdir deployname
    read -r targetdir deployname <<< "$(variant_info "$variant")"
    resolve_remote_dir "$variant" || return 1

    if [ -z "$WIND_REMOTE" ]; then
        err "未配置 WIND_REMOTE, 请在 scripts/deploy.local 中设置"
        return 1
    fi

    local src="target/$TARGET/$targetdir/$ARTIFACT_NAME"

    if [ ! -f "$src" ]; then
        warn "未找到 $src, 先执行构建..."
        do_build "$variant" || return 1
    fi

    # 确保远程目录存在
    ssh "$WIND_REMOTE" "mkdir -p '$REMOTE_DIR'" 2>/dev/null

    # 杀远端进程（按部署名，避免文件锁定）
    say "▶ 终止远端 $deployname..."
    ssh "$WIND_REMOTE" "taskkill /F /IM $deployname" >/dev/null 2>&1 || true
    sleep 1

    # 改名旧 exe 让路（已加载的 EXE 可改名、不可覆盖）
    ssh "$WIND_REMOTE" "if exist '$REMOTE_DIR\\$deployname' ren '$REMOTE_DIR\\$deployname' '${deployname}.old'" 2>/dev/null || true

    # 推送新 exe 并按变体改名
    say "▶ Push $src → $WIND_REMOTE:$REMOTE_DIR/$deployname"
    scp -q "$src" "$WIND_REMOTE:$REMOTE_DIR/$deployname" 2>&1
    local status=$?

    if [ $status -eq 0 ]; then
        say "✓ 推送成功: $WIND_REMOTE:$REMOTE_DIR/$deployname"
    else
        err "✗ 推送失败"
        return 1
    fi
}

do_clean() {
    say "▶ 清理构建产物"
    cargo clean 2>&1
    say "✓ 清理完成"
}

do_ci() {
    do_fmt_check || return 1
    do_clippy || return 1
    do_check || return 1
    do_test || return 1
    say "✓ CI 全部通过"
}

do_run() {
    local variant="${1:-release}"
    if [ "$variant" = dev ]; then
        say "▶ WIND_VARIANT=dev cargo run"
        WIND_VARIANT=dev cargo run --target "$TARGET" 2>&1
    elif [ -n "$WIND_PIPE_SUFFIX" ]; then
        say "▶ WIND_PIPE_SUFFIX=$WIND_PIPE_SUFFIX cargo run"
        WIND_PIPE_SUFFIX="$WIND_PIPE_SUFFIX" cargo run --target "$TARGET" 2>&1
    else
        say "▶ cargo run"
        cargo run --target "$TARGET" 2>&1
    fi
}

# ---------- 菜单 ----------

show_menu() {
    echo ""
    echo -e "${C_CYAN}wind_setting 开发菜单${C_RESET}"
    echo "─────────────────────────────────"
    echo "  b  / build      Release 构建"
    echo "  db / dbuild     Dev 构建 (debug profile)"
    echo "  p  / push       Push release → Windows (wind_setting.exe)"
    echo "  dp / dpush      Push dev → Windows (wind_setting_dev.exe)"
    echo "  t  / test       测试 (编译验证)"
    echo "  c  / check      cargo check"
    echo "  l  / clippy     cargo clippy"
    echo "  f  / fmt        cargo fmt"
    echo "  ci              fmt + clippy + check + test"
    echo "  clean           清理"
    echo "  run             本机运行 release"
    echo "  dr / drun       本机运行 dev (WIND_VARIANT=dev)"
    echo "  q  / quit       退出"
    echo ""
}

# ---------- 入口 ----------

run_cmd() {
    local cmd="$1"
    case "$cmd" in
        b|build)        do_build release ;;
        db|dbuild)      do_build dev ;;
        p|push)         do_push release ;;
        dp|dpush)       do_push dev ;;
        t|test)         do_test ;;
        c|check)        do_check ;;
        l|clippy)       do_clippy ;;
        f|fmt)          do_fmt ;;
        fmt-check)      do_fmt_check ;;
        ci)             do_ci ;;
        clean)          do_clean ;;
        run)            do_run release ;;
        dr|drun)        do_run dev ;;
        q|quit|exit)    exit 0 ;;
        *)              err "未知命令: $cmd"; return 1 ;;
    esac
}

# 非交互模式: 直接执行参数
if [ $# -gt 0 ]; then
    run_cmd "$1"
    exit $?
fi

# 交互模式: 菜单循环
while true; do
    show_menu
    read -rp "选择命令: " choice
    [ -z "$choice" ] && continue
    case "$choice" in
        q|quit|exit) say "再见"; break ;;
        *) run_cmd "$choice" ;;
    esac
    echo ""
done
