# wind_setting_native 开发脚本 (Windows PowerShell 版本)
#
# 用法:
#   .\scripts\dev.ps1            # 交互式菜单
#   .\scripts\dev.ps1 <命令>     # 非交互直调
#
# 命令:
#   b  / build      Release 构建
#   db / dbuild     Debug 构建
#   p  / copy       复制 release exe → WIND_DIR_RELEASE
#   dp / dcopy      复制 debug exe → WIND_DIR_DEBUG
#   t  / test       测试 (编译验证)
#   c  / check      cargo check
#   l  / clippy     cargo clippy
#   f  / fmt        cargo fmt
#   ci              fmt + clippy + check + test
#   clean           清理构建产物
#   run             本机运行 (WIND_PIPE_SUFFIX=xxx .\scripts\dev.ps1 run)
#
# 部署配置 scripts\deploy.local.ps1:
#   $WIND_DIR_RELEASE  = "C:\...\WindInput"       (p 复制目标)
#   $WIND_DIR_DEBUG    = "C:\...\WindInputDebug"  (dp 复制目标)
#   $WIND_PIPE_SUFFIX  = ""                        (可选, run 时注入)

param(
    [string]$Cmd = ""
)

# ---------- 路径 ----------
$ScriptDir   = $PSScriptRoot
$ProjectRoot = Split-Path $ScriptDir -Parent
$BinaryName  = "wind_setting_native.exe"

# 加载本地部署配置
$DeployLocal = Join-Path $ScriptDir "deploy.local.ps1"
$WIND_DIR_RELEASE  = ""
$WIND_DIR_DEBUG    = ""
$WIND_PIPE_SUFFIX  = ""
if (Test-Path $DeployLocal) {
    . $DeployLocal
}

# ---------- 颜色输出 ----------
function Say  { param($msg) Write-Host $msg -ForegroundColor Green  }
function Warn { param($msg) Write-Host $msg -ForegroundColor Yellow }
function Err  { param($msg) Write-Host $msg -ForegroundColor Red    }
function Gray { param($msg) Write-Host $msg -ForegroundColor DarkGray }

# ---------- 目录解析 ----------
function Resolve-DeployDir {
    param([string]$Profile)
    if ($Profile -eq "debug") {
        if (-not $WIND_DIR_DEBUG) {
            Err "未配置 WIND_DIR_DEBUG（deploy.local.ps1）"
            return $null
        }
        return $WIND_DIR_DEBUG.TrimEnd('\')
    } else {
        if (-not $WIND_DIR_RELEASE) {
            Err "未配置 WIND_DIR_RELEASE（deploy.local.ps1）"
            return $null
        }
        return $WIND_DIR_RELEASE.TrimEnd('\')
    }
}

# ---------- 提权 ----------

function Test-IsAdmin {
    ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

# 以管理员身份重新运行本脚本并传入命令，等待完成后返回退出码
# -WindowStyle Hidden 消除闪窗；子进程将全部输出写入临时日志，父进程读回展示
function Invoke-Elevated {
    param([string]$ElevatedCmd)
    Warn "目标目录需要管理员权限，正在提权..."
    $TmpLog = Join-Path $env:TEMP "wind_deploy.log"
    Remove-Item $TmpLog -ErrorAction SilentlyContinue

    # *> 捕获所有流（含 Write-Host 的 stream 6）；EncodedCommand 避免路径转义问题
    $innerCmd    = "& `"$PSCommandPath`" -Cmd `"$ElevatedCmd`" *> `"$TmpLog`""
    $encodedCmd  = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($innerCmd))
    $proc = Start-Process pwsh -Verb RunAs -Wait -PassThru -ArgumentList @(
        "-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden",
        "-EncodedCommand", $encodedCmd
    )

    if (Test-Path $TmpLog) {
        Get-Content $TmpLog | ForEach-Object { Write-Host $_ }
        Remove-Item $TmpLog -ErrorAction SilentlyContinue
    }
    if ($proc.ExitCode -ne 0) {
        Err "✗ 提权操作失败 (退出码: $($proc.ExitCode))"
    }
    return $proc.ExitCode
}

# ---------- 核心函数 ----------

function Do-Check {
    Say "▶ cargo check"
    cargo check
    return $LASTEXITCODE
}

function Do-Clippy {
    Say "▶ cargo clippy"
    cargo clippy -- -W clippy::all
    return $LASTEXITCODE
}

function Do-Fmt {
    Say "▶ cargo fmt"
    cargo fmt --all
    return $LASTEXITCODE
}

function Do-FmtCheck {
    Say "▶ cargo fmt --check"
    cargo fmt --all -- --check
    return $LASTEXITCODE
}

function Do-Test {
    Say "▶ cargo test (编译验证)"
    cargo test --no-run
    $code = $LASTEXITCODE
    Gray "  (测试二进制已生成)"
    return $code
}

function Do-Build {
    param([string]$Profile)

    if ($Profile -eq "release") {
        Say "▶ Release 构建"
        cargo build --release
    } else {
        Say "▶ Debug 构建"
        cargo build
    }

    if ($LASTEXITCODE -ne 0) {
        Err "✗ 构建失败"
        return 1
    }

    $ExePath = Join-Path $ProjectRoot "target\$Profile\$BinaryName"
    $Size = (Get-Item $ExePath -ErrorAction SilentlyContinue)?.Length
    $SizeStr = if ($Size) { "{0:N0} KB" -f ($Size / 1KB) } else { "?" }
    Say "✓ 构建成功: target\$Profile\$BinaryName ($SizeStr)"
    return 0
}

function Do-Copy {
    param([string]$Profile)

    $DestDir = Resolve-DeployDir $Profile
    if (-not $DestDir) { return 1 }

    # 目标目录需要写权限时自动提权（如 Program Files）
    if (-not (Test-IsAdmin)) {
        $elevatedCmd = if ($Profile -eq "debug") { "dp" } else { "p" }
        return Invoke-Elevated $elevatedCmd
    }

    $SrcPath = Join-Path $ProjectRoot "target\$Profile\$BinaryName"

    if (-not (Test-Path $SrcPath)) {
        Warn "未找到 $SrcPath，先执行构建..."
        $code = Do-Build $Profile
        if ($code -ne 0) { return 1 }
    }

    # 确保目标目录存在
    if (-not (Test-Path $DestDir)) {
        New-Item -ItemType Directory -Force $DestDir | Out-Null
    }

    # 终止占用进程（避免文件锁）
    $proc = Get-Process -Name ($BinaryName -replace '\.exe$','') -ErrorAction SilentlyContinue
    if ($proc) {
        Say "▶ 终止运行中的 $BinaryName..."
        $proc | Stop-Process -Force
        Start-Sleep -Milliseconds 500
    }

    # 备份旧 exe
    $DestPath = Join-Path $DestDir $BinaryName
    $OldPath  = Join-Path $DestDir "$BinaryName.old"
    if (Test-Path $DestPath) {
        Move-Item -Force $DestPath $OldPath -ErrorAction SilentlyContinue
    }

    # 复制新 exe（Copy-Item 是 cmdlet，用 try/catch 而非 $LASTEXITCODE 判断成败）
    Say "▶ 复制 $SrcPath → $DestDir\"
    try {
        Copy-Item -Force $SrcPath $DestDir -ErrorAction Stop
        Say "✓ 复制成功: $DestPath"
        return 0
    } catch {
        Err "✗ 复制失败: $_"
        return 1
    }
}

function Do-Clean {
    Say "▶ 清理构建产物"
    cargo clean
    Say "✓ 清理完成"
    return $LASTEXITCODE
}

function Do-CI {
    $code = Do-FmtCheck; if ($code -ne 0) { return $code }
    $code = Do-Clippy;   if ($code -ne 0) { return $code }
    $code = Do-Check;    if ($code -ne 0) { return $code }
    $code = Do-Test;     if ($code -ne 0) { return $code }
    Say "✓ CI 全部通过"
    return 0
}

function Do-Run {
    if ($WIND_PIPE_SUFFIX) {
        Say "▶ WIND_PIPE_SUFFIX=$WIND_PIPE_SUFFIX cargo run"
        $env:WIND_PIPE_SUFFIX = $WIND_PIPE_SUFFIX
        cargo run
        Remove-Item Env:WIND_PIPE_SUFFIX -ErrorAction SilentlyContinue
    } else {
        Say "▶ cargo run"
        cargo run
    }
    return $LASTEXITCODE
}

# ---------- 菜单 ----------

function Show-Menu {
    Write-Host ""
    Write-Host "wind_setting_native 开发菜单" -ForegroundColor Cyan
    Write-Host "─────────────────────────────────"
    Write-Host "  b  / build      Release 构建"
    Write-Host "  db / dbuild     Debug 构建"
    Write-Host "  p  / copy       复制 release exe → WIND_DIR_RELEASE"
    Write-Host "  dp / dcopy      复制 debug exe → WIND_DIR_DEBUG"
    Write-Host "  t  / test       测试 (编译验证)"
    Write-Host "  c  / check      cargo check"
    Write-Host "  l  / clippy     cargo clippy"
    Write-Host "  f  / fmt        cargo fmt"
    Write-Host "  ci              fmt + clippy + check + test"
    Write-Host "  clean           清理"
    Write-Host "  run             本机运行"
    Write-Host "  q  / quit       退出"
    Write-Host ""
}

# ---------- 命令分发 ----------

function Run-Cmd {
    param([string]$cmd)
    switch ($cmd) {
        { $_ -in 'b','build'   } { return Do-Build release }
        { $_ -in 'db','dbuild' } { return Do-Build debug   }
        { $_ -in 'p','copy'    } { return Do-Copy  release }
        { $_ -in 'dp','dcopy'  } { return Do-Copy  debug   }
        { $_ -in 't','test'    } { return Do-Test           }
        { $_ -in 'c','check'   } { return Do-Check          }
        { $_ -in 'l','clippy'  } { return Do-Clippy         }
        { $_ -in 'f','fmt'     } { return Do-Fmt            }
        'fmt-check'              { return Do-FmtCheck        }
        'ci'                     { return Do-CI              }
        'clean'                  { return Do-Clean           }
        'run'                    { return Do-Run             }
        { $_ -in 'q','quit','exit' } { exit 0 }
        default { Err "未知命令: $cmd"; return 1 }
    }
}

# ---------- 入口 ----------

Set-Location $ProjectRoot

# 非交互模式
if ($Cmd) {
    $code = Run-Cmd $Cmd
    exit $code
}

# 交互模式
while ($true) {
    Show-Menu
    $choice = Read-Host "选择命令"
    if (-not $choice) { continue }
    if ($choice -in 'q','quit','exit') { Say "再见"; break }
    Run-Cmd $choice
    Write-Host ""
}
