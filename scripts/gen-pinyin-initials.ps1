# 生成 src/assets/pinyin_initials.txt —— 搜索用的汉字拼音首字母表
#
# 用法:
#   pwsh scripts/gen-pinyin-initials.ps1
#
# 什么时候要跑:
#   `search::pinyin::tests::table_covers_every_title_char` 失败时。它意味着有新的
#   设置项标题带进了表里没有的字（改文案、加功能都会），跑一次即可，**不需要人工查读音**。
#
# 数据来源:
#   ../WindInput/build/data/pinyin_map.txt —— core 的构建产物（44 439 行，源自
#   pinyin-data，经 kMandarin/kTGHZ2013/kXHC1983 白名单裁过古音，多音字按常用频率排序）。
#   用它而不是另找一份表，是为了让设置页的拼音检索与这套输入法自己的读音一致。
#   它不随本仓发布，故只在生成时读一次，运行时零依赖。
#
# 语料来源:
#   cargo test dump_title_chars —— 索引里全部**标题**的汉字。只覆盖标题是有意的：
#   拼音首字母不对 hint 生效（长句首字母串没有检索价值，只会制造误命中）。

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$out = Join-Path $repo 'src/assets/pinyin_initials.txt'

# ── 1. 找拼音真相源 ──
$candidates = @(
    (Join-Path $repo '../WindInput/build/data/pinyin_map.txt'),
    (Join-Path $repo '../WindInput/build_dev/data/pinyin_map.txt'),
    (Join-Path $repo '../WindInput/.cache/pinyin-data/pinyin.txt')
)
$mapPath = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $mapPath) {
    throw "找不到拼音表。它是 core 的构建产物，请先在 WindInput 侧构建一次。找过：`n  " + ($candidates -join "`n  ")
}
Write-Host "拼音来源: $mapPath"

# ── 2. 取语料（索引里所有标题的汉字）──
Write-Host "取标题语料…"
$dump = & cargo test --manifest-path (Join-Path $repo 'Cargo.toml') dump_title_chars -- --ignored --nocapture 2>&1
$lines = $dump -split "`r?`n"
$begin = [Array]::IndexOf($lines, '<<<TITLE_CHARS>>>')
if ($begin -lt 0) { throw "没抓到语料转储，cargo test 输出：`n$($dump -join "`n")" }
$chars = $lines[$begin + 1]
if ([string]::IsNullOrWhiteSpace($chars)) { throw '语料为空' }
Write-Host "标题汉字 $($chars.Length) 个"

# ── 3. 声调归一 ──
# 零声母字（安/额/欧…）的首字母自带声调，必须归一，否则表里会出现 "安 ā" 这种查不中的条目。
$toneMap = @{}
'āáǎà'.ToCharArray() | ForEach-Object { $toneMap[$_] = 'a' }
'ēéěèê'.ToCharArray() | ForEach-Object { $toneMap[$_] = 'e' }
'īíǐì'.ToCharArray() | ForEach-Object { $toneMap[$_] = 'i' }
'ōóǒò'.ToCharArray() | ForEach-Object { $toneMap[$_] = 'o' }
'ūúǔù'.ToCharArray() | ForEach-Object { $toneMap[$_] = 'u' }
'ńňǹ'.ToCharArray() | ForEach-Object { $toneMap[$_] = 'n' }
'ḿ'.ToCharArray() | ForEach-Object { $toneMap[$_] = 'm' }

function Get-Initial([string]$py) {
    if ([string]::IsNullOrWhiteSpace($py)) { return $null }
    $c = $py[0]
    if ($toneMap.ContainsKey($c)) { return $toneMap[$c] }
    $lower = [char]::ToLowerInvariant($c)
    if ($lower -ge 'a' -and $lower -le 'z') { return [string]$lower }
    return $null
}

# ── 4. 建 码点 → 首字母 索引（只查我们要的那几百个字）──
$want = @{}
foreach ($c in $chars.ToCharArray()) { $want["{0:X4}" -f [int]$c] = $true }

$found = @{}
foreach ($line in [System.IO.File]::ReadLines($mapPath)) {
    if ($line.Length -lt 8 -or $line[0] -ne 'U') { continue }
    $colon = $line.IndexOf(':')
    if ($colon -lt 0) { continue }
    $code = $line.Substring(2, $colon - 2)          # "U+XXXX" → "XXXX"
    if (-not $want.ContainsKey($code)) { continue }
    $rest = $line.Substring($colon + 1)
    $hash = $rest.IndexOf('#')
    if ($hash -ge 0) { $rest = $rest.Substring(0, $hash) }
    # 多音字取常用频率前两读；第三个往后基本是生僻音，收进来只会制造误命中。
    $inits = @()
    foreach ($py in ($rest -split ',')) {
        $i = Get-Initial $py.Trim()
        if ($i -and ($inits -notcontains $i)) { $inits += $i }
        if ($inits.Count -ge 2) { break }
    }
    if ($inits.Count -gt 0) { $found[$code] = $inits }
}

# ── 5. 写表 ──
$rows = @()
$missing = @()
foreach ($c in ($chars.ToCharArray() | Sort-Object)) {
    $code = "{0:X4}" -f [int]$c
    if ($found.ContainsKey($code)) {
        $rows += "$c $($found[$code] -join ' ')"
    } else {
        $missing += $c
    }
}

$header = @(
    '# 汉字 → 拼音首字母（1-2 个，多音字取常用频率前两读）',
    '# 由 scripts/gen-pinyin-initials.ps1 从 core 的 build/data/pinyin_map.txt 生成，请勿手改。',
    "# 字数: $($rows.Count)"
)
Set-Content -Path $out -Value ($header + $rows) -Encoding utf8NoBOM
Write-Host "已写出 $out（$($rows.Count) 字）"

if ($missing.Count -gt 0) {
    Write-Warning "有 $($missing.Count) 个字在拼音表里查不到，需要人工补：$($missing -join '')"
    exit 1
}
