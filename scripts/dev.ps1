# wind_setting 开发脚本 (Windows PowerShell 版本)
#
# 用法:
#   .\scripts\dev.ps1            # 交互式菜单
#   .\scripts\dev.ps1 <命令>     # 非交互直调
#   .\scripts\dev.ps1 d1 pd1     # 连续命令（前者失败则停止）
#
# 命令:
#   1/b/b1/build    Release 构建（产物 wind_setting.exe）
#   d1/db/dbuild    Dev 构建（debug profile，产物仍为 wind_setting.exe）
#   p1/p/copy       复制 release exe → WIND_DIR_RELEASE\wind_setting.exe
#   pd1/dp/dcopy    复制 dev exe → WIND_DIR_DEV\wind_setting_dev.exe（改名带 _dev）
#   t  / test       测试 (编译验证)
#   c  / check      cargo check
#   l  / clippy     cargo clippy
#   f  / fmt        cargo fmt
#   ci              fmt + clippy + check + test
#   clean           清理构建产物
#   run             本机运行 release（mock: $env:WIND_RPC_MOCK=1）
#   dr / drun       本机运行 dev（注入 WIND_VARIANT=dev，连 dev 管道）
#
# 变体身份（对齐 core wind-config::variant）：靠 exe 文件名 `_dev` 后缀识别。
#   release 部署为 wind_setting.exe，dev 部署为 wind_setting_dev.exe。
#   编译画像（release / debug profile）与变体身份解耦：dev 用 debug profile 快编译，
#   复制时改名带 `_dev` 即获得 dev 身份（native 无控制台问题，无需 core 的 dev-variant profile）。
#
# 部署配置 scripts\deploy.local.ps1:
#   $WIND_DIR_RELEASE  = "C:\...\WindInput"      (p 复制目标)
#   $WIND_DIR_DEV      = "C:\...\WindInputDev"   (dp 复制目标)
#   $WIND_PIPE_SUFFIX  = ""                       (可选, run 时注入)

param(
    # 支持连续命令: .\dev.ps1 d1 pd1 (前者失败则后者不执行)
    [Parameter(Position = 0, ValueFromRemainingArguments)] [string[]]$Commands = @()
)

# ---------- 路径 ----------
$ScriptDir   = $PSScriptRoot
$ProjectRoot = Split-Path $ScriptDir -Parent
# cargo 产物名（package 名）；release / dev 两个 profile 的产物都是这个文件名
$ArtifactName = "wind_setting.exe"

# 加载本地部署配置
$DeployLocal = Join-Path $ScriptDir "deploy.local.ps1"
$WIND_DIR_RELEASE  = ""
$WIND_DIR_DEV      = ""
$WIND_PIPE_SUFFIX  = ""
if (Test-Path $DeployLocal) {
    . $DeployLocal
}

# ---------- 颜色输出 ----------
function Say  { param($msg) Write-Host $msg -ForegroundColor Green  }
function Warn { param($msg) Write-Host $msg -ForegroundColor Yellow }
function Err  { param($msg) Write-Host $msg -ForegroundColor Red    }
function Gray { param($msg) Write-Host $msg -ForegroundColor DarkGray }

# ---------- 变体信息 ----------
# 变体（release/dev）→ cargo 产物目录 + 部署文件名（dev 带 _dev）+ 目标目录。
# 编译画像与变体身份解耦：dev 走 debug profile（target\debug），部署时改名带 _dev。
function Get-VariantInfo {
    param([string]$Variant)
    if ($Variant -eq "dev") {
        return [pscustomobject]@{
            CargoRelease = $false
            TargetDir    = "debug"
            DeployName   = "wind_setting_dev.exe"
            DeployDir    = $WIND_DIR_DEV
            DeployVar    = "WIND_DIR_DEV"
            Label        = "Dev"
        }
    } else {
        return [pscustomobject]@{
            CargoRelease = $true
            TargetDir    = "release"
            DeployName   = "wind_setting.exe"
            DeployDir    = $WIND_DIR_RELEASE
            DeployVar    = "WIND_DIR_RELEASE"
            Label        = "Release"
        }
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
    $innerCmd    = "& `"$PSCommandPath`" `"$ElevatedCmd`" *> `"$TmpLog`""
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
    param([string]$Variant)
    $info = Get-VariantInfo $Variant

    Say "▶ $($info.Label) 构建"
    if ($info.CargoRelease) { cargo build --release } else { cargo build }

    if ($LASTEXITCODE -ne 0) {
        Err "✗ 构建失败"
        return 1
    }

    $ExePath = Join-Path $ProjectRoot "target\$($info.TargetDir)\$ArtifactName"
    $Size = (Get-Item $ExePath -ErrorAction SilentlyContinue)?.Length
    $SizeStr = if ($Size) { "{0:N0} KB" -f ($Size / 1KB) } else { "?" }
    Say "✓ 构建成功: target\$($info.TargetDir)\$ArtifactName ($SizeStr)"
    return 0
}

function Do-Copy {
    param([string]$Variant)
    $info = Get-VariantInfo $Variant

    $DestDir = $info.DeployDir
    if (-not $DestDir) {
        Err "未配置 $($info.DeployVar)（deploy.local.ps1）"
        return 1
    }
    $DestDir = $DestDir.TrimEnd('\')

    # 目标目录需要写权限时自动提权（如 Program Files）
    if (-not (Test-IsAdmin)) {
        $elevatedCmd = if ($Variant -eq "dev") { "pd1" } else { "p1" }
        return Invoke-Elevated $elevatedCmd
    }

    $SrcPath = Join-Path $ProjectRoot "target\$($info.TargetDir)\$ArtifactName"

    if (-not (Test-Path $SrcPath)) {
        Warn "未找到 $SrcPath，先执行构建..."
        $code = Do-Build $Variant
        if ($code -ne 0) { return 1 }
    }

    # 确保目标目录存在
    if (-not (Test-Path $DestDir)) {
        New-Item -ItemType Directory -Force $DestDir | Out-Null
    }

    # 终止占用进程（按部署名，避免文件锁）
    $procName = $info.DeployName -replace '\.exe$',''
    $proc = Get-Process -Name $procName -ErrorAction SilentlyContinue
    if ($proc) {
        Say "▶ 终止运行中的 $($info.DeployName)..."
        $proc | Stop-Process -Force
        Start-Sleep -Milliseconds 500
    }

    # 备份旧 exe（按部署名）
    $DestPath = Join-Path $DestDir $info.DeployName
    $OldPath  = "$DestPath.old"
    if (Test-Path $DestPath) {
        Move-Item -Force $DestPath $OldPath -ErrorAction SilentlyContinue
    }

    # 复制新 exe 并按变体改名（dev → wind_setting_dev.exe）
    # Copy-Item 是 cmdlet，用 try/catch 而非 $LASTEXITCODE 判断成败
    Say "▶ 复制 $SrcPath → $DestPath"
    try {
        Copy-Item -Force $SrcPath $DestPath -ErrorAction Stop
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
    param([string]$Variant)
    if ($Variant -eq "dev") {
        # dev 本机运行：注入 WIND_VARIANT=dev，连 dev 管道（无需改 exe 名）
        Say "▶ WIND_VARIANT=dev cargo run"
        $env:WIND_VARIANT = "dev"
        cargo run
        Remove-Item Env:WIND_VARIANT -ErrorAction SilentlyContinue
    } elseif ($WIND_PIPE_SUFFIX) {
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
    Write-Host "wind_setting 开发菜单" -ForegroundColor Cyan
    Write-Host "─────────────────────────────────"
    Write-Host "  1/b/build/b1    Release 构建"
    Write-Host "  d1/db/dbuild    Dev 构建 (debug profile)"
    Write-Host "  p1/p/copy       复制 release → WIND_DIR_RELEASE (wind_setting.exe)"
    Write-Host "  pd1/dp/dcopy    复制 dev → WIND_DIR_DEV (wind_setting_dev.exe)"
    Write-Host "  t  / test       测试 (编译验证)"
    Write-Host "  c  / check      cargo check"
    Write-Host "  l  / clippy     cargo clippy"
    Write-Host "  f  / fmt        cargo fmt"
    Write-Host "  ci              fmt + clippy + check + test"
    Write-Host "  clean           清理"
    Write-Host "  run             本机运行 release"
    Write-Host "  dr / drun       本机运行 dev (WIND_VARIANT=dev)"
    Write-Host "  q  / quit       退出"
    Write-Host "  (支持空格分隔连续命令, 如: d1 pd1)"
    Write-Host ""
}

# ---------- 命令分发 / 菜单 ----------

function Dispatch {
    param([string]$cmd)
    switch ($cmd) {
        { $_ -in '1','b','build','b1' }  { return Do-Build release }
        { $_ -in 'd1','db','dbuild' }    { return Do-Build dev     }
        { $_ -in 'p1','p','copy' }       { return Do-Copy  release }
        { $_ -in 'pd1','dp','dcopy' }    { return Do-Copy  dev     }
        { $_ -in 't','test' }            { return Do-Test           }
        { $_ -in 'c','check' }           { return Do-Check          }
        { $_ -in 'l','clippy' }          { return Do-Clippy         }
        { $_ -in 'f','fmt' }             { return Do-Fmt            }
        'fmt-check'                       { return Do-FmtCheck       }
        'ci'                              { return Do-CI             }
        'clean'                           { return Do-Clean          }
        'run'                             { return Do-Run release    }
        { $_ -in 'dr','drun' }           { return Do-Run dev        }
        default                           { Err "未知命令: $cmd"; return 1 }
    }
}

function Menu-Loop {
    while ($true) {
        Show-Menu
        $raw = (Read-Host "选择命令").Trim()
        if (-not $raw) { continue }
        if ($raw.ToLower() -in 'q','quit','exit') { Say "再见"; break }

        # 支持空格分隔的连续命令: "d1 pd1" → 依次执行, 前者失败则停止
        $tokens = $raw.ToLower() -split '\s+' | Where-Object { $_ -ne "" }
        $anyFailed = $false
        foreach ($token in $tokens) {
            if ($anyFailed) { break }
            $rc = Dispatch $token
            if ($rc -ne 0) { $anyFailed = $true }
        }
        Write-Host ""
        if ($anyFailed) { Write-Host "按回车继续..." -NoNewline; Read-Host | Out-Null }
    }
}

# ---------- 入口 ----------

Set-Location $ProjectRoot

$allCmds = @($Commands | Where-Object { $_ -ne "" })

# 无参数 → 交互菜单
if ($allCmds.Count -eq 0) { Menu-Loop; return }

$firstCmd = $allCmds[0].Trim().ToLower()

# help
if ($firstCmd -in "-h", "--help", "help") {
    Get-Content $PSCommandPath | Where-Object { $_ -match '^#' } | ForEach-Object { $_ -replace '^# ?', '' }
    return
}

# menu (显式)
if ($firstCmd -in "menu") { Menu-Loop; return }

# 按序执行所有命令; 前者失败则停止
foreach ($c in $allCmds) {
    $cmd = $c.Trim().ToLower()
    if ($cmd -in 'q','quit','exit') { exit 0 }
    $rc = Dispatch $cmd
    if ($rc -ne 0) { exit $rc }
}
exit 0
